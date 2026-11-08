#
# multiprocessing.context -- start-method contexts.
#
# A context pairs one way of creating child processes ("fork", "spawn" or
# "forkserver") with factories for everything that has to agree with it:
# locks, queues, pools, shared values.  The package's top-level functions
# are the methods of the default context, whose start method is chosen the
# first time it is needed.
#

import os
import sys
import threading

from . import process
from . import reduction

__all__ = ()


class ProcessError(Exception):
    pass


class BufferTooShort(ProcessError):
    pass


class TimeoutError(ProcessError):
    pass


class AuthenticationError(ProcessError):
    pass


class BaseContext(object):
    """Factories shared by every context; subclasses set _name and Process."""

    ProcessError = ProcessError
    BufferTooShort = BufferTooShort
    TimeoutError = TimeoutError
    AuthenticationError = AuthenticationError

    current_process = staticmethod(process.current_process)
    parent_process = staticmethod(process.parent_process)
    active_children = staticmethod(process.active_children)

    def cpu_count(self):
        """Return the number of CPUs in the system."""
        count = os.cpu_count()
        if count is None:
            raise NotImplementedError('cannot determine number of cpus')
        return count

    # -- objects bound to this context ---------------------------------------

    def Manager(self):
        """Start a manager server process; return a SyncManager for it."""
        from .managers import SyncManager
        manager = SyncManager(ctx=self.get_context())
        manager.start()
        return manager

    def Pipe(self, duplex=True):
        """Return a pair of connected Connection objects."""
        from .connection import Pipe
        return Pipe(duplex)

    def Lock(self):
        """A non-recursive lock shared between processes."""
        from .synchronize import Lock
        return Lock(ctx=self.get_context())

    def RLock(self):
        """A recursive lock shared between processes."""
        from .synchronize import RLock
        return RLock(ctx=self.get_context())

    def Condition(self, lock=None):
        """A condition variable shared between processes."""
        from .synchronize import Condition
        return Condition(lock, ctx=self.get_context())

    def Semaphore(self, value=1):
        """A semaphore shared between processes."""
        from .synchronize import Semaphore
        return Semaphore(value, ctx=self.get_context())

    def BoundedSemaphore(self, value=1):
        """A bounded semaphore shared between processes."""
        from .synchronize import BoundedSemaphore
        return BoundedSemaphore(value, ctx=self.get_context())

    def Event(self):
        """An event shared between processes."""
        from .synchronize import Event
        return Event(ctx=self.get_context())

    def Barrier(self, parties, action=None, timeout=None):
        """A barrier shared between processes."""
        from .synchronize import Barrier
        return Barrier(parties, action, timeout, ctx=self.get_context())

    def Queue(self, maxsize=0):
        """A FIFO queue between processes."""
        from .queues import Queue
        return Queue(maxsize, ctx=self.get_context())

    def JoinableQueue(self, maxsize=0):
        """A queue with task_done() and join()."""
        from .queues import JoinableQueue
        return JoinableQueue(maxsize, ctx=self.get_context())

    def SimpleQueue(self):
        """A minimal queue: put(), get(), empty(), close()."""
        from .queues import SimpleQueue
        return SimpleQueue(ctx=self.get_context())

    def Pool(self, processes=None, initializer=None, initargs=(),
             maxtasksperchild=None):
        """A pool of worker processes."""
        from .pool import Pool
        return Pool(processes, initializer, initargs, maxtasksperchild,
                    context=self.get_context())

    def RawValue(self, typecode_or_type, *args):
        """A ctypes object in shared memory, without a lock."""
        from .sharedctypes import RawValue
        return RawValue(typecode_or_type, *args)

    def RawArray(self, typecode_or_type, size_or_initializer):
        """A ctypes array in shared memory, without a lock."""
        from .sharedctypes import RawArray
        return RawArray(typecode_or_type, size_or_initializer)

    def Value(self, typecode_or_type, *args, lock=True):
        """A ctypes object in shared memory, guarded by a lock."""
        from .sharedctypes import Value
        return Value(typecode_or_type, *args, lock=lock,
                     ctx=self.get_context())

    def Array(self, typecode_or_type, size_or_initializer, *, lock=True):
        """A ctypes array in shared memory, guarded by a lock."""
        from .sharedctypes import Array
        return Array(typecode_or_type, size_or_initializer, lock=lock,
                     ctx=self.get_context())

    # -- configuration -------------------------------------------------------

    def freeze_support(self):
        """In a frozen Windows program started as a child, run the child
        and exit.  Does nothing anywhere else."""
        if sys.platform == 'win32' and getattr(sys, 'frozen', False):
            from .spawn import freeze_support
            freeze_support()

    def get_logger(self):
        """Return the multiprocessing logger."""
        from .util import get_logger
        return get_logger()

    def log_to_stderr(self, level=None):
        """Send multiprocessing's log records to stderr."""
        from .util import log_to_stderr
        return log_to_stderr(level)

    def allow_connection_pickling(self):
        """Allow Connection objects to be pickled.  (Importing the
        connection module registers the reducers.)"""
        from . import connection  # noqa: F401

    def set_executable(self, executable):
        """Choose the interpreter that spawned children run."""
        from .spawn import set_executable
        set_executable(executable)

    def set_forkserver_preload(self, module_names, *, on_error='ignore'):
        """Name modules the fork server should import before forking."""
        from .forkserver import set_forkserver_preload
        set_forkserver_preload(module_names, on_error=on_error)

    def get_context(self, method=None):
        if method is None:
            return self
        ctx = _concrete_contexts.get(method)
        if ctx is None:
            raise ValueError('cannot find context for %r' % method)
        ctx._check_available()
        return ctx

    def get_start_method(self, allow_none=False):
        return self._name

    def set_start_method(self, method, force=False):
        raise ValueError('cannot set start method of concrete context')

    @property
    def reducer(self):
        """The module (or object) that pickles data for children."""
        return reduction

    @reducer.setter
    def reducer(self, value):
        _install_reducer(value)

    def _check_available(self):
        pass


