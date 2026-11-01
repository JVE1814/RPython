"""A general purpose event scheduler.

A scheduler keeps a priority queue of events.  It is parametrized with a
time function and a delay function, so it can run in real time
(time.monotonic and time.sleep, the defaults) or in simulated time.

Events are named tuples (time, priority, sequence, action, argument,
kwargs); running one means calling action(*argument, **kwargs).  Events
scheduled for the same time run in priority order (lower first), then in
the order in which they were entered.
"""

import heapq
import threading
import time
from collections import namedtuple
from itertools import count
from time import monotonic as _default_time

__all__ = ["scheduler"]

Event = namedtuple('Event', 'time, priority, sequence, action, argument, kwargs')
Event.time.__doc__ = 'When the event is due (in units of the time function).'
Event.priority.__doc__ = 'Lower values run first among events due at the same time.'
Event.sequence.__doc__ = 'Insertion counter, breaking remaining ties.'
Event.action.__doc__ = 'The callable to run: action(*argument, **kwargs).'
Event.argument.__doc__ = 'Positional arguments for the action.'
Event.kwargs.__doc__ = 'Keyword arguments for the action.'

_NO_KWARGS = object()


class scheduler:

    def __init__(self, timefunc=_default_time, delayfunc=time.sleep):
        """Create a scheduler using the given time and delay functions."""
        self.timefunc = timefunc
        self.delayfunc = delayfunc
        self._heap = []
        self._mutex = threading.RLock()
        self._counter = count()

    def enterabs(self, time, priority, action, argument=(), kwargs=_NO_KWARGS):
        """Schedule `action` at absolute `time`; returns the event, which
        can be passed to cancel()."""
        if kwargs is _NO_KWARGS:
            kwargs = {}
        with self._mutex:
            event = Event(time, priority, next(self._counter),
                          action, argument, kwargs)
            heapq.heappush(self._heap, event)
        return event

    def enter(self, delay, priority, action, argument=(), kwargs=_NO_KWARGS):
        """Schedule `action` `delay` time units from now."""
        return self.enterabs(self.timefunc() + delay, priority, action,
                             argument, kwargs)

    def cancel(self, event):
        """Remove a pending event (ValueError if it is not queued)."""
        with self._mutex:
            self._heap.remove(event)
            heapq.heapify(self._heap)

    def empty(self):
        """True if no events are pending."""
        with self._mutex:
            return not self._heap

    def run(self, blocking=True):
        """Run due events until the queue is empty.

        With blocking=False, run only the events that are already due and
        return the delay until the next one (None if the queue is empty).
        Exceptions from actions or the delay function propagate; the
        scheduler stays consistent and run() may be called again.
        """
        heap = self._heap
        while True:
            with self._mutex:
                if not heap:
                    return None
                head = heap[0]
                now = self.timefunc()
                due = head.time <= now
                if due:
                    heapq.heappop(heap)
            if not due:
                if not blocking:
                    return head.time - now
                self.delayfunc(head.time - now)
                continue
            head.action(*head.argument, **head.kwargs)
            # give other threads a chance to run between events
            self.delayfunc(0)

    @property
    def queue(self):
        """The pending events, in the order in which they would run."""
        with self._mutex:
            pending = list(self._heap)
        pending.sort()
        return pending
