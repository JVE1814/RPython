"""ProcessPoolExecutor: runs calls in a pool of worker processes.

Data flow:

  submit() -> pending work items (dict) + work id queue
     -> manager thread moves items to the call queue (bounded: workers
        only get a little work ahead) -> worker processes
     -> result queue -> manager thread completes the futures

The executor manager thread is the only consumer of the result queue; it
also notices dead workers (process sentinels) and then breaks the pool,
replaces workers that exit after max_tasks_per_child tasks, and shuts the
pool down.  submit()/shutdown() wake it through a _ThreadWakeup pipe.
"""

import os
from concurrent.futures import _base
import queue
import multiprocessing as mp
import multiprocessing.connection
from multiprocessing.queues import Queue
import threading
import weakref
from functools import partial
import itertools
import sys
from traceback import format_exception


# executor manager thread -> its wakeup (for interpreter exit)
_threads_wakeups = weakref.WeakKeyDictionary()
_global_shutdown = False


class _ThreadWakeup:
    """Self-pipe waking the executor manager thread."""

    def __init__(self):
        self._closed = False
        self._lock = threading.Lock()
        self._reader, self._writer = mp.Pipe(duplex=False)

    def close(self):
        with self._lock:
            if not self._closed:
                self._closed = True
                self._writer.close()
                self._reader.close()

    def wakeup(self):
        with self._lock:
            if not self._closed:
                self._writer.send_bytes(b"")

    def clear(self):
        # Only the manager thread reads; no lock needed (and taking the
        # executor's shutdown lock here could deadlock with submit()).
        if self._closed:
            raise RuntimeError('operation on closed _ThreadWakeup')
        while self._reader.poll():
            self._reader.recv_bytes()


def _python_exit():
    global _global_shutdown
    _global_shutdown = True
    items = list(_threads_wakeups.items())
    for _, thread_wakeup in items:
        thread_wakeup.wakeup()
    for t, _ in items:
        t.join()


# Join the manager threads before atexit hooks run (they could otherwise
# keep running while the interpreter tears down).
threading._register_atexit(_python_exit)

# Items the call queue may hold beyond one per worker: keeps the workers
# busy without moving work out of reach of cancel().
EXTRA_QUEUED_CALLS = 1

# WaitForMultipleObjects limit (Windows only)
_MAX_WINDOWS_WORKERS = 63 - 2

# operations of _force_shutdown()
_TERMINATE = "terminate"
_KILL = "kill"
_SHUTDOWN_CALLBACK_OPERATION = {_TERMINATE, _KILL}


class _RemoteTraceback(Exception):
    """Formatted traceback of a worker's exception (its __cause__)."""

    def __init__(self, tb):
        self.tb = tb

    def __str__(self):
        return self.tb


class _ExceptionWithTraceback:
    """Pickles an exception with its formatted traceback."""

    def __init__(self, exc, tb):
        text = ''.join(format_exception(type(exc), exc, tb))
        self.exc = exc
        # the traceback object itself cannot be pickled
        self.exc.__traceback__ = None
        self.tb = '\n"""\n%s"""' % text

    def __reduce__(self):
        return _rebuild_exc, (self.exc, self.tb)


def _rebuild_exc(exc, tb):
    exc.__cause__ = _RemoteTraceback(tb)
    return exc


class _WorkItem(object):
    def __init__(self, future, fn, args, kwargs):
        self.future = future
        self.fn = fn
        self.args = args
        self.kwargs = kwargs


class _ResultItem(object):
    def __init__(self, work_id, exception=None, result=None, exit_pid=None):
        self.work_id = work_id
        self.exception = exception
        self.result = result
        # set when the worker exits after this task (max_tasks_per_child)
        self.exit_pid = exit_pid


class _CallItem(object):
    def __init__(self, work_id, fn, args, kwargs):
        self.work_id = work_id
        self.fn = fn
        self.args = args
        self.kwargs = kwargs


