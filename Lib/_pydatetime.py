"""Pure-Python implementation of the datetime module.

This file is also the method source of the native ``_datetime`` module:
``_datetime`` executes this code and then rebinds the class names to its
native types (which have the C struct layout of Include/datetime.h), so
every method below must reach the instance fields only through the
``_year``/``_month``/... attributes and build new objects only through the
public constructors (or the ``_new_*`` helpers).
"""

__all__ = ("date", "datetime", "time", "timedelta", "timezone", "tzinfo",
           "MINYEAR", "MAXYEAR", "UTC")

import time as _time
import math as _math
import sys
from operator import index as _index

MINYEAR = 1
MAXYEAR = 9999
_MAXORDINAL = 3652059          # date.max.toordinal()

_DAYS_IN_MONTH = (0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
# _CUMULATIVE_DAYS[m] = days in the months before month m (non-leap year)
_CUMULATIVE_DAYS = (0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334)

_MONTHNAMES = (None, "Jan", "Feb", "Mar", "Apr", "May", "Jun",
               "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
_DAYNAMES = (None, "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


def _module_label(obj):
    mod = type(obj).__module__
    return 'datetime' if mod in ('_pydatetime', '_datetime') else mod


def _qualified(obj):
    tp = type(obj)
    if tp.__module__ in ('datetime', '_pydatetime', '_datetime'):
        return 'datetime.' + tp.__qualname__
    return tp.__qualname__


# ---------------------------------------------------------------------------
# Calendar arithmetic (proleptic Gregorian, ordinal 1 == 0001-01-01)

def _is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def _days_before_year(year):
    y = year - 1
    return y * 365 + y // 4 - y // 100 + y // 400


def _days_in_month(year, month):
    if month == 2 and _is_leap(year):
        return 29
    return _DAYS_IN_MONTH[month]


def _days_before_month(year, month):
    return _CUMULATIVE_DAYS[month] + (month > 2 and _is_leap(year))


def _ymd2ord(year, month, day):
    return _days_before_year(year) + _days_before_month(year, month) + day


_DAYS_PER_400Y = 146097
_DAYS_PER_100Y = 36524
_DAYS_PER_4Y = 1461


def _ord2ymd(n):
    """Inverse of _ymd2ord."""
    n -= 1
    q400, n = divmod(n, _DAYS_PER_400Y)
    year = q400 * 400 + 1
    q100, n = divmod(n, _DAYS_PER_100Y)
    q4, n = divmod(n, _DAYS_PER_4Y)
    q1, n = divmod(n, 365)
    year += q100 * 100 + q4 * 4 + q1
    if q1 == 4 or q100 == 4:
        # the last day of a leap cycle
        return year - 1, 12, 31
    leap = q1 == 3 and (q4 != 24 or q100 == 3)
    month = (n + 50) >> 5
    before = _CUMULATIVE_DAYS[month] + (month > 2 and leap)
    if before > n:
        month -= 1
        before -= _DAYS_IN_MONTH[month] + (month == 2 and leap)
    return year, month, n - before + 1


def _iso_week1_monday(year):
    first = _ymd2ord(year, 1, 1)
    weekday = (first + 6) % 7          # Monday == 0
    monday = first - weekday
    if weekday > 3:                    # Jan 1 is Fri..Sun: week 1 starts later
        monday += 7
    return monday


def _build_struct_time(y, m, d, hh, mm, ss, dstflag):
    wday = (_ymd2ord(y, m, d) + 6) % 7
    yday = _days_before_month(y, m) + d
    return _time.struct_time((y, m, d, hh, mm, ss, wday, yday, dstflag))


# ---------------------------------------------------------------------------
# Argument validation

def _int_arg(value):
    if isinstance(value, float):
        raise TypeError('integer argument expected, got float')
    try:
        value = _index(value)
    except TypeError:
        raise TypeError('an integer is required (got type %s)' %
                        type(value).__name__) from None
    if not isinstance(value, int):
        raise TypeError('__index__ returned non-int (type %s)' %
                        type(value).__name__)
    return value


def _range(name, value, lo, hi):
    if not lo <= value <= hi:
        raise ValueError(f"{name} must be in {lo}..{hi}, not {value}")


def _check_date_fields(year, month, day):
    year, month, day = _int_arg(year), _int_arg(month), _int_arg(day)
    _range('year', year, MINYEAR, MAXYEAR)
    _range('month', month, 1, 12)
    last = _days_in_month(year, month)
    if not 1 <= day <= last:
        raise ValueError(f"day {day} must be in range 1..{last} for month "
                         f"{month} in year {year}")
    return year, month, day


def _check_time_fields(hour, minute, second, microsecond, fold):
    hour, minute = _int_arg(hour), _int_arg(minute)
    second, microsecond = _int_arg(second), _int_arg(microsecond)
    _range('hour', hour, 0, 23)
    _range('minute', minute, 0, 59)
    _range('second', second, 0, 59)
    _range('microsecond', microsecond, 0, 999999)
    if fold not in (0, 1):
        raise ValueError('fold must be either 0 or 1', fold)
    return hour, minute, second, microsecond, fold


def _check_tzinfo_arg(tz):
    if tz is not None and not isinstance(tz, tzinfo):
        raise TypeError("tzinfo argument must be None or of a tzinfo "
                        "subclass, not type '%s'" % type(tz).__name__)


def _check_utc_offset(name, offset):
    """Validate the result of utcoffset()/dst()."""
    if offset is None:
        return
    if not isinstance(offset, timedelta):
        raise TypeError("tzinfo.%s() must return None or timedelta, "
                        "not '%s'" % (name, type(offset).__name__))
    if not -timedelta(1) < offset < timedelta(1):
        raise ValueError("offset must be a timedelta strictly between "
                         "-timedelta(hours=24) and timedelta(hours=24), "
                         "not %r" % (offset,))


def _check_tzname(name):
    if name is not None and not isinstance(name, str):
        raise TypeError("tzinfo.tzname() must return None or string, "
                        "not '%s'" % type(name).__name__)


def _cmp(x, y):
    return 0 if x == y else (1 if x > y else -1)


def _divide_and_round(a, b):
    """a / b rounded half to even (b may be negative)."""
    q, r = divmod(a, b)
    r2 = r * 2
    if (r2 > b) if b > 0 else (r2 < b):
        q += 1
    elif r2 == b and q % 2 == 1:
        q += 1
    return q


# ---------------------------------------------------------------------------
# Formatting helpers

def _format_time(hh, mm, ss, us, timespec='auto'):
    if timespec == 'auto':
        timespec = 'microseconds' if us else 'seconds'
    elif timespec == 'milliseconds':
        us //= 1000
    if timespec == 'hours':
        return '%02d' % hh
    if timespec == 'minutes':
        return '%02d:%02d' % (hh, mm)
    if timespec == 'seconds':
        return '%02d:%02d:%02d' % (hh, mm, ss)
    if timespec == 'milliseconds':
        return '%02d:%02d:%02d.%03d' % (hh, mm, ss, us)
    if timespec == 'microseconds':
        return '%02d:%02d:%02d.%06d' % (hh, mm, ss, us)
    raise ValueError('Unknown timespec value')


def _format_offset(off, sep=':'):
    """'+HH:MM[:SS[.ffffff]]' for a utcoffset timedelta (or '' for None)."""
    if off is None:
        return ''
    sign = '+'
    if off.days < 0:
        sign = '-'
        off = -off
    hh, rest = divmod(off, timedelta(hours=1))
    mm, rest = divmod(rest, timedelta(minutes=1))
    text = '%s%02d%s%02d' % (sign, hh, sep, mm)
    if rest.seconds or rest.microseconds:
        text += '%s%02d' % (sep, rest.seconds)
        if rest.microseconds:
            text += '.%06d' % rest.microseconds
    return text


def _strftime_expand(obj, fmt, timetuple):
    """strftime with the %f, %z, %:z and %Z directives resolved first."""
    if not isinstance(fmt, str):
        raise TypeError("must be str, not %s" % type(fmt).__name__)
    pieces = []
    i, n = 0, len(fmt)
    while i < n:
        ch = fmt[i]
        i += 1
        if ch != '%':
            pieces.append(ch)
            continue
        if i >= n:
            pieces.append('%')
            break
        code = fmt[i]
        i += 1
        if code in 'YGFC' and hasattr(obj, 'year'):
            year = obj.year
            if code == 'G':
                year = obj.isocalendar()[0]
            if code == 'C':
                pieces.append('%02d' % (year // 100))
            elif code == 'F':
                pieces.append('%04d-%02d-%02d' % (year, obj.month, obj.day))
            else:
                pieces.append('%04d' % year)
        elif code == 'f':
            pieces.append('%06d' % getattr(obj, 'microsecond', 0))
        elif code == 'z':
            pieces.append(_format_offset(_utcoffset_of(obj), ''))
        elif code == ':' and i < n and fmt[i] == 'z':
            i += 1
            pieces.append(_format_offset(_utcoffset_of(obj), ':'))
        elif code == 'Z':
            name = ''
            tz = getattr(obj, 'tzinfo', None)
            if tz is not None:
                name = obj.tzname() or ''
            pieces.append(name.replace('%', '%%'))
        else:
            pieces.append('%' + code)
    return _time.strftime(''.join(pieces), timetuple)


def _utcoffset_of(obj):
    if getattr(obj, 'tzinfo', None) is None:
        return None
    return obj.utcoffset()


# ---------------------------------------------------------------------------
# ISO 8601 parsing

def _parse_digits(text, pos, count):
    chunk = text[pos:pos + count]
    if len(chunk) != count or not chunk.isascii() or not chunk.isdigit():
        raise ValueError
    return int(chunk)


def _parse_iso_date(dstr):
    """Parse YYYY-MM-DD, YYYYMMDD, YYYY-Www[-D] or YYYYWww[D]."""
    year = _parse_digits(dstr, 0, 4)
    rest = dstr[4:]
    has_sep = rest[:1] == '-'
    if has_sep:
        rest = rest[1:]
    if rest[:1] == 'W':
        rest = rest[1:]
        week = _parse_digits(rest, 0, 2)
        rest = rest[2:]
        day = 1
        if rest:
            if has_sep:
                if rest[:1] != '-':
                    raise ValueError
                rest = rest[1:]
            if len(rest) != 1:
                raise ValueError
            day = _parse_digits(rest, 0, 1)
        return list(_isocalendar_to_ymd(year, week, day))
    month = _parse_digits(rest, 0, 2)
    rest = rest[2:]
    if has_sep:
        if rest[:1] != '-':
            raise ValueError
        rest = rest[1:]
    if len(rest) != 2:
        raise ValueError
    return [year, month, _parse_digits(rest, 0, 2)]


def _find_iso_date_end(dtstr):
    # Length of the date part of a combined datetime string.
    if len(dtstr) < 7:
        raise ValueError
    if dtstr[4] == '-':
        if dtstr[5] == 'W':
            return 10 if dtstr[8:9] == '-' else 8
        return 10
    if dtstr[4] != 'W':
        return 8
    # Compact week date: YYYYWww or YYYYWwwD.  When both readings are
    # possible, the time that follows must have an even number of digits.
    if not dtstr[7:8].isdigit():
        return 7
    if not dtstr[8:9].isdigit():
        return 8
    run = 0
    while dtstr[8 + run:9 + run].isdigit():
        run += 1
    return 7 if run % 2 == 0 else 8


def _parse_iso_time_components(tstr):
    """Parse HH[[:]MM[[:]SS[{.,}ffffff]]] into [h, m, s, us]."""
    comps = [0, 0, 0, 0]
    pos = 0
    has_sep = None
    for idx in range(3):
        comps[idx] = _parse_digits(tstr, pos, 2)
        pos += 2
        if pos >= len(tstr):
            return comps
        nxt = tstr[pos]
        if idx < 2 and nxt == ':':
            if has_sep is False:
                raise ValueError
            has_sep = True
            pos += 1
            continue
        if idx < 2 and nxt.isdigit():
            if has_sep:
                raise ValueError
            has_sep = False
            continue
        break
    if pos < len(tstr):
        if tstr[pos] not in '.,' or idx < 2:
            raise ValueError
        frac = tstr[pos + 1:]
        if not frac or not frac.isascii() or not frac.isdigit():
            raise ValueError
        digits = frac[:6]
        us = int(digits) * 10 ** (6 - len(digits))
        comps[3] = us
    return comps


def _parse_iso_time(tstr):
    """Parse a time with optional UTC offset into [h, m, s, us, tz]."""
    if not tstr:
        raise ValueError
    cut = len(tstr)
    for i, ch in enumerate(tstr):
        if ch in '+-Z':
            cut = i
            break
    comps = _parse_iso_time_components(tstr[:cut])
    tz = None
    tail = tstr[cut:]
    if tail:
        if tail == 'Z':
            tz = timezone.utc
        else:
            if tail[0] == 'Z':
                raise ValueError
            sign = -1 if tail[0] == '-' else 1
            off = tail[1:]
            if len(off) < 8 and len(off) not in (2, 4, 5, 6):
                raise ValueError
            parts = _parse_iso_time_components(off)
            if parts[0] > 23 or parts[1] > 59 or parts[2] > 59:
                raise ValueError
            delta = timedelta(hours=parts[0], minutes=parts[1],
                              seconds=parts[2], microseconds=parts[3])
            tz = timezone.utc if not delta else timezone(sign * delta)
    comps.append(tz)
    return comps


def _isocalendar_to_ymd(year, week, day):
    if not MINYEAR <= year <= MAXYEAR:
        raise ValueError(f"year must be in {MINYEAR}..{MAXYEAR}, not {year}")
    if not 0 < week < 53:
        long_year = (_ymd2ord(year, 1, 1) % 7 == 4 or
                     (_ymd2ord(year, 1, 1) % 7 == 3 and _is_leap(year)))
        if not (week == 53 and long_year):
            raise ValueError(f"Invalid week: {week}")
    if not 0 < day < 8:
        raise ValueError(f"Invalid weekday: {day} (range is [1, 7])")
    ordinal = _iso_week1_monday(year) + (week - 1) * 7 + day - 1
    return _ord2ymd(ordinal)


# ---------------------------------------------------------------------------
# timedelta

_US_PER_SECOND = 1000000
_US_PER_DAY = 86400 * _US_PER_SECOND


def _as_microseconds(value, scale):
    """value * scale microseconds as (int, float remainder) for a number."""
    if isinstance(value, int):
        # int.__pos__ yields a plain int, sidestepping subclass arithmetic
        return int.__pos__(value) * scale, 0.0
    if isinstance(value, float):
        whole = value * scale
        rounded = _math.floor(whole)
        return rounded, whole - rounded
    raise TypeError("unsupported type for timedelta %s component: %s"
                    % (_TD_FIELD_NAMES[scale], type(value).__name__))


_TD_FIELD_NAMES = {
    86400 * 1000000: 'days', 1000000: 'seconds', 1: 'microseconds',
    1000: 'milliseconds', 60 * 1000000: 'minutes',
    3600 * 1000000: 'hours', 7 * 86400 * 1000000: 'weeks',
}


def _timedelta_fields(days=0, seconds=0, microseconds=0,
                      milliseconds=0, minutes=0, hours=0, weeks=0):
    """Normalized (days, seconds, microseconds) for the constructor."""
    total = 0
    frac = 0.0
    for value, scale in ((weeks, 7 * _US_PER_DAY), (days, _US_PER_DAY),
                         (hours, 3600 * _US_PER_SECOND),
                         (minutes, 60 * _US_PER_SECOND),
                         (seconds, _US_PER_SECOND),
                         (milliseconds, 1000), (microseconds, 1)):
        if isinstance(value, int) and value == 0:
            continue
        if isinstance(value, float) and not _math.isfinite(value):
            raise OverflowError("cannot convert float %s to integer"
                                % ('infinity' if value == value else 'NaN'))
        whole, rest = _as_microseconds(value, scale)
        total += whole
        frac += rest
    if frac:
        whole = _math.floor(frac)
        total += whole
        frac -= whole
        # round half to even on the remaining fraction of a microsecond
        if frac > 0.5 or (frac == 0.5 and total % 2 == 1):
            total += 1
    d, rem = divmod(total, _US_PER_DAY)
    s, us = divmod(rem, _US_PER_SECOND)
    if not -999999999 <= d <= 999999999:
        raise OverflowError("days=%d; must have magnitude <= 999999999" % d)
    return d, s, us


class timedelta:
    """A duration, normalised to days, seconds and microseconds.

    Every keyword (weeks, days, hours, minutes, seconds, milliseconds,
    microseconds) defaults to 0 and may be an int or a float, positive or
    negative.
    """
    __slots__ = ('_days', '_seconds', '_microseconds', '_hashcode')

    def __new__(cls, days=0, seconds=0, microseconds=0,
                milliseconds=0, minutes=0, hours=0, weeks=0):
        d, s, us = _timedelta_fields(days, seconds, microseconds,
                                     milliseconds, minutes, hours, weeks)
        self = object.__new__(cls)
        self._days, self._seconds, self._microseconds = d, s, us
        self._hashcode = -1
        return self

    @classmethod
    def _from_us(cls, total):
        d, rem = divmod(total, _US_PER_DAY)
        s, us = divmod(rem, _US_PER_SECOND)
        if not -999999999 <= d <= 999999999:
            raise OverflowError("days=%d; must have magnitude <= 999999999"
                                % d)
        return timedelta(d, s, us)

    def _to_us(self):
        return (self._days * 86400 + self._seconds) * _US_PER_SECOND + \
            self._microseconds

    def __repr__(self):
        parts = []
        if self._days:
            parts.append('days=%d' % self._days)
        if self._seconds:
            parts.append('seconds=%d' % self._seconds)
        if self._microseconds:
            parts.append('microseconds=%d' % self._microseconds)
        if not parts:
            parts.append('0')
        return '%s(%s)' % (_qualified(self), ', '.join(parts))

    def __str__(self):
        mm, ss = divmod(self._seconds, 60)
        hh, mm = divmod(mm, 60)
        text = '%d:%02d:%02d' % (hh, mm, ss)
        if self._days:
            unit = 'day' if abs(self._days) == 1 else 'days'
            text = '%d %s, %s' % (self._days, unit, text)
        if self._microseconds:
            text += '.%06d' % self._microseconds
        return text

    def total_seconds(self):
        """The duration in seconds, as a float."""
        return self._to_us() / 10**6

    @property
    def days(self):
        """Whole days (may be negative)."""
        return self._days

    @property
    def seconds(self):
        """Seconds within the day, 0..86399."""
        return self._seconds

    @property
    def microseconds(self):
        """Microseconds within the second, 0..999999."""
        return self._microseconds

    def __add__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(self._to_us() + other._to_us())
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(self._to_us() - other._to_us())
        return NotImplemented

    def __rsub__(self, other):
        if isinstance(other, timedelta):
            return -self + other
        return NotImplemented

    def __neg__(self):
        return timedelta._from_us(-self._to_us())

    def __pos__(self):
        return timedelta(self._days, self._seconds, self._microseconds)

    def __abs__(self):
        return -self if self._days < 0 else self.__pos__()

    def __mul__(self, other):
        if isinstance(other, int):
            return timedelta._from_us(self._to_us() * other)
        if isinstance(other, float):
            num, den = other.as_integer_ratio()
            return timedelta._from_us(
                _divide_and_round(self._to_us() * num, den))
        return NotImplemented

    __rmul__ = __mul__

    def __floordiv__(self, other):
        if isinstance(other, timedelta):
            return self._to_us() // other._to_us()
        if isinstance(other, int):
            return timedelta._from_us(self._to_us() // other)
        return NotImplemented

    def __truediv__(self, other):
        if isinstance(other, timedelta):
            return self._to_us() / other._to_us()
        if isinstance(other, int):
            return timedelta._from_us(_divide_and_round(self._to_us(), other))
        if isinstance(other, float):
            num, den = other.as_integer_ratio()
            return timedelta._from_us(
                _divide_and_round(den * self._to_us(), num))
        return NotImplemented

    def __mod__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(self._to_us() % other._to_us())
        return NotImplemented

    def __divmod__(self, other):
        if isinstance(other, timedelta):
            q, r = divmod(self._to_us(), other._to_us())
            return q, timedelta._from_us(r)
        return NotImplemented

    def _key(self):
        return (self._days, self._seconds, self._microseconds)

    def __eq__(self, other):
        if isinstance(other, timedelta):
            return self._key() == other._key()
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, timedelta):
            return self._key() < other._key()
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, timedelta):
            return self._key() <= other._key()
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, timedelta):
            return self._key() > other._key()
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, timedelta):
            return self._key() >= other._key()
        return NotImplemented

    def __hash__(self):
        if self._hashcode == -1:
            self._hashcode = hash(self._key())
        return self._hashcode

    def __bool__(self):
        return bool(self._days or self._seconds or self._microseconds)

    def __reduce__(self):
        return (type(self), self._key())


# ---------------------------------------------------------------------------
# date

def _date_state_or_none(year, month, day):
    """Decode the pickle state form date(bytes) or return None."""
    if month is not None or day is not None:
        return None
    if isinstance(year, str):
        if len(year) != 4:
            return None
        try:
            year = year.encode('latin1')
        except UnicodeEncodeError:
            raise ValueError(
                "Failed to encode latin1 string when unpickling a date "
                "object. pickle.load(data, encoding='latin1') is assumed.")
    if isinstance(year, (bytes, bytearray)) and len(year) == 4:
        if 1 <= year[2] <= 12:
            return year
        raise TypeError("bad date state")
    return None


def _date_new_args(year, month=None, day=None):
    state = _date_state_or_none(year, month, day)
    if state is not None:
        return state[0] * 256 + state[1], state[2], state[3]
    return _check_date_fields(year, month, day)


class date:
    """Calendar date: date(year, month, day)."""
    __slots__ = ('_year', '_month', '_day', '_hashcode')

    def __new__(cls, year, month=None, day=None):
        y, m, d = _date_new_args(year, month, day)
        self = object.__new__(cls)
        self._year, self._month, self._day = y, m, d
        self._hashcode = -1
        return self

    # alternative constructors

    @classmethod
    def fromtimestamp(cls, t):
        "The local date of a POSIX timestamp."
        if t is None:
            raise TypeError("'NoneType' object cannot be interpreted as an "
                            "integer")
        y, m, d = _time.localtime(t)[:3]
        return cls(y, m, d)

    @classmethod
    def today(cls):
        "Today's local date."
        return cls.fromtimestamp(_time.time())

    @classmethod
    def fromordinal(cls, n):
        """The date with the given day number (0001-01-01 is day 1)."""
        n = _int_arg(n)
        if not 1 <= n <= _MAXORDINAL:
            raise ValueError("ordinal must be in 1..%d, not %d"
                             % (_MAXORDINAL, n))
        return cls(*_ord2ymd(n))

    @classmethod
    def fromisoformat(cls, date_string):
        """Parse ISO 8601 date text."""
        if not isinstance(date_string, str):
            raise TypeError('fromisoformat: argument must be str')
        if len(date_string) not in (7, 8, 10):
            raise ValueError(f'Invalid isoformat string: {date_string!r}')
        try:
            ymd = _parse_iso_date(date_string)
        except ValueError:
            raise ValueError(
                f'Invalid isoformat string: {date_string!r}') from None
        return cls(*ymd)

    @classmethod
    def fromisocalendar(cls, year, week, day):
        """The date of an ISO 8601 (year, week, weekday) triple; the
        inverse of isocalendar()."""
        return cls(*_isocalendar_to_ymd(year, week, day))

    @classmethod
    def strptime(cls, date_string, format):
        """Parse *date_string* with a time.strptime() format."""
        import _strptime
        return _strptime._strptime_datetime_date(cls, date_string, format)

    # conversions to string

    def __repr__(self):
        return "%s(%d, %d, %d)" % (_qualified(self), self._year,
                                   self._month, self._day)

    def ctime(self):
        "Text in the style of time.ctime()."
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d 00:00:00 %04d" % (
            _DAYNAMES[weekday], _MONTHNAMES[self._month], self._day,
            self._year)

    def strftime(self, format):
        """Render with a time.strftime() format string."""
        return _strftime_expand(self, format, self.timetuple())

    def __format__(self, fmt):
        if not isinstance(fmt, str):
            raise TypeError("must be str, not %s" % type(fmt).__name__)
        if fmt:
            return self.strftime(fmt)
        return str(self)

    def isoformat(self):
        """The date as YYYY-MM-DD."""
        return "%04d-%02d-%02d" % (self._year, self._month, self._day)

    __str__ = isoformat

    # attributes

    @property
    def year(self):
        """Year, 1..9999."""
        return self._year

    @property
    def month(self):
        """Month, 1..12."""
        return self._month

    @property
    def day(self):
        """Day of the month, 1..31."""
        return self._day

    # standard conversions

    def timetuple(self):
        "The value as a time.struct_time."
        return _build_struct_time(self._year, self._month, self._day,
                                  0, 0, 0, -1)

    def toordinal(self):
        """Day number of the date in the proleptic Gregorian calendar
        (0001-01-01 is day 1)."""
        return _ymd2ord(self._year, self._month, self._day)

    def replace(self, year=None, month=None, day=None):
        """Copy with the given fields changed."""
        if year is None:
            year = self._year
        if month is None:
            month = self._month
        if day is None:
            day = self._day
        return type(self)(year, month, day)

    __replace__ = replace

    # comparisons

    def __eq__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._date_cmp(other) == 0
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._date_cmp(other) <= 0
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._date_cmp(other) < 0
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._date_cmp(other) >= 0
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._date_cmp(other) > 0
        return NotImplemented

    def _date_cmp(self, other):
        return _cmp((self._year, self._month, self._day),
                    (other._year, other._month, other._day))

    def __hash__(self):
        if self._hashcode == -1:
            self._hashcode = hash(self._getstate())
        return self._hashcode

    # arithmetic

    def __add__(self, other):
        if isinstance(other, timedelta):
            o = self.toordinal() + other.days
            if 0 < o <= _MAXORDINAL:
                return type(self).fromordinal(o)
            raise OverflowError("result out of range")
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(-other.days)
        if isinstance(other, date) and not isinstance(other, datetime):
            return timedelta(self.toordinal() - other.toordinal())
        return NotImplemented

    def weekday(self):
        "Weekday number, Monday 0 through Sunday 6."
        return (self.toordinal() + 6) % 7

    def isoweekday(self):
        "ISO weekday number, Monday 1 through Sunday 7."
        return self.toordinal() % 7 or 7

    def isocalendar(self):
        """The ISO 8601 year, week number and weekday as a named
        tuple."""
        year = self._year
        today = self.toordinal()
        week1 = _iso_week1_monday(year)
        if today < week1:
            year -= 1
            week1 = _iso_week1_monday(year)
        else:
            nxt = _iso_week1_monday(year + 1)
            if today >= nxt:
                year += 1
                week1 = nxt
        week, day = divmod(today - week1, 7)
        return _IsoCalendarDate(year, week + 1, day + 1)

    # pickling

    def _getstate(self):
        yhi, ylo = divmod(self._year, 256)
        return (bytes([yhi, ylo, self._month, self._day]),)

    def __reduce__(self):
        return (type(self), self._getstate())


class tzinfo:
    """Base class of time zone descriptions used as tzinfo."""
    __slots__ = ()

    def tzname(self, dt):
        "Name of the zone at *dt* (subclasses must override)."
        raise NotImplementedError("tzinfo subclass must override tzname()")

    def utcoffset(self, dt):
        """Offset of local time from UTC at *dt*, east positive (subclasses
        must override)."""
        raise NotImplementedError("tzinfo subclass must override utcoffset()")

    def dst(self, dt):
        """Daylight saving adjustment (a timedelta) already included in
        utcoffset(); zero outside DST, None when unknown."""
        raise NotImplementedError("tzinfo subclass must override dst()")

    def fromutc(self, dt):
        "Convert *dt*, a UTC wall time carrying this tzinfo, to local time."
        if not isinstance(dt, datetime):
            raise TypeError("fromutc() requires a datetime argument")
        if dt.tzinfo is not self:
            raise ValueError("dt.tzinfo is not self")
        offset = dt.utcoffset()
        if offset is None:
            raise ValueError("fromutc() requires a non-None utcoffset() "
                             "result")
        dst = dt.dst()
        if dst is None:
            raise ValueError("fromutc() requires a non-None dst() result")
        standard = offset - dst
        if standard:
            dt += standard
            dst = dt.dst()
            if dst is None:
                raise ValueError("fromutc(): dt.dst gave inconsistent "
                                 "results; cannot convert")
        return dt + dst

    def __reduce__(self):
        getinitargs = getattr(self, "__getinitargs__", None)
        args = getinitargs() if getinitargs else ()
        return (type(self), args, self.__getstate__())


class _IsoCalendarDate(tuple):

    def __new__(cls, year, week, weekday, /):
        return super().__new__(cls, (year, week, weekday))

    @property
    def year(self):
        return self[0]

    @property
    def week(self):
        return self[1]

    @property
    def weekday(self):
        return self[2]

    def __reduce__(self):
        # Pickled as a plain tuple: the class is private.
        return (tuple, (tuple(self),))

    def __repr__(self):
        return (f'{self.__class__.__name__}(year={self[0]}, '
                f'week={self[1]}, weekday={self[2]})')


_IsoCalendarDate.__name__ = 'IsoCalendarDate'
_IsoCalendarDate.__qualname__ = 'IsoCalendarDate'


# ---------------------------------------------------------------------------
# time

def _time_state_or_none(hour, minute, tzinfo_):
    if not isinstance(hour, (bytes, bytearray, str)):
        return None
    if isinstance(hour, str):
        if len(hour) != 6:
            return None
        try:
            hour = hour.encode('latin1')
        except UnicodeEncodeError:
            raise ValueError(
                "Failed to encode latin1 string when unpickling a time "
                "object. pickle.load(data, encoding='latin1') is assumed.")
    if len(hour) != 6 or (hour[0] & 0x7F) >= 24:
        return None
    if not (minute is None or type(minute) is int and minute == 0 or
            isinstance(minute, tzinfo)):
        raise TypeError("bad tzinfo state arg")
    return hour


def _time_new_args(hour=0, minute=0, second=0, microsecond=0, tzinfo=None,
                   *, fold=0):
    state = _time_state_or_none(hour, minute, tzinfo)
    if state is not None:
        h = state[0]
        f = 0
        if h > 127:
            f, h = 1, h - 128
        us = (state[3] << 16) | (state[4] << 8) | state[5]
        tz = minute if minute is not None and type(minute) is not int else None
        return h, state[1], state[2], us, tz, f
    h, mi, s, us, fold = _check_time_fields(hour, minute, second,
                                            microsecond, fold)
    _check_tzinfo_arg(tzinfo)
    return h, mi, s, us, tzinfo, fold


class time:
    """Time of day: time(hour=0, minute=0, second=0, microsecond=0,
    tzinfo=None, *, fold=0)."""
    __slots__ = ('_hour', '_minute', '_second', '_microsecond', '_tzinfo',
                 '_hashcode', '_fold')

    def __new__(cls, hour=0, minute=0, second=0, microsecond=0, tzinfo=None,
                *, fold=0):
        h, mi, s, us, tz, f = _time_new_args(hour, minute, second,
                                             microsecond, tzinfo, fold=fold)
        self = object.__new__(cls)
        self._hour, self._minute, self._second = h, mi, s
        self._microsecond, self._tzinfo, self._fold = us, tz, f
        self._hashcode = -1
        return self

    @classmethod
    def strptime(cls, date_string, format):
        """Parse *date_string* with a time.strptime() format."""
        import _strptime
        return _strptime._strptime_datetime_time(cls, date_string, format)

    @property
    def hour(self):
        """Hour, 0..23."""
        return self._hour

    @property
    def minute(self):
        """Minute, 0..59."""
        return self._minute

    @property
    def second(self):
        """Second, 0..59."""
        return self._second

    @property
    def microsecond(self):
        """Microsecond, 0..999999."""
        return self._microsecond

    @property
    def tzinfo(self):
        """The tzinfo object, or None."""
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    # comparisons

    def __eq__(self, other):
        if isinstance(other, time):
            return self._time_cmp(other, allow_mixed=True) == 0
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, time):
            return self._time_cmp(other) <= 0
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, time):
            return self._time_cmp(other) < 0
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, time):
            return self._time_cmp(other) >= 0
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, time):
            return self._time_cmp(other) > 0
        return NotImplemented

    def _time_cmp(self, other, allow_mixed=False):
        mytz, ottz = self._tzinfo, other._tzinfo
        myoff = otoff = None
        if mytz is ottz:
            same = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            same = myoff == otoff
        mine = (self._hour, self._minute, self._second, self._microsecond)
        theirs = (other._hour, other._minute, other._second,
                  other._microsecond)
        if same:
            return _cmp(mine, theirs)
        if myoff is None or otoff is None:
            if allow_mixed:
                return 2           # naive vs aware: never equal
            raise TypeError("can't compare offset-naive and offset-aware "
                            "times")
        mysecs = (mine[0] * 60 + mine[1]) * 60 + mine[2] - \
            myoff // timedelta(seconds=1)
        otsecs = (theirs[0] * 60 + theirs[1]) * 60 + theirs[2] - \
            otoff // timedelta(seconds=1)
        return _cmp((mysecs, mine[3]), (otsecs, theirs[3]))

    def __hash__(self):
        if self._hashcode == -1:
            t = self.replace(fold=0) if self._fold else self
            off = t.utcoffset()
            if not off:
                h = hash(t._getstate()[0])
            else:
                hh, mm = divmod(timedelta(hours=self._hour,
                                          minutes=self._minute) - off,
                                timedelta(hours=1))
                assert not mm % timedelta(minutes=1), "whole minute"
                mm //= timedelta(minutes=1)
                if 0 <= hh < 24:
                    h = hash(time(hh, mm, self._second, self._microsecond))
                else:
                    h = hash((hh, mm, self._second, self._microsecond))
            self._hashcode = h
        return self._hashcode

    # conversion to string

    def _tzstr(self):
        return _format_offset(self.utcoffset())

    def __repr__(self):
        if self._microsecond:
            s = ", %d, %d" % (self._second, self._microsecond)
        elif self._second:
            s = ", %d" % self._second
        else:
            s = ""
        s = "%s(%d, %d%s)" % (_qualified(self), self._hour, self._minute, s)
        if self._tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self._tzinfo + ")"
        if self._fold:
            s = s[:-1] + ", fold=1)"
        return s

    def isoformat(self, timespec='auto'):
        """ISO 8601 text: HH:MM:SS[.ffffff][+HH:MM].

        *timespec* ('auto', 'hours', 'minutes', 'seconds', 'milliseconds'
        or 'microseconds') selects the precision; 'auto' drops a zero
        fraction.
        """
        s = _format_time(self._hour, self._minute, self._second,
                         self._microsecond, timespec)
        return s + self._tzstr()

    __str__ = isoformat

    @classmethod
    def fromisoformat(cls, time_string):
        """Parse ISO 8601 time text."""
        if not isinstance(time_string, str):
            raise TypeError('fromisoformat: argument must be str')
        text = time_string[1:] if time_string.startswith('T') else \
            time_string
        try:
            h, m, s, us, tz = _parse_iso_time(text)
        except ValueError:
            raise ValueError(
                f'Invalid isoformat string: {time_string!r}') from None
        if h == 24:
            if m or s or us:
                raise ValueError(
                    f'Invalid isoformat string: {time_string!r}')
            h = 0
        return cls(h, m, s, us, tz)

    def strftime(self, format):
        """Render with a time.strftime() format string (date fields are
        those of 1900-01-01)."""
        tt = (1900, 1, 1, self._hour, self._minute, self._second, 0, 1, -1)
        return _strftime_expand(self, format, tt)

    def __format__(self, fmt):
        if not isinstance(fmt, str):
            raise TypeError("must be str, not %s" % type(fmt).__name__)
        if fmt:
            return self.strftime(fmt)
        return str(self)

    # timezone info

    def utcoffset(self):
        """Offset from UTC (east positive) given by tzinfo, or None."""
        if self._tzinfo is None:
            return None
        off = self._tzinfo.utcoffset(None)
        _check_utc_offset("utcoffset", off)
        return off

    def tzname(self):
        """Zone name reported by tzinfo (free-form text), or None."""
        if self._tzinfo is None:
            return None
        name = self._tzinfo.tzname(None)
        _check_tzname(name)
        return name

    def dst(self):
        """Daylight saving adjustment reported by tzinfo, or None."""
        if self._tzinfo is None:
            return None
        off = self._tzinfo.dst(None)
        _check_utc_offset("dst", off)
        return off

    def replace(self, hour=None, minute=None, second=None, microsecond=None,
                tzinfo=True, *, fold=None):
        """Copy with the given fields changed."""
        if hour is None:
            hour = self._hour
        if minute is None:
            minute = self._minute
        if second is None:
            second = self._second
        if microsecond is None:
            microsecond = self._microsecond
        if tzinfo is True:
            tzinfo = self._tzinfo
        if fold is None:
            fold = self._fold
        return type(self)(hour, minute, second, microsecond, tzinfo,
                          fold=fold)

    __replace__ = replace

    # pickle support

    def _getstate(self, protocol=3):
        us2, us3 = divmod(self._microsecond, 256)
        us1, us2 = divmod(us2, 256)
        h = self._hour
        if self._fold and protocol > 3:
            h += 128
        basestate = bytes([h, self._minute, self._second, us1, us2, us3])
        if self._tzinfo is None:
            return (basestate,)
        return (basestate, self._tzinfo)

    def __reduce_ex__(self, protocol):
        return (type(self), self._getstate(protocol))

    def __reduce__(self):
        return self.__reduce_ex__(2)


