"""Lenient parsing of RFC 2822 addresses and dates (the legacy parser used by
email.utils)."""

__all__ = [
    'mktime_tz',
    'parsedate',
    'parsedate_tz',
    'quote',
]

import time

SPACE = ' '
EMPTYSTRING = ''
COMMASPACE = ', '

_MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep',
           'oct', 'nov', 'dec',
           'january', 'february', 'march', 'april', 'may', 'june', 'july',
           'august', 'september', 'october', 'november', 'december']
_DAYS = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun']

# Time zone names accepted in dates, as +-HHMM values.
_ZONES = {
    'UT': 0, 'UTC': 0, 'GMT': 0, 'Z': 0,
    'AST': -400, 'ADT': -300,
    'EST': -500, 'EDT': -400,
    'CST': -600, 'CDT': -500,
    'MST': -700, 'MDT': -600,
    'PST': -800, 'PDT': -700,
}


def parsedate_tz(data):
    """Parse a date string; return a 10-tuple (the last item being the UTC
    offset in seconds, or None) or None if the string is not a date."""
    result = _parsedate_tz(data)
    if not result:
        return None
    if result[9] is None:
        result[9] = 0
    return tuple(result)


def _split_time(text):
    for sep in (':', '.'):
        if sep == '.' and ':' in text:
            break
        parts = text.split(sep)
        if sep == ':' and len(parts) == 1:
            continue
        if len(parts) == 2:
            return parts[0], parts[1], '0'
        if len(parts) == 3:
            return parts[0], parts[1], parts[2]
        return None
    return None


