"""Client-side HTTP cookie handling.

CookieJar stores cookies received in responses (Set-Cookie: Netscape
style, also carrying RFC 2109 cookies; Set-Cookie2: RFC 2965) and adds the
matching ones to later requests.  Whether a cookie is accepted or returned
is decided by a CookiePolicy (DefaultCookiePolicy by default).
FileCookieJar subclasses persist cookies: LWPCookieJar in the libwww-perl
"Set-Cookie3" format, MozillaCookieJar in the Netscape cookies.txt format.

Requests and responses are duck-typed after urllib.request.Request and
the objects urllib.request.urlopen() returns (only .info() is used).
"""

__all__ = ['Cookie', 'CookieJar', 'CookiePolicy', 'DefaultCookiePolicy',
           'FileCookieJar', 'LWPCookieJar', 'LoadError', 'MozillaCookieJar']

import os
import re
import time
import urllib.parse
import threading as _threading

DEFAULT_HTTP_PORT = "80"
MISSING_FILENAME_TEXT = ("a filename was not supplied (nor was the CookieJar "
                         "instance initialised with one)")

# ---------------------------------------------------------------------------
# dates

_DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
_MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
           "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
_FULL_DAYS = ["monday", "tuesday", "wednesday", "thursday", "friday",
              "saturday", "sunday"]
_FULL_MONTHS = ["january", "february", "march", "april", "may", "june",
                "july", "august", "september", "october", "november",
                "december"]
_UTC_NAMES = {"gmt", "utc", "ut", "z"}


def _days_since_epoch(year, month, day):
    # proleptic Gregorian calendar; month 1-12
    y = year - (month <= 2)
    era = y // 400
    yoe = y - era * 400
    mp = (month + 9) % 12
    doy = (153 * mp + 2) // 5 + day - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def _utc_seconds(year, month, day, hour=0, minute=0, second=0):
    days = _days_since_epoch(year, month, day)
    return ((days * 24 + hour) * 60 + minute) * 60 + second


def time2isoz(t=None):
    """UTC time as 'YYYY-MM-DD hh:mm:ssZ' (default: now)."""
    tm = time.gmtime(t)
    return "%04d-%02d-%02d %02d:%02d:%02dZ" % tm[:6]


def time2netscape(t=None):
    """UTC time as 'Wdy, DD-Mon-YYYY hh:mm:ss GMT' (default: now)."""
    tm = time.gmtime(t)
    return "%s, %02d-%s-%04d %02d:%02d:%02d GMT" % (
        _DAYS[tm.tm_wday], tm.tm_mday, _MONTHS[tm.tm_mon - 1], tm.tm_year,
        tm.tm_hour, tm.tm_min, tm.tm_sec)


# words, digit runs and single punctuation characters; whitespace is
# dropped.  Tokenizing first keeps parsing linear in the input size.
_DATE_TOKEN_RE = re.compile(r"[a-z]+|[0-9]+|[^\sa-z0-9]", re.ASCII)


def _name_index(word, full_names):
    # 'feb', 'febr' and 'february' all name February
    if len(word) < 3:
        return None
    for i, name in enumerate(full_names):
        if name.startswith(word):
            return i
    return None


def _expand_year(digits):
    if len(digits) > 4:
        return None
    year = int(digits)
    if len(digits) <= 2:
        # two-digit year: the one closest to the current year
        current = time.gmtime()[0]
        year += current - current % 100
        if year > current + 50:
            year -= 100
        elif year < current - 50:
            year += 100
    return year


class _Tokens:
    def __init__(self, text):
        self.toks = _DATE_TOKEN_RE.findall(text.lower())
        self.pos = 0

    def peek(self, offset=0):
        i = self.pos + offset
        return self.toks[i] if i < len(self.toks) else None

    def take(self):
        tok = self.peek()
        self.pos += 1
        return tok

    def at_end(self):
        return self.pos >= len(self.toks)

    def skip(self, chars):
        while self.peek() is not None and self.peek() in chars:
            self.pos += 1

    def number(self, maxlen=None):
        tok = self.peek()
        if tok is None or not tok.isdigit():
            return None
        if maxlen is not None and len(tok) > maxlen:
            return None
        self.pos += 1
        return tok

    def clock(self):
        """hh:mm[:ss[.fraction]] [am|pm] -> (h, m, s), or None."""
        if not (self.peek(0) is not None and self.peek(0).isdigit()
                and self.peek(1) == ':'):
            return None
        hour = int(self.take())
        self.take()
        minute = self.number(2)
        if minute is None:
            return False
        second = 0
        if self.peek() == ':':
            self.take()
            sec = self.number(2)
            if sec is None:
                return False
            second = int(sec)
            if self.peek() == '.' and self.peek(1) is not None \
                    and self.peek(1).isdigit():
                self.pos += 2
        ampm = self.peek()
        if ampm in ('am', 'pm'):
            self.take()
            if not 1 <= hour <= 12:
                return False
            if ampm == 'am' and hour == 12:
                hour = 0
            elif ampm == 'pm' and hour != 12:
                hour += 12
        return hour, int(minute), second

    def zone(self):
        """Offset from UTC in seconds; None if absent, False if bad."""
        tok = self.peek()
        if tok is None:
            return None
        if tok in _UTC_NAMES:
            self.take()
            if self.peek() in ('+', '-'):
                # 'GMT+0100' style
                return self._numeric_zone()
            return 0
        if tok in ('+', '-'):
            return self._numeric_zone()
        return None

    def _numeric_zone(self):
        sign = 1 if self.take() == '+' else -1
        digits = self.number(4)
        if digits is None:
            return False
        if self.peek() == ':' and len(digits) <= 2:
            self.take()
            minutes = self.number(2)
            if minutes is None:
                return False
            hours = int(digits)
            mins = int(minutes)
        elif len(digits) <= 2:
            hours, mins = int(digits), 0
        else:
            hours, mins = int(digits[:-2]), int(digits[-2:])
        if hours > 23 or mins > 59:
            return False
        return sign * (hours * 3600 + mins * 60)


