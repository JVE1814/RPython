#
# multiprocessing.pool -- Pool (worker processes) and ThreadPool (the same
# interface over threads).
#
# Layout of a running pool:
#
#   caller threads -- task batches --> taskqueue (queue.SimpleQueue)
#   feeder thread  -- one task at a time --> inqueue --> workers
#   workers        -- (job, index, outcome) --> outqueue
#   supervisor     -- reads outqueue, hands outcomes to the result objects
#                     registered in the cache, replaces workers that exited
#                     and decides when the pool has finished
#
# The helper threads never see the Pool object itself, only the pieces they
# need, so an abandoned pool can be collected; its util.Finalize then shuts
# everything down.
#

__all__ = ['Pool', 'ThreadPool']

import collections
import functools
import itertools
import os
import queue
import threading
import traceback
import types
import warnings

from . import util
from . import get_context, TimeoutError
from .connection import wait

# Pool states (also the values of Pool._state)
INIT = "INIT"
RUN = "RUN"
CLOSE = "CLOSE"
TERMINATE = "TERMINATE"

# every result object gets a distinct job number
job_counter = itertools.count()

# results read from the out-queue in one go before the supervisor looks at
# the pool again
_RESULT_BATCH = 64


def mapstar(args):
    """Run one map() chunk: args is (func, items)."""
    func, items = args
    return list(map(func, items))


def starmapstar(args):
    """Run one starmap() chunk: args is (func, argument_tuples)."""
    func, items = args
    return list(itertools.starmap(func, items))


#
# Exceptions crossing the process boundary
#

class RemoteTraceback(Exception):
    """The formatted traceback of an exception raised in a worker; it becomes
    the __cause__ of the exception re-raised in the parent."""

    def __init__(self, tb):
        self.tb = tb

    def __str__(self):
        return self.tb


class ExceptionWithTraceback:
    """Pickles as the wrapped exception, with the worker's traceback text
    attached as a RemoteTraceback cause when it is unpickled."""

    def __init__(self, exc, tb):
        lines = traceback.format_exception(type(exc), exc, tb)
        self.exc = exc
        self.tb = '\n"""\n' + ''.join(lines) + '"""'

    def __reduce__(self):
        return rebuild_exc, (self.exc, self.tb)


def rebuild_exc(exc, tb):
    exc.__cause__ = RemoteTraceback(tb)
    return exc


class MaybeEncodingError(Exception):
    """A worker could not send a result back (it failed to pickle)."""

    def __init__(self, exc, value):
        self.exc = repr(exc)
        self.value = repr(value)
        super().__init__(self.exc, self.value)

    def __str__(self):
        return ("Error sending result: '%s'. Reason: '%s'"
                % (self.value, self.exc))

    def __repr__(self):
        return '<%s: %s>' % (type(self).__name__, self)


#
# The worker loop
#

def worker(inqueue, outqueue, initializer=None, initargs=(), maxtasks=None,
           wrap_exception=False):
    """Body of a pool worker: run tasks from inqueue until it delivers None
    (or is closed), or until maxtasks tasks are done."""
    if maxtasks is not None and not (type(maxtasks) is int and maxtasks >= 1):
        raise AssertionError("Maxtasks {!r} is not valid".format(maxtasks))
    take, give = inqueue.get, outqueue.put
    # a worker process inherited both ends of both pipes; it only reads the
    # task pipe and only writes the result pipe
    if hasattr(inqueue, '_writer'):
        inqueue._writer.close()
        outqueue._reader.close()
    if initializer is not None:
        initializer(*initargs)

    done = 0
    while maxtasks is None or done < maxtasks:
        try:
            task = take()
        except (EOFError, OSError):
            util.debug('worker got EOFError or OSError -- exiting')
            break
        if task is None:
            util.debug('worker got sentinel -- exiting')
            break
        job, index, func, args, kwds = task
        task = None
        try:
            outcome = (True, func(*args, **kwds))
        except Exception as exc:
            if wrap_exception and func is not _helper_reraises_exception:
                exc = ExceptionWithTraceback(exc, exc.__traceback__)
            outcome = (False, exc)
            exc = None
        func = args = kwds = None
        try:
            give((job, index, outcome))
        except Exception as exc:
            util.debug("Possible encoding error while sending result: %s"
                       % (MaybeEncodingError(exc, outcome[1]),))
            give((job, index, (False, MaybeEncodingError(exc, outcome[1]))))
            exc = None
        outcome = None
        done += 1
    util.debug('worker exiting after %d tasks' % done)