def _parsedate_tz(data):
    """Like parsedate_tz() but returns a list, with None as the offset when
    the date specifies none (or '-0000')."""
    if not data:
        return None
    fields = data.split()
    if not fields:
        return None
    # An optional leading day of the week, with or without a comma.
    head = fields[0]
    if head.endswith(',') or head.lower() in _DAYS:
        del fields[0]
    elif ',' in head:
        fields[0] = head[head.rindex(',') + 1:]
    # RFC 850 style: 19-Nov-82
    if len(fields) == 3:
        pieces = fields[0].split('-')
        if len(pieces) == 3:
            fields = pieces + fields[1:]
    # A zone glued to the time: 14:58:26+0800
    if len(fields) == 4:
        last = fields[3]
        cut = last.find('+')
        if cut == -1:
            cut = last.find('-')
        if cut > 0:
            fields[3:] = [last[:cut], last[cut:]]
        else:
            fields.append('')
    if len(fields) < 5:
        return None
    day, month, year, clock, zone = fields[:5]
    if not (day and month and year):
        return None
    month = month.lower()
    if month not in _MONTHS:
        day, month = month, day.lower()
        if month not in _MONTHS:
            return None
    month = _MONTHS.index(month) % 12 + 1
    if day.endswith(','):
        day = day[:-1]
    if year.find(':') > 0:
        year, clock = clock, year
    if year.endswith(','):
        year = year[:-1]
    if not year:
        return None
    if not year[0].isdigit():
        year, zone = zone, year
    if clock.endswith(','):
        clock = clock[:-1]
    hms = _split_time(clock)
    if hms is None:
        return None
    try:
        year = int(year)
        day = int(day)
        hour, minute, second = (int(x) for x in hms)
    except ValueError:
        return None
    if year < 100:
        year += 1900 if year > 68 else 2000
    zone = zone.upper()
    offset = _ZONES.get(zone)
    if offset is None:
        try:
            offset = int(zone)
        except ValueError:
            offset = None
        if offset == 0 and zone.startswith('-'):
            offset = None
    if offset:
        sign = -1 if offset < 0 else 1
        offset = abs(offset)
        offset = sign * ((offset // 100) * 3600 + (offset % 100) * 60)
    return [year, month, day, hour, minute, second, 0, 1, -1, offset]


def parsedate(data):
    """Parse a date string into a 9-tuple usable with time.mktime(), or
    None."""
    t = parsedate_tz(data)
    if isinstance(t, tuple):
        return t[:9]
    return t


def _days_from_civil(year, month, day):
    # Days since 1970-01-01 in the proleptic Gregorian calendar.
    year -= month <= 2
    era = (year if year >= 0 else year - 399) // 400
    yoe = year - era * 400
    doy = (153 * (month + (-3 if month > 2 else 9)) + 2) // 5 + day - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def _timegm(t):
    days = _days_from_civil(t[0], t[1], 1) + t[2] - 1
    return ((days * 24 + t[3]) * 60 + t[4]) * 60 + t[5]


def mktime_tz(data):
    """Turn a 10-tuple from parsedate_tz() into a POSIX timestamp."""
    if data[9] is None:
        return time.mktime(data[:8] + (-1,))
    return _timegm(data) - data[9]


def quote(str):
    """Escape backslashes and double quotes for a quoted-string."""
    return str.replace('\\', '\\\\').replace('"', '\\"')


class AddrlistClass:
    """Parser for a list of RFC 2822 addresses.

    The field may also be a list of strings (each then acts as one opaque
    atom).  getaddrlist() returns (realname, address) pairs.
    """

    specials = '()<>@,:;.\"[]'
    LWS = ' \t'
    CR = '\r\n'
    FWS = LWS + CR
    atomends = specials + LWS + CR
    # periods are allowed inside phrases (obs-phrase)
    phraseends = atomends.replace('.', '')

    def __init__(self, field):
        self.field = field
        self.pos = 0
        self.commentlist = []

    # -- helpers ------------------------------------------------------------

    def _at(self, chars):
        return self.pos < len(self.field) and self.field[self.pos] in chars

    def gotonext(self):
        """Skip whitespace and comments (which are collected); return the
        skipped spaces and tabs."""
        spaces = []
        field = self.field
        while self.pos < len(field):
            ch = field[self.pos]
            if ch in self.LWS + '\n\r':
                if ch not in '\n\r':
                    spaces.append(ch)
                self.pos += 1
            elif ch == '(':
                self.commentlist.append(self.getcomment())
            else:
                break
        return EMPTYSTRING.join(spaces)

    def getdelimited(self, beginchar, endchars, allowcomments=True):
        """Read text between *beginchar* and one of *endchars*, handling
        backslash escapes (and nested comments if *allowcomments*)."""
        field = self.field
        if field[self.pos] != beginchar:
            return ''
        out = ['']
        self.pos += 1
        escaped = False
        while self.pos < len(field):
            ch = field[self.pos]
            if escaped:
                out.append(ch)
                escaped = False
            elif ch in endchars:
                self.pos += 1
                break
            elif allowcomments and ch == '(':
                out.append(self.getcomment())
                continue
            elif ch == '\\':
                escaped = True
            else:
                out.append(ch)
            self.pos += 1
        return EMPTYSTRING.join(out)

    def getquote(self):
        return self.getdelimited('"', '"\r', False)

    def getcomment(self):
        return self.getdelimited('(', ')\r', True)

    def getdomainliteral(self):
        return '[%s]' % self.getdelimited('[', ']\r', False)

    def getatom(self, atomends=None):
        """Read characters up to one of *atomends*."""
        if atomends is None:
            atomends = self.atomends
        field = self.field
        out = ['']
        while self.pos < len(field) and field[self.pos] not in atomends:
            out.append(field[self.pos])
            self.pos += 1
        return EMPTYSTRING.join(out)

    def getphraselist(self):
        """Read a phrase: a list of words (atoms and quoted strings)."""
        words = []
        field = self.field
        while self.pos < len(field):
            ch = field[self.pos]
            if ch in self.FWS:
                self.pos += 1
            elif ch == '"':
                words.append(self.getquote())
            elif ch == '(':
                self.commentlist.append(self.getcomment())
            elif ch in self.phraseends:
                break
            else:
                words.append(self.getatom(self.phraseends))
        return words

    def getdomain(self):
        """Read a domain; '' if it contains a second '@'."""
        parts = []
        field = self.field
        while self.pos < len(field):
            ch = field[self.pos]
            if ch in self.LWS:
                self.pos += 1
            elif ch == '(':
                self.commentlist.append(self.getcomment())
            elif ch == '[':
                parts.append(self.getdomainliteral())
            elif ch == '.':
                self.pos += 1
                parts.append('.')
            elif ch == '@':
                return EMPTYSTRING
            elif ch in self.atomends:
                break
            else:
                parts.append(self.getatom())
        return EMPTYSTRING.join(parts)

    def getaddrspec(self):
        """Read local-part@domain."""
        local = []
        field = self.field

        def drop_trailing_space():
            if local and not local[-1].strip():
                local.pop()

        self.gotonext()
        while self.pos < len(field):
            keep_space = True
            ch = field[self.pos]
            if ch == '.':
                drop_trailing_space()
                local.append('.')
                self.pos += 1
                keep_space = False
            elif ch == '"':
                local.append('"%s"' % quote(self.getquote()))
            elif ch in self.atomends:
                drop_trailing_space()
                break
            else:
                local.append(self.getatom())
            space = self.gotonext()
            if keep_space and space:
                local.append(space)
        if not self._at('@'):
            return EMPTYSTRING.join(local)
        local.append('@')
        self.pos += 1
        self.gotonext()
        domain = self.getdomain()
        if not domain:
            return EMPTYSTRING
        return EMPTYSTRING.join(local) + domain

    def getrouteaddr(self):
        """Read an angle-bracketed address, skipping any source route."""
        field = self.field
        if field[self.pos] != '<':
            return None
        self.pos += 1
        self.gotonext()
        address = ''
        route_domain_next = False
        while self.pos < len(field):
            ch = field[self.pos]
            if route_domain_next:
                self.getdomain()
                route_domain_next = False
            elif ch == '>':
                self.pos += 1
                break
            elif ch == '@':
                self.pos += 1
                route_domain_next = True
            elif ch == ':':
                self.pos += 1
            else:
                address = self.getaddrspec()
                self.pos += 1
                break
            self.gotonext()
        return address

    def getaddress(self):
        """Read one address (or a group); returns a list of pairs."""
        field = self.field
        self.commentlist = []
        self.gotonext()
        start = self.pos
        saved_comments = self.commentlist
        phrase = self.getphraselist()
        self.gotonext()
        result = []
        if self.pos >= len(field):
            if phrase:
                result = [(SPACE.join(self.commentlist), phrase[0])]
        elif field[self.pos] in '.@':
            # a bare addr-spec
            self.pos = start
            self.commentlist = saved_comments
            spec = self.getaddrspec()
            result = [(SPACE.join(self.commentlist), spec)]
        elif field[self.pos] == ':':
            # a group: display-name ":" [mailbox-list] ";"
            self.pos += 1
            while self.pos < len(field):
                self.gotonext()
                if self._at(';'):
                    self.pos += 1
                    break
                result = result + self.getaddress()
        elif field[self.pos] == '<':
            route = self.getrouteaddr()
            name = SPACE.join(phrase)
            if self.commentlist:
                name += ' (' + ' '.join(self.commentlist) + ')'
            result = [(name, route)]
        else:
            if phrase:
                result = [(SPACE.join(self.commentlist), phrase[0])]
            elif field[self.pos] in self.specials:
                self.pos += 1
        self.gotonext()
        if self._at(','):
            self.pos += 1
        return result

    def getaddrlist(self):
        """Parse the whole field into a list of (realname, address) pairs;
        unparsable pieces give ('', '')."""
        result = []
        while self.pos < len(self.field):
            pairs = self.getaddress()
            if pairs:
                result += pairs
            else:
                result.append(('', ''))
        return result


class AddressList(AddrlistClass):
    """A set of parsed addresses supporting set-like operations."""

    def __init__(self, field):
        AddrlistClass.__init__(self, field)
        self.addresslist = self.getaddrlist() if field else []

    def __len__(self):
        return len(self.addresslist)

    def __add__(self, other):
        new = AddressList(None)
        new.addresslist = self.addresslist[:]
        for pair in other.addresslist:
            if pair not in self.addresslist:
                new.addresslist.append(pair)
        return new

    def __iadd__(self, other):
        for pair in other.addresslist:
            if pair not in self.addresslist:
                self.addresslist.append(pair)
        return self

    def __sub__(self, other):
        new = AddressList(None)
        new.addresslist = [p for p in self.addresslist
                           if p not in other.addresslist]
        return new

    def __isub__(self, other):
        for pair in other.addresslist:
            if pair in self.addresslist:
                self.addresslist.remove(pair)
        return self

    def __getitem__(self, index):
        return self.addresslist[index]
