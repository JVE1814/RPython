"""Pure-Python implementation of thread-local data (threading.local).

Each `local` instance keeps one attribute dictionary per thread.  Attribute
access swaps the calling thread's dictionary in as the instance __dict__
(under a lock).  A thread's dictionary is created on first access, running
the subclass' __init__ with the arguments given at construction.

Dictionaries go away with their thread (a weak reference to the thread
object removes them) and the per-thread bookkeeping goes away with the
local object (a weak reference to it, stored in the thread's __dict__).
"""

from weakref import ref
from contextlib import contextmanager

__all__ = ["local"]


class _localimpl:
    """Per-thread dictionaries of one `local` object."""

    __slots__ = 'key', 'dicts', 'localargs', 'locallock', '__weakref__'

    def __init__(self):
        # key under which the thread object references this impl
        self.key = '_threading_local._localimpl.' + str(id(self))
        # id(thread) -> (weak reference to the thread, its dict)
        self.dicts = {}

    def get_dict(self):
        """The calling thread's dict (KeyError if it has none yet)."""
        thread = current_thread()
        return self.dicts[id(thread)][1]

    def create_dict(self):
        """Create the calling thread's dict."""
        localdict = {}
        key = self.key
        thread = current_thread()
        idt = id(thread)

        def local_deleted(_, key=key):
            # the local object died: drop the thread's reference to it
            thread = wrthread()
            if thread is not None:
                del thread.__dict__[key]

        def thread_deleted(_, idt=idt):
            # the thread died: drop its dict
            local = wrlocal()
            if local is not None:
                local.dicts.pop(idt)

        wrlocal = ref(self, local_deleted)
        wrthread = ref(thread, thread_deleted)
        thread.__dict__[key] = wrlocal
        self.dicts[idt] = wrthread, localdict
        return localdict


@contextmanager
def _patch(self):
    impl = object.__getattribute__(self, '_local__impl')
    try:
        dct = impl.get_dict()
    except KeyError:
        dct = impl.create_dict()
        args, kw = impl.localargs
        self.__init__(*args, **kw)
    with impl.locallock:
        object.__setattr__(self, '__dict__', dct)
        yield


class local:
    """Thread-local data: attributes set on an instance are only visible to
    the thread that set them."""

    __slots__ = '_local__impl', '__dict__'

    def __new__(cls, /, *args, **kw):
        if (args or kw) and (cls.__init__ is object.__init__):
            raise TypeError("Initialization arguments are not supported")
        self = object.__new__(cls)
        impl = _localimpl()
        impl.localargs = (args, kw)
        impl.locallock = RLock()
        object.__setattr__(self, '_local__impl', impl)
        # The creating thread gets its dict now (__init__ is called on it
        # by the normal instance creation protocol).
        impl.create_dict()
        return self

    def __getattribute__(self, name):
        with _patch(self):
            return object.__getattribute__(self, name)

    def __setattr__(self, name, value):
        if name == '__dict__':
            raise AttributeError(
                "%r object attribute '__dict__' is read-only"
                % self.__class__.__name__)
        with _patch(self):
            return object.__setattr__(self, name, value)

    def __delattr__(self, name):
        if name == '__dict__':
            raise AttributeError(
                "%r object attribute '__dict__' is read-only"
                % self.__class__.__name__)
        with _patch(self):
            return object.__delattr__(self, name)


from threading import current_thread, RLock
