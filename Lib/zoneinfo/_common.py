"""TZif file parsing and access to the tzdata package (RFC 8536)."""

import struct


class ZoneInfoNotFoundError(KeyError):
    """No time zone data could be found for a key."""


def load_tzdata(key):
    """Open the TZif resource for *key* from the tzdata package."""
    *dirs, name = key.split("/")
    package = ".".join(["tzdata.zoneinfo", *dirs])
    try:
        from importlib import resources
        return resources.files(package).joinpath(name).open("rb")
    except (ImportError, FileNotFoundError, UnicodeEncodeError,
            IsADirectoryError, NotADirectoryError):
        # ImportError: no tzdata (or no such sub-package)
        # UnicodeEncodeError: the key cannot name a file
        raise ZoneInfoNotFoundError(f"No time zone found with key {key}")


_HEADER = struct.Struct(">4s1s15x6l")


def _read_exactly(fobj, size, what):
    data = fobj.read(size)
    if len(data) != size:
        raise ValueError(f"Invalid TZif file: truncated {what}")
    return data


def _read_header(fobj):
    raw = fobj.read(_HEADER.size)
    if len(raw) != _HEADER.size or raw[:4] != b"TZif":
        raise ValueError("Invalid TZif file: magic not found")
    magic, version, *counts = _HEADER.unpack(raw)
    version = 1 if version == b"\x00" else int(version)
    return version, counts


def _read_block(fobj, counts, time_size):
    """Parse one data block; returns (times, indices, ttinfos, abbrs)."""
    isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt = counts
    if min(counts) < 0:
        raise ValueError("Invalid TZif file: negative count")
    time_format = ">%d%s" % (timecnt, "l" if time_size == 4 else "q")
    times = struct.unpack(time_format,
                          _read_exactly(fobj, timecnt * time_size,
                                        "transition times"))
    indices = _read_exactly(fobj, timecnt, "transition types")
    types = []
    for _ in range(typecnt):
        utoff, isdst, abbrind = struct.unpack(
            ">lbB", _read_exactly(fobj, 6, "local time types"))
        types.append((utoff, isdst, abbrind))
    chars = _read_exactly(fobj, charcnt, "abbreviations")
    # leap second records, standard/wall and UT/local indicators: unused
    _read_exactly(fobj, leapcnt * (time_size + 4) + isstdcnt + isutcnt,
                  "trailing records")

    if any(index >= typecnt for index in indices):
        raise ValueError("Invalid TZif file: transition type index out of "
                         "range")
    abbrs = []
    for _utoff, _isdst, abbrind in types:
        end = chars.find(b"\x00", abbrind)
        if end < 0:
            end = len(chars)
        abbrs.append(chars[abbrind:end].decode())
    return times, tuple(indices), types, abbrs


def load_data(fobj):
    """Read a TZif file.

    Returns (transition type indices, transition times (UTC seconds),
    UTC offsets, isdst flags, abbreviations, footer TZ string or None),
    the per-type lists being indexed by type number.  The 64-bit data of
    version 2+ files is used when present.
    """
    version, counts = _read_header(fobj)
    if version == 1:
        times, indices, types, abbrs = _read_block(fobj, counts, 4)
        tz_str = None
    else:
        # skip the version 1 block, then read the 64-bit one
        isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt = counts
        _read_exactly(fobj, timecnt * 5 + typecnt * 6 + charcnt +
                      leapcnt * 8 + isstdcnt + isutcnt, "version 1 data")
        version, counts = _read_header(fobj)
        times, indices, types, abbrs = _read_block(fobj, counts, 8)
        footer = fobj.read()
        if not footer.startswith(b"\n"):
            raise ValueError("Invalid TZif file: footer not found")
        end = footer.find(b"\n", 1)
        if end < 0:
            raise ValueError("Invalid TZif file: unterminated footer")
        tz_str = footer[1:end]

    utcoffsets = [utoff for utoff, _isdst, _ in types]
    isdst = [isdst for _utoff, isdst, _ in types]
    return indices, times, utcoffsets, isdst, abbrs, tz_str