def _finish(year, month, day, clock, offset):
    if year is None or not 1 <= month <= 12 or not 1 <= day <= 31:
        return None
    hour, minute, second = clock if clock else (0, 0, 0)
    if hour > 24 or minute > 59 or second > 61:
        return None
    t = _utc_seconds(year, month, day, hour, minute, second)
    if offset:
        t -= offset
    return t


def http2time(text):
    """Seconds since the epoch for an HTTP date, or None.

    Liberal: RFC 1123 ('Wed, 09 Feb 1994 22:23:32 GMT'), RFC 850
    ('Wednesday, 09-Feb-94 22:23:32 GMT'), asctime and many sloppier
    variants; the time of day and the zone (default UTC) are optional.
    """
    tk = _Tokens(text)
    if tk.peek() is not None and _name_index(tk.peek(), _FULL_DAYS) is not None:
        tk.take()
        tk.skip(',')
    year = None
    clock = None
    tok = tk.peek()
    if tok is None:
        return None
    if tok.isdigit():
        # day month year [time] [zone]
        day = tk.number(2)
        if day is None:
            return None
        tk.skip('-/')
        word = tk.take()
        month = _name_index(word, _FULL_MONTHS) if word else None
        if month is None:
            return None
        tk.skip('-/')
        digits = tk.number()
        if digits is None:
            return None
        year = _expand_year(digits)
        clock = tk.clock()
    else:
        # month day [time] [year]  (asctime, ls -l)
        month = _name_index(tok, _FULL_MONTHS)
        if month is None:
            return None
        tk.take()
        tk.skip('-/')
        day = tk.number(2)
        if day is None:
            return None
        tk.skip(',')
        clock = tk.clock()
        if clock is False:
            return None
        digits = tk.number()
        if digits is not None:
            year = _expand_year(digits)
        elif clock:
            year = time.gmtime()[0]
        else:
            return None
        if clock is None:
            clock = tk.clock()
    if clock is False:
        return None
    offset = tk.zone()
    if offset is False:
        return None
    if tk.peek() == '(':
        # trailing comment such as '(PST)'
        tk.take()
        while not tk.at_end() and tk.peek() != ')':
            tk.take()
        tk.skip(')')
    if not tk.at_end():
        return None
    return _finish(year, month + 1, int(day), clock, offset)


def iso2time(text):
    """Seconds since the epoch for an ISO 8601 date, or None.

    Accepts 'YYYY-MM-DD hh:mm:ss[.f] zone', 'YYYY-MM-DDThh:mm:ss',
    'YYYYMMDD', 'YYYYMMDDThhmmssZ' and similar; the zone defaults to UTC.
    """
    tk = _Tokens(text)
    first = tk.peek()
    if first is None or not first.isdigit():
        return None
    if len(first) == 8:
        tk.take()
        year, month, day = int(first[:4]), int(first[4:6]), int(first[6:])
    elif len(first) == 4 and tk.peek(1) == '-':
        tk.take()
        tk.take()
        m = tk.number(2)
        if m is None or tk.peek() != '-':
            return None
        tk.take()
        d = tk.number(2)
        if d is None:
            return None
        year, month, day = int(first), int(m), int(d)
    else:
        return None
    clock = None
    if tk.peek() == 't':
        tk.take()
    tok = tk.peek()
    if tok is not None and tok.isdigit():
        if tk.peek(1) == ':':
            clock = tk.clock()
            if not clock:
                return None
        elif len(tok) in (4, 6):
            tk.take()
            clock = (int(tok[:2]), int(tok[2:4]),
                     int(tok[4:]) if len(tok) == 6 else 0)
            if tk.peek() == '.' and tk.peek(1) is not None \
                    and tk.peek(1).isdigit():
                tk.pos += 2
        elif tk.peek(1) is None or tk.peek(1) in ('+', '-'):
            pass
        else:
            return None
    offset = tk.zone()
    if offset is False:
        return None
    if not tk.at_end():
        return None
    return _finish(year, month, day, clock, offset)


# ---------------------------------------------------------------------------
# header parsing

_HDR_NAME_RE = re.compile(r"[^=\s;,]+")
_HDR_EQ_RE = re.compile(r"\s*=\s*")
_HDR_QUOTED_RE = re.compile(r'"((?:[^\\"]|\\.)*)"', re.DOTALL)
_HDR_TOKEN_RE = re.compile(r"[^\s;,]*")
_BACKSLASH_RE = re.compile(r"\\(.)", re.DOTALL)


def split_header_words(header_values):
    """Parse structured header values into lists of (name, value) pairs.

    Each header value is a comma-separated list of elements; an element is
    a ';'- (or whitespace-) separated sequence of 'name', 'name=value' or
    'name="quoted value"' items.  Missing values are None.  The result has
    one list per element:

    >>> split_header_words(['foo="bar"; port="80,81"; discard, bar=baz'])
    [[('foo', 'bar'), ('port', '80,81'), ('discard', None)], [('bar', 'baz')]]
    """
    if isinstance(header_values, str):
        raise TypeError("split_header_words() expects a list of strings")
    result = []
    for text in header_values:
        pos = 0
        n = len(text)
        current = []
        while pos < n:
            m = _HDR_NAME_RE.match(text, pos)
            if m:
                name = m.group()
                pos = m.end()
                value = None
                eq = _HDR_EQ_RE.match(text, pos)
                if eq:
                    pos = eq.end()
                    q = _HDR_QUOTED_RE.match(text, pos)
                    if q:
                        value = _BACKSLASH_RE.sub(r"\1", q.group(1))
                        pos = q.end()
                    else:
                        t = _HDR_TOKEN_RE.match(text, pos)
                        value = t.group().rstrip()
                        pos = t.end()
                current.append((name, value))
                continue
            ch = text[pos]
            pos += 1
            if ch == ',':
                if current:
                    result.append(current)
                current = []
            # whitespace, ';' and stray characters only separate items
        if current:
            result.append(current)
    return result


_TOKEN_VALUE_RE = re.compile(r"[\w.-]+", re.ASCII)
_NEEDS_ESCAPE_RE = re.compile(r'([\\"])')


