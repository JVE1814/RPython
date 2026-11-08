"""Thread objects and synchronisation primitives on top of the _thread module.

Locks come straight from _thread; Condition, Semaphore, Event, Barrier and
Timer are built here from those locks. Every running Thread object is
registered in `_active` under its thread identifier; a thread that was not
started through this module (a "foreign" thread) gets a _DummyThread entry the
first time it asks for current_thread().
"""

import os as _os
import sys as _sys
import _thread
import _contextvars
lazy import functools
lazy import warnings

from time import monotonic as _time
from _weakrefset import WeakSet
from itertools import count as _make_counter
from _collections import deque as _deque

__all__ = ['get_ident', 'active_count', 'Condition', 'current_thread',
           'enumerate', 'main_thread', 'TIMEOUT_MAX',
           'Event', 'Lock', 'RLock', 'Semaphore', 'BoundedSemaphore', 'Thread',
           'Barrier', 'BrokenBarrierError', 'Timer', 'ThreadError',
           'ExceptHookArgs', 'setprofile', 'settrace', 'local', 'stack_size',
           'excepthook', 'get_native_id', 'getprofile', 'gettrace',
           'setprofile_all_threads', 'settrace_all_threads',
           'serialize_iterator', 'synchronized_iterator', 'concurrent_tee']

# ---------------------------------------------------------------- _thread API

_start_joinable_thread = _thread.start_joinable_thread
_daemon_threads_allowed = _thread.daemon_threads_allowed
_allocate_lock = _thread.allocate_lock
_LockType = _thread.LockType
_thread_shutdown = _thread._shutdown
_make_thread_handle = _thread._make_thread_handle
_ThreadHandle = _thread._ThreadHandle
_get_main_thread_ident = _thread._get_main_thread_ident
_is_main_interpreter = _thread._is_main_interpreter
get_ident = _thread.get_ident
try:
    get_native_id = _thread.get_native_id
    _HAVE_THREAD_NATIVE_ID = True
except AttributeError:
    _HAVE_THREAD_NATIVE_ID = False
try:
    _set_name = _thread.set_name
except AttributeError:
    _set_name = None
ThreadError = _thread.error
try:
    _CRLock = _thread.RLock
except AttributeError:
    _CRLock = None
TIMEOUT_MAX = _thread.TIMEOUT_MAX
stack_size = _thread.stack_size
Lock = _LockType
from _thread import _local as local


def _deprecated(message):
    warnings.warn(message, DeprecationWarning, stacklevel=3)


# ---------------------------------------------------------------- tracing hooks

_profile_hook = None
_trace_hook = None


def setprofile(func):
    """Install *func* as the profile function of threads started from now on
    by this module (sys.setprofile() in each of them)."""
    global _profile_hook
    _profile_hook = func


def setprofile_all_threads(func):
    """setprofile() for future threads and sys.setprofile() for every thread
    that is running now."""
    setprofile(func)
    _sys._setprofileallthreads(func)


def getprofile():
    """The profile function installed by setprofile()."""
    return _profile_hook


def settrace(func):
    """Install *func* as the trace function of threads started from now on
    by this module (sys.settrace() in each of them)."""
    global _trace_hook
    _trace_hook = func


def settrace_all_threads(func):
    """settrace() for future threads and sys.settrace() for every thread
    that is running now."""
    settrace(func)
    _sys._settraceallthreads(func)


def gettrace():
    """The trace function installed by settrace()."""
    return _trace_hook


# ---------------------------------------------------------------- reentrant lock

def RLock(*args, **kwargs):
    """A reentrant lock: the thread that holds it may acquire it again and
    must release it as many times as it acquired it."""
    if _CRLock is None:
        return _PyRLock(*args, **kwargs)
    return _CRLock(*args, **kwargs)


