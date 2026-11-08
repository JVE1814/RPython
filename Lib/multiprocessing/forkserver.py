#
# multiprocessing.forkserver -- the fork server.
#
# A fresh, single-threaded interpreter (optionally with some modules already
# imported) listens on a Unix socket and forks a child whenever a client
# asks.  Forking from it is safe even when the client has threads.
#
# One request: the client connects, both sides prove they know the server's
# random key (the connection module's challenge/response), and the client
# sends descriptors over the socket:
#     [child_r, child_w, server-alive fd, resource-tracker fd, *extra fds]
# The server forks; the child reads its preparation data and Process object
# from child_r, and the server writes the child's pid, later its exit
# status, to child_w.  The server exits when the write end of its "alive"
# pipe closes, i.e. when the process that started it goes away.
#
# Start-up settings travel through a pipe as well: first the key, then,
# optionally, a JSON object with the main module path, sys.path and
# sys.argv (too big for a command-line argument in general).
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
SIGNED_STRUCT = struct.Struct('q')     # wide enough for a pid or a status
_AUTHKEY_LEN = 32
_ON_ERROR_CHOICES = ('ignore', 'warn', 'fail')
# Descriptors every request carries besides the caller's own.
_FIXED_FDS = 4


def _check_on_error(on_error):
    if on_error not in _ON_ERROR_CHOICES:
        raise ValueError(
            f"on_error must be 'ignore', 'warn', or 'fail', not {on_error!r}")


def _authenticate(sock, authkey, server_side):
    # Run the mutual challenge over a borrowed Connection without letting it
    # own (and close) the socket's descriptor.
    conn = connection.Connection(sock.fileno())
    try:
        if server_side:
            connection.deliver_challenge(conn, authkey)
            connection.answer_challenge(conn, authkey)
        else:
            connection.answer_challenge(conn, authkey)
            connection.deliver_challenge(conn, authkey)
    finally:
        conn._detach()


class ForkServer(object):
    """Client-side handle on this process's fork server."""

    def __init__(self):
        self._forkserver_authkey = None
        self._forkserver_address = None
        self._forkserver_alive_fd = None
        self._forkserver_pid = None
        self._inherited_fds = None
        self._lock = threading.Lock()
        self._preload_modules = ['__main__']
        self._preload_on_error = 'ignore'

    # -- configuration -------------------------------------------------------

    def set_forkserver_preload(self, modules_names, on_error='ignore'):
        """Modules the server imports before forking any child.

        *on_error* chooses what a failing import does: 'ignore' it, 'warn'
        (an ImportWarning in the server) or 'fail' (the server exits)."""
        if not all(type(name) is str for name in modules_names):
            raise TypeError('module_names must be a list of strings')
        _check_on_error(on_error)
        self._preload_modules = modules_names
        self._preload_on_error = on_error

    def get_inherited_fds(self):
        """In a child of the server: the extra descriptors sent along."""
        return self._inherited_fds

    # -- starting and stopping the server ------------------------------------

    def _forget_server(self):
        self._forkserver_address = None
        self._forkserver_alive_fd = None
        self._forkserver_authkey = None
        self._forkserver_pid = None

    def _stop(self):
        with self._lock:
            self._stop_unlocked()

    def _stop_unlocked(self):
        if self._forkserver_pid is None:
            return
        # Closing our end of the alive pipe tells the server to exit.
        os.close(self._forkserver_alive_fd)
        os.waitpid(self._forkserver_pid, 0)
        address = self._forkserver_address
        if not util.is_abstract_socket_namespace(address):
            try:
                os.unlink(address)
            except FileNotFoundError:
                pass
        self._forget_server()

    def _server_died(self):
        # True (after cleaning up) if a previously started server has exited.
        pid, _status = os.waitpid(self._forkserver_pid, os.WNOHANG)
        if not pid:
            return False
        os.close(self._forkserver_alive_fd)
        self._forget_server()
        return True

    def _startup_extras(self):
        # Settings the server needs to preload __main__ like a spawned child
        # would import it.
        if not self._preload_modules:
            return {}
        data = spawn.get_preparation_data('ignore')
        extras = {}
        for ours, theirs in (('sys_path', 'sys_path'),
                             ('sys_argv', 'sys_argv'),
                             ('init_main_from_path', 'main_path')):
            if ours in data:
                extras[theirs] = data[ours]
        return extras

    def ensure_running(self):
        """Start the server unless it is already running."""
        with self._lock:
            resource_tracker.ensure_running()
            if self._forkserver_pid is not None and not self._server_died():
                return
            extras = self._startup_extras()
            with socket.socket(socket.AF_UNIX) as listener:
                address = connection.arbitrary_address('AF_UNIX')
                listener.bind(address)
                if not util.is_abstract_socket_namespace(address):
                    os.chmod(address, 0o600)
                listener.listen()
                authkey = os.urandom(_AUTHKEY_LEN)
                alive_r, alive_w = os.pipe()
                settings_r, settings_w = os.pipe()
                try:
                    pid = self._spawn_server(listener.fileno(), alive_r,
                                             settings_r)
                except BaseException:
                    os.close(alive_w)
                    os.close(settings_w)
                    raise
                finally:
                    os.close(alive_r)
                    os.close(settings_r)
                payload = authkey
                if extras:
                    import json
                    payload += json.dumps(extras).encode('ascii')
                try:
                    write_all(settings_w, payload)
                finally:
                    os.close(settings_w)
                self._forkserver_authkey = authkey
                self._forkserver_address = address
                self._forkserver_alive_fd = alive_w
                self._forkserver_pid = pid

    def _spawn_server(self, listener_fd, alive_r, settings_r):
        options = {'on_error': self._preload_on_error,
                   'authkey_r': settings_r}
        code = ('from multiprocessing.forkserver import main; '
                'main(%d, %d, %r, **%r)'
                % (listener_fd, alive_r, self._preload_modules, options))
        exe = spawn.get_executable()
        argv = [exe] + util._args_from_interpreter_flags() + ['-c', code]
        return util.spawnv_passfds(exe, argv,
                                   [listener_fd, alive_r, settings_r])

    # -- asking for a child --------------------------------------------------

    def connect_to_new_process(self, fds):
        """Have the server fork a child.  Returns (sentinel, w): the read
        end the server reports the pid and status on, and the write end of
        the pipe the child reads its pickled state from."""
        self.ensure_running()
        assert self._forkserver_authkey
        if len(fds) + _FIXED_FDS >= MAXFDS_TO_SEND:
            raise ValueError('too many fds')
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(self._forkserver_address)
                return self._send_request(client, fds)
        except (EOFError, ConnectionError) as exc:
            exc.add_note('Forkserver process may have crashed during '
                         'startup (e.g. a preloaded module failed to import '
                         'with on_error="fail"); check its stderr.')
            raise

    def _send_request(self, client, fds):
        status_r, status_w = os.pipe()
        child_r, to_child = os.pipe()
        try:
            client.setblocking(True)
            _authenticate(client, self._forkserver_authkey, server_side=False)
            reduction.sendfds(client, [child_r, status_w,
                                       self._forkserver_alive_fd,
                                       resource_tracker.getfd(), *fds])
        except BaseException:
            os.close(status_r)
            os.close(to_child)
            raise
        finally:
            # The server holds its own copies now.
            os.close(child_r)
            os.close(status_w)
        return status_r, to_child