# ---------------------------------------------------------------------------
# datetime

def _datetime_state_or_none(year, month, tzinfo_):
    if not isinstance(year, (bytes, bytearray, str)):
        return None
    if isinstance(year, str):
        if len(year) != 10:
            return None
        try:
            year = year.encode('latin1')
        except UnicodeEncodeError:
            raise ValueError(
                "Failed to encode latin1 string when unpickling a datetime "
                "object. pickle.load(data, encoding='latin1') is assumed.")
    if len(year) != 10 or not 1 <= (year[2] & 0x7F) <= 12:
        return None
    if month is not None and not isinstance(month, tzinfo):
        raise TypeError("bad tzinfo state arg")
    return year


def _datetime_new_args(year, month=None, day=None, hour=0, minute=0,
                       second=0, microsecond=0, tzinfo=None, *, fold=0):
    state = _datetime_state_or_none(year, month, tzinfo)
    if state is not None:
        m = state[2]
        f = 0
        if m > 127:
            f, m = 1, m - 128
        us = (state[7] << 16) | (state[8] << 8) | state[9]
        return (state[0] * 256 + state[1], m, state[3], state[4], state[5],
                state[6], us, month, f)
    y, m, d = _check_date_fields(year, month, day)
    h, mi, s, us, fold = _check_time_fields(hour, minute, second,
                                            microsecond, fold)
    _check_tzinfo_arg(tzinfo)
    return y, m, d, h, mi, s, us, tzinfo, fold


