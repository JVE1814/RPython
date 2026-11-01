"""Higher-level threading interface on top of _thread: Thread, locks,
conditions, semaphores, events, barriers, timers and thread-local data."""

import os as _os
import sys as _sys
import _thread
try:
    import _contextvars
except ImportError:
    class _contextvars:
        class Context:
            def run(self, func, *args, **kwargs):
                return func(*args, **kwargs)
        @staticmethod
        def copy_context():
            return _contextvars.Context()

from time import monotonic as _time
from _weakrefset import WeakSet
from itertools import count as _count
try:
    from _collections import deque as _deque
except ImportError:
    from collections import deque as _deque

__all__ = ['get_ident', 'active_count', 'Condition', 'current_thread',
           'enumerate', 'main_thread', 'TIMEOUT_MAX',
           'Event', 'Lock', 'RLock', 'Semaphore', 'BoundedSemaphore', 'Thread',
           'Barrier', 'BrokenBarrierError', 'Timer', 'ThreadError',
           'ExceptHookArgs', 'setprofile', 'settrace', 'local', 'stack_size',
           'excepthook', 'get_native_id', 'getprofile', 'gettrace',
           'setprofile_all_threads', 'settrace_all_threads']

_start_joinable_thread = _thread.start_joinable_thread
_daemon_threads_allowed = _thread.daemon_threads_allowed
_allocate_lock = _thread.allocate_lock
_LockType = _thread.LockType
_thread_shutdown = _thread._shutdown
_make_thread_handle = _thread._make_thread_handle
_ThreadHandle = _thread._ThreadHandle
get_ident = _thread.get_ident
_get_main_thread_ident = _thread._get_main_thread_ident
_is_main_interpreter = _thread._is_main_interpreter
try:
    get_native_id = _thread.get_native_id
    _HAVE_THREAD_NATIVE_ID = True
    __all__.append('get_native_id')
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
del _thread

try:
    from _thread import _local as local
except ImportError:
    from _threading_local import local



_profile_hook = None
_trace_hook = None


def setprofile(func):
    """Set a profile function for all threads started from the threading
    module (installed with sys.setprofile() in each thread)."""
    global _profile_hook
    _profile_hook = func


def setprofile_all_threads(func):
    """Set a profile function for all threads started from the threading
    module and all Python threads that are currently executing."""
    setprofile(func)
    _sys._setprofileallthreads(func)


def getprofile():
    """Get the profiler function as set by threading.setprofile()."""
    return _profile_hook


def settrace(func):
    """Set a trace function for all threads started from the threading
    module (installed with sys.settrace() in each thread)."""
    global _trace_hook
    _trace_hook = func


def settrace_all_threads(func):
    """Set a trace function for all threads started from the threading
    module and all Python threads that are currently executing."""
    settrace(func)
    _sys._settraceallthreads(func)


def gettrace():
    """Get the trace function as set by threading.settrace()."""
    return _trace_hook



Lock = _LockType


def RLock(*args, **kwargs):
    """Factory returning a new reentrant lock: the lock must be released by
    the thread that acquired it, as many times as it was acquired."""
    if args or kwargs:
        import warnings
        warnings.warn(
            'Passing arguments to RLock is deprecated and will be removed in 3.15',
            DeprecationWarning,
            stacklevel=2,
        )
    if _CRLock is None:
        return _PyRLock(*args, **kwargs)
    return _CRLock(*args, **kwargs)


