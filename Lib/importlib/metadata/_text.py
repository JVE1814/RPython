"""Case-insensitive strings for metadata header names."""


class FoldedCase(str):
    """A str that compares, hashes and searches case-insensitively,
    while keeping its original spelling for display.

    >>> FoldedCase('Requires-Dist') == 'requires-dist'
    True
    >>> 'requires-dist' in [FoldedCase('Requires-Dist')]
    True
    >>> FoldedCase('Name') in {'name': 1}
    False
    """

    def _folded(self):
        return str.casefold(self)

    def __eq__(self, other):
        if not isinstance(other, str):
            return NotImplemented
        return self._folded() == str.casefold(other)

    def __ne__(self, other):
        if not isinstance(other, str):
            return NotImplemented
        return self._folded() != str.casefold(other)

    def __lt__(self, other):
        return self._folded() < str.casefold(other)

    def __gt__(self, other):
        return self._folded() > str.casefold(other)

    def __hash__(self):
        return hash(self._folded())

    def __contains__(self, other):
        return str.casefold(other) in self._folded()

    def lower(self):
        return str.lower(self)

    def in_(self, other):
        """True if this string is contained in *other*, ignoring case."""
        return self in FoldedCase(other)

    def index(self, sub, *args):
        return self._folded().index(str.casefold(sub), *args)

    def split(self, splitter=' ', maxsplit=-1):
        """Split on *splitter* ignoring its case."""
        folded = self._folded()
        sep = str.casefold(splitter)
        pieces = []
        start = 0
        while maxsplit < 0 or len(pieces) < maxsplit:
            pos = folded.find(sep, start)
            if pos < 0:
                break
            pieces.append(str(self)[start:pos])
            start = pos + len(sep)
        pieces.append(str(self)[start:])
        return pieces