class _RLock:
    """Reentrant lock written in Python (the _thread one is normally used)."""

    def __init__(self):
        self._mutex = _allocate_lock()
        self._holder = None     # identifier of the owning thread
        self._depth = 0         # how many times the owner acquired it

    def __repr__(self):
        holder = self._holder
        try:
            holder = _active[holder].name
        except KeyError:
            pass
        state = 'locked' if self._mutex.locked() else 'unlocked'
        cls = type(self)
        return (f'<{state} {cls.__module__}.{cls.__qualname__} object '
                f'owner={holder!r} count={self._depth} at {hex(id(self))}>')

    def _at_fork_reinit(self):
        self._mutex._at_fork_reinit()
        self._holder = None
        self._depth = 0

    def acquire(self, blocking=True, timeout=-1):
        """Acquire the lock (again, if this thread holds it already). With
        blocking false, or once *timeout* seconds passed, give up and return
        False."""
        me = get_ident()
        if self._holder == me:
            self._depth += 1
            return True
        got = self._mutex.acquire(blocking, timeout)
        if got:
            self._holder = me
            self._depth = 1
        return got

    __enter__ = acquire

    def release(self):
        """Release one level of ownership; the lock is free once every
        acquire() of the owner is matched."""
        if self._holder != get_ident():
            raise RuntimeError('cannot release un-acquired lock')
        self._depth -= 1
        if self._depth == 0:
            self._holder = None
            self._mutex.release()

    def __exit__(self, *exc_info):
        self.release()

    def locked(self):
        """Whether some thread holds the lock."""
        return self._mutex.locked()

    # Condition support: give the lock up entirely and take it back later
    def _release_save(self):
        if self._depth == 0:
            raise RuntimeError('cannot release un-acquired lock')
        saved = (self._depth, self._holder)
        self._depth = 0
        self._holder = None
        self._mutex.release()
        return saved

    def _acquire_restore(self, saved):
        self._mutex.acquire()
        self._depth, self._holder = saved

    def _is_owned(self):
        return self._holder == get_ident()

    def _recursion_count(self):
        return self._depth if self._holder == get_ident() else 0


_PyRLock = _RLock


# ---------------------------------------------------------------- Condition

class Condition:
    """A condition variable: threads wait() until another thread that holds
    the same lock calls notify() or notify_all()."""

    def __init__(self, lock=None):
        if lock is None:
            lock = RLock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release
        # a lock may know how to give itself up completely (RLock does)
        for name in ('_release_save', '_acquire_restore', '_is_owned'):
            method = getattr(lock, name, None)
            if method is not None:
                setattr(self, name, method)
        self._waiters = _deque()

    def _at_fork_reinit(self):
        self._lock._at_fork_reinit()
        self._waiters.clear()

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *exc_info):
        return self._lock.__exit__(*exc_info)

    def __repr__(self):
        return f'<Condition({self._lock}, {len(self._waiters)})>'

    def locked(self):
        """Whether the underlying lock is held."""
        return self._lock.locked()

    # fallbacks for plain (non-reentrant) locks
    def _release_save(self):
        self._lock.release()

    def _acquire_restore(self, saved):
        self._lock.acquire()

    def _is_owned(self):
        if self._lock.acquire(False):
            self._lock.release()
            return False
        return True

    def wait(self, timeout=None):
        """Release the lock, block until notified or until *timeout* seconds
        passed, then re-acquire the lock. Returns False on timeout."""
        if not self._is_owned():
            raise RuntimeError('cannot wait on un-acquired lock')
        gate = _allocate_lock()
        gate.acquire()
        self._waiters.append(gate)
        saved = self._release_save()
        woken = False
        try:
            if timeout is None:
                gate.acquire()
                woken = True
            else:
                # a negative timeout polls; NaN is rejected by the lock
                woken = gate.acquire(True, max(timeout, 0))
            return woken
        finally:
            self._acquire_restore(saved)
            if not woken:
                try:
                    self._waiters.remove(gate)
                except ValueError:
                    pass

    def wait_for(self, predicate, timeout=None):
        """wait() until predicate() is true or *timeout* seconds passed;
        return the last value of predicate()."""
        deadline = None
        remaining = timeout
        result = predicate()
        while not result:
            if remaining is not None:
                if deadline is None:
                    deadline = _time() + remaining
                else:
                    remaining = deadline - _time()
                    if remaining <= 0:
                        break
            self.wait(remaining)
            result = predicate()
        return result

    def notify(self, n=1):
        """Wake up at most *n* of the threads waiting on this condition."""
        if not self._is_owned():
            raise RuntimeError('cannot notify on un-acquired lock')
        waiters = self._waiters
        while waiters and n > 0:
            gate = waiters[0]
            try:
                gate.release()
            except RuntimeError:
                # its wait() timed out and is about to remove it
                pass
            else:
                n -= 1
            try:
                waiters.remove(gate)
            except ValueError:
                pass

    def notify_all(self):
        """Wake up every thread waiting on this condition."""
        self.notify(len(self._waiters))

    def notifyAll(self):
        """Deprecated alias of notify_all()."""
        _deprecated('Condition.notifyAll() is deprecated; use notify_all() instead')
        self.notify_all()


