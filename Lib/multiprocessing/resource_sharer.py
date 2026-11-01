#
# multiprocessing.resource_sharer: hand file descriptors to other processes
# on request.
#
# Pickling an fd outside of process start-up registers a duplicate with a
# per-process server thread; the unpickling process connects to that
# server (authenticated with the authkey) and receives the fd over the
# Unix socket (SCM_RIGHTS).
#

import os
import signal
import socket
import sys
import threading

from . import process
from .context import reduction
from . import util

__all__ = ['stop']


class DupFd(object):
    """Picklable stand-in for a duplicated file descriptor."""

    def __init__(self, fd):
        new_fd = os.dup(fd)

        def send(conn, pid):
            reduction.send_handle(conn, new_fd, pid)

        def close():
            os.close(new_fd)

        self._id = _resource_sharer.register(send, close)

    def detach(self):
        """Fetch the file descriptor from the owning process."""
        with _resource_sharer.get_connection(self._id) as conn:
            return reduction.recv_handle(conn)


class _ResourceSharer(object):
    """The server thread handing out registered resources (one per
    process, started on first use)."""

    def __init__(self):
        self._key = 0
        self._cache = {}
        self._lock = threading.Lock()
        self._listener = None
        self._address = None
        self._thread = None
        util.register_after_fork(self, _ResourceSharer._afterfork)

    def register(self, send, close):
        """Register a resource; returns its identifier for get_connection()."""
        with self._lock:
            if self._address is None:
                self._start()
            self._key += 1
            self._cache[self._key] = (send, close)
            return (self._address, self._key)

    @staticmethod
    def get_connection(ident):
        """Connect to the server owning the resource *ident*."""
        from .connection import Client
        address, key = ident
        c = Client(address, authkey=process.current_process().authkey)
        c.send((key, os.getpid()))
        return c

    def stop(self, timeout=None):
        """Stop the server thread and close every pending resource."""
        from .connection import Client
        with self._lock:
            if self._address is not None:
                c = Client(self._address,
                           authkey=process.current_process().authkey)
                c.send(None)
                c.close()
                self._thread.join(timeout)
                if self._thread.is_alive():
                    util.sub_warning('_ResourceSharer thread did '
                                     'not stop when asked')
                self._listener.close()
                self._thread = None
                self._address = None
                self._listener = None
                for key, (send, close) in self._cache.items():
                    close()
                self._cache.clear()

    def _afterfork(self):
        # the server thread does not exist in a forked child
        for key, (send, close) in self._cache.items():
            close()
        self._cache.clear()
        self._lock._at_fork_reinit()
        if self._listener is not None:
            self._listener.close()
        self._listener = None
        self._address = None
        self._thread = None

    def _start(self):
        from .connection import Listener
        assert self._listener is None, "Already have Listener"
        util.debug('starting listener and thread for sending handles')
        self._listener = Listener(authkey=process.current_process().authkey,
                                  backlog=128)
        self._address = self._listener.address
        t = threading.Thread(target=self._serve)
        t.daemon = True
        t.start()
        self._thread = t

    def _serve(self):
        if hasattr(signal, 'pthread_sigmask'):
            # signals are for the main thread
            signal.pthread_sigmask(signal.SIG_BLOCK, signal.valid_signals())
        while True:
            try:
                with self._listener.accept() as conn:
                    msg = conn.recv()
                    if msg is None:
                        break
                    key, destination_pid = msg
                    send, close = self._cache.pop(key)
                    try:
                        send(conn, destination_pid)
                    finally:
                        close()
            except:
                if not util.is_exiting():
                    sys.excepthook(*sys.exc_info())


_resource_sharer = _ResourceSharer()
stop = _resource_sharer.stop