def join_header_words(lists):
    """Inverse of split_header_words(): build a header value from lists of
    (name, value) pairs, quoting values that are not simple tokens.

    >>> join_header_words([[("text/plain", None), ("charset", "iso-8859-1")]])
    'text/plain; charset=iso-8859-1'
    """
    elements = []
    for pairs in lists:
        items = []
        for name, value in pairs:
            if value is not None:
                if not _TOKEN_VALUE_RE.fullmatch(value):
                    value = '"%s"' % _NEEDS_ESCAPE_RE.sub(r"\\\1", value)
                name = "%s=%s" % (name, value)
            items.append(name)
        if items:
            elements.append("; ".join(items))
    return ", ".join(elements)


def _strip_quotes(text):
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        return text[1:-1]
    if text.startswith('"'):
        text = text[1:]
    if text.endswith('"'):
        text = text[:-1]
    return text


_NS_KNOWN_ATTRS = ("expires", "domain", "path", "secure", "version", "port",
                   "max-age")


def parse_ns_headers(ns_headers):
    """Parse Netscape-style Set-Cookie header values.

    Unlike split_header_words(), only ';' separates items (commas belong to
    values, e.g. in expiry dates), known attribute names are lowercased,
    'expires' becomes seconds since the epoch (None if unparsable) and a
    ('version', '0') pair is appended unless the header gave a version.
    """
    result = []
    for header in ns_headers:
        pairs = []
        version_given = False
        for index, item in enumerate(header.split(";")):
            name, eq, value = item.partition("=")
            name = name.strip()
            if not name:
                if index == 0:
                    break       # no cookie name: ignore the whole header
                continue
            value = value.strip() if eq else None
            if index > 0:
                lname = name.lower()
                if lname in _NS_KNOWN_ATTRS:
                    name = lname
                if name == "version":
                    if value is not None:
                        value = _strip_quotes(value)
                    version_given = True
                elif name == "expires":
                    if value is not None:
                        value = http2time(_strip_quotes(value))
            pairs.append((name, value))
        if pairs:
            if not version_given:
                pairs.append(("version", "0"))
            result.append(pairs)
    return result


# ---------------------------------------------------------------------------
# host names, domains, paths and ports

_IPV4_RE = re.compile(r"\.\d+$", re.ASCII)


def is_HDN(text):
    """True for a host domain name (not an IP address, no empty labels at
    the ends)."""
    if not text or _IPV4_RE.search(text):
        return False
    return not (text.startswith(".") or text.endswith("."))


def domain_match(A, B):
    """RFC 2965 domain matching (case-insensitive): A and B are equal, or
    A is a host name, B is '.' + a host name and A ends with B."""
    A = A.lower()
    B = B.lower()
    if A == B:
        return True
    if not is_HDN(A) or not B.startswith("."):
        return False
    return len(A) > len(B) and A.endswith(B) and is_HDN(B[1:])


def liberal_is_HDN(text):
    """Like is_HDN() but only excludes IP addresses."""
    return not _IPV4_RE.search(text)


def user_domain_match(A, B):
    """Matching for user-supplied domain lists: with a leading dot, B
    matches the domains below it; otherwise only itself."""
    A = A.lower()
    B = B.lower()
    if not (liberal_is_HDN(A) and liberal_is_HDN(B)):
        return A == B      # IP addresses only match exactly
    if B.startswith("."):
        return A.endswith(B)
    return A == B


_PORT_SUFFIX_RE = re.compile(r":\d+$", re.ASCII)


def request_host(request):
    """The lowercased host of *request* without the port (taken from the
    URL, or the Host header if the URL has none)."""
    host = urllib.parse.urlsplit(request.get_full_url()).netloc
    if not host:
        host = request.get_header("Host", "")
    return _PORT_SUFFIX_RE.sub("", host, 1).lower()


def eff_request_host(request):
    """(request host, effective request host): host names without a dot
    get '.local' appended in the second (RFC 2965)."""
    host = request_host(request)
    if "." not in host:
        return host, host + ".local"
    return host, host


def request_path(request):
    """The escaped path (with parameters) of *request*'s URL."""
    parts = urllib.parse.urlsplit(request.get_full_url())
    path = escape_path(parts.path)
    if not path.startswith("/"):
        path = "/" + path
    return path


def request_port(request):
    """The port of *request*'s URL as a string (DEFAULT_HTTP_PORT if none,
    None if it is not a number)."""
    host = request.host
    sep = host.find(":")
    if sep < 0:
        return DEFAULT_HTTP_PORT
    port = host[sep + 1:]
    try:
        int(port)
    except ValueError:
        return None
    return port


_ESCAPE_SAFE = "%/;:@&=+$,!~*'()"
_PCT_RE = re.compile(r"%([0-9a-fA-F][0-9a-fA-F])")


def escape_path(path):
    """Percent-encode the characters of *path* that are not allowed in a
    URL path (UTF-8), and normalise existing escapes to upper case."""
    path = urllib.parse.quote(path, _ESCAPE_SAFE)
    return _PCT_RE.sub(lambda m: "%" + m.group(1).upper(), path)


def reach(h):
    """The domain a cookie from host *h* may at most reach (RFC 2965):
    'www.acme.com' -> '.acme.com'; hosts with fewer labels, IP addresses
    and non-host names are returned unchanged."""
    first = h.find(".")
    if first >= 0:
        rest = h[first + 1:]
        if is_HDN(h) and ("." in rest or rest == "local"):
            return "." + rest
    return h


def is_third_party(request):
    """True if *request*'s host is outside the reach of the host that
    started the transaction (request.origin_req_host)."""
    host = request_host(request)
    return not domain_match(host, reach(request.origin_req_host))


def _path_matches(req_path, cookie_path):
    if req_path == cookie_path:
        return True
    if not req_path.startswith(cookie_path):
        return False
    return cookie_path.endswith("/") or req_path[len(cookie_path)] == "/"


# ---------------------------------------------------------------------------
# cookies and policies