class _SafeQueue(Queue):
    """The call queue: a call item that cannot be pickled fails its future
    instead of killing the feeder thread."""

    def __init__(self, max_size=0, *, ctx, pending_work_items, thread_wakeup):
        self.pending_work_items = pending_work_items
        self.thread_wakeup = thread_wakeup
        super().__init__(max_size, ctx=ctx)

    def _on_queue_feeder_error(self, e, obj):
        if isinstance(obj, _CallItem):
            tb = format_exception(type(e), e, e.__traceback__)
            e.__cause__ = _RemoteTraceback('\n"""\n{}"""'.format(''.join(tb)))
            work_item = self.pending_work_items.pop(obj.work_id, None)
            self.thread_wakeup.wakeup()
            if work_item is not None:
                work_item.future.set_exception(e)
        else:
            super()._on_queue_feeder_error(e, obj)


def _chain_from_iterable_of_lists(iterable):
    """Flatten the chunk results of map(), dropping references early."""
    for element in iterable:
        element.reverse()
        while element:
            yield element.pop()


def _process_chunk(fn, chunk):
    """Run *fn* over a chunk of argument tuples (one map() task)."""
    return [fn(*args) for args in chunk]


def _sendback_result(result_queue, work_id, result=None, exception=None,
                     exit_pid=None):
    """Put a result on the queue; an unpicklable result becomes an error."""
    try:
        result_queue.put(_ResultItem(work_id, result=result,
                                     exception=exception, exit_pid=exit_pid))
    except BaseException as e:
        exc = _ExceptionWithTraceback(e, e.__traceback__)
        result_queue.put(_ResultItem(work_id, exception=exc,
                                     exit_pid=exit_pid))


def _process_worker(call_queue, result_queue, initializer, initargs, max_tasks=None):
    """Main loop of a worker process.

    Runs call items until it gets None (shutdown: it answers with its pid)
    or until it has run *max_tasks* tasks (its last result carries its pid).
    """
    if initializer is not None:
        try:
            initializer(*initargs)
        except BaseException:
            _base.LOGGER.critical('Exception in initializer:', exc_info=True)
            # exiting without a word breaks the pool
            return
    num_tasks = 0
    exit_pid = None
    while True:
        call_item = call_queue.get(block=True)
        if call_item is None:
            result_queue.put(os.getpid())
            return

        if max_tasks is not None:
            num_tasks += 1
            if num_tasks >= max_tasks:
                exit_pid = os.getpid()

        try:
            r = call_item.fn(*call_item.args, **call_item.kwargs)
        except BaseException as e:
            exc = _ExceptionWithTraceback(e, e.__traceback__)
            _sendback_result(result_queue, call_item.work_id, exception=exc,
                             exit_pid=exit_pid)
        else:
            _sendback_result(result_queue, call_item.work_id, result=r,
                             exit_pid=exit_pid)
            del r

        # free the arguments before blocking for the next item
        del call_item

        if exit_pid is not None:
            return


def _spawn_worker(mp_context, call_queue, result_queue, initializer, initargs,
                  max_tasks_per_child):
    """Start one worker process."""
    p = mp_context.Process(
        target=_process_worker,
        args=(call_queue, result_queue, initializer, initargs,
              max_tasks_per_child))
    p.start()
    return p


