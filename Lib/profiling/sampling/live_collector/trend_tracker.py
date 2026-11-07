"""Track whether a live metric is rising, falling or steady between refreshes, so
the terminal view can show a direction indicator for each function row.
"""

__all__ = ('TrendTracker',)

# Relative change below this is reported as "stable".
_STABLE_THRESHOLD = 0.001


class TrendTracker:
    def __init__(self, colors=None, *, enabled=True):
        self.colors = colors or {}
        self.enabled = enabled
        self._last = {}

    def update(self, key, metric, value):
        """Return 'up', 'down' or 'stable' for *metric* of *key* vs its last value."""
        if not self.enabled:
            return "stable"
        prev = self._last.get((key, metric))
        self._last[(key, metric)] = value
        if prev is None:
            return "stable"
        if prev == 0:
            if value == 0:
                return "stable"
            return "up" if value > 0 else "down"
        rel = (value - prev) / abs(prev)
        if abs(rel) < _STABLE_THRESHOLD:
            return "stable"
        return "up" if rel > 0 else "down"

    def update_metrics(self, key, metrics):
        """Update several metrics of *key* at once; return {metric: trend}."""
        return {metric: self.update(key, metric, value)
                for metric, value in metrics.items()}

    def trend_attr(self, trend):
        """The display attribute configured for *trend* (0 when unset)."""
        return self.colors.get(f"trend_{trend}", 0)

    def reset(self, key=None):
        if key is None:
            self._last.clear()
        else:
            self._last = {k: v for k, v in self._last.items() if k[0] != key}
