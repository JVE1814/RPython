"""High-level I/O multiplexing built on the select module.

A selector watches a set of file objects for readiness events (EVENT_READ,
EVENT_WRITE).  DefaultSelector is the most efficient implementation
available on the running platform.
"""

from abc import ABCMeta, abstractmethod
from collections import namedtuple
from collections.abc import Mapping
import math
import select
import sys


EVENT_READ = (1 << 0)
EVENT_WRITE = (1 << 1)


def _fileobj_to_fd(fileobj):
    """Return the file descriptor of *fileobj* (an int or an object with a
    fileno() method).  Raises ValueError for invalid objects."""
    if isinstance(fileobj, int):
        fd = fileobj
    else:
        try:
            fd = int(fileobj.fileno())
        except (AttributeError, TypeError, ValueError):
            raise ValueError("Invalid file object: "
                             "{!r}".format(fileobj)) from None
    if fd < 0:
        raise ValueError("Invalid file descriptor: {}".format(fd))
    return fd


SelectorKey = namedtuple('SelectorKey', ['fileobj', 'fd', 'events', 'data'])

SelectorKey.__doc__ = """SelectorKey(fileobj, fd, events, data)

    Associates a registered file object with its file descriptor, the events
    being monitored and arbitrary attached data.
"""
SelectorKey.fileobj.__doc__ = 'File object registered.'
SelectorKey.fd.__doc__ = 'Underlying file descriptor.'
SelectorKey.events.__doc__ = 'Events to wait for on this file object.'
SelectorKey.data.__doc__ = ('Optional opaque data associated with this file '
                            'object (e.g. a callback or a session id).')


class _SelectorMapping(Mapping):
    """Read-only view: file object -> SelectorKey."""

    def __init__(self, selector):
        self._selector = selector

    def __len__(self):
        return len(self._selector._fd_to_key)

    def get(self, fileobj, default=None):
        fd = self._selector._fileobj_lookup(fileobj)
        return self._selector._fd_to_key.get(fd, default)

    def __getitem__(self, fileobj):
        fd = self._selector._fileobj_lookup(fileobj)
        key = self._selector._fd_to_key.get(fd)
        if key is None:
            raise KeyError("{!r} is not registered".format(fileobj))
        return key

    def __iter__(self):
        return iter(self._selector._fd_to_key)


class BaseSelector(metaclass=ABCMeta):
    """Abstract selector interface.

    Subclasses implement register(), unregister(), select() and get_map();
    modify() defaults to unregister() + register().
    """

    @abstractmethod
    def register(self, fileobj, events, data=None):
        """Start monitoring *fileobj* for *events*; return its SelectorKey.

        Raises ValueError for invalid events or file objects and KeyError if
        the file object is already registered.
        """
        raise NotImplementedError

    @abstractmethod
    def unregister(self, fileobj):
        """Stop monitoring *fileobj*; return its SelectorKey.

        Raises KeyError if it is not registered.  A closed file object must
        be unregistered before (or right after) being closed.
        """
        raise NotImplementedError

    def modify(self, fileobj, events, data=None):
        """Change the monitored events and/or data of a registered object."""
        self.unregister(fileobj)
        return self.register(fileobj, events, data)

    @abstractmethod
    def select(self, timeout=None):
        """Wait until some registered file objects become ready.

        *timeout* None blocks indefinitely, <= 0 polls without blocking,
        otherwise it is the maximum wait in seconds.  Returns a list of
        (key, events) pairs.
        """
        raise NotImplementedError

    def close(self):
        """Release the resources held by the selector."""
        pass

    def get_key(self, fileobj):
        """Return the SelectorKey of a registered file object."""
        mapping = self.get_map()
        if mapping is None:
            raise RuntimeError('Selector is closed')
        try:
            return mapping[fileobj]
        except KeyError:
            raise KeyError("{!r} is not registered".format(fileobj)) from None

    @abstractmethod
    def get_map(self):
        """Return a mapping of file objects to selector keys."""
        raise NotImplementedError

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class _BaseSelectorImpl(BaseSelector):
    """Common bookkeeping for the concrete selectors."""

    def __init__(self):
        self._fd_to_key = {}
        self._map = _SelectorMapping(self)

    def _fileobj_lookup(self, fileobj):
        """Like _fileobj_to_fd(), but also finds closed file objects that are
        still registered (their fileno() no longer works)."""
        try:
            return _fileobj_to_fd(fileobj)
        except ValueError:
            for key in self._fd_to_key.values():
                if key.fileobj is fileobj:
                    return key.fd
            raise

    def register(self, fileobj, events, data=None):
        if (not events) or (events & ~(EVENT_READ | EVENT_WRITE)):
            raise ValueError("Invalid events: {!r}".format(events))

        key = SelectorKey(fileobj, self._fileobj_lookup(fileobj), events, data)

        if key.fd in self._fd_to_key:
            raise KeyError("{!r} (FD {}) is already registered"
                           .format(fileobj, key.fd))

        self._fd_to_key[key.fd] = key
        return key

    def unregister(self, fileobj):
        try:
            key = self._fd_to_key.pop(self._fileobj_lookup(fileobj))
        except KeyError:
            raise KeyError("{!r} is not registered".format(fileobj)) from None
        return key

    def modify(self, fileobj, events, data=None):
        try:
            key = self._fd_to_key[self._fileobj_lookup(fileobj)]
        except KeyError:
            raise KeyError("{!r} is not registered".format(fileobj)) from None
        if events != key.events:
            self.unregister(fileobj)
            key = self.register(fileobj, events, data)
        elif data != key.data:
            key = key._replace(data=data)
            self._fd_to_key[key.fd] = key
        return key

    def close(self):
        self._fd_to_key.clear()
        self._map = None

    def get_map(self):
        return self._map

    def _key_from_fd(self, fd):
        """Return the key registered for *fd*, or None."""
        try:
            return self._fd_to_key[fd]
        except KeyError:
            return None


