"""Readiness notification for file objects, on top of the select module.

Register file objects (anything with fileno(), or a plain descriptor) for
EVENT_READ / EVENT_WRITE and call select() to learn which became ready.
DefaultSelector names the best implementation for this platform.
"""

from abc import ABCMeta, abstractmethod
from collections import namedtuple
from collections.abc import Mapping
import math
import select
import sys


EVENT_READ = 1
EVENT_WRITE = 2
_ALL_EVENTS = EVENT_READ | EVENT_WRITE


def _fileobj_to_fd(fileobj):
    """Descriptor number of *fileobj*; ValueError if there is none."""
    if isinstance(fileobj, int):
        fd = fileobj
    else:
        try:
            fd = int(fileobj.fileno())
        except (AttributeError, TypeError, ValueError):
            raise ValueError(f"Invalid file object: {fileobj!r}") from None
    if fd < 0:
        raise ValueError(f"Invalid file descriptor: {fd}")
    return fd


SelectorKey = namedtuple('SelectorKey', 'fileobj fd events data')
SelectorKey.__doc__ = ("SelectorKey(fileobj, fd, events, data): registration "
                       "record of a file object.")
SelectorKey.fileobj.__doc__ = 'The registered file object.'
SelectorKey.fd.__doc__ = 'Its file descriptor.'
SelectorKey.events.__doc__ = 'Mask of the events being watched.'
SelectorKey.data.__doc__ = 'User data attached at registration.'


class _KeyView(Mapping):
    """Live read-only mapping file object -> SelectorKey of a selector."""

    __slots__ = ('_owner',)

    def __init__(self, owner):
        self._owner = owner

    def __len__(self):
        return len(self._owner._keys)

    def __iter__(self):
        return iter(self._owner._keys)

    def __getitem__(self, fileobj):
        key = self._owner._keys.get(self._owner._fd_for(fileobj))
        if key is None:
            raise KeyError(f"{fileobj!r} is not registered")
        return key

    def get(self, fileobj, default=None):
        return self._owner._keys.get(self._owner._fd_for(fileobj), default)


class BaseSelector(metaclass=ABCMeta):
    """Interface of all selectors.

    Concrete classes provide register(), unregister(), select() and
    get_map(); modify() and get_key() have generic implementations.
    """

    @abstractmethod
    def register(self, fileobj, events, data=None):
        """Watch *fileobj* for *events*; return the new SelectorKey.

        ValueError for bad events or file objects, KeyError if already
        registered.
        """
        raise NotImplementedError

    @abstractmethod
    def unregister(self, fileobj):
        """Stop watching *fileobj* and return its key (KeyError if unknown)."""
        raise NotImplementedError

    def modify(self, fileobj, events, data=None):
        """Change the events and/or data of a registered file object."""
        self.unregister(fileobj)
        return self.register(fileobj, events, data)

    @abstractmethod
    def select(self, timeout=None):
        """Wait for readiness; return a list of (key, ready_events).

        timeout None waits forever, <= 0 only polls.
        """
        raise NotImplementedError

    def close(self):
        """Free the selector's resources."""

    def get_key(self, fileobj):
        """Return the key of a registered file object."""
        mapping = self.get_map()
        if mapping is None:
            raise RuntimeError('Selector is closed')
        try:
            return mapping[fileobj]
        except KeyError:
            raise KeyError(f"{fileobj!r} is not registered") from None

    @abstractmethod
    def get_map(self):
        """Mapping of registered file objects to their keys."""
        raise NotImplementedError

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()


