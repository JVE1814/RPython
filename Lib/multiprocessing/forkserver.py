#
# multiprocessing.forkserver: a single-threaded server process (started
# fresh, optionally with some modules preloaded) that forks the children.
#
# Protocol for one child: the parent connects to the server's Unix socket,
# both sides authenticate with a per-server random key, and the parent
# sends file descriptors: [child_r, child_w, alive fd, tracker fd, *extra].
# The server forks; it writes the child's pid and later its exit code to
# child_w.  The parent writes the pickled preparation data and Process
# object into the pipe read by the child (child_r).
#

import atexit
import errno
import os
import selectors
import signal
import socket
import struct
import sys
import threading
import warnings

from . import AuthenticationError
from . import connection
from . import process
from .context import reduction
from . import resource_tracker
from . import spawn
from . import util

__all__ = ['ensure_running', 'get_inherited_fds', 'connect_to_new_process',
           'set_forkserver_preload']

MAXFDS_TO_SEND = 256
SIGNED_STRUCT = struct.Struct('q')     # large enough for pid_t
_AUTHKEY_LEN = 32
_ON_ERROR_CHOICES = ('ignore', 'warn', 'fail')


class ForkServer(object):

    def __init__(self):
        self._forkserver_authkey = None
        self._forkserver_address = None
        self._forkserver_alive_fd = None
        self._forkserver_pid = None
        self._inherited_fds = None
        self._lock = threading.Lock()
        self._preload_modules = ['__main__']
        self._preload_on_error = 'ignore'

    def _stop(self):
        with self._lock:
            self._stop_unlocked()

    def _stop_unlocked(self):
        if self._forkserver_pid is None:
            return

        # closing the "alive" pipe makes the server exit
        os.close(self._forkserver_alive_fd)
        self._forkserver_alive_fd = None

        os.waitpid(self._forkserver_pid, 0)
        self._forkserver_pid = None

        if not util.is_abstract_socket_namespace(self._forkserver_address):
            try:
                os.unlink(self._forkserver_address)
            except FileNotFoundError:
                pass
        self._forkserver_address = None
        self._forkserver_authkey = None

    def set_forkserver_preload(self, modules_names, on_error='ignore'):
        """Modules the server imports before it forks any child.

        *on_error* says what a failing import does: 'ignore' it, 'warn'
        (ImportWarning in the server) or 'fail' (the server exits)."""
        if not all(type(mod) is str for mod in modules_names):
            raise TypeError('module_names must be a list of strings')
        if on_error not in _ON_ERROR_CHOICES:
            raise ValueError(
                f"on_error must be 'ignore', 'warn', or 'fail', not {on_error!r}")
        self._preload_modules = modules_names
        self._preload_on_error = on_error

    def get_inherited_fds(self):
        """The extra fds received from the parent (in a forkserver child)."""
        return self._inherited_fds

    def connect_to_new_process(self, fds):
        """Ask the server for a new child; returns (sentinel, write end of
        the pipe the child reads its pickled state from)."""
        self.ensure_running()
        assert self._forkserver_authkey
        if len(fds) + 4 >= MAXFDS_TO_SEND:
            raise ValueError('too many fds')
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(self._forkserver_address)
                parent_r, child_w = os.pipe()
                child_r, parent_w = os.pipe()
                allfds = [child_r, child_w, self._forkserver_alive_fd,
                          resource_tracker.getfd()]
                allfds += fds
                try:
                    client.setblocking(True)
                    wrapped = connection.Connection(client.fileno())
                    try:
                        connection.answer_challenge(wrapped,
                                                    self._forkserver_authkey)
                        connection.deliver_challenge(wrapped,
                                                     self._forkserver_authkey)
                    finally:
                        wrapped._detach()
                        del wrapped
                    reduction.sendfds(client, allfds)
                    return parent_r, parent_w
                except:
                    os.close(parent_r)
                    os.close(parent_w)
                    raise
                finally:
                    os.close(child_r)
                    os.close(child_w)
        except (EOFError, ConnectionError) as exc:
            exc.add_note('Forkserver process may have crashed during '
                         'startup (e.g. a preloaded module failed to import '
                         'with on_error="fail"); check its stderr.')
            raise

    def ensure_running(self):
        """Start the server if it is not running."""
        with self._lock:
            resource_tracker.ensure_running()
            if self._forkserver_pid is not None:
                pid, status = os.waitpid(self._forkserver_pid, os.WNOHANG)
                if not pid:
                    return
                # the server died: start a new one
                os.close(self._forkserver_alive_fd)
                self._forkserver_address = None
                self._forkserver_alive_fd = None
                self._forkserver_authkey = None
                self._forkserver_pid = None

            kwargs = {'on_error': self._preload_on_error}
            if self._preload_modules:
                data = spawn.get_preparation_data('ignore')
                if 'sys_path' in data:
                    kwargs['sys_path'] = data['sys_path']
                if 'init_main_from_path' in data:
                    kwargs['main_path'] = data['init_main_from_path']

            with socket.socket(socket.AF_UNIX) as listener:
                address = connection.arbitrary_address('AF_UNIX')
                listener.bind(address)
                if not util.is_abstract_socket_namespace(address):
                    os.chmod(address, 0o600)
                listener.listen()

                # The server exits when the write end of "alive" closes.
                alive_r, alive_w = os.pipe()
                authkey_r, authkey_w = os.pipe()
                self._forkserver_authkey = os.urandom(_AUTHKEY_LEN)
                kwargs['authkey_r'] = authkey_r
                try:
                    fds_to_pass = [listener.fileno(), alive_r, authkey_r]
                    cmd = ('from multiprocessing.forkserver import main; '
                           'main(%d, %d, %r, **%r)'
                           % (listener.fileno(), alive_r,
                              self._preload_modules, kwargs))
                    exe = spawn.get_executable()
                    args = [exe] + util._args_from_interpreter_flags()
                    args += ['-c', cmd]
                    pid = util.spawnv_passfds(exe, args, fds_to_pass)
                except:
                    os.close(alive_w)
                    os.close(authkey_w)
                    raise
                finally:
                    os.close(alive_r)
                    os.close(authkey_r)
                try:
                    write_all(authkey_w, self._forkserver_authkey)
                finally:
                    os.close(authkey_w)
                self._forkserver_address = address
                self._forkserver_alive_fd = alive_w
                self._forkserver_pid = pid


