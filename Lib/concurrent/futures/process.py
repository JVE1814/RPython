"""ProcessPoolExecutor: calls run in a pool of worker processes.

How work moves:

    submit()  --> a pending entry (id -> future and call) plus its id on a
                  thread-safe id queue, then a wake-up
    manager   --> takes ids off that queue and puts the calls on the call
                  queue, a bounded multiprocessing queue, so that work which
                  has not reached a worker can still be cancelled
    worker    --> takes a call, runs it, puts the outcome on the result queue
    manager   --> completes the future

One manager thread per executor owns the result queue.  Besides results it
watches the workers' sentinels (a worker that dies breaks the pool),
replaces workers that retire after max_tasks_per_child calls, and carries
out the shutdown.  submit() and shutdown() wake it through a pipe.
"""

import itertools
import multiprocessing as mp
import multiprocessing.connection
import os
import queue
import sys
import threading
import weakref
from functools import partial
from multiprocessing.queues import Queue
from traceback import format_exception

from concurrent.futures import _base


# Calls that may wait on the call queue in addition to one per worker:
# enough to keep the workers busy, little enough that queued work can still
# be cancelled.
EXTRA_QUEUED_CALLS = 1

# Windows' WaitForMultipleObjects() limit, minus the two other handles the
# manager waits on.
_MAX_WINDOWS_WORKERS = 63 - 2

# Operations of ProcessPoolExecutor._force_shutdown().
_TERMINATE = "terminate"
_KILL = "kill"
_FORCE_OPERATIONS = frozenset((_TERMINATE, _KILL))


# --- interpreter exit -------------------------------------------------------
#
# At exit every manager thread is woken (it sees the flag and shuts its
# pool down) and joined, before the atexit hooks run.

_live_managers = weakref.WeakKeyDictionary()     # thread -> its wake-up pipe
_interpreter_exiting = False


def _python_exit():
    global _interpreter_exiting
    _interpreter_exiting = True
    managers = list(_live_managers.items())
    for _thread, wakeup in managers:
        wakeup.wakeup()
    for thread, _wakeup in managers:
        thread.join()


threading._register_atexit(_python_exit)


class _ThreadWakeup:
    """A pipe whose only purpose is to make the manager thread's wait
    return."""

    def __init__(self):
        self._closed = False
        self._lock = threading.Lock()
        self._reader, self._writer = mp.Pipe(duplex=False)

    def close(self):
        with self._lock:
            if self._closed:
                return
            self._closed = True
            self._writer.close()
            self._reader.close()

    def wakeup(self):
        with self._lock:
            if not self._closed:
                self._writer.send_bytes(b"")

    def clear(self):
        # Called by the manager thread only, without the lock: taking it here
        # could deadlock against a submit() blocked writing to a full pipe.
        if self._closed:
            raise RuntimeError('operation on closed _ThreadWakeup')
        while self._reader.poll():
            self._reader.recv_bytes()


# --- what travels between the processes -------------------------------------

class _RemoteTraceback(Exception):
    """Text of the traceback of an exception raised in a worker; becomes the
    __cause__ of the exception re-raised in the parent."""

    def __init__(self, tb):
        self.tb = tb

    def __str__(self):
        return self.tb


def _quoted_traceback(lines):
    return '\n"""\n%s"""' % ''.join(lines)


def _rebuild_exc(exc, tb):
    exc.__cause__ = _RemoteTraceback(tb)
    return exc


class _PicklableError:
    """Carries a worker's exception to the parent together with its
    formatted traceback (traceback objects cannot be pickled)."""

    def __init__(self, exc, tb):
        self.tb = _quoted_traceback(format_exception(type(exc), exc, tb))
        exc.__traceback__ = None
        self.exc = exc

    def __reduce__(self):
        return _rebuild_exc, (self.exc, self.tb)


