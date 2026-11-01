#
# multiprocessing.util: logging, finalizers, after-fork hooks and helpers
# shared by the rest of the package.
#

import os
import itertools
import sys
import weakref
import atexit
import threading

from . import process

__all__ = [
    'sub_debug', 'debug', 'info', 'sub_warning', 'warn', 'get_logger',
    'log_to_stderr', 'get_temp_dir', 'register_after_fork',
    'is_exiting', 'Finalize', 'ForkAwareThreadLock', 'ForkAwareLocal',
    'close_all_fds_except', 'SUBDEBUG', 'SUBWARNING',
]

# ---------------------------------------------------------------- logging

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


def _log(level, msg, args):
    if _logger:
        _logger.log(level, msg, *args, stacklevel=3)


def sub_debug(msg, *args):
    _log(SUBDEBUG, msg, args)


def debug(msg, *args):
    _log(DEBUG, msg, args)


def info(msg, *args):
    _log(INFO, msg, args)


def warn(msg, *args):
    _log(WARNING, msg, args)


def sub_warning(msg, *args):
    _log(SUBWARNING, msg, args)


def get_logger():
    """Return the 'multiprocessing' logger (created on first use)."""
    global _logger
    import logging

    with logging._lock:
        if not _logger:
            _logger = logging.getLogger(LOGGER_NAME)
            _logger.propagate = 0

            # Make sure our exit hook runs before logging's own, which
            # closes the handlers.
            if hasattr(atexit, 'unregister'):
                atexit.unregister(_exit_function)
                atexit.register(_exit_function)
            else:
                atexit._exithandlers.remove((_exit_function, (), {}))
                atexit._exithandlers.append((_exit_function, (), {}))
    return _logger


def log_to_stderr(level=None):
    """Make the multiprocessing logger print to stderr."""
    global _log_to_stderr
    import logging

    logger = get_logger()
    formatter = logging.Formatter(DEFAULT_LOGGING_FORMAT)
    handler = logging.StreamHandler()
    handler.setFormatter(formatter)
    logger.addHandler(handler)

    if level:
        logger.setLevel(level)
    _log_to_stderr = True
    return _logger


# ---------------------------------------------------------------- sockets

def _platform_supports_abstract_sockets():
    return sys.platform in ("linux", "android")


def is_abstract_socket_namespace(address):
    if not address:
        return False
    if isinstance(address, bytes):
        return address[0] == 0
    elif isinstance(address, str):
        return address[0] == "\0"
    raise TypeError(f'address type of {address!r} unrecognized')


abstract_sockets_supported = _platform_supports_abstract_sockets()

# ---------------------------------------------------------------- temp dir

# Unix socket paths are limited (108 bytes on Linux): keep the directory
# short so that "<tempdir>/pymp-XXXXXXXX/listener-XXXXXXXX" fits.
_SUN_PATH_MAX = 104 if sys.platform == 'darwin' else 108


def _remove_temp_dir(rmtree, tempdir):
    rmtree(tempdir)
    current = process.current_process()
    if current is not None:
        current._config['tempdir'] = None


def _get_base_temp_dir(tempfile):
    base = tempfile.gettempdir()
    # room for "/pymp-XXXXXXXX" + "/listener-XXXXXXXX" + NUL
    needed = 14 + 18 + 1
    if len(os.fsencode(base)) + needed <= _SUN_PATH_MAX:
        return base
    for candidate in ('/tmp', '/var/tmp', '/usr/tmp'):
        try:
            if os.path.isdir(candidate) and os.access(candidate, os.W_OK):
                return candidate
        except OSError:
            pass
    return base


def get_temp_dir():
    """Return this process's private temporary directory (created on first
    use and removed at exit)."""
    tempdir = process.current_process()._config.get('tempdir')
    if tempdir is None:
        import shutil
        import tempfile
        base = _get_base_temp_dir(tempfile)
        tempdir = tempfile.mkdtemp(prefix='pymp-', dir=base)
        info('created temp directory %s', tempdir)
        # The finalizer must not depend on module globals still existing.
        Finalize(None, _remove_temp_dir, args=(shutil.rmtree, tempdir),
                 exitpriority=-100)
        process.current_process()._config['tempdir'] = tempdir
    return tempdir