class _RLock:
    """Pure-Python reentrant lock (used when _thread.RLock is missing, and
    for testing)."""

    def __init__(self):
        self._block = _allocate_lock()
        self._owner = None
        self._count = 0

    def __repr__(self):
        owner = self._owner
        try:
            owner = _active[owner].name
        except KeyError:
            pass
        return "<%s %s.%s object owner=%r count=%d at %s>" % (
            "locked" if self.locked() else "unlocked",
            self.__class__.__module__,
            self.__class__.__qualname__,
            owner,
            self._count,
            hex(id(self))
        )

    def _at_fork_reinit(self):
        self._block._at_fork_reinit()
        self._owner = None
        self._count = 0

    def acquire(self, blocking=True, timeout=-1):
        """Acquire the lock (recursively if this thread already owns it).

        Returns True on success, False if the lock could not be acquired
        (non-blocking call or timeout).
        """
        me = get_ident()
        if self._owner == me:
            self._count += 1
            return 1
        rc = self._block.acquire(blocking, timeout)
        if rc:
            self._owner = me
            self._count = 1
        return rc

    __enter__ = acquire

    def release(self):
        """Release one level of ownership; the lock is unlocked when the
        recursion level reaches zero."""
        if self._owner != get_ident():
            raise RuntimeError("cannot release un-acquired lock")
        self._count = count = self._count - 1
        if not count:
            self._owner = None
            self._block.release()

    def __exit__(self, t, v, tb):
        self.release()

    def locked(self):
        """Return whether this object is locked."""
        return self._block.locked()


    def _acquire_restore(self, state):
        self._block.acquire()
        self._count, self._owner = state

    def _release_save(self):
        if self._count == 0:
            raise RuntimeError("cannot release un-acquired lock")
        count = self._count
        self._count = 0
        owner = self._owner
        self._owner = None
        self._block.release()
        return (count, owner)

    def _is_owned(self):
        return self._owner == get_ident()


    def _recursion_count(self):
        if self._owner != get_ident():
            return 0
        return self._count


_PyRLock = _RLock


class Condition:
    """Condition variable: lets threads wait until notified by another
    thread.  It is associated with a lock (an RLock by default)."""

    def __init__(self, lock=None):
        if lock is None:
            lock = RLock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release
        if hasattr(lock, '_release_save'):
            self._release_save = lock._release_save
        if hasattr(lock, '_acquire_restore'):
            self._acquire_restore = lock._acquire_restore
        if hasattr(lock, '_is_owned'):
            self._is_owned = lock._is_owned
        if hasattr(lock, '_at_fork_reinit'):
            self._at_fork_reinit = self._at_fork_reinit_lock
        self._waiters = _deque()

    def _at_fork_reinit_lock(self):
        self._lock._at_fork_reinit()
        self._waiters.clear()

    def _at_fork_reinit(self):
        self._lock._at_fork_reinit()
        self._waiters.clear()

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *args):
        return self._lock.__exit__(*args)

    def __repr__(self):
        return "<Condition(%s, %d)>" % (self._lock, len(self._waiters))

    def _release_save(self):
        self._lock.release()

    def _acquire_restore(self, x):
        self._lock.acquire()

    def _is_owned(self):
        if self._lock.acquire(False):
            self._lock.release()
            return False
        else:
            return True

    def wait(self, timeout=None):
        """Release the lock, wait until notified or until the timeout
        expires, then re-acquire the lock.  Returns False on timeout."""
        if not self._is_owned():
            raise RuntimeError("cannot wait on un-acquired lock")
        waiter = _allocate_lock()
        waiter.acquire()
        self._waiters.append(waiter)
        saved_state = self._release_save()
        gotit = False
        try:
            if timeout is None:
                waiter.acquire()
                gotit = True
            else:
                if timeout > 0:
                    gotit = waiter.acquire(True, timeout)
                else:
                    gotit = waiter.acquire(False)
            return gotit
        finally:
            self._acquire_restore(saved_state)
            if not gotit:
                try:
                    self._waiters.remove(waiter)
                except ValueError:
                    pass

    def wait_for(self, predicate, timeout=None):
        """Wait until *predicate* (a callable) returns a true value, or
        until the timeout expires; return the last predicate value."""
        endtime = None
        waittime = timeout
        result = predicate()
        while not result:
            if waittime is not None:
                if endtime is None:
                    endtime = _time() + waittime
                else:
                    waittime = endtime - _time()
                    if waittime <= 0:
                        break
            self.wait(waittime)
            result = predicate()
        return result

    def notify(self, n=1):
        """Wake up at most *n* threads waiting on this condition."""
        if not self._is_owned():
            raise RuntimeError("cannot notify on un-acquired lock")
        waiters = self._waiters
        while waiters and n > 0:
            waiter = waiters[0]
            try:
                waiter.release()
            except RuntimeError:
                pass
            else:
                n -= 1
            try:
                waiters.remove(waiter)
            except ValueError:
                pass

    def notify_all(self):
        """Wake up all threads waiting on this condition."""
        self.notify(len(self._waiters))

    def notifyAll(self):
        """Wake up all threads waiting on this condition (deprecated alias
        of notify_all())."""
        import warnings
        warnings.warn('notifyAll() is deprecated, use notify_all() instead',
                      DeprecationWarning, stacklevel=2)
        self.notify_all()


