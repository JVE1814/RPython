"""Create temporary files and directories safely.

Public interface:

  TemporaryFile, NamedTemporaryFile, SpooledTemporaryFile,
  TemporaryDirectory -- high level objects that clean up after themselves.
  mkstemp, mkdtemp     -- low level creation; the caller removes the result.
  mktemp               -- returns an unused name only (unsafe, kept for
                          compatibility).
  gettempdir, gettempdirb, gettempprefix, gettempprefixb -- defaults.

Module variables:

  tempdir  -- when not None, the default directory for new temporary items.
  template -- the default name prefix.
  TMP_MAX  -- how many candidate names are tried before giving up.
"""

__all__ = [
    "NamedTemporaryFile", "TemporaryFile", "SpooledTemporaryFile",
    "TemporaryDirectory",
    "mkstemp", "mkdtemp", "mktemp",
    "TMP_MAX", "gettempprefix", "tempdir", "gettempdir",
    "gettempprefixb", "gettempdirb",
]

import errno as _errno
import io as _io
import os as _os
import shutil as _shutil
import sys as _sys
import types as _types
import warnings as _warnings
import weakref as _weakref
import _thread
from random import Random as _Random

_allocate_lock = _thread.allocate_lock


# --------------------------------------------------------------------------
# open(2) flags

def _compute_open_flags():
    text = _os.O_RDWR | _os.O_CREAT | _os.O_EXCL
    for extra in ("O_NOFOLLOW", "O_NOINHERIT"):
        text |= getattr(_os, extra, 0)
    binary = text | getattr(_os, "O_BINARY", 0)
    return text, binary

_text_openflags, _bin_openflags = _compute_open_flags()
del _compute_open_flags

TMP_MAX = getattr(_os, "TMP_MAX", 10000)

template = "tmp"

# Guards lazy initialisation of the module-level caches below.
_once_lock = _allocate_lock()

# Whether O_TMPFILE is usable; cleared after the first failure.
_O_TMPFILE_WORKS = hasattr(_os, "O_TMPFILE")


# --------------------------------------------------------------------------
# Small helpers

def _exists(path):
    try:
        _os.lstat(path)
    except OSError:
        return False
    return True


def _infer_return_type(*parts):
    """Decide whether names built from *parts* are str or bytes.

    None entries are ignored; path-like objects count as whatever their
    __fspath__() yields.  Mixing str and bytes raises TypeError.  With no
    informative argument the answer is str.
    """
    kind = None
    for part in parts:
        if part is None:
            continue
        if isinstance(part, _os.PathLike):
            part = _os.fspath(part)
        this = bytes if isinstance(part, bytes) else str
        if kind is None:
            kind = this
        elif kind is not this:
            raise TypeError("Can't mix bytes and non-bytes in "
                            "path components.")
    return str if kind is None else kind


def _sanitize_params(prefix, suffix, dir):
    """Fill in defaults for the (prefix, suffix, dir) triple.

    Returns (prefix, suffix, dir, output_type).
    """
    output_type = _infer_return_type(prefix, suffix, dir)
    if (prefix, suffix, dir) == (None, None, None) and isinstance(tempdir, bytes):
        # A bytes module-level tempdir selects bytes results when the
        # caller supplied nothing at all.
        output_type = bytes
    if suffix is None:
        suffix = output_type()
    if prefix is None:
        prefix = template if output_type is str else _os.fsencode(template)
    if dir is None:
        dir = gettempdir() if output_type is str else gettempdirb()
    return prefix, suffix, dir, output_type


class _RandomNameSequence:
    """Endless iterator of eight-character random names.

    The underlying generator is recreated after a fork so that parent and
    child do not produce the same sequence.
    """

    characters = "abcdefghijklmnopqrstuvwxyz0123456789_"

    def __init__(self):
        self._owner_pid = None
        self._random = None

    @property
    def rng(self):
        pid = _os.getpid()
        if pid != self._owner_pid:
            self._random = _Random()
            self._owner_pid = pid
        return self._random

    def __iter__(self):
        return self

    def __next__(self):
        pick = self.rng.choice
        alphabet = self.characters
        return "".join([pick(alphabet) for _ in range(8)])


