# Root cause: pandas reads transitions from the pure-Python zoneinfo implementation
# (zoneinfo._zoneinfo.ZoneInfo: _trans_utc, _ttinfos, _tti_before, _fixed_offset,
# _tz_after) and needs _tz_after.transitions(year) -> (start, end) for the POSIX rule
# that covers dates after the last stored transition.  Without it, dates after ~2007
# (tzdata's slim files) used the last stored offset: US/Eastern was an hour off in
# winter.  (pandas arithmetic/test_datetime64, tseries/offsets, ...: ~400 failures.)
import datetime
from zoneinfo._zoneinfo import ZoneInfo as PyZoneInfo

z = PyZoneInfo("America/New_York")
rule = z._tz_after
start, end = rule.transitions(2021)
std = int(rule.std.utcoff.total_seconds())
dst = int(rule.dst.utcoff.total_seconds())
utc = datetime.timezone.utc
epoch = datetime.datetime(1970, 1, 1, tzinfo=utc)
# DST 2021 in New York: 2021-03-14 07:00 UTC .. 2021-11-07 06:00 UTC
assert epoch + datetime.timedelta(seconds=start - std) == datetime.datetime(2021, 3, 14, 7, tzinfo=utc)
assert epoch + datetime.timedelta(seconds=end - dst) == datetime.datetime(2021, 11, 7, 6, tzinfo=utc)

try:
    import pandas as pd
except ImportError:
    print("OK (pandas part skipped)")
else:
    ts = pd.Timestamp("2021-01-01 05:00", tz="UTC").tz_convert("US/Eastern")
    assert str(ts) == "2021-01-01 00:00:00-05:00", ts
    print("OK")
