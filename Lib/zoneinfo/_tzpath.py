"""The time zone search path (TZPATH) and zone discovery."""

import os
import sysconfig


class InvalidTZPathWarning(RuntimeWarning):
    """A relative path was found in the time zone search path."""


TZPATH = ()

# used when neither PYTHONTZPATH nor the build configuration names a path
_DEFAULT_TZPATH = ("/usr/share/zoneinfo", "/usr/lib/zoneinfo",
                   "/usr/share/lib/zoneinfo", "/etc/zoneinfo")

# warnings raised here are attributed to the first caller outside zoneinfo
_PACKAGE_DIR = os.path.dirname(__file__)


def _warn_relative(message):
    import warnings
    warnings.warn(message, InvalidTZPathWarning,
                  skip_file_prefixes=(_PACKAGE_DIR,))


def _relative_paths_message(paths):
    bad = [path for path in paths if not os.path.isabs(path)]
    return ("Paths should be absolute but found the following relative "
            "paths:\n" + "\n".join(f"    {path}" for path in bad))


def _paths_from_environment(value):
    if not value:
        return ()
    parts = value.split(os.pathsep)
    absolute = tuple(part for part in parts if os.path.isabs(part))
    if len(absolute) != len(parts):
        _warn_relative("Invalid paths specified in PYTHONTZPATH environment "
                       "variable. " + _relative_paths_message(parts))
    return absolute


def reset_tzpath(to=None):
    """Set TZPATH to the absolute directories *to*, or reinitialise it
    from PYTHONTZPATH (or the build-time default) when *to* is None."""
    global TZPATH
    if to is not None:
        if isinstance(to, (str, bytes)):
            raise TypeError(f"tzpaths must be a list or tuple, not "
                            f"{type(to)}: {to!r}")
        paths = []
        for entry in to:
            path = os.fspath(entry)
            if not isinstance(path, str):
                raise TypeError(f"tzpath entries must be str or os.PathLike "
                                f"returning str, not {type(path)}: {path!r}")
            paths.append(path)
        if not all(os.path.isabs(path) for path in paths):
            raise ValueError(_relative_paths_message(paths))
        TZPATH = tuple(paths)
        return
    value = os.environ.get("PYTHONTZPATH")
    if value is None:
        value = sysconfig.get_config_var("TZPATH")
    if value is None:
        value = os.pathsep.join(_DEFAULT_TZPATH)
    TZPATH = _paths_from_environment(value)


def find_tzfile(key):
    """Path of the TZif file for *key* in TZPATH, or None."""
    _validate_tzfile_path(key)
    for directory in TZPATH:
        candidate = os.path.join(directory, key)
        if os.path.isfile(candidate):
            return candidate
    return None


# stand-in root directory used to check that a key stays below it
_ROOT = "zoneinfo-root"


def _validate_tzfile_path(path):
    if os.path.isabs(path):
        raise ValueError(f"ZoneInfo keys may not be absolute paths, "
                         f"got: {path}")
    normalized = os.path.normpath(path)
    if len(normalized) != len(path):
        raise ValueError(f"ZoneInfo keys must be normalized relative paths, "
                         f"got: {path}")
    resolved = os.path.normpath(os.path.join(_ROOT, normalized))
    if not resolved.startswith(_ROOT + os.sep):
        raise ValueError(f"ZoneInfo keys must refer to subdirectories of "
                         f"TZPATH, got: {path}")


def _is_tzif(path):
    try:
        with open(path, "rb") as f:
            return f.read(4) == b"TZif"
    except OSError:
        return False


def available_timezones():
    """Set of every key for which a time zone can be loaded.

    This scans the TZPATH directories (and the tzdata package's zone
    list), so it is relatively expensive.
    """
    keys = set()
    try:
        from importlib import resources
        with resources.files("tzdata").joinpath("zones").open("r") as f:
            keys.update(line.strip() for line in f if line.strip())
    except (ImportError, FileNotFoundError):
        pass

    for root_dir in TZPATH:
        if not os.path.exists(root_dir):
            continue
        for directory, subdirs, files in os.walk(root_dir):
            if directory == root_dir:
                # alternative trees of the same zones
                for alternative in ("right", "posix"):
                    if alternative in subdirs:
                        subdirs.remove(alternative)
            for name in files:
                path = os.path.join(directory, name)
                key = os.path.relpath(path, start=root_dir)
                if os.sep != "/":
                    key = key.replace(os.sep, "/")
                if not key or key in keys:
                    continue
                if _is_tzif(path):
                    keys.add(key)

    # aliases for the system's local/default zone, not real keys
    keys.discard("posixrules")
    keys.discard("localtime")
    return keys


reset_tzpath()
