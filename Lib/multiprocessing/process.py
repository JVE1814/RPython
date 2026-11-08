#
# multiprocessing.process -- Process objects (shaped like threading.Thread)
# and the record of which process we are, who our parent is and which
# children we have started.
#

__all__ = ['BaseProcess', 'current_process', 'active_children',
           'parent_process']

import itertools
import os
import signal
import sys
import threading
from _weakrefset import WeakSet

# The working directory at import time; spawned children need it to find
# the main script when it was given as a relative path.
try:
    ORIGINAL_DIR = os.path.abspath(os.getcwd())
except OSError:
    ORIGINAL_DIR = None


def current_process():
    """The Process object standing for the running process."""
    return _current_process


def parent_process():
    """The Process object of our parent, or None in the main process."""
    return _parent_process


def active_children():
    """Children started by this process that have not finished yet."""
    _cleanup()
    return list(_children)


def _cleanup():
    # Drop children whose exit status has been collected.
    for child in list(_children):
        handle = child._popen
        if handle and handle.poll() is not None:
            _children.discard(child)


class BaseProcess(object):
    """Work to be done in a separate process.

    A subclass decides how the child is created by providing _Popen; the
    rest of the API -- start(), join(), is_alive(), name, daemon, ... --
    follows threading.Thread."""

    def _Popen(self):
        raise NotImplementedError

    def __init__(self, group=None, target=None, name=None, args=(), kwargs={},
                 *, daemon=None):
        assert group is None, 'group argument must be None for now'
        creator = _current_process
        self._identity = creator._identity + (next(_process_counter),)
        # Each child gets its own copy of the creator's settings (authkey,
        # daemon flag, temp dir, semaphore prefix).
        self._config = creator._config.copy()
        self._parent_pid = os.getpid()
        self._parent_name = creator.name
        self._popen = None
        self._closed = False
        self._target = target
        self._args = tuple(args)
        self._kwargs = dict(kwargs)
        if name:
            self._name = name
        else:
            self._name = '%s-%s' % (type(self).__name__,
                                    ':'.join(map(str, self._identity)))
        if daemon is not None:
            self.daemon = daemon
        _dangling.add(self)

    # -- internal checks ---------------------------------------------------

    def _check_closed(self):
        if self._closed:
            raise ValueError("process object is closed")

    def _check_in_parent(self, message):
        assert self._parent_pid == os.getpid(), message

    # -- the work ----------------------------------------------------------

    def run(self):
        """What the child executes; by default target(*args, **kwargs)."""
        if self._target:
            self._target(*self._args, **self._kwargs)

    # -- life cycle --------------------------------------------------------

    def start(self):
        """Create the child process and run run() in it."""
        self._check_closed()
        assert self._popen is None, 'cannot start a process twice'
        self._check_in_parent(
            'can only start a process object created by current process')
        assert not _current_process._config.get('daemon'), \
            'daemonic processes are not allowed to have children'
        _cleanup()
        handle = self._Popen(self)
        self._popen = handle
        self._sentinel = handle.sentinel
        # The child has its own copy now; do not keep the target (and what
        # it references) alive in the parent.
        del self._target, self._args, self._kwargs
        _children.add(self)

    def _signal_child(self, how):
        self._check_closed()
        getattr(self._popen, how)()

    def interrupt(self):
        """Send SIGINT to the child."""
        self._signal_child('interrupt')

    def terminate(self):
        """Send SIGTERM to the child."""
        self._signal_child('terminate')

    def kill(self):
        """Send SIGKILL to the child."""
        self._signal_child('kill')

    def join(self, timeout=None):
        """Block until the child exits or *timeout* seconds have passed."""
        self._check_closed()
        self._check_in_parent('can only join a child process')
        assert self._popen is not None, 'can only join a started process'
        if self._popen.wait(timeout) is not None:
            _children.discard(self)

    def is_alive(self):
        """Whether the child is still running."""
        self._check_closed()
        if self is _current_process:
            return True
        self._check_in_parent('can only test a child process')
        handle = self._popen
        if handle is None:
            return False
        if handle.poll() is None:
            return True
        _children.discard(self)
        return False

    def close(self):
        """Free the resources held for a child that has finished.  Most
        methods raise ValueError afterwards."""
        handle = self._popen
        if handle is not None:
            if handle.poll() is None:
                raise ValueError(
                    "Cannot close a process while it is still running. "
                    "You should first call join() or terminate().")
            handle.close()
            self._popen = None
            del self._sentinel
            _children.discard(self)
        self._closed = True

    # -- attributes --------------------------------------------------------

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, name):
        assert isinstance(name, str), 'name must be a string'
        self._name = name

    @property
    def daemon(self):
        """A daemonic child is terminated when its parent exits, and may
        not start children of its own."""
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
        """The child's exit code: None until it has finished, -N if signal
        N killed it."""
        self._check_closed()
        handle = self._popen
        return None if handle is None else handle.poll()

    @property
    def ident(self):
        """Process id, or None before the child was started."""
        self._check_closed()
        if self is _current_process:
            return os.getpid()
        handle = self._popen
        return handle and handle.pid

    pid = ident

    @property
    def sentinel(self):
        """A handle that becomes readable when the child ends (usable with
        multiprocessing.connection.wait())."""
        self._check_closed()
        try:
            return self._sentinel
        except AttributeError:
            raise ValueError("process not started") from None

    def _describe_state(self):
        # -> (state word, exit code or None) for repr()
        if self is _current_process:
            return 'started', None
        if self._closed:
            return 'closed', None
        if self._parent_pid != os.getpid():
            return 'unknown', None
        if self._popen is None:
            return 'initial', None
        code = self._popen.poll()
        return ('started' if code is None else 'stopped'), code

    def __repr__(self):
        state, code = self._describe_state()
        words = [type(self).__name__, f'name={self._name!r}']
        if self._popen is not None:
            words.append(f'pid={self._popen.pid}')
        words.append(f'parent={self._parent_pid}')
        words.append(state)
        if code is not None:
            words.append(f'exitcode={_exitcode_to_name.get(code, code)}')
        if self.daemon:
            words.append('daemon')
        return '<' + ' '.join(words) + '>'

    # -- what runs in the child --------------------------------------------

    def _bootstrap(self, parent_sentinel=None):
        """Turn the freshly created child into this process, call run() and
        return the exit code."""
        from . import util, context
        global _current_process, _parent_process, _process_counter, _children
        exitcode = 1
        try:
            if self._start_method is not None:
                context._force_start_method(self._start_method)
            # The parent's children and numbering mean nothing here.
            _process_counter = itertools.count(1)
            _children = set()
            util._close_stdin()
            _current_process = self
            _parent_process = _ParentProcess(self._parent_name,
                                             self._parent_pid, parent_sentinel)
            if threading._HAVE_THREAD_NATIVE_ID:
                threading.main_thread()._set_native_id()
            self._after_fork()
            util.info('child process calling self.run()')
            self.run()
            exitcode = 0
        except SystemExit as exc:
            exitcode = _exit_status(exc.code)
        except BaseException:
            import traceback
            sys.stderr.write('Process %s:\n' % self.name)
            traceback.print_exc()
            exitcode = 1
        finally:
            threading._shutdown()
            util.info('process exiting with exitcode %d' % exitcode)
            util._flush_std_streams()
        return exitcode

    @staticmethod
    def _after_fork():
        # Finalizers and fork hooks registered by the parent: the former do
        # not apply to us, the latter exist to be run now.
        from . import util
        util._finalizer_registry.clear()
        util._run_after_forkers()


