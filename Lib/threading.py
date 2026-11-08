"""Higher-level threading interface built on the low-level _thread module.

Lock and RLock come from _thread (with a Python RLock as a fallback).
Condition, Semaphore, BoundedSemaphore, Event, Barrier, Thread and Timer are
defined here.  Every thread this module knows about is recorded in `_active`
(keyed by thread identifier) once it runs; threads started some other way get
a _DummyThread record the first time they call current_thread().
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

# ------------------------------------------------------------ from _thread

_start_joinable_thread = _thread.start_joinable_thread
_daemon_threads_allowed = _thread.daemon_threads_allowed
_allocate_lock = _thread.allocate_lock
_ThreadHandle = _thread._ThreadHandle
_make_thread_handle = _thread._make_thread_handle
_get_main_thread_ident = _thread._get_main_thread_ident
_is_main_interpreter = _thread._is_main_interpreter
_join_remaining_threads = _thread._shutdown

get_ident = _thread.get_ident
ThreadError = _thread.error
TIMEOUT_MAX = _thread.TIMEOUT_MAX
stack_size = _thread.stack_size
Lock = _thread.LockType
from _thread import _local as local

_HAVE_THREAD_NATIVE_ID = hasattr(_thread, 'get_native_id')
if _HAVE_THREAD_NATIVE_ID:
    get_native_id = _thread.get_native_id
_set_os_thread_name = getattr(_thread, 'set_name', None)
_CRLock = getattr(_thread, 'RLock', None)


def _deprecated(message):
    # stacklevel 3: the caller of the deprecated function or method
    warnings.warn(message, DeprecationWarning, stacklevel=3)


# ------------------------------------------------------------ trace hooks

# Installed with sys.settrace()/sys.setprofile() in every thread this module
# starts from now on.
_profile_hook = None
_trace_hook = None


def setprofile(func):
    """Have every thread started from now on run sys.setprofile(func)."""
    global _profile_hook
    _profile_hook = func


def setprofile_all_threads(func):
    """Like setprofile(), and also set func on all threads running now."""
    setprofile(func)
    _sys._setprofileallthreads(func)


def getprofile():
    """The function last passed to setprofile(), or None."""
    return _profile_hook


def settrace(func):
    """Have every thread started from now on run sys.settrace(func)."""
    global _trace_hook
    _trace_hook = func


def settrace_all_threads(func):
    """Like settrace(), and also set func on all threads running now."""
    settrace(func)
    _sys._settraceallthreads(func)


def gettrace():
    """The function last passed to settrace(), or None."""
    return _trace_hook


# ------------------------------------------------------------ RLock

def RLock(*args, **kwargs):
    """Return a new reentrant lock.

    The thread that holds it can acquire it again; it is free again once
    each acquire() has been matched by a release() from that thread.
    """
    if _CRLock is None:
        return _PyRLock(*args, **kwargs)
    return _CRLock(*args, **kwargs)


class _RLock:
    """Pure Python reentrant lock (used when _thread has none)."""

    def __init__(self):
        self._block = _allocate_lock()
        self._owner = None      # identifier of the holding thread
        self._count = 0         # how deep the holder is

    def __repr__(self):
        owner = self._owner
        thread = _active.get(owner)
        if thread is not None:
            owner = thread.name
        cls = type(self)
        return "<%s %s.%s object owner=%r count=%d at %s>" % (
            "locked" if self._block.locked() else "unlocked",
            cls.__module__, cls.__qualname__, owner, self._count, hex(id(self)))

    def _at_fork_reinit(self):
        self._block._at_fork_reinit()
        self._owner = None
        self._count = 0

    def acquire(self, blocking=True, timeout=-1):
        """Acquire the lock, or go one level deeper if already holding it.

        Returns False if blocking is false or the timeout passes while
        another thread holds it.
        """
        me = get_ident()
        if self._owner == me:
            self._count += 1
            return True
        if not self._block.acquire(blocking, timeout):
            return False
        self._owner, self._count = me, 1
        return True

    __enter__ = acquire

    def release(self):
        """Undo one acquire() of the holding thread."""
        if self._owner != get_ident():
            raise RuntimeError("cannot release un-acquired lock")
        self._count -= 1
        if not self._count:
            self._owner = None
            self._block.release()

    def __exit__(self, *exc_info):
        self.release()

    def locked(self):
        """Whether any thread holds the lock."""
        return self._block.locked()

    # The protocol Condition uses to give up every level at once while
    # waiting, and to restore them afterwards.

    def _release_save(self):
        if not self._count:
            raise RuntimeError("cannot release un-acquired lock")
        state = (self._count, self._owner)
        self._owner, self._count = None, 0
        self._block.release()
        return state

    def _acquire_restore(self, state):
        self._block.acquire()
        self._count, self._owner = state

    def _is_owned(self):
        return self._owner == get_ident()

    def _recursion_count(self):
        return self._count if self._owner == get_ident() else 0


_PyRLock = _RLock


# ------------------------------------------------------------ Condition

class Condition:
    """Condition variable around a lock (by default a new RLock).

    The Condition also works as that lock: acquire(), release() and the
    with statement go straight to it.
    """

    def __init__(self, lock=None):
        if lock is None:
            lock = RLock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release
        # Locks that can save and restore their whole state (RLock) are used
        # through that; for others the fallbacks below apply.
        for name in ('_release_save', '_acquire_restore', '_is_owned'):
            if hasattr(lock, name):
                setattr(self, name, getattr(lock, name))
        # One locked "gate" per waiting thread, oldest first; notify()
        # opens gates from the front.
        self._waiters = _deque()

    def _at_fork_reinit(self):
        self._lock._at_fork_reinit()
        self._waiters.clear()

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *exc_info):
        return self._lock.__exit__(*exc_info)

    def __repr__(self):
        return "<Condition(%s, %d)>" % (self._lock, len(self._waiters))

    def locked(self):
        """Whether the underlying lock is held."""
        return self._lock.locked()

    def _release_save(self):
        self._lock.release()

    def _acquire_restore(self, state):
        self._lock.acquire()

    def _is_owned(self):
        # A plain lock cannot say who holds it: assume it is us if it is
        # held at all.
        if self._lock.acquire(False):
            self._lock.release()
            return False
        return True

    def wait(self, timeout=None):
        """Release the lock and sleep until notified or until timeout
        seconds pass, then take the lock back.

        Returns True if this thread was notified, False on timeout.
        """
        if not self._is_owned():
            raise RuntimeError("cannot wait on un-acquired lock")
        gate = _allocate_lock()
        gate.acquire()
        self._waiters.append(gate)
        state = self._release_save()
        try:
            if timeout is None:
                gate.acquire()
            else:
                if timeout < 0:
                    timeout = 0         # a past deadline: just check
                gate.acquire(True, timeout)     # NaN raises ValueError
        finally:
            self._acquire_restore(state)
            # Still queued means nobody picked this waiter.  With the lock
            # held again, notify() cannot run concurrently with this check.
            notified = True
            try:
                self._waiters.remove(gate)
                notified = False
            except ValueError:
                pass
        return notified

    def wait_for(self, predicate, timeout=None):
        """wait() until predicate() is true; give up when timeout seconds
        have passed.  Returns the last result of predicate()."""
        outcome = predicate()
        if outcome:
            return outcome
        deadline = None if timeout is None else _time() + timeout
        left = timeout
        while True:
            self.wait(left)
            outcome = predicate()
            if outcome:
                return outcome
            if deadline is not None:
                left = deadline - _time()
                if left <= 0:
                    return outcome

    def notify(self, n=1):
        """Wake up to n of the threads waiting on this condition."""
        if not self._is_owned():
            raise RuntimeError("cannot notify on un-acquired lock")
        waiters = self._waiters
        while n > 0 and waiters:
            waiters.popleft().release()
            n -= 1

    def notify_all(self):
        """Wake up every thread waiting on this condition."""
        if not self._is_owned():
            raise RuntimeError("cannot notify on un-acquired lock")
        waiters = self._waiters
        while waiters:
            waiters.popleft().release()

    def notifyAll(self):
        """Deprecated spelling of notify_all()."""
        _deprecated('Condition.notifyAll() is deprecated, use notify_all() instead')
        self.notify_all()


# ------------------------------------------------------------ semaphores

class Semaphore:
    """A counter of free slots.

    acquire() takes a slot, sleeping while there is none; release() gives
    slots back.  A slot released while threads sleep goes straight to the
    one that has waited longest.
    """

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("semaphore initial value must be >= 0")
        self._mutex = _allocate_lock()
        self._value = value
        self._sleepers = _deque()   # gates of blocked acquire() calls

    def __repr__(self):
        cls = type(self)
        return "<%s.%s at %s: value=%s>" % (
            cls.__module__, cls.__qualname__, hex(id(self)), self._value)

    def acquire(self, blocking=True, timeout=None):
        """Take a slot.

        Without blocking, return False at once if none is free; with a
        timeout, return False if none became free in time.  True otherwise.
        """
        if not blocking and timeout is not None:
            raise ValueError("can't specify timeout for non-blocking acquire")
        with self._mutex:
            if self._value > 0:
                self._value -= 1
                return True
            if not blocking:
                return False
            gate = _allocate_lock()
            gate.acquire()
            self._sleepers.append(gate)
        try:
            if timeout is None:
                return gate.acquire()
            if timeout < 0:
                timeout = 0
            if gate.acquire(True, timeout):
                return True
        except BaseException:
            self._withdraw(gate)
            raise
        return self._withdraw(gate)

    def _withdraw(self, gate):
        # The wait for gate ended without it opening.  If a release() opened
        # it in the meantime the slot is ours after all; True then.
        with self._mutex:
            try:
                self._sleepers.remove(gate)
            except ValueError:
                return True
            return False

    __enter__ = acquire

    def _add_slots(self, n):
        # Called with _mutex held.
        self._value += n
        sleepers = self._sleepers
        while self._value and sleepers:
            self._value -= 1
            sleepers.popleft().release()

    def release(self, n=1):
        """Give back n slots, waking up that many sleeping threads."""
        if n < 1:
            raise ValueError("n must be one or more")
        with self._mutex:
            self._add_slots(n)

    def __exit__(self, *exc_info):
        self.release()


class BoundedSemaphore(Semaphore):
    """A Semaphore that refuses to hold more slots than it started with."""

    def __init__(self, value=1):
        Semaphore.__init__(self, value)
        self._initial_value = value

    def __repr__(self):
        cls = type(self)
        return "<%s.%s at %s: value=%s/%s>" % (
            cls.__module__, cls.__qualname__, hex(id(self)), self._value,
            self._initial_value)

    def release(self, n=1):
        """Give back n slots; ValueError if that exceeds the initial value."""
        if n < 1:
            raise ValueError("n must be one or more")
        with self._mutex:
            if self._value + n > self._initial_value:
                raise ValueError("Semaphore released too many times")
            self._add_slots(n)


# ------------------------------------------------------------ Event

class Event:
    """A flag that threads can wait on until another thread sets it."""

    def __init__(self):
        self._cond = Condition(Lock())
        self._flag = False

    def __repr__(self):
        cls = type(self)
        return "<%s.%s at %s: %s>" % (cls.__module__, cls.__qualname__,
                                      hex(id(self)),
                                      "set" if self._flag else "unset")

    def _at_fork_reinit(self):
        self._cond._at_fork_reinit()

    def is_set(self):
        """Whether the flag is set."""
        return self._flag

    def isSet(self):
        """Deprecated spelling of is_set()."""
        _deprecated('Event.isSet() is deprecated, use is_set() instead')
        return self.is_set()

    def set(self):
        """Set the flag; every thread waiting for it wakes up."""
        with self._cond:
            self._flag = True
            self._cond.notify_all()

    def clear(self):
        """Lower the flag again."""
        with self._cond:
            self._flag = False

    def wait(self, timeout=None):
        """Sleep until the flag is set or timeout seconds have passed.

        Returns True if the flag was set (even if another thread has
        lowered it again before this one ran), False on timeout.
        """
        with self._cond:
            return self._flag or self._cond.wait(timeout)


# ------------------------------------------------------------ Barrier

class BrokenBarrierError(RuntimeError):
    """A Barrier was aborted, reset or timed out while threads used it."""


# Phases of a Barrier, kept in its `_state`.  They are ints because
# multiprocessing's Barrier stores `_state` and `_count` in shared memory.
_GATHERING = 0      # threads arrive and wait
_PASSING = 1        # the round is complete; its threads are on their way out
_CANCELLED = 2      # reset() while threads waited; they leave with an error
_BROKEN = 3         # unusable until reset()


class Barrier:
    """A meeting point for a fixed number of threads.

    Each wait() blocks until `parties` threads are waiting, then all of
    them continue.  The last one to arrive first runs `action`, if given.
    Arrivals for the next round wait until the previous round has left.
    """

    def __init__(self, parties, action=None, timeout=None):
        if parties < 1:
            raise ValueError("parties must be >= 1")
        self._cond = Condition(Lock())
        self._action = action
        self._timeout = timeout
        self._parties = parties
        self._state = _GATHERING
        self._count = 0     # threads inside wait() that belong to this round

    def __repr__(self):
        cls = type(self)
        head = "<%s.%s at %s" % (cls.__module__, cls.__qualname__, hex(id(self)))
        if self.broken:
            return head + ": broken>"
        return "%s: waiters=%d/%d>" % (head, self.n_waiting, self.parties)

    def _admits(self):
        return self._state not in (_PASSING, _CANCELLED)

    def _decided(self):
        return self._state != _GATHERING

    def wait(self, timeout=None):
        """Wait for the others; returns this thread's arrival index, 0 to
        parties-1.

        Raises BrokenBarrierError if the barrier is broken, gets broken
        (abort(), a timeout, a failing action) or is reset meanwhile.
        """
        if timeout is None:
            timeout = self._timeout
        cond = self._cond
        with cond:
            cond.wait_for(self._admits)
            if self._state == _BROKEN:
                raise BrokenBarrierError
            index = self._count
            self._count = index + 1
            try:
                if index + 1 == self._parties:
                    self._complete_round()
                elif not cond.wait_for(self._decided, timeout):
                    self._break()       # timed out: nobody else gets through
                    raise BrokenBarrierError
                elif self._state != _PASSING:
                    raise BrokenBarrierError
                return index
            finally:
                self._leave()

    def _complete_round(self):
        if self._action is not None:
            try:
                self._action()
            except:
                self._break()
                raise
        self._state = _PASSING
        self._cond.notify_all()

    def _leave(self):
        self._count -= 1
        if self._count == 0 and self._state in (_PASSING, _CANCELLED):
            # The last one out lets the next round in.
            self._state = _GATHERING
            self._cond.notify_all()

    def _break(self):
        self._state = _BROKEN
        self._cond.notify_all()

    def reset(self):
        """Make the barrier usable again; threads waiting in it now get
        BrokenBarrierError."""
        with self._cond:
            if self._count == 0:
                self._state = _GATHERING
            elif self._state in (_GATHERING, _BROKEN):
                self._state = _CANCELLED
            # (while _PASSING the round ends normally and reopens by itself)
            self._cond.notify_all()

    def abort(self):
        """Break the barrier: wait() raises BrokenBarrierError, now and
        until reset()."""
        with self._cond:
            self._break()

    @property
    def parties(self):
        """How many threads it takes to pass the barrier."""
        return self._parties

    @property
    def n_waiting(self):
        """How many threads wait in the current round."""
        return self._count if self._state == _GATHERING else 0

    @property
    def broken(self):
        """Whether the barrier is broken."""
        return self._state == _BROKEN


# ------------------------------------------------------------ thread registry

_counter = _make_counter(1).__next__   # numbers default thread names
_active_limbo_lock = RLock()
_active = {}        # ident -> Thread, for threads that run
_limbo = {}         # Thread -> Thread, started but not running yet
_dangling = WeakSet()   # every Thread object made (tests look for leftovers)

_threading_atexits = []
_SHUTTING_DOWN = False

excepthook = _thread._excepthook
__excepthook__ = excepthook
ExceptHookArgs = _thread._ExceptHookArgs


class _FailureReporter:
    """Hands an exception that escaped Thread.run() to threading.excepthook.

    Each Thread gets one when it is created, remembering the hooks of that
    moment: late at interpreter shutdown the module attributes may already
    be gone.
    """

    __slots__ = ('_hook', '_sys_hook', '_sys')

    def __init__(self):
        if excepthook is None:
            raise RuntimeError("threading.excepthook is None")
        if _sys.excepthook is None:
            raise RuntimeError("sys.excepthook is None")
        self._hook = excepthook
        self._sys_hook = _sys.excepthook
        self._sys = _sys

    def __call__(self, thread):
        sys_module = self._sys
        info = sys_module.exc_info()
        hook_args = None
        try:
            hook_args = ExceptHookArgs([*info, thread])
            (excepthook or self._hook)(hook_args)
        except Exception as failure:
            failure.__suppress_context__ = True
            del failure
            # Say where the error came from, then let sys.excepthook show
            # the hook's own exception.
            stream = sys_module.stderr if sys_module.stderr is not None \
                else thread._stderr
            print("Exception in threading.excepthook:", file=stream, flush=True)
            (sys_module.excepthook or self._sys_hook)(*sys_module.exc_info())
        finally:
            hook_args = info = None


# ------------------------------------------------------------ Thread

class Thread:
    """A thread of control.

    Either pass a callable as target (called with args and kwargs) or
    override run() in a subclass; then call start().
    """

    _initialized = False

    def __init__(self, group=None, target=None, name=None,
                 args=(), kwargs=None, *, daemon=None, context=None):
        assert group is None, "group argument must be None for now"
        if name:
            name = str(name)
        else:
            name = "Thread-%d" % _counter()
            label = getattr(target, '__name__', None) if target is not None else None
            if label is not None:
                name = "%s (%s)" % (name, label)
        if daemon is None:
            daemon = current_thread().daemon
        elif daemon and not _daemon_threads_allowed():
            raise RuntimeError("daemon threads are disabled in this interpreter")
        self._target = target
        self._args = args
        self._kwargs = {} if kwargs is None else kwargs
        self._name = name
        self._daemonic = daemon
        self._context = context
        self._ident = None
        if _HAVE_THREAD_NATIVE_ID:
            self._native_id = None
        self._handle = _ThreadHandle()
        self._started = Event()
        self._initialized = True
        # For reporting a failure even after sys.stderr was set to None.
        self._stderr = _sys.stderr
        self._report_failure = _FailureReporter()
        _dangling.add(self)

    def __repr__(self):
        assert self._initialized, "Thread.__init__() was not called"
        if self._handle.is_done():
            status = "stopped"
        elif self._started.is_set():
            status = "started"
        else:
            status = "initial"
        if self._daemonic:
            status += " daemon"
        if self._ident is not None:
            status += " %s" % self._ident
        return "<%s(%s, %s)>" % (type(self).__name__, self._name, status)

    def _choose_context(self):
        # run() executes in the given context, a copy of the starting
        # thread's (with -X thread_inherit_context) or a fresh one.
        if self._context is not None:
            return self._context
        if _sys.flags.thread_inherit_context:
            return _contextvars.copy_context()
        return _contextvars.Context()

    def start(self):
        """Start the thread: run() is called in a new thread of control.

        A Thread can be started only once.
        """
        if not self._initialized:
            raise RuntimeError("thread.__init__() not called")
        if self._started.is_set():
            raise RuntimeError("threads can only be started once")
        self._run_context = self._choose_context()
        with _active_limbo_lock:
            _limbo[self] = self
        try:
            _start_joinable_thread(self._bootstrap, handle=self._handle,
                                   daemon=self.daemon)
        except Exception:
            with _active_limbo_lock:
                _limbo.pop(self, None)
            raise
        self._started.wait()    # until the new thread has registered itself

    def run(self):
        """What the thread does; by default call the target.

        Subclasses may override this.
        """
        try:
            if self._target is not None:
                self._target(*self._args, **self._kwargs)
        finally:
            # A finished thread object must not keep these alive (they may
            # refer back to it).
            del self._target, self._args, self._kwargs

    def _bootstrap(self):
        # Entry point of the new thread.
        try:
            self._bootstrap_inner()
        except:
            # A daemon thread can outlive most of the interpreter at exit;
            # then there is nothing left to report to.
            if self._daemonic and _sys is None:
                return
            raise

    def _set_ident(self):
        self._ident = get_ident()

    if _HAVE_THREAD_NATIVE_ID:
        def _set_native_id(self):
            self._native_id = get_native_id()

    def _set_os_name(self):
        if _set_os_thread_name is not None and self._name:
            try:
                _set_os_thread_name(self._name)
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
            context, self._run_context = self._run_context, None
            try:
                context.run(self.run)
            except:
                self._report_failure(self)
        finally:
            with _active_limbo_lock:
                _active.pop(get_ident(), None)

    def _reset_after_fork(self, ident=None):
        # In a child process after fork(): only the forking thread survives.
        # It keeps its object (with ident, its new identifier); every other
        # thread is gone.
        self._started._at_fork_reinit()
        if ident is None:
            try:
                self._handle._set_done()
            except RuntimeError:
                # it never started (or another thread was starting it at the
                # fork): nothing ran, so there is nothing to mark as finished
                pass
            return
        self._ident = ident
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        self._handle = _make_thread_handle(ident)

    def join(self, timeout=None):
        """Wait until the thread ends, or for at most timeout seconds.

        Returns None either way: is_alive() tells whether it ended.
        """
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if not self._started.is_set():
            raise RuntimeError("cannot join thread before it is started")
        if self is current_thread():
            raise RuntimeError("cannot join current thread")
        if timeout is not None and timeout < 0:
            timeout = 0
        self._handle.join(timeout)

    @property
    def name(self):
        """The thread's name: a label with no meaning to the system."""
        assert self._initialized, "Thread.__init__() not called"
        return self._name

    @name.setter
    def name(self, name):
        assert self._initialized, "Thread.__init__() not called"
        self._name = str(name)
        if self._ident == get_ident():
            self._set_os_name()

    @property
    def ident(self):
        """The thread's identifier (see get_ident()), None until started.

        Identifiers of finished threads can be handed out again.
        """
        assert self._initialized, "Thread.__init__() not called"
        return self._ident

    if _HAVE_THREAD_NATIVE_ID:
        @property
        def native_id(self):
            """The system's identifier of the thread, None until started."""
            assert self._initialized, "Thread.__init__() not called"
            return self._native_id

    def is_alive(self):
        """Whether run() has started and not yet finished."""
        assert self._initialized, "Thread.__init__() not called"
        return self._started.is_set() and not self._handle.is_done()

    @property
    def daemon(self):
        """Whether this is a daemon thread, which the program does not wait
        for at exit.  By default the creating thread's setting."""
        assert self._initialized, "Thread.__init__() not called"
        return self._daemonic

    @daemon.setter
    def daemon(self, daemonic):
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if daemonic and not _daemon_threads_allowed():
            raise RuntimeError("daemon threads are disabled in this interpreter")
        if self._started.is_set():
            raise RuntimeError("cannot set daemon status of active thread")
        self._daemonic = daemonic

    def isDaemon(self):
        """Deprecated: read the daemon attribute instead."""
        _deprecated('isDaemon() is deprecated, get the daemon attribute instead')
        return self.daemon

    def setDaemon(self, daemonic):
        """Deprecated: assign the daemon attribute instead."""
        _deprecated('setDaemon() is deprecated, set the daemon attribute instead')
        self.daemon = daemonic

    def getName(self):
        """Deprecated: read the name attribute instead."""
        _deprecated('getName() is deprecated, get the name attribute instead')
        return self.name

    def setName(self, name):
        """Deprecated: assign the name attribute instead."""
        _deprecated('setName() is deprecated, set the name attribute instead')
        self.name = name


