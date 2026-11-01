#
# multiprocessing.process: the Process object (an API mirroring
# threading.Thread) and the bookkeeping of the current process.
#

__all__ = ['BaseProcess', 'current_process', 'active_children',
           'parent_process']

import os
import sys
import signal
import itertools
import threading
from _weakrefset import WeakSet

try:
    ORIGINAL_DIR = os.path.abspath(os.getcwd())
except OSError:
    ORIGINAL_DIR = None


def current_process():
    """Return the Process object of the running process."""
    return _current_process


def active_children():
    """Return the started children that are still running (reaping the
    finished ones)."""
    _cleanup()
    return list(_children)


def parent_process():
    """Return the parent's Process object (None in the main process)."""
    return _parent_process


def _cleanup():
    # forget the children that have finished
    for p in list(_children):
        if (child_popen := p._popen) and child_popen.poll() is not None:
            _children.discard(p)


class BaseProcess(object):
    """A unit of work run in a child process.  Subclasses supply _Popen
    (how the child is started); the API follows threading.Thread."""

    def _Popen(self):
        raise NotImplementedError

    def __init__(self, group=None, target=None, name=None, args=(), kwargs={},
                 *, daemon=None):
        assert group is None, 'group argument must be None for now'
        count = next(_process_counter)
        self._identity = _current_process._identity + (count,)
        self._config = _current_process._config.copy()
        self._parent_pid = os.getpid()
        self._parent_name = _current_process.name
        self._popen = None
        self._closed = False
        self._target = target
        self._args = tuple(args)
        self._kwargs = dict(kwargs)
        self._name = name or (type(self).__name__ + '-' +
                              ':'.join(str(i) for i in self._identity))
        if daemon is not None:
            self.daemon = daemon
        _dangling.add(self)

    def _check_closed(self):
        if self._closed:
            raise ValueError("process object is closed")

    def run(self):
        """The code run in the child; calls the target by default."""
        if self._target:
            self._target(*self._args, **self._kwargs)

    def start(self):
        """Start the child process."""
        self._check_closed()
        assert self._popen is None, 'cannot start a process twice'
        assert self._parent_pid == os.getpid(), \
               'can only start a process object created by current process'
        assert not _current_process._config.get('daemon'), \
               'daemonic processes are not allowed to have children'
        _cleanup()
        self._popen = self._Popen(self)
        self._sentinel = self._popen.sentinel
        # The parent no longer needs the target and its arguments.
        del self._target, self._args, self._kwargs
        _children.add(self)

    def interrupt(self):
        """Send SIGINT to the child."""
        self._check_closed()
        self._popen.interrupt()

    def terminate(self):
        """Send SIGTERM to the child."""
        self._check_closed()
        self._popen.terminate()

    def kill(self):
        """Send SIGKILL to the child."""
        self._check_closed()
        self._popen.kill()

    def join(self, timeout=None):
        """Wait (at most *timeout* seconds) for the child to finish."""
        self._check_closed()
        assert self._parent_pid == os.getpid(), 'can only join a child process'
        assert self._popen is not None, 'can only join a started process'
        res = self._popen.wait(timeout)
        if res is not None:
            _children.discard(self)

    def is_alive(self):
        """True while the child runs."""
        self._check_closed()
        if self is _current_process:
            return True
        assert self._parent_pid == os.getpid(), 'can only test a child process'

        if self._popen is None:
            return False

        if self._popen.poll() is None:
            return True
        _children.discard(self)
        return False

    def close(self):
        """Release the resources of a finished process object."""
        if self._popen is not None:
            if self._popen.poll() is None:
                raise ValueError("Cannot close a process while it is still running. "
                                 "You should first call join() or terminate().")
            self._popen.close()
            self._popen = None
            del self._sentinel
            _children.discard(self)
        self._closed = True

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, name):
        assert isinstance(name, str), 'name must be a string'
        self._name = name

    @property
    def daemon(self):
        """Daemonic children are terminated when their parent exits."""
        return self._config.get('daemon', False)

    @daemon.setter
    def daemon(self, daemonic):
        assert self._popen is None, 'process has already started'
        self._config['daemon'] = daemonic

    @property
    def authkey(self):
        return self._config['authkey']

    @authkey.setter
    def authkey(self, authkey):
        self._config['authkey'] = AuthenticationString(authkey)

    @property
    def exitcode(self):
        """None while running (or not started), else the exit code
        (negative for a signal)."""
        self._check_closed()
        if self._popen is None:
            return self._popen
        return self._popen.poll()

    @property
    def ident(self):
        """The process id (None before start())."""
        self._check_closed()
        if self is _current_process:
            return os.getpid()
        return self._popen and self._popen.pid

    pid = ident

    @property
    def sentinel(self):
        """A file descriptor that becomes ready when the process ends."""
        self._check_closed()
        try:
            return self._sentinel
        except AttributeError:
            raise ValueError("process not started") from None

    def __repr__(self):
        exitcode = None
        if self is _current_process:
            status = 'started'
        elif self._closed:
            status = 'closed'
        elif self._parent_pid != os.getpid():
            status = 'unknown'
        elif self._popen is None:
            status = 'initial'
        else:
            exitcode = self._popen.poll()
            status = 'started' if exitcode is None else 'stopped'

        parts = [type(self).__name__, 'name=%r' % self._name]
        if self._popen is not None:
            parts.append('pid=%s' % self._popen.pid)
        parts.append('parent=%s' % self._parent_pid)
        parts.append(status)
        if exitcode is not None:
            exitcode = _exitcode_to_name.get(exitcode, exitcode)
            parts.append('exitcode=%s' % exitcode)
        if self.daemon:
            parts.append('daemon')
        return '<%s>' % ' '.join(parts)

    def _bootstrap(self, parent_sentinel=None):
        """Run in the child: set up the process state, run(), and return the
        exit code."""
        from . import util, context
        global _current_process, _parent_process, _process_counter, _children

        try:
            if self._start_method is not None:
                context._force_start_method(self._start_method)
            _process_counter = itertools.count(1)
            _children = set()
            util._close_stdin()
            old_process = _current_process
            _current_process = self
            _parent_process = _ParentProcess(
                self._parent_name, self._parent_pid, parent_sentinel)
            if threading._HAVE_THREAD_NATIVE_ID:
                threading.main_thread()._set_native_id()
            try:
                self._after_fork()
            finally:
                # do not keep the parent's process object alive
                del old_process
            util.info('child process calling self.run()')
            self.run()
            exitcode = 0
        except SystemExit as e:
            if e.code is None:
                exitcode = 0
            elif isinstance(e.code, int):
                exitcode = e.code
            else:
                sys.stderr.write(str(e.code) + '\n')
                exitcode = 1
        except:
            exitcode = 1
            import traceback
            sys.stderr.write('Process %s:\n' % self.name)
            traceback.print_exc()
        finally:
            threading._shutdown()
            util.info('process exiting with exitcode %d' % exitcode)
            util._flush_std_streams()

        return exitcode

    @staticmethod
    def _after_fork():
        from . import util
        util._finalizer_registry.clear()
        util._run_after_forkers()


