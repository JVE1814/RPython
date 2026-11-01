"""A framework for network servers.

A server class (TCPServer, UDPServer, UnixStreamServer, UnixDatagramServer)
accepts requests and hands each one to an instance of a request handler
class (subclass of BaseRequestHandler, typically StreamRequestHandler or
DatagramRequestHandler) whose handle() method does the work.  Combine a
server with ThreadingMixIn or ForkingMixIn to process requests
concurrently.
"""

import io
import os
import selectors
import socket
import sys
import threading
from time import monotonic as _time

__all__ = ["BaseServer", "TCPServer", "UDPServer",
           "ThreadingUDPServer", "ThreadingTCPServer",
           "BaseRequestHandler", "StreamRequestHandler",
           "DatagramRequestHandler", "ThreadingMixIn"]
if hasattr(os, "fork"):
    __all__ += ["ForkingUDPServer", "ForkingTCPServer", "ForkingMixIn"]
if hasattr(socket, "AF_UNIX"):
    __all__ += ["UnixStreamServer", "UnixDatagramServer",
                "ThreadingUnixStreamServer", "ThreadingUnixDatagramServer"]
    if hasattr(os, "fork"):
        __all__ += ["ForkingUnixStreamServer", "ForkingUnixDatagramServer"]

# poll() needs no descriptor of its own, unlike epoll/kqueue
_ServerSelector = getattr(selectors, 'PollSelector', selectors.SelectSelector)


def __getattr__(name):
    if name == "__version__":
        from warnings import _deprecated
        _deprecated("__version__", remove=(3, 20))
        return "0.4"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


class BaseServer:
    """Request loop independent of the transport.

    Subclasses provide fileno(), get_request(), server_close() and the
    socket handling; users override handle_error(), verify_request(),
    process_request() etc. as needed.
    """

    timeout = None   # seconds handle_request() waits; None = forever

    def __init__(self, server_address, RequestHandlerClass):
        self.server_address = server_address
        self.RequestHandlerClass = RequestHandlerClass
        self._quit_requested = threading.Event()
        self._loop_stopped = threading.Event()
        self._loop_stopped.set()

    def server_activate(self):
        """Hook called by the constructor once bound."""

    def serve_forever(self, poll_interval=0.5):
        """Handle requests until shutdown() is called.

        The shutdown flag is checked every *poll_interval* seconds.
        """
        self._loop_stopped.clear()
        try:
            with _ServerSelector() as selector:
                selector.register(self, selectors.EVENT_READ)
                while not self._quit_requested.is_set():
                    ready = selector.select(poll_interval)
                    if self._quit_requested.is_set():
                        break
                    if ready:
                        self._handle_request_noblock()
                    self.service_actions()
        finally:
            self._quit_requested.clear()
            self._loop_stopped.set()

    def shutdown(self):
        """Stop serve_forever() and wait for it (call from another
        thread, or it deadlocks)."""
        self._quit_requested.set()
        self._loop_stopped.wait()

    def service_actions(self):
        """Called once per serve_forever() iteration."""

    def handle_request(self):
        """Wait (up to the timeouts) for one request and handle it."""
        timeout = self.socket.gettimeout()
        if timeout is None:
            timeout = self.timeout
        elif self.timeout is not None:
            timeout = min(timeout, self.timeout)
        deadline = None if timeout is None else _time() + timeout
        with _ServerSelector() as selector:
            selector.register(self, selectors.EVENT_READ)
            while True:
                if selector.select(timeout):
                    return self._handle_request_noblock()
                if timeout is not None:
                    timeout = deadline - _time()
                    if timeout < 0:
                        return self.handle_timeout()

    def _handle_request_noblock(self):
        try:
            request, client_address = self.get_request()
        except OSError:
            return
        if not self.verify_request(request, client_address):
            self.shutdown_request(request)
            return
        try:
            self.process_request(request, client_address)
        except Exception:
            self.handle_error(request, client_address)
            self.shutdown_request(request)
        except:
            self.shutdown_request(request)
            raise

    def handle_timeout(self):
        """Called when handle_request() times out."""

    def verify_request(self, request, client_address):
        """Return False to reject a request."""
        return True

    def process_request(self, request, client_address):
        """Handle the request (mix-ins override this to go concurrent)."""
        self.finish_request(request, client_address)
        self.shutdown_request(request)

    def server_close(self):
        """Release the server's resources."""

    def finish_request(self, request, client_address):
        """Instantiate the handler class, which processes the request."""
        self.RequestHandlerClass(request, client_address, self)

    def shutdown_request(self, request):
        """Terminate a request (calls close_request())."""
        self.close_request(request)

    def close_request(self, request):
        """Release a request's resources."""

    def handle_error(self, request, client_address):
        """Report an exception raised while handling a request."""
        print('-' * 40, file=sys.stderr)
        print('Exception occurred during processing of request from',
              client_address, file=sys.stderr)
        import traceback
        traceback.print_exc()
        print('-' * 40, file=sys.stderr)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.server_close()