def _candidate_tempdir_list():
    """Directories worth trying as the default temporary directory."""
    found = []
    for var in ("TMPDIR", "TEMP", "TMP"):
        value = _os.getenv(var)
        if value:
            found.append(value)

    if _os.name == "nt":
        drive = _os.path.splitdrive(_os.environ.get("SYSTEMROOT", r"c:\windows"))[0]
        found.extend([_os.path.expanduser(r"~\AppData\Local\Temp"),
                      _os.path.expandvars(r"%SYSTEMROOT%\Temp"),
                      drive + "\\temp", drive + "\\tmp", r"c:\temp", r"c:\tmp",
                      r"\temp", r"\tmp"])
    else:
        found.extend(["/tmp", "/var/tmp", "/usr/tmp"])

    try:
        found.append(_os.getcwd())
    except (AttributeError, OSError):
        found.append(_os.curdir)
    return found


def _probe_directory(directory, names):
    """Return True if a file can be created, written and removed in
    *directory*.  Raises nothing for ordinary failures."""
    for _ in range(100):
        probe = _os.path.join(directory, next(names))
        try:
            fd = _os.open(probe, _bin_openflags, 0o600)
        except FileExistsError:
            continue
        except PermissionError:
            # Windows reports an existing directory of the same name as
            # a permission problem; anything else means "not usable".
            if (_os.name == "nt" and _os.path.isdir(directory)
                    and _os.access(directory, _os.W_OK)):
                continue
            return False
        except OSError:
            return False
        try:
            try:
                _os.write(fd, b"blat")
            finally:
                _os.close(fd)
        except OSError:
            _remove_quietly(probe)
            return False
        _remove_quietly(probe)
        return True
    return False


def _remove_quietly(path):
    try:
        _os.unlink(path)
    except OSError:
        pass


def _get_default_tempdir():
    """Find the first candidate directory that is actually writable."""
    names = _RandomNameSequence()
    candidates = _candidate_tempdir_list()
    for directory in candidates:
        if directory != _os.curdir:
            directory = _os.path.abspath(directory)
        if _probe_directory(directory, names):
            return directory
    raise FileNotFoundError(_errno.ENOENT,
                            "No usable temporary directory found in %s"
                            % candidates)


_name_sequence = None

def _get_candidate_names():
    """Shared _RandomNameSequence instance (created on first use)."""
    global _name_sequence
    if _name_sequence is None:
        with _once_lock:
            if _name_sequence is None:
                _name_sequence = _RandomNameSequence()
    return _name_sequence


def _candidate_paths(dir, pre, suf, output_type):
    names = _get_candidate_names()
    for _ in range(TMP_MAX):
        name = next(names)
        if output_type is bytes:
            name = _os.fsencode(name)
        yield _os.path.join(dir, pre + name + suf)


def _windows_dir_collision(dir):
    return (_os.name == "nt" and _os.path.isdir(dir)
            and _os.access(dir, _os.W_OK))


def _mkstemp_inner(dir, pre, suf, flags, output_type):
    """Create and open a new file; return (fd, absolute path)."""
    dir = _os.path.abspath(dir)
    for path in _candidate_paths(dir, pre, suf, output_type):
        _sys.audit("tempfile.mkstemp", path)
        try:
            fd = _os.open(path, flags, 0o600)
        except FileExistsError:
            continue
        except PermissionError:
            if _windows_dir_collision(dir):
                continue
            raise
        return fd, path
    raise FileExistsError(_errno.EEXIST, "No usable temporary file name found")


# --------------------------------------------------------------------------
# Defaults

def gettempprefix():
    """The default prefix for temporary names, as str."""
    return _os.fsdecode(template)

def gettempprefixb():
    """The default prefix for temporary names, as bytes."""
    return _os.fsencode(template)

tempdir = None

def _resolved_tempdir():
    global tempdir
    if tempdir is None:
        with _once_lock:
            if tempdir is None:
                tempdir = _get_default_tempdir()
    return tempdir

def gettempdir():
    """The default directory for temporary files, as str."""
    return _os.fsdecode(_resolved_tempdir())

def gettempdirb():
    """The default directory for temporary files, as bytes."""
    return _os.fsencode(_resolved_tempdir())


# --------------------------------------------------------------------------
# Low level API