# ---------------------------------------------------------------- semaphores

class Semaphore:
    """A counter of available slots: acquire() takes one (waiting while there
    is none), release() returns slots."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError('semaphore initial value must be >= 0')
        self._cond = Condition(Lock())
        self._value = value

    def __repr__(self):
        cls = type(self)
        return (f'<{cls.__module__}.{cls.__qualname__} at {hex(id(self))}: '
                f'value={self._value}>')

    def acquire(self, blocking=True, timeout=None):
        """Take a slot. Without blocking, or when *timeout* seconds pass
        first, return False instead of waiting."""
        if not blocking and timeout is not None:
            raise ValueError("can't specify timeout for non-blocking acquire")
        deadline = None
        with self._cond:
            while self._value == 0:
                if not blocking:
                    return False
                if timeout is not None:
                    if deadline is None:
                        deadline = _time() + timeout
                    else:
                        timeout = deadline - _time()
                        if timeout <= 0:
                            return False
                self._cond.wait(timeout)
            self._value -= 1
            return True

    __enter__ = acquire

    def release(self, n=1):
        """Return *n* slots, waking up as many waiting threads."""
        if n < 1:
            raise ValueError('n must be one or more')
        with self._cond:
            self._value += n
            self._cond.notify(n)

    def __exit__(self, *exc_info):
        self.release()


class BoundedSemaphore(Semaphore):
    """A Semaphore that refuses to hold more slots than it started with."""

    def __init__(self, value=1):
        super().__init__(value)
        self._initial_value = value

    def __repr__(self):
        cls = type(self)
        return (f'<{cls.__module__}.{cls.__qualname__} at {hex(id(self))}: '
                f'value={self._value}/{self._initial_value}>')

    def release(self, n=1):
        if n < 1:
            raise ValueError('n must be one or more')
        with self._cond:
            if self._value + n > self._initial_value:
                raise ValueError('Semaphore released too many times')
            self._value += n
            self._cond.notify(n)


# ---------------------------------------------------------------- Event

class Event:
    """A flag that threads can wait for until some thread sets it."""

    def __init__(self):
        self._cond = Condition(Lock())
        self._flag = False

    def __repr__(self):
        cls = type(self)
        state = 'set' if self._flag else 'unset'
        return f'<{cls.__module__}.{cls.__qualname__} at {hex(id(self))}: {state}>'

    def _at_fork_reinit(self):
        self._cond._at_fork_reinit()

    def is_set(self):
        """Whether the flag is set."""
        return self._flag

    def isSet(self):
        """Deprecated alias of is_set()."""
        _deprecated('Event.isSet() is deprecated; use is_set() instead')
        return self.is_set()

    def set(self):
        """Set the flag and wake up every waiting thread."""
        with self._cond:
            self._flag = True
            self._cond.notify_all()

    def clear(self):
        """Reset the flag."""
        with self._cond:
            self._flag = False

    def wait(self, timeout=None):
        """Block until the flag is set or *timeout* seconds passed; return
        True if the flag was set (also when it was cleared again before this
        thread got to run)."""
        with self._cond:
            if self._flag:
                return True
            return self._cond.wait(timeout)


# ---------------------------------------------------------------- Barrier

class BrokenBarrierError(RuntimeError):
    pass


class Barrier:
    """A rendezvous point for a fixed number of threads: wait() returns once
    *parties* threads are waiting. *action*, if given, is run by one of them
    before they are released."""

    # phases of one round
    # (ints, kept in `_state`/`_count`: multiprocessing's Barrier keeps them in
    # shared memory by overriding those two attributes with properties)
    _FILLING, _DRAINING, _RESETTING, _BROKEN = 0, 1, -1, -2

    def __init__(self, parties, action=None, timeout=None):
        if parties < 1:
            raise ValueError('parties must be >= 1')
        self._cond = Condition(Lock())
        self._action = action
        self._timeout = timeout
        self._parties = parties
        self._state = self._FILLING
        self._count = 0       # threads currently in wait()

    def __repr__(self):
        cls = type(self)
        head = f'<{cls.__module__}.{cls.__qualname__} at {hex(id(self))}'
        if self.broken:
            return f'{head}: broken>'
        return f'{head}: waiters={self.n_waiting}/{self.parties}>'

    def wait(self, timeout=None):
        """Wait until all parties arrived; return this thread's arrival index
        (0 .. parties-1). Raises BrokenBarrierError if the barrier is or
        becomes broken (abort(), reset(), a timeout or a failing action)."""
        if timeout is None:
            timeout = self._timeout
        with self._cond:
            # threads of the previous round have to leave first
            while self._state in (self._DRAINING, self._RESETTING):
                self._cond.wait()
            if self._state == self._BROKEN:
                raise BrokenBarrierError
            index = self._count
            self._count += 1
            try:
                if index + 1 == self._parties:
                    self._release_all()
                else:
                    self._await_release(timeout)
                return index
            finally:
                self._count -= 1
                if self._count == 0 and self._state in (self._DRAINING, self._RESETTING):
                    self._state = self._FILLING
                    self._cond.notify_all()

    def _release_all(self):
        try:
            if self._action is not None:
                self._action()
        except BaseException:
            self._break()
            raise
        self._state = self._DRAINING
        self._cond.notify_all()

    def _await_release(self, timeout):
        if not self._cond.wait_for(lambda: self._state != self._FILLING, timeout):
            self._break()
            raise BrokenBarrierError
        if self._state in (self._BROKEN, self._RESETTING):
            raise BrokenBarrierError

    def _break(self):
        self._state = self._BROKEN
        self._cond.notify_all()

    def reset(self):
        """Return the barrier to its initial state; threads waiting in it get
        BrokenBarrierError."""
        with self._cond:
            if self._count > 0:
                if self._state in (self._FILLING, self._BROKEN):
                    self._state = self._RESETTING
            else:
                self._state = self._FILLING
            self._cond.notify_all()

    def abort(self):
        """Break the barrier: current and future wait() calls raise
        BrokenBarrierError until reset()."""
        with self._cond:
            self._break()

    @property
    def parties(self):
        """The number of threads needed to pass the barrier."""
        return self._parties

    @property
    def n_waiting(self):
        """The number of threads waiting in the current round."""
        return self._count if self._state == self._FILLING else 0

    @property
    def broken(self):
        """Whether the barrier is broken."""
        return self._state == self._BROKEN


# ---------------------------------------------------------------- registry

_counter = _make_counter(1).__next__
_active_limbo_lock = RLock()
_active = {}        # identifier -> Thread, for running threads
_limbo = {}         # Thread -> Thread, started but not yet running
_dangling = WeakSet()

_threading_atexits = []
_SHUTTING_DOWN = False

excepthook = _thread._excepthook
__excepthook__ = excepthook
ExceptHookArgs = _thread._ExceptHookArgs


def _make_invoke_excepthook():
    # The hooks are looked up when a thread fails, but late in interpreter
    # shutdown the module globals may already be gone: keep fallbacks.
    fallback_hook = excepthook
    fallback_sys_hook = _sys.excepthook
    if fallback_hook is None:
        raise RuntimeError('threading.excepthook is None')
    if fallback_sys_hook is None:
        raise RuntimeError('sys.excepthook is None')
    exc_info = _sys.exc_info
    sys_module = _sys
    write = print

    def invoke_excepthook(thread):
        args = None
        try:
            hook = excepthook
            if hook is None:
                hook = fallback_hook
            args = ExceptHookArgs([*exc_info(), thread])
            hook(args)
        except Exception as exc:
            exc.__suppress_context__ = True
            del exc
            stderr = None
            if sys_module is not None:
                stderr = sys_module.stderr
            if stderr is None:
                stderr = thread._stderr
            write('Exception in threading.excepthook:', file=stderr, flush=True)
            sys_hook = None
            if sys_module is not None:
                sys_hook = sys_module.excepthook
            if sys_hook is None:
                sys_hook = fallback_sys_hook
            sys_hook(*exc_info())
        finally:
            args = None

    return invoke_excepthook


# ---------------------------------------------------------------- Thread

class Thread:
    """A thread of control: pass *target* (called with *args* and *kwargs*)
    or override run(), then start() it."""

    _initialized = False

    def __init__(self, group=None, target=None, name=None,
                 args=(), kwargs=None, *, daemon=None, context=None):
        assert group is None, 'group argument must be None for now'
        if kwargs is None:
            kwargs = {}
        if name:
            name = str(name)
        else:
            name = f'Thread-{_counter()}'
            if target is not None:
                try:
                    name = f'{name} ({target.__name__})'
                except AttributeError:
                    pass
        self._target = target
        self._name = name
        self._args = args
        self._kwargs = kwargs
        if daemon is not None:
            if daemon and not _daemon_threads_allowed():
                raise RuntimeError('daemon threads are disabled in this interpreter')
            self._daemonic = daemon
        else:
            self._daemonic = current_thread().daemon
        self._context = context
        self._ident = None
        if _HAVE_THREAD_NATIVE_ID:
            self._native_id = None
        self._handle = _ThreadHandle()
        self._started = Event()
        self._initialized = True
        # kept for reporting errors when sys.stderr is gone
        self._stderr = _sys.stderr
        self._invoke_excepthook = _make_invoke_excepthook()
        _dangling.add(self)

    def _after_fork(self, new_ident=None):
        # in a forked child: only the forking thread exists any more
        self._started._at_fork_reinit()
        if new_ident is not None:
            self._ident = new_ident
            if _HAVE_THREAD_NATIVE_ID:
                self._set_native_id()
            self._handle = _make_thread_handle(new_ident)
        else:
            try:
                self._handle._set_done()
            except RuntimeError:
                # never started (created, or being started by another thread at the
                # fork): nothing ran, the object stays as it was
                pass

    def __repr__(self):
        assert self._initialized, 'Thread.__init__() was not called'
        if self._handle.is_done():
            status = 'stopped'
        elif self._started.is_set():
            status = 'started'
        else:
            status = 'initial'
        if self._daemonic:
            status += ' daemon'
        if self._ident is not None:
            status += f' {self._ident}'
        return f'<{type(self).__name__}({self._name}, {status})>'

    def start(self):
        """Start running the thread's run() method in a new thread (at most
        once per Thread object)."""
        if not self._initialized:
            raise RuntimeError('thread.__init__() not called')
        if self._started.is_set():
            raise RuntimeError('threads can only be started once')
        # the context run() executes in: given, a copy of the caller's, or empty
        if self._context is not None:
            self._run_context = self._context
        elif _sys.flags.thread_inherit_context:
            self._run_context = _contextvars.copy_context()
        else:
            self._run_context = _contextvars.Context()
        with _active_limbo_lock:
            _limbo[self] = self
        try:
            _start_joinable_thread(self._bootstrap, handle=self._handle,
                                   daemon=self.daemon)
        except Exception:
            with _active_limbo_lock:
                _limbo.pop(self, None)
            raise
        self._started.wait()

    def run(self):
        """The thread's activity: calls the target passed to the
        constructor. Subclasses may override it."""
        try:
            if self._target is not None:
                self._target(*self._args, **self._kwargs)
        finally:
            # the thread may outlive these; do not keep them alive with it
            del self._target, self._args, self._kwargs

    def _bootstrap(self):
        try:
            self._bootstrap_inner()
        except:
            # daemon threads still running during shutdown may find the
            # interpreter half torn down; there is nothing to report to
            if self._daemonic and _sys is None:
                return
            raise

    def _set_ident(self):
        self._ident = get_ident()

    if _HAVE_THREAD_NATIVE_ID:
        def _set_native_id(self):
            self._native_id = get_native_id()

    def _set_os_name(self):
        if _set_name is None or not self._name:
            return
        try:
            _set_name(self._name)
        except OSError:
            pass

    def _bootstrap_inner(self):
        try:
            self._set_ident()
            if _HAVE_THREAD_NATIVE_ID:
                self._set_native_id()
            self._set_os_name()
            with _active_limbo_lock:
                _active[self._ident] = self
                _limbo.pop(self, None)
            self._started.set()
            if _trace_hook:
                _sys.settrace(_trace_hook)
            if _profile_hook:
                _sys.setprofile(_profile_hook)
            context = self._run_context
            self._run_context = None
            try:
                context.run(self.run)
            except:
                self._invoke_excepthook(self)
        finally:
            self._delete()

    def _delete(self):
        with _active_limbo_lock:
            _active.pop(get_ident(), None)

    def join(self, timeout=None):
        """Wait until the thread ends, or at most *timeout* seconds; use
        is_alive() to tell which happened."""
        if not self._initialized:
            raise RuntimeError('Thread.__init__() not called')
        if not self._started.is_set():
            raise RuntimeError('cannot join thread before it is started')
        if self is current_thread():
            raise RuntimeError('cannot join current thread')
        if timeout is not None:
            timeout = max(timeout, 0)
        self._handle.join(timeout)

    @property
    def name(self):
        """The thread's name (for identification only)."""
        assert self._initialized, 'Thread.__init__() not called'
        return self._name

    @name.setter
    def name(self, name):
        assert self._initialized, 'Thread.__init__() not called'
        self._name = str(name)
        if get_ident() == self._ident:
            self._set_os_name()

    @property
    def ident(self):
        """The thread's identifier (see get_ident()), or None before it
        started. Identifiers of finished threads may be reused."""
        assert self._initialized, 'Thread.__init__() not called'
        return self._ident

    if _HAVE_THREAD_NATIVE_ID:
        @property
        def native_id(self):
            """The operating system's identifier of the thread, or None
            before it started."""
            assert self._initialized, 'Thread.__init__() not called'
            return self._native_id

    def is_alive(self):
        """Whether the thread is running (started and not finished)."""
        assert self._initialized, 'Thread.__init__() not called'
        if not self._started.is_set():
            return False
        return not self._handle.is_done()

    @property
    def daemon(self):
        """Whether this is a daemon thread: the program exits without waiting
        for daemon threads. Inherited from the creating thread by default."""
        assert self._initialized, 'Thread.__init__() not called'
        return self._daemonic

    @daemon.setter
    def daemon(self, daemonic):
        if not self._initialized:
            raise RuntimeError('Thread.__init__() not called')
        if daemonic and not _daemon_threads_allowed():
            raise RuntimeError('daemon threads are disabled in this interpreter')
        if self._started.is_set():
            raise RuntimeError('cannot set daemon status of active thread')
        self._daemonic = daemonic

    def isDaemon(self):
        """Deprecated: get the daemon attribute."""
        _deprecated('isDaemon() is deprecated, get the daemon attribute instead')
        return self.daemon

    def setDaemon(self, daemonic):
        """Deprecated: set the daemon attribute."""
        _deprecated('setDaemon() is deprecated, set the daemon attribute instead')
        self.daemon = daemonic

    def getName(self):
        """Deprecated: get the name attribute."""
        _deprecated('getName() is deprecated, get the name attribute instead')
        return self.name

    def setName(self, name):
        """Deprecated: set the name attribute."""
        _deprecated('setName() is deprecated, set the name attribute instead')
        self.name = name



