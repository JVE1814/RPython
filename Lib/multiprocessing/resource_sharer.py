#
# multiprocessing.resource_sharer -- lending file descriptors to processes
# that were not started by us.
#
# Pickling a descriptor outside of process start-up (a socket sent through
# a queue, say) parks a duplicate of it here and pickles a ticket instead.
# The process that unpickles the ticket connects to this process's sharer
# thread, authenticating with the shared authkey, presents the ticket and
# receives the descriptor over the Unix socket (SCM_RIGHTS).  A ticket can
# be redeemed once.
#

import os
import signal
import sys
import threading

from . import process
from .context import reduction
from . import util

__all__ = ['stop']


class DupFd(object):
    """Picklable ticket for a duplicate of a file descriptor."""

    def __init__(self, fd):
        copy = os.dup(fd)

        def hand_over(conn, pid):
            reduction.send_handle(conn, copy, pid)

        def discard():
            os.close(copy)

        self._id = _resource_sharer.register(hand_over, discard)

    def detach(self):
        """Redeem the ticket: fetch the descriptor from its owner."""
        with _resource_sharer.get_connection(self._id) as conn:
            return reduction.recv_handle(conn)


def _authkey():
    return process.current_process().authkey


class _ResourceSharer(object):
    """Owner of the parked resources and of the thread that serves them.
    The thread and its listener are created on first use."""

    def __init__(self):
        self._key = 0
        self._cache = {}            # ticket number -> (hand_over, discard)
        self._lock = threading.Lock()
        self._listener = None
        self._address = None
        self._thread = None
        util.register_after_fork(self, _ResourceSharer._afterfork)

    def register(self, send, close):
        """Park a resource; returns its ticket (address, number)."""
        with self._lock:
            if self._address is None:
                self._start()
            self._key += 1
            self._cache[self._key] = (send, close)
            return (self._address, self._key)

    @staticmethod
    def get_connection(ident):
        """Open an authenticated connection to the owner of a ticket and
        present the ticket."""
        from .connection import Client
        address, key = ident
        conn = Client(address, authkey=_authkey())
        conn.send((key, os.getpid()))
        return conn

    def _discard_all(self):
        for _send, close in self._cache.values():
            close()
        self._cache.clear()

    def stop(self, timeout=None):
        """Stop serving and close every resource still parked."""
        from .connection import Client
        with self._lock:
            if self._address is None:
                return
            # A None request tells the serving thread to finish.
            stopper = Client(self._address, authkey=_authkey())
            stopper.send(None)
            stopper.close()
            self._thread.join(timeout)
            if self._thread.is_alive():
                util.sub_warning('_ResourceSharer thread did '
                                 'not stop when asked')
            self._listener.close()
            self._listener = self._address = self._thread = None
            self._discard_all()

    def _afterfork(self):
        # The serving thread was not copied into the forked child, and the
        # parked resources belong to the parent.
        self._discard_all()
        self._lock._at_fork_reinit()
        if self._listener is not None:
            self._listener.close()
        self._listener = self._address = self._thread = None

    def _start(self):
        from .connection import Listener
        assert self._listener is None, "Already have Listener"
        util.debug('starting listener and thread for sending handles')
        self._listener = Listener(authkey=_authkey(), backlog=128)
        self._address = self._listener.address
        thread = threading.Thread(target=self._serve, daemon=True)
        thread.start()
        self._thread = thread

    def _serve(self):
        if hasattr(signal, 'pthread_sigmask'):
            # Leave signal handling to the main thread.
            signal.pthread_sigmask(signal.SIG_BLOCK, signal.valid_signals())
        while True:
            try:
                with self._listener.accept() as conn:
                    request = conn.recv()
                    if request is None:
                        return
                    key, destination_pid = request
                    send, close = self._cache.pop(key)
                    try:
                        send(conn, destination_pid)
                    finally:
                        close()
            except BaseException:
                if not util.is_exiting():
                    sys.excepthook(*sys.exc_info())


_resource_sharer = _ResourceSharer()
stop = _resource_sharer.stop
