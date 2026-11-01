"""Pure-Python implementation of zoneinfo.ZoneInfo."""

import bisect
import collections
import weakref
from datetime import datetime, timedelta, tzinfo

from . import _common, _tzpath

_EPOCH_ORDINAL = datetime(1970, 1, 1).toordinal()
_SECONDS_PER_DAY = 86400

# read the real fields even when a datetime subclass shadows the
# attribute names
_HOUR = datetime.hour.__get__
_MINUTE = datetime.minute.__get__
_SECOND = datetime.second.__get__


def _local_seconds(dt):
    """Seconds between 1970-01-01 00:00 and the wall time of *dt*."""
    return ((dt.toordinal() - _EPOCH_ORDINAL) * _SECONDS_PER_DAY
            + _HOUR(dt) * 3600 + _MINUTE(dt) * 60 + _SECOND(dt))


class _TTInfo:
    """One local time type: UTC offset, DST offset and abbreviation."""
    __slots__ = ("utcoff", "dstoff", "tzname", "offset_seconds")

    def __init__(self, offset_seconds, dst_seconds, tzname):
        self.offset_seconds = offset_seconds
        self.utcoff = timedelta(seconds=offset_seconds)
        self.dstoff = timedelta(seconds=dst_seconds)
        self.tzname = tzname

    def __eq__(self, other):
        if not isinstance(other, _TTInfo):
            return NotImplemented
        return (self.utcoff == other.utcoff and self.dstoff == other.dstoff
                and self.tzname == other.tzname)

    __hash__ = None

    def __repr__(self):
        return (f"{type(self).__name__}({self.utcoff}, {self.dstoff}, "
                f"{self.tzname})")


_NO_INFO = _TTInfo(0, 0, None)
_NO_INFO.utcoff = _NO_INFO.dstoff = None


# --------------------------------------------------------------------------
# calendar arithmetic for any integer year

def _is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


_DAYS_BEFORE_MONTH = (0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304,
                      334)
_DAYS_IN_MONTH = (0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)


def _epoch_days(year, month, day):
    """Days from 1970-01-01 to the given proleptic Gregorian date."""
    y = year - 1
    days = y * 365 + y // 4 - y // 100 + y // 400
    days += _DAYS_BEFORE_MONTH[month] + (month > 2 and _is_leap(year))
    return days + day - _EPOCH_ORDINAL


# --------------------------------------------------------------------------
# POSIX TZ strings (the TZif footer)

class _Rule:
    """Daylight saving time described by a POSIX TZ string."""

    def __init__(self, std, dst, start, end):
        self.std = std
        self.dst = dst
        self.start = start      # (date spec, seconds after local midnight)
        self.end = end

    @staticmethod
    def _date_days(spec, year):
        kind = spec[0]
        if kind == "J":         # 1..365, February 29 never counted
            n = spec[1]
            extra = 1 if _is_leap(year) and n >= 60 else 0
            return _epoch_days(year, 1, 1) + n - 1 + extra
        if kind == "N":         # 0..365, February 29 counted
            return _epoch_days(year, 1, 1) + spec[1]
        _, month, week, weekday = spec          # Mm.w.d, d=0 is Sunday
        first = _epoch_days(year, month, 1)
        first_weekday = (first + 4) % 7         # 1970-01-01 was a Thursday
        day = 1 + (weekday - first_weekday) % 7 + (week - 1) * 7
        length = _DAYS_IN_MONTH[month] + (month == 2 and _is_leap(year))
        while day > length:
            day -= 7
        return first + day - 1

    def _events(self, year):
        """(UTC seconds, enters DST?, offset before, offset after) for the
        transitions of the years around *year*, in order."""
        std = self.std.offset_seconds
        dst = self.dst.offset_seconds
        events = []
        for y in (year - 1, year, year + 1):
            spec, at = self.start
            start = self._date_days(spec, y) * _SECONDS_PER_DAY + at - std
            spec, at = self.end
            end = self._date_days(spec, y) * _SECONDS_PER_DAY + at - dst
            events.append((start, True, std, dst))
            events.append((end, False, dst, std))
        # when both happen at once the end comes first, so DST continues
        events.sort(key=lambda event: (event[0], event[1]))
        return events

    def _state(self, entered_dst):
        return self.dst if entered_dst else self.std

    def info_local(self, ts, year, fold):
        """Time type in effect at local wall time *ts*."""
        chosen = None
        for utc, enters, before, after in self._events(year):
            wall = utc + (max(before, after) if not fold
                          else min(before, after))
            if wall <= ts:
                chosen = enters
        if chosen is None:
            chosen = not self._events(year)[0][1]
        return self._state(chosen)

    def info_utc(self, ts, year):
        """(time type, fold) for the UTC timestamp *ts*."""
        chosen = None
        for event in self._events(year):
            if event[0] <= ts:
                chosen = event
        if chosen is None:
            first = self._events(year)[0]
            return self._state(not first[1]), False
        utc, enters, before, after = chosen
        fold = before > after and ts - utc < before - after
        return self._state(enters), fold