class _RegistrySelector(BaseSelector):
    """Bookkeeping shared by the concrete selectors.

    Subclasses translate registrations to the OS facility through the
    hooks _os_add / _os_change / _os_remove and implement _os_wait, which
    returns (fd, ready_events) pairs in EVENT_* terms.
    """

    def __init__(self):
        self._keys = {}            # fd -> SelectorKey
        self._map = _KeyView(self)

    # -- hooks
    def _os_add(self, fd, events):
        pass

    def _os_change(self, fd, events):
        self._os_remove(fd)
        self._os_add(fd, events)

    def _os_remove(self, fd):
        pass

    def _os_wait(self, timeout):
        raise NotImplementedError

    # -- helpers
    def _fd_for(self, fileobj):
        try:
            return _fileobj_to_fd(fileobj)
        except ValueError:
            # a registered object may already be closed: find it by identity
            for key in self._keys.values():
                if key.fileobj is fileobj:
                    return key.fd
            raise

    _fileobj_lookup = _fd_for

    def _key_from_fd(self, fd):
        return self._keys.get(fd)

    # -- public API
    def register(self, fileobj, events, data=None):
        if not events or events & ~_ALL_EVENTS:
            raise ValueError(f"Invalid events: {events!r}")
        fd = self._fd_for(fileobj)
        if fd in self._keys:
            raise KeyError(f"{fileobj!r} (FD {fd}) is already registered")
        key = SelectorKey(fileobj, fd, events, data)
        self._keys[fd] = key
        try:
            self._os_add(fd, events)
        except BaseException:
            del self._keys[fd]
            raise
        return key

    def unregister(self, fileobj):
        fd = self._fd_for(fileobj)
        key = self._keys.pop(fd, None)
        if key is None:
            raise KeyError(f"{fileobj!r} is not registered")
        try:
            self._os_remove(fd)
        except OSError:
            # e.g. the descriptor was closed before being unregistered
            pass
        return key

    def modify(self, fileobj, events, data=None):
        fd = self._fd_for(fileobj)
        old = self._keys.get(fd)
        if old is None:
            raise KeyError(f"{fileobj!r} is not registered")
        if not events or events & ~_ALL_EVENTS:
            raise ValueError(f"Invalid events: {events!r}")
        if events != old.events:
            try:
                self._os_change(fd, events)
            except BaseException:
                # the OS state is unknown now: drop the registration
                self._keys.pop(fd, None)
                try:
                    self._os_remove(fd)
                except BaseException:
                    pass
                raise
        if events == old.events and data == old.data:
            return old
        key = old._replace(events=events, data=data)
        self._keys[fd] = key
        return key

    def select(self, timeout=None):
        if timeout is not None and timeout < 0:
            timeout = 0
        try:
            ready = self._os_wait(timeout)
        except InterruptedError:
            return []
        result = []
        keys = self._keys
        for fd, events in ready:
            key = keys.get(fd)
            if key is not None:
                result.append((key, events & key.events))
        return result

    def close(self):
        self._keys.clear()
        self._map = None

    def get_map(self):
        return self._map


class SelectSelector(_RegistrySelector):
    """Selector using select.select()."""

    def __init__(self):
        super().__init__()
        self._readers = set()
        self._writers = set()

    def _os_add(self, fd, events):
        if events & EVENT_READ:
            self._readers.add(fd)
        if events & EVENT_WRITE:
            self._writers.add(fd)

    def _os_remove(self, fd):
        self._readers.discard(fd)
        self._writers.discard(fd)

    def _select(self, r, w, x, timeout=None):
        if sys.platform == 'win32':
            # Windows reports failed connects as exceptional conditions
            r, w, x = select.select(r, w, w, timeout)
            return r, w + x, []
        return select.select(r, w, x, timeout)

    def _os_wait(self, timeout):
        r, w, _ = self._select(self._readers, self._writers, [], timeout)
        flags = {}
        for fd in r:
            flags[fd] = flags.get(fd, 0) | EVENT_READ
        for fd in w:
            flags[fd] = flags.get(fd, 0) | EVENT_WRITE
        return list(flags.items())


class _PollStyleSelector(_RegistrySelector):
    """Selectors backed by an object with register/modify/unregister/poll."""

    _selector_cls = None
    _IN = 0
    _OUT = 0

    def __init__(self):
        super().__init__()
        self._selector = self._selector_cls()

    def _native(self, events):
        mask = 0
        if events & EVENT_READ:
            mask |= self._IN
        if events & EVENT_WRITE:
            mask |= self._OUT
        return mask

    def _translate(self, native):
        # anything but "readable" counts as writable and vice versa, so that
        # error/hang-up conditions wake up whoever is waiting
        events = 0
        if native & ~self._IN:
            events |= EVENT_WRITE
        if native & ~self._OUT:
            events |= EVENT_READ
        return events

    def _os_add(self, fd, events):
        self._selector.register(fd, self._native(events))

    def _os_change(self, fd, events):
        self._selector.modify(fd, self._native(events))

    def _os_remove(self, fd):
        self._selector.unregister(fd)

    def _poll_timeout(self, timeout):
        # poll() wants milliseconds; round up so we never return early
        if timeout is None:
            return None
        return math.ceil(timeout * 1e3) if timeout > 0 else 0

    def _os_wait(self, timeout):
        pairs = self._selector.poll(self._poll_timeout(timeout))
        return [(fd, self._translate(ev)) for fd, ev in pairs]