def mkstemp(suffix=None, prefix=None, dir=None, text=False):
    """Create a new file that only the current user can access.

    Returns (fd, absolute_path).  The file is opened read-write; the
    caller is responsible for closing the descriptor and removing the
    file.  If *text* is true the file is opened in text mode on platforms
    that make a difference.
    """
    prefix, suffix, dir, output_type = _sanitize_params(prefix, suffix, dir)
    flags = _text_openflags if text else _bin_openflags
    return _mkstemp_inner(dir, prefix, suffix, flags, output_type)


def mkdtemp(suffix=None, prefix=None, dir=None):
    """Create a new directory (mode 0o700) and return its absolute path.

    The caller is responsible for removing it.
    """
    prefix, suffix, dir, output_type = _sanitize_params(prefix, suffix, dir)
    dir = _os.path.abspath(dir)
    for path in _candidate_paths(dir, prefix, suffix, output_type):
        _sys.audit("tempfile.mkdtemp", path)
        try:
            _os.mkdir(path, 0o700)
        except FileExistsError:
            continue
        except PermissionError:
            if _windows_dir_collision(dir):
                continue
            raise
        return path
    raise FileExistsError(_errno.EEXIST, "No usable temporary directory name found")


def mktemp(suffix="", prefix=template, dir=None):
    """Return a name that did not exist when it was chosen.

    Deprecated: another process may create the file before the caller
    does.  Prefer mkstemp() or NamedTemporaryFile().
    """
    if dir is None:
        dir = gettempdir()
    names = _get_candidate_names()
    for _ in range(TMP_MAX):
        candidate = _os.path.join(dir, prefix + next(names) + suffix)
        if not _exists(candidate):
            return candidate
    raise FileExistsError(_errno.EEXIST, "No usable temporary filename found")


# --------------------------------------------------------------------------
# NamedTemporaryFile

class _TemporaryFileCloser:
    """Owns the underlying file object and the deletion policy.

    Kept separate from the wrapper so the wrapper can be collected
    independently of bound methods handed out to callers.
    """

    def __init__(self, file, name, delete=True, delete_on_close=True):
        self.file = file
        self.name = name
        self.delete = delete
        self.delete_on_close = delete_on_close
        self.close_called = False

    def _unlink(self):
        try:
            _os.unlink(self.name)
        except FileNotFoundError:
            pass

    def close(self):
        """Close the file; unlink it if deletion on close was requested."""
        if self.close_called:
            return
        self.close_called = True
        try:
            self.file.close()
        finally:
            if self.delete and self.delete_on_close:
                self._unlink()

    def cleanup(self, warn_owner=None):
        """Close and (if delete) unlink, regardless of delete_on_close."""
        if warn_owner is not None and not self.close_called:
            _warnings.warn("Implicitly cleaning up %r" % (warn_owner,),
                           ResourceWarning, stacklevel=3)
        try:
            if not self.close_called:
                self.close_called = True
                self.file.close()
        finally:
            if self.delete:
                self._unlink()


class _TemporaryFileWrapper:
    """File-like proxy around a named temporary file.

    Attribute access is forwarded to the real file object; methods are
    returned wrapped so that the proxy stays alive while a caller holds
    one of them.
    """

    def __init__(self, file, name, delete=True, delete_on_close=True):
        self.file = file
        self.name = name
        self._closer = _TemporaryFileCloser(file, name, delete, delete_on_close)

    def __repr__(self):
        return "<%s file=%r>" % (type(self).__name__, self.__dict__.get("file"))

    def __getattr__(self, name):
        file = self.__dict__.get("file")
        if file is None:
            raise AttributeError(name)
        value = getattr(file, name)
        if callable(value) and not isinstance(value, int):
            target = value
            proxy = self

            def forward(*args, **kwargs):
                # Referencing the proxy keeps it (and the file) alive.
                proxy
                return target(*args, **kwargs)
            try:
                forward.__name__ = target.__name__
                forward.__doc__ = target.__doc__
            except (AttributeError, TypeError):
                pass
            return forward
        return value

    def __enter__(self):
        self.file.__enter__()
        return self

    def __exit__(self, exc_type, exc, tb):
        try:
            return self.file.__exit__(exc_type, exc, tb)
        finally:
            self._closer.cleanup()

    def close(self):
        """Close the file (removing it when delete_on_close applies)."""
        self._closer.close()

    def __iter__(self):
        # A generator keeps a reference to self while the caller iterates.
        for line in self.file:
            yield line

    def __del__(self):
        closer = self.__dict__.get("_closer")
        if closer is None:
            return
        if closer.delete and not closer.delete_on_close and not closer.close_called:
            closer.cleanup(warn_owner=self)
        else:
            closer.close()

    __class_getitem__ = classmethod(_types.GenericAlias)