class Cookie:
    """A cookie: a plain record of its attributes.

    'rest' holds the non-standard attributes.  'port' is a comma-separated
    string of port numbers, 'expires' seconds since the epoch.  The
    *_specified flags tell whether the attribute was given explicitly
    (otherwise the value is a default derived from the request).
    """

    def __init__(self, version, name, value,
                 port, port_specified,
                 domain, domain_specified, domain_initial_dot,
                 path, path_specified,
                 secure,
                 expires,
                 discard,
                 comment,
                 comment_url,
                 rest,
                 rfc2109=False):
        if version is not None:
            version = int(version)
        if expires is not None:
            expires = int(float(expires))
        if port is None and port_specified is True:
            raise ValueError("if port is None, port_specified must be false")
        self.version = version
        self.name = name
        self.value = value
        self.port = port
        self.port_specified = port_specified
        # domains are case-insensitive: store them lowercased
        self.domain = domain.lower()
        self.domain_specified = domain_specified
        self.domain_initial_dot = domain_initial_dot
        self.path = path
        self.path_specified = path_specified
        self.secure = secure
        self.expires = expires
        self.discard = discard
        self.comment = comment
        self.comment_url = comment_url
        self.rfc2109 = rfc2109
        self._rest = dict(rest)

    def has_nonstandard_attr(self, name):
        return name in self._rest

    def get_nonstandard_attr(self, name, default=None):
        return self._rest.get(name, default)

    def set_nonstandard_attr(self, name, value):
        self._rest[name] = value

    def is_expired(self, now=None):
        if now is None:
            now = time.time()
        return self.expires is not None and self.expires <= now

    def __str__(self):
        where = (self.domain + self.path) if self.port is None else \
            "%s:%s%s" % (self.domain, self.port, self.path)
        what = self.name if self.value is None else \
            "%s=%s" % (self.name, self.value)
        return "<Cookie %s for %s>" % (what, where)

    _FIELDS = ("version", "name", "value", "port", "port_specified",
               "domain", "domain_specified", "domain_initial_dot",
               "path", "path_specified", "secure", "expires", "discard",
               "comment", "comment_url")

    def __repr__(self):
        args = ["%s=%r" % (field, getattr(self, field))
                for field in self._FIELDS]
        args.append("rest=%r" % (self._rest,))
        args.append("rfc2109=%r" % (self.rfc2109,))
        return "%s(%s)" % (self.__class__.__name__, ", ".join(args))


class CookiePolicy:
    """Interface of the objects deciding which cookies are accepted from a
    server and which are returned to it."""

    netscape = True
    rfc2965 = False
    hide_cookie2 = False

    def set_ok(self, cookie, request):
        """Whether *cookie*, received in reply to *request*, may be
        stored."""
        raise NotImplementedError()

    def return_ok(self, cookie, request):
        """Whether *cookie* may be sent with *request*."""
        raise NotImplementedError()

    def domain_return_ok(self, domain, request):
        """Quick pre-filter: False if no cookie of *domain* can be returned
        with *request*."""
        return True

    def path_return_ok(self, path, request):
        """Quick pre-filter: False if no cookie with *path* can be returned
        with *request*."""
        return True


# second-level labels that are registries in many country domains
_REGISTRY_LABELS = frozenset([
    "co", "ac", "com", "edu", "org", "net", "gov", "mil", "int", "aero",
    "biz", "cat", "coop", "info", "jobs", "mobi", "museum", "name", "pro",
    "travel", "eu", "tx", "ny", "ca"])