def _exit_status(code):
    # SystemExit's argument -> process exit status, like the interpreter.
    if code is None:
        return 0
    if isinstance(code, int):
        return code
    sys.stderr.write(str(code) + '\n')
    return 1


class AuthenticationString(bytes):
    """An authentication key: bytes that may only be pickled while a
    child process is being started."""

    def __reduce__(self):
        from .context import get_spawning_popen
        if get_spawning_popen() is None:
            raise TypeError(
                'Pickling an AuthenticationString object is '
                'disallowed for security reasons')
        return AuthenticationString, (bytes(self),)


class _ParentProcess(BaseProcess):
    """How a child sees its parent: name and pid, plus the read end of a
    pipe that reaches EOF when the parent goes away."""

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

    pid = ident

    def join(self, timeout=None):
        """Wait for the parent to exit."""
        from multiprocessing.connection import wait
        wait([self._sentinel], timeout=timeout)


class _MainProcess(BaseProcess):
    """The process the program was started as."""

    def __init__(self):
        self._identity = ()
        self._name = 'MainProcess'
        self._parent_pid = None
        self._popen = None
        self._closed = False
        # 'semprefix' starts the names of POSIX named semaphores, which must
        # begin with a slash.
        self._config = {'authkey': AuthenticationString(os.urandom(32)),
                        'semprefix': '/mp'}

    def close(self):
        pass


_parent_process = None
_current_process = _MainProcess()
_process_counter = itertools.count(1)
_children = set()
del _MainProcess

# repr() shows a child killed by a signal as e.g. "exitcode=-SIGTERM".
_exitcode_to_name = {
    -signum: f'-{name}'
    for name, signum in signal.__dict__.items()
    if name.startswith('SIG') and '_' not in name and isinstance(signum, int)
}

# Every Process object created and still alive; the test suite checks it
# for processes that were left behind.
_dangling = WeakSet()