def NamedTemporaryFile(mode="w+b", buffering=-1, encoding=None,
                       newline=None, suffix=None, prefix=None,
                       dir=None, delete=True, *, errors=None,
                       delete_on_close=True):
    """Create a temporary file that has a visible name in the filesystem.

    The arguments mode, buffering, encoding, newline and errors are passed
    to io.open(); suffix, prefix and dir work as for mkstemp().  With
    delete true the file is removed when it is closed (if delete_on_close)
    or at the latest when the context manager exits / the object is
    finalized.  The returned object has a .name and a .file attribute.
    """
    prefix, suffix, dir, output_type = _sanitize_params(prefix, suffix, dir)
    flags = _bin_openflags
    if _os.name == "nt" and delete and delete_on_close:
        flags |= getattr(_os, "O_TEMPORARY", 0)
    if "b" not in mode:
        encoding = _io.text_encoding(encoding)

    created = []

    def opener(_ignored, _flags):
        fd, path = _mkstemp_inner(dir, prefix, suffix, flags, output_type)
        created.append(path)
        return fd

    try:
        file = _io.open(dir, mode, buffering=buffering, newline=newline,
                        encoding=encoding, errors=errors, opener=opener)
        try:
            raw = getattr(file, "buffer", file)
            raw = getattr(raw, "raw", raw)
            raw.name = created[0] if created else None
        except Exception:
            pass
        try:
            return _TemporaryFileWrapper(file, created[0] if created else None,
                                         delete, delete_on_close)
        except BaseException:
            file.close()
            raise
    except BaseException:
        if created and not (_os.name == "nt" and delete):
            _remove_quietly(created[0])
        raise


# --------------------------------------------------------------------------
# TemporaryFile

if _os.name != "posix" or _sys.platform == "cygwin":
    # Without unlink-while-open semantics an anonymous file cannot exist.
    TemporaryFile = NamedTemporaryFile

else:
    def _open_anonymous(dir, flags):
        """Try O_TMPFILE; return an fd or None if unsupported."""
        global _O_TMPFILE_WORKS
        if not _O_TMPFILE_WORKS:
            return None
        tmp_flags = (flags | _os.O_TMPFILE) & ~_os.O_CREAT
        try:
            return _os.open(dir, tmp_flags, 0o600)
        except IsADirectoryError:
            # The kernel ignores unknown flags and treated this as an
            # attempt to open the directory itself: no O_TMPFILE support.
            _O_TMPFILE_WORKS = False
        except OSError:
            # Unsupported by this filesystem, or a real error which the
            # regular code path will report.
            pass
        return None

    def TemporaryFile(mode="w+b", buffering=-1, encoding=None,
                      newline=None, suffix=None, prefix=None,
                      dir=None, *, errors=None):
        """Create a temporary file without a name in the filesystem.

        Arguments are as for NamedTemporaryFile() (without delete).  The
        result is a regular file object that disappears once closed.
        """
        prefix, suffix, dir, output_type = _sanitize_params(prefix, suffix, dir)
        flags = _bin_openflags
        if "b" not in mode:
            encoding = _io.text_encoding(encoding)

        fd = _open_anonymous(dir, flags)
        if fd is None:
            fd, path = _mkstemp_inner(dir, prefix, suffix, flags, output_type)
            try:
                _os.unlink(path)
            except BaseException:
                _os.close(fd)
                raise
        try:
            return _io.open(fd, mode, buffering=buffering, newline=newline,
                            encoding=encoding, errors=errors)
        except BaseException:
            # io.open() may already have closed the descriptor (e.g. when
            # the text layer fails after the raw file took ownership).
            try:
                _os.close(fd)
            except OSError:
                pass
            raise


