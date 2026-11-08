#
# multiprocessing.util -- the package's shared plumbing: a lazily created
# logger, object/exit finalizers, hooks that run in forked children, a
# per-process scratch directory and a few file-descriptor helpers.
#

import atexit
import itertools
import os
import sys
import threading
import weakref

from . import process

__all__ = [
    'sub_debug', 'debug', 'info', 'sub_warning', 'warn', 'get_logger',
    'log_to_stderr', 'get_temp_dir', 'register_after_fork',
    'is_exiting', 'Finalize', 'ForkAwareThreadLock', 'ForkAwareLocal',
    'close_all_fds_except', 'SUBDEBUG', 'SUBWARNING',
]


# --- logging ---------------------------------------------------------------
#
# Nothing is logged until somebody asks for the logger: until then every
# helper below is a cheap no-op and the logging package is not imported.

NOTSET = 0
SUBDEBUG = 5
DEBUG = 10
INFO = 20
SUBWARNING = 25
WARNING = 30

LOGGER_NAME = 'multiprocessing'
DEFAULT_LOGGING_FORMAT = '[%(levelname)s/%(processName)s] %(message)s'

_logger = None
_log_to_stderr = False


def _level_logger(level, name):
    # stacklevel=2 attributes the record to whoever called the helper.
    def emit(msg, *args):
        logger = _logger
        if logger:
            logger.log(level, msg, *args, stacklevel=2)
    emit.__name__ = emit.__qualname__ = name
    return emit


sub_debug = _level_logger(SUBDEBUG, 'sub_debug')
debug = _level_logger(DEBUG, 'debug')
info = _level_logger(INFO, 'info')
sub_warning = _level_logger(SUBWARNING, 'sub_warning')
warn = _level_logger(WARNING, 'warn')


def get_logger():
    """Return the 'multiprocessing' logger, creating it on first use."""
    global _logger
    import logging
    with logging._lock:
        if not _logger:
            created = logging.getLogger(LOGGER_NAME)
            created.propagate = 0
            _logger = created
            # logging closes its handlers from an atexit hook; atexit runs
            # hooks newest first, so registering ours again now lets the
            # shutdown messages still reach those handlers.
            atexit.unregister(_exit_function)
            atexit.register(_exit_function)
    return _logger


def log_to_stderr(level=None):
    """Attach a stderr handler to the multiprocessing logger."""
    global _log_to_stderr
    import logging
    logger = get_logger()
    handler = logging.StreamHandler()
    handler.setFormatter(logging.Formatter(DEFAULT_LOGGING_FORMAT))
    logger.addHandler(handler)
    if level:
        logger.setLevel(level)
    _log_to_stderr = True
    return logger


# --- abstract unix sockets -------------------------------------------------

abstract_sockets_supported = sys.platform in ('linux', 'android')


def is_abstract_socket_namespace(address):
    """True for an AF_UNIX address in the Linux abstract namespace."""
    if not address:
        return False
    if isinstance(address, bytes):
        return address[:1] == b'\0'
    if isinstance(address, str):
        return address[:1] == '\0'
    raise TypeError(f'address type of {address!r} unrecognized')


# --- the per-process temporary directory -----------------------------------

# Longest AF_UNIX path (including the terminating NUL) the platform accepts.
_SUN_PATH_MAX = 104 if sys.platform == 'darwin' else 108

# What get_temp_dir() and connection.arbitrary_address() add below the base
# directory: "/pymp-" + 8 chars, then "/listener-" + 8 chars, then a NUL.
_SOCKET_SUFFIX_ROOM = len('/pymp-') + 8 + len('/listener-') + 8 + 1


def _choose_temp_base(tempfile):
    preferred = tempfile.gettempdir()
    if len(os.fsencode(preferred)) + _SOCKET_SUFFIX_ROOM <= _SUN_PATH_MAX:
        return preferred
    # The configured directory is too deep for socket names; fall back to a
    # short system directory when one is writable.
    for fallback in ('/tmp', '/var/tmp', '/usr/tmp'):
        try:
            usable = os.path.isdir(fallback) and os.access(fallback, os.W_OK)
        except OSError:
            usable = False
        if usable:
            return fallback
    return preferred


def _drop_temp_dir(rmtree, path):
    # Called from a finalizer, possibly late at exit: everything it needs
    # comes in through its arguments.
    rmtree(path)
    me = process.current_process()
    if me is not None:
        me._config['tempdir'] = None


