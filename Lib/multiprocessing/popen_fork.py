#
# multiprocessing.popen_fork -- the "fork" start method.
#
# The Popen class here also provides what every POSIX start method shares:
# exit-status collection, waiting with a timeout and signalling the child.
#

import atexit
import os
import signal
import sys

from . import util

__all__ = ['Popen']


class Popen(object):
    """Handle on a child process created with os.fork()."""

    method = 'fork'

    def __init__(self, process_obj):
        util._flush_std_streams()
        self.returncode = None
        self.finalizer = None
        self._launch(process_obj)

    def duplicate_for_child(self, fd):
        # fork() copies every descriptor, so nothing needs to be done.
        return fd

    # -- status --------------------------------------------------------------

    def poll(self, flag=os.WNOHANG):
        """Return the exit code, or None while the child runs (when *flag*
        is WNOHANG)."""
        if self.returncode is None:
            try:
                pid, status = os.waitpid(self.pid, flag)
            except OSError:
                # Someone else reaped it (or it is not our child): there is
                # no status to report.
                return None
            if pid == self.pid:
                self.returncode = os.waitstatus_to_exitcode(status)
        return self.returncode

    def wait(self, timeout=None):
        """Wait for the child; None if *timeout* expires first."""
        if self.returncode is not None:
            return self.returncode
        if timeout is not None:
            from multiprocessing.connection import wait
            if not wait([self.sentinel], timeout):
                return None
        # The sentinel is readable, so the child has exited or is about to;
        # a blocking waitpid() is short.
        return self.poll(os.WNOHANG if timeout == 0.0 else 0)

    # -- signals -------------------------------------------------------------

    def _send_signal(self, signum):
        if self.returncode is not None:
            return
        try:
            os.kill(self.pid, signum)
        except ProcessLookupError:
            pass
        except OSError:
            # Tolerate a child that exited in the meantime.
            if self.wait(timeout=0.1) is None:
                raise

    def interrupt(self):
        self._send_signal(signal.SIGINT)

    def terminate(self):
        self._send_signal(signal.SIGTERM)

    def kill(self):
        self._send_signal(signal.SIGKILL)

    # -- starting the child --------------------------------------------------

    def _launch(self, process_obj):
        # Two pipes.  The parent keeps the read end of the first: it reaches
        # EOF when the child ends (our sentinel).  The child keeps the read
        # end of the second: EOF there means the parent ended.
        sentinel_r, sentinel_w = os.pipe()
        parent_alive_r, parent_alive_w = os.pipe()
        tracker = sys.modules.get('multiprocessing.resource_tracker')
        if tracker is not None:
            # This child keeps using our resource tracker.
            tracker._keep_tracker_fd_in_child(True)
        try:
            self.pid = os.fork()
        finally:
            if tracker is not None:
                tracker._keep_tracker_fd_in_child(False)
        if self.pid == 0:
            self._run_child(process_obj, (sentinel_r, parent_alive_w),
                            parent_alive_r)
        os.close(sentinel_w)
        os.close(parent_alive_r)
        self.finalizer = util.Finalize(self, util.close_fds,
                                       (sentinel_r, parent_alive_w))
        self.sentinel = sentinel_r

    @staticmethod
    def _run_child(process_obj, parent_fds, parent_sentinel):
        # In the forked child: never returns.  The parent's atexit hooks are
        # not ours; only multiprocessing's own exit handling runs.
        status = 1
        try:
            atexit._clear()
            atexit.register(util._exit_function)
            for fd in parent_fds:
                os.close(fd)
            status = process_obj._bootstrap(parent_sentinel=parent_sentinel)
        finally:
            atexit._run_exitfuncs()
            os._exit(status)

    def close(self):
        if self.finalizer is not None:
            self.finalizer()