# --------------------------------------------------------------------------
# SpooledTemporaryFile

class SpooledTemporaryFile(_io.IOBase):
    """A file that lives in memory until it grows past max_size (or
    fileno() is requested), then moves to a real TemporaryFile."""

    _rolled = False

    def __init__(self, max_size=0, mode="w+b", buffering=-1,
                 encoding=None, newline=None,
                 suffix=None, prefix=None, dir=None, *, errors=None):
        if "b" in mode:
            self._file = _io.BytesIO()
        else:
            encoding = _io.text_encoding(encoding)
            self._file = _io.TextIOWrapper(_io.BytesIO(), encoding=encoding,
                                           errors=errors, newline=newline)
        self._max_size = max_size
        self._rolled = False
        self._TemporaryFileArgs = {
            "mode": mode, "buffering": buffering, "suffix": suffix,
            "prefix": prefix, "encoding": encoding, "newline": newline,
            "dir": dir, "errors": errors,
        }

    __class_getitem__ = classmethod(_types.GenericAlias)

    def _check(self, file):
        if self._rolled:
            return
        limit = self._max_size
        if limit and file.tell() > limit:
            self.rollover()

    def rollover(self):
        """Move the data to a real temporary file (once)."""
        if self._rolled:
            return
        memory = self._file
        disk = TemporaryFile(**self._TemporaryFileArgs)
        del self._TemporaryFileArgs
        position = memory.tell()
        if hasattr(disk, "buffer"):
            disk.buffer.write(memory.detach().getvalue())
        else:
            disk.write(memory.getvalue())
        disk.seek(position, 0)
        self._file = disk
        self._rolled = True

    # -- context management / lifetime

    def __enter__(self):
        if self._file.closed:
            raise ValueError("Cannot enter context with closed file")
        return self

    def __exit__(self, exc_type, exc, tb):
        self._file.close()

    def __iter__(self):
        return self._file.__iter__()

    def __next__(self):
        return self._file.__next__()

    def __del__(self):
        file = self.__dict__.get("_file")
        if file is None or file.closed:
            return
        _warnings.warn("Unclosed file {!r}".format(self), ResourceWarning,
                       stacklevel=2, source=self)
        self.close()

    def close(self):
        self._file.close()

    @property
    def closed(self):
        return self._file.closed

    # -- attributes

    @property
    def encoding(self):
        return self._file.encoding

    @property
    def errors(self):
        return self._file.errors

    @property
    def newlines(self):
        return self._file.newlines

    @property
    def mode(self):
        try:
            return self._file.mode
        except AttributeError:
            return self._TemporaryFileArgs["mode"]

    @property
    def name(self):
        try:
            return self._file.name
        except AttributeError:
            return None

    # -- I/O methods

    def fileno(self):
        self.rollover()
        return self._file.fileno()

    def flush(self):
        self._file.flush()

    def isatty(self):
        return self._file.isatty()

    def readable(self):
        return self._file.readable()

    def seekable(self):
        return self._file.seekable()

    def writable(self):
        return self._file.writable()

    def detach(self):
        return self._file.detach()

    def read(self, *args):
        return self._file.read(*args)

    def read1(self, *args):
        return self._file.read1(*args)

    def readinto(self, b):
        return self._file.readinto(b)

    def readinto1(self, b):
        return self._file.readinto1(b)

    def readline(self, *args):
        return self._file.readline(*args)

    def readlines(self, *args):
        return self._file.readlines(*args)

    def seek(self, *args):
        return self._file.seek(*args)

    def tell(self):
        return self._file.tell()

    def truncate(self, size=None):
        if size is None:
            return self._file.truncate()
        if self._max_size and size > self._max_size:
            self.rollover()
        return self._file.truncate(size)

    def write(self, s):
        file = self._file
        written = file.write(s)
        self._check(file)
        return written

    def writelines(self, iterable):
        if self._rolled:
            # Already on disk: no rollover check needed per chunk.
            return self._file.writelines(iterable)
        for chunk in iterable:
            self.write(chunk)


# --------------------------------------------------------------------------
# TemporaryDirectory

# Whether the directory removal below can rely on dir_fd based functions
# (immune to symlink swaps while recovering from permission errors).
_rmtree_use_dir_fd = bool(
    getattr(_shutil.rmtree, "avoids_symlink_attacks", False)
    and hasattr(_os, "fchmod"))


