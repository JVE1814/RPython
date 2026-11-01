#
# multiprocessing.resource_tracker: a helper process that removes named
# resources (POSIX semaphores, shared memory segments) which processes
# created but did not get to remove because they died.
#
# Processes write REGISTER / UNREGISTER lines to a pipe shared by the whole
# process tree.  When every write end is closed the tracker cleans up what
# is still registered, warning about each leaked resource.
#
# Line formats:
#   simple:  "CMD:name:rtype\n"      (ASCII; names must not contain '\n')
#   json:    '{"cmd": .., "name": .., "rtype": ..}\n'
# The tracker accepts both; clients use the json form unless
# _use_simple_format is set (and always for names the simple form cannot
# carry).
#

import os
import signal
import sys
import threading
import warnings

from . import spawn
from . import util

__all__ = ['ensure_running', 'register', 'unregister']

_HAVE_SIGMASK = hasattr(signal, 'pthread_sigmask')
_IGNORED_SIGNALS = (signal.SIGINT, signal.SIGTERM)

# Writes up to PIPE_BUF bytes to a pipe are atomic; longer messages could
# interleave with other processes' messages.
_MAX_MESSAGE = 512


def _noop(name):
    pass


_CLEANUP_FUNCS = {
    'noop': _noop,
    # used by tests
    'dummy': _noop,
}

if os.name == 'posix':
    import _multiprocessing
    import _posixshmem

    if hasattr(_multiprocessing, 'sem_unlink'):
        _CLEANUP_FUNCS['semaphore'] = _multiprocessing.sem_unlink
    _CLEANUP_FUNCS['shared_memory'] = _posixshmem.shm_unlink


class ReentrantCallError(RuntimeError):
    pass


def _encode(cmd, name, rtype, simple):
    if simple and '\n' not in name and name.isascii():
        return f'{cmd}:{name}:{rtype}\n'.encode('ascii')
    import json
    return (json.dumps({'cmd': cmd, 'name': name, 'rtype': rtype},
                       ensure_ascii=True) + '\n').encode('ascii')


def _decode(line):
    text = line.decode('ascii').rstrip('\n')
    if text.startswith('{'):
        import json
        d = json.loads(text)
        return d['cmd'], d['name'], d['rtype']
    # names may contain colons; command and type never do
    cmd, rest = text.split(':', 1)
    name, rtype = rest.rsplit(':', 1)
    return cmd, name, rtype


# Set (per thread) by multiprocessing's fork start method: a child created
# that way keeps using the parent's tracker pipe.
_fork_state = threading.local()