class Timer(Thread):
    """A thread that calls function(*args, **kwargs) once interval seconds
    have passed, unless cancel() comes first."""

    def __init__(self, interval, function, args=None, kwargs=None):
        Thread.__init__(self)
        self.interval = interval
        self.function = function
        self.args = [] if args is None else args
        self.kwargs = {} if kwargs is None else kwargs
        self.finished = Event()     # set by cancel(), and once it has fired

    def cancel(self):
        """Stop the timer, unless it has fired already."""
        self.finished.set()

    def run(self):
        cancelled = self.finished.wait(self.interval)
        try:
            if not cancelled:
                self.function(*self.args, **self.kwargs)
        finally:
            self.finished.set()


class _MainThread(Thread):
    """The thread the interpreter started with."""

    def __init__(self):
        Thread.__init__(self, name="MainThread", daemon=False)
        self._started.set()
        self._ident = _get_main_thread_ident()
        self._handle = _make_thread_handle(self._ident)
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        with _active_limbo_lock:
            _active[self._ident] = self


class _ForeignThreadSentinel:
    """Lives in the thread-local storage of a thread this module did not
    start.  When that thread ends, its storage is freed and with it this
    object, which takes the thread's _DummyThread out of `_active`."""

    __slots__ = ('_thread', '_ident')

    def __init__(self, thread):
        self._thread = thread
        self._ident = thread._ident

    def __del__(self):
        try:
            with _active_limbo_lock:
                if _active.get(self._ident) is self._thread:
                    del _active[self._ident]
            self._thread._handle._set_done()
        except Exception:
            pass        # the module may be half gone at interpreter exit