class DefaultCookiePolicy(CookiePolicy):
    """The standard rules for Netscape and RFC 2965 cookies, with
    switches for stricter behaviour."""

    DomainStrictNoDots = 1
    DomainStrictNonDomain = 2
    DomainRFC2965Match = 4

    DomainLiberal = 0
    DomainStrict = DomainStrictNoDots | DomainStrictNonDomain

    def __init__(self,
                 blocked_domains=None, allowed_domains=None,
                 netscape=True, rfc2965=False,
                 rfc2109_as_netscape=None,
                 hide_cookie2=False,
                 strict_domain=False,
                 strict_rfc2965_unverifiable=True,
                 strict_ns_unverifiable=False,
                 strict_ns_domain=DomainLiberal,
                 strict_ns_set_initial_dollar=False,
                 strict_ns_set_path=False,
                 secure_protocols=("https", "wss"),
                 ):
        self.netscape = netscape
        self.rfc2965 = rfc2965
        self.rfc2109_as_netscape = rfc2109_as_netscape
        self.hide_cookie2 = hide_cookie2
        self.strict_domain = strict_domain
        self.strict_rfc2965_unverifiable = strict_rfc2965_unverifiable
        self.strict_ns_unverifiable = strict_ns_unverifiable
        self.strict_ns_domain = strict_ns_domain
        self.strict_ns_set_initial_dollar = strict_ns_set_initial_dollar
        self.strict_ns_set_path = strict_ns_set_path
        self.secure_protocols = secure_protocols
        self._blocked_domains = tuple(blocked_domains or ())
        self._allowed_domains = None if allowed_domains is None else \
            tuple(allowed_domains)
        self._now = int(time.time())

    # -- domain lists

    def blocked_domains(self):
        """The sequence of blocked domains (as a tuple)."""
        return self._blocked_domains

    def set_blocked_domains(self, blocked_domains):
        """Replace the sequence of blocked domains."""
        self._blocked_domains = tuple(blocked_domains)

    def is_blocked(self, domain):
        return any(user_domain_match(domain, blocked)
                   for blocked in self._blocked_domains)

    def allowed_domains(self):
        """None (all domains allowed), or the tuple of allowed domains."""
        return self._allowed_domains

    def set_allowed_domains(self, allowed_domains):
        """Replace the allowed domains (None allows every domain)."""
        if allowed_domains is not None:
            allowed_domains = tuple(allowed_domains)
        self._allowed_domains = allowed_domains

    def is_not_allowed(self, domain):
        if self._allowed_domains is None:
            return False
        return not any(user_domain_match(domain, allowed)
                       for allowed in self._allowed_domains)

    # -- accepting cookies

    def set_ok(self, cookie, request):
        """Apply the acceptance rules; any failing check rejects."""
        for check in (self.set_ok_version, self.set_ok_verifiability,
                      self.set_ok_name, self.set_ok_path,
                      self.set_ok_domain, self.set_ok_port):
            if not check(cookie, request):
                return False
        return True

    def _version_allowed(self, version):
        if version is None or version < 0:
            return False      # neither Netscape nor RFC 2965
        if version == 0:
            return bool(self.netscape)
        return bool(self.rfc2965)

    def set_ok_version(self, cookie, request):
        return self._version_allowed(cookie.version)

    def _verifiable_ok(self, cookie, request):
        if request.unverifiable and is_third_party(request):
            if cookie.version > 0 and self.strict_rfc2965_unverifiable:
                return False
            if cookie.version == 0 and self.strict_ns_unverifiable:
                return False
        return True

    def set_ok_verifiability(self, cookie, request):
        return self._verifiable_ok(cookie, request)

    def set_ok_name(self, cookie, request):
        # '$Version' etc. are reserved by RFC 2965
        if cookie.version == 0 and self.strict_ns_set_initial_dollar \
                and cookie.name.startswith("$"):
            return False
        return True

    def set_ok_path(self, cookie, request):
        if cookie.path_specified and (
                cookie.version > 0 or self.strict_ns_set_path):
            if not _path_matches(request_path(request), cookie.path):
                return False
        return True

    def set_ok_domain(self, cookie, request):
        if self.is_blocked(cookie.domain) or \
                self.is_not_allowed(cookie.domain):
            return False
        if not cookie.domain_specified:
            return True       # the default is the request host itself
        req_host, erhn = eff_request_host(request)
        domain = cookie.domain
        bare = domain[1:] if domain.startswith(".") else domain
        if self.strict_domain:
            labels = bare.split(".")
            if len(labels) == 2 and len(labels[1]) == 2 and \
                    labels[0].lower() in _REGISTRY_LABELS:
                return False  # e.g. '.co.uk'
        if "." not in bare and not erhn.endswith(".local"):
            return False      # a top-level domain is too general
        if cookie.version == 0:
            dotted = domain if domain.startswith(".") else "." + domain
            if not (("." + erhn).endswith(dotted) or
                    erhn.endswith(bare + ".local")):
                return False
        rfc_rules = cookie.version > 0
        if rfc_rules or self.strict_ns_domain & self.DomainRFC2965Match:
            if not domain_match(erhn, domain):
                return False
        if rfc_rules or self.strict_ns_domain & self.DomainStrictNoDots:
            prefix = erhn[:-len(domain)] if len(domain) < len(erhn) else ""
            if "." in prefix and not _IPV4_RE.search(req_host):
                return False  # the host must not be "deeper" than one label
        return True

    def set_ok_port(self, cookie, request):
        if cookie.port_specified:
            req_port = request_port(request) or DEFAULT_HTTP_PORT
            for port in cookie.port.split(","):
                port = port.strip()
                try:
                    int(port)
                except ValueError:
                    return False
                if port == req_port:
                    return True
            return False
        return True

    # -- returning cookies

    def return_ok(self, cookie, request):
        """Apply the rules for sending; any failing check refuses."""
        for check in (self.return_ok_version, self.return_ok_verifiability,
                      self.return_ok_secure, self.return_ok_expires,
                      self.return_ok_port, self.return_ok_domain):
            if not check(cookie, request):
                return False
        return True

    def return_ok_version(self, cookie, request):
        return self._version_allowed(cookie.version)

    def return_ok_verifiability(self, cookie, request):
        return self._verifiable_ok(cookie, request)

    def return_ok_secure(self, cookie, request):
        return not cookie.secure or request.type in self.secure_protocols

    def return_ok_expires(self, cookie, request):
        return not cookie.is_expired(self._now)

    def return_ok_port(self, cookie, request):
        if cookie.port:
            req_port = request_port(request) or DEFAULT_HTTP_PORT
            return any(port.strip() == req_port
                       for port in cookie.port.split(","))
        return True

    def return_ok_domain(self, cookie, request):
        req_host, erhn = eff_request_host(request)
        domain = cookie.domain
        dotted = domain if domain.startswith(".") or not domain else \
            "." + domain
        if (cookie.version == 0 and
                self.strict_ns_domain & self.DomainStrictNonDomain and
                not cookie.domain_specified and domain != erhn):
            return False
        if cookie.version > 0 and not domain_match(erhn, domain):
            return False
        if cookie.version == 0 and not ("." + erhn).endswith(dotted):
            return False
        return True

    def domain_return_ok(self, domain, request):
        req_host, erhn = eff_request_host(request)
        dotted = domain if domain.startswith(".") or not domain else \
            "." + domain
        if not (("." + req_host).endswith(dotted) or
                ("." + erhn).endswith(dotted)):
            return False
        if self.is_blocked(domain) or self.is_not_allowed(domain):
            return False
        return True

    def path_return_ok(self, path, request):
        return _path_matches(request_path(request), path)


# ---------------------------------------------------------------------------
# the jar

_VALUE_ATTRS = ("version", "expires", "max-age", "domain", "path", "port",
                "comment", "commenturl")
_BOOLEAN_ATTRS = ("discard", "secure")
_OPTIONAL_VALUE_ATTRS = ("port", "comment", "commenturl")
_NON_WORD_RE = re.compile(r"\W", re.ASCII)


def _report_internal_error():
    # a bug in this module must not break the request; make it visible
    import io
    import traceback
    import warnings
    buf = io.StringIO()
    traceback.print_exc(None, buf)
    warnings.warn("http.cookiejar internal error:\n%s" % buf.getvalue(),
                  stacklevel=3)


class _Missing:
    pass