class _ExecutorManagerThread(threading.Thread):
    """Moves work to the workers, collects results, watches the workers."""

    def __init__(self, executor):
        self.thread_wakeup = executor._executor_manager_thread_wakeup
        self.shutdown_lock = executor._shutdown_lock

        # Only a weak reference: the executor may be garbage collected
        # (which shuts the pool down); the callback wakes us up.
        def weakref_cb(_,
                       thread_wakeup=self.thread_wakeup,
                       mp_util_debug=mp.util.debug):
            mp_util_debug('Executor collected: triggering callback for'
                          ' QueueManager wakeup')
            thread_wakeup.wakeup()

        self.executor_reference = weakref.ref(executor, weakref_cb)
        self.processes = executor._processes
        self.call_queue = executor._call_queue
        self.result_queue = executor._result_queue
        self.work_ids_queue = executor._work_ids
        self.max_tasks_per_child = executor._max_tasks_per_child
        self.pending_work_items = executor._pending_work_items

        super().__init__()

    def run(self):
        while True:
            try:
                self.add_call_item_to_queue()
            except BaseException as exc:
                # e.g. no new thread can be started at interpreter exit
                self.terminate_broken(format_exception(exc))
                return

            result_item, is_broken, cause = self.wait_result_broken_or_wakeup()

            if is_broken:
                self.terminate_broken(cause)
                return

            if result_item is not None:
                if isinstance(result_item, int):
                    # a worker left after receiving the shutdown sentinel
                    p = self.processes.pop(result_item, None)
                    if p is not None:
                        p.join()
                else:
                    self.process_result_item(result_item)
                    exit_pid = result_item.exit_pid
                    del result_item
                    if exit_pid is not None:
                        # the worker reached max_tasks_per_child
                        p = self.processes.pop(exit_pid, None)
                        if p is not None:
                            p.join()
                        self.replace_exited_worker()
                    else:
                        executor = self.executor_reference()
                        if executor is not None:
                            executor._idle_worker_semaphore.release()
                        del executor

            if self.is_shutting_down():
                self.flag_executor_shutting_down()

                # queue what is left (cancelled items are dropped here)
                self.add_call_item_to_queue()

                if not self.pending_work_items:
                    self.join_executor_internals()
                    return

    def add_call_item_to_queue(self):
        # Fill the call queue (bounded) from the work id queue.
        while True:
            if self.call_queue.full():
                return
            try:
                work_id = self.work_ids_queue.get(block=False)
            except queue.Empty:
                return
            work_item = self.pending_work_items.get(work_id)
            if work_item is None:
                continue
            if work_item.future.set_running_or_notify_cancel():
                self.call_queue.put(_CallItem(work_id,
                                              work_item.fn,
                                              work_item.args,
                                              work_item.kwargs),
                                    block=True)
            else:
                del self.pending_work_items[work_id]

    def wait_result_broken_or_wakeup(self):
        """Wait for a result, a dead worker or a wakeup.

        Returns (result_item, is_broken, cause)."""
        result_reader = self.result_queue._reader
        assert not self.thread_wakeup._closed
        wakeup_reader = self.thread_wakeup._reader
        readers = [result_reader, wakeup_reader]
        workers = list(self.processes.values())
        worker_sentinels = [p.sentinel for p in workers]
        ready = mp.connection.wait(readers + worker_sentinels)

        cause = None
        is_broken = True
        result_item = None
        if result_reader in ready:
            try:
                result_item = result_reader.recv()
                is_broken = False
            except BaseException as exc:
                cause = format_exception(exc)
        elif wakeup_reader in ready:
            is_broken = False
        else:
            # a worker died without telling us
            for p in workers:
                if p.sentinel in ready:
                    p.join()
                    cause = [f"A process in the process pool (pid {p.pid}) "
                             f"terminated abruptly with exit code "
                             f"{p.exitcode}.\n"]
                    break

        self.thread_wakeup.clear()

        return result_item, is_broken, cause

    def process_result_item(self, result_item):
        work_item = self.pending_work_items.pop(result_item.work_id, None)
        if work_item is not None:
            if result_item.exception is not None:
                work_item.future.set_exception(result_item.exception)
            else:
                work_item.future.set_result(result_item.result)

    def replace_exited_worker(self):
        """Start a replacement for a worker that exited at its task limit,
        or fail the remaining work if that is impossible."""
        executor = self.executor_reference()
        if executor is None:
            if self.pending_work_items and not self.processes:
                self.fail_pending_work(
                    "The executor was garbage collected and no worker was "
                    "left to run the pending work.")
            return
        try:
            with self.shutdown_lock:
                if executor._force_shutting_down:
                    if self.pending_work_items and not self.processes:
                        self.fail_pending_work(
                            "The process pool was forcefully shut down "
                            "(terminate_workers()/kill_workers()) before the "
                            "pending work could run.")
                    return
                if (len(self.processes) < executor._max_workers
                        and (self.pending_work_items
                             or not executor._shutdown_thread)):
                    executor._spawn_process()
        except Exception:
            if not self.processes:
                # Nothing can run any more work: the pool is broken.
                message = ("A process in the process pool could not be "
                           "started to replace an exited worker.")
                executor._broken = message
                executor._shutdown_thread = True
                self.fail_pending_work(message)
        finally:
            del executor

    def fail_pending_work(self, message):
        for work_id, work_item in list(self.pending_work_items.items()):
            try:
                work_item.future.set_exception(BrokenProcessPool(message))
            except _base.InvalidStateError:
                pass
        self.pending_work_items.clear()
        while True:
            try:
                self.work_ids_queue.get_nowait()
            except queue.Empty:
                break

    def is_shutting_down(self):
        executor = self.executor_reference()
        return (_global_shutdown or executor is None
                or executor._shutdown_thread)

    def _terminate_broken(self, cause):
        executor = self.executor_reference()
        if executor is not None:
            executor._broken = ('A child process terminated '
                                'abruptly, the process pool is not '
                                'usable anymore')
            executor._shutdown_thread = True
            executor = None

        remote = None
        if cause is not None:
            remote = _RemoteTraceback(f"\n'''\n{''.join(cause)}'''")

        # every future gets its own exception (they get raised separately)
        for work_id, work_item in self.pending_work_items.items():
            bpe = BrokenProcessPool("A process in the process pool was "
                                    "terminated abruptly while the future was "
                                    "running or pending.")
            if remote is not None:
                bpe.__cause__ = remote
            try:
                work_item.future.set_exception(bpe)
            except _base.InvalidStateError:
                pass
            del work_item
        self.pending_work_items.clear()

        for p in self.processes.values():
            p.terminate()

        self.call_queue._terminate_broken()

        self._join_executor_internals(broken=True)

    def terminate_broken(self, cause):
        with self.shutdown_lock:
            self._terminate_broken(cause)

    def flag_executor_shutting_down(self):
        executor = self.executor_reference()
        if executor is not None:
            executor._shutdown_thread = True
            if executor._cancel_pending_futures:
                # cancel what has not started yet
                for work_id, work_item in list(self.pending_work_items.items()):
                    if work_item.future.cancel():
                        del self.pending_work_items[work_id]
                while True:
                    try:
                        self.work_ids_queue.get_nowait()
                    except queue.Empty:
                        break
                executor._cancel_pending_futures = False

    def join_executor_internals(self):
        with self.shutdown_lock:
            self._join_executor_internals()

    def _join_executor_internals(self, broken=False):
        if not broken:
            # one sentinel per worker still running
            for _ in range(self.get_n_children_alive()):
                self.call_queue.put(None)
        self.call_queue.close()
        self.call_queue.join_thread()
        self.thread_wakeup.close()
        for p in self.processes.values():
            if broken:
                p.terminate()
            p.join()

    def get_n_children_alive(self):
        return sum(p.is_alive() for p in self.processes.values())


