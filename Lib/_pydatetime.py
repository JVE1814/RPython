"""Pure-Python implementation of the datetime module.

This file is also the method source of the native ``_datetime`` module:
``_datetime`` executes this code and then rebinds the class names to its
native types (which have the C struct layout of Include/datetime.h), so
every method below must reach the instance fields only through the
``_dt_year``/``_dt_month``/... attributes and build new objects only through the
public constructors (or the ``_new_*`` helpers).
"""

__all__ = ("date", "datetime", "time", "timedelta", "timezone", "tzinfo",
           "MINYEAR", "MAXYEAR", "UTC")

import time as _time
import math as _math
import sys
from bisect import bisect_right as _bisect_right
from operator import index as _index

MINYEAR = 1
MAXYEAR = 9999

# Days are numbered ("ordinals") in the proleptic Gregorian calendar with
# 0001-01-01 as day 1.
_LAST_ORDINAL = 3652059          # 9999-12-31
_EPOCH_ORDINAL = 719163          # 1970-01-01

# Month lengths of a common year, and for each month the number of days of
# the year before it (common year / leap year).
_MONTH_DAYS = (31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
_MONTH_START = tuple(sum(_MONTH_DAYS[:i]) for i in range(12))
_MONTH_START_LEAP = _MONTH_START[:2] + tuple(n + 1 for n in _MONTH_START[2:])

# English abbreviations used by ctime(): months from January, weekdays from
# Monday.
_MONTH_ABBR = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
               "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
_DAY_ABBR = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


def _module_label(obj):
    mod = type(obj).__module__
    return 'datetime' if mod in ('_pydatetime', '_datetime') else mod


def _qualified(obj):
    tp = type(obj)
    if tp.__module__ in ('datetime', '_pydatetime', '_datetime'):
        return 'datetime.' + tp.__qualname__
    return tp.__qualname__


# ---------------------------------------------------------------------------
# Calendar arithmetic

def _leap(year):
    if year % 4:
        return False
    return year % 100 != 0 or year % 400 == 0


def _month_length(year, month):
    if month == 2:
        return 29 if _leap(year) else 28
    return _MONTH_DAYS[month - 1]


def _days_until(year):
    """Number of days in the years before *year* (from year 1 on)."""
    done = year - 1
    return 365 * done + done // 4 - done // 100 + done // 400


def _ordinal(year, month, day):
    starts = _MONTH_START_LEAP if _leap(year) else _MONTH_START
    return _days_until(year) + starts[month - 1] + day


def _from_ordinal(n):
    """(year, month, day) of day number *n*."""
    # 400 Gregorian years have 146097 days: guess the year from that ratio,
    # then step to the year whose span contains n (at most one step).
    year = n * 400 // 146097 + 1
    while _days_until(year) >= n:
        year -= 1
    while _days_until(year + 1) < n:
        year += 1
    day_of_year = n - _days_until(year)          # 1-based
    starts = _MONTH_START_LEAP if _leap(year) else _MONTH_START
    month = _bisect_right(starts, day_of_year - 1)
    return year, month, day_of_year - starts[month - 1]


def _iso_week1_monday(year):
    """Ordinal of the Monday that starts ISO week 1 of *year*: the week
    that contains the year's first Thursday."""
    jan1 = _ordinal(year, 1, 1)
    jan1_weekday = (jan1 + 6) % 7      # Monday == 0
    monday = jan1 - jan1_weekday
    if jan1_weekday > 3:
        monday += 7
    return monday


def _struct_time(y, m, d, hh, mm, ss, dst_flag):
    n = _ordinal(y, m, d)
    return _time.struct_time((y, m, d, hh, mm, ss, (n + 6) % 7,
                              n - _days_until(y), dst_flag))


# ---------------------------------------------------------------------------
# Argument validation

# Default of the year/month/day constructor parameters: tells "not passed"
# apart from an explicit None (which is rejected like any non-integer).
_ABSENT = object()


def _required_ymd(func, year, month, day):
    for pos, (name, value) in enumerate(
            (("year", year), ("month", month), ("day", day)), 1):
        if value is _ABSENT:
            raise TypeError(f"{func}() missing required argument "
                            f"'{name}' (pos {pos})")
    return year, month, day


def _int_arg(value):
    # operator.index gives the "'X' object cannot be interpreted as an
    # integer" error of an int parameter (floats included)
    value = _index(value)
    if not isinstance(value, int):
        raise TypeError('__index__ returned non-int (type %s)' %
                        type(value).__name__)
    # the fields are C ints: out of that range it is an OverflowError, not
    # the ValueError of the range checks (dateutil/pandas rely on the type)
    if not -0x80000000 <= value <= 0x7fffffff:
        raise OverflowError("Python int too large to convert to C int")
    return value


def _range(name, value, lo, hi):
    if not lo <= value <= hi:
        raise ValueError(f"{name} must be in {lo}..{hi}, not {value}")


def _valid_ymd(year, month, day):
    """Integer (year, month, day), each within its range."""
    year, month, day = _int_arg(year), _int_arg(month), _int_arg(day)
    _range('year', year, MINYEAR, MAXYEAR)
    _range('month', month, 1, 12)
    last = _month_length(year, month)
    if day < 1 or day > last:
        raise ValueError(f"day {day} must be in range 1..{last} for month "
                         f"{month} in year {year}")
    return year, month, day


# (name, upper bound) of the clock fields, in constructor order
_CLOCK_FIELDS = (('hour', 23), ('minute', 59), ('second', 59),
                 ('microsecond', 999999))


def _valid_clock(hour, minute, second, microsecond, fold):
    """Integer (hour, minute, second, microsecond, fold), each in range."""
    values = []
    for (name, top), value in zip(_CLOCK_FIELDS, (hour, minute, second, microsecond)):
        value = _int_arg(value)
        _range(name, value, 0, top)
        values.append(value)
    if fold != 0 and fold != 1:
        raise ValueError('fold must be either 0 or 1', fold)
    values.append(fold)
    return tuple(values)


def _require_tzinfo(tz):
    if tz is None or isinstance(tz, tzinfo):
        return
    raise TypeError("tzinfo argument must be None or of a tzinfo "
                    "subclass, not type '%s'" % type(tz).__name__)


# |utcoffset()| and |dst()| must stay below one day
_DAY_US = 86400 * 1000000


def _vetted_offset(method, offset):
    """Return what tzinfo.<method>() returned after checking it is None or
    a timedelta of less than a day in magnitude."""
    if offset is None:
        return None
    if not isinstance(offset, timedelta):
        raise TypeError("tzinfo.%s() must return None or timedelta, "
                        "not '%s'" % (method, type(offset).__name__))
    if abs(_td_us(offset)) >= _DAY_US:
        raise ValueError("offset must be a timedelta strictly between "
                         "-timedelta(hours=24) and timedelta(hours=24), "
                         "not %r" % (offset,))
    return offset


def _vetted_tzname(name):
    if name is None or isinstance(name, str):
        return name
    raise TypeError("tzinfo.tzname() must return None or string, "
                    "not '%s'" % type(name).__name__)


def _order(a, b):
    """-1, 0 or 1 as a sorts before, with or after b."""
    if a == b:
        return 0
    return -1 if a < b else 1


def _divide_and_round(a, b):
    """a / b rounded to the nearest integer, halves to even (any signs)."""
    q, r = divmod(a, b)
    # r has the sign of b: compare |2r| with |b|
    twice, limit = (2 * r, b) if b > 0 else (-2 * r, -b)
    if twice > limit or (twice == limit and q % 2):
        q += 1
    return q


# ---------------------------------------------------------------------------
# Formatting helpers

# timespec name -> (number of clock fields shown, digits of the fraction)
_TIMESPECS = {
    'hours': (1, 0),
    'minutes': (2, 0),
    'seconds': (3, 0),
    'milliseconds': (3, 3),
    'microseconds': (3, 6),
}


def _clock_text(hh, mm, ss, us, timespec='auto'):
    """HH[:MM[:SS[.fff[fff]]]] as selected by *timespec*."""
    if timespec == 'auto':
        timespec = 'microseconds' if us else 'seconds'
    try:
        shown, digits = _TIMESPECS[timespec]
    except (KeyError, TypeError):
        raise ValueError('Unknown timespec value') from None
    text = ':'.join('%02d' % v for v in (hh, mm, ss)[:shown])
    if digits == 3:
        text += '.%03d' % (us // 1000)
    elif digits == 6:
        text += '.%06d' % us
    return text


def _offset_text(off, sep=':'):
    """'+HH<sep>MM[<sep>SS[.ffffff]]' for a UTC offset, '' for None."""
    if off is None:
        return ''
    us = _td_us(off)
    sign = '-' if us < 0 else '+'
    seconds, fraction = divmod(abs(us), 1000000)
    minutes, seconds = divmod(seconds, 60)
    hours, minutes = divmod(minutes, 60)
    text = '%s%02d%s%02d' % (sign, hours, sep, minutes)
    if seconds or fraction:
        text += '%s%02d' % (sep, seconds)
        if fraction:
            text += '.%06d' % fraction
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
            pieces.append(_offset_text(_utcoffset_of(obj), ''))
        elif code == ':' and i < n and fmt[i] == 'z':
            i += 1
            pieces.append(_offset_text(_utcoffset_of(obj), ':'))
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
    # a year has 52 or 53 ISO weeks: the distance between its week-1 Monday
    # and the next year's
    week_count = (_iso_week1_monday(year + 1) - _iso_week1_monday(year)) // 7
    if not 0 < week <= week_count:
        raise ValueError(f"Invalid week: {week}")
    if not 0 < day < 8:
        raise ValueError(f"Invalid weekday: {day} (range is [1, 7])")
    ordinal = _iso_week1_monday(year) + (week - 1) * 7 + day - 1
    return _from_ordinal(ordinal)


# ---------------------------------------------------------------------------
# timedelta

_US_PER_SECOND = 1000000
_US_PER_DAY = 86400 * _US_PER_SECOND


def _td_us(td):
    """Total microseconds of a timedelta, read from its own fields.

    Arithmetic with a timedelta operand works on the fields, never through
    the operand's operators: a subclass (pandas' Timedelta counts nanoseconds
    in a narrower range) must not take over datetime + delta."""
    return ((td._dt_days * 86400 + td._dt_seconds) * _US_PER_SECOND
            + td._dt_microseconds)


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
    __slots__ = ('_dt_days', '_dt_seconds', '_dt_microseconds', '_dt_hashcode')

    def __new__(cls, days=0, seconds=0, microseconds=0,
                milliseconds=0, minutes=0, hours=0, weeks=0):
        d, s, us = _timedelta_fields(days, seconds, microseconds,
                                     milliseconds, minutes, hours, weeks)
        self = object.__new__(cls)
        self._dt_days, self._dt_seconds, self._dt_microseconds = d, s, us
        self._dt_hashcode = -1
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
        return (self._dt_days * 86400 + self._dt_seconds) * _US_PER_SECOND + \
            self._dt_microseconds

    def __repr__(self):
        parts = []
        if self._dt_days:
            parts.append('days=%d' % self._dt_days)
        if self._dt_seconds:
            parts.append('seconds=%d' % self._dt_seconds)
        if self._dt_microseconds:
            parts.append('microseconds=%d' % self._dt_microseconds)
        if not parts:
            parts.append('0')
        return '%s(%s)' % (_qualified(self), ', '.join(parts))

    def __str__(self):
        mm, ss = divmod(self._dt_seconds, 60)
        hh, mm = divmod(mm, 60)
        text = '%d:%02d:%02d' % (hh, mm, ss)
        if self._dt_days:
            unit = 'day' if abs(self._dt_days) == 1 else 'days'
            text = '%d %s, %s' % (self._dt_days, unit, text)
        if self._dt_microseconds:
            text += '.%06d' % self._dt_microseconds
        return text

    def total_seconds(self):
        """The duration in seconds, as a float."""
        return self._to_us() / 10**6

    @property
    def days(self):
        """Whole days (may be negative)."""
        return self._dt_days

    @property
    def seconds(self):
        """Seconds within the day, 0..86399."""
        return self._dt_seconds

    @property
    def microseconds(self):
        """Microseconds within the second, 0..999999."""
        return self._dt_microseconds

    def __add__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(self._to_us() + _td_us(other))
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(self._to_us() - _td_us(other))
        return NotImplemented

    def __rsub__(self, other):
        if isinstance(other, timedelta):
            return timedelta._from_us(_td_us(other) - self._to_us())
        return NotImplemented

    def __neg__(self):
        return timedelta._from_us(-self._to_us())

    def __pos__(self):
        return timedelta(self._dt_days, self._dt_seconds, self._dt_microseconds)

    def __abs__(self):
        return -self if self._dt_days < 0 else self.__pos__()

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
        return (self._dt_days, self._dt_seconds, self._dt_microseconds)

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
        if self._dt_hashcode == -1:
            self._dt_hashcode = hash(self._key())
        return self._dt_hashcode

    def __bool__(self):
        return bool(self._dt_days or self._dt_seconds or self._dt_microseconds)

    def __reduce__(self):
        return (type(self), self._key())


# ---------------------------------------------------------------------------
# date

def _date_state_or_none(year, month, day):
    """Decode the pickle state form date(bytes) or return None."""
    if month is not _ABSENT or day is not _ABSENT:
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


def _date_new_args(year=_ABSENT, month=_ABSENT, day=_ABSENT):
    state = _date_state_or_none(year, month, day)
    if state is not None:
        return state[0] * 256 + state[1], state[2], state[3]
    return _valid_ymd(*_required_ymd("date", year, month, day))


class date:
    """Calendar date: date(year, month, day)."""
    __slots__ = ('_dt_year', '_dt_month', '_dt_day', '_dt_hashcode')

    def __new__(cls, year=_ABSENT, month=_ABSENT, day=_ABSENT):
        y, m, d = _date_new_args(year, month, day)
        self = object.__new__(cls)
        self._dt_year, self._dt_month, self._dt_day = y, m, d
        self._dt_hashcode = -1
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
        if not 1 <= n <= _LAST_ORDINAL:
            raise ValueError("ordinal must be in 1..%d, not %d"
                             % (_LAST_ORDINAL, n))
        return cls(*_from_ordinal(n))

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
        return "%s(%d, %d, %d)" % (_qualified(self), self._dt_year,
                                   self._dt_month, self._dt_day)

    def ctime(self):
        "Text in the style of time.ctime()."
        weekday = (self.toordinal() + 6) % 7
        return "%s %s %2d 00:00:00 %04d" % (
            _DAY_ABBR[weekday], _MONTH_ABBR[self._dt_month - 1], self._dt_day,
            self._dt_year)

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
        return "%04d-%02d-%02d" % (self._dt_year, self._dt_month, self._dt_day)

    __str__ = isoformat

    # attributes

    @property
    def year(self):
        """Year, 1..9999."""
        return self._dt_year

    @property
    def month(self):
        """Month, 1..12."""
        return self._dt_month

    @property
    def day(self):
        """Day of the month, 1..31."""
        return self._dt_day

    # standard conversions

    def timetuple(self):
        "The value as a time.struct_time."
        return _struct_time(self._dt_year, self._dt_month, self._dt_day,
                                  0, 0, 0, -1)

    def toordinal(self):
        """Day number of the date in the proleptic Gregorian calendar
        (0001-01-01 is day 1)."""
        return _ordinal(self._dt_year, self._dt_month, self._dt_day)

    def replace(self, year=None, month=None, day=None):
        """Copy with the given fields changed."""
        if year is None:
            year = self._dt_year
        if month is None:
            month = self._dt_month
        if day is None:
            day = self._dt_day
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
        return _order((self._dt_year, self._dt_month, self._dt_day),
                    (other._dt_year, other._dt_month, other._dt_day))

    def __hash__(self):
        if self._dt_hashcode == -1:
            # hashing bytes keeps the value salted per process, like str
            self._dt_hashcode = hash(self._pickle_state()[0])
        return self._dt_hashcode

    # arithmetic

    def __add__(self, other):
        if isinstance(other, timedelta):
            o = self.toordinal() + other._dt_days
            if 0 < o <= _LAST_ORDINAL:
                return type(self).fromordinal(o)
            raise OverflowError("result out of range")
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(-other._dt_days)
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
        year = self._dt_year
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

    def _pickle_state(self):
        # four bytes: year (big-endian), month, day
        packed = self._dt_year.to_bytes(2, 'big') + bytes((self._dt_month, self._dt_day))
        return (packed,)

    def __reduce__(self):
        return (type(self), self._pickle_state())


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
        # subclasses may name their constructor arguments with
        # __getinitargs__; the instance state travels separately
        initargs = getattr(self, "__getinitargs__", None)
        return (type(self), tuple(initargs()) if initargs else (),
                self.__getstate__())


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
    h, mi, s, us, fold = _valid_clock(hour, minute, second,
                                            microsecond, fold)
    _require_tzinfo(tzinfo)
    return h, mi, s, us, tzinfo, fold


class time:
    """Time of day: time(hour=0, minute=0, second=0, microsecond=0,
    tzinfo=None, *, fold=0)."""
    __slots__ = ('_dt_hour', '_dt_minute', '_dt_second', '_dt_microsecond', '_dt_tzinfo',
                 '_dt_hashcode', '_dt_fold')

    def __new__(cls, hour=0, minute=0, second=0, microsecond=0, tzinfo=None,
                *, fold=0):
        h, mi, s, us, tz, f = _time_new_args(hour, minute, second,
                                             microsecond, tzinfo, fold=fold)
        self = object.__new__(cls)
        self._dt_hour, self._dt_minute, self._dt_second = h, mi, s
        self._dt_microsecond, self._dt_tzinfo, self._dt_fold = us, tz, f
        self._dt_hashcode = -1
        return self

    @classmethod
    def strptime(cls, date_string, format):
        """Parse *date_string* with a time.strptime() format."""
        import _strptime
        return _strptime._strptime_datetime_time(cls, date_string, format)

    @property
    def hour(self):
        """Hour, 0..23."""
        return self._dt_hour

    @property
    def minute(self):
        """Minute, 0..59."""
        return self._dt_minute

    @property
    def second(self):
        """Second, 0..59."""
        return self._dt_second

    @property
    def microsecond(self):
        """Microsecond, 0..999999."""
        return self._dt_microsecond

    @property
    def tzinfo(self):
        """The tzinfo object, or None."""
        return self._dt_tzinfo

    @property
    def fold(self):
        return self._dt_fold

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
        mytz, ottz = self._dt_tzinfo, other._dt_tzinfo
        myoff = otoff = None
        if mytz is ottz:
            same = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            same = myoff == otoff
        mine = (self._dt_hour, self._dt_minute, self._dt_second, self._dt_microsecond)
        theirs = (other._dt_hour, other._dt_minute, other._dt_second,
                  other._dt_microsecond)
        if same:
            return _order(mine, theirs)
        if myoff is None or otoff is None:
            if allow_mixed:
                return 2           # naive vs aware: never equal
            raise TypeError("can't compare offset-naive and offset-aware "
                            "times")
        mysecs = (mine[0] * 60 + mine[1]) * 60 + mine[2] - \
            myoff // timedelta(seconds=1)
        otsecs = (theirs[0] * 60 + theirs[1]) * 60 + theirs[2] - \
            otoff // timedelta(seconds=1)
        return _order((mysecs, mine[3]), (otsecs, theirs[3]))

    def _utc_key(self):
        # (seconds of the day minus the UTC offset, microsecond), with the
        # offset of the fold=0 reading: the quantity comparisons use
        plain = self.replace(fold=0) if self._dt_fold else self
        off = plain.utcoffset()
        secs = (self._dt_hour * 60 + self._dt_minute) * 60 + self._dt_second
        if off is not None:
            secs -= _td_us(off) // 1000000
        return secs, self._dt_microsecond

    def __hash__(self):
        if self._dt_hashcode == -1:
            secs, us = self._utc_key()
            # bytes hash: salted per process, like str
            self._dt_hashcode = hash((secs * 1000000 + us).to_bytes(8, 'little', signed=True))
        return self._dt_hashcode

    # conversion to string

    def __repr__(self):
        if self._dt_microsecond:
            s = ", %d, %d" % (self._dt_second, self._dt_microsecond)
        elif self._dt_second:
            s = ", %d" % self._dt_second
        else:
            s = ""
        s = "%s(%d, %d%s)" % (_qualified(self), self._dt_hour, self._dt_minute, s)
        if self._dt_tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self._dt_tzinfo + ")"
        if self._dt_fold:
            s = s[:-1] + ", fold=1)"
        return s

    def isoformat(self, timespec='auto'):
        """ISO 8601 text: HH:MM:SS[.ffffff][+HH:MM].

        *timespec* ('auto', 'hours', 'minutes', 'seconds', 'milliseconds'
        or 'microseconds') selects the precision; 'auto' drops a zero
        fraction.
        """
        s = _clock_text(self._dt_hour, self._dt_minute, self._dt_second,
                         self._dt_microsecond, timespec)
        return s + _offset_text(self.utcoffset())

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
        tt = (1900, 1, 1, self._dt_hour, self._dt_minute, self._dt_second, 0, 1, -1)
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
        if self._dt_tzinfo is None:
            return None
        return _vetted_offset("utcoffset", self._dt_tzinfo.utcoffset(None))

    def tzname(self):
        """Zone name reported by tzinfo (free-form text), or None."""
        if self._dt_tzinfo is None:
            return None
        return _vetted_tzname(self._dt_tzinfo.tzname(None))

    def dst(self):
        """Daylight saving adjustment reported by tzinfo, or None."""
        if self._dt_tzinfo is None:
            return None
        return _vetted_offset("dst", self._dt_tzinfo.dst(None))

    def replace(self, hour=None, minute=None, second=None, microsecond=None,
                tzinfo=True, *, fold=None):
        """Copy with the given fields changed."""
        if hour is None:
            hour = self._dt_hour
        if minute is None:
            minute = self._dt_minute
        if second is None:
            second = self._dt_second
        if microsecond is None:
            microsecond = self._dt_microsecond
        if tzinfo is True:
            tzinfo = self._dt_tzinfo
        if fold is None:
            fold = self._dt_fold
        return type(self)(hour, minute, second, microsecond, tzinfo,
                          fold=fold)

    __replace__ = replace

    # pickle support

    def _pickle_state(self, protocol=3):
        # six bytes: hour (+128 when fold is set and the protocol can carry
        # it), minute, second, microsecond (3 bytes, big-endian)
        hour = self._dt_hour + (128 if self._dt_fold and protocol > 3 else 0)
        packed = (bytes((hour, self._dt_minute, self._dt_second))
                  + self._dt_microsecond.to_bytes(3, 'big'))
        if self._dt_tzinfo is None:
            return (packed,)
        return (packed, self._dt_tzinfo)

    def __reduce_ex__(self, protocol):
        return (type(self), self._pickle_state(protocol))

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
    if month is not _ABSENT and month is not None and not isinstance(month, tzinfo):
        raise TypeError("bad tzinfo state arg")
    return year


def _datetime_new_args(year=_ABSENT, month=_ABSENT, day=_ABSENT, hour=0, minute=0,
                       second=0, microsecond=0, tzinfo=None, *, fold=0):
    state = _datetime_state_or_none(year, month, tzinfo)
    if state is not None:
        m = state[2]
        f = 0
        if m > 127:
            f, m = 1, m - 128
        us = (state[7] << 16) | (state[8] << 8) | state[9]
        return (state[0] * 256 + state[1], m, state[3], state[4], state[5],
                state[6], us, None if month is _ABSENT else month, f)
    y, m, d = _valid_ymd(*_required_ymd("datetime", year, month, day))
    h, mi, s, us, fold = _valid_clock(hour, minute, second,
                                            microsecond, fold)
    _require_tzinfo(tzinfo)
    return y, m, d, h, mi, s, us, tzinfo, fold


def _local_offset(u):
    """Seconds east of UTC of the local time zone at POSIX time u."""
    tm = _time.localtime(u)
    local = (_ordinal(*tm[:3]) - _EPOCH_ORDINAL) * 86400 + \
        (tm[3] * 60 + tm[4]) * 60 + tm[5]
    return local - u


def _is_date(obj):
    return isinstance(obj, date)


def _is_time(obj):
    return isinstance(obj, time)


class datetime(date):
    """Date and time of day: datetime(year, month, day, hour=0, minute=0,
    second=0, microsecond=0, tzinfo=None, *, fold=0)."""
    __slots__ = ('_dt_hour', '_dt_minute', '_dt_second', '_dt_microsecond', '_dt_tzinfo',
                 '_dt_fold')

    def __new__(cls, year=_ABSENT, month=_ABSENT, day=_ABSENT, hour=0, minute=0, second=0,
                microsecond=0, tzinfo=None, *, fold=0):
        (y, m, d, h, mi, s, us, tz, f) = _datetime_new_args(
            year, month, day, hour, minute, second, microsecond, tzinfo,
            fold=fold)
        self = object.__new__(cls)
        self._dt_year, self._dt_month, self._dt_day = y, m, d
        self._dt_hour, self._dt_minute, self._dt_second = h, mi, s
        self._dt_microsecond, self._dt_tzinfo, self._dt_fold = us, tz, f
        self._dt_hashcode = -1
        return self

    @property
    def hour(self):
        """Hour, 0..23."""
        return self._dt_hour

    @property
    def minute(self):
        """Minute, 0..59."""
        return self._dt_minute

    @property
    def second(self):
        """Second, 0..59."""
        return self._dt_second

    @property
    def microsecond(self):
        """Microsecond, 0..999999."""
        return self._dt_microsecond

    @property
    def tzinfo(self):
        """The tzinfo object, or None."""
        return self._dt_tzinfo

    @property
    def fold(self):
        return self._dt_fold

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
        _require_tzinfo(tz)
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
        _valid_ymd(y, m, d)
        if next_day and (y, m, d) == (MAXYEAR, 12, 31):
            # rolling over past the last representable day
            _valid_ymd(MAXYEAR + 1, 1, 1)
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
        return _struct_time(self._dt_year, self._dt_month, self._dt_day,
                                  self._dt_hour, self._dt_minute, self._dt_second,
                                  flag)

    def _local_timestamp(self):
        """POSIX time of this naive datetime read as local time."""
        epoch_days = _ordinal(self._dt_year, self._dt_month, self._dt_day) - _EPOCH_ORDINAL
        wall = epoch_days * 86400 + (self._dt_hour * 60 + self._dt_minute) * 60 + \
            self._dt_second
        candidates = set()
        for probe in (wall - 86400, wall, wall + 86400):
            u = wall - _local_offset(probe)
            if u + _local_offset(u) == wall:
                candidates.add(u)
        if candidates:
            ordered = sorted(candidates)
            return ordered[-1] if self._dt_fold else ordered[0]
        # In a gap: fold=0 uses the offset from before the transition.
        before = _local_offset(wall - 86400)
        after = _local_offset(wall + 86400)
        return wall - (after if self._dt_fold else before)

    def timestamp(self):
        "Seconds since the POSIX epoch, as a float."
        if self._dt_tzinfo is None:
            s = self._local_timestamp()
            return s + self._dt_microsecond / 1e6
        return (self - _EPOCH).total_seconds()

    def utctimetuple(self):
        "The UTC value as a time.struct_time."
        offset = self.utcoffset()
        if offset:
            self -= offset
        return _struct_time(self._dt_year, self._dt_month, self._dt_day,
                                  self._dt_hour, self._dt_minute, self._dt_second, 0)

    def date(self):
        "The date portion."
        return date(self._dt_year, self._dt_month, self._dt_day)

    def time(self):
        "The time of day, without tzinfo."
        return time(self._dt_hour, self._dt_minute, self._dt_second,
                    self._dt_microsecond, fold=self._dt_fold)

    def timetz(self):
        "The time of day, keeping tzinfo."
        return time(self._dt_hour, self._dt_minute, self._dt_second,
                    self._dt_microsecond, self._dt_tzinfo, fold=self._dt_fold)

    def replace(self, year=None, month=None, day=None, hour=None,
                minute=None, second=None, microsecond=None, tzinfo=True,
                *, fold=None):
        """Copy with the given fields changed."""
        if year is None:
            year = self._dt_year
        if month is None:
            month = self._dt_month
        if day is None:
            day = self._dt_day
        if hour is None:
            hour = self._dt_hour
        if minute is None:
            minute = self._dt_minute
        if second is None:
            second = self._dt_second
        if microsecond is None:
            microsecond = self._dt_microsecond
        if tzinfo is True:
            tzinfo = self._dt_tzinfo
        if fold is None:
            fold = self._dt_fold
        return type(self)(year, month, day, hour, minute, second,
                          microsecond, tzinfo, fold=fold)

    __replace__ = replace

    def _local_timezone(self):
        if self._dt_tzinfo is None:
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
        mytz = self._dt_tzinfo
        myoffset = None if mytz is None else mytz.utcoffset(self)
        if myoffset is None:
            # naive (or offset-less) values are read as local wall time
            naive = self.replace(tzinfo=None)
            wall_days = _ordinal(self._dt_year, self._dt_month, self._dt_day) - _EPOCH_ORDINAL
            secs = wall_days * 86400 + \
                (self._dt_hour * 60 + self._dt_minute) * 60 + self._dt_second
            myoffset = timedelta(seconds=secs - naive._local_timestamp())
        elif tz is mytz:
            return self
        utc = (self - myoffset).replace(tzinfo=tz)
        return tz.fromutc(utc)

    # string conversions

    def ctime(self):
        "Text in the style of time.ctime()."
        weekday = (self.toordinal() + 6) % 7
        return "%s %s %2d %02d:%02d:%02d %04d" % (
            _DAY_ABBR[weekday], _MONTH_ABBR[self._dt_month - 1], self._dt_day,
            self._dt_hour, self._dt_minute, self._dt_second, self._dt_year)

    def isoformat(self, sep='T', timespec='auto'):
        """ISO 8601 text: YYYY-MM-DD<sep>HH:MM:SS[.ffffff][+HH:MM].

        *sep* separates date and time; *timespec* selects the precision as
        for time.isoformat().
        """
        if not isinstance(sep, str) or len(sep) != 1:
            raise TypeError("isoformat() argument 1 must be a unicode "
                            "character, not %s" % type(sep).__name__)
        s = "%04d-%02d-%02d%s" % (self._dt_year, self._dt_month, self._dt_day, sep)
        s += _clock_text(self._dt_hour, self._dt_minute, self._dt_second,
                          self._dt_microsecond, timespec)
        return s + _offset_text(self.utcoffset())

    def __repr__(self):
        fields = [self._dt_year, self._dt_month, self._dt_day, self._dt_hour,
                  self._dt_minute, self._dt_second, self._dt_microsecond]
        while len(fields) > 5 and fields[-1] == 0:
            fields.pop()
        s = "%s(%s)" % (_qualified(self), ", ".join(map(str, fields)))
        if self._dt_tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self._dt_tzinfo + ")"
        if self._dt_fold:
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
        if self._dt_tzinfo is None:
            return None
        return _vetted_offset("utcoffset", self._dt_tzinfo.utcoffset(self))

    def tzname(self):
        """Zone name reported by tzinfo, or None."""
        if self._dt_tzinfo is None:
            return None
        return _vetted_tzname(self._dt_tzinfo.tzname(self))

    def dst(self):
        """Daylight saving adjustment reported by tzinfo, or None."""
        if self._dt_tzinfo is None:
            return None
        return _vetted_offset("dst", self._dt_tzinfo.dst(self))

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
        mytz, ottz = self._dt_tzinfo, other._dt_tzinfo
        myoff = otoff = None
        if mytz is ottz:
            same = True
        else:
            myoff = self.utcoffset()
            otoff = other.utcoffset()
            # Wall times that are ambiguous or in a gap never compare equal
            # to times in other zones.
            if allow_mixed:
                if myoff != self.replace(fold=not self._dt_fold).utcoffset():
                    return 2
                if otoff != other.replace(fold=not other._dt_fold).utcoffset():
                    return 2
            same = myoff == otoff
        if same:
            return _order((self._dt_year, self._dt_month, self._dt_day, self._dt_hour,
                         self._dt_minute, self._dt_second, self._dt_microsecond),
                        (other._dt_year, other._dt_month, other._dt_day, other._dt_hour,
                         other._dt_minute, other._dt_second, other._dt_microsecond))
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
        return self._plus_us(_td_us(other))

    __radd__ = __add__

    def _plus_us(self, us):
        total = (self.toordinal() * _US_PER_DAY
                 + ((self._dt_hour * 60 + self._dt_minute) * 60
                    + self._dt_second) * _US_PER_SECOND
                 + self._dt_microsecond + us)
        days, rem = divmod(total, _US_PER_DAY)
        secs, micro = divmod(rem, _US_PER_SECOND)
        hour, rem = divmod(secs, 3600)
        minute, second = divmod(rem, 60)
        if 0 < days <= _LAST_ORDINAL:
            y, m, d = _from_ordinal(days)
            return type(self).combine(
                date(y, m, d),
                time(hour, minute, second, micro, tzinfo=self._dt_tzinfo))
        raise OverflowError("date value out of range")

    def __rsub__(self, other):
        # the reflected call of datetime - datetime: a subclass on the left
        # whose own __sub__ declined (pandas' Timestamp out of its range)
        # still gets the plain datetime difference, as from a C slot
        if isinstance(other, datetime):
            return datetime.__sub__(other, self)
        return NotImplemented

    def __sub__(self, other):
        "datetime - datetime or datetime - timedelta."
        if not isinstance(other, datetime):
            if isinstance(other, timedelta):
                return self._plus_us(-_td_us(other))
            return NotImplemented
        days1 = self.toordinal()
        days2 = other.toordinal()
        secs1 = self._dt_second + self._dt_minute * 60 + self._dt_hour * 3600
        secs2 = other._dt_second + other._dt_minute * 60 + other._dt_hour * 3600
        base = timedelta(days1 - days2, secs1 - secs2,
                         self._dt_microsecond - other._dt_microsecond)
        if self._dt_tzinfo is other._dt_tzinfo:
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
        if self._dt_hashcode == -1:
            # microseconds since 0001-01-01 on the UTC time line (aware) or
            # on the wall clock (naive); fold=0 decides the offset
            plain = self.replace(fold=0) if self._dt_fold else self
            off = plain.utcoffset()
            day_secs = (self._dt_hour * 60 + self._dt_minute) * 60 + self._dt_second
            stamp = ((_ordinal(self._dt_year, self._dt_month, self._dt_day) * 86400
                      + day_secs) * 1000000 + self._dt_microsecond)
            if off is not None:
                stamp -= _td_us(off)
            # bytes hash: salted per process, like str
            self._dt_hashcode = hash(stamp.to_bytes(8, 'little', signed=True))
        return self._dt_hashcode

    # pickle support

    def _pickle_state(self, protocol=3):
        # ten bytes: year (2, big-endian), month (+128 when fold is set and
        # the protocol can carry it), day, hour, minute, second,
        # microsecond (3, big-endian)
        month = self._dt_month + (128 if self._dt_fold and protocol > 3 else 0)
        packed = (self._dt_year.to_bytes(2, 'big')
                  + bytes((month, self._dt_day, self._dt_hour,
                           self._dt_minute, self._dt_second))
                  + self._dt_microsecond.to_bytes(3, 'big'))
        if self._dt_tzinfo is None:
            return (packed,)
        return (packed, self._dt_tzinfo)

    def __reduce_ex__(self, protocol):
        return (type(self), self._pickle_state(protocol))

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
    if abs(_td_us(offset)) >= _DAY_US:
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
                return _utc_label(self._offset)
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


def _utc_label(offset):
    """Default timezone name: 'UTC' followed by the offset unless it is 0."""
    return 'UTC' + _offset_text(offset) if _td_us(offset) else 'UTC'


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
    utc = timezone(timedelta(0))
    _set_class_attr(timezone, 'utc', utc)
    _set_class_attr(timezone, 'min', timezone(-timedelta(hours=23,
                                                         minutes=59)))
    _set_class_attr(timezone, 'max', timezone(timedelta(hours=23,
                                                        minutes=59)))
    return utc


UTC = _init_constants()
_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
