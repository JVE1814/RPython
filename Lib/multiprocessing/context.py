#
# multiprocessing.context: start-method contexts.  A context bundles a way of
# starting children (fork / spawn / forkserver) with factory methods for
# the synchronisation primitives, queues, pools, ...
#

import os
import sys
import threading

from . import process
from . import reduction

__all__ = ()

# The module implementing pickling for children (replaceable via the
# context's `reducer` property).
_reducer_module = [reduction]


class ProcessError(Exception):
    pass


class BufferTooShort(ProcessError):
    pass


class TimeoutError(ProcessError):
    pass


class AuthenticationError(ProcessError):
    pass


class BaseContext(object):

    ProcessError = ProcessError
    BufferTooShort = BufferTooShort
    TimeoutError = TimeoutError
    AuthenticationError = AuthenticationError

    current_process = staticmethod(process.current_process)
    parent_process = staticmethod(process.parent_process)
    active_children = staticmethod(process.active_children)

    def cpu_count(self):
        """Number of CPUs in the system."""
        num = os.cpu_count()
        if num is None:
            raise NotImplementedError('cannot determine number of cpus')
        return num

    def Manager(self):
        """Start a server process and return a SyncManager connected to it."""
        from .managers import SyncManager
        m = SyncManager(ctx=self.get_context())
        m.start()
        return m

    def Pipe(self, duplex=True):
        """Return two Connection objects joined by a pipe."""
        from .connection import Pipe
        return Pipe(duplex)

    def Lock(self):
        from .synchronize import Lock
        return Lock(ctx=self.get_context())

    def RLock(self):
        from .synchronize import RLock
        return RLock(ctx=self.get_context())

    def Condition(self, lock=None):
        from .synchronize import Condition
        return Condition(lock, ctx=self.get_context())

    def Semaphore(self, value=1):
        from .synchronize import Semaphore
        return Semaphore(value, ctx=self.get_context())

    def BoundedSemaphore(self, value=1):
        from .synchronize import BoundedSemaphore
        return BoundedSemaphore(value, ctx=self.get_context())

    def Event(self):
        from .synchronize import Event
        return Event(ctx=self.get_context())

    def Barrier(self, parties, action=None, timeout=None):
        from .synchronize import Barrier
        return Barrier(parties, action, timeout, ctx=self.get_context())

    def Queue(self, maxsize=0):
        from .queues import Queue
        return Queue(maxsize, ctx=self.get_context())

    def JoinableQueue(self, maxsize=0):
        from .queues import JoinableQueue
        return JoinableQueue(maxsize, ctx=self.get_context())

    def SimpleQueue(self):
        from .queues import SimpleQueue
        return SimpleQueue(ctx=self.get_context())

    def Pool(self, processes=None, initializer=None, initargs=(),
             maxtasksperchild=None):
        from .pool import Pool
        return Pool(processes, initializer, initargs, maxtasksperchild,
                    context=self.get_context())

    def RawValue(self, typecode_or_type, *args):
        from .sharedctypes import RawValue
        return RawValue(typecode_or_type, *args)

    def RawArray(self, typecode_or_type, size_or_initializer):
        from .sharedctypes import RawArray
        return RawArray(typecode_or_type, size_or_initializer)

    def Value(self, typecode_or_type, *args, lock=True):
        from .sharedctypes import Value
        return Value(typecode_or_type, *args, lock=lock,
                     ctx=self.get_context())

    def Array(self, typecode_or_type, size_or_initializer, *, lock=True):
        from .sharedctypes import Array
        return Array(typecode_or_type, size_or_initializer, lock=lock,
                     ctx=self.get_context())

    def freeze_support(self):
        """Support for frozen Windows executables (no-op elsewhere)."""
        if sys.platform == 'win32' and getattr(sys, 'frozen', False):
            from .spawn import freeze_support
            freeze_support()

    def get_logger(self):
        """Return the multiprocessing logger."""
        from .util import get_logger
        return get_logger()

    def log_to_stderr(self, level=None):
        """Log multiprocessing activity to stderr."""
        from .util import log_to_stderr
        return log_to_stderr(level)

    def allow_connection_pickling(self):
        """Make Connection objects picklable (registration happens when the
        connection module is imported)."""
        from . import connection  # noqa: F401

    def set_executable(self, executable):
        """Set the interpreter used to start spawned children."""
        from .spawn import set_executable
        set_executable(executable)

    def set_forkserver_preload(self, module_names, *, on_error='ignore'):
        """Modules the fork server imports before forking children."""
        from .forkserver import set_forkserver_preload
        set_forkserver_preload(module_names, on_error=on_error)

    def get_context(self, method=None):
        if method is None:
            return self
        try:
            ctx = _concrete_contexts[method]
        except KeyError:
            raise ValueError('cannot find context for %r' % method) from None
        ctx._check_available()
        return ctx

    def get_start_method(self, allow_none=False):
        return self._name

    def set_start_method(self, method, force=False):
        raise ValueError('cannot set start method of concrete context')

    @property
    def reducer(self):
        """The reduction module used to pickle objects for children."""
        return _reducer_module[0]

    @reducer.setter
    def reducer(self, module):
        _reducer_module[0] = module

    def _check_available(self):
        pass