class Semaphore:
    """A counter decremented by acquire() and incremented by release();
    acquire() blocks while the counter is zero."""

    def __init__(self, value=1):
        if value < 0:
            raise ValueError("semaphore initial value must be >= 0")
        self._cond = Condition(Lock())
        self._value = value

    def __repr__(self):
        cls = self.__class__
        return (f"<{cls.__module__}.{cls.__qualname__} at {id(self):#x}:"
                f" value={self._value}>")

    def acquire(self, blocking=True, timeout=None):
        """Decrement the counter, waiting (unless non-blocking) while it is
        zero.  Returns False if the semaphore could not be acquired."""
        if not blocking and timeout is not None:
            raise ValueError("can't specify timeout for non-blocking acquire")
        rc = False
        endtime = None
        with self._cond:
            while self._value == 0:
                if not blocking:
                    break
                if timeout is not None:
                    if endtime is None:
                        endtime = _time() + timeout
                    else:
                        timeout = endtime - _time()
                        if timeout <= 0:
                            break
                self._cond.wait(timeout)
            else:
                self._value -= 1
                rc = True
        return rc

    __enter__ = acquire

    def release(self, n=1):
        """Increment the counter by *n* and wake up waiting threads."""
        if n < 1:
            raise ValueError('n must be one or more')
        with self._cond:
            self._value += n
            self._cond.notify(n)

    def __exit__(self, t, v, tb):
        self.release()


class BoundedSemaphore(Semaphore):
    """A semaphore whose counter may not exceed its initial value."""

    def __init__(self, value=1):
        super().__init__(value)
        self._initial_value = value

    def __repr__(self):
        cls = self.__class__
        return (f"<{cls.__module__}.{cls.__qualname__} at {id(self):#x}:"
                f" value={self._value}/{self._initial_value}>")

    def release(self, n=1):
        """Increment the counter; ValueError if it would exceed the initial
        value."""
        if n < 1:
            raise ValueError('n must be one or more')
        with self._cond:
            if self._value + n > self._initial_value:
                raise ValueError("Semaphore released too many times")
            self._value += n
            self._cond.notify(n)


class Event:
    """A flag that threads can wait on until another thread sets it."""

    def __init__(self):
        self._cond = Condition(Lock())
        self._flag = False

    def __repr__(self):
        cls = self.__class__
        status = 'set' if self._flag else 'unset'
        return f"<{cls.__module__}.{cls.__qualname__} at {id(self):#x}: {status}>"

    def _at_fork_reinit(self):
        self._cond._at_fork_reinit()

    def is_set(self):
        """Return true if and only if the internal flag is true."""
        return self._flag

    def isSet(self):
        """Deprecated alias of is_set()."""
        import warnings
        warnings.warn('isSet() is deprecated, use is_set() instead',
                      DeprecationWarning, stacklevel=2)
        return self.is_set()

    def set(self):
        """Set the flag and wake up all waiting threads."""
        with self._cond:
            self._flag = True
            self._cond.notify_all()

    def clear(self):
        """Reset the flag to false."""
        with self._cond:
            self._flag = False

    def wait(self, timeout=None):
        """Block until the flag is true or the timeout expires; return the
        flag."""
        with self._cond:
            signaled = self._flag
            if not signaled:
                signaled = self._cond.wait(timeout)
            return signaled