def _install_reducer(value):
    # Other modules read context.reduction when they need the pickler.
    global reduction
    reduction = value


# -- the Process classes of each start method --------------------------------

def _popen_class(module_name):
    import importlib
    return importlib.import_module('.' + module_name, __package__).Popen


class Process(process.BaseProcess):
    """Process of the default context: the start method is looked up when
    the process is started."""

    _start_method = None

    @staticmethod
    def _Popen(process_obj):
        return _default_context.get_context().Process._Popen(process_obj)

    @staticmethod
    def _after_fork():
        return _default_context.get_context().Process._after_fork()


class _MethodProcess(process.BaseProcess):
    # Subclasses name their start method and the module with its Popen.
    _start_method = None
    _popen_module = None

    @classmethod
    def _Popen(cls, process_obj):
        return _popen_class(cls._popen_module)(process_obj)


class DefaultContext(BaseContext):
    """The context behind the package-level functions."""

    Process = Process

    def __init__(self, context):
        # *context* is the default context to use, or None to pick one the
        # first time it is needed (that probes the temp directory, which
        # importing multiprocessing must not do).
        self._default_context = context
        self._actual_context = None

    def _resolve_default(self):
        if self._default_context is None:
            self._default_context = _concrete_contexts[_default_method_name()]
        return self._default_context

    def get_context(self, method=None):
        if method is not None:
            return super().get_context(method)
        if self._actual_context is None:
            self._actual_context = self._resolve_default()
        return self._actual_context

    def set_start_method(self, method, force=False):
        if self._actual_context is not None and not force:
            raise RuntimeError('context has already been set')
        if method is None and force:
            self._actual_context = None
        else:
            self._actual_context = self.get_context(method)

    def get_start_method(self, allow_none=False):
        if self._actual_context is None:
            if allow_none:
                return None
            self._actual_context = self._resolve_default()
        return self._actual_context._name

    def get_all_start_methods(self):
        """The start methods this platform supports, the default first."""
        if sys.platform == 'win32':
            return ['spawn']
        default = self._resolve_default()._name
        available = ['fork', 'spawn']
        if default != 'spawn' and reduction.HAVE_SEND_HANDLE:
            available.append('forkserver')
        return [default] + [m for m in available if m != default]


if sys.platform != 'win32':

    class ForkProcess(_MethodProcess):
        _start_method = 'fork'
        _popen_module = 'popen_fork'

    class SpawnProcess(_MethodProcess):
        _start_method = 'spawn'
        _popen_module = 'popen_spawn_posix'

        @staticmethod
        def _after_fork():
            # A spawned child is a new interpreter: no inherited state to
            # reset.
            pass

    class ForkServerProcess(_MethodProcess):
        _start_method = 'forkserver'
        _popen_module = 'popen_forkserver'

    class ForkContext(BaseContext):
        _name = 'fork'
        Process = ForkProcess

    class SpawnContext(BaseContext):
        _name = 'spawn'
        Process = SpawnProcess

    class ForkServerContext(BaseContext):
        _name = 'forkserver'
        Process = ForkServerProcess

        def _check_available(self):
            if not reduction.HAVE_SEND_HANDLE:
                raise ValueError('forkserver start method not available')

    _concrete_contexts = {ctx._name: ctx for ctx in
                          (ForkContext(), SpawnContext(), ForkServerContext())}

else:

    class SpawnProcess(_MethodProcess):
        _start_method = 'spawn'
        _popen_module = 'popen_spawn_win32'

        @staticmethod
        def _after_fork():
            pass

    class SpawnContext(BaseContext):
        _name = 'spawn'
        Process = SpawnProcess

    _concrete_contexts = {'spawn': SpawnContext()}


# -- choosing the default start method ---------------------------------------

def _can_create_temp_files():
    # The fork server listens on a socket in the temporary directory; only
    # make it the default when files can actually be created there.
    import tempfile
    try:
        fd, path = tempfile.mkstemp(prefix='pymp-probe-',
                                    dir=tempfile.gettempdir())
    except OSError:
        return False
    os.close(fd)
    try:
        os.unlink(path)
    except OSError:
        pass
    return True


def _default_method_name():
    if sys.platform in ('win32', 'darwin'):
        return 'spawn'
    if reduction.HAVE_SEND_HANDLE and _can_create_temp_files():
        return 'forkserver'
    return 'spawn'


_default_context = DefaultContext(None)


# -- the child currently being started, per thread ---------------------------
#
# Some objects (semaphores, shared ctypes, ...) can only travel to a child
# as part of starting it.  Their reducers check that a Popen object is
# registered here.

_spawning = threading.local()


def get_spawning_popen():
    return getattr(_spawning, 'popen', None)


def set_spawning_popen(popen):
    _spawning.popen = popen


def assert_spawning(obj):
    if get_spawning_popen() is None:
        raise RuntimeError(
            '%s objects should only be shared between processes'
            ' through inheritance' % type(obj).__name__)


def _force_start_method(method):
    # Used in a child so that it keeps its parent's start method.
    _default_context._actual_context = _concrete_contexts[method]
