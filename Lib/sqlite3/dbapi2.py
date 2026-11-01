"""DB-API 2.0 module globals for sqlite3, on top of the _sqlite3 driver."""

import collections.abc
import time

try:
    import datetime
except ImportError:  # the interpreter may be built without datetime
    datetime = None

from _sqlite3 import *

paramstyle = "qmark"
apilevel = "2.0"

if datetime is not None:
    Date = datetime.date
    Time = datetime.time
    Timestamp = datetime.datetime


def DateFromTicks(ticks):
    return Date(*time.localtime(ticks)[:3])


def TimeFromTicks(ticks):
    return Time(*time.localtime(ticks)[3:6])


def TimestampFromTicks(ticks):
    return Timestamp(*time.localtime(ticks)[:6])


sqlite_version_info = tuple(int(part) for part in sqlite_version.split("."))

Binary = memoryview
collections.abc.Sequence.register(Row)


def _install_default_adapters():
    # The default date/datetime adapters and converters still work, but warn.
    import warnings

    def _deprecated(kind):
        warnings.warn(
            f"The default {kind} is deprecated as of Python 3.12; see the "
            "sqlite3 documentation for suggested replacement recipes",
            DeprecationWarning, stacklevel=3)

    def date_to_text(value):
        _deprecated("date adapter")
        return value.isoformat()

    def datetime_to_text(value):
        _deprecated("datetime adapter")
        return value.isoformat(" ")

    def text_to_date(raw):
        _deprecated("date converter")
        year, month, day = raw.split(b"-")
        return datetime.date(int(year), int(month), int(day))

    def text_to_timestamp(raw):
        _deprecated("timestamp converter")
        date_part, time_part = raw.split(b" ")
        year, month, day = (int(x) for x in date_part.split(b"-"))
        clock, _, fraction = time_part.partition(b".")
        hours, minutes, seconds = (int(x) for x in clock.split(b":"))
        micro = int(fraction.ljust(6, b"0")[:6]) if fraction else 0
        return datetime.datetime(year, month, day, hours, minutes, seconds, micro)

    register_adapter(datetime.date, date_to_text)
    register_adapter(datetime.datetime, datetime_to_text)
    register_converter("date", text_to_date)
    register_converter("timestamp", text_to_timestamp)


if datetime is not None:
    _install_default_adapters()
del _install_default_adapters
