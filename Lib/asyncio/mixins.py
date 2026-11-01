"""Lazy binding of synchronization primitives to the running loop."""

import threading

from . import events

_bind_lock = threading.Lock()


class _LoopBoundMixin:
    # The loop is chosen on first use (not at construction) and may not
    # change afterwards.
    _loop = None

    def _get_loop(self):
        running = events._get_running_loop()
        if self._loop is None:
            with _bind_lock:
                if self._loop is None:
                    self._loop = running
        if running is not self._loop:
            raise RuntimeError(f'{self!r} is bound to a different event loop')
        return running