class Barrier:
    """A barrier for a fixed number of threads: each wait() blocks until
    all parties have called it."""

    def __init__(self, parties, action=None, timeout=None):
        if parties < 1:
            raise ValueError("parties must be >= 1")
        self._cond = Condition(Lock())
        self._action = action
        self._timeout = timeout
        self._parties = parties
        self._state = 0
        self._count = 0

    def __repr__(self):
        cls = self.__class__
        if self.broken:
            return f"<{cls.__module__}.{cls.__qualname__} at {id(self):#x}: broken>"
        return (f"<{cls.__module__}.{cls.__qualname__} at {id(self):#x}:"
                f" waiters={self.n_waiting}/{self.parties}>")

    def wait(self, timeout=None):
        """Wait for all parties; returns an index in range(parties) unique
        to each thread.  Raises BrokenBarrierError on abort/reset/timeout."""
        if timeout is None:
            timeout = self._timeout
        with self._cond:
            self._enter()
            index = self._count
            self._count += 1
            try:
                if index + 1 == self._parties:
                    self._release()
                else:
                    self._wait(timeout)
                return index
            finally:
                self._count -= 1
                self._exit()

    def _enter(self):
        while self._state in (-1, 1):
            self._cond.wait()
        if self._state < 0:
            raise BrokenBarrierError
        assert self._state == 0

    def _release(self):
        try:
            if self._action:
                self._action()
            self._state = 1
            self._cond.notify_all()
        except:
            self._break()
            raise

    def _wait(self, timeout):
        if not self._cond.wait_for(lambda : self._state != 0, timeout):
            self._break()
            raise BrokenBarrierError
        if self._state < 0:
            raise BrokenBarrierError
        assert self._state == 1

    def _exit(self):
        if self._count == 0:
            if self._state in (-1, 1):
                self._state = 0
                self._cond.notify_all()

    def reset(self):
        """Return the barrier to its initial state; waiting threads get
        BrokenBarrierError."""
        with self._cond:
            if self._count > 0:
                if self._state == 0:
                    self._state = -1
                elif self._state == -2:
                    self._state = -1
            else:
                self._state = 0
            self._cond.notify_all()

    def abort(self):
        """Put the barrier into the broken state."""
        with self._cond:
            self._break()

    def _break(self):
        self._state = -2
        self._cond.notify_all()

    @property
    def parties(self):
        """The number of threads required to trip the barrier."""
        return self._parties

    @property
    def n_waiting(self):
        """The number of threads currently waiting."""
        if self._state == 0:
            return self._count
        return 0

    @property
    def broken(self):
        """True if the barrier is broken."""
        return self._state == -2


class BrokenBarrierError(RuntimeError):
    pass


_counter = _count(1).__next__
def _newname(name_template):
    return name_template % _counter()


_active_limbo_lock = RLock()
_active = {}
_limbo = {}
_dangling = WeakSet()