# --- inside the server ------------------------------------------------------

def _handle_preload(preload, main_path=None, sys_path=None, on_error='ignore',
                    sys_argv=None):
    """Import the modules listed in *preload* in the server process."""
    if not preload:
        return
    _check_on_error(on_error)
    if sys_path is not None:
        sys.path[:] = sys_path
    if sys_argv is not None:
        # While being preloaded the main module sees the parent's arguments.
        sys.argv[:] = sys_argv
    if '__main__' in preload and main_path is not None:
        me = process.current_process()
        me._inheriting = True
        try:
            spawn.import_main_path(main_path)
        except Exception as exc:
            _preload_failed(on_error, exc,
                            f"Failed to preload __main__ from {main_path!r}: {exc}")
        finally:
            del me._inheriting
    for name in preload:
        if name == '__main__':
            continue
        try:
            __import__(name)
        except ImportError as exc:
            _preload_failed(on_error, exc,
                            f"Failed to preload module {name!r}: {exc}")
    # Anything the preloaded code printed must reach the parent's streams
    # before the first fork duplicates the buffers.
    util._flush_std_streams()


def _preload_failed(on_error, exc, message):
    if on_error == 'fail':
        raise exc
    if on_error == 'warn':
        warnings.warn(message, ImportWarning, stacklevel=3)


def _read_settings(settings_fd):
    # -> (authkey, extras dict) from the settings pipe.
    try:
        authkey = read_all(settings_fd, _AUTHKEY_LEN)
        chunks = []
        while True:
            chunk = os.read(settings_fd, 65536)
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        os.close(settings_fd)
    extras = {}
    if chunks:
        import json
        extras = json.loads(b''.join(chunks))
    return authkey, extras


def _receive_request(sock, authkey):
    # -> the descriptors of one request, or None for a client that went away
    # or failed to authenticate.
    try:
        if authkey:
            _authenticate(sock, authkey, server_side=True)
        fds = reduction.recvfds(sock, MAXFDS_TO_SEND + 1)
    except (EOFError, BrokenPipeError, AuthenticationError):
        return None
    if len(fds) > MAXFDS_TO_SEND:
        raise RuntimeError("Too many ({0:n}) fds to send".format(len(fds)))
    return fds