class Process(process.BaseProcess):
    """Process of the default context (start method resolved at start())."""
    _start_method = None

    @staticmethod
    def _Popen(process_obj):
        return _default_context.get_context().Process._Popen(process_obj)

    @staticmethod
    def _after_fork():
        return _default_context.get_context().Process._after_fork()


class DefaultContext(BaseContext):
    Process = Process

    def __init__(self, context):
        # *context* may be None: the default is then chosen on first use.
        self._default_context = context
        self._actual_context = None

    def _get_default(self):
        if self._default_context is None:
            self._default_context = _concrete_contexts[_choose_default_method()]
        return self._default_context

    def get_context(self, method=None):
        if method is None:
            if self._actual_context is None:
                self._actual_context = self._get_default()
            return self._actual_context
        return super().get_context(method)

    def set_start_method(self, method, force=False):
        if self._actual_context is not None and not force:
            raise RuntimeError('context has already been set')
        if method is None and force:
            self._actual_context = None
            return
        self._actual_context = self.get_context(method)

    def get_start_method(self, allow_none=False):
        if self._actual_context is None:
            if allow_none:
                return None
            self._actual_context = self._get_default()
        return self._actual_context._name

    def get_all_start_methods(self):
        """Supported start methods, the default first."""
        if sys.platform == 'win32':
            return ['spawn']
        default = self._get_default()._name
        methods = ['fork', 'spawn']
        if default != 'spawn' and reduction.HAVE_SEND_HANDLE:
            methods.append('forkserver')
        methods.remove(default)
        return [default] + methods


if sys.platform != 'win32':

    class ForkProcess(process.BaseProcess):
        _start_method = 'fork'

        @staticmethod
        def _Popen(process_obj):
            from .popen_fork import Popen
            return Popen(process_obj)

    class SpawnProcess(process.BaseProcess):
        _start_method = 'spawn'

        @staticmethod
        def _Popen(process_obj):
            from .popen_spawn_posix import Popen
            return Popen(process_obj)

        @staticmethod
        def _after_fork():
            # Spawned children start fresh: nothing to reset.
            pass

    class ForkServerProcess(process.BaseProcess):
        _start_method = 'forkserver'

        @staticmethod
        def _Popen(process_obj):
            from .popen_forkserver import Popen
            return Popen(process_obj)

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

    _concrete_contexts = {
        'fork': ForkContext(),
        'spawn': SpawnContext(),
        'forkserver': ForkServerContext(),
    }

else:

    class SpawnProcess(process.BaseProcess):
        _start_method = 'spawn'

        @staticmethod
        def _Popen(process_obj):
            from .popen_spawn_win32 import Popen
            return Popen(process_obj)

        @staticmethod
        def _after_fork():
            pass

    class SpawnContext(BaseContext):
        _name = 'spawn'
        Process = SpawnProcess

    _concrete_contexts = {
        'spawn': SpawnContext(),
    }


def _tempdir_is_writable():
    # The fork server listens on a Unix socket created in the temporary
    # directory, so it is only a sensible default when that directory works.
    import tempfile
    try:
        base = tempfile.gettempdir()
    except OSError:
        return False
    try:
        fd, path = tempfile.mkstemp(prefix='pymp-probe-', dir=base)
    except OSError:
        return False
    os.close(fd)
    try:
        os.unlink(path)
    except OSError:
        pass
    return True


def _choose_default_method():
    if sys.platform == 'win32' or sys.platform == 'darwin':
        return 'spawn'
    if reduction.HAVE_SEND_HANDLE and _tempdir_is_writable():
        return 'forkserver'
    return 'spawn'


_default_context = DefaultContext(None)

# ---------------------------------------------------------------- spawning state
#
# While a child is being started, the Popen object doing it is recorded per
# thread: objects that may only be passed by inheritance check for it when
# they are pickled.

_tls = threading.local()


def get_spawning_popen():
    return getattr(_tls, 'spawning_popen', None)


def set_spawning_popen(popen):
    _tls.spawning_popen = popen


def assert_spawning(obj):
    if get_spawning_popen() is None:
        raise RuntimeError(
            '%s objects should only be shared between processes'
            ' through inheritance' % type(obj).__name__
            )


def _force_start_method(method):
    _default_context._actual_context = _concrete_contexts[method]