class TCPServer(BaseServer):
    """Stream socket server (TCP by default)."""

    address_family = socket.AF_INET
    socket_type = socket.SOCK_STREAM
    request_queue_size = 5
    allow_reuse_address = False
    allow_reuse_port = False

    def __init__(self, server_address, RequestHandlerClass,
                 bind_and_activate=True):
        BaseServer.__init__(self, server_address, RequestHandlerClass)
        self.socket = socket.socket(self.address_family, self.socket_type)
        if bind_and_activate:
            try:
                self.server_bind()
                self.server_activate()
            except:
                self.server_close()
                raise

    def server_bind(self):
        """Bind the socket (honouring allow_reuse_address/port)."""
        inet = self.address_family in (socket.AF_INET,
                                       getattr(socket, 'AF_INET6', None))
        if self.allow_reuse_address and hasattr(socket, "SO_REUSEADDR"):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if self.allow_reuse_port and hasattr(socket, "SO_REUSEPORT") and inet:
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
        self.socket.bind(self.server_address)
        self.server_address = self.socket.getsockname()

    def server_activate(self):
        self.socket.listen(self.request_queue_size)

    def server_close(self):
        self.socket.close()

    def fileno(self):
        return self.socket.fileno()

    def get_request(self):
        return self.socket.accept()

    def shutdown_request(self, request):
        try:
            # tell the peer we are done before closing
            request.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        self.close_request(request)

    def close_request(self, request):
        request.close()


class UDPServer(TCPServer):
    """Datagram server: each request is (data, socket)."""

    allow_reuse_address = False
    allow_reuse_port = False
    socket_type = socket.SOCK_DGRAM
    max_packet_size = 8192

    def get_request(self):
        data, client_addr = self.socket.recvfrom(self.max_packet_size)
        return (data, self.socket), client_addr

    def server_activate(self):
        pass   # nothing to listen() on

    def shutdown_request(self, request):
        self.close_request(request)

    def close_request(self, request):
        pass   # the socket is shared by all requests


if hasattr(os, "fork"):
    class ForkingMixIn:
        """Handle each request in a forked child process."""

        timeout = 300
        active_children = None   # pids of running children
        max_children = 40
        block_on_close = True    # server_close() waits for children

        def collect_children(self, *, blocking=False):
            """Reap finished children (limiting their number)."""
            if self.active_children is None:
                return
            while len(self.active_children) >= self.max_children:
                try:
                    pid, _ = os.waitpid(-1, 0)
                    self.active_children.discard(pid)
                except ChildProcessError:
                    self.active_children.clear()
                except OSError:
                    break
            for pid in list(self.active_children):
                try:
                    flags = 0 if blocking else os.WNOHANG
                    reaped, _ = os.waitpid(pid, flags)
                    if reaped:
                        self.active_children.discard(reaped)
                except ChildProcessError:
                    self.active_children.discard(pid)
                except OSError:
                    pass

        def handle_timeout(self):
            self.collect_children()

        def service_actions(self):
            self.collect_children()

        def process_request(self, request, client_address):
            pid = os.fork()
            if pid:
                if self.active_children is None:
                    self.active_children = set()
                self.active_children.add(pid)
                self.close_request(request)
                return
            # child: handle the request and exit without returning
            status = 1
            try:
                self.finish_request(request, client_address)
                status = 0
            except Exception:
                self.handle_error(request, client_address)
                status = 1
            finally:
                try:
                    self.shutdown_request(request)
                finally:
                    os._exit(status)

        def server_close(self):
            super().server_close()
            self.collect_children(blocking=self.block_on_close)


class _Threads:
    """Handler threads to join on close; finished ones are dropped as new
    ones are added."""

    def __init__(self):
        self._lock = threading.Lock()
        self._items = []

    def __len__(self):
        return len(self._items)

    def __iter__(self):
        return iter(list(self._items))

    def append(self, thread):
        with self._lock:
            self._items = [t for t in self._items if t.is_alive()]
            if not thread.daemon:
                self._items.append(thread)

    def join(self):
        with self._lock:
            pending, self._items = self._items, []
        for thread in pending:
            thread.join()