class Thread:
    """A thread of control: either pass a callable as *target* or override
    run() in a subclass."""

    _initialized = False

    def __init__(self, group=None, target=None, name=None,
                 args=(), kwargs=None, *, daemon=None, context=None):
        """*group* must be None; *target* is called by run() with *args*
        and *kwargs*; *name* defaults to "Thread-N (target)"; *daemon*
        defaults to the creating thread's daemon flag; *context* is the
        contextvars.Context to run in (a copy of the caller's context when
        sys.flags.thread_inherit_context is set, else a new one)."""
        assert group is None, "group argument must be None for now"
        if kwargs is None:
            kwargs = {}
        if name:
            name = str(name)
        else:
            name = _newname("Thread-%d")
            if target is not None:
                try:
                    target_name = target.__name__
                    name += f" ({target_name})"
                except AttributeError:
                    pass

        self._target = target
        self._name = name
        self._args = args
        self._kwargs = kwargs
        if daemon is not None:
            if daemon and not _daemon_threads_allowed():
                raise RuntimeError('daemon threads are disabled in this (sub)interpreter')
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
        self._stderr = _sys.stderr
        self._invoke_excepthook = _make_invoke_excepthook()
        _dangling.add(self)

    def _after_fork(self, new_ident=None):
        self._started._at_fork_reinit()
        if new_ident is not None:
            self._ident = new_ident
            assert self._handle.ident == new_ident
        else:
            pass

    def __repr__(self):
        assert self._initialized, "Thread.__init__() was not called"
        status = "initial"
        if self._started.is_set():
            status = "started"
        if self._handle.is_done():
            status = "stopped"
        if self._daemonic:
            status += " daemon"
        if self._ident is not None:
            status += " %s" % self._ident
        return "<%s(%s, %s)>" % (self.__class__.__name__, self._name, status)

    def start(self):
        """Start the thread's activity (at most once per Thread object)."""
        if not self._initialized:
            raise RuntimeError("thread.__init__() not called")

        if self._started.is_set():
            raise RuntimeError("threads can only be started once")

        with _active_limbo_lock:
            _limbo[self] = self

        if self._context is None:
            if _sys.flags.thread_inherit_context:
                self._context = _contextvars.copy_context()
            else:
                self._context = _contextvars.Context()

        try:
            _start_joinable_thread(self._bootstrap, handle=self._handle,
                                   daemon=self.daemon)
        except Exception:
            with _active_limbo_lock:
                del _limbo[self]
            raise
        self._started.wait()

    def run(self):
        """The thread's activity: calls the target with args/kwargs.
        Subclasses may override."""
        try:
            if self._target is not None:
                self._target(*self._args, **self._kwargs)
        finally:
            del self._target, self._args, self._kwargs

    def _bootstrap(self):
        try:
            self._bootstrap_inner()
        except:
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
            self._started.set()
            with _active_limbo_lock:
                _active[self._ident] = self
                del _limbo[self]

            if _trace_hook:
                _sys.settrace(_trace_hook)
            if _profile_hook:
                _sys.setprofile(_profile_hook)

            try:
                self._context.run(self.run)
            except:
                self._invoke_excepthook(self)
        finally:
            self._delete()

    def _delete(self):
        "Remove current thread from the dict of currently running threads."
        with _active_limbo_lock:
            del _active[get_ident()]

    def join(self, timeout=None):
        """Wait until the thread terminates (or the timeout in seconds
        expires; check is_alive() afterwards).  Joining the current thread
        or a thread that was not started raises RuntimeError."""
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if not self._started.is_set():
            raise RuntimeError("cannot join thread before it is started")
        if self is current_thread():
            raise RuntimeError("cannot join current thread")

        if timeout is not None:
            timeout = max(timeout, 0)

        self._handle.join(timeout)

    @property
    def name(self):
        """A string used for identification purposes only."""
        assert self._initialized, "Thread.__init__() not called"
        return self._name

    @name.setter
    def name(self, name):
        assert self._initialized, "Thread.__init__() not called"
        self._name = str(name)
        if get_ident() == self._ident:
            self._set_os_name()

    @property
    def ident(self):
        """Thread identifier of this thread or None if it has not been
        started (a nonzero integer, possibly recycled after exit)."""
        assert self._initialized, "Thread.__init__() not called"
        return self._ident

    if _HAVE_THREAD_NATIVE_ID:
        @property
        def native_id(self):
            """Native integral thread ID of this thread, or None if it has
            not been started (OS-assigned, possibly recycled)."""
            assert self._initialized, "Thread.__init__() not called"
            return self._native_id

    def is_alive(self):
        """Return whether the thread is alive (between start() returning
        and run() terminating)."""
        assert self._initialized, "Thread.__init__() not called"
        return self._started.is_set() and not self._handle.is_done()

    @property
    def daemon(self):
        """Whether this thread is a daemon thread (the program exits when
        only daemon threads are left).  Must be set before start()."""
        assert self._initialized, "Thread.__init__() not called"
        return self._daemonic

    @daemon.setter
    def daemon(self, daemonic):
        if not self._initialized:
            raise RuntimeError("Thread.__init__() not called")
        if daemonic and not _daemon_threads_allowed():
            raise RuntimeError('daemon threads are disabled in this interpreter')
        if self._started.is_set():
            raise RuntimeError("cannot set daemon status of active thread")
        self._daemonic = daemonic

    def isDaemon(self):
        """Return whether this thread is a daemon (deprecated; use the
        daemon attribute)."""
        import warnings
        warnings.warn('isDaemon() is deprecated, get the daemon attribute instead',
                      DeprecationWarning, stacklevel=2)
        return self.daemon

    def setDaemon(self, daemonic):
        """Set whether this thread is a daemon (deprecated; set the daemon
        attribute)."""
        import warnings
        warnings.warn('setDaemon() is deprecated, set the daemon attribute instead',
                      DeprecationWarning, stacklevel=2)
        self.daemon = daemonic

    def getName(self):
        """Return a string used for identification purposes only
        (deprecated; use the name attribute)."""
        import warnings
        warnings.warn('getName() is deprecated, get the name attribute instead',
                      DeprecationWarning, stacklevel=2)
        return self.name

    def setName(self, name):
        """Set the name string for this thread (deprecated; set the name
        attribute)."""
        import warnings
        warnings.warn('setName() is deprecated, set the name attribute instead',
                      DeprecationWarning, stacklevel=2)
        self.name = name