class ResourceTracker(object):

    def __init__(self):
        self._lock = threading.RLock()
        self._fd = None
        self._pid = None
        self._exitcode = None
        self._waitpid_timed_out = False
        self._use_simple_format = False

    def _reentrant_call_error(self):
        raise ReentrantCallError(
            "Reentrant call into the multiprocessing resource tracker")

    def __del__(self):
        try:
            self._stop(use_blocking_lock=False)
        except Exception:
            pass

    def _stop(self, use_blocking_lock=True, wait_timeout=1.0):
        """Close our end of the pipe and reap the tracker if we started it.

        The tracker exits once every process holding the pipe closed it; a
        forked child may keep it open, so the wait is bounded."""
        if use_blocking_lock:
            self._lock.acquire()
        elif not self._lock.acquire(blocking=False):
            return
        try:
            if self._fd is None:
                return
            os.close(self._fd)
            self._fd = None
            if self._pid is None:
                return
            pid = self._pid
            import time
            deadline = time.monotonic() + wait_timeout
            delay = 0.0005
            while True:
                try:
                    reaped, status = os.waitpid(pid, os.WNOHANG)
                except ChildProcessError:
                    self._pid = None
                    return
                if reaped:
                    self._exitcode = os.waitstatus_to_exitcode(status)
                    self._pid = None
                    return
                if time.monotonic() >= deadline:
                    # Somebody else still holds the pipe: give up waiting.
                    self._waitpid_timed_out = True
                    self._pid = None
                    return
                time.sleep(delay)
                delay = min(delay * 2, 0.05)
        finally:
            self._lock.release()

    def getfd(self):
        self.ensure_running()
        return self._fd

    def ensure_running(self):
        """Start the tracker process if it is not running (relaunching it,
        with a warning, if it died)."""
        with self._lock:
            if self._lock._recursion_count() > 1:
                return self._reentrant_call_error()
            if self._fd is not None:
                if self._check_alive():
                    return
                # the tracker died: start a new one
                os.close(self._fd)
                if self._pid is not None:
                    try:
                        os.waitpid(self._pid, 0)
                    except ChildProcessError:
                        pass
                self._fd = None
                self._pid = None
                self._exitcode = None
                warnings.warn('resource_tracker: process died unexpectedly, '
                              'relaunching.  Some resources might leak.')
            self._launch()

    def _launch(self):
        fds_to_pass = []
        try:
            fds_to_pass.append(sys.stderr.fileno())
        except Exception:
            pass
        r, w = os.pipe()
        try:
            fds_to_pass.append(r)
            exe = spawn.get_executable()
            args = [exe] + util._args_from_interpreter_flags()
            args += ['-c',
                     f'from multiprocessing.resource_tracker import main;main({r})']
            old_mask = None
            if _HAVE_SIGMASK:
                # the tracker must not die of a Ctrl-C meant for the parent;
                # it unblocks the signals after ignoring them
                old_mask = signal.pthread_sigmask(signal.SIG_BLOCK,
                                                  _IGNORED_SIGNALS)
            try:
                pid = util.spawnv_passfds(exe, args, fds_to_pass)
            finally:
                if old_mask is not None:
                    signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
        except:
            os.close(w)
            raise
        else:
            self._fd = w
            self._pid = pid
            self._exitcode = None
            self._waitpid_timed_out = False
        finally:
            os.close(r)

    def _check_alive(self):
        """True if the tracker still reads the pipe."""
        try:
            self._write(_encode('PROBE', '0', 'noop', self._use_simple_format))
        except OSError:
            return False
        return True

    def register(self, name, rtype):
        """Track *name* for cleanup."""
        self._send('REGISTER', name, rtype)

    def unregister(self, name, rtype):
        """Stop tracking *name*."""
        self._send('UNREGISTER', name, rtype)

    def _write(self, msg):
        nbytes = os.write(self._fd, msg)
        assert nbytes == len(msg), f"{nbytes=} != {len(msg)=}"

    def _send(self, cmd, name, rtype):
        msg = _encode(cmd, name, rtype, self._use_simple_format)
        if len(msg) > _MAX_MESSAGE:
            raise ValueError('msg too long')
        self.ensure_running()
        self._write(msg)

    def _after_fork_in_child(self):
        self._lock._at_fork_reinit()
        # The child does not own the tracker process.
        self._pid = None
        self._exitcode = None
        self._waitpid_timed_out = False
        if self._fd is not None and not getattr(_fork_state, 'keep_fd', False):
            # A plain fork(): drop our copy of the pipe so that the parent
            # alone decides when the tracker ends.
            try:
                os.close(self._fd)
            except OSError:
                pass
            self._fd = None


_resource_tracker = ResourceTracker()
ensure_running = _resource_tracker.ensure_running
register = _resource_tracker.register
unregister = _resource_tracker.unregister
getfd = _resource_tracker.getfd

if hasattr(os, 'register_at_fork'):
    os.register_at_fork(after_in_child=_resource_tracker._after_fork_in_child)


def main(fd):
    """Run the tracker: read commands from *fd* until EOF, then clean up."""
    # Ctrl-C and SIGTERM are for the user's processes, not for us.
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    if _HAVE_SIGMASK:
        signal.pthread_sigmask(signal.SIG_UNBLOCK, _IGNORED_SIGNALS)

    for f in (sys.stdin, sys.stdout):
        try:
            f.close()
        except Exception:
            pass

    cache = {rtype: set() for rtype in _CLEANUP_FUNCS}
    exit_code = 0
    try:
        with open(fd, 'rb') as f:
            for line in f:
                try:
                    cmd, name, rtype = _decode(line)
                    if _CLEANUP_FUNCS.get(rtype) is None:
                        raise ValueError(
                            f'Cannot register {name} for automatic cleanup: '
                            f'unknown resource type {rtype}')
                    if cmd == 'REGISTER':
                        cache[rtype].add(name)
                    elif cmd == 'UNREGISTER':
                        cache[rtype].discard(name)
                    elif cmd == 'PROBE':
                        pass
                    else:
                        raise RuntimeError('unrecognized command %r' % cmd)
                except Exception:
                    exit_code = 3
                    try:
                        sys.excepthook(*sys.exc_info())
                    except:
                        pass
    finally:
        for rtype, names in cache.items():
            if not names:
                continue
            exit_code = max(exit_code, 1)
            if rtype in ('noop', 'dummy'):
                continue
            try:
                warnings.warn(
                    f'resource_tracker: There appear to be {len(names)} '
                    f'leaked {rtype} objects to clean up at shutdown: '
                    f'{sorted(names)}')
            except Exception:
                pass
            for name in names:
                try:
                    _CLEANUP_FUNCS[rtype](name)
                except Exception as e:
                    exit_code = 2
                    try:
                        warnings.warn('resource_tracker: %r: %s' % (name, e))
                    except Exception:
                        pass
    try:
        sys.stderr.flush()
    except Exception:
        pass
    os._exit(exit_code)