def main(listener_fd, alive_r, preload, main_path=None, sys_path=None,
         *, authkey_r=None, on_error='ignore', sys_argv=None):
    """The fork server's main loop (run in the server process)."""
    authkey = b''
    if authkey_r is not None:
        authkey, extras = _read_settings(authkey_r)
        main_path = extras.get('main_path', main_path)
        sys_path = extras.get('sys_path', sys_path)
        sys_argv = extras.get('sys_argv', sys_argv)
    _handle_preload(preload, main_path, sys_path, on_error, sys_argv)
    util._close_stdin()

    # SIGCHLD only has to interrupt the select() below: the wakeup fd makes
    # sig_r readable.  Ctrl-C reaches the whole process group; the client
    # handles it, not us.
    sig_r, sig_w = os.pipe()
    os.set_blocking(sig_r, False)
    os.set_blocking(sig_w, False)
    previous_handlers = {
        signal.SIGCHLD: signal.signal(signal.SIGCHLD, lambda *args: None),
        signal.SIGINT: signal.signal(signal.SIGINT, signal.SIG_IGN),
    }
    signal.set_wakeup_fd(sig_w)

    status_fds = {}     # child pid -> descriptor its status is reported on
    with socket.socket(socket.AF_UNIX, fileno=listener_fd) as listener, \
            selectors.DefaultSelector() as selector:
        _forkserver._forkserver_address = listener.getsockname()
        for source in (listener, alive_r, sig_r):
            selector.register(source, selectors.EVENT_READ)
        while True:
            try:
                ready = []
                while not ready:
                    ready = [key.fileobj for key, _ in selector.select()]
                if alive_r in ready:
                    # The client is gone or asked us to stop.
                    assert os.read(alive_r, 1) == b'', "Not at EOF?"
                    raise SystemExit
                if sig_r in ready:
                    os.read(sig_r, 65536)
                    _reap_children(status_fds)
                if listener in ready:
                    _serve_request(listener, selector, authkey, status_fds,
                                   (alive_r, sig_r, sig_w), previous_handlers)
            except OSError as exc:
                if exc.errno != errno.ECONNABORTED:
                    raise


def _serve_request(listener, selector, authkey, status_fds, server_fds,
                   previous_handlers):
    sock = listener.accept()[0]
    with sock:
        fds = _receive_request(sock, authkey)
    if fds is None:
        return
    child_r, status_w, *inherited = fds
    pid = os.fork()
    if pid == 0:
        # The child: drop everything that belongs to the server, then become
        # the requested process.  Never returns.
        exitcode = 1
        try:
            listener.close()
            selector.close()
            for fd in (*server_fds, status_w, *status_fds.values()):
                os.close(fd)
            atexit._clear()
            atexit.register(util._exit_function)
            exitcode = _serve_one(child_r, inherited, previous_handlers)
        except Exception:
            sys.excepthook(*sys.exc_info())
            sys.stderr.flush()
        finally:
            atexit._run_exitfuncs()
            os._exit(exitcode)
    try:
        write_signed(status_w, pid)
    except BrokenPipeError:
        pass        # the client already went away
    status_fds[pid] = status_w
    os.close(child_r)
    for fd in inherited:
        os.close(fd)


def _reap_children(status_fds):
    # Collect every exited child and report its status to its client.
    while True:
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return
        status_w = status_fds.pop(pid, None)
        if status_w is None:
            warnings.warn('forkserver: waitpid returned '
                          'unexpected pid %d' % pid)
            continue
        try:
            write_signed(status_w, os.waitstatus_to_exitcode(status))
        except BrokenPipeError:
            pass
        os.close(status_w)


def _serve_one(child_r, fds, handlers):
    # In a freshly forked child of the server.
    signal.set_wakeup_fd(-1)
    for signum, handler in handlers.items():
        signal.signal(signum, handler)
    (_forkserver._forkserver_alive_fd,
     resource_tracker._resource_tracker._fd,
     *_forkserver._inherited_fds) = fds
    return spawn._main(child_r, os.dup(child_r))


# --- small pipe helpers -----------------------------------------------------

def read_all(fd, size):
    """Read exactly *size* bytes from *fd*."""
    parts = []
    missing = size
    while missing:
        chunk = os.read(fd, missing)
        if not chunk:
            raise EOFError('unexpected EOF')
        parts.append(chunk)
        missing -= len(chunk)
    return b''.join(parts)


def write_all(fd, data):
    """Write all of *data* to *fd*."""
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        if written == 0:
            raise RuntimeError('short write')
        view = view[written:]


def read_signed(fd):
    return SIGNED_STRUCT.unpack(read_all(fd, SIGNED_STRUCT.size))[0]


def write_signed(fd, n):
    write_all(fd, SIGNED_STRUCT.pack(n))


_forkserver = ForkServer()
ensure_running = _forkserver.ensure_running
get_inherited_fds = _forkserver.get_inherited_fds
connect_to_new_process = _forkserver.connect_to_new_process
set_forkserver_preload = _forkserver.set_forkserver_preload
