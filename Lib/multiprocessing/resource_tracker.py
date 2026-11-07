#
# multiprocessing.resource_tracker -- removes named system resources (POSIX
# named semaphores, shared memory segments) that their processes failed to
# remove before dying.
#
# The tracker is a separate process reading a pipe.  The process that
# starts it and every child that inherits the pipe write one line per
# REGISTER or UNREGISTER.  When the last write end is closed the tracker
# reaches EOF, removes whatever is still registered (with a warning per
# kind) and exits; the exit status says whether anything had leaked.
#
# A line is either "CMD:name:rtype" (ASCII, no newline in the name) or a
# JSON object {"cmd": ..., "name": ..., "rtype": ...}.  The tracker reads
# both.  Writers use JSON unless _use_simple_format is set, and fall back to
# JSON for names the short form cannot carry.  Every line must fit in one
# atomic pipe write.
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

# Upper bound for one message: POSIX guarantees atomic pipe writes up to
# PIPE_BUF bytes, which is at least 512.
_MAX_MESSAGE = 512


def _ignore(name):
    pass


# rtype -> function removing a resource of that kind by name.  'noop' is the
# liveness probe's type, 'dummy' exists for the tests.
_CLEANUP_FUNCS = {'noop': _ignore, 'dummy': _ignore}

if os.name == 'posix':
    import _multiprocessing
    import _posixshmem

    if hasattr(_multiprocessing, 'sem_unlink'):
        _CLEANUP_FUNCS['semaphore'] = _multiprocessing.sem_unlink
    _CLEANUP_FUNCS['shared_memory'] = _posixshmem.shm_unlink

# Resource types whose leftovers are expected and not worth a warning.
_SILENT_TYPES = frozenset(('noop', 'dummy'))


class ReentrantCallError(RuntimeError):
    pass


def _encode_message(cmd, name, rtype, short_form):
    if short_form and name.isascii() and '\n' not in name:
        return f'{cmd}:{name}:{rtype}\n'.encode('ascii')
    import json
    text = json.dumps({'cmd': cmd, 'name': name, 'rtype': rtype},
                      ensure_ascii=True)
    return (text + '\n').encode('ascii')


def _decode_message(line):
    text = line.decode('ascii').rstrip('\n')
    if text[:1] == '{':
        import json
        fields = json.loads(text)
        return fields['cmd'], fields['name'], fields['rtype']
    # The name may contain colons; the command and the type cannot.
    cmd, _, rest = text.partition(':')
    name, _, rtype = rest.rpartition(':')
    return cmd, name, rtype


# Per thread: whether a fork() about to happen in this thread is
# multiprocessing's fork start method, whose children keep using our
# tracker.  After any other fork() the child drops its copy of the pipe so
# that the tracker's lifetime stays tied to this process.
_child_keeps_fd = threading.local()


def _keep_tracker_fd_in_child(flag):
    _child_keeps_fd.value = flag