class _NoThreads:
    """Stand-in for _Threads when threads are not tracked."""

    def append(self, thread):
        pass

    def join(self):
        pass


class ThreadingMixIn:
    """Handle each request in a new thread."""

    daemon_threads = False   # don't wait for handler threads at exit
    block_on_close = True    # server_close() joins handler threads
    _threads = _NoThreads()

    def process_request_thread(self, request, client_address):
        """Thread body: run finish_request() with error handling."""
        try:
            self.finish_request(request, client_address)
        except Exception:
            self.handle_error(request, client_address)
        finally:
            self.shutdown_request(request)

    def process_request(self, request, client_address):
        if self.block_on_close:
            vars(self).setdefault('_threads', _Threads())
        worker = threading.Thread(target=self.process_request_thread,
                                  args=(request, client_address))
        worker.daemon = self.daemon_threads
        self._threads.append(worker)
        worker.start()

    def server_close(self):
        super().server_close()
        self._threads.join()


if hasattr(os, "fork"):
    class ForkingUDPServer(ForkingMixIn, UDPServer):
        pass

    class ForkingTCPServer(ForkingMixIn, TCPServer):
        pass


class ThreadingUDPServer(ThreadingMixIn, UDPServer):
    pass


class ThreadingTCPServer(ThreadingMixIn, TCPServer):
    pass


if hasattr(socket, 'AF_UNIX'):

    class UnixStreamServer(TCPServer):
        address_family = socket.AF_UNIX

    class UnixDatagramServer(UDPServer):
        address_family = socket.AF_UNIX

    class ThreadingUnixStreamServer(ThreadingMixIn, UnixStreamServer):
        pass

    class ThreadingUnixDatagramServer(ThreadingMixIn, UnixDatagramServer):
        pass

    if hasattr(os, "fork"):
        class ForkingUnixStreamServer(ForkingMixIn, UnixStreamServer):
            pass

        class ForkingUnixDatagramServer(ForkingMixIn, UnixDatagramServer):
            pass


class BaseRequestHandler:
    """Base of request handlers: the constructor runs setup(), handle()
    and finish().  Attributes: request, client_address, server."""

    def __init__(self, request, client_address, server):
        self.request = request
        self.client_address = client_address
        self.server = server
        self.setup()
        try:
            self.handle()
        finally:
            self.finish()

    def setup(self):
        pass

    def handle(self):
        pass

    def finish(self):
        pass


class StreamRequestHandler(BaseRequestHandler):
    """Handler for stream sockets: provides rfile and wfile."""

    rbufsize = -1   # buffered reading by default
    wbufsize = 0    # unbuffered writing: data leaves immediately
    timeout = None  # socket timeout applied to the connection
    disable_nagle_algorithm = False

    def setup(self):
        self.connection = self.request
        if self.timeout is not None:
            self.connection.settimeout(self.timeout)
        if self.disable_nagle_algorithm:
            self.connection.setsockopt(socket.IPPROTO_TCP,
                                       socket.TCP_NODELAY, True)
        self.rfile = self.connection.makefile('rb', self.rbufsize)
        if self.wbufsize == 0:
            self.wfile = _SocketWriter(self.connection)
        else:
            self.wfile = self.connection.makefile('wb', self.wbufsize)

    def finish(self):
        if not self.wfile.closed:
            try:
                self.wfile.flush()
            except OSError:
                pass   # e.g. the client reset the connection
        self.wfile.close()
        self.rfile.close()


class _SocketWriter(io.BufferedIOBase):
    """Unbuffered binary writer sending everything with sendall()."""

    def __init__(self, sock):
        self._sock = sock

    def writable(self):
        return True

    def write(self, b):
        self._sock.sendall(b)
        with memoryview(b) as view:
            return view.nbytes

    def fileno(self):
        return self._sock.fileno()


class DatagramRequestHandler(BaseRequestHandler):
    """Handler for datagram servers: rfile holds the packet, wfile is
    sent back to the client by finish()."""

    def setup(self):
        from io import BytesIO
        self.packet, self.socket = self.request
        self.rfile = BytesIO(self.packet)
        self.wfile = BytesIO()

    def finish(self):
        self.socket.sendto(self.wfile.getvalue(), self.client_address)