class Timer(Thread):
    """A thread that calls function(*args, **kwargs) after *interval*
    seconds unless cancel() was called first."""

    def __init__(self, interval, function, args=None, kwargs=None):
        Thread.__init__(self)
        self.interval = interval
        self.function = function
        self.args = args if args is not None else []
        self.kwargs = kwargs if kwargs is not None else {}
        self.finished = Event()

    def cancel(self):
        """Stop the timer if it has not fired yet."""
        self.finished.set()

    def run(self):
        self.finished.wait(self.interval)
        if not self.finished.is_set():
            self.function(*self.args, **self.kwargs)
        self.finished.set()


class _MainThread(Thread):
    """The thread the interpreter started in."""

    def __init__(self):
        Thread.__init__(self, name='MainThread', daemon=False)
        self._started.set()
        self._ident = _get_main_thread_ident()
        self._handle = _make_thread_handle(self._ident)
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        with _active_limbo_lock:
            _active[self._ident] = self


class _DummyThreadRemover:
    """Kept in thread-local storage of a foreign thread: when that thread ends
    its storage is released and the _DummyThread is unregistered."""

    def __init__(self, thread):
        self._thread = thread
        self._ident = thread._ident

    def __del__(self):
        thread = self._thread
        try:
            with _active_limbo_lock:
                if _active.get(self._ident) is thread:
                    del _active[self._ident]
            thread._handle._set_done()
        except Exception:
            # late in shutdown the module may be torn down already
            pass