def _grant_access_and_retry(func, path, top, direntry, dir_fd, retry_tree):
    """Recover from a PermissionError reported by shutil.rmtree().

    Only objects strictly inside the tree rooted at *top* (and *top*
    itself) are ever modified.  When rmtree supplies the descriptor of the
    containing directory, all changes go through that descriptor, so a
    directory swapped for a symlink in the meantime cannot redirect them.
    Returns True when the failed operation has been redone.
    """
    import stat as _stat
    if dir_fd is not None and direntry is not None:
        _os.fchmod(dir_fd, 0o700)
        if func is _os.unlink or func is _os.rmdir:
            func(direntry.name, dir_fd=dir_fd)
            return True
        info = _os.stat(direntry.name, dir_fd=dir_fd, follow_symlinks=False)
        if not _stat.S_ISDIR(info.st_mode):
            return False
        _os.chmod(direntry.name, 0o700, dir_fd=dir_fd)
        retry_tree(path)
        return True

    entering = func in (_os.open, _os.scandir, _os.listdir)
    if path == top:
        # The containing directory is not ours: we may only fix the
        # top directory itself, and only if it could not be read.
        if not entering or _os.path.islink(path):
            return False
        _os.chmod(path, 0o700)
        retry_tree(path)
        return True

    # Path based fallback (no dir_fd support).
    _os.chmod(_os.path.dirname(path), 0o700)
    if func is _os.unlink or func is _os.rmdir:
        func(path)
    elif entering and not _os.path.islink(path):
        _os.chmod(path, 0o700)
        retry_tree(path)
    else:
        return False
    return True


class TemporaryDirectory:
    """A directory created with mkdtemp() and removed with its contents
    on cleanup(), on context manager exit, or when the object is
    garbage collected (with a ResourceWarning).

        with TemporaryDirectory() as path:
            ...
    """

    def __init__(self, suffix=None, prefix=None, dir=None,
                 ignore_cleanup_errors=False, *, delete=True):
        self.name = mkdtemp(suffix, prefix, dir)
        self._ignore_cleanup_errors = ignore_cleanup_errors
        self._delete = delete
        self._finalizer = _weakref.finalize(
            self, self._cleanup, self.name,
            warn_message="Implicitly cleaning up {!r}".format(self),
            ignore_errors=self._ignore_cleanup_errors, delete=self._delete)

    @classmethod
    def _rmtree(cls, name, ignore_errors=False, repeated=False):
        def retry_tree(path):
            if path == name and repeated:
                raise PermissionError(_errno.EACCES,
                                      "Permission denied", path)
            cls._rmtree(path, ignore_errors=ignore_errors,
                        repeated=(path == name))

        def on_error(func, path, exc, *, direntry=None, dir_fd=None):
            if isinstance(exc, FileNotFoundError):
                return
            if isinstance(exc, PermissionError):
                try:
                    if _grant_access_and_retry(func, path, name, direntry,
                                               dir_fd, retry_tree):
                        return
                except FileNotFoundError:
                    return
                except OSError as again:
                    if again is not exc and not isinstance(again, PermissionError):
                        exc = again
            if not ignore_errors:
                raise exc

        if _rmtree_use_dir_fd:
            _shutil.rmtree(name, onexc=on_error, _onexc_kwargs=True)
        else:
            _shutil.rmtree(name, onexc=on_error)

    @classmethod
    def _cleanup(cls, name, warn_message, ignore_errors=False, delete=True):
        if delete:
            cls._rmtree(name, ignore_errors=ignore_errors)
            _warnings.warn(warn_message, ResourceWarning)

    def __repr__(self):
        return "<{} {!r}>".format(self.__class__.__name__, self.name)

    def __enter__(self):
        return self.name

    def __exit__(self, exc_type, exc, tb):
        if self._delete:
            self.cleanup()

    def cleanup(self):
        """Remove the directory and everything below it."""
        if self._finalizer.detach() or _os.path.exists(self.name):
            self._rmtree(self.name, ignore_errors=self._ignore_cleanup_errors)

    __class_getitem__ = classmethod(_types.GenericAlias)
