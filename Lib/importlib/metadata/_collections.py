"""Small container types used by importlib.metadata."""

import collections


class FreezableDefaultDict(collections.defaultdict):
    """A defaultdict that stops creating entries once frozen; missing keys
    then just produce the default value.

    >>> d = FreezableDefaultDict(list)
    >>> d['a'].append(1)
    >>> d.freeze()
    >>> d['b']
    []
    >>> 'b' in d
    False
    """

    def __missing__(self, key):
        if getattr(self, '_frozen', False):
            return self.default_factory()
        return super().__missing__(key)

    def freeze(self):
        self._frozen = True


class Pair(collections.namedtuple('Pair', 'name value')):
    @classmethod
    def parse(cls, text):
        """Parse 'name = value' (whitespace around both is ignored)."""
        name, _, value = text.partition('=')
        return cls(name.strip(), value.strip())