class _PendingCall:
    # In the parent: a submitted call and its future.
    __slots__ = ('future', 'fn', 'args', 'kwargs')

    def __init__(self, future, fn, args, kwargs):
        self.future = future
        self.fn = fn
        self.args = args
        self.kwargs = kwargs


class _Task:
    # Parent -> worker: one call to make.
    def __init__(self, work_id, fn, args, kwargs):
        self.work_id = work_id
        self.fn = fn
        self.args = args
        self.kwargs = kwargs


class _Outcome:
    # Worker -> parent: what a call produced.  exit_pid is set on the last
    # outcome of a worker that retires (max_tasks_per_child reached).
    def __init__(self, work_id, result=None, error=None, exit_pid=None):
        self.work_id = work_id
        self.result = result
        self.error = error
        self.exit_pid = exit_pid


class _CallQueue(Queue):
    """The call queue.  A call that cannot be pickled fails its own future
    instead of taking down the queue's feeder thread."""

    def __init__(self, max_size=0, *, ctx, pending, wakeup):
        self._pending = pending
        self._wakeup = wakeup
        super().__init__(max_size, ctx=ctx)

    def _on_queue_feeder_error(self, e, obj):
        if not isinstance(obj, _Task):
            return super()._on_queue_feeder_error(e, obj)
        e.__cause__ = _RemoteTraceback(
            _quoted_traceback(format_exception(type(e), e, e.__traceback__)))
        entry = self._pending.pop(obj.work_id, None)
        self._wakeup.wakeup()
        if entry is not None:
            entry.future.set_exception(e)


# --- the worker process -----------------------------------------------------

def _run_batch(fn, batch):
    # One map() task: fn applied to a batch of argument tuples.
    return [fn(*args) for args in batch]


def _flatten_batches(batches):
    # map() results batch by batch, releasing each value once yielded.
    for batch in batches:
        batch.reverse()
        while batch:
            yield batch.pop()


def _report(result_queue, outcome):
    try:
        result_queue.put(outcome)
    except BaseException as e:
        # The result itself could not be pickled: report that instead.
        result_queue.put(_Outcome(outcome.work_id,
                                  error=_PicklableError(e, e.__traceback__),
                                  exit_pid=outcome.exit_pid))


def _worker_main(call_queue, result_queue, initializer, initargs,
                 max_tasks=None):
    """Body of a worker process.  It stops on a None task (and answers with
    its pid) or after *max_tasks* calls (its last outcome carries its
    pid).  A failing initializer makes it exit silently, which the manager
    treats as a broken pool."""
    if initializer is not None:
        try:
            initializer(*initargs)
        except BaseException:
            _base.LOGGER.critical('Exception in initializer:', exc_info=True)
            return
    calls_left = max_tasks
    while True:
        task = call_queue.get(block=True)
        if task is None:
            result_queue.put(os.getpid())
            return
        exit_pid = None
        if calls_left is not None:
            calls_left -= 1
            if calls_left <= 0:
                exit_pid = os.getpid()
        try:
            value = task.fn(*task.args, **task.kwargs)
        except BaseException as e:
            outcome = _Outcome(task.work_id,
                               error=_PicklableError(e, e.__traceback__),
                               exit_pid=exit_pid)
        else:
            outcome = _Outcome(task.work_id, result=value, exit_pid=exit_pid)
            del value
        _report(result_queue, outcome)
        # Drop the call and its outcome before waiting for the next task.
        del task, outcome
        if exit_pid is not None:
            return


def _spawn_worker(mp_context, call_queue, result_queue, initializer, initargs,
                  max_tasks_per_child):
    """Start one worker process and return it."""
    worker = mp_context.Process(
        target=_worker_main,
        args=(call_queue, result_queue, initializer, initargs,
              max_tasks_per_child))
    worker.start()
    return worker


# --- the manager thread -----------------------------------------------------

_BROKEN_POOL = ('A child process terminated abruptly, the process pool is '
                'not usable anymore')
_BROKEN_FUTURE = ('A process in the process pool was terminated abruptly '
                  'while the future was running or pending.')