# ---------------------------------------------------------------- after fork

_afterfork_registry = weakref.WeakValueDictionary()
_afterfork_counter = itertools.count()


def _run_after_forkers():
    items = list(_afterfork_registry.items())
    items.sort()
    for (index, ident, func), obj in items:
        try:
            func(obj)
        except Exception as e:
            info('after forker raised exception %s', e)


def register_after_fork(obj, func):
    """Call func(obj) in a child process created by fork()."""
    _afterfork_registry[(next(_afterfork_counter), id(obj), func)] = obj


# ---------------------------------------------------------------- finalizers

_finalizer_registry = {}
_finalizer_counter = itertools.count()


class Finalize(object):
    """Call a callback when an object dies or when the process exits.

    Finalizers with an *exitpriority* (not None) also run at exit, highest
    priority first and, for equal priorities, newest first."""

    def __init__(self, obj, callback, args=(), kwargs=None, exitpriority=None):
        if (exitpriority is not None) and not isinstance(exitpriority, int):
            raise TypeError(
                "Exitpriority ({0!r}) must be None or int, not {1!s}".format(
                    exitpriority, type(exitpriority)))

        if obj is not None:
            self._weakref = weakref.ref(obj, self)
        elif exitpriority is None:
            raise ValueError("Without object, exitpriority cannot be None")

        self._callback = callback
        self._args = args
        self._kwargs = kwargs or {}
        self._key = (exitpriority, next(_finalizer_counter))
        self._pid = os.getpid()

        _finalizer_registry[self._key] = self

    def __call__(self, wr=None,
                 # bound as defaults: module globals may be gone at exit
                 _finalizer_registry=_finalizer_registry,
                 sub_debug=sub_debug, getpid=os.getpid):
        """Run the callback (at most once).  Returns its result."""
        try:
            del _finalizer_registry[self._key]
        except KeyError:
            sub_debug('finalizer no longer registered')
            return None
        if self._pid != getpid():
            sub_debug('finalizer ignored because different process')
            res = None
        else:
            sub_debug('finalizer calling %s with args %s and kwargs %s',
                      self._callback, self._args, self._kwargs)
            res = self._callback(*self._args, **self._kwargs)
        self._weakref = self._callback = self._args = \
                        self._kwargs = self._key = None
        return res

    def cancel(self):
        """Forget the finalizer without running it."""
        try:
            del _finalizer_registry[self._key]
        except KeyError:
            pass
        else:
            self._weakref = self._callback = self._args = \
                            self._kwargs = self._key = None

    def still_active(self):
        """True if the callback has not run (nor been cancelled) yet."""
        return self._key in _finalizer_registry

    def __repr__(self):
        try:
            obj = self._weakref()
        except (AttributeError, TypeError):
            obj = None

        if obj is None:
            return '<%s object, dead>' % self.__class__.__name__

        x = '<%s object, callback=%s' % (
                self.__class__.__name__,
                getattr(self._callback, '__name__', self._callback))
        if self._args:
            x += ', args=' + str(self._args)
        if self._kwargs:
            x += ', kwargs=' + str(self._kwargs)
        if self._key[0] is not None:
            x += ', exitpriority=' + str(self._key[0])
        return x + '>'


def _run_finalizers(minpriority=None):
    """Run the registered finalizers whose priority is at least
    *minpriority* (all with a priority, if None), highest first."""
    if _finalizer_registry is None:
        # at shutdown the module may already be torn down
        return

    if minpriority is None:
        def wanted(prio):
            return prio[0] is not None
    else:
        def wanted(prio):
            return prio[0] is not None and prio[0] >= minpriority

    # Snapshot the keys: finalizers (and the GC in other threads) may
    # change the registry while we run them.
    keys = [key for key in list(_finalizer_registry) if wanted(key)]
    keys.sort(reverse=True)

    for key in keys:
        finalizer = _finalizer_registry.get(key)
        if finalizer is not None:
            sub_debug('calling %s', finalizer)
            try:
                finalizer()
            except Exception:
                import traceback
                traceback.print_exc()

    if minpriority is None:
        _finalizer_registry.clear()


