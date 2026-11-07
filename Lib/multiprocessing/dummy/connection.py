#
# multiprocessing.dummy.connection -- Listener, Client and Pipe for
# threads.  A connection is a pair of queues; a listener's address is the
# queue on which clients drop their halves of a new pair.
#

__all__ = ['Client', 'Listener', 'Pipe']

from queue import Queue


families = [None]


class Listener(object):

    def __init__(self, address=None, family=None, backlog=1):
        self._backlog_queue = Queue(backlog)

    def accept(self):
        return Connection(*self._backlog_queue.get())

    def close(self):
        self._backlog_queue = None

    @property
    def address(self):
        return self._backlog_queue

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()


def Client(address):
    """Connect to the Listener whose address is *address*."""
    to_server, to_client = Queue(), Queue()
    # The listener's side reads what we send and sends what we read.
    address.put((to_server, to_client))
    return Connection(to_client, to_server)


def Pipe(duplex=True):
    """Two connected Connection objects (always duplex)."""
    one_way, other_way = Queue(), Queue()
    return Connection(one_way, other_way), Connection(other_way, one_way)


class Connection(object):
    """One end of a queue-based connection: receives from *_in*, sends to
    *_out*."""

    def __init__(self, _in, _out):
        self._in = _in
        self._out = _out
        self.send = self.send_bytes = _out.put
        self.recv = self.recv_bytes = _in.get

    def poll(self, timeout=0.0):
        incoming = self._in
        if incoming.qsize() > 0:
            return True
        if timeout <= 0.0:
            return False
        with incoming.not_empty:
            incoming.not_empty.wait(timeout)
        return incoming.qsize() > 0

    def close(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()
