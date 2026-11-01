"""robots.txt parser (RFC 9309).

RobotFileParser reads a robots.txt file and answers whether a given user
agent may fetch a URL:

    rp = RobotFileParser('http://example.com/robots.txt')
    rp.read()
    rp.can_fetch('mybot', 'http://example.com/some/page')

Rules support the '*' wildcard and the '$' end anchor; the longest
matching rule wins and Allow wins ties.  Paths are compared after
normalising their percent-encoding.
"""

import collections
import time
import urllib.error
import urllib.parse
import urllib.request

__all__ = ['RobotFileParser']

RequestRate = collections.namedtuple('RequestRate', 'requests seconds')


# --------------------------------------------------------------------------
# path canonicalisation
#
# Paths and rule patterns are turned into strings in which every "unit" is
# one character, so that string search is aligned on units:
#   * unreserved ASCII characters stand for themselves,
#   * the query delimiter '?' and, inside the query, the separators '=' and
#     '&' stand for themselves (they are significant when raw),
#   * every other octet (raw or percent-encoded) becomes a private-use
#     character _OCTET + value.

_OCTET = 0xF000
_UNRESERVED = frozenset('ABCDEFGHIJKLMNOPQRSTUVWXYZ'
                        'abcdefghijklmnopqrstuvwxyz'
                        '0123456789-._~')
_HEX = frozenset('0123456789abcdefABCDEF')


def _units(text, pattern):
    """Yield (kind, value) units: ('raw', char), ('octet', int) or
    ('wild', '*'/'$') for pattern metacharacters."""
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if (ch == "%" and i + 2 < n and text[i + 1] in _HEX and
                text[i + 2] in _HEX):
            yield 'octet', int(text[i + 1:i + 3], 16)
            i += 3
            continue
        if pattern and ch in '*$':
            yield 'wild', ch
        elif ch < '\x80':
            yield 'raw', ch
        else:
            for b in ch.encode('utf-8', 'surrogateescape'):
                yield 'octet', b
        i += 1


def _canonical(text, pattern=False):
    """Return the canonical form of a path (or a list of pieces/markers for
    a pattern: str pieces separated by '*' markers, '$' marker last)."""
    out = []
    piece = []
    in_query = False
    for kind, value in _units(text, pattern):
        if kind == 'wild':
            out.append(''.join(piece))
            piece = []
            out.append(value)
            continue
        if kind == 'raw':
            if value == '?' and not in_query:
                in_query = True
                piece.append('?')
                continue
            if in_query and value in '=&':
                piece.append(value)
                continue
            if value in _UNRESERVED:
                piece.append(value)
                continue
            value = ord(value)
        # an octet
        ch = chr(value)
        if value < 0x80 and ch in _UNRESERVED:
            piece.append(ch)
        else:
            piece.append(chr(_OCTET + value))
    out.append(''.join(piece))
    if pattern:
        return out
    return out[0]


class _Pattern:
    """A compiled rule path: literal pieces separated by wildcards."""

    def __init__(self, path):
        self.valid = True
        tokens = _canonical(path, pattern=True)
        # tokens alternate: piece, marker, piece, marker, ..., piece
        pieces = [tokens[0]]
        anchored = False
        text = []
        for i in range(1, len(tokens), 2):
            marker, following = tokens[i], tokens[i + 1]
            if anchored:
                if following:
                    self.valid = False
                continue
            if marker == '$':
                anchored = True
                if following:
                    self.valid = False
                continue
            # '*': collapse consecutive stars (empty pieces in between)
            if pieces[-1] == '' and len(pieces) > 1:
                pieces[-1] = following
            else:
                pieces.append(following)
        self.pieces = pieces
        self.anchored = anchored
        self.text = self._normalized_text(path)

    @staticmethod
    def _normalized_text(path):
        out = []
        for ch in path:
            if out and ch == '*' and out[-1] == '*':
                continue
            if '$' in out:
                break
            out.append(ch)
        text = ''.join(out)
        if text.endswith('$*'):
            text = text[:-1]
        return text

    def match_length(self, path):
        """Length of the longest match of this pattern as a prefix of
        *path*, or -1 if it does not match."""
        pieces = self.pieces
        first = pieces[0]
        if not path.startswith(first):
            return -1
        if len(pieces) == 1:
            if self.anchored:
                return len(path) if len(path) == len(first) else -1
            return len(first)
        pos = len(first)
        for piece in pieces[1:-1]:
            found = path.find(piece, pos)
            if found < 0:
                return -1
            pos = found + len(piece)
        last = pieces[-1]
        if self.anchored:
            if len(path) - len(last) >= pos and path.endswith(last):
                return len(path)
            return -1
        if not last:
            return len(path)
        found = path.rfind(last, pos)
        if found < 0:
            return -1
        return found + len(last)


class RuleLine:
    """An Allow (allowance true) or Disallow line with its path pattern."""

    def __init__(self, path, allowance):
        if path == '' and not allowance:
            # "Disallow:" with an empty value allows everything
            allowance = True
        self._pattern = _Pattern(path)
        self.path = self._pattern.text
        self.allowance = allowance

    def applies_to(self, filename):
        return self._pattern.valid and \
            self._pattern.match_length(_canonical(filename)) >= 0

    def __str__(self):
        return ('Allow' if self.allowance else 'Disallow') + ': ' + self.path