_dummy_storage = local()


class _DummyThread(Thread):
    """Stands for a thread that was not started through this module. It is
    always considered alive and daemonic, and cannot be joined."""

    def __init__(self):
        Thread.__init__(self, name=f'Dummy-{_counter()}',
                        daemon=_daemon_threads_allowed())
        self._started.set()
        self._set_ident()
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        self._handle = _make_thread_handle(self._ident)
        with _active_limbo_lock:
            _active[self._ident] = self
        _dummy_storage.remover = _DummyThreadRemover(self)

    def is_alive(self):
        if self._started.is_set() and not self._handle.is_done():
            return True
        raise RuntimeError('thread is not alive')

    def join(self, timeout=None):
        raise RuntimeError('cannot join a dummy thread')

    def _after_fork(self, new_ident=None):
        if new_ident is not None:
            self.__class__ = _MainThread
            self._name = 'MainThread'
            self._daemonic = False
        Thread._after_fork(self, new_ident=new_ident)


# ---------------------------------------------------------------- module functions

def current_thread():
    """The Thread object of the calling thread (a _DummyThread for threads
    not started through this module)."""
    try:
        return _active[get_ident()]
    except KeyError:
        return _DummyThread()


def currentThread():
    """Deprecated alias of current_thread()."""
    _deprecated('currentThread() is deprecated, use current_thread() instead')
    return current_thread()