class CookieJar:
    """A collection of cookies, organised as
    domain -> path -> name -> Cookie (in insertion order)."""

    def __init__(self, policy=None):
        if policy is None:
            policy = DefaultCookiePolicy()
        self._policy = policy
        self._cookies_lock = _threading.RLock()
        self._cookies = {}
        self._now = int(time.time())

    def set_policy(self, policy):
        self._policy = policy

    def _stamp_time(self):
        self._policy._now = self._now = int(time.time())

    # -- sending

    def _cookies_for_request(self, request):
        policy = self._policy
        found = []
        for domain, paths in self._cookies.items():
            if not policy.domain_return_ok(domain, request):
                continue
            for path, by_name in paths.items():
                if not policy.path_return_ok(path, request):
                    continue
                for cookie in by_name.values():
                    if policy.return_ok(cookie, request):
                        found.append(cookie)
        # the most specific path first (RFC 2965 3.3.4); sort is stable
        found.sort(key=lambda c: len(c.path), reverse=True)
        return found

    def _cookie_attrs(self, cookies):
        """The items of a Cookie header for *cookies*."""
        attrs = []
        version_done = False
        for cookie in cookies:
            version = cookie.version
            if not version_done:
                version_done = True
                if version > 0:
                    attrs.append("$Version=%s" % version)
            value = cookie.value
            if value is not None and version > 0 and \
                    _NON_WORD_RE.search(value):
                value = _NEEDS_ESCAPE_RE.sub(r"\\\1", value)
            if value is None:
                attrs.append(cookie.name)
            else:
                attrs.append("%s=%s" % (cookie.name, value))
            if version > 0:
                if cookie.path_specified:
                    attrs.append('$Path="%s"' % cookie.path)
                if cookie.domain.startswith("."):
                    domain = cookie.domain
                    if not cookie.domain_initial_dot:
                        domain = domain[1:]
                    attrs.append('$Domain="%s"' % domain)
                if cookie.port is not None:
                    item = "$Port"
                    if cookie.port_specified:
                        item += '="%s"' % cookie.port
                    attrs.append(item)
        return attrs

    def add_cookie_header(self, request):
        """Add the Cookie (and, for RFC 2965, Cookie2) header for the
        cookies that may be sent with *request*."""
        with self._cookies_lock:
            self._stamp_time()
            cookies = self._cookies_for_request(request)
            attrs = self._cookie_attrs(cookies)
            if attrs and not request.has_header("Cookie"):
                request.add_unredirected_header("Cookie", "; ".join(attrs))
            policy = self._policy
            if policy.rfc2965 and not policy.hide_cookie2 and \
                    not request.has_header("Cookie2"):
                # tell the server we understand RFC 2965 cookies
                if any(cookie.version != 1 for cookie in cookies):
                    request.add_unredirected_header("Cookie2", '$Version="1"')
            self.clear_expired_cookies()

    # -- receiving

    def _normalized_cookie_tuples(self, attrs_set):
        """(name, value, standard attrs, other attrs) for each cookie;
        malformed cookies are dropped."""
        tuples = []
        for attrs in attrs_set:
            name, value = attrs[0]
            standard = {}
            rest = {}
            max_age_seen = False
            bad = False
            for key, val in attrs[1:]:
                lkey = key.lower()
                if lkey in _VALUE_ATTRS or lkey in _BOOLEAN_ATTRS:
                    key = lkey
                if key in _BOOLEAN_ATTRS and val is None:
                    val = True
                if key in standard:
                    continue      # the first occurrence wins
                if key == "domain":
                    if val is None:
                        bad = True
                        break
                    val = val.lower()
                elif key == "expires":
                    if max_age_seen or val is None:
                        continue  # Max-Age wins; bad dates mean session
                elif key == "max-age":
                    max_age_seen = True
                    try:
                        val = int(val)
                    except (TypeError, ValueError):
                        bad = True
                        break
                    key = "expires"
                    val = self._now + val
                if key in _VALUE_ATTRS or key in _BOOLEAN_ATTRS:
                    if val is None and key not in _OPTIONAL_VALUE_ATTRS:
                        bad = True
                        break
                    standard[key] = val
                else:
                    rest[key] = val
            if not bad:
                tuples.append((name, value, standard, rest))
        return tuples

    def _cookie_from_cookie_tuple(self, tup, request):
        name, value, standard, rest = tup
        missing = _Missing
        domain = standard.get("domain", missing)
        path = standard.get("path", missing)
        port = standard.get("port", missing)
        expires = standard.get("expires", missing)
        version = standard.get("version")
        if version is not None:
            try:
                version = int(version)
            except ValueError:
                return None
            if version < 0:
                return None
        secure = standard.get("secure", False)
        discard = standard.get("discard", False)
        comment = standard.get("comment")
        comment_url = standard.get("commenturl")

        if path is not missing and path != "":
            path_specified = True
            path = escape_path(path)
        else:
            # default: the directory of the request path (Netscape cookies
            # without the trailing slash)
            path_specified = False
            path = request_path(request)
            slash = path.rfind("/")
            if slash >= 0:
                path = path[:slash] if version == 0 else path[:slash + 1]
            if not path:
                path = "/"

        domain_specified = domain is not missing
        domain_initial_dot = False
        if domain_specified:
            domain_initial_dot = domain.startswith(".")
            if not domain_initial_dot:
                domain = "." + domain
        else:
            domain = eff_request_host(request)[1]

        port_specified = False
        if port is missing:
            port = None
        elif port is None:
            port = request_port(request)   # 'Port' without a value
        else:
            port_specified = True
            port = re.sub(r"\s+", "", port)

        if expires is missing:
            expires = None
            discard = True
        elif expires <= self._now:
            # an expiry in the past deletes the cookie
            try:
                self.clear(domain, path, name)
            except KeyError:
                pass
            return None

        return Cookie(version, name, value, port, port_specified,
                      domain, domain_specified, domain_initial_dot,
                      path, path_specified, secure, expires, discard,
                      comment, comment_url, rest)

    def _cookies_from_attrs_set(self, attrs_set, request):
        cookies = []
        for tup in self._normalized_cookie_tuples(attrs_set):
            cookie = self._cookie_from_cookie_tuple(tup, request)
            if cookie is not None:
                cookies.append(cookie)
        return cookies

    def _process_rfc2109_cookies(self, cookies):
        as_netscape = self._policy.rfc2109_as_netscape
        if as_netscape is None:
            as_netscape = not self._policy.rfc2965
        for cookie in cookies:
            if cookie.version == 1:
                cookie.rfc2109 = True
                if as_netscape:
                    cookie.version = 0

    def make_cookies(self, response, request):
        """The cookies set by *response* (not checked against the
        policy)."""
        headers = response.info()
        rfc2965_hdrs = headers.get_all("Set-Cookie2", [])
        ns_hdrs = headers.get_all("Set-Cookie", [])
        self._stamp_time()
        policy = self._policy
        use_2965 = bool(rfc2965_hdrs) and policy.rfc2965
        use_ns = bool(ns_hdrs) and policy.netscape
        if not use_2965 and not use_ns:
            return []
        cookies = []
        if use_2965:
            try:
                cookies = self._cookies_from_attrs_set(
                    split_header_words(rfc2965_hdrs), request)
            except Exception:
                _report_internal_error()
                cookies = []
        if use_ns:
            try:
                ns_cookies = self._cookies_from_attrs_set(
                    parse_ns_headers(ns_hdrs), request)
            except Exception:
                _report_internal_error()
                ns_cookies = []
            self._process_rfc2109_cookies(ns_cookies)
            if cookies:
                # an RFC 2965 cookie overrides a Netscape one with the
                # same domain, path and name
                seen = {(c.domain, c.path, c.name) for c in cookies}
                ns_cookies = [c for c in ns_cookies
                              if (c.domain, c.path, c.name) not in seen]
            cookies.extend(ns_cookies)
        return cookies

    def set_cookie_if_ok(self, cookie, request):
        """Store *cookie* if the policy accepts it."""
        with self._cookies_lock:
            self._stamp_time()
            if self._policy.set_ok(cookie, request):
                self.set_cookie(cookie)

    def set_cookie(self, cookie):
        """Store *cookie* unconditionally."""
        with self._cookies_lock:
            paths = self._cookies.setdefault(cookie.domain, {})
            paths.setdefault(cookie.path, {})[cookie.name] = cookie

    def extract_cookies(self, response, request):
        """Store the acceptable cookies set by *response* to *request*."""
        with self._cookies_lock:
            for cookie in self.make_cookies(response, request):
                if self._policy.set_ok(cookie, request):
                    self.set_cookie(cookie)

    # -- removing

    def clear(self, domain=None, path=None, name=None):
        """Remove cookies: all, those of *domain*, of *domain* and *path*,
        or the single cookie named *name* (KeyError if absent)."""
        with self._cookies_lock:
            if name is not None:
                if domain is None or path is None:
                    raise ValueError(
                        "domain and path must be given to remove a cookie "
                        "by name")
                del self._cookies[domain][path][name]
            elif path is not None:
                if domain is None:
                    raise ValueError(
                        "domain must be given to remove cookies by path")
                del self._cookies[domain][path]
            elif domain is not None:
                del self._cookies[domain]
            else:
                self._cookies = {}

    def clear_session_cookies(self):
        """Remove the cookies that last only for the session (discard
        flag set)."""
        with self._cookies_lock:
            for cookie in list(self):
                if cookie.discard:
                    self.clear(cookie.domain, cookie.path, cookie.name)

    def clear_expired_cookies(self):
        """Remove the cookies whose expiry time has passed."""
        with self._cookies_lock:
            now = time.time()
            for cookie in list(self):
                if cookie.is_expired(now):
                    self.clear(cookie.domain, cookie.path, cookie.name)

    # -- container protocol

    def __iter__(self):
        with self._cookies_lock:
            cookies = [cookie
                       for paths in self._cookies.values()
                       for by_name in paths.values()
                       for cookie in by_name.values()]
        return iter(cookies)

    def __len__(self):
        return sum(1 for _ in self)

    def __repr__(self):
        return "<%s[%s]>" % (self.__class__.__name__,
                             ", ".join(repr(c) for c in self))

    def __str__(self):
        return "<%s[%s]>" % (self.__class__.__name__,
                             ", ".join(str(c) for c in self))


