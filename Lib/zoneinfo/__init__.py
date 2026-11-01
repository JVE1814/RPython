"""IANA time zone support (PEP 615).

    >>> from zoneinfo import ZoneInfo
    >>> from datetime import datetime
    >>> datetime(2024, 7, 1, 12, tzinfo=ZoneInfo("Europe/Paris")).utcoffset()
    datetime.timedelta(seconds=7200)

Zone data is read from the directories listed in TZPATH (configured with
the PYTHONTZPATH environment variable or reset_tzpath()), falling back to
the ``tzdata`` package when it is installed.
"""

__all__ = [
    "ZoneInfo",
    "reset_tzpath",
    "available_timezones",
    "TZPATH",
    "ZoneInfoNotFoundError",
    "InvalidTZPathWarning",
]

from . import _tzpath
from ._common import ZoneInfoNotFoundError

try:
    from _zoneinfo import ZoneInfo
except (ImportError, AttributeError):
    from ._zoneinfo import ZoneInfo

reset_tzpath = _tzpath.reset_tzpath
available_timezones = _tzpath.available_timezones
InvalidTZPathWarning = _tzpath.InvalidTZPathWarning


def __getattr__(name):
    # TZPATH is looked up dynamically because reset_tzpath() replaces it
    if name == "TZPATH":
        return _tzpath.TZPATH
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def __dir__():
    return sorted(set(globals()) | {"TZPATH"})
