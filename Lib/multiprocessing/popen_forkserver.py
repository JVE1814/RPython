#
# multiprocessing.popen_forkserver -- the "forkserver" start method: the
# fork server process forks the child on our behalf.
#

import os

from .context import reduction, set_spawning_popen
if not reduction.HAVE_SEND_HANDLE:
    raise ImportError('No support for sending fds between processes')
from . import forkserver
from . import popen_fork
from . import popen_spawn_posix
from . import util

__all__ = ['Popen']


class _DupFd(object):
    """A descriptor that travels to the server with the fork request.  The
    child finds it at position *ind* of the descriptors it inherited."""

    def __init__(self, ind):
        self.ind = ind

    def detach(self):
        return forkserver.get_inherited_fds()[self.ind]


class Popen(popen_fork.Popen):
    method = 'forkserver'
    DupFd = _DupFd

    def __init__(self, process_obj):
        self._fds = []          # extra descriptors sent along to the server
        super().__init__(process_obj)

    def duplicate_for_child(self, fd):
        self._fds.append(fd)
        return len(self._fds) - 1

    def _launch(self, process_obj):
        # Pickle first: duplicate_for_child() fills self._fds, which has to be
        # complete before the request goes to the server.
        payload = popen_spawn_posix._pickle_for_child(self, process_obj)
        self.sentinel, to_child = forkserver.connect_to_new_process(self._fds)
        # A spare copy of the child's input pipe stays open until we are
        # finalized: the child sees EOF on it (its parent sentinel) only when
        # we are gone.
        spare = os.dup(to_child)
        self.finalizer = util.Finalize(self, util.close_fds,
                                       (spare, self.sentinel))
        with open(to_child, 'wb', closefd=True) as stream:
            stream.write(payload.getbuffer())
        self.pid = forkserver.read_signed(self.sentinel)

    def poll(self, flag=os.WNOHANG):
        # The server reports the exit status through the sentinel pipe.
        if self.returncode is not None:
            return self.returncode
        from multiprocessing.connection import wait
        if not wait([self.sentinel], 0 if flag == os.WNOHANG else None):
            return None
        try:
            self.returncode = forkserver.read_signed(self.sentinel)
        except (OSError, EOFError):
            # The server vanished without telling us; the status is lost.
            self.returncode = 255
        return self.returncode
