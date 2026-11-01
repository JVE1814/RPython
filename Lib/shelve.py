"""Persistent dictionaries of arbitrary Python objects ("shelves").

A shelf maps str keys to Python values; values are serialised (with
pickle by default) and stored in a dbm-style mapping of bytes to bytes::

    import shelve
    with shelve.open("inventory") as db:
        db["apples"] = {"count": 3}
        print(db["apples"]["count"])

Values read from a shelf are fresh copies: mutating them does not change
the stored data unless the shelf was opened with ``writeback=True``, which
caches every accessed entry and writes the cache back on sync()/close()
(at the cost of memory and a slower close).

Warning: the default deserialiser is pickle, which can run arbitrary code;
only open shelves that come from a trusted source.
"""

import collections.abc
import pickle

__all__ = ["ShelveError", "Shelf", "BsdDbShelf", "DbfilenameShelf", "open"]


class ShelveError(Exception):
    """Invalid shelf configuration."""


class _ClosedDict(collections.abc.MutableMapping):
    """Stand-in for the backing store of a closed shelf."""

    def _closed(self, *args):
        raise ValueError('invalid operation on closed shelf')

    __iter__ = __len__ = __getitem__ = __setitem__ = __delitem__ = _closed
    keys = _closed

    def __repr__(self):
        return '<Closed Dictionary>'


def _pickle_value(value, protocol):
    return pickle.dumps(value, protocol)


class Shelf(collections.abc.MutableMapping):
    """Shelf over any mapping of bytes keys to bytes values.

    *protocol* is the pickle protocol (pickle.DEFAULT_PROTOCOL when None),
    *keyencoding* encodes the str keys.  *serializer(value, protocol)* and
    *deserializer(data)* replace pickle; they must be given together.
    """

    def __init__(self, dict, protocol=None, writeback=False,
                 keyencoding="utf-8", *, serializer=None, deserializer=None):
        self.dict = dict
        if protocol is None:
            protocol = pickle.DEFAULT_PROTOCOL
        self._protocol = protocol
        self.writeback = writeback
        self.cache = {}
        self.keyencoding = keyencoding
        if (serializer is None) != (deserializer is None):
            raise ShelveError("serializer and deserializer must be given "
                              "together")
        if serializer is None:
            serializer, deserializer = _pickle_value, pickle.loads
        self._serializer = serializer
        self._deserializer = deserializer

    def _encode(self, key):
        return key.encode(self.keyencoding)

    def __iter__(self):
        for raw in self.dict.keys():
            yield raw.decode(self.keyencoding)

    def __len__(self):
        return len(self.dict)

    def __contains__(self, key):
        return self._encode(key) in self.dict

    def get(self, key, default=None):
        if self._encode(key) in self.dict:
            return self[key]
        return default

    def __getitem__(self, key):
        try:
            return self.cache[key]
        except KeyError:
            pass
        value = self._deserializer(self.dict[self._encode(key)])
        if self.writeback:
            self.cache[key] = value
        return value

    def __setitem__(self, key, value):
        if self.writeback:
            self.cache[key] = value
        self.dict[self._encode(key)] = self._serializer(value, self._protocol)

    def __delitem__(self, key):
        del self.dict[self._encode(key)]
        self.cache.pop(key, None)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        self.close()

    def close(self):
        """Write back cached entries and close the backing store."""
        if self.dict is None or isinstance(self.dict, _ClosedDict):
            return
        try:
            self.sync()
        finally:
            closer = getattr(self.dict, 'close', None)
            try:
                if closer is not None:
                    closer()
            finally:
                # later operations raise ValueError instead of reaching a
                # closed database
                self.dict = _ClosedDict()

    def __del__(self):
        if not hasattr(self, 'writeback'):
            return          # __init__ did not finish
        self.close()

    def sync(self):
        """Store the writeback cache and flush the backing store."""
        if self.writeback and self.cache:
            pending, self.cache = self.cache, {}
            self.writeback = False
            try:
                for key, value in pending.items():
                    self[key] = value
            finally:
                self.writeback = True
        flush = getattr(self.dict, 'sync', None)
        if flush is not None:
            flush()

    def reorganize(self):
        """Sync, then let the backing store reclaim unused space if it can."""
        self.sync()
        compact = getattr(self.dict, 'reorganize', None)
        if compact is not None:
            compact()


class BsdDbShelf(Shelf):
    """Shelf over a Berkeley-DB-style mapping that also offers
    first()/last()/next()/previous()/set_location()."""

    def _pair(self, pair):
        key, data = pair
        return key.decode(self.keyencoding), self._deserializer(data)

    def set_location(self, key):
        return self._pair(self.dict.set_location(key))

    def next(self):
        return self._pair(next(self.dict))

    def previous(self):
        return self._pair(self.dict.previous())

    def first(self):
        return self._pair(self.dict.first())

    def last(self):
        return self._pair(self.dict.last())


class DbfilenameShelf(Shelf):
    """Shelf stored in a dbm database file (see dbm.open() for *flag*)."""

    def __init__(self, filename, flag='c', protocol=None, writeback=False, *,
                 serializer=None, deserializer=None):
        import dbm
        Shelf.__init__(self, dbm.open(filename, flag), protocol, writeback,
                       serializer=serializer, deserializer=deserializer)


def open(filename, flag='c', protocol=None, writeback=False, *,
         serializer=None, deserializer=None):
    """Open a shelf stored in the dbm database *filename*.

    *flag* is passed to dbm.open() ('c' creates the database when needed);
    the other arguments are those of Shelf.
    """
    return DbfilenameShelf(filename, flag, protocol, writeback,
                           serializer=serializer, deserializer=deserializer)