class AuthenticationString(bytes):
    """bytes that refuse to be pickled except while spawning a child."""

    def __reduce__(self):
        from .context import get_spawning_popen
        if get_spawning_popen() is None:
            raise TypeError(
                'Pickling an AuthenticationString object is '
                'disallowed for security reasons'
                )
        return AuthenticationString, (bytes(self),)


class _ParentProcess(BaseProcess):
    """Stand-in for the parent, as seen from a child."""

    def __init__(self, name, pid, sentinel):
        self._identity = ()
        self._name = name
        self._pid = pid
        self._parent_pid = None
        self._popen = None
        self._closed = False
        self._sentinel = sentinel
        self._config = {}

    def is_alive(self):
        from multiprocessing.connection import wait
        return not wait([self._sentinel], timeout=0)

    @property
    def ident(self):
        return self._pid

    def join(self, timeout=None):
        """Wait until the parent process exits."""
        from multiprocessing.connection import wait
        wait([self._sentinel], timeout=timeout)

    pid = ident


class _MainProcess(BaseProcess):
    """The process the program started in."""

    def __init__(self):
        self._identity = ()
        self._name = 'MainProcess'
        self._parent_pid = None
        self._popen = None
        self._closed = False
        self._config = {'authkey': AuthenticationString(os.urandom(32)),
                        'semprefix': '/mp'}
        # Note: 'semprefix' is the prefix of named semaphores (a leading
        # slash is required by sem_open on POSIX).

    def close(self):
        pass


_parent_process = None
_current_process = _MainProcess()
_process_counter = itertools.count(1)
_children = set()
del _MainProcess

# exit code -> "-SIGNAME" for repr()
_exitcode_to_name = {}

for name, signum in list(signal.__dict__.items()):
    if name[:3] == 'SIG' and '_' not in name:
        _exitcode_to_name[-signum] = f'-{name}'
del name, signum

# Every Process object ever created (tests look for leftovers).
_dangling = WeakSet()