def _local_offset(u):
    """Seconds east of UTC of the local time zone at POSIX time u."""
    tm = _time.localtime(u)
    local = (_ymd2ord(*tm[:3]) - 719163) * 86400 + \
        (tm[3] * 60 + tm[4]) * 60 + tm[5]
    return local - u


def _is_date(obj):
    return isinstance(obj, date)


def _is_time(obj):
    return isinstance(obj, time)


class datetime(date):
    """Date and time of day: datetime(year, month, day, hour=0, minute=0,
    second=0, microsecond=0, tzinfo=None, *, fold=0)."""
    __slots__ = ('_hour', '_minute', '_second', '_microsecond', '_tzinfo',
                 '_fold')

    def __new__(cls, year, month=None, day=None, hour=0, minute=0, second=0,
                microsecond=0, tzinfo=None, *, fold=0):
        (y, m, d, h, mi, s, us, tz, f) = _datetime_new_args(
            year, month, day, hour, minute, second, microsecond, tzinfo,
            fold=fold)
        self = object.__new__(cls)
        self._year, self._month, self._day = y, m, d
        self._hour, self._minute, self._second = h, mi, s
        self._microsecond, self._tzinfo, self._fold = us, tz, f
        self._hashcode = -1
        return self

    @property
    def hour(self):
        """Hour, 0..23."""
        return self._hour

    @property
    def minute(self):
        """Minute, 0..59."""
        return self._minute

    @property
    def second(self):
        """Second, 0..59."""
        return self._second

    @property
    def microsecond(self):
        """Microsecond, 0..999999."""
        return self._microsecond

    @property
    def tzinfo(self):
        """The tzinfo object, or None."""
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    @classmethod
    def _from_posix(cls, t, utc, tz):
        """Build from a POSIX timestamp."""
        frac, whole = _math.modf(t)
        us = round(frac * 1e6)
        if us >= 1000000:
            whole += 1
            us -= 1000000
        elif us < 0:
            whole -= 1
            us += 1000000
        whole = int(whole)
        convert = _time.gmtime if utc else _time.localtime
        y, m, d, hh, mm, ss = convert(whole)[:6]
        ss = min(ss, 59)          # leap seconds are clamped
        result = cls(y, m, d, hh, mm, ss, us, tz)
        if tz is None and not utc:
            # The same wall time occurs twice if the offset 24h earlier is
            # larger and the earlier instant maps to the same wall time.
            try:
                now_off = _local_offset(whole)
                before = _local_offset(whole - 86400)
            except (OverflowError, OSError, ValueError):
                return result
            gap = before - now_off
            if gap > 0:
                earlier = convert(whole - gap)[:6]
                if tuple(earlier) == (y, m, d, hh, mm, ss):
                    result = result.replace(fold=1)
        elif tz is not None:
            result = tz.fromutc(result)
        return result

    @classmethod
    def fromtimestamp(cls, timestamp, tz=None):
        """The local (or, with *tz*, zone) time of a POSIX timestamp."""
        _check_tzinfo_arg(tz)
        if timestamp is None:
            raise TypeError("'NoneType' object cannot be interpreted as an "
                            "integer")
        return cls._from_posix(timestamp, tz is not None, tz)

    @classmethod
    def utcfromtimestamp(cls, t):
        """Naive UTC datetime of a POSIX timestamp."""
        import warnings
        warnings.warn("datetime.datetime.utcfromtimestamp() is deprecated "
                      "and scheduled for removal in a future version. Use "
                      "timezone-aware objects to represent datetimes in UTC: "
                      "datetime.datetime.fromtimestamp(t, datetime.UTC).",
                      DeprecationWarning, stacklevel=2)
        return cls._from_posix(t, True, None)

    @classmethod
    def now(cls, tz=None):
        "The current local (or *tz*) date and time."
        t = _time.time()
        return cls.fromtimestamp(t, tz)

    @classmethod
    def utcnow(cls):
        "The current UTC date and time, naive."
        import warnings
        warnings.warn("datetime.datetime.utcnow() is deprecated and "
                      "scheduled for removal in a future version. Use "
                      "timezone-aware objects to represent datetimes in UTC: "
                      "datetime.datetime.now(datetime.UTC).",
                      DeprecationWarning, stacklevel=2)
        t = _time.time()
        return cls._from_posix(t, True, None)

    @classmethod
    def combine(cls, date, time, tzinfo=True):
        "Join a date and a time into a datetime."
        date_, time_ = date, time
        if not _is_date(date_):
            raise TypeError("combine() argument 1 must be datetime.date, "
                            "not %s" % type(date_).__name__)
        if not _is_time(time_):
            raise TypeError("combine() argument 2 must be datetime.time, "
                            "not %s" % type(time_).__name__)
        if tzinfo is True:
            tzinfo = time_.tzinfo
        return cls(date_.year, date_.month, date_.day,
                   time_.hour, time_.minute, time_.second, time_.microsecond,
                   tzinfo, fold=time_.fold)

    @classmethod
    def fromisoformat(cls, date_string):
        """Parse ISO 8601 date-and-time text."""
        if not isinstance(date_string, str):
            raise TypeError('fromisoformat: argument must be str')
        if len(date_string) < 7:
            raise ValueError(f'Invalid isoformat string: {date_string!r}')
        try:
            end = _find_iso_date_end(date_string)
            ymd = _parse_iso_date(date_string[:end])
        except ValueError:
            raise ValueError(
                f'Invalid isoformat string: {date_string!r}') from None
        rest = date_string[end + 1:]
        if end < len(date_string):
            if not rest:
                raise ValueError(
                    f'Invalid isoformat string: {date_string!r}')
            try:
                tparts = _parse_iso_time(rest)
            except ValueError:
                raise ValueError(
                    f'Invalid isoformat string: {date_string!r}') from None
        else:
            tparts = [0, 0, 0, 0, None]
        y, m, d = ymd
        h = tparts[0]
        next_day = False
        if h == 24:
            if any(tparts[1:4]):
                raise ValueError(
                    f'Invalid isoformat string: {date_string!r}')
            next_day = True
            h = 0
        # validate the fields (with their own error messages)
        _check_date_fields(y, m, d)
        if next_day and (y, m, d) == (MAXYEAR, 12, 31):
            # rolling over past the last representable day
            _check_date_fields(MAXYEAR + 1, 1, 1)
        result = cls(y, m, d, h, tparts[1], tparts[2], tparts[3], tparts[4])
        if next_day:
            result += timedelta(days=1)
        return result

    def timetuple(self):
        "The value as a time.struct_time."
        dst = self.dst()
        if dst is None:
            flag = -1
        else:
            flag = 1 if dst else 0
        return _build_struct_time(self._year, self._month, self._day,
                                  self._hour, self._minute, self._second,
                                  flag)

    def _local_timestamp(self):
        """POSIX time of this naive datetime read as local time."""
        epoch_days = _ymd2ord(self._year, self._month, self._day) - 719163
        wall = epoch_days * 86400 + (self._hour * 60 + self._minute) * 60 + \
            self._second
        candidates = set()
        for probe in (wall - 86400, wall, wall + 86400):
            u = wall - _local_offset(probe)
            if u + _local_offset(u) == wall:
                candidates.add(u)
        if candidates:
            ordered = sorted(candidates)
            return ordered[-1] if self._fold else ordered[0]
        # In a gap: fold=0 uses the offset from before the transition.
        before = _local_offset(wall - 86400)
        after = _local_offset(wall + 86400)
        return wall - (after if self._fold else before)

    def timestamp(self):
        "Seconds since the POSIX epoch, as a float."
        if self._tzinfo is None:
            s = self._local_timestamp()
            return s + self._microsecond / 1e6
        return (self - _EPOCH).total_seconds()

    def utctimetuple(self):
        "The UTC value as a time.struct_time."
        offset = self.utcoffset()
        if offset:
            self -= offset
        return _build_struct_time(self._year, self._month, self._day,
                                  self._hour, self._minute, self._second, 0)

    def date(self):
        "The date portion."
        return date(self._year, self._month, self._day)

    def time(self):
        "The time of day, without tzinfo."
        return time(self._hour, self._minute, self._second,
                    self._microsecond, fold=self._fold)

    def timetz(self):
        "The time of day, keeping tzinfo."
        return time(self._hour, self._minute, self._second,
                    self._microsecond, self._tzinfo, fold=self._fold)

    def replace(self, year=None, month=None, day=None, hour=None,
                minute=None, second=None, microsecond=None, tzinfo=True,
                *, fold=None):
        """Copy with the given fields changed."""
        if year is None:
            year = self._year
        if month is None:
            month = self._month
        if day is None:
            day = self._day
        if hour is None:
            hour = self._hour
        if minute is None:
            minute = self._minute
        if second is None:
            second = self._second
        if microsecond is None:
            microsecond = self._microsecond
        if tzinfo is True:
            tzinfo = self._tzinfo
        if fold is None:
            fold = self._fold
        return type(self)(year, month, day, hour, minute, second,
                          microsecond, tzinfo, fold=fold)

    __replace__ = replace

    def _local_timezone(self):
        if self._tzinfo is None:
            ts = self._local_timestamp()
        else:
            ts = (self - _EPOCH) // timedelta(seconds=1)
        localtm = _time.localtime(ts)
        local = datetime(*localtm[:6])
        gmtoff = getattr(localtm, 'tm_gmtoff', None)
        zone = getattr(localtm, 'tm_zone', None)
        if gmtoff is None:
            gmtoff = (local - datetime(*_time.gmtime(ts)[:6])) // \
                timedelta(seconds=1)
        return timezone(timedelta(seconds=gmtoff), zone)

    def astimezone(self, tz=None):
        if tz is None:
            tz = self._local_timezone()
        elif not isinstance(tz, tzinfo):
            raise TypeError("tz argument must be an instance of tzinfo")
        mytz = self._tzinfo
        myoffset = None if mytz is None else mytz.utcoffset(self)
        if myoffset is None:
            # naive (or offset-less) values are read as local wall time
            naive = self.replace(tzinfo=None)
            wall_days = _ymd2ord(self._year, self._month, self._day) - 719163
            secs = wall_days * 86400 + \
                (self._hour * 60 + self._minute) * 60 + self._second
            myoffset = timedelta(seconds=secs - naive._local_timestamp())
        elif tz is mytz:
            return self
        utc = (self - myoffset).replace(tzinfo=tz)
        return tz.fromutc(utc)

    # string conversions

    def ctime(self):
        "Text in the style of time.ctime()."
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d %02d:%02d:%02d %04d" % (
            _DAYNAMES[weekday], _MONTHNAMES[self._month], self._day,
            self._hour, self._minute, self._second, self._year)

    def isoformat(self, sep='T', timespec='auto'):
        """ISO 8601 text: YYYY-MM-DD<sep>HH:MM:SS[.ffffff][+HH:MM].

        *sep* separates date and time; *timespec* selects the precision as
        for time.isoformat().
        """
        if not isinstance(sep, str) or len(sep) != 1:
            raise TypeError("isoformat() argument 1 must be a unicode "
                            "character, not %s" % type(sep).__name__)
        s = "%04d-%02d-%02d%s" % (self._year, self._month, self._day, sep)
        s += _format_time(self._hour, self._minute, self._second,
                          self._microsecond, timespec)
        return s + _format_offset(self.utcoffset())

    def __repr__(self):
        fields = [self._year, self._month, self._day, self._hour,
                  self._minute, self._second, self._microsecond]
        while len(fields) > 5 and fields[-1] == 0:
            fields.pop()
        s = "%s(%s)" % (_qualified(self), ", ".join(map(str, fields)))
        if self._tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self._tzinfo + ")"
        if self._fold:
            s = s[:-1] + ", fold=1)"
        return s

    def __str__(self):
        "isoformat() with a space separator."
        return self.isoformat(sep=' ')

    @classmethod
    def strptime(cls, date_string, format):
        'string, format -> new datetime parsed from a string (like time.strptime()).'
        import _strptime
        return _strptime._strptime_datetime_datetime(cls, date_string, format)

    def strftime(self, format):
        """Render with a time.strftime() format string."""
        return _strftime_expand(self, format, self.timetuple())

    def utcoffset(self):
        """Offset from UTC (east positive) given by tzinfo, or None."""
        if self._tzinfo is None:
            return None
        off = self._tzinfo.utcoffset(self)
        _check_utc_offset("utcoffset", off)
        return off

    def tzname(self):
        """Zone name reported by tzinfo, or None."""
        if self._tzinfo is None:
            return None
        name = self._tzinfo.tzname(self)
        _check_tzname(name)
        return name

    def dst(self):
        """Daylight saving adjustment reported by tzinfo, or None."""
        if self._tzinfo is None:
            return None
        off = self._tzinfo.dst(self)
        _check_utc_offset("dst", off)
        return off

    # comparisons

    def __eq__(self, other):
        if isinstance(other, datetime):
            return self._dt_cmp(other, allow_mixed=True) == 0
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, datetime):
            return self._dt_cmp(other) <= 0
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, datetime):
            return self._dt_cmp(other) < 0
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, datetime):
            return self._dt_cmp(other) >= 0
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, datetime):
            return self._dt_cmp(other) > 0
        return NotImplemented

    def _dt_cmp(self, other, allow_mixed=False):
        mytz, ottz = self._tzinfo, other._tzinfo
        myoff = otoff = None
        if mytz is ottz:
            same = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            # Wall times that are ambiguous or in a gap never compare equal
            # to times in other zones.
            if allow_mixed:
                if myoff != self.replace(fold=not self._fold).utcoffset():
                    return 2
                if otoff != other.replace(fold=not other._fold).utcoffset():
                    return 2
            same = myoff == otoff
        if same:
            return _cmp((self._year, self._month, self._day, self._hour,
                         self._minute, self._second, self._microsecond),
                        (other._year, other._month, other._day, other._hour,
                         other._minute, other._second, other._microsecond))
        if myoff is None or otoff is None:
            if allow_mixed:
                return 2
            raise TypeError("can't compare offset-naive and offset-aware "
                            "datetimes")
        diff = self - other
        if diff.days < 0:
            return -1
        return 1 if diff else 0

    def __add__(self, other):
        "datetime + timedelta."
        if not isinstance(other, timedelta):
            return NotImplemented
        delta = timedelta(self.toordinal(), hours=self._hour,
                          minutes=self._minute, seconds=self._second,
                          microseconds=self._microsecond) + other
        hour, rem = divmod(delta.seconds, 3600)
        minute, second = divmod(rem, 60)
        if 0 < delta.days <= _MAXORDINAL:
            y, m, d = _ord2ymd(delta.days)
            return type(self).combine(
                date(y, m, d),
                time(hour, minute, second, delta.microseconds,
                     tzinfo=self._tzinfo))
        raise OverflowError("date value out of range")

    __radd__ = __add__

    def __sub__(self, other):
        "datetime - datetime or datetime - timedelta."
        if not isinstance(other, datetime):
            if isinstance(other, timedelta):
                return self + -other
            return NotImplemented
        days1 = self.toordinal()
        days2 = other.toordinal()
        secs1 = self._second + self._minute * 60 + self._hour * 3600
        secs2 = other._second + other._minute * 60 + other._hour * 3600
        base = timedelta(days1 - days2, secs1 - secs2,
                         self._microsecond - other._microsecond)
        if self._tzinfo is other._tzinfo:
            return base
        myoff = self.utcoffset()
        otoff = other.utcoffset()
        if myoff == otoff:
            return base
        if myoff is None or otoff is None:
            raise TypeError("can't subtract offset-naive and offset-aware "
                            "datetimes")
        return base + otoff - myoff

    def __hash__(self):
        if self._hashcode == -1:
            t = self.replace(fold=0) if self._fold else self
            off = t.utcoffset()
            if off is None:
                h = hash(t._getstate()[0])
            else:
                days = _ymd2ord(self._year, self._month, self._day)
                seconds = self._hour * 3600 + self._minute * 60 + \
                    self._second
                h = hash(timedelta(days, seconds, self._microsecond) - off)
            self._hashcode = h
        return self._hashcode

    # pickle support

    def _getstate(self, protocol=3):
        yhi, ylo = divmod(self._year, 256)
        us2, us3 = divmod(self._microsecond, 256)
        us1, us2 = divmod(us2, 256)
        m = self._month
        if self._fold and protocol > 3:
            m += 128
        basestate = bytes([yhi, ylo, m, self._day, self._hour, self._minute,
                           self._second, us1, us2, us3])
        if self._tzinfo is None:
            return (basestate,)
        return (basestate, self._tzinfo)

    def __reduce_ex__(self, protocol):
        return (type(self), self._getstate(protocol))

    def __reduce__(self):
        return self.__reduce_ex__(2)