def _helper_reraises_exception(ex):
    """Task standing in for an exception raised by the caller's iterable."""
    raise ex


#
# State shared by the pool and its helper threads
#

class _Phase:
    """Mutable holder of the state the helper threads act on."""
    __slots__ = ('value',)

    def __init__(self):
        self.value = RUN


class _PoolCache(dict):
    """job number -> pending result object.

    When the last pending job goes away after the pool stopped accepting
    work, the supervisor is woken up so that it can finish."""

    def __init__(self, /, *args, phase, wake, **kwds):
        super().__init__(*args, **kwds)
        self._phase = phase
        self._wake = wake

    def __delitem__(self, job):
        super().__delitem__(job)
        # delete first, then look at the phase: close() sets the phase before
        # it wakes the supervisor, so one of the two always sees the other
        if not self and self._phase.value != RUN:
            self._wake()


def _start_workers(ctx, Process, processes, pool, inqueue, outqueue,
                   initializer, initargs, maxtasksperchild, wrap_exception):
    """Start workers until the pool has `processes` of them."""
    while len(pool) < processes:
        w = Process(ctx, target=worker,
                    args=(inqueue, outqueue, initializer, initargs,
                          maxtasksperchild, wrap_exception))
        w.name = w.name.replace('Process', 'PoolWorker')
        w.daemon = True
        w.start()
        pool.append(w)
        util.debug('added worker')


def _reap_workers(pool):
    """Join and drop the workers that have exited; True if there were any."""
    gone = [w for w in pool if w.exitcode is not None]
    for w in gone:
        util.debug('cleaning up worker %s' % w.name)
        w.join()
        pool.remove(w)
    return bool(gone)


def _deliver(cache, outcome):
    job, index, value = outcome
    entry = cache.get(job)
    if entry is not None:
        entry._set(index, value)


#
# Pool
#