try:
    from _thread import (_excepthook as excepthook,
                         _ExceptHookArgs as ExceptHookArgs)
except ImportError:
    from traceback import print_exception as _print_exception
    from collections import namedtuple

    _ExceptHookArgs = namedtuple(
        'ExceptHookArgs',
        'exc_type exc_value exc_traceback thread')

    def ExceptHookArgs(args):
        return _ExceptHookArgs(*args)

    def excepthook(args, /):
        """Handle an uncaught exception raised by Thread.run()."""
        if args.exc_type == SystemExit:
            return

        if _sys is not None and _sys.stderr is not None:
            stderr = _sys.stderr
        elif args.thread is not None:
            stderr = args.thread._stderr
            if stderr is None:
                return
        else:
            return

        if args.thread is not None:
            name = args.thread.name
        else:
            name = get_ident()
        print(f"Exception in thread {name}:",
              file=stderr, flush=True)
        _print_exception(args.exc_type, args.exc_value, args.exc_traceback,
                         file=stderr)
        stderr.flush()


__excepthook__ = excepthook


def _make_invoke_excepthook():

    old_excepthook = excepthook
    old_sys_excepthook = _sys.excepthook
    if old_excepthook is None:
        raise RuntimeError("threading.excepthook is None")
    if old_sys_excepthook is None:
        raise RuntimeError("sys.excepthook is None")

    sys_exc_info = _sys.exc_info
    local_print = print
    local_sys = _sys

    def invoke_excepthook(thread):
        global excepthook
        try:
            hook = excepthook
            if hook is None:
                hook = old_excepthook

            args = ExceptHookArgs([*sys_exc_info(), thread])

            hook(args)
        except Exception as exc:
            exc.__suppress_context__ = True
            del exc

            if local_sys is not None and local_sys.stderr is not None:
                stderr = local_sys.stderr
            else:
                stderr = thread._stderr

            local_print("Exception in threading.excepthook:",
                        file=stderr, flush=True)

            if local_sys is not None and local_sys.excepthook is not None:
                sys_excepthook = local_sys.excepthook
            else:
                sys_excepthook = old_sys_excepthook

            sys_excepthook(*sys_exc_info())
        finally:
            args = None

    return invoke_excepthook



class Timer(Thread):
    """A thread that calls *function* after *interval* seconds unless
    cancel() is called first."""

    def __init__(self, interval, function, args=None, kwargs=None):
        Thread.__init__(self)
        self.interval = interval
        self.function = function
        self.args = args if args is not None else []
        self.kwargs = kwargs if kwargs is not None else {}
        self.finished = Event()

    def cancel(self):
        """Stop the timer if it hasn't finished yet."""
        self.finished.set()

    def run(self):
        self.finished.wait(self.interval)
        if not self.finished.is_set():
            self.function(*self.args, **self.kwargs)
        self.finished.set()



class _MainThread(Thread):

    def __init__(self):
        Thread.__init__(self, name="MainThread", daemon=False)
        self._started.set()
        self._ident = _get_main_thread_ident()
        self._handle = _make_thread_handle(self._ident)
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        with _active_limbo_lock:
            _active[self._ident] = self


_thread_local_info = local()