def is_exiting():
    """True while the process is shutting down."""
    return _exiting or _exiting is None


_exiting = False


def _exit_function(info=info, debug=debug, _run_finalizers=_run_finalizers,
                   active_children=process.active_children,
                   current_process=process.current_process):
    # Arguments are bound as defaults because module globals may already be
    # cleared when this runs at interpreter exit.
    global _exiting

    if not _exiting:
        _exiting = True

        info('process shutting down')
        debug('running all "atexit" finalizers with priority >= 0')
        _run_finalizers(0)

        if current_process() is not None:
            # Daemonic children are terminated; the others are joined.
            for p in active_children():
                if p.daemon:
                    info('calling terminate() for daemon %s', p.name)
                    p._popen.terminate()

            for p in active_children():
                info('calling join() for process %s', p.name)
                p.join()

        debug('running the remaining "atexit" finalizers')
        _run_finalizers()


atexit.register(_exit_function)


# ---------------------------------------------------------------- fork aware

class ForkAwareThreadLock(object):
    """A threading.Lock that is replaced by a fresh one in forked children."""

    def __init__(self):
        self._lock = threading.Lock()
        self.acquire = self._lock.acquire
        self.release = self._lock.release
        register_after_fork(self, ForkAwareThreadLock._at_fork_reinit)

    def _at_fork_reinit(self):
        self._lock._at_fork_reinit()

    def __enter__(self):
        return self._lock.__enter__()

    def __exit__(self, *args):
        return self._lock.__exit__(*args)


class ForkAwareLocal(threading.local):
    """A threading.local whose contents are dropped in forked children."""

    def __init__(self):
        register_after_fork(self, lambda obj: obj.__dict__.clear())

    def __reduce__(self):
        return type(self), ()


# ---------------------------------------------------------------- fds

try:
    MAXFD = os.sysconf("SC_OPEN_MAX")
except Exception:
    MAXFD = 256


def close_all_fds_except(fds):
    """Close every file descriptor except those in *fds*."""
    fds = sorted(set(list(fds) + [-1, MAXFD]))
    assert fds[-1] == MAXFD, 'fd too large'
    for low, high in zip(fds, fds[1:]):
        os.closerange(low + 1, high)


def _close_stdin():
    if sys.stdin is None:
        return
    try:
        sys.stdin.close()
    except (OSError, ValueError):
        pass
    try:
        fd = os.open(os.devnull, os.O_RDONLY)
        try:
            sys.stdin = open(fd, encoding="utf-8", closefd=False)
        except:
            os.close(fd)
            raise
    except (OSError, ValueError):
        pass


def _flush_std_streams():
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (AttributeError, ValueError):
            pass


def spawnv_passfds(path, args, passfds):
    """Start *path* with argv *args*, keeping only *passfds* open."""
    import _posixsubprocess
    import subprocess
    passfds = tuple(sorted(map(int, passfds)))
    errpipe_read, errpipe_write = os.pipe()
    try:
        return _posixsubprocess.fork_exec(
            args, [path], True, passfds, None, None,
            -1, -1, -1, -1, -1, -1, errpipe_read, errpipe_write,
            False, False, -1, None, None, None, -1, None,
            subprocess._USE_VFORK)
    finally:
        os.close(errpipe_read)
        os.close(errpipe_write)


def close_fds(*fds):
    """Close each file descriptor given in parameters."""
    for fd in fds:
        os.close(fd)


def _cleanup_tests():
    """Cleanup of multiprocessing resources done after running the tests."""
    from test import support

    # cleanup the multiprocessing process started by the forkserver
    from multiprocessing import forkserver
    forkserver._forkserver._stop()

    # stop the resource tracker
    from multiprocessing import resource_tracker
    resource_tracker._resource_tracker._stop()

    # remove the temporary directories made by get_temp_dir()
    _run_finalizers()
    support.gc_collect()

    support.reap_children()


def _args_from_interpreter_flags():
    # subprocess is imported lazily: importing multiprocessing must stay cheap
    import subprocess
    return subprocess._args_from_interpreter_flags()