class Pool(object):
    """A pool of worker processes to which jobs can be submitted."""

    _wrap_exception = True

    @staticmethod
    def Process(ctx, *args, **kwds):
        return ctx.Process(*args, **kwds)

    def __init__(self, processes=None, initializer=None, initargs=(),
                 maxtasksperchild=None, context=None):
        # set first so that __del__ works whatever goes wrong below
        self._pool = []
        self._state = INIT

        if processes is None:
            processes = os.process_cpu_count() or 1
        if processes < 1:
            raise ValueError("Number of processes must be at least 1")
        if maxtasksperchild is not None and (
                not isinstance(maxtasksperchild, int) or maxtasksperchild <= 0):
            raise ValueError("maxtasksperchild must be a positive int or None")
        if initializer is not None and not callable(initializer):
            raise TypeError('initializer must be a callable')

        self._ctx = context or get_context()
        self._processes = processes
        self._initializer = initializer
        self._initargs = initargs
        self._maxtasksperchild = maxtasksperchild
        self._taskqueue = queue.SimpleQueue()
        self._phase = _Phase()
        self._setup_queues()
        self._wake = self._make_wake()
        self._cache = _PoolCache(phase=self._phase, wake=self._wake)

        try:
            self._repopulate_pool()
        except Exception:
            for w in self._pool:
                if w.exitcode is None:
                    w.terminate()
            for w in self._pool:
                w.join()
            raise

        self._supervisor = threading.Thread(
            target=Pool._supervise,
            args=(self._phase, self._cache, self._taskqueue, self._pool,
                  self._make_gather(), self._make_refill()),
            name='multiprocessing.pool.supervisor', daemon=True)
        self._feeder = threading.Thread(
            target=Pool._feed,
            args=(self._phase, self._cache, self._taskqueue, self._pool,
                  self._send_task),
            name='multiprocessing.pool.feeder', daemon=True)
        self._supervisor.start()
        self._feeder.start()

        self._terminate = util.Finalize(
            self, type(self)._terminate_pool,
            args=(self._phase, self._taskqueue, self._wake, self._pool,
                  self._supervisor, self._feeder, self._inqueue),
            exitpriority=15)
        self._state = RUN

    # warnings.warn is bound as a default: at interpreter shutdown the module
    # global may already be gone
    def __del__(self, _warn=warnings.warn, RUN=RUN):
        if self._state == RUN:
            _warn(f"unclosed running multiprocessing pool {self!r}",
                  ResourceWarning, source=self)

    def __repr__(self):
        cls = type(self)
        return (f'<{cls.__module__}.{cls.__qualname__} '
                f'state={self._state} pool_size={len(self._pool)}>')

    def __reduce__(self):
        raise NotImplementedError(
            'pool objects cannot be passed between processes or pickled')

    def __enter__(self):
        self._check_running()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.terminate()

    # -- the parts that differ between process and thread pools

    def _setup_queues(self):
        self._inqueue = self._ctx.SimpleQueue()
        self._outqueue = self._ctx.SimpleQueue()
        self._notifier = self._ctx.SimpleQueue()
        self._send_task = self._inqueue._writer.send

    def _make_wake(self):
        return functools.partial(self._notifier.put, None)

    def _make_gather(self):
        """A function that blocks until something happens and returns the
        outcomes that arrived meanwhile (possibly none)."""
        results = self._outqueue._reader
        recv, ready_now = results.recv, results.poll
        notices = self._notifier._reader
        pool = self._pool

        def gather():
            ready = wait([results, notices] + [w.sentinel for w in pool])
            if notices in ready:
                while notices.poll():
                    notices.recv_bytes()
            got = []
            if results in ready:
                got.append(recv())
                while len(got) < _RESULT_BATCH and ready_now():
                    got.append(recv())
            return got
        return gather

    @staticmethod
    def _stop_workers(pool, inqueue, feeder):
        """Called while terminating, after the supervisor has finished: kill
        the workers and drain the task pipe so that a feeder blocked on it
        can notice that the pool is going away."""
        for w in pool:
            if w.exitcode is None:
                w.terminate()
        reader = inqueue._reader
        while feeder.is_alive():
            try:
                if reader.poll(0.05):
                    reader.recv_bytes()
            except (OSError, EOFError):
                break

    # -- worker management

    def _make_refill(self):
        return functools.partial(
            _start_workers, self._ctx, self.Process, self._processes,
            self._pool, self._inqueue, self._outqueue, self._initializer,
            self._initargs, self._maxtasksperchild, self._wrap_exception)

    def _repopulate_pool(self):
        """Top the pool up to its size (after workers exited)."""
        self._make_refill()()

    # -- submitting work

    def _check_running(self):
        if self._state != RUN:
            raise ValueError("Pool not running")

    def apply(self, func, args=(), kwds={}):
        """Run func(*args, **kwds) in a worker and return its result."""
        return self.apply_async(func, args, kwds).get()

    def apply_async(self, func, args=(), kwds={}, callback=None,
                    error_callback=None):
        """Asynchronous apply(); returns an ApplyResult."""
        self._check_running()
        result = ApplyResult(self, callback, error_callback)
        self._taskqueue.put(([(result._job, 0, func, args, kwds)], None))
        return result

    def map(self, func, iterable, chunksize=None):
        """Parallel equivalent of list(map(func, iterable))."""
        return self._map_async(func, iterable, mapstar, chunksize).get()

    def starmap(self, func, iterable, chunksize=None):
        """Like map(), but each item of iterable is an argument tuple."""
        return self._map_async(func, iterable, starmapstar, chunksize).get()

    def map_async(self, func, iterable, chunksize=None, callback=None,
                  error_callback=None):
        """Asynchronous map(); returns a MapResult."""
        return self._map_async(func, iterable, mapstar, chunksize, callback,
                               error_callback)

    def starmap_async(self, func, iterable, chunksize=None, callback=None,
                      error_callback=None):
        """Asynchronous starmap(); returns a MapResult."""
        return self._map_async(func, iterable, starmapstar, chunksize,
                               callback, error_callback)

    def _map_async(self, func, iterable, runner, chunksize=None, callback=None,
                   error_callback=None):
        self._check_running()
        if not hasattr(iterable, '__len__'):
            iterable = list(iterable)
        count = len(iterable)
        if chunksize is None:
            # about four chunks per worker
            chunksize = -(-count // (len(self._pool) * 4)) if count else 0
        result = MapResult(self, chunksize, count, callback,
                           error_callback=error_callback)
        if count:
            tasks = [(result._job, n, runner, (chunk,), {})
                     for n, chunk in enumerate(_chunks(func, iterable,
                                                       chunksize))]
            self._taskqueue.put((tasks, None))
        return result

    def imap(self, func, iterable, chunksize=1):
        """Lazy map(): yields results in order as they become available."""
        self._check_running()
        return self._imap(IMapIterator, func, iterable, chunksize, '{0:n}')

    def imap_unordered(self, func, iterable, chunksize=1):
        """Like imap(), but yields results in completion order."""
        self._check_running()
        return self._imap(IMapUnorderedIterator, func, iterable, chunksize,
                          '{0!r}')

    def _imap(self, iterator_class, func, iterable, chunksize, spec):
        result = iterator_class(self)
        if chunksize == 1:
            tasks = _guarded_tasks(result._job, ((func, (x,)) for x in iterable))
            self._taskqueue.put((tasks, result._set_length))
            return result
        if chunksize < 1:
            raise ValueError(
                ("Chunksize must be 1+, not " + spec).format(chunksize))
        chunks = ((mapstar, (chunk,))
                  for chunk in _chunks(func, iterable, chunksize))
        self._taskqueue.put((_guarded_tasks(result._job, chunks),
                             result._set_length))
        return (item for chunk in result for item in chunk)

    # -- shutting down

    def close(self):
        """Accept no more work; the workers exit once the queued work is
        done."""
        util.debug('closing pool')
        if self._state == RUN:
            self._state = CLOSE
            self._phase.value = CLOSE
            self._wake()

    def terminate(self):
        """Stop at once, without finishing outstanding work."""
        util.debug('terminating pool')
        self._state = TERMINATE
        self._terminate()

    def join(self):
        """Wait for the helper threads and the workers (after close() or
        terminate())."""
        util.debug('joining pool')
        if self._state == RUN:
            raise ValueError("Pool is still running")
        if self._state not in (CLOSE, TERMINATE):
            raise ValueError("In unknown state")
        self._supervisor.join()
        self._feeder.join()
        for w in self._pool:
            w.join()

    @classmethod
    def _terminate_pool(cls, phase, taskqueue, wake, pool, supervisor, feeder,
                        inqueue):
        util.debug('finalizing pool')
        here = threading.current_thread()
        phase.value = TERMINATE
        wake()
        taskqueue.put(None)
        # the supervisor must be gone before the workers are stopped, or it
        # would replace them
        if supervisor is not here and supervisor.ident is not None:
            supervisor.join()
        if feeder.ident is not None:
            cls._stop_workers(pool, inqueue, feeder)
            if feeder is not here:
                feeder.join()
        for w in pool:
            if w.is_alive():
                util.debug('joining worker %s' % w.name)
                w.join()

    # -- the helper threads

    @staticmethod
    def _supervise(phase, cache, taskqueue, pool, gather, refill):
        """Supervisor thread: collect outcomes and keep the pool staffed."""
        try:
            while phase.value != TERMINATE and (phase.value == RUN or cache):
                if _reap_workers(pool) and phase.value != TERMINATE:
                    refill()
                for outcome in gather():
                    if outcome is not None:
                        _deliver(cache, outcome)
                # do not keep the last task's result alive while waiting
                outcome = None
        except (OSError, EOFError):
            util.debug('supervisor lost the result queue')
        # tell the feeder that no more batches will come
        taskqueue.put(None)
        util.debug('supervisor exiting: %d jobs left, phase %s'
                   % (len(cache), phase.value))

    @staticmethod
    def _feed(phase, cache, taskqueue, pool, send):
        """Feeder thread: move tasks from the task queue to the workers."""
        for tasks, set_length in iter(taskqueue.get, None):
            sent = 0
            stopped = False
            try:
                for task in tasks:
                    if phase.value == TERMINATE:
                        stopped = True
                        break
                    try:
                        send(task)
                    except Exception as exc:
                        entry = cache.get(task[0])
                        if entry is not None:
                            entry._set(task[1], (False, exc))
                    sent += 1
            finally:
                tasks = task = None
            if stopped:
                util.debug('feeder: pool is terminating')
                break
            if set_length is not None:
                util.debug('feeder: %d tasks queued for an imap' % sent)
                set_length(sent)
            set_length = None
        if phase.value != TERMINATE:
            # one stop marker per worker
            try:
                for _ in range(len(pool)):
                    send(None)
            except OSError:
                util.debug('feeder: could not send the stop markers')
        util.debug('feeder exiting')

    @staticmethod
    def _get_tasks(func, it, size):
        return _chunks(func, it, size)


def _chunks(func, iterable, size):
    """(func, tuple_of_up_to_size_items) for successive slices of iterable."""
    it = iter(iterable)
    while True:
        chunk = tuple(itertools.islice(it, size))
        if not chunk:
            return
        yield func, chunk


def _guarded_tasks(job, calls):
    """Tasks for the (func, args) pairs of calls.  If producing them raises,
    the exception becomes one last task that re-raises it in a worker, so
    that the consumer of the results sees it in its place."""
    index = 0
    try:
        for func, args in calls:
            yield (job, index, func, args, {})
            index += 1
    except Exception as exc:
        yield (job, index, _helper_reraises_exception, (exc,), {})


#
# Result objects
#

class ApplyResult(object):
    """Pending outcome of apply_async()."""

    def __init__(self, pool, callback, error_callback):
        self._pool = pool
        self._event = threading.Event()
        self._job = next(job_counter)
        self._cache = pool._cache
        self._callback = callback
        self._error_callback = error_callback
        self._cache[self._job] = self

    def ready(self):
        return self._event.is_set()

    def successful(self):
        if not self.ready():
            raise ValueError("{0!r} not ready".format(self))
        return self._success

    def wait(self, timeout=None):
        self._event.wait(timeout)

    def get(self, timeout=None):
        self.wait(timeout)
        if not self.ready():
            raise TimeoutError
        if self._success:
            return self._value
        raise self._value

    def _finish(self, success, value):
        self._success, self._value = success, value
        hook = self._callback if success else self._error_callback
        try:
            if hook is not None:
                _run_hook(hook, value)
        finally:
            self._event.set()
            del self._cache[self._job]
            self._pool = None

    def _set(self, index, outcome):
        self._finish(*outcome)

    __class_getitem__ = classmethod(types.GenericAlias)


AsyncResult = ApplyResult


def _run_hook(hook, value):
    """Run a user callback on the supervisor thread; an exception in it is
    reported instead of stopping the pool."""
    try:
        hook(value)
    except Exception:
        traceback.print_exc()


class MapResult(ApplyResult):
    """Pending outcome of map_async(): chunk outcomes are put together in
    order; the first failing chunk decides the error."""

    def __init__(self, pool, chunksize, length, callback, error_callback):
        ApplyResult.__init__(self, pool, callback,
                             error_callback=error_callback)
        self._chunksize = chunksize
        self._success = True
        self._value = [None] * length
        if length == 0 or chunksize <= 0:
            self._number_left = 0
            self._event.set()
            del self._cache[self._job]
        else:
            self._number_left = -(-length // chunksize)

    def _set(self, index, outcome):
        ok, value = outcome
        self._number_left -= 1
        if ok and self._success:
            size = self._chunksize
            self._value[index * size:(index + 1) * size] = value
        elif not ok and self._success:
            self._success = False
            self._value = value
        if self._number_left == 0:
            self._finish(self._success, self._value)


class IMapIterator(object):
    """Iterator over the results of imap(), in order."""

    def __init__(self, pool):
        self._pool = pool
        self._cond = threading.Condition(threading.Lock())
        self._job = next(job_counter)
        self._cache = pool._cache
        self._items = collections.deque()    # outcomes ready to be returned
        self._index = 0                       # outcomes received so far
        self._length = None                   # number of tasks, once known
        self._unsorted = {}                   # early outcomes, by index
        self._cache[self._job] = self

    def __iter__(self):
        return self

    def next(self, timeout=None):
        with self._cond:
            if not self._items:
                if self._index == self._length:
                    self._pool = None
                    raise StopIteration
                self._cond.wait(timeout)
                if not self._items:
                    if self._index == self._length:
                        self._pool = None
                        raise StopIteration
                    raise TimeoutError
            ok, value = self._items.popleft()
        if ok:
            return value
        raise value

    __next__ = next

    def _complete_if_done(self):
        # with self._cond held
        if self._index == self._length:
            del self._cache[self._job]
            self._pool = None

    def _set(self, index, outcome):
        with self._cond:
            if index != self._index:
                self._unsorted[index] = outcome
                return
            self._items.append(outcome)
            self._index += 1
            while self._index in self._unsorted:
                self._items.append(self._unsorted.pop(self._index))
                self._index += 1
            self._cond.notify()
            self._complete_if_done()

    def _set_length(self, length):
        with self._cond:
            self._length = length
            if self._index == length:
                self._cond.notify()
                self._complete_if_done()


class IMapUnorderedIterator(IMapIterator):
    """Iterator over the results of imap_unordered(), as they come."""

    def _set(self, index, outcome):
        with self._cond:
            self._items.append(outcome)
            self._index += 1
            self._cond.notify()
            self._complete_if_done()


#
# ThreadPool
#

_WAKE = object()     # marker put on a thread pool's out-queue to wake it


class ThreadPool(Pool):
    """The Pool interface on worker threads."""

    _wrap_exception = False

    @staticmethod
    def Process(ctx, *args, **kwds):
        from .dummy import Process
        return Process(*args, **kwds)

    def __init__(self, processes=None, initializer=None, initargs=()):
        Pool.__init__(self, processes, initializer, initargs)

    def _setup_queues(self):
        self._inqueue = queue.SimpleQueue()
        self._outqueue = queue.SimpleQueue()
        self._send_task = self._inqueue.put

    def _make_wake(self):
        return functools.partial(self._outqueue.put, _WAKE)

    def _make_gather(self):
        outqueue = self._outqueue

        def gather():
            got = [outqueue.get()]
            try:
                while len(got) < _RESULT_BATCH:
                    got.append(outqueue.get_nowait())
            except queue.Empty:
                pass
            return [item for item in got if item is not _WAKE]
        return gather

    @staticmethod
    def _stop_workers(pool, inqueue, feeder):
        # threads cannot be killed: wait for the feeder (it never blocks on
        # the unbounded queue), discard the tasks no worker took, and ask
        # each worker to exit after its current task
        feeder.join()
        try:
            while True:
                inqueue.get_nowait()
        except queue.Empty:
            pass
        for _ in range(len(pool)):
            inqueue.put(None)