class _DeleteDummyThreadOnDel:
    """Removes a _DummyThread from _active when the OS thread it represents
    finishes (its thread-local data is released)."""

    def __init__(self, dummy_thread):
        self._dummy_thread = dummy_thread
        self._tident = dummy_thread.ident
        _thread_local_info._track_dummy_thread_ref = self

    def __del__(self, _active_limbo_lock=_active_limbo_lock, _active=_active):
        with _active_limbo_lock:
            if _active.get(self._tident) is self._dummy_thread:
                _active.pop(self._tident, None)



class _DummyThread(Thread):

    def __init__(self):
        Thread.__init__(self, name=_newname("Dummy-%d"),
                        daemon=_daemon_threads_allowed())
        self._started.set()
        self._set_ident()
        self._handle = _make_thread_handle(self._ident)
        if _HAVE_THREAD_NATIVE_ID:
            self._set_native_id()
        with _active_limbo_lock:
            _active[self._ident] = self
        _DeleteDummyThreadOnDel(self)

    def is_alive(self):
        if not self._handle.is_done() and self._started.is_set():
            return True
        raise RuntimeError("thread is not alive")

    def join(self, timeout=None):
        raise RuntimeError("cannot join a dummy thread")

    def _after_fork(self, new_ident=None):
        if new_ident is not None:
            self.__class__ = _MainThread
            self._name = 'MainThread'
            self._daemonic = False
        Thread._after_fork(self, new_ident=new_ident)



def current_thread():
    """Return the Thread object for the caller's thread of control (a
    dummy object for threads not created through threading)."""
    try:
        return _active[get_ident()]
    except KeyError:
        return _DummyThread()


def currentThread():
    """Deprecated alias of current_thread()."""
    import warnings
    warnings.warn('currentThread() is deprecated, use current_thread() instead',
                  DeprecationWarning, stacklevel=2)
    return current_thread()


def active_count():
    """Return the number of Thread objects currently alive (the length of
    the list returned by enumerate())."""
    with _active_limbo_lock:
        return len(_active) + len(_limbo)


def activeCount():
    """Deprecated alias of active_count()."""
    import warnings
    warnings.warn('activeCount() is deprecated, use active_count() instead',
                  DeprecationWarning, stacklevel=2)
    return active_count()


def _enumerate():
    return list(_active.values()) + list(_limbo.values())


def enumerate():
    """Return a list of all Thread objects currently alive (including
    daemonic and dummy threads and the main thread)."""
    with _active_limbo_lock:
        return list(_active.values()) + list(_limbo.values())


from _thread import stack_size


_main_thread = _MainThread()

_threading_atexits = []
_SHUTTING_DOWN = False


def _register_atexit(func, *arg, **kwargs):
    """Register a function called when non-daemon threads have been joined
    at interpreter shutdown (before atexit callbacks run)."""
    if _SHUTTING_DOWN:
        raise RuntimeError("can't register atexit after shutdown")

    _threading_atexits.append(lambda: func(*arg, **kwargs))


def _shutdown():
    """Wait for all non-daemon threads (called at interpreter exit)."""
    if _main_thread._handle.is_done() and _is_main_interpreter():
        return

    global _SHUTTING_DOWN
    _SHUTTING_DOWN = True

    for atexit_call in reversed(_threading_atexits):
        atexit_call()

    if _main_thread.ident == get_ident():
        _main_thread._handle._set_done()

    _thread_shutdown()


def main_thread():
    """Return the main thread object (the one the interpreter started
    in)."""
    return _main_thread


def _after_fork():
    """Clean up threading state in a child process after os.fork()."""
    global _active_limbo_lock, _main_thread
    _active_limbo_lock = RLock()

    new_active = {}

    try:
        current = _active[get_ident()]
    except KeyError:
        current = _MainThread()

    _main_thread = current

    with _active_limbo_lock:
        threads = set(_enumerate())
        threads.update(_dangling)
        for thread in threads:
            if thread is current:
                ident = get_ident()
                thread._after_fork(new_ident=ident)
                new_active[ident] = thread
            else:
                thread._after_fork()

        _limbo.clear()
        _active.clear()
        _active.update(new_active)
        assert len(_active) == 1


if hasattr(_os, "register_at_fork"):
    _os.register_at_fork(after_in_child=_after_fork)