_foreign_thread_storage = local()


class _DummyThread(Thread):
    """Represents a thread started outside this module.

    It counts as alive and daemonic, and cannot be joined.
    """

    def __init__(self):
        Thread.__init__(self, name="Dummy-%d" % _counter(),
                        daemon=_daemon_threads_allowed())
        self._started.set()
        self._set_ident()
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        self._handle = _make_thread_handle(self._ident)
        with _active_limbo_lock:
            _active[self._ident] = self
        _foreign_thread_storage.sentinel = _ForeignThreadSentinel(self)

    def is_alive(self):
        if self._started.is_set() and not self._handle.is_done():
            return True
        raise RuntimeError("thread is not alive")

    def join(self, timeout=None):
        raise RuntimeError("cannot join a dummy thread")


# ------------------------------------------------------------ module functions

def current_thread():
    """The Thread object of the calling thread.

    A thread not started through this module gets a _DummyThread.
    """
    try:
        return _active[get_ident()]
    except KeyError:
        return _DummyThread()


def currentThread():
    """Deprecated spelling of current_thread()."""
    _deprecated('currentThread() is deprecated, use current_thread() instead')
    return current_thread()


def _living_threads():
    with _active_limbo_lock:
        return [*_active.values(), *_limbo.values()]


def active_count():
    """How many Thread objects are alive (see enumerate())."""
    with _active_limbo_lock:
        return len(_active) + len(_limbo)


