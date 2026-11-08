"""Discover and follow child processes of a profiling target, so ``--subprocesses``
can start a profiler for each Python child.
"""

import os
import subprocess
import sys
import threading
import time

__all__ = (
    'get_child_pids',
    'is_python_process',
    'ChildProcessMonitor',
)

# How many child profilers may run at once, and how often (in poll cycles) the
# monitor reaps finished ones.
_MAX_CHILD_PROFILERS = 100
_CLEANUP_INTERVAL_CYCLES = 10
_POLL_INTERVAL_SEC = 0.1


def get_child_pids(pid, recursive=True):
    """Child process ids of *pid* (all descendants when *recursive*)."""
    try:
        import _remote_debugging
        if hasattr(_remote_debugging, "get_child_pids"):
            pids = _remote_debugging.get_child_pids(pid)
            if not recursive:
                pids = [p for p in pids if _parent_of(p) == pid]
            return list(pids)
    except ImportError:
        pass
    return _get_child_pids_fallback(pid, recursive)


def _parent_of(pid):
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as f:
            data = f.read()
        rparen = data.rfind(")")
        return int(data[rparen + 1:].split()[1])
    except (OSError, ValueError, IndexError):
        return None


def _get_child_pids_fallback(pid, recursive):
    parents = {}
    try:
        for name in os.listdir("/proc"):
            if not name.isdigit():
                continue
            parent = _parent_of(int(name))
            if parent is not None:
                parents.setdefault(parent, []).append(int(name))
    except OSError:
        return []
    out = []
    frontier = [pid]
    while frontier:
        cur = frontier.pop()
        for child in parents.get(cur, ()):
            if child not in out:
                out.append(child)
                if recursive:
                    frontier.append(child)
    return out


def is_python_process(pid):
    """Whether *pid* is a process we can remotely inspect (a compatible Python)."""
    try:
        import _remote_debugging
        return bool(_remote_debugging.is_python_process(pid))
    except ImportError:
        return False
    except (OSError, RuntimeError):
        return False


class ChildProcessMonitor:
    """Watches a target process for new Python children and starts a profiler
    subprocess for each one; used as a context manager."""

    def __init__(self, pid, cli_args, output_pattern):
        self.parent_pid = pid
        self.cli_args = cli_args
        self.output_pattern = output_pattern
        self._spawned_profilers = []
        self._known_pids = set()
        self._monitor_thread = None
        self._stop = threading.Event()

    @property
    def spawned_profilers(self):
        return list(self._spawned_profilers)

    def __enter__(self):
        self._stop.clear()
        self._monitor_thread = threading.Thread(
            target=self._run, name="child-process-monitor", daemon=True
        )
        self._monitor_thread.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        if self._monitor_thread is not None:
            self._monitor_thread.join(timeout=5)
        return False

    def _run(self):
        cycles = 0
        while not self._stop.is_set():
            cycles += 1
            try:
                self._poll()
            except Exception:
                pass
            if cycles % _CLEANUP_INTERVAL_CYCLES == 0:
                self._reap()
            self._stop.wait(_POLL_INTERVAL_SEC)

    def _poll(self):
        for child in get_child_pids(self.parent_pid, recursive=True):
            if child in self._known_pids:
                continue
            self._known_pids.add(child)
            if not is_python_process(child):
                continue
            self._spawn_profiler_for_child(child)

    def _running_profilers(self):
        return [proc for proc in self._spawned_profilers if proc.poll() is None]

    def _spawn_profiler_for_child(self, pid):
        """Start ``python -m profiling.sampling attach PID`` for a new child."""
        if len(self._running_profilers()) >= _MAX_CHILD_PROFILERS:
            print(f"Warning: Max child profilers ({_MAX_CHILD_PROFILERS}) "
                  f"reached; not profiling child process {pid}",
                  file=sys.stderr)
            return None
        args = [sys.executable, "-m", "profiling.sampling", "attach", str(pid)]
        args += list(self.cli_args)
        if self.output_pattern:
            args += ["-o", self.output_pattern.format(pid=pid)]
        try:
            proc = subprocess.Popen(args, stdin=subprocess.DEVNULL)
        except OSError as exc:
            print(f"Warning: could not start a profiler for child {pid}: {exc}",
                  file=sys.stderr)
            return None
        self._spawned_profilers.append(proc)
        return proc

    def _reap(self):
        self._spawned_profilers = self._running_profilers()

    def wait_for_profilers(self, timeout=None):
        """Wait (at most *timeout* seconds) for the child profilers to finish."""
        pending = list(self._spawned_profilers)
        if not pending:
            return
        count = len(pending)
        print(f"Waiting for {count} child profiler{'s' if count != 1 else ''} "
              f"to finish...", file=sys.stderr)
        deadline = None if timeout is None else time.monotonic() + timeout
        for proc in pending:
            remaining = None
            if deadline is not None:
                remaining = max(0.0, deadline - time.monotonic())
            try:
                proc.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                break
        self._reap()