class LoadError(OSError):
    """A cookie file could not be parsed."""


class FileCookieJar(CookieJar):
    """A CookieJar that can be saved to and loaded from a file."""

    def __init__(self, filename=None, delayload=False, policy=None):
        super().__init__(policy)
        if filename is not None:
            filename = os.fspath(filename)
        self.filename = filename
        self.delayload = bool(delayload)

    def save(self, filename=None, ignore_discard=False, ignore_expires=False):
        """Write the cookies to *filename* (default: self.filename)."""
        raise NotImplementedError()

    def _target(self, filename):
        if filename is None:
            if self.filename is None:
                raise ValueError(MISSING_FILENAME_TEXT)
            filename = self.filename
        return filename

    def _open_for_save(self, filename):
        # cookies are credentials: only the owner may read the file
        fd = os.open(filename, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
        return os.fdopen(fd, "w")

    def load(self, filename=None, ignore_discard=False, ignore_expires=False):
        """Add the cookies stored in *filename* (default: self.filename)."""
        filename = self._target(filename)
        with open(filename) as f:
            self._really_load(f, filename, ignore_discard, ignore_expires)

    def revert(self, filename=None,
               ignore_discard=False, ignore_expires=False):
        """Replace the cookies by those of the file; on error the jar is
        left unchanged."""
        filename = self._target(filename)
        with self._cookies_lock:
            saved = self._cookies
            self._cookies = {}
            try:
                self.load(filename, ignore_discard, ignore_expires)
            except OSError:
                self._cookies = saved
                raise


def lwp_cookie_str(cookie):
    """The Set-Cookie3 representation of *cookie* (without the header
    name), as written by LWPCookieJar."""
    items = [(cookie.name, cookie.value),
             ("path", cookie.path),
             ("domain", cookie.domain)]
    if cookie.port is not None:
        items.append(("port", cookie.port))
    if cookie.path_specified:
        items.append(("path_spec", None))
    if cookie.port_specified:
        items.append(("port_spec", None))
    if cookie.domain_initial_dot:
        items.append(("domain_dot", None))
    if cookie.secure:
        items.append(("secure", None))
    if cookie.expires:
        items.append(("expires", time2isoz(float(cookie.expires))))
    if cookie.discard:
        items.append(("discard", None))
    if cookie.comment:
        items.append(("comment", cookie.comment))
    if cookie.comment_url:
        items.append(("commenturl", cookie.comment_url))
    for key in sorted(cookie._rest):
        items.append((key, str(cookie._rest[key])))
    items.append(("version", str(cookie.version)))
    return join_header_words([items])


class LWPCookieJar(FileCookieJar):
    """Cookies stored in the libwww-perl 'Set-Cookie3' format, which keeps
    every attribute (RFC 2965 included)."""

    _MAGIC = "#LWP-Cookies-2.0"
    _LINE_PREFIX = "Set-Cookie3:"
    _FLAG_ATTRS = ("port_spec", "path_spec", "domain_dot", "secure",
                   "discard")
    _VALUE_ATTRS = ("version", "port", "path", "domain", "expires",
                    "comment", "commenturl")

    def as_lwp_str(self, ignore_discard=True, ignore_expires=True):
        """The cookies as Set-Cookie3 lines (newline-terminated)."""
        now = time.time()
        lines = []
        for cookie in self:
            if not ignore_discard and cookie.discard:
                continue
            if not ignore_expires and cookie.is_expired(now):
                continue
            lines.append("%s %s\n" % (self._LINE_PREFIX,
                                       lwp_cookie_str(cookie)))
        return "".join(lines)

    def save(self, filename=None, ignore_discard=False, ignore_expires=False):
        filename = self._target(filename)
        with self._open_for_save(filename) as f:
            f.write(self._MAGIC + "\n")
            f.write(self.as_lwp_str(ignore_discard, ignore_expires))

    def _really_load(self, f, filename, ignore_discard, ignore_expires):
        magic = f.readline()
        if not magic.startswith(self._MAGIC):
            raise LoadError("%r does not look like a Set-Cookie3 (LWP) "
                            "format file" % filename)
        now = time.time()
        line = ""
        try:
            for line in f:
                if not line.startswith(self._LINE_PREFIX):
                    continue
                body = line[len(self._LINE_PREFIX):].strip()
                for items in split_header_words([body]):
                    self._load_one(items, now, ignore_discard, ignore_expires)
        except OSError:
            raise
        except Exception:
            _report_internal_error()
            raise LoadError("invalid Set-Cookie3 format file %r: %r"
                            % (filename, line))

    def _load_one(self, items, now, ignore_discard, ignore_expires):
        name, value = items[0]
        attrs = dict.fromkeys(self._FLAG_ATTRS, False)
        rest = {}
        for key, val in items[1:]:
            lkey = key.lower()
            if lkey in self._FLAG_ATTRS:
                attrs[lkey] = True if val is None else val
            elif lkey in self._VALUE_ATTRS:
                attrs[lkey] = val
            else:
                rest[key] = val
        expires = attrs.get("expires")
        discard = attrs["discard"]
        if expires is not None:
            expires = iso2time(expires)
        if expires is None:
            discard = True
        domain = attrs.get("domain")
        cookie = Cookie(attrs.get("version"), name, value,
                        attrs.get("port"), attrs["port_spec"],
                        domain, domain.startswith("."), attrs["domain_dot"],
                        attrs.get("path"), attrs["path_spec"],
                        attrs["secure"], expires, discard,
                        attrs.get("comment"), attrs.get("commenturl"), rest)
        if not ignore_discard and cookie.discard:
            return
        if not ignore_expires and cookie.is_expired(now):
            return
        self.set_cookie(cookie)


NETSCAPE_HEADER_TEXT = """\
# Netscape HTTP Cookie File
# Written by Python's http.cookiejar; the format is tab-separated:
# domain, subdomains, path, secure, expiry, name, value.

"""

_HTTPONLY_ATTR = "HTTPOnly"
_HTTPONLY_PREFIX = "#HttpOnly_"


class MozillaCookieJar(FileCookieJar):
    """Cookies stored in the Netscape/Mozilla 'cookies.txt' format (also
    used by curl and wget).

    The format only knows Netscape cookies: RFC 2965 attributes are lost
    and loaded cookies get version 0.
    """

    # ASCII-only case folding: 'ſ' must not match 's'
    magic_re = re.compile("#( Netscape)? HTTP Cookie File",
                          re.IGNORECASE | re.ASCII)

    def save(self, filename=None, ignore_discard=False, ignore_expires=False):
        filename = self._target(filename)
        now = time.time()
        with self._open_for_save(filename) as f:
            f.write(NETSCAPE_HEADER_TEXT)
            for cookie in self:
                if not ignore_discard and cookie.discard:
                    continue
                if not ignore_expires and cookie.is_expired(now):
                    continue
                domain = cookie.domain
                if cookie.has_nonstandard_attr(_HTTPONLY_ATTR):
                    domain = _HTTPONLY_PREFIX + domain
                if cookie.value is None:
                    # a value-less cookie: empty name, the name as value
                    name, value = "", cookie.name
                else:
                    name, value = cookie.name, cookie.value
                fields = [domain,
                          "TRUE" if cookie.domain.startswith(".") else "FALSE",
                          cookie.path,
                          "TRUE" if cookie.secure else "FALSE",
                          "" if cookie.expires is None else str(cookie.expires),
                          name, value]
                f.write("\t".join(fields) + "\n")

    def _really_load(self, f, filename, ignore_discard, ignore_expires):
        now = time.time()
        magic = f.readline()
        if not self.magic_re.match(magic):
            raise LoadError("%r does not look like a Netscape format cookies "
                            "file" % filename)
        line = ""
        try:
            for line in f:
                rest = {}
                text = line
                if text.startswith(_HTTPONLY_PREFIX):
                    rest[_HTTPONLY_ATTR] = ""
                    text = text[len(_HTTPONLY_PREFIX):]
                if text.endswith("\n"):
                    text = text[:-1]
                stripped = text.strip()
                if not stripped or stripped.startswith(("#", "$")):
                    continue
                (domain, subdomains, path, secure, expires,
                 name, value) = text.split("\t")
                if name == "":
                    name, value = value, None
                initial_dot = domain.startswith(".")
                discard = False
                if expires in ("", "0"):
                    # session cookie (curl and wget write 0)
                    expires = None
                    discard = True
                cookie = Cookie(0, name, value, None, False,
                                domain, subdomains == "TRUE", initial_dot,
                                path, False, secure == "TRUE",
                                expires, discard, None, None, rest)
                if not ignore_discard and cookie.discard:
                    continue
                if not ignore_expires and cookie.is_expired(now):
                    continue
                self.set_cookie(cookie)
        except OSError:
            raise
        except Exception:
            _report_internal_error()
            raise LoadError("invalid Netscape format cookies file %r: %r"
                            % (filename, line))
