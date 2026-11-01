"""ThreadPoolExecutor: runs calls on a pool of worker threads."""

from concurrent.futures import _base
import itertools
import queue
import threading
import types
import weakref
import os

# Work queues of live worker threads, so that interpreter exit can wake and
# join them.  Weak keys: a thread that died is forgotten.
_threads_queues = weakref.WeakKeyDictionary()
_shutdown = False
# Serialises interpreter shutdown against the creation of new workers.
_global_shutdown_lock = threading.Lock()


def _python_exit():
    global _shutdown
    with _global_shutdown_lock:
        _shutdown = True
    entries = list(_threads_queues.items())
    for _thread, q in entries:
        q.put(None)
    for t, _q in entries:
        t.join()


# Joined before the atexit hooks run (non-daemon worker threads would
# otherwise keep the interpreter alive and atexit would never be reached).
threading._register_atexit(_python_exit)

if hasattr(os, 'register_at_fork'):
    os.register_at_fork(before=_global_shutdown_lock.acquire,
                        after_in_child=_global_shutdown_lock._at_fork_reinit,
                        after_in_parent=_global_shutdown_lock.release)
    os.register_at_fork(after_in_child=_threads_queues.clear)


class WorkerContext:
    """Per-worker execution context.  The thread pool runs tasks directly;
    subclasses (e.g. the interpreter pool) override initialize/finalize/run."""

    @classmethod
    def prepare(cls, initializer, initargs):
        if initializer is not None and not callable(initializer):
            raise TypeError(f'initializer must be a callable, got {initializer!r}')

        def create_context():
            return cls(initializer, initargs)

        def resolve_task(fn, args, kwargs):
            return (fn, args, kwargs)

        return create_context, resolve_task

    def __init__(self, initializer, initargs):
        self.initializer = initializer
        self.initargs = initargs

    def initialize(self):
        if self.initializer is not None:
            self.initializer(*self.initargs)

    def finalize(self):
        pass

    def run(self, task):
        fn, args, kwargs = task
        return fn(*args, **kwargs)


class _WorkItem:
    def __init__(self, future, task):
        self.future = future
        self.task = task

    def run(self, ctx):
        if not self.future.set_running_or_notify_cancel():
            return
        try:
            result = ctx.run(self.task)
        except BaseException as exc:
            self.future.set_exception(exc)
            # break the reference cycle exc -> traceback -> frame -> self
            self = None
        else:
            self.future.set_result(result)

    __class_getitem__ = classmethod(types.GenericAlias)


def _worker(executor_reference, ctx, work_queue):
    try:
        ctx.initialize()
    except BaseException:
        _base.LOGGER.critical('Exception in initializer:', exc_info=True)
        executor = executor_reference()
        if executor is not None:
            executor._initializer_failed()
        return
    try:
        while True:
            try:
                work_item = work_queue.get_nowait()
            except queue.Empty:
                # Tell the executor this worker is idle before blocking.
                executor = executor_reference()
                if executor is not None:
                    executor._idle_semaphore.release()
                del executor
                work_item = work_queue.get(block=True)

            if work_item is not None:
                work_item.run(ctx)
                # do not keep the item (and its result) alive while idle
                del work_item
                continue

            executor = executor_reference()
            # Leave if the interpreter is exiting, the executor was garbage
            # collected or shut down.
            if _shutdown or executor is None or executor._shutdown:
                if executor is not None:
                    executor._shutdown = True
                # wake the next worker so that it leaves too
                work_queue.put(None)
                return
            del executor
    except BaseException:
        _base.LOGGER.critical('Exception in worker', exc_info=True)
    finally:
        ctx.finalize()


class BrokenThreadPool(_base.BrokenExecutor):
    """Raised once a worker's initializer failed: the pool is unusable."""


class ThreadPoolExecutor(_base.Executor):

    BROKEN = BrokenThreadPool

    # Gives every executor without an explicit prefix a unique name.
    _counter = itertools.count().__next__

    @classmethod
    def prepare_context(cls, initializer, initargs):
        return WorkerContext.prepare(initializer, initargs)

    def __init__(self, max_workers=None, thread_name_prefix='',
                 initializer=None, initargs=(), **ctxkwargs):
        """Create an executor running at most *max_workers* threads.

        *initializer(*initargs)* runs at the start of each worker."""
        if max_workers is None:
            # I/O bound work is the common case, so allow a few more
            # threads than CPUs, but cap the pool for big machines.
            max_workers = min(32, (os.process_cpu_count() or 1) + 4)
        if max_workers <= 0:
            raise ValueError("max_workers must be greater than 0")

        (self._create_worker_context,
         self._resolve_work_item_task,
         ) = type(self).prepare_context(initializer, initargs, **ctxkwargs)

        self._max_workers = max_workers
        self._work_queue = queue.SimpleQueue()
        self._idle_semaphore = threading.Semaphore(0)
        self._threads = set()
        self._broken = False
        self._shutdown = False
        self._shutdown_lock = threading.Lock()
        self._thread_name_prefix = (thread_name_prefix or
                                    ("ThreadPoolExecutor-%d" % self._counter()))

    def submit(self, fn, /, *args, **kwargs):
        with self._shutdown_lock, _global_shutdown_lock:
            if self._broken:
                raise self.BROKEN(self._broken)
            if self._shutdown:
                raise RuntimeError('cannot schedule new futures after shutdown')
            if _shutdown:
                raise RuntimeError('cannot schedule new futures after '
                                   'interpreter shutdown')

            f = _base.Future()
            task = self._resolve_work_item_task(fn, args, kwargs)
            self._work_queue.put(_WorkItem(f, task))
            self._adjust_thread_count()
            return f
    submit.__doc__ = _base.Executor.submit.__doc__

    def _adjust_thread_count(self):
        # Reuse an idle worker if there is one.
        if self._idle_semaphore.acquire(timeout=0):
            return

        # When the executor is collected, wake a worker so that it exits.
        def weakref_cb(_, q=self._work_queue):
            q.put(None)

        num_threads = len(self._threads)
        if num_threads < self._max_workers:
            thread_name = '%s_%d' % (self._thread_name_prefix or self, num_threads)
            t = threading.Thread(name=thread_name, target=_worker,
                                 args=(weakref.ref(self, weakref_cb),
                                       self._create_worker_context(),
                                       self._work_queue))
            t.start()
            self._threads.add(t)
            _threads_queues[t] = self._work_queue

    def _initializer_failed(self):
        with self._shutdown_lock:
            self._broken = ('A thread initializer failed, the thread pool '
                            'is not usable anymore')
            # Fail everything that is still queued.
            while True:
                try:
                    work_item = self._work_queue.get_nowait()
                except queue.Empty:
                    break
                if work_item is not None:
                    work_item.future.set_exception(self.BROKEN(self._broken))

    def shutdown(self, wait=True, *, cancel_futures=False):
        with self._shutdown_lock:
            self._shutdown = True
            if cancel_futures:
                # Cancel everything that has not started yet.
                while True:
                    try:
                        work_item = self._work_queue.get_nowait()
                    except queue.Empty:
                        break
                    if work_item is not None:
                        work_item.future.cancel()
            # One sentinel wakes a worker, which passes it on to the next.
            self._work_queue.put(None)
        if wait:
            for t in self._threads:
                t.join()
    shutdown.__doc__ = _base.Executor.shutdown.__doc__