class SelectSelector(_BaseSelectorImpl):
    """Selector based on select.select()."""

    def __init__(self):
        super().__init__()
        self._readers = set()
        self._writers = set()

    def register(self, fileobj, events, data=None):
        key = super().register(fileobj, events, data)
        if events & EVENT_READ:
            self._readers.add(key.fd)
        if events & EVENT_WRITE:
            self._writers.add(key.fd)
        return key

    def unregister(self, fileobj):
        key = super().unregister(fileobj)
        self._readers.discard(key.fd)
        self._writers.discard(key.fd)
        return key

    if sys.platform == 'win32':
        def _select(self, r, w, _, timeout=None):
            r, w, x = select.select(r, w, w, timeout)
            return r, w + x, []
    else:
        _select = select.select

    def select(self, timeout=None):
        timeout = None if timeout is None else max(timeout, 0)
        ready = []
        try:
            r, w, _ = self._select(self._readers, self._writers, [], timeout)
        except InterruptedError:
            return ready
        r = frozenset(r)
        w = frozenset(w)
        get = self._fd_to_key.get
        for fd in r | w:
            key = get(fd)
            if key:
                events = ((fd in r and EVENT_READ)
                          | (fd in w and EVENT_WRITE))
                ready.append((key, events & key.events))
        return ready


class _PollLikeSelector(_BaseSelectorImpl):
    """Shared implementation of the poll()/epoll()/devpoll() selectors."""
    _selector_cls = None
    _EVENT_READ = None
    _EVENT_WRITE = None

    def __init__(self):
        super().__init__()
        self._selector = self._selector_cls()

    def _native_events(self, events):
        return ((events & EVENT_READ and self._EVENT_READ)
                | (events & EVENT_WRITE and self._EVENT_WRITE))

    def register(self, fileobj, events, data=None):
        key = super().register(fileobj, events, data)
        try:
            self._selector.register(key.fd, self._native_events(events))
        except:
            super().unregister(fileobj)
            raise
        return key

    def unregister(self, fileobj):
        key = super().unregister(fileobj)
        try:
            self._selector.unregister(key.fd)
        except OSError:
            # the fd may have been closed already
            pass
        return key

    def modify(self, fileobj, events, data=None):
        try:
            key = self._fd_to_key[self._fileobj_lookup(fileobj)]
        except KeyError:
            raise KeyError(f"{fileobj!r} is not registered") from None

        changed = False
        if events != key.events:
            try:
                self._selector.modify(key.fd, self._native_events(events))
            except:
                super().unregister(fileobj)
                raise
            changed = True
        if data != key.data:
            changed = True

        if changed:
            key = key._replace(events=events, data=data)
            self._fd_to_key[key.fd] = key
        return key

    def select(self, timeout=None):
        # poll() takes milliseconds; round up so we never wake up early
        if timeout is None:
            timeout = None
        elif timeout <= 0:
            timeout = 0
        else:
            timeout = math.ceil(timeout * 1e3)
        ready = []
        try:
            fd_event_list = self._selector.poll(timeout)
        except InterruptedError:
            return ready
        get = self._fd_to_key.get
        for fd, event in fd_event_list:
            key = get(fd)
            if key:
                events = ((event & ~self._EVENT_READ and EVENT_WRITE)
                          | (event & ~self._EVENT_WRITE and EVENT_READ))
                ready.append((key, events & key.events))
        return ready


if hasattr(select, 'poll'):

    class PollSelector(_PollLikeSelector):
        """Selector based on select.poll()."""
        _selector_cls = select.poll
        _EVENT_READ = select.POLLIN
        _EVENT_WRITE = select.POLLOUT


