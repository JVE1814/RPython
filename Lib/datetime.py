"""Dates, times, durations and time zones.

The classes come from the native ``_datetime`` module when it is available
and from the pure-Python ``_pydatetime`` otherwise; both behave the same.
"""

try:
    from _datetime import *
except ImportError:
    from _pydatetime import *

__all__ = ("date", "datetime", "time", "timedelta", "timezone", "tzinfo",
           "MINYEAR", "MAXYEAR", "UTC")