def _handle_preload(preload, main_path=None, sys_path=None, on_error='ignore'):
    """Import the modules to preload in the server."""
    if not preload:
        return
    if on_error not in _ON_ERROR_CHOICES:
        raise ValueError(
            f"on_error must be 'ignore', 'warn', or 'fail', not {on_error!r}")
    if sys_path is not None:
        sys.path[:] = sys_path

    if '__main__' in preload and main_path is not None:
        process.current_process()._inheriting = True
        try:
            spawn.import_main_path(main_path)
        except Exception as e:
            if on_error == 'fail':
                raise
            if on_error == 'warn':
                warnings.warn(
                    f"Failed to preload __main__ from {main_path!r}: {e}",
                    ImportWarning, stacklevel=2)
        finally:
            del process.current_process()._inheriting

    for modname in preload:
        if modname == '__main__':
            continue
        try:
            __import__(modname)
        except ImportError as e:
            if on_error == 'fail':
                raise
            if on_error == 'warn':
                warnings.warn(
                    f"Failed to preload module {modname!r}: {e}",
                    ImportWarning, stacklevel=2)

    # output of the preloaded modules must reach the parent's streams now
    util._flush_std_streams()


def main(listener_fd, alive_r, preload, main_path=None, sys_path=None,
         *, authkey_r=None, on_error='ignore'):
    """The server's main loop."""
    if authkey_r is not None:
        try:
            authkey = read_all(authkey_r, _AUTHKEY_LEN)
        finally:
            os.close(authkey_r)
    else:
        authkey = b''

    _handle_preload(preload, main_path, sys_path, on_error)

    util._close_stdin()

    # SIGCHLD only needs to wake the select() below (via the wakeup fd).
    sig_r, sig_w = os.pipe()
    os.set_blocking(sig_r, False)
    os.set_blocking(sig_w, False)

    def sigchld_handler(*_unused):
        pass

    handlers = {
        signal.SIGCHLD: sigchld_handler,
        # Ctrl-C goes to the whole process group: the parent handles it
        signal.SIGINT: signal.SIG_IGN,
        }
    old_handlers = {sig: signal.signal(sig, val)
                    for (sig, val) in handlers.items()}

    signal.set_wakeup_fd(sig_w)

    # child pid -> fd where its exit code is reported
    pid_to_fd = {}

    with socket.socket(socket.AF_UNIX, fileno=listener_fd) as listener, \
         selectors.DefaultSelector() as selector:
        _forkserver._forkserver_address = listener.getsockname()

        selector.register(listener, selectors.EVENT_READ)
        selector.register(alive_r, selectors.EVENT_READ)
        selector.register(sig_r, selectors.EVENT_READ)

        while True:
            try:
                while True:
                    rfds = [key.fileobj for (key, events) in selector.select()]
                    if rfds:
                        break

                if alive_r in rfds:
                    # the parent is gone (or stopped us)
                    assert os.read(alive_r, 1) == b'', "Not at EOF?"
                    raise SystemExit

                if sig_r in rfds:
                    os.read(sig_r, 65536)
                    _reap_children(pid_to_fd)

                if listener in rfds:
                    with listener.accept()[0] as s:
                        try:
                            if authkey:
                                wrapped = connection.Connection(s.fileno())
                                try:
                                    connection.deliver_challenge(wrapped, authkey)
                                    connection.answer_challenge(wrapped, authkey)
                                finally:
                                    wrapped._detach()
                                    del wrapped
                            fds = reduction.recvfds(s, MAXFDS_TO_SEND + 1)
                        except (EOFError, BrokenPipeError, AuthenticationError):
                            s.close()
                            continue
                        if len(fds) > MAXFDS_TO_SEND:
                            raise RuntimeError(
                                "Too many ({0:n}) fds to send".format(len(fds)))
                        child_r, child_w, *fds = fds
                        s.close()
                        pid = os.fork()
                        if pid == 0:
                            code = 1
                            try:
                                listener.close()
                                selector.close()
                                unused_fds = [alive_r, child_w, sig_r, sig_w]
                                unused_fds.extend(pid_to_fd.values())
                                atexit._clear()
                                atexit.register(util._exit_function)
                                code = _serve_one(child_r, fds, unused_fds,
                                                  old_handlers)
                            except Exception:
                                sys.excepthook(*sys.exc_info())
                                sys.stderr.flush()
                            finally:
                                atexit._run_exitfuncs()
                                os._exit(code)
                        else:
                            try:
                                write_signed(child_w, pid)
                            except BrokenPipeError:
                                # the parent went away
                                pass
                            pid_to_fd[pid] = child_w
                            os.close(child_r)
                            for fd in fds:
                                os.close(fd)

            except OSError as e:
                if e.errno != errno.ECONNABORTED:
                    raise