def get_temp_dir():
    """Return a private scratch directory, made on first use and removed
    when the process exits."""
    config = process.current_process()._config
    path = config.get('tempdir')
    if path is None:
        import shutil
        import tempfile
        path = tempfile.mkdtemp(prefix='pymp-', dir=_choose_temp_base(tempfile))
        info('created temp directory %s', path)
        Finalize(None, _drop_temp_dir, args=(shutil.rmtree, path),
                 exitpriority=-100)
        config['tempdir'] = path
    return path


# --- hooks run in a child created by fork() --------------------------------
#
# Keys are (registration number, id(obj), func) so that the hooks run in
# registration order; values are held weakly, a dead object drops its hook.

_afterfork_registry = weakref.WeakValueDictionary()
_afterfork_counter = itertools.count()


def register_after_fork(obj, func):
    """Arrange for func(obj) to be called in children made by fork()."""
    _afterfork_registry[(next(_afterfork_counter), id(obj), func)] = obj


def _run_after_forkers():
    for key, obj in sorted(_afterfork_registry.items()):
        hook = key[2]
        try:
            hook(obj)
        except Exception as exc:
            info('after forker raised exception %s', exc)


# --- finalizers ------------------------------------------------------------
#
# A Finalize object is registered under (exitpriority, serial).  It fires
# when its object is collected, when it is called directly, or -- if it has
# an exit priority -- from _exit_function().  Firing removes the entry, so
# each callback runs at most once.

_finalizer_registry = {}
_finalizer_counter = itertools.count()


class Finalize(object):
    """Run callback(*args, **kwargs) once, when *obj* dies or at exit."""

    def __init__(self, obj, callback, args=(), kwargs=None, exitpriority=None):
        if exitpriority is not None and not isinstance(exitpriority, int):
            raise TypeError(
                f"Exitpriority ({exitpriority!r}) must be None or int, "
                f"not {type(exitpriority)!s}")
        if obj is None:
            if exitpriority is None:
                raise ValueError("Without object, exitpriority cannot be None")
        else:
            self._weakref = weakref.ref(obj, self)
        self._callback = callback
        self._args = args
        self._kwargs = kwargs or {}
        self._key = (exitpriority, next(_finalizer_counter))
        self._pid = os.getpid()
        _finalizer_registry[self._key] = self

    def _forget(self):
        self._weakref = self._callback = None
        self._args = self._kwargs = self._key = None

    # The defaults keep what the call needs reachable even after the module
    # dict has been cleared during interpreter shutdown.
    def __call__(self, wr=None, _finalizer_registry=_finalizer_registry,
                 sub_debug=sub_debug, getpid=os.getpid):
        """Fire the finalizer if it has not fired yet; return the result."""
        if _finalizer_registry.pop(self._key, None) is None:
            sub_debug('finalizer no longer registered')
            return None
        result = None
        if self._pid == getpid():
            sub_debug('finalizer calling %s with args %s and kwargs %s',
                      self._callback, self._args, self._kwargs)
            result = self._callback(*self._args, **self._kwargs)
        else:
            # Inherited through fork(): the resource belongs to the parent.
            sub_debug('finalizer ignored because different process')
        self._forget()
        return result

    def cancel(self):
        """Unregister without running the callback."""
        if _finalizer_registry.pop(self._key, None) is not None:
            self._forget()

    def still_active(self):
        """Whether the callback is still pending."""
        return self._key in _finalizer_registry

    def __repr__(self):
        cls = type(self).__name__
        ref = getattr(self, '_weakref', None)
        target = ref() if ref is not None else None
        if target is None:
            return f'<{cls} object, dead>'
        parts = [f'callback={getattr(self._callback, "__name__", self._callback)}']
        if self._args:
            parts.append(f'args={self._args}')
        if self._kwargs:
            parts.append(f'kwargs={self._kwargs}')
        if self._key[0] is not None:
            parts.append(f'exitpriority={self._key[0]}')
        return f'<{cls} object, ' + ', '.join(parts) + '>'


def _run_finalizers(minpriority=None):
    """Fire the finalizers that have an exit priority >= *minpriority*
    (any exit priority when it is None), highest priority and newest first.
    With no *minpriority* the registry is emptied afterwards."""
    registry = _finalizer_registry
    if registry is None:
        return
    # Work on a snapshot: callbacks, and garbage collection in other threads,
    # add and remove entries while we run.
    if minpriority is None:
        due = [k for k in list(registry) if k[0] is not None]
    else:
        due = [k for k in list(registry)
               if k[0] is not None and k[0] >= minpriority]
    due.sort(reverse=True)
    for key in due:
        finalizer = registry.get(key)
        if finalizer is None:
            continue
        sub_debug('calling %s', finalizer)
        try:
            finalizer()
        except Exception:
            import traceback
            traceback.print_exc()
    if minpriority is None:
        registry.clear()