if hasattr(select, 'epoll'):

    _NOT_EPOLLIN = ~select.EPOLLIN
    _NOT_EPOLLOUT = ~select.EPOLLOUT

    class EpollSelector(_PollLikeSelector):
        """Selector based on select.epoll()."""
        _selector_cls = select.epoll
        _EVENT_READ = select.EPOLLIN
        _EVENT_WRITE = select.EPOLLOUT

        def fileno(self):
            return self._selector.fileno()

        def select(self, timeout=None):
            if timeout is None:
                timeout = -1
            elif timeout <= 0:
                timeout = 0
            else:
                # epoll_wait() has millisecond resolution: round up
                timeout = math.ceil(timeout * 1e3) * 1e-3

            # epoll_wait() needs maxevents > 0 even with nothing registered
            max_ev = max(len(self._fd_to_key), 1)

            ready = []
            try:
                fd_event_list = self._selector.poll(timeout, max_ev)
            except InterruptedError:
                return ready

            get = self._fd_to_key.get
            for fd, event in fd_event_list:
                key = get(fd)
                if key:
                    events = ((event & _NOT_EPOLLIN and EVENT_WRITE)
                              | (event & _NOT_EPOLLOUT and EVENT_READ))
                    ready.append((key, events & key.events))
            return ready

        def close(self):
            self._selector.close()
            super().close()


if hasattr(select, 'devpoll'):

    class DevpollSelector(_PollLikeSelector):
        """Solaris /dev/poll selector."""
        _selector_cls = select.devpoll
        _EVENT_READ = select.POLLIN
        _EVENT_WRITE = select.POLLOUT

        def fileno(self):
            return self._selector.fileno()

        def close(self):
            self._selector.close()
            super().close()


if hasattr(select, 'kqueue'):

    class KqueueSelector(_BaseSelectorImpl):
        """Selector based on select.kqueue()."""

        def __init__(self):
            super().__init__()
            self._selector = select.kqueue()
            self._max_events = 0

        def fileno(self):
            return self._selector.fileno()

        def register(self, fileobj, events, data=None):
            key = super().register(fileobj, events, data)
            try:
                if events & EVENT_READ:
                    kev = select.kevent(key.fd, select.KQ_FILTER_READ,
                                        select.KQ_EV_ADD)
                    self._selector.control([kev], 0, 0)
                    self._max_events += 1
                if events & EVENT_WRITE:
                    kev = select.kevent(key.fd, select.KQ_FILTER_WRITE,
                                        select.KQ_EV_ADD)
                    self._selector.control([kev], 0, 0)
                    self._max_events += 1
            except:
                super().unregister(fileobj)
                raise
            return key

        def unregister(self, fileobj):
            key = super().unregister(fileobj)
            if key.events & EVENT_READ:
                kev = select.kevent(key.fd, select.KQ_FILTER_READ,
                                    select.KQ_EV_DELETE)
                self._max_events -= 1
                try:
                    self._selector.control([kev], 0, 0)
                except OSError:
                    pass
            if key.events & EVENT_WRITE:
                kev = select.kevent(key.fd, select.KQ_FILTER_WRITE,
                                    select.KQ_EV_DELETE)
                self._max_events -= 1
                try:
                    self._selector.control([kev], 0, 0)
                except OSError:
                    pass
            return key

        def select(self, timeout=None):
            timeout = None if timeout is None else max(timeout, 0)
            max_ev = self._max_events or 1
            ready = []
            try:
                kev_list = self._selector.control(None, max_ev, timeout)
            except InterruptedError:
                return ready
            get = self._fd_to_key.get
            for kev in kev_list:
                fd = kev.ident
                flag = kev.filter
                events = 0
                if flag == select.KQ_FILTER_READ:
                    events |= EVENT_READ
                if flag == select.KQ_FILTER_WRITE:
                    events |= EVENT_WRITE
                key = get(fd)
                if key:
                    ready.append((key, events & key.events))
            return ready

        def close(self):
            self._selector.close()
            super().close()


def _can_use(method):
    """Check that select.<method> exists and actually works here."""
    selector = getattr(select, method, None)
    if selector is None:
        return False
    try:
        selector_obj = selector()
        if method == 'poll':
            selector_obj.poll(0)
        else:
            selector_obj.close()
        return True
    except OSError:
        return False


# Prefer kqueue, then epoll, /dev/poll, poll and finally select.
if _can_use('kqueue'):
    DefaultSelector = KqueueSelector
elif _can_use('epoll'):
    DefaultSelector = EpollSelector
elif _can_use('devpoll'):
    DefaultSelector = DevpollSelector
elif _can_use('poll'):
    DefaultSelector = PollSelector
else:
    DefaultSelector = SelectSelector
