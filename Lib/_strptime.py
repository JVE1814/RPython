"""strptime() implementation shared by time.strptime and datetime.*.strptime.

Parsing works by turning the format into a regular expression whose named
groups correspond to directives.  Locale dependent names (weekdays, months,
AM/PM, time zone names and the %c/%x/%X layouts) are discovered by running
time.strftime() on known dates, so the parser matches whatever the C library
produces for the current LC_TIME locale.
"""

import time
import re as _re
from re import compile as re_compile, IGNORECASE
from _thread import allocate_lock as _thread_allocate_lock

__all__ = []


def _getlang():
    """Return the (language, encoding) pair of the current LC_TIME locale."""
    try:
        import locale
    except ImportError:
        return (None, None)
    return locale.getlocale(locale.LC_TIME)


# ---------------------------------------------------------------------------
# Proleptic Gregorian calendar helpers (no dependency on datetime)

_DAYS_IN_MONTH = (0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
_DAYS_BEFORE_MONTH = (0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334)


def _is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def _days_in_month(year, month):
    if month == 2 and _is_leap(year):
        return 29
    return _DAYS_IN_MONTH[month]


def _toordinal(year, month, day):
    if not 1 <= year <= 9999:
        raise ValueError('year %d is out of range' % year)
    if not 1 <= month <= 12:
        raise ValueError('month must be in 1..12')
    if not 1 <= day <= _days_in_month(year, month):
        raise ValueError('day %d must be in range 1..%d for month %d in year %d'
                         % (day, _days_in_month(year, month), month, year))
    y = year - 1
    days = y * 365 + y // 4 - y // 100 + y // 400
    days += _DAYS_BEFORE_MONTH[month] + (month > 2 and _is_leap(year))
    return days + day


def _fromordinal(n):
    """Inverse of _toordinal: return (year, month, day)."""
    n -= 1
    n400, n = divmod(n, 146097)
    year = n400 * 400 + 1
    n100, n = divmod(n, 36524)
    n4, n = divmod(n, 1461)
    n1, n = divmod(n, 365)
    year += n100 * 100 + n4 * 4 + n1
    if n1 == 4 or n100 == 4:
        return year - 1, 12, 31
    leap = _is_leap(year)
    month = (n + 50) >> 5
    preceding = _DAYS_BEFORE_MONTH[month] + (month > 2 and leap)
    if preceding > n:
        month -= 1
        preceding -= _DAYS_IN_MONTH[month] + (month == 2 and leap)
    return year, month, n - preceding + 1


def _weekday(year, month, day):
    """Monday is 0."""
    return (_toordinal(year, month, day) + 6) % 7


def _iso_to_ordinal(year, week, day):
    if not 0 < week < 53:
        jan1 = _weekday(year, 1, 1)
        if not (week == 53 and (jan1 == 3 or (jan1 == 2 and _is_leap(year)))):
            raise ValueError(f"Invalid week: {week}")
    if not 0 < day < 8:
        raise ValueError(f"Invalid weekday: {day} (range is [1, 7])")
    jan4 = _toordinal(year, 1, 4)
    week1_monday = jan4 - (jan4 + 6) % 7
    return week1_monday + (week - 1) * 7 + (day - 1)


def _calc_julian_from_U_or_W(year, week_of_year, day_of_week, week_starts_Mon):
    """Day of the year for a %U/%W week number and a weekday (Monday == 0)."""
    first_weekday = _weekday(year, 1, 1)
    if not week_starts_Mon:
        first_weekday = (first_weekday + 1) % 7
        day_of_week = (day_of_week + 1) % 7
    week_0_length = (7 - first_weekday) % 7
    if week_of_year == 0:
        return 1 + day_of_week - first_weekday
    return 1 + week_0_length + 7 * (week_of_year - 1) + day_of_week


# ---------------------------------------------------------------------------
# Locale information

class LocaleTime(object):
    """Locale-specific time names, all lower-cased.

    Attributes: f_weekday, a_weekday (Monday first), f_month, a_month (index 0
    is ''), am_pm, LC_date_time, LC_date, LC_time (strftime formats equivalent
    to %c, %x and %X), timezone (pair of frozensets: non-DST and DST names),
    lang (the locale the data was computed for).
    """

    def __init__(self):
        self.lang = _getlang()
        self._compute_weekdays()
        self._compute_months()
        self._compute_am_pm()
        self._compute_date_time()
        self._compute_timezone()
        if _getlang() != self.lang:
            raise ValueError("locale changed during initialization")
        if time.tzname != self.tzname or time.daylight != self.daylight:
            raise ValueError("timezone changed during initialization")

    def _compute_weekdays(self):
        # 2001-01-01 is a Monday
        self.a_weekday = [time.strftime('%a', (2001, 1, 1 + i, 0, 0, 0, i, 1 + i, 0)).lower()
                          for i in range(7)]
        self.f_weekday = [time.strftime('%A', (2001, 1, 1 + i, 0, 0, 0, i, 1 + i, 0)).lower()
                          for i in range(7)]

    def _compute_months(self):
        self.a_month = [''] + [time.strftime('%b', (2001, m, 1, 0, 0, 0, 0, 1, 0)).lower()
                               for m in range(1, 13)]
        self.f_month = [''] + [time.strftime('%B', (2001, m, 1, 0, 0, 0, 0, 1, 0)).lower()
                               for m in range(1, 13)]

    def _compute_am_pm(self):
        self.am_pm = [time.strftime('%p', (1999, 3, 17, hour, 44, 55, 2, 76, 0)).lower()
                      for hour in (1, 22)]

    def _derive_format(self, directive):
        """Turn strftime(directive) of a known date back into a format string."""
        magic = (1999, 3, 17, 22, 44, 55, 2, 76, 0)
        text = time.strftime(directive, magic).lower()
        replacements = [
            (self.f_weekday[2], '%A'), (self.f_month[3], '%B'),
            (self.a_weekday[2], '%a'), (self.a_month[3], '%b'),
            (self.am_pm[1], '%p'),
            ('1999', '%Y'), ('99', '%y'), ('22', '%H'), ('44', '%M'),
            ('55', '%S'), ('76', '%j'), ('17', '%d'), ('03', '%m'),
            ('3', '%m'), ('2', '%w'), ('10', '%I'),
        ]
        replacements = [(old, new) for old, new in replacements if old]
        out = []
        i = 0
        n = len(text)
        while i < n:
            for old, new in replacements:
                if text.startswith(old, i):
                    out.append(new)
                    i += len(old)
                    break
            else:
                ch = text[i]
                out.append('%%' if ch == '%' else ch)
                i += 1
        return ''.join(out)

    def _compute_date_time(self):
        self.LC_date_time = self._derive_format('%c')
        self.LC_date = self._derive_format('%x')
        self.LC_time = self._derive_format('%X')

    def _compute_timezone(self):
        try:
            time.tzset()
        except AttributeError:
            pass
        self.tzname = time.tzname
        self.daylight = time.daylight
        no_saving = frozenset({"utc", "gmt", self.tzname[0].lower()})
        if self.daylight:
            has_saving = frozenset({self.tzname[1].lower()})
        else:
            has_saving = frozenset()
        self.timezone = (no_saving, has_saving)


# ---------------------------------------------------------------------------
# Format -> regular expression

_Z_PATTERN = r"[+-]\d\d:?[0-5]\d(?::?[0-5]\d(?:\.\d{1,6})?)?|(?-i:Z)"
# %:z: the separators are required; a value without them stops matching after
# "+HH:MM" and the rest is reported as a missing colon
_COLON_Z_PATTERN = r"[+-]\d\d:[0-5]\d(?::[0-5]\d(?:\.\d{1,6})?)?|(?-i:Z)"

_MODIFIED = {
    'O': set('deHImMSuUVwWy'),
    'E': set('cCxXyY'),
}


class TimeRE(dict):
    """Map of directive -> regular expression fragment."""

    def __init__(self, locale_time=None):
        self.locale_time = locale_time if locale_time is not None else LocaleTime()
        base = super()
        base.__init__({
            'd': r"(?P<d>3[01]|[12]\d|0[1-9]|[1-9]| [1-9])",
            'e': r"(?P<e>3[01]|[12]\d|0[1-9]|[1-9]| [1-9])",
            'f': r"(?P<f>[0-9]{1,6})",
            'H': r"(?P<H>2[0-3]|[0-1]\d|\d)",
            'I': r"(?P<I>1[0-2]|0[1-9]|[1-9]| [1-9])",
            'G': r"(?P<G>\d\d\d\d)",
            'j': r"(?P<j>36[0-6]|3[0-5]\d|[12]\d\d|0[1-9]\d|00[1-9]|[1-9]\d|0[1-9]|[1-9])",
            'm': r"(?P<m>1[0-2]|0[1-9]|[1-9])",
            'M': r"(?P<M>[0-5]\d|\d)",
            'S': r"(?P<S>6[0-1]|[0-5]\d|\d)",
            'U': r"(?P<U>5[0-3]|[0-4]\d|\d)",
            'w': r"(?P<w>[0-6])",
            'u': r"(?P<u>[1-7])",
            'V': r"(?P<V>5[0-3]|0[1-9]|[1-4]\d|\d)",
            'y': r"(?P<y>\d\d)",
            'Y': r"(?P<Y>\d\d\d\d)",
            'z': r"(?P<z>" + _Z_PATTERN + r")?",
            ':z': r"(?P<colon_z>" + _COLON_Z_PATTERN + r")?",
            'A': self._seq_to_re(self.locale_time.f_weekday, 'A'),
            'a': self._seq_to_re(self.locale_time.a_weekday, 'a'),
            'B': self._seq_to_re(self.locale_time.f_month[1:], 'B'),
            'b': self._seq_to_re(self.locale_time.a_month[1:], 'b'),
            'p': self._seq_to_re(self.locale_time.am_pm, 'p'),
            'Z': self._seq_to_re((tz for tz_names in self.locale_time.timezone
                                  for tz in tz_names), 'Z'),
            'n': r"\s*",
            't': r"\s*",
            '%': '%',
        })
        base.__setitem__('W', base.__getitem__('U').replace('U', 'W'))
        base.__setitem__('c', self.pattern(self.locale_time.LC_date_time))
        base.__setitem__('x', self.pattern(self.locale_time.LC_date))
        base.__setitem__('X', self.pattern(self.locale_time.LC_time))
        base.__setitem__('D', self.pattern('%m/%d/%y'))
        base.__setitem__('F', self.pattern('%Y-%m-%d'))
        base.__setitem__('T', self.pattern('%H:%M:%S'))

    def _seq_to_re(self, to_convert, directive):
        """Alternation of the (escaped) values, longest first so that a
        shorter value cannot match a prefix of a longer one."""
        values = sorted({v for v in to_convert if v}, key=len, reverse=True)
        if not values:
            return ''
        body = '|'.join(_re.escape(v) for v in values)
        return '(?P<%s>%s)' % (directive, body)

    # Subclasses that are themselves named TimeRE (pandas' strptime) reach
    # this helper through the private, name-mangled spelling.
    __seqToRE = _seq_to_re

    def _directives(self, format):
        """Split format into ('lit', text) and ('dir', key) items."""
        items = []
        lit = []
        i = 0
        n = len(format)
        while i < n:
            ch = format[i]
            if ch != '%':
                lit.append(ch)
                i += 1
                continue
            if i + 1 >= n or format[i + 1].isspace():
                raise ValueError("stray %% in format '%s'" % format)
            if lit:
                items.append(('lit', ''.join(lit)))
                lit = []
            d = format[i + 1]
            if d == ':' and format[i + 2:i + 3] == 'z':
                items.append(('dir', ':z'))
                i += 3
                continue
            if d == ':':
                # ':' only introduces %:z; name the pair that was found
                bad = (d + format[i + 2:i + 3]).rstrip()
                raise ValueError("'%s' is a bad directive in format '%s'" % (bad, format))
            if d in _MODIFIED:
                nxt = format[i + 2:i + 3]
                if nxt and nxt in _MODIFIED[d]:
                    items.append(('dir', nxt))
                    i += 3
                    continue
                bad = (d + nxt).rstrip()
                raise ValueError("'%s' is a bad directive in format '%s'" % (bad, format))
            if d not in self:
                raise ValueError("'%s' is a bad directive in format '%s'" % (d, format))
            items.append(('dir', d))
            i += 2
        if lit:
            items.append(('lit', ''.join(lit)))
        return items

    def pattern(self, format):
        """Regular expression source equivalent to format."""
        out = []
        for kind, value in self._directives(format):
            if kind == 'dir':
                out.append(self[value])
            else:
                piece = _re.escape(value)
                out.append(_re.sub(r'(?:\\\s|\s)+', lambda m: r'\s+', piece))
        return ''.join(out)

    def compile(self, format):
        """Compiled, case-insensitive regex for format."""
        return re_compile(self.pattern(format), IGNORECASE)


_cache_lock = _thread_allocate_lock()
_TimeRE_cache = None
_CACHE_MAX_SIZE = 5
_regex_cache = {}

_YEAR_DIRECTIVES = frozenset('YyGcxD') | {'F'}


def _check_day_without_year(format, items):
    keys = {value for kind, value in items if kind == 'dir'}
    if keys & _YEAR_DIRECTIVES:
        return
    if 'd' in keys:
        raise ValueError(
            "Parsing a day of month without a year specified is ambiguous "
            "and fails to parse leap day; add a year to the input and format "
            "(see https://github.com/python/cpython/issues/70647)")
    if 'e' in keys:
        import warnings
        warnings.warn(
            "Parsing dates involving a day of month without a year specified "
            "is ambiguous and fails to parse leap day. The default behavior "
            "will change in Python 3.17 to either always raise an exception "
            "or to use a different default year (TBD). To avoid trouble, add "
            "a specific year to the input & format. "
            "See https://github.com/python/cpython/issues/70647.",
            DeprecationWarning,
            skip_file_prefixes=(__file__,) if __file__ else ())


def _get_regex(format):
    global _TimeRE_cache
    with _cache_lock:
        locale_time = _TimeRE_cache.locale_time if _TimeRE_cache is not None else None
        if (_TimeRE_cache is None or _getlang() != locale_time.lang or
                time.tzname != locale_time.tzname or
                time.daylight != locale_time.daylight):
            _TimeRE_cache = TimeRE()
            _regex_cache.clear()
        regex = _regex_cache.get(format)
        if regex is None:
            items = _TimeRE_cache._directives(format)
            _check_day_without_year(format, items)
            regex = _TimeRE_cache.compile(format)
            if len(_regex_cache) > _CACHE_MAX_SIZE:
                _regex_cache.clear()
            _regex_cache[format] = regex
        return regex, _TimeRE_cache.locale_time


def _parse_offset(z, colon):
    """Return (seconds, microsecond fraction) for a %z / %:z value."""
    if z == 'Z':
        return 0, 0
    original = z
    if colon:
        if z[3:4] != ':':
            raise ValueError(f"Missing colon in %:z before '{z[3:5]}', got '{original}'")
        z = z[:3] + z[4:]
        if len(z) > 5:
            if z[5] != ':':
                raise ValueError(f"Missing colon in %:z before '{z[5:7]}', got '{original}'")
            z = z[:5] + z[6:]
    elif z[3:4] == ':':
        z = z[:3] + z[4:]
        if len(z) > 5:
            if z[5] != ':':
                raise ValueError(f"Inconsistent use of : in {original}")
            z = z[:5] + z[6:]
    hours = int(z[1:3])
    minutes = int(z[3:5])
    seconds = int(z[5:7] or 0)
    if len(z) > 7 and z[7] != '.':
        raise ValueError(f"Inconsistent use of : in {original}")
    gmtoff = hours * 3600 + minutes * 60 + seconds
    rest = z[8:]
    fraction = int(rest + '0' * (6 - len(rest))) if rest else 0
    if z.startswith('-'):
        return -gmtoff, -fraction
    return gmtoff, fraction


def _strptime(data_string, format="%a %b %d %H:%M:%S %Y"):
    """Return a 3-tuple: (time tuple + (tzname, gmtoff), microseconds,
    gmtoff microseconds)."""
    for index, arg in enumerate([data_string, format]):
        if not isinstance(arg, str):
            msg = "strptime() argument {} must be str, not {}"
            raise TypeError(msg.format(index, type(arg)))

    try:
        format_regex, locale_time = _get_regex(format)
    except ValueError as err:
        raise ValueError(*err.args) from None

    found = format_regex.match(data_string)
    if not found:
        raise ValueError("time data %r does not match format %r" % (data_string, format))
    if len(data_string) != found.end():
        rest = data_string[found.end():]
        if ('colon_z' in found.re.groupindex
                and found.group('colon_z') is not None and rest[0] != ':'):
            raise ValueError(f"Missing colon in %:z before '{rest}', got "
                             f"'{data_string}'")
        raise ValueError("unconverted data remains: %s" % rest)

    iso_year = year = None
    month = day = 1
    hour = minute = second = fraction = 0
    tz = -1
    gmtoff = None
    gmtoff_fraction = 0
    iso_week = week_of_year = None
    week_of_year_start = None
    weekday = julian = None
    found_dict = found.groupdict()
    for key, value in found_dict.items():
        if value is None:
            continue
        if key == 'y':
            year = int(value)
            year += 2000 if year <= 68 else 1900
        elif key == 'Y':
            year = int(value)
        elif key == 'G':
            iso_year = int(value)
        elif key == 'm':
            month = int(value)
        elif key == 'B':
            month = locale_time.f_month.index(value.lower())
        elif key == 'b':
            month = locale_time.a_month.index(value.lower())
        elif key in ('d', 'e'):
            day = int(value)
        elif key == 'H':
            hour = int(value)
        elif key == 'I':
            hour = int(value)
            ampm = (found_dict.get('p') or '').lower()
            if ampm in ('', locale_time.am_pm[0]):
                if hour == 12:
                    hour = 0
            elif ampm == locale_time.am_pm[1]:
                if hour != 12:
                    hour += 12
        elif key == 'M':
            minute = int(value)
        elif key == 'S':
            second = int(value)
        elif key == 'f':
            fraction = int(value + '0' * (6 - len(value)))
        elif key == 'A':
            weekday = locale_time.f_weekday.index(value.lower())
        elif key == 'a':
            weekday = locale_time.a_weekday.index(value.lower())
        elif key == 'w':
            weekday = int(value)
            weekday = 6 if weekday == 0 else weekday - 1
        elif key == 'u':
            weekday = int(value) - 1
        elif key == 'j':
            julian = int(value)
        elif key in ('U', 'W'):
            week_of_year = int(value)
            week_of_year_start = 6 if key == 'U' else 0
        elif key == 'V':
            iso_week = int(value)
        elif key in ('z', 'colon_z'):
            gmtoff, gmtoff_fraction = _parse_offset(value, key == 'colon_z')
        elif key == 'Z':
            found_zone = value.lower()
            for tz_value, tz_names in enumerate(locale_time.timezone):
                if found_zone in tz_names:
                    if (time.tzname[0] == time.tzname[1] and time.daylight
                            and found_zone not in ("utc", "gmt")):
                        break
                    tz = tz_value
                    break

    if iso_year is not None:
        if julian is not None:
            raise ValueError("Day of the year directive '%j' is not compatible "
                             "with ISO year directive '%G'. Use '%Y' instead.")
        if iso_week is None or weekday is None:
            raise ValueError("ISO year directive '%G' must be used with the ISO "
                             "week directive '%V' and a weekday directive "
                             "('%A', '%a', '%w', or '%u').")
    elif iso_week is not None:
        if year is None or weekday is None:
            raise ValueError("ISO week directive '%V' must be used with the ISO "
                             "year directive '%G' and a weekday directive "
                             "('%A', '%a', '%w', or '%u').")
        raise ValueError("ISO week directive '%V' is incompatible with the "
                         "year directive '%Y'. Use the ISO year '%G' instead.")

    leap_year_fix = False
    if year is None:
        if month == 2 and day == 29:
            year = 1904  # any leap year: the result's year is reset below
            leap_year_fix = True
        else:
            year = 1900

    if julian is None and weekday is not None:
        if week_of_year is not None:
            julian = _calc_julian_from_U_or_W(year, week_of_year, weekday,
                                              week_of_year_start == 0)
        elif iso_year is not None and iso_week is not None:
            ordinal = _iso_to_ordinal(iso_year, iso_week, weekday + 1)
            year, month, day = _fromordinal(ordinal)
            julian = ordinal - _toordinal(year, 1, 1) + 1
        if julian is not None and julian <= 0:
            year -= 1
            julian += 366 if _is_leap(year) else 365

    if julian is None:
        julian = _toordinal(year, month, day) - _toordinal(year, 1, 1) + 1
    else:
        year, month, day = _fromordinal(julian - 1 + _toordinal(year, 1, 1))
    if weekday is None:
        weekday = _weekday(year, month, day)

    tzname = found_dict.get("Z")
    if leap_year_fix:
        year = 1900

    return ((year, month, day, hour, minute, second, weekday, julian, tz,
             tzname, gmtoff), fraction, gmtoff_fraction)


def _strptime_time(data_string, format="%a %b %d %H:%M:%S %Y"):
    """time.strptime(): return a struct_time."""
    tt = _strptime(data_string, format)[0]
    return time.struct_time(tt[:time._STRUCT_TM_ITEMS])


def _make_tz(tt, gmtoff_fraction):
    import datetime
    tzname, gmtoff = tt[-2:]
    if gmtoff is None:
        return None
    delta = datetime.timedelta(seconds=gmtoff, microseconds=gmtoff_fraction)
    if tzname:
        return datetime.timezone(delta, tzname)
    return datetime.timezone(delta)


def _strptime_datetime_date(cls, data_string, format="%a %b %d %Y"):
    """datetime.date.strptime()."""
    tt, _, _ = _strptime(data_string, format)
    return cls(*tt[:3])


def _strptime_datetime_time(cls, data_string, format="%H:%M:%S"):
    """datetime.time.strptime()."""
    tt, fraction, gmtoff_fraction = _strptime(data_string, format)
    args = tt[3:6] + (fraction,)
    tz = _make_tz(tt, gmtoff_fraction)
    if tz is not None:
        args += (tz,)
    return cls(*args)


def _strptime_datetime_datetime(cls, data_string, format="%a %b %d %H:%M:%S %Y"):
    """datetime.datetime.strptime()."""
    tt, fraction, gmtoff_fraction = _strptime(data_string, format)
    args = tt[:6] + (fraction,)
    tz = _make_tz(tt, gmtoff_fraction)
    if tz is not None:
        args += (tz,)
    return cls(*args)


# Name used by older datetime implementations.
_strptime_datetime = _strptime_datetime_datetime