# ---------------------------------------------------------------------------
# timezone

_TZ_NO_NAME = object()


def _timezone_new_args(offset, name=_TZ_NO_NAME):
    if not isinstance(offset, timedelta):
        raise TypeError("timezone() argument 1 must be datetime.timedelta, "
                        "not %s" % type(offset).__name__)
    if name is _TZ_NO_NAME:
        name = None
    elif not isinstance(name, str):
        raise TypeError("timezone() argument 2 must be str, not %s"
                        % type(name).__name__)
    if not timezone._minoffset <= offset <= timezone._maxoffset:
        raise ValueError("offset must be a timedelta strictly between "
                         "-timedelta(hours=24) and timedelta(hours=24), "
                         "not %r." % (offset,))
    return offset, name


class timezone(tzinfo):
    """tzinfo with a constant UTC offset."""
    __slots__ = ('_offset', '_name')

    def __init_subclass__(cls, **kwargs):
        raise TypeError("type 'datetime.timezone' is not an acceptable "
                        "base type")

    def __new__(cls, offset, name=_TZ_NO_NAME):
        offset, name = _timezone_new_args(offset, name)
        if name is None and not offset and cls is timezone:
            try:
                return timezone.utc
            except AttributeError:
                pass
        self = tzinfo.__new__(cls)
        self._offset = offset
        self._name = name
        return self

    def __getinitargs__(self):
        """Constructor arguments used for pickling."""
        if self._name is None:
            return (self._offset,)
        return (self._offset, self._name)

    def __reduce__(self):
        return (type(self), self.__getinitargs__())

    def __eq__(self, other):
        if isinstance(other, timezone):
            return self._offset == other._offset
        return NotImplemented

    def __hash__(self):
        return hash(self._offset)

    def __repr__(self):
        if self is timezone.utc:
            return 'datetime.timezone.utc'
        if self._name is None:
            return "%s(%r)" % (_qualified(self), self._offset)
        return "%s(%r, %r)" % (_qualified(self), self._offset, self._name)

    def __str__(self):
        return self.tzname(None)

    def utcoffset(self, dt):
        if isinstance(dt, datetime) or dt is None:
            return self._offset
        raise TypeError("utcoffset() argument must be a datetime instance"
                        " or None")

    def tzname(self, dt):
        if isinstance(dt, datetime) or dt is None:
            if self._name is None:
                return self._name_from_offset(self._offset)
            return self._name
        raise TypeError("tzname() argument must be a datetime instance"
                        " or None")

    def dst(self, dt):
        if isinstance(dt, datetime) or dt is None:
            return None
        raise TypeError("dst() argument must be a datetime instance"
                        " or None")

    def fromutc(self, dt):
        if isinstance(dt, datetime):
            if dt.tzinfo is not self:
                raise ValueError("fromutc: dt.tzinfo is not self")
            return dt + self._offset
        raise TypeError("fromutc() argument must be a datetime instance"
                        " or None")

    _maxoffset = timedelta(hours=24, microseconds=-1)
    _minoffset = -_maxoffset

    @staticmethod
    def _name_from_offset(delta):
        if not delta:
            return 'UTC'
        return 'UTC' + _format_offset(delta)