_system_limits_checked = False
_system_limited = None


def _check_system_limits():
    global _system_limits_checked, _system_limited
    if _system_limits_checked:
        if _system_limited:
            raise NotImplementedError(_system_limited)
        return
    _system_limits_checked = True
    try:
        import multiprocessing.synchronize  # noqa: F401
    except ImportError:
        _system_limited = (
            "This Python build lacks multiprocessing.synchronize, usually due "
            "to named semaphores being unavailable on this platform."
        )
        raise NotImplementedError(_system_limited)
    try:
        nsems_max = os.sysconf("SC_SEM_NSEMS_MAX")
    except (AttributeError, ValueError):
        # unknown limit: assume it is enough
        return
    if nsems_max == -1:
        # no limit
        return
    if nsems_max >= 256:
        return
    _system_limited = ("system provides too few semaphores (%d"
                       " available, 256 necessary)" % nsems_max)
    raise NotImplementedError(_system_limited)


class BrokenProcessPool(_base.BrokenExecutor):
    """A worker process died abruptly (or could not be started): the pool
    cannot be used any more."""


class ProcessPoolExecutor(_base.Executor):
    def __init__(self, max_workers=None, mp_context=None,
                 initializer=None, initargs=(), *, max_tasks_per_child=None):
        """Create an executor running at most *max_workers* processes.

        *mp_context* selects the start method; *initializer(*initargs)* runs
        in each worker first; with *max_tasks_per_child* workers are
        replaced after that many tasks."""
        _check_system_limits()

        if max_workers is None:
            self._max_workers = os.process_cpu_count() or 1
            if sys.platform == 'win32':
                self._max_workers = min(_MAX_WINDOWS_WORKERS,
                                        self._max_workers)
        else:
            if max_workers <= 0:
                raise ValueError("max_workers must be greater than 0")
            elif (sys.platform == 'win32' and
                  max_workers > _MAX_WINDOWS_WORKERS):
                raise ValueError(
                    f"max_workers must be <= {_MAX_WINDOWS_WORKERS}")

            self._max_workers = max_workers

        if mp_context is None:
            if max_tasks_per_child is not None:
                mp_context = mp.get_context("spawn")
            else:
                mp_context = mp.get_context()
        self._mp_context = mp_context

        # fork() is only safe before threads exist: such pools start all
        # their workers up front.
        self._safe_to_dynamically_spawn_children = (
                self._mp_context.get_start_method(allow_none=False) != "fork")

        if initializer is not None and not callable(initializer):
            raise TypeError("initializer must be a callable")
        self._initializer = initializer
        self._initargs = initargs

        if max_tasks_per_child is not None:
            if not isinstance(max_tasks_per_child, int):
                raise TypeError("max_tasks_per_child must be an integer")
            elif max_tasks_per_child <= 0:
                raise ValueError("max_tasks_per_child must be >= 1")
            if self._mp_context.get_start_method(allow_none=False) == "fork":
                raise ValueError("max_tasks_per_child is incompatible with"
                                 " the 'fork' multiprocessing start method;"
                                 " supply a different mp_context.")
        self._max_tasks_per_child = max_tasks_per_child

        self._executor_manager_thread = None
        self._processes = {}

        self._shutdown_thread = False
        self._force_shutting_down = False
        self._shutdown_lock = threading.Lock()
        self._idle_worker_semaphore = threading.Semaphore(0)
        self._broken = False
        self._queue_count = 0
        self._pending_work_items = {}
        self._cancel_pending_futures = False

        self._executor_manager_thread_wakeup = _ThreadWakeup()

        queue_size = self._max_workers + EXTRA_QUEUED_CALLS
        self._call_queue = _SafeQueue(
            max_size=queue_size, ctx=self._mp_context,
            pending_work_items=self._pending_work_items,
            thread_wakeup=self._executor_manager_thread_wakeup)
        # a dead worker must not kill the feeder thread with EPIPE
        self._call_queue._ignore_epipe = True
        self._result_queue = mp_context.SimpleQueue()
        self._work_ids = queue.Queue()

    def _start_executor_manager_thread(self):
        if self._executor_manager_thread is None:
            if not self._safe_to_dynamically_spawn_children:
                self._launch_processes()
            self._executor_manager_thread = _ExecutorManagerThread(self)
            self._executor_manager_thread.start()
            _threads_wakeups[self._executor_manager_thread] = \
                self._executor_manager_thread_wakeup

    def _adjust_process_count(self):
        # an idle worker can take the new work
        if self._idle_worker_semaphore.acquire(blocking=False):
            return

        process_count = len(self._processes)
        if process_count < self._max_workers:
            self._spawn_process()

    def _launch_processes(self):
        assert not self._executor_manager_thread, (
                'Processes cannot be fork()ed after the thread has started, '
                'deadlock in the child processes could result.')
        for _ in range(len(self._processes), self._max_workers):
            self._spawn_process()

    def _spawn_process(self):
        p = _spawn_worker(self._mp_context, self._call_queue,
                          self._result_queue, self._initializer,
                          self._initargs, self._max_tasks_per_child)
        self._processes[p.pid] = p

    def submit(self, fn, /, *args, **kwargs):
        with self._shutdown_lock:
            if self._broken:
                raise BrokenProcessPool(self._broken)
            if self._shutdown_thread:
                raise RuntimeError('cannot schedule new futures after shutdown')
            if _global_shutdown:
                raise RuntimeError('cannot schedule new futures after '
                                   'interpreter shutdown')

            f = _base.Future()
            w = _WorkItem(f, fn, args, kwargs)

            self._pending_work_items[self._queue_count] = w
            self._work_ids.put(self._queue_count)
            self._queue_count += 1
            self._executor_manager_thread_wakeup.wakeup()

            if self._safe_to_dynamically_spawn_children:
                self._adjust_process_count()
            self._start_executor_manager_thread()
            return f
    submit.__doc__ = _base.Executor.submit.__doc__

    def map(self, fn, *iterables, timeout=None, chunksize=1, buffersize=None):
        """Like map(fn, *iterables), running in the worker processes.

        *chunksize* argument tuples are sent to a worker at a time."""
        if chunksize < 1:
            raise ValueError("chunksize must be >= 1.")

        results = super().map(partial(_process_chunk, fn),
                              itertools.batched(zip(*iterables), chunksize),
                              timeout=timeout,
                              buffersize=buffersize)
        return _chain_from_iterable_of_lists(results)

    def shutdown(self, wait=True, *, cancel_futures=False):
        with self._shutdown_lock:
            self._cancel_pending_futures = cancel_futures
            self._shutdown_thread = True
            if self._executor_manager_thread_wakeup is not None:
                self._executor_manager_thread_wakeup.wakeup()

        if self._executor_manager_thread is not None and wait:
            self._executor_manager_thread.join()
        if wait:
            # Everything has stopped: drop the references.  (Without wait
            # the manager thread may still need them to replace workers.)
            self._executor_manager_thread = None
            self._call_queue = None
            if self._result_queue is not None and wait:
                self._result_queue.close()
            self._result_queue = None
            self._processes = None
            self._executor_manager_thread_wakeup = None

    shutdown.__doc__ = _base.Executor.shutdown.__doc__

    def _force_shutdown(self, operation):
        """Shut down and terminate (or kill) the workers right away; work
        that has not finished is cancelled or fails."""
        if operation not in _SHUTDOWN_CALLBACK_OPERATION:
            raise ValueError(f"Unsupported operation: {operation!r}")

        processes = {}
        if self._processes:
            processes = self._processes.copy()

        with self._shutdown_lock:
            self._force_shutting_down = True
        self.shutdown(wait=False, cancel_futures=True)

        for proc in processes.values():
            try:
                if not proc.is_alive():
                    continue
            except ValueError:
                # closed process object
                continue

            try:
                if operation == _TERMINATE:
                    proc.terminate()
                elif operation == _KILL:
                    proc.kill()
            except ProcessLookupError:
                continue

    def terminate_workers(self):
        """Shut down, sending SIGTERM to the workers."""
        return self._force_shutdown(operation=_TERMINATE)

    def kill_workers(self):
        """Shut down, sending SIGKILL to the workers."""
        return self._force_shutdown(operation=_KILL)