def active_count():
    """The number of Thread objects that are alive."""
    with _active_limbo_lock:
        return len(_active) + len(_limbo)


def activeCount():
    """Deprecated alias of active_count()."""
    _deprecated('activeCount() is deprecated, use active_count() instead')
    return active_count()


def _enumerate():
    return list(_active.values()) + list(_limbo.values())


def enumerate():
    """The Thread objects that are alive (including daemonic, dummy and the
    main thread; not those that are not started or finished)."""
    with _active_limbo_lock:
        return list(_active.values()) + list(_limbo.values())


def main_thread():
    """The main Thread object (the thread the interpreter started in)."""
    return _main_thread


def _register_atexit(func, *args, **kwargs):
    """Call func(*args, **kwargs) at interpreter shutdown, before the
    non-daemon threads are joined (unlike the atexit module, whose functions
    run afterwards). Functions run in reverse order of registration."""
    if _SHUTTING_DOWN:
        raise RuntimeError("can't register atexit after shutdown")
    _threading_atexits.append((func, args, kwargs))


def _shutdown():
    """Called by the interpreter at exit: run the _register_atexit()
    functions, then wait for every non-daemon thread."""
    global _SHUTTING_DOWN
    if _main_thread._handle.is_done() and _is_main_interpreter():
        # shut down already
        return
    _SHUTTING_DOWN = True
    while _threading_atexits:
        func, args, kwargs = _threading_atexits.pop()
        func(*args, **kwargs)
    if _main_thread.ident == get_ident():
        _main_thread._handle._set_done()
    _thread_shutdown()