class ResourceTracker(object):

    def __init__(self):
        self._lock = threading.RLock()
        self._fd = None             # our write end of the tracker's pipe
        self._pid = None            # the tracker's pid, if we started it
        self._exitcode = None       # its exit status once reaped
        self._waitpid_timed_out = False
        self._use_simple_format = False
        # Messages produced by a nested call (e.g. a finalizer run by the
        # garbage collector while this thread is starting the tracker);
        # written once the outer call has a pipe.
        self._backlog = []

    def _reentrant_call_error(self):
        raise ReentrantCallError(
            "Reentrant call into the multiprocessing resource tracker")

    def __del__(self):
        try:
            self._stop(use_blocking_lock=False)
        except Exception:
            pass

    # -- the tracker process -------------------------------------------------

    def getfd(self):
        self.ensure_running()
        return self._fd

    def ensure_running(self):
        """Start the tracker unless it is running already.  A tracker that
        died is replaced, with a warning (what it tracked may leak)."""
        with self._lock:
            if self._lock._recursion_count() > 1:
                # The outer call in this thread is taking care of it.
                return
            self._ensure_running_locked()

    def _ensure_running_locked(self):
        if self._fd is not None:
            if self._check_alive():
                return
            self._discard_dead_tracker()
            warnings.warn('resource_tracker: process died unexpectedly, '
                          'relaunching.  Some resources might leak.')
        self._launch()
        self._flush_backlog()

    def _discard_dead_tracker(self):
        os.close(self._fd)
        self._fd = None
        if self._pid is not None:
            try:
                os.waitpid(self._pid, 0)
            except ChildProcessError:
                pass
        self._pid = None
        self._exitcode = None

    def _launch(self):
        passed = []
        try:
            passed.append(sys.stderr.fileno())
        except Exception:
            pass
        read_end, write_end = os.pipe()
        try:
            passed.append(read_end)
            exe = spawn.get_executable()
            argv = [exe] + util._args_from_interpreter_flags() + [
                '-c',
                f'from multiprocessing.resource_tracker import main;main({read_end})']
            # The tracker must survive a Ctrl-C aimed at its parent.  It
            # starts with SIGINT/SIGTERM blocked, ignores them, then
            # unblocks them; our own mask is restored right away.
            saved_mask = None
            if _HAVE_SIGMASK:
                saved_mask = signal.pthread_sigmask(signal.SIG_BLOCK,
                                                    _IGNORED_SIGNALS)
            try:
                pid = util.spawnv_passfds(exe, argv, passed)
            finally:
                if saved_mask is not None:
                    signal.pthread_sigmask(signal.SIG_SETMASK, saved_mask)
        except BaseException:
            os.close(write_end)
            raise
        finally:
            os.close(read_end)
        self._fd = write_end
        self._pid = pid
        self._exitcode = None
        self._waitpid_timed_out = False

    def _check_alive(self):
        """Whether the tracker still reads its pipe."""
        try:
            self._write(_encode_message('PROBE', '0', 'noop',
                                        self._use_simple_format))
        except OSError:
            return False
        return True

    def _stop(self, use_blocking_lock=True, wait_timeout=1.0):
        """Close our end of the pipe and, if we started the tracker, reap it.

        The tracker only exits once every holder of the pipe has closed it,
        and a forked child may still hold it, so the wait is bounded by
        *wait_timeout* seconds (afterwards _waitpid_timed_out is set)."""
        if use_blocking_lock:
            self._lock.acquire()
        elif not self._lock.acquire(blocking=False):
            return
        try:
            if self._fd is None:
                return
            os.close(self._fd)
            self._fd = None
            if self._pid is not None:
                self._reap(self._pid, wait_timeout)
                self._pid = None
        finally:
            self._lock.release()

    def _reap(self, pid, wait_timeout):
        import time
        deadline = time.monotonic() + wait_timeout
        pause = 0.0005
        while True:
            try:
                done, status = os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                return
            if done:
                self._exitcode = os.waitstatus_to_exitcode(status)
                return
            if time.monotonic() >= deadline:
                self._waitpid_timed_out = True
                return
            time.sleep(pause)
            pause = min(pause * 2, 0.05)

    # -- messages ------------------------------------------------------------

    def register(self, name, rtype):
        """Have the tracker remove *name* should nobody else do it."""
        self._send('REGISTER', name, rtype)

    def unregister(self, name, rtype):
        """*name* has been removed (or handed over): stop tracking it."""
        self._send('UNREGISTER', name, rtype)

    def _write(self, msg):
        written = os.write(self._fd, msg)
        assert written == len(msg), f"{written=} != {len(msg)=}"

    def _send(self, cmd, name, rtype):
        msg = _encode_message(cmd, name, rtype, self._use_simple_format)
        if len(msg) > _MAX_MESSAGE:
            raise ValueError('msg too long')
        with self._lock:
            if self._lock._recursion_count() > 1:
                # Nested inside one of our own calls in this thread.
                if self._fd is None:
                    self._backlog.append(msg)
                else:
                    self._write(msg)
                return
            self._ensure_running_locked()
            self._write(msg)

    def _flush_backlog(self):
        pending, self._backlog = self._backlog, []
        for msg in pending:
            self._write(msg)

    # -- fork ----------------------------------------------------------------

    def _after_fork_in_child(self):
        # The child did not start the tracker and must not wait for it.  A
        # lock held by another thread of the parent stays held forever in
        # the child, hence the reinit.
        self._lock._at_fork_reinit()
        self._pid = None
        self._exitcode = None
        self._waitpid_timed_out = False
        self._backlog = []
        if self._fd is not None and not getattr(_child_keeps_fd, 'value', False):
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


# --- the tracker process itself ---------------------------------------------

def _report_bad_line():
    try:
        sys.excepthook(*sys.exc_info())
    except BaseException:
        pass


def _warn_quietly(message):
    try:
        warnings.warn(message)
    except Exception:
        pass


def _remove_leftovers(registered):
    # -> exit status contribution: 0 nothing left, 1 leaks removed,
    #    2 some removal failed.
    status = 0
    for rtype, names in registered.items():
        if not names:
            continue
        status = max(status, 1)
        if rtype in _SILENT_TYPES:
            continue
        _warn_quietly(f'resource_tracker: There appear to be {len(names)} '
                      f'leaked {rtype} objects to clean up at shutdown: '
                      f'{sorted(names)}')
        remove = _CLEANUP_FUNCS[rtype]
        for name in names:
            try:
                remove(name)
            except Exception as exc:
                status = 2
                _warn_quietly('resource_tracker: %r: %s' % (name, exc))
    return status


def main(fd):
    """Body of the tracker process: follow the pipe *fd* until EOF."""
    for signum in _IGNORED_SIGNALS:
        signal.signal(signum, signal.SIG_IGN)
    if _HAVE_SIGMASK:
        signal.pthread_sigmask(signal.SIG_UNBLOCK, _IGNORED_SIGNALS)
    for stream in (sys.stdin, sys.stdout):
        try:
            stream.close()
        except Exception:
            pass

    registered = {rtype: set() for rtype in _CLEANUP_FUNCS}
    bad_input = False
    try:
        with open(fd, 'rb') as pipe:
            for line in pipe:
                try:
                    cmd, name, rtype = _decode_message(line)
                    if _CLEANUP_FUNCS.get(rtype) is None:
                        raise ValueError(
                            f'Cannot register {name} for automatic cleanup: '
                            f'unknown resource type {rtype}')
                    if cmd == 'REGISTER':
                        registered[rtype].add(name)
                    elif cmd == 'UNREGISTER':
                        registered[rtype].discard(name)
                    elif cmd != 'PROBE':
                        raise RuntimeError('unrecognized command %r' % cmd)
                except Exception:
                    bad_input = True
                    _report_bad_line()
    finally:
        status = _remove_leftovers(registered)
        if bad_input:
            status = 3
    try:
        sys.stderr.flush()
    except Exception:
        pass
    os._exit(status)