class _TZStringParser:
    def __init__(self, text, raw):
        self.text = text
        self.raw = raw
        self.pos = 0

    def fail(self):
        raise ValueError(f"Invalid TZ string: {self.text} "
                         f"(file footer {self.raw!r})")

    def peek(self):
        return self.text[self.pos:self.pos + 1]

    def at_end(self):
        return self.pos == len(self.text)

    def expect(self, ch):
        if self.peek() != ch:
            self.fail()
        self.pos += 1

    def digits(self, least, most):
        start = self.pos
        while (self.pos < len(self.text) and self.pos - start < most
               and self.text[self.pos] in "0123456789"):
            self.pos += 1
        if self.pos - start < least:
            self.fail()
        return int(self.text[start:self.pos])

    def name(self):
        if self.peek() == "<":
            close = self.text.find(">", self.pos)
            if close < 0:
                self.fail()
            inner = self.text[self.pos + 1:close]
            if not inner or not all(c.isascii() and (c.isalnum() or c in "+-")
                                    for c in inner):
                self.fail()
            self.pos = close + 1
            return inner
        start = self.pos
        while (self.pos < len(self.text) and self.text[self.pos].isascii()
               and self.text[self.pos].isalpha()):
            self.pos += 1
        if self.pos - start < 3:
            self.fail()
        return self.text[start:self.pos]

    def clock(self, max_hours):
        """[+-]hh[:mm[:ss]] as signed seconds."""
        sign = 1
        if self.peek() in ("+", "-"):
            sign = -1 if self.peek() == "-" else 1
            self.pos += 1
        hours = self.digits(1, 3 if max_hours > 99 else 2)
        if hours > max_hours:
            self.fail()
        minutes = seconds = 0
        if self.peek() == ":":
            self.pos += 1
            minutes = self.digits(2, 2)
            if self.peek() == ":":
                self.pos += 1
                seconds = self.digits(2, 2)
        if minutes > 59 or seconds > 59:
            self.fail()
        return sign * (hours * 3600 + minutes * 60 + seconds)

    def offset_ahead(self):
        return self.peek() in ("+", "-") or self.peek() in tuple("0123456789")

    def rule(self):
        if self.peek() == "J":
            self.pos += 1
            n = self.digits(1, 3)
            if not 1 <= n <= 365:
                self.fail()
            spec = ("J", n)
        elif self.peek() == "M":
            self.pos += 1
            month = self.digits(1, 2)
            self.expect(".")
            week = self.digits(1, 1)
            self.expect(".")
            weekday = self.digits(1, 1)
            if not (1 <= month <= 12 and 1 <= week <= 5 and weekday <= 6):
                self.fail()
            spec = ("M", month, week, weekday)
        else:
            n = self.digits(1, 3)
            if n > 365:
                self.fail()
            spec = ("N", n)
        at = 2 * 3600
        if self.peek() == "/":
            self.pos += 1
            at = self.clock(167)
        return spec, at


