"""ThreadPoolExecutor: calls run on a pool of worker threads.

Submitted calls go onto one queue shared by the workers.  A worker that
finds the queue empty reports itself idle before it blocks, so that
submit() starts a new thread only when no idle worker can take the call.
A None on the queue means "check whether to stop": a worker that stops puts
it back for the next one.
"""

from concurrent.futures import _base
import itertools
import os
import queue
import threading
import types
import weakref

# Queue of every live worker thread, so that interpreter exit can wake and
# join them.  Weak keys: finished threads drop out.
_threads_queues = weakref.WeakKeyDictionary()
_shutdown = False
# Taken by submit() and by the exit hook, so that no worker starts while the
# interpreter is shutting the pools down.
_global_shutdown_lock = threading.Lock()


def _python_exit():
    global _shutdown
    with _global_shutdown_lock:
        _shutdown = True
    workers = list(_threads_queues.items())
    for _thread, work_queue in workers:
        work_queue.put(None)
    for thread, _work_queue in workers:
        thread.join()


# Runs before the atexit hooks: worker threads are not daemons, the
# interpreter would otherwise wait for them before ever reaching atexit.
threading._register_atexit(_python_exit)

if hasattr(os, 'register_at_fork'):
    # A child must not inherit the lock held, nor try to join the parent's
    # threads at exit.
    os.register_at_fork(before=_global_shutdown_lock.acquire,
                        after_in_child=_global_shutdown_lock._at_fork_reinit,
                        after_in_parent=_global_shutdown_lock.release)
    os.register_at_fork(after_in_child=_threads_queues.clear)


class WorkerContext:
    """What a worker thread runs its calls in.  Here that is the thread
    itself; subclasses (the interpreter pool's) give each worker its own
    environment through initialize()/finalize()/run()."""

    @classmethod
    def prepare(cls, initializer, initargs):
        """Return (make_context, as_task): a factory for one context per
        worker, and the function turning (fn, args, kwargs) into what run()
        accepts."""
        if initializer is not None and not callable(initializer):
            raise TypeError(f'initializer must be a callable, got {initializer!r}')

        def make_context():
            return cls(initializer, initargs)

        def as_task(fn, args, kwargs):
            return (fn, args, kwargs)

        return make_context, as_task

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
    """A queued call together with its future."""

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
            # The traceback references this frame and so this item; drop
            # the item to avoid keeping a cycle alive.
            self = None
        else:
            self.future.set_result(result)

    __class_getitem__ = classmethod(types.GenericAlias)


def _should_stop(executor):
    # A None was taken off the queue: stop if the interpreter is exiting or
    # the executor is gone or shut down.
    return _shutdown or executor is None or executor._shutdown


def _serve(executor_reference, ctx, work_queue):
    # Body of a worker thread.  The executor is only referenced weakly, and
    # never across a blocking get(), so that it can be collected.
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
                item = work_queue.get_nowait()
            except queue.Empty:
                executor = executor_reference()
                if executor is not None:
                    executor._idle_semaphore.release()
                del executor
                item = work_queue.get(block=True)
            if item is not None:
                item.run(ctx)
                # Do not keep the call (and its result) alive while waiting.
                del item
                continue
            executor = executor_reference()
            if _should_stop(executor):
                if executor is not None:
                    executor._shutdown = True
                work_queue.put(None)        # pass the stop on
                return
            del executor
    except BaseException:
        _base.LOGGER.critical('Exception in worker', exc_info=True)
    finally:
        ctx.finalize()


class BrokenThreadPool(_base.BrokenExecutor):
    """A worker's initializer failed; the pool cannot be used any more."""


class ThreadPoolExecutor(_base.Executor):

    BROKEN = BrokenThreadPool

    # Numbers the executors created without a thread_name_prefix.
    _counter = itertools.count().__next__

    @classmethod
    def prepare_context(cls, initializer, initargs):
        return WorkerContext.prepare(initializer, initargs)

    def __init__(self, max_workers=None, thread_name_prefix='',
                 initializer=None, initargs=(), **ctxkwargs):
        """Run calls on at most *max_workers* threads.

        The default suits I/O-bound work: four more threads than the CPUs
        this process may use, but no more than 32.  *initializer*(*initargs)
        runs first in every worker thread."""
        if max_workers is None:
            max_workers = min(32, (os.process_cpu_count() or 1) + 4)
        if max_workers <= 0:
            raise ValueError("max_workers must be greater than 0")
        self._create_worker_context, self._resolve_work_item_task = (
            type(self).prepare_context(initializer, initargs, **ctxkwargs))
        self._max_workers = max_workers
        self._work_queue = queue.SimpleQueue()
        self._idle_semaphore = threading.Semaphore(0)
        self._threads = set()
        self._broken = False
        self._shutdown = False
        self._shutdown_lock = threading.Lock()
        self._thread_name_prefix = (
            thread_name_prefix or "ThreadPoolExecutor-%d" % self._counter())

    def submit(self, fn, /, *args, **kwargs):
        with self._shutdown_lock, _global_shutdown_lock:
            if self._broken:
                raise self.BROKEN(self._broken)
            if self._shutdown:
                raise RuntimeError('cannot schedule new futures after shutdown')
            if _shutdown:
                raise RuntimeError('cannot schedule new futures after '
                                   'interpreter shutdown')
            future = _base.Future()
            task = self._resolve_work_item_task(fn, args, kwargs)
            self._work_queue.put(_WorkItem(future, task))
            self._adjust_thread_count()
            return future
    submit.__doc__ = _base.Executor.submit.__doc__

    def _adjust_thread_count(self):
        if self._idle_semaphore.acquire(timeout=0):
            return                  # an idle worker will take the call
        count = len(self._threads)
        if count >= self._max_workers:
            return
        work_queue = self._work_queue

        def wake_a_worker(_ref):
            # The executor was collected: let the workers notice.
            work_queue.put(None)

        thread = threading.Thread(
            name='%s_%d' % (self._thread_name_prefix or self, count),
            target=_serve,
            args=(weakref.ref(self, wake_a_worker),
                  self._create_worker_context(), work_queue))
        thread.start()
        self._threads.add(thread)
        _threads_queues[thread] = work_queue

    def _drain(self, settle):
        # Take every queued call off the queue and settle(future) it.
        while True:
            try:
                item = self._work_queue.get_nowait()
            except queue.Empty:
                return
            if item is not None:
                settle(item.future)

    def _initializer_failed(self):
        with self._shutdown_lock:
            self._broken = ('A thread initializer failed, the thread pool '
                            'is not usable anymore')
            broken = self._broken
            self._drain(lambda future: future.set_exception(self.BROKEN(broken)))

    def shutdown(self, wait=True, *, cancel_futures=False):
        with self._shutdown_lock:
            self._shutdown = True
            if cancel_futures:
                self._drain(lambda future: future.cancel())
            # One None reaches one worker, which hands it on.
            self._work_queue.put(None)
        if wait:
            for thread in self._threads:
                thread.join()
    shutdown.__doc__ = _base.Executor.shutdown.__doc__