def activeCount():
    """Deprecated spelling of active_count()."""
    _deprecated('activeCount() is deprecated, use active_count() instead')
    return active_count()


def enumerate():
    """All Thread objects that are alive: started and not finished,
    including daemon and dummy threads and the main thread."""
    return _living_threads()


def main_thread():
    """The Thread object of the main thread."""
    return _main_thread


# ------------------------------------------------------------ exit and fork

def _register_atexit(func, *args, **kwargs):
    """Have func(*args, **kwargs) called at interpreter exit, before the
    non-daemon threads are waited for (the atexit module's functions run
    after that).  The most recently registered function runs first."""
    if _SHUTTING_DOWN:
        raise RuntimeError("can't register atexit after shutdown")
    _threading_atexits.append((func, args, kwargs))


def _shutdown():
    """Run at interpreter exit: call the _register_atexit() functions, then
    wait for all non-daemon threads to finish."""
    global _SHUTTING_DOWN
    if _main_thread._handle.is_done() and _is_main_interpreter():
        return      # done already (an explicit call before exit)
    _SHUTTING_DOWN = True
    while _threading_atexits:
        func, args, kwargs = _threading_atexits.pop()
        func(*args, **kwargs)
    if _main_thread.ident == get_ident():
        _main_thread._handle._set_done()
    _join_remaining_threads()