# --- process exit ----------------------------------------------------------

_exiting = False


def is_exiting():
    """True once the process has started shutting down."""
    return _exiting or _exiting is None


def _exit_function(info=info, debug=debug, _run_finalizers=_run_finalizers,
                   active_children=process.active_children,
                   current_process=process.current_process):
    # Runs from atexit (and at the end of a child's bootstrap).  Everything
    # is passed in as a default: module globals may already be gone.
    global _exiting
    if _exiting:
        return
    _exiting = True
    info('process shutting down')
    debug('running all "atexit" finalizers with priority >= 0')
    _run_finalizers(0)
    if current_process() is not None:
        # Daemonic children are killed off, the rest are waited for.
        children = active_children()
        for child in children:
            if child.daemon:
                info('calling terminate() for daemon %s', child.name)
                child._popen.terminate()
        for child in active_children():
            info('calling join() for process %s', child.name)
            child.join()
    debug('running the remaining "atexit" finalizers')
    _run_finalizers()


atexit.register(_exit_function)


# --- locks and thread-locals that reset in forked children -----------------

class ForkAwareThreadLock(object):
    """A lock that a forked child gets back in the released state."""

    def __init__(self):
        lock = threading.Lock()
        self._lock = lock
        self.acquire = lock.acquire
        self.release = lock.release
        register_after_fork(self, ForkAwareThreadLock._at_fork_reinit)

    def _at_fork_reinit(self):
        self._lock._at_fork_reinit()

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *exc_info):
        return self._lock.__exit__(*exc_info)


def _clear_local(local):
    local.__dict__.clear()


class ForkAwareLocal(threading.local):
    """A threading.local that a forked child sees empty."""

    def __init__(self):
        register_after_fork(self, _clear_local)

    def __reduce__(self):
        return type(self), ()


# --- file descriptors ------------------------------------------------------

try:
    MAXFD = os.sysconf("SC_OPEN_MAX")
except Exception:
    MAXFD = 256


def close_all_fds_except(fds):
    """Close every descriptor below MAXFD that is not listed in *fds*."""
    keep = sorted(set(fds))
    if keep and keep[-1] > MAXFD:
        raise AssertionError('fd too large')
    low = 0
    for fd in keep:
        if fd >= low:
            os.closerange(low, fd)
            low = fd + 1
    os.closerange(low, MAXFD)


def _close_stdin():
    # A child must not share the parent's stdin; give it /dev/null instead.
    if sys.stdin is None:
        return
    try:
        sys.stdin.close()
    except (OSError, ValueError):
        pass
    try:
        fd = os.open(os.devnull, os.O_RDONLY)
    except (OSError, ValueError):
        return
    try:
        sys.stdin = open(fd, encoding="utf-8", closefd=False)
    except (OSError, ValueError):
        os.close(fd)
    except BaseException:
        os.close(fd)
        raise


def _flush_std_streams():
    for name in ('stdout', 'stderr'):
        stream = getattr(sys, name, None)
        try:
            stream.flush()
        except (AttributeError, ValueError):
            pass


def spawnv_passfds(path, args, passfds):
    """Run *path* with argument list *args* in a child that inherits only
    the descriptors in *passfds*; return the child's pid."""
    import _posixsubprocess
    keep = tuple(sorted(int(fd) for fd in passfds))
    report_r, report_w = os.pipe()
    try:
        return _posixsubprocess.fork_exec(
            args, [path], True, keep, None, None,
            -1, -1, -1, -1, -1, -1,          # no stdio redirection
            report_r, report_w,
            False, False, -1,                # signals, setsid, process group
            None, None, None, -1, None)      # gid, groups, uid, umask, preexec
    finally:
        os.close(report_r)
        os.close(report_w)


def close_fds(*fds):
    """Close each of the given file descriptors."""
    for fd in fds:
        os.close(fd)


def _cleanup_tests():
    """Release what multiprocessing left behind after a test run: the fork
    server, the resource tracker, scratch directories, zombie children."""
    from test import support
    from multiprocessing import forkserver, resource_tracker
    forkserver._forkserver._stop()
    resource_tracker._resource_tracker._stop()
    _run_finalizers()
    support.gc_collect()
    support.reap_children()


def _args_from_interpreter_flags():
    # Imported here so that importing multiprocessing stays cheap.
    import subprocess
    return subprocess._args_from_interpreter_flags()