def _reap_children(pid_to_fd):
    while True:
        try:
            pid, sts = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break
        if pid == 0:
            break
        child_w = pid_to_fd.pop(pid, None)
        if child_w is not None:
            returncode = os.waitstatus_to_exitcode(sts)
            try:
                write_signed(child_w, returncode)
            except BrokenPipeError:
                pass
            os.close(child_w)
        else:
            warnings.warn('forkserver: waitpid returned '
                          'unexpected pid %d' % pid)


def _serve_one(child_r, fds, unused_fds, handlers):
    # back to the default signal handling in the child
    signal.set_wakeup_fd(-1)
    for sig, val in handlers.items():
        signal.signal(sig, val)

    for fd in unused_fds:
        os.close(fd)

    (_forkserver._forkserver_alive_fd,
     resource_tracker._resource_tracker._fd,
     *_forkserver._inherited_fds) = fds

    parent_sentinel = os.dup(child_r)
    return spawn._main(child_r, parent_sentinel)


# ---------------------------------------------------------------- helpers

def read_all(fd, size):
    data = b''
    while len(data) < size:
        chunk = os.read(fd, size - len(data))
        if not chunk:
            raise EOFError('unexpected EOF')
        data += chunk
    return data


def write_all(fd, data):
    view = memoryview(data)
    while view:
        n = os.write(fd, view)
        if n == 0:
            raise RuntimeError('short write')
        view = view[n:]


def read_signed(fd):
    return SIGNED_STRUCT.unpack(read_all(fd, SIGNED_STRUCT.size))[0]


def write_signed(fd, n):
    write_all(fd, SIGNED_STRUCT.pack(n))


_forkserver = ForkServer()
ensure_running = _forkserver.ensure_running
get_inherited_fds = _forkserver.get_inherited_fds
connect_to_new_process = _forkserver.connect_to_new_process
set_forkserver_preload = _forkserver.set_forkserver_preload