class Entry:
    """A group: user agents plus the rules and limits that apply to them."""

    def __init__(self):
        self.useragents = []
        self.rulelines = []
        self.delay = None
        self.req_rate = None

    def __str__(self):
        lines = [f'User-agent: {agent}' for agent in self.useragents]
        if self.delay is not None:
            lines.append(f'Crawl-delay: {self.delay}')
        if self.req_rate is not None:
            rate = self.req_rate
            lines.append(f'Request-rate: {rate.requests}/{rate.seconds}')
        printable = [str(r) for r in self.rulelines if r._pattern.valid]
        lines.extend(printable or ['Allow:'])
        return '\n'.join(lines)

    def applies_to(self, useragent):
        """True if this group names the product token of *useragent*."""
        token = _product_token(useragent)
        for agent in self.useragents:
            if agent == '*':
                return True
            if token and agent.lower() == token:
                return True
        return False

    def allowance(self, filename):
        """Whether the rules of this group allow *filename*."""
        return _decide(self.rulelines, filename)


def _product_token(useragent):
    useragent = useragent.strip()
    end = 0
    while end < len(useragent) and (useragent[end].isascii() and
                                    (useragent[end].isalpha() or
                                     useragent[end] in '_-')):
        end += 1
    return useragent[:end].lower()


def _decide(rulelines, path):
    canon = _canonical(path)
    best = -1
    allowed = True
    for rule in rulelines:
        pattern = rule._pattern
        if not pattern.valid:
            continue
        length = pattern.match_length(canon)
        if length < 0:
            continue
        if length > best or (length == best and rule.allowance):
            best = length
            allowed = rule.allowance
    return allowed


class RobotFileParser:
    """Fetch and evaluate a robots.txt file."""

    def __init__(self, url=''):
        self.entries = []
        self.sitemaps = []
        self.default_entry = None
        self.disallow_all = False
        self.allow_all = False
        self.set_url(url)
        self.last_checked = 0

    def mtime(self):
        """Time the robots.txt file was last fetched (0 if never)."""
        return self.last_checked

    def modified(self):
        """Record that the robots.txt file was fetched now."""
        self.last_checked = time.time()

    def set_url(self, url):
        """Set the URL of the robots.txt file."""
        self.url = url
        self.host, self.path = urllib.parse.urlsplit(url)[1:3]

    def read(self):
        """Fetch the robots.txt file and parse it."""
        try:
            response = urllib.request.urlopen(self.url)
        except urllib.error.HTTPError as err:
            if err.code in (401, 403) or err.code >= 500:
                self.disallow_all = True
            elif 400 <= err.code < 500:
                self.allow_all = True
            err.close()
        else:
            with response:
                raw = response.read()
            self.parse(raw.decode('utf-8', 'surrogateescape').splitlines())

    def _add_entry(self, entry):
        if '*' in entry.useragents and self.default_entry is None:
            self.default_entry = entry
        self.entries.append(entry)

    def parse(self, lines):
        """Parse the lines of a robots.txt file."""
        self.modified()
        entry = None
        in_rules = False
        for line in lines:
            comment = line.find('#')
            if comment >= 0:
                line = line[:comment]
            line = line.strip()
            if not line:
                continue
            key, colon, value = line.partition(':')
            if not colon:
                continue
            key = key.strip().lower()
            value = value.strip()
            if key == 'user-agent':
                if entry is None or in_rules:
                    entry = Entry()
                    self._add_entry(entry)
                    in_rules = False
                entry.useragents.append(value)
            elif key in ('allow', 'disallow'):
                if entry is not None:
                    entry.rulelines.append(RuleLine(value, key == 'allow'))
                    in_rules = True
            elif key == 'crawl-delay':
                if entry is not None:
                    if value.isascii() and value.isdigit():
                        entry.delay = int(value)
                    in_rules = True
            elif key == 'request-rate':
                if entry is not None:
                    num, slash, den = value.partition('/')
                    num, den = num.strip(), den.strip()
                    if (slash and num.isascii() and num.isdigit() and
                            den.isascii() and den.isdigit()):
                        entry.req_rate = RequestRate(int(num), int(den))
                    in_rules = True
            elif key == 'sitemap':
                self.sitemaps.append(value)

    def _groups_for(self, useragent):
        token = _product_token(useragent)
        if token:
            exact = [e for e in self.entries
                     if any(a.lower() == token for a in e.useragents)]
            if exact:
                return exact
        return [e for e in self.entries if '*' in e.useragents]

    def can_fetch(self, useragent, url):
        """True if *useragent* may fetch *url*."""
        if self.disallow_all:
            return False
        if self.allow_all:
            return True
        if not self.last_checked:
            return False
        parts = urllib.parse.urlsplit(url, allow_fragments=False)
        path = parts.path or '/'
        if parts.query:
            path += '?' + parts.query
        if path == '/robots.txt':
            return True
        groups = self._groups_for(useragent)
        if not groups:
            return True
        rules = [rule for group in groups for rule in group.rulelines]
        return _decide(rules, path)

    def crawl_delay(self, useragent):
        if not self.mtime():
            return None
        for group in self._groups_for(useragent):
            if group.delay is not None:
                return group.delay
        return None

    def request_rate(self, useragent):
        if not self.mtime():
            return None
        for group in self._groups_for(useragent):
            if group.req_rate is not None:
                return group.req_rate
        return None

    def site_maps(self):
        return self.sitemaps or None

    def __str__(self):
        return '\n\n'.join(str(e) for e in self.entries if e.useragents)