def _after_fork():
    """os.fork() hook for the child: the forking thread is now the only
    thread, and it becomes the main thread."""
    global _active_limbo_lock, _main_thread
    # The lock may have been held by a thread that does not exist here.
    _active_limbo_lock = RLock()
    me = get_ident()
    survivor = _active.get(me)
    if survivor is None:
        # forked from a thread nobody had made a Thread object for
        survivor = _MainThread()
        if survivor._ident != me:
            survivor._reset_after_fork(me)
    else:
        if isinstance(survivor, _DummyThread):
            # it is the process's main thread now, and joinable as such
            survivor.__class__ = _MainThread
            survivor._name = "MainThread"
            survivor._daemonic = False
        survivor._reset_after_fork(me)
    _main_thread = survivor
    with _active_limbo_lock:
        vanished = {*_active.values(), *_limbo.values(), *_dangling}
        vanished.discard(survivor)
        for thread in vanished:
            thread._reset_after_fork()
        _limbo.clear()
        _active.clear()
        _active[me] = survivor
    _dangling.clear()
    _dangling.add(survivor)


if hasattr(_os, 'register_at_fork'):
    _os.register_at_fork(after_in_child=_after_fork)

_main_thread = _MainThread()


# ------------------------------------------------------------ iterator tools

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