class _ExecutorManagerThread(threading.Thread):
    """Feeds calls to the workers, completes futures, keeps the pool
    staffed and shuts it down.  It holds the executor only weakly: an
    executor that is garbage collected shuts its pool down."""

    def __init__(self, executor):
        wakeup = executor._executor_manager_thread_wakeup
        self.thread_wakeup = wakeup
        self.shutdown_lock = executor._shutdown_lock
        debug = mp.util.debug

        def on_executor_collected(_ref):
            debug('Executor collected: triggering callback for'
                  ' QueueManager wakeup')
            wakeup.wakeup()

        self.executor_reference = weakref.ref(executor, on_executor_collected)
        self.processes = executor._processes
        self.call_queue = executor._call_queue
        self.result_queue = executor._result_queue
        self.queued_ids = executor._queued_ids
        self.max_tasks_per_child = executor._max_tasks_per_child
        self.pending = executor._pending
        super().__init__()

    def run(self):
        while True:
            try:
                self._feed_workers()
            except BaseException as exc:
                # E.g. the call queue's feeder thread cannot be started
                # during interpreter shutdown.
                self.terminate_broken(format_exception(exc))
                return
            event, payload = self._wait_for_event()
            if event == 'broken':
                self.terminate_broken(payload)
                return
            if event == 'result':
                self._deliver(payload)
                del payload
            if self._shutting_down():
                self._begin_shutdown()
                # Hand over what is still queued; cancelled calls drop out.
                self._feed_workers()
                if not self.pending:
                    with self.shutdown_lock:
                        self._close_pool()
                    return

    # -- moving calls and results --------------------------------------------

    def _feed_workers(self):
        while not self.call_queue.full():
            try:
                work_id = self.queued_ids.get(block=False)
            except queue.Empty:
                return
            entry = self.pending.get(work_id)
            if entry is None:
                continue
            if entry.future.set_running_or_notify_cancel():
                self.call_queue.put(
                    _Task(work_id, entry.fn, entry.args, entry.kwargs),
                    block=True)
            else:
                del self.pending[work_id]

    def _wait_for_event(self):
        # -> ('result', item), ('wakeup', None) or ('broken', cause lines)
        results = self.result_queue._reader
        assert not self.thread_wakeup._closed
        wakeups = self.thread_wakeup._reader
        workers = list(self.processes.values())
        ready = mp.connection.wait([results, wakeups]
                                   + [w.sentinel for w in workers])
        if results in ready:
            try:
                event = ('result', results.recv())
            except BaseException as exc:
                event = ('broken', format_exception(exc))
        elif wakeups in ready:
            event = ('wakeup', None)
        else:
            event = ('broken', None)
            for worker in workers:
                if worker.sentinel in ready:
                    worker.join()
                    event = ('broken', [
                        f"A process in the process pool (pid {worker.pid}) "
                        f"terminated abruptly with exit code "
                        f"{worker.exitcode}.\n"])
                    break
        self.thread_wakeup.clear()
        return event

    def _deliver(self, item):
        if isinstance(item, int):
            # A worker acknowledging the stop sentinel, about to exit.
            self._reap(item)
            return
        entry = self.pending.pop(item.work_id, None)
        if entry is not None:
            if item.error is not None:
                entry.future.set_exception(item.error)
            else:
                entry.future.set_result(item.result)
        if item.exit_pid is not None:
            self._reap(item.exit_pid)
            self._replace_retired_worker()
        else:
            executor = self.executor_reference()
            if executor is not None:
                executor._idle_workers.release()
            del executor

    def _reap(self, pid):
        worker = self.processes.pop(pid, None)
        if worker is not None:
            worker.join()

    # -- keeping the pool staffed --------------------------------------------

    def _replace_retired_worker(self):
        # A worker left after max_tasks_per_child calls.  Start another one
        # if there is (or may be) work for it; if that cannot happen and
        # nobody is left to run the queued calls, fail them.
        executor = self.executor_reference()
        if executor is None:
            if self.pending and not self.processes:
                self._fail_pending(
                    "The executor was garbage collected and no worker was "
                    "left to run the pending work.")
            return
        try:
            with self.shutdown_lock:
                if executor._force_shutting_down:
                    # A replacement would escape terminate/kill_workers().
                    if self.pending and not self.processes:
                        self._fail_pending(
                            "The process pool was forcefully shut down "
                            "(terminate_workers()/kill_workers()) before the "
                            "pending work could run.")
                        # Nothing will run work any more: let this thread
                        # wind the pool down instead of waiting forever.
                        executor._shutting_down = True
                    return
                if (len(self.processes) < executor._max_workers
                        and (self.pending or not executor._shutting_down)):
                    executor._spawn_process()
        except Exception:
            if not self.processes:
                message = ("A process in the process pool could not be "
                           "started to replace an exited worker.")
                executor._broken = message
                executor._shutting_down = True
                self._fail_pending(message)
        finally:
            del executor

    def _fail_pending(self, message):
        for entry in list(self.pending.values()):
            try:
                entry.future.set_exception(BrokenProcessPool(message))
            except _base.InvalidStateError:
                pass
        self.pending.clear()
        self._drain_queued_ids()

    def _drain_queued_ids(self):
        while True:
            try:
                self.queued_ids.get_nowait()
            except queue.Empty:
                return

    # -- shutting down -------------------------------------------------------

    def _shutting_down(self):
        executor = self.executor_reference()
        return (_interpreter_exiting or executor is None
                or executor._shutting_down)

    def _begin_shutdown(self):
        executor = self.executor_reference()
        if executor is None:
            return
        executor._shutting_down = True
        if executor._cancel_on_shutdown:
            # Calls that have not started are cancelled.
            for work_id, entry in list(self.pending.items()):
                if entry.future.cancel():
                    del self.pending[work_id]
            self._drain_queued_ids()
            executor._cancel_on_shutdown = False

    def _close_pool(self, broken=False):
        # Caller holds the shutdown lock.
        if not broken:
            alive = sum(w.is_alive() for w in self.processes.values())
            for _ in range(alive):
                self.call_queue.put(None)       # one stop sentinel each
        self.call_queue.close()
        self.call_queue.join_thread()
        self.thread_wakeup.close()
        for worker in self.processes.values():
            if broken:
                worker.terminate()
            worker.join()

    def terminate_broken(self, cause):
        """Fail every pending future, stop the workers and mark the executor
        broken.  *cause* (formatted lines, or None) becomes each failure's
        __cause__."""
        with self.shutdown_lock:
            executor = self.executor_reference()
            if executor is not None:
                executor._broken = _BROKEN_POOL
                executor._shutting_down = True
            del executor
            remote = None
            if cause is not None:
                remote = _RemoteTraceback(f"\n'''\n{''.join(cause)}'''")
            # A separate exception object per future: each may be raised in
            # a different thread.
            for entry in self.pending.values():
                error = BrokenProcessPool(_BROKEN_FUTURE)
                if remote is not None:
                    error.__cause__ = remote
                try:
                    entry.future.set_exception(error)
                except _base.InvalidStateError:
                    pass
            self.pending.clear()
            for worker in self.processes.values():
                worker.terminate()
            self.call_queue._terminate_broken()
            self._close_pool(broken=True)