def _parse_tz_str(raw):
    """Parse a TZif footer into a fixed _TTInfo or a _Rule."""
    text = raw.decode()
    p = _TZStringParser(text, raw)
    std_name = p.name()
    std_offset = -p.clock(24)           # POSIX offsets count westwards
    std = _TTInfo(std_offset, 0, std_name)
    if p.at_end():
        return std
    dst_name = p.name()
    dst_offset = std_offset + 3600
    if p.offset_ahead():
        dst_offset = -p.clock(24)
    p.expect(",")
    start = p.rule()
    p.expect(",")
    end = p.rule()
    if not p.at_end():
        p.fail()
    dst = _TTInfo(dst_offset, dst_offset - std_offset, dst_name)
    return _Rule(std, dst, start, end)


# --------------------------------------------------------------------------
# ZoneInfo

class ZoneInfo(tzinfo):
    """IANA time zone loaded from TZif data, identified by its key."""

    _strong_cache_size = 8
    _strong_cache = collections.OrderedDict()
    _weak_cache = weakref.WeakValueDictionary()
    __module__ = "zoneinfo"

    def __init_subclass__(cls):
        # every subclass caches its own instances
        cls._strong_cache = collections.OrderedDict()
        cls._weak_cache = weakref.WeakValueDictionary()

    def __new__(cls, key):
        weak = cls._weak_cache
        zone = weak.get(key, None)
        if zone is None:
            zone = weak.setdefault(key, cls._new_instance(key))
            if isinstance(zone, cls):
                zone._from_cache = True
        if not isinstance(zone, cls):
            raise RuntimeError(f"Unexpected instance of "
                               f"{type(zone).__name__} in {cls.__name__} "
                               f"weak cache for key {key!r}")
        # keep the most recently used zones alive
        strong = cls._strong_cache
        strong[key] = strong.pop(key, zone)
        if len(strong) > cls._strong_cache_size:
            strong.popitem(last=False)
        return zone

    @classmethod
    def no_cache(cls, key):
        """Load *key* afresh, bypassing (and not populating) the cache."""
        zone = cls._new_instance(key)
        zone._from_cache = False
        return zone

    @classmethod
    def _new_instance(cls, key):
        zone = super().__new__(cls)
        zone._key = key
        zone._file_path = _tzpath.find_tzfile(key)
        if zone._file_path is not None:
            source = open(zone._file_path, "rb")
        else:
            source = _common.load_tzdata(key)
        with source as fobj:
            zone._load_file(fobj)
        return zone

    @classmethod
    def from_file(cls, file_obj, /, key=None):
        """Create a zone from an open binary TZif file (never cached)."""
        zone = super().__new__(cls)
        zone._key = key
        zone._file_path = None
        zone._load_file(file_obj)
        zone._file_repr = repr(file_obj)
        # zones read from a stream cannot be recreated by key
        zone.__reduce__ = zone._file_reduce
        return zone

    @classmethod
    def clear_cache(cls, *, only_keys=None):
        """Drop cached zones (all of them, or those in *only_keys*)."""
        if only_keys is None:
            cls._weak_cache.clear()
            cls._strong_cache.clear()
        else:
            for key in only_keys:
                cls._weak_cache.pop(key, None)
                cls._strong_cache.pop(key, None)

    @property
    def key(self):
        return self._key

    # -- tzinfo protocol ------------------------------------------------

    def utcoffset(self, dt):
        return self._info_for(dt).utcoff

    def dst(self, dt):
        return self._info_for(dt).dstoff

    def tzname(self, dt):
        return self._info_for(dt).tzname

    def fromutc(self, dt):
        """Convert *dt* (UTC wall time with tzinfo=self) to local time."""
        if not isinstance(dt, datetime):
            raise TypeError("fromutc() requires a datetime argument")
        if dt.tzinfo is not self:
            raise ValueError("dt.tzinfo is not self")

        ts = _local_seconds(dt)
        times = self._trans_utc
        count = len(times)
        index = bisect.bisect_right(times, ts)
        if count and index == 0:
            info, fold = self._tti_before, False
        elif count == 0 or ts > times[-1]:
            if isinstance(self._tz_after, _Rule):
                info, fold = self._tz_after.info_utc(ts, dt.year)
            else:
                info, fold = self._tz_after, False
            if count and not fold:
                fold = self._fold_after(count, ts)
        else:
            info = self._ttinfos[index - 1]
            fold = self._fold_after(index, ts)

        local = dt + info.utcoff
        if fold:
            return local.replace(fold=1)
        return local

    def _fold_after(self, index, ts):
        """Whether *ts* lies in the repeated hour after transition
        number index-1."""
        current = self._ttinfos[index - 1].offset_seconds
        if index >= 2:
            previous = self._ttinfos[index - 2].offset_seconds
        else:
            previous = self._tti_before.offset_seconds
        return previous - current > ts - self._trans_utc[index - 1]

    def _info_for(self, dt):
        if dt is None:
            # without a date only a fixed offset can be answered
            if self._fixed_offset:
                return self._tz_after
            return _NO_INFO
        ts = _local_seconds(dt)
        walls = self._trans_local[dt.fold]
        if walls:
            if ts < walls[0]:
                return self._tti_before
            if ts < walls[-1]:
                return self._ttinfos[bisect.bisect_right(walls, ts) - 1]
        # at or after the last transition
        if isinstance(self._tz_after, _Rule):
            return self._tz_after.info_local(ts, dt.year, dt.fold)
        return self._tz_after

    # -- loading ----------------------------------------------------------

    def _load_file(self, fobj):
        indices, times, offsets, isdst, abbrs, tz_str = \
            _common.load_data(fobj)
        dst_offsets = self._dst_offsets(indices, offsets, isdst)
        types = [_TTInfo(offsets[i], dst_offsets[i], abbrs[i])
                 for i in range(len(offsets))]

        self._trans_utc = list(times)
        self._ttinfos = [types[i] for i in indices]

        # local time before the first transition: the first standard type
        self._tti_before = next(
            (types[i] for i in range(len(types)) if not isdst[i]),
            types[0] if types else None)

        if tz_str:
            self._tz_after = _parse_tz_str(tz_str)
        elif self._ttinfos:
            self._tz_after = self._ttinfos[-1]
        elif types:
            self._tz_after = types[0]
        else:
            raise ValueError("No time zone information found.")

        self._trans_local = self._wall_times()

        if isinstance(self._tz_after, _Rule):
            self._fixed_offset = False
        elif not types:
            self._fixed_offset = True
        else:
            self._fixed_offset = all(t == self._tz_after for t in types)

    def _wall_times(self):
        """Local wall-clock times of the transitions for fold=0 and
        fold=1: a repeated or skipped hour belongs to the earlier offset
        for fold=0 and to the later one for fold=1."""
        fold0 = []
        fold1 = []
        previous = self._tti_before
        for utc, info in zip(self._trans_utc, self._ttinfos):
            before = previous.offset_seconds if previous is not None \
                else info.offset_seconds
            after = info.offset_seconds
            fold0.append(utc + max(before, after))
            fold1.append(utc + min(before, after))
            previous = info
        return fold0, fold1

    @staticmethod
    def _dst_offsets(indices, offsets, isdst):
        """DST amount of every local time type: the difference to the
        standard-time type next to it in the transition sequence (one hour
        when it cannot be told)."""
        result = [0] * len(offsets)
        for position, kind in enumerate(indices):
            if not isdst[kind] or result[kind]:
                continue
            amount = 0
            if position > 0 and not isdst[indices[position - 1]]:
                amount = offsets[kind] - offsets[indices[position - 1]]
            if not amount and position + 1 < len(indices) \
                    and not isdst[indices[position + 1]]:
                amount = offsets[kind] - offsets[indices[position + 1]]
            result[kind] = amount
        for kind, dst in enumerate(isdst):
            if dst and not result[kind]:
                result[kind] = 3600
        return result

    # -- representation and pickling ------------------------------------

    def __str__(self):
        if self._key is not None:
            return f"{self._key}"
        return repr(self)

    def __repr__(self):
        if self._key is not None:
            return f"{type(self).__name__}(key={self._key!r})"
        return f"{type(self).__name__}.from_file({self._file_repr})"

    def __reduce__(self):
        return (self.__class__._unpickle, (self._key, self._from_cache))

    def _file_reduce(self):
        import pickle
        raise pickle.PicklingError(
            "Cannot pickle a ZoneInfo file created from a file stream.")

    @classmethod
    def _unpickle(cls, key, from_cache, /):
        if from_cache:
            return cls(key)
        return cls.no_cache(key)