if hasattr(select, 'poll'):

    class PollSelector(_PollStyleSelector):
        """Selector using select.poll()."""
        _selector_cls = select.poll
        _IN = select.POLLIN
        _OUT = select.POLLOUT


if hasattr(select, 'epoll'):

    class EpollSelector(_PollStyleSelector):
        """Selector using select.epoll()."""
        _selector_cls = select.epoll
        _IN = select.EPOLLIN
        _OUT = select.EPOLLOUT

        def fileno(self):
            return self._selector.fileno()

        def _os_wait(self, timeout):
            if timeout is None:
                seconds = -1
            elif timeout <= 0:
                seconds = 0
            else:
                # epoll has millisecond granularity: round up
                seconds = math.ceil(timeout * 1e3) * 1e-3
            pairs = self._selector.poll(seconds, max(len(self._keys), 1))
            return [(fd, self._translate(ev)) for fd, ev in pairs]

        def close(self):
            self._selector.close()
            super().close()


if hasattr(select, 'devpoll'):

    class DevpollSelector(_PollStyleSelector):
        """Selector using Solaris /dev/poll."""
        _selector_cls = select.devpoll
        _IN = select.POLLIN
        _OUT = select.POLLOUT

        def fileno(self):
            return self._selector.fileno()

        def close(self):
            self._selector.close()
            super().close()


if hasattr(select, 'kqueue'):

    class KqueueSelector(_RegistrySelector):
        """Selector using select.kqueue() (BSD/macOS)."""

        def __init__(self):
            super().__init__()
            self._selector = select.kqueue()
            self._nfilters = 0

        def fileno(self):
            return self._selector.fileno()

        def _filters(self, events):
            if events & EVENT_READ:
                yield select.KQ_FILTER_READ
            if events & EVENT_WRITE:
                yield select.KQ_FILTER_WRITE

        def _os_add(self, fd, events):
            for flt in self._filters(events):
                self._selector.control(
                    [select.kevent(fd, flt, select.KQ_EV_ADD)], 0, 0)
                self._nfilters += 1

        def _os_change(self, fd, events):
            old = self._keys[fd].events
            self._kq_remove(fd, old)
            self._os_add(fd, events)

        def _kq_remove(self, fd, events):
            for flt in self._filters(events):
                self._nfilters -= 1
                try:
                    self._selector.control(
                        [select.kevent(fd, flt, select.KQ_EV_DELETE)], 0, 0)
                except OSError:
                    pass

        def unregister(self, fileobj):
            key = super().unregister(fileobj)
            self._kq_remove(key.fd, key.events)
            return key

        def _os_wait(self, timeout):
            out = []
            for kev in self._selector.control(None, self._nfilters or 1, timeout):
                ev = EVENT_READ if kev.filter == select.KQ_FILTER_READ else 0
                if kev.filter == select.KQ_FILTER_WRITE:
                    ev |= EVENT_WRITE
                out.append((kev.ident, ev))
            return out

        def close(self):
            self._selector.close()
            super().close()


def _works(name):
    factory = getattr(select, name, None)
    if factory is None:
        return False
    try:
        obj = factory()
        if name == 'poll':
            obj.poll(0)
        else:
            obj.close()
    except OSError:
        return False
    return True


for _name, _cls in (('kqueue', 'KqueueSelector'), ('epoll', 'EpollSelector'),
                    ('devpoll', 'DevpollSelector'), ('poll', 'PollSelector')):
    if _works(_name):
        DefaultSelector = globals()[_cls]
        break
else:
    DefaultSelector = SelectSelector
del _name, _cls