# --- platform check ---------------------------------------------------------

_system_limits_checked = False
_system_limited = None


def _check_system_limits():
    """Raise NotImplementedError if this system cannot run a process pool
    (no named semaphores, or too few of them)."""
    global _system_limits_checked, _system_limited
    if not _system_limits_checked:
        _system_limits_checked = True
        _system_limited = _find_system_limit()
    if _system_limited:
        raise NotImplementedError(_system_limited)


def _find_system_limit():
    try:
        import multiprocessing.synchronize  # noqa: F401
    except ImportError:
        return ("This Python build lacks multiprocessing.synchronize, usually "
                "due to named semaphores being unavailable on this platform.")
    try:
        available = os.sysconf("SC_SEM_NSEMS_MAX")
    except (AttributeError, ValueError):
        return None             # unknown: assume enough
    if available == -1 or available >= 256:
        return None
    return ("system provides too few semaphores (%d"
            " available, 256 necessary)" % available)


class BrokenProcessPool(_base.BrokenExecutor):
    """A worker process died abruptly (or could not be started); the pool
    cannot be used any more."""


# --- the executor -----------------------------------------------------------

class ProcessPoolExecutor(_base.Executor):

    def __init__(self, max_workers=None, mp_context=None,
                 initializer=None, initargs=(), *, max_tasks_per_child=None):
        """Run calls in at most *max_workers* processes (default: the
        number of CPUs this process may use).

        *mp_context* is the multiprocessing context that starts the workers.
        *initializer*(*initargs) runs first in every worker.  With
        *max_tasks_per_child* a worker is replaced after that many calls
        (this needs a start method other than fork; spawn is the default
        then)."""
        _check_system_limits()
        self._max_workers = self._worker_limit(max_workers)
        if mp_context is None:
            mp_context = mp.get_context(
                "spawn" if max_tasks_per_child is not None else None)
        self._mp_context = mp_context
        forking = mp_context.get_start_method(allow_none=False) == "fork"
        # With fork, workers must exist before any thread is started, so
        # they are all created up front; otherwise they are added as needed.
        self._spawn_on_demand = not forking
        if initializer is not None and not callable(initializer):
            raise TypeError("initializer must be a callable")
        self._initializer = initializer
        self._initargs = initargs
        if max_tasks_per_child is not None:
            if not isinstance(max_tasks_per_child, int):
                raise TypeError("max_tasks_per_child must be an integer")
            if max_tasks_per_child <= 0:
                raise ValueError("max_tasks_per_child must be >= 1")
            if forking:
                raise ValueError("max_tasks_per_child is incompatible with"
                                 " the 'fork' multiprocessing start method;"
                                 " supply a different mp_context.")
        self._max_tasks_per_child = max_tasks_per_child

        self._executor_manager_thread = None
        self._processes = {}            # pid -> worker
        self._shutting_down = False
        self._force_shutting_down = False
        self._shutdown_lock = threading.Lock()
        self._idle_workers = threading.Semaphore(0)
        self._broken = False
        self._next_id = 0
        self._pending = {}              # work id -> _PendingCall
        self._cancel_on_shutdown = False
        self._executor_manager_thread_wakeup = _ThreadWakeup()
        self._call_queue = _CallQueue(
            max_size=self._max_workers + EXTRA_QUEUED_CALLS,
            ctx=mp_context, pending=self._pending,
            wakeup=self._executor_manager_thread_wakeup)
        # Writing to a worker that died must not kill the feeder thread; the
        # manager notices the death through the sentinel.
        self._call_queue._ignore_epipe = True
        self._result_queue = mp_context.SimpleQueue()
        self._queued_ids = queue.Queue()

    @staticmethod
    def _worker_limit(max_workers):
        if max_workers is None:
            count = os.process_cpu_count() or 1
            if sys.platform == 'win32':
                count = min(_MAX_WINDOWS_WORKERS, count)
            return count
        if max_workers <= 0:
            raise ValueError("max_workers must be greater than 0")
        if sys.platform == 'win32' and max_workers > _MAX_WINDOWS_WORKERS:
            raise ValueError(f"max_workers must be <= {_MAX_WINDOWS_WORKERS}")
        return max_workers

    # -- workers -------------------------------------------------------------

    def _start_executor_manager_thread(self):
        if self._executor_manager_thread is not None:
            return
        if not self._spawn_on_demand:
            self._launch_processes()
        thread = _ExecutorManagerThread(self)
        thread.start()
        self._executor_manager_thread = thread
        _live_managers[thread] = self._executor_manager_thread_wakeup

    def _adjust_process_count(self):
        # An idle worker will pick the new call up; otherwise add one.
        if self._idle_workers.acquire(blocking=False):
            return
        if len(self._processes) < self._max_workers:
            self._spawn_process()

    def _launch_processes(self):
        assert not self._executor_manager_thread, (
            'Processes cannot be fork()ed after the thread has started, '
            'deadlock in the child processes could result.')
        while len(self._processes) < self._max_workers:
            self._spawn_process()

    def _spawn_process(self):
        worker = _spawn_worker(self._mp_context, self._call_queue,
                               self._result_queue, self._initializer,
                               self._initargs, self._max_tasks_per_child)
        self._processes[worker.pid] = worker

    # -- the Executor API ----------------------------------------------------

    def submit(self, fn, /, *args, **kwargs):
        with self._shutdown_lock:
            if self._broken:
                raise BrokenProcessPool(self._broken)
            if self._shutting_down:
                raise RuntimeError('cannot schedule new futures after shutdown')
            if _interpreter_exiting:
                raise RuntimeError('cannot schedule new futures after '
                                   'interpreter shutdown')
            future = _base.Future()
            work_id = self._next_id
            self._next_id += 1
            self._pending[work_id] = _PendingCall(future, fn, args, kwargs)
            self._queued_ids.put(work_id)
            self._executor_manager_thread_wakeup.wakeup()
            if self._spawn_on_demand:
                self._adjust_process_count()
            self._start_executor_manager_thread()
            return future
    submit.__doc__ = _base.Executor.submit.__doc__

    def map(self, fn, *iterables, timeout=None, chunksize=1, buffersize=None):
        """Like the builtin map(), but the calls run in the worker processes,
        *chunksize* argument tuples per round trip."""
        if chunksize < 1:
            raise ValueError("chunksize must be >= 1.")
        batches = super().map(partial(_run_batch, fn),
                              itertools.batched(zip(*iterables), chunksize),
                              timeout=timeout, buffersize=buffersize)
        return _flatten_batches(batches)

    def shutdown(self, wait=True, *, cancel_futures=False):
        with self._shutdown_lock:
            self._cancel_on_shutdown = cancel_futures
            self._shutting_down = True
            if self._executor_manager_thread_wakeup is not None:
                self._executor_manager_thread_wakeup.wakeup()
        if not wait:
            # The manager thread may still need everything (e.g. to replace
            # a retiring worker for the remaining work).
            return
        if self._executor_manager_thread is not None:
            self._executor_manager_thread.join()
        self._executor_manager_thread = None
        self._call_queue = None
        if self._result_queue is not None:
            self._result_queue.close()
        self._result_queue = None
        self._processes = None
        self._executor_manager_thread_wakeup = None
    shutdown.__doc__ = _base.Executor.shutdown.__doc__

    def _force_shutdown(self, operation):
        """Shut down without waiting and signal every live worker; calls that
        have not finished are cancelled or fail."""
        if operation not in _FORCE_OPERATIONS:
            raise ValueError(f"Unsupported operation: {operation!r}")
        workers = list(self._processes.values()) if self._processes else []
        with self._shutdown_lock:
            self._force_shutting_down = True
        self.shutdown(wait=False, cancel_futures=True)
        for worker in workers:
            try:
                if not worker.is_alive():
                    continue
                if operation == _TERMINATE:
                    worker.terminate()
                else:
                    worker.kill()
            except (ValueError, ProcessLookupError):
                # A closed process object, or a worker already gone.
                continue

    def terminate_workers(self):
        """Shut down, sending SIGTERM to the workers that are still alive."""
        return self._force_shutdown(operation=_TERMINATE)

    def kill_workers(self):
        """Shut down, sending SIGKILL to the workers that are still alive."""
        return self._force_shutdown(operation=_KILL)