def _after_fork():
    """In the child after os.fork(): the forking thread is the only one left
    and becomes the main thread."""
    global _active_limbo_lock, _main_thread
    _active_limbo_lock = RLock()
    me = get_ident()
    current = _active.get(me)
    if current is None:
        # a foreign thread without a _DummyThread: a new main thread object
        current = _MainThread()
        if current._ident != me:
            current._after_fork(new_ident=me)
    else:
        current._after_fork(new_ident=me)
    _main_thread = current
    with _active_limbo_lock:
        for thread in set(_enumerate()) | set(_dangling):
            if thread is not current:
                thread._after_fork()
        _limbo.clear()
        _active.clear()
        _active[me] = current
    _dangling.clear()
    _dangling.add(current)


if hasattr(_os, 'register_at_fork'):
    _os.register_at_fork(after_in_child=_after_fork)


_main_thread = _MainThread()


# ---------------------------------------------------------------- iterator tools

class serialize_iterator:
    """Iterator that lets one thread at a time advance the wrapped iterator
    (also through its send(), throw() and close(), when it has them)."""

    def __init__(self, iterable):
        self._iterator = iter(iterable)
        self._lock = Lock()

    def __iter__(self):
        return self

    def __next__(self):
        with self._lock:
            return next(self._iterator)

    def send(self, value):
        with self._lock:
            return self._iterator.send(value)

    def throw(self, *args):
        with self._lock:
            return self._iterator.throw(*args)

    def close(self):
        with self._lock:
            return self._iterator.close()


def synchronized_iterator(func):
    """Decorator for generator functions: each generator it creates is
    wrapped in a serialize_iterator."""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        return serialize_iterator(func(*args, **kwargs))
    return wrapper


class _TeeCell:
    # one produced value; `following` is filled in when the next one is
    __slots__ = ('value', 'following')

    def __init__(self, value=None):
        self.value = value
        self.following = None


class _concurrent_tee_iterator:
    """One of the iterators made by concurrent_tee()."""

    def __init__(self, source, lock, position):
        self._source = source
        self.lock = lock
        self._position = position

    def __iter__(self):
        return self

    def __next__(self):
        with self.lock:
            cell = self._position.following
            if cell is None:
                cell = _TeeCell(next(self._source))
                self._position.following = cell
            self._position = cell
            return cell.value


def concurrent_tee(iterable, n=2):
    """Return *n* independent iterators over *iterable* that may be used from
    different threads; they share one lock (their `lock` attribute)."""
    if n < 0:
        raise ValueError('n must be >= 0')
    source = iter(iterable)
    lock = Lock()
    start = _TeeCell()
    return tuple(_concurrent_tee_iterator(source, lock, start) for _ in range(n))
