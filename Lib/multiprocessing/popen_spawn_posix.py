#
# multiprocessing.popen_spawn_posix -- the "spawn" start method on POSIX:
# the child is a new interpreter started with fork+exec.
#

import io
import os

from .context import reduction, set_spawning_popen
from . import popen_fork
from . import spawn
from . import util

__all__ = ['Popen']


class _DupFd(object):
    """A descriptor handed to the child by leaving it open across exec();
    the number stays the same in the child."""

    def __init__(self, fd):
        self.fd = fd

    def detach(self):
        return self.fd


def _pickle_for_child(popen, process_obj):
    # The preparation data and the Process object, pickled while *popen* is
    # registered as the Popen starting a child (reducers of inheritable
    # objects check for that and may call popen.duplicate_for_child()).
    data = spawn.get_preparation_data(process_obj._name)
    out = io.BytesIO()
    set_spawning_popen(popen)
    try:
        reduction.dump(data, out)
        reduction.dump(process_obj, out)
    finally:
        set_spawning_popen(None)
    return out


class Popen(popen_fork.Popen):
    method = 'spawn'
    DupFd = _DupFd

    def __init__(self, process_obj):
        self._fds = []          # descriptors the new interpreter inherits
        super().__init__(process_obj)

    def duplicate_for_child(self, fd):
        self._fds.append(fd)
        return fd

    def _launch(self, process_obj):
        from . import resource_tracker
        tracker_fd = resource_tracker.getfd()
        self._fds.append(tracker_fd)
        payload = _pickle_for_child(self, process_obj)
        # Our ends of the two pipes are kept (and closed by the finalizer);
        # the child's ends are closed here once it has been started.
        ours, theirs = [], []
        try:
            # Nothing is ever written to the first pipe: its read end is our
            # sentinel.  The child reads its two pickles from the second.
            sentinel_r, sentinel_w = os.pipe()
            ours.append(sentinel_r)
            theirs.append(sentinel_w)
            child_r, parent_w = os.pipe()
            theirs.append(child_r)
            ours.append(parent_w)
            argv = spawn.get_command_line(tracker_fd=tracker_fd,
                                          pipe_handle=child_r)
            self._fds += [child_r, sentinel_w]
            self.pid = util.spawnv_passfds(spawn.get_executable(), argv,
                                           self._fds)
            self.sentinel = sentinel_r
            with open(parent_w, 'wb', closefd=False) as stream:
                stream.write(payload.getbuffer())
        finally:
            self.finalizer = util.Finalize(self, util.close_fds, ours)
            for fd in theirs:
                os.close(fd)