def _set_class_attr(cls, name, value):
    # The native module replaces this with a function able to update its
    # immutable types.
    setattr(cls, name, value)


def _init_constants():
    _set_class_attr(timedelta, 'min', timedelta(-999999999))
    _set_class_attr(timedelta, 'max', timedelta(days=999999999, hours=23,
                                                minutes=59, seconds=59,
                                                microseconds=999999))
    _set_class_attr(timedelta, 'resolution', timedelta(microseconds=1))
    _set_class_attr(date, 'min', date(1, 1, 1))
    _set_class_attr(date, 'max', date(9999, 12, 31))
    _set_class_attr(date, 'resolution', timedelta(days=1))
    _set_class_attr(time, 'min', time(0, 0, 0))
    _set_class_attr(time, 'max', time(23, 59, 59, 999999))
    _set_class_attr(time, 'resolution', timedelta(microseconds=1))
    _set_class_attr(datetime, 'min', datetime(1, 1, 1))
    _set_class_attr(datetime, 'max', datetime(9999, 12, 31, 23, 59, 59,
                                              999999))
    _set_class_attr(datetime, 'resolution', timedelta(microseconds=1))
    _set_class_attr(timezone, '_maxoffset',
                    timedelta(hours=24, microseconds=-1))
    _set_class_attr(timezone, '_minoffset',
                    -timedelta(hours=24, microseconds=-1))
    utc = timezone(timedelta(0))
    _set_class_attr(timezone, 'utc', utc)
    _set_class_attr(timezone, 'min', timezone(-timedelta(hours=23,
                                                         minutes=59)))
    _set_class_attr(timezone, 'max', timezone(timedelta(hours=23,
                                                        minutes=59)))
    return utc


UTC = _init_constants()
_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
