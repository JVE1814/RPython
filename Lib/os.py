"""Portable access to operating system services.

This module re-exports everything from the platform module (posix), makes
``os.path`` refer to the matching path module (posixpath), and adds
higher-level helpers built on top of them: makedirs/removedirs/renames,
walk/fwalk, the environ mappings, the exec*/spawn* families, popen, fdopen,
fsencode/fsdecode and the PathLike ABC.
"""

import abc
import sys
import stat as st

from _collections_abc import _check_methods

GenericAlias = type(list[int])

__all__ = ["altsep", "curdir", "pardir", "sep", "pathsep", "linesep",
           "defpath", "name", "path", "devnull", "SEEK_SET", "SEEK_CUR",
           "SEEK_END", "fsencode", "fsdecode", "get_exec_path", "fdopen",
           "extsep"]


def _exists(name):
    return name in globals()


def _public_names(module):
    names = getattr(module, '__all__', None)
    if names is not None:
        return list(names)
    return [n for n in dir(module) if not n.startswith('_')]


if 'posix' in sys.builtin_module_names:
    name = 'posix'
    linesep = '\n'
    from posix import *
    from posix import _exit
    __all__.append('_exit')
    try:
        from posix import _have_functions
    except ImportError:
        pass
    try:
        from posix import _create_environ
    except ImportError:
        pass
    import posixpath as path
    import posix as _platform
elif 'nt' in sys.builtin_module_names:
    name = 'nt'
    linesep = '\r\n'
    from nt import *
    from nt import _exit
    __all__.append('_exit')
    try:
        from nt import _have_functions
    except ImportError:
        pass
    import ntpath as path
    import nt as _platform
else:
    raise ImportError('no os specific module found')

__all__.extend(_public_names(_platform))
del _platform

sys.modules['os.path'] = path
from os.path import (curdir, pardir, sep, pathsep, defpath, extsep, altsep,
                     devnull)


def _capabilities(pairs):
    """Set of functions whose HAVE_* capability is present."""
    found = set()
    g = globals()
    for cap, fname in pairs:
        if cap in _have_functions and fname in g:
            found.add(g[fname])
    return found


if _exists("_have_functions"):
    supports_dir_fd = _capabilities([
        ("HAVE_FACCESSAT", "access"), ("HAVE_FCHMODAT", "chmod"),
        ("HAVE_FCHOWNAT", "chown"), ("HAVE_FSTATAT", "stat"),
        ("HAVE_FUTIMESAT", "utime"), ("HAVE_LINKAT", "link"),
        ("HAVE_MKDIRAT", "mkdir"), ("HAVE_MKFIFOAT", "mkfifo"),
        ("HAVE_MKNODAT", "mknod"), ("HAVE_OPENAT", "open"),
        ("HAVE_READLINKAT", "readlink"), ("HAVE_RENAMEAT", "rename"),
        ("HAVE_SYMLINKAT", "symlink"), ("HAVE_UNLINKAT", "unlink"),
        ("HAVE_UNLINKAT", "rmdir"), ("HAVE_UTIMENSAT", "utime"),
    ])
    supports_effective_ids = _capabilities([("HAVE_FACCESSAT", "access")])
    supports_fd = _capabilities([
        ("HAVE_FCHDIR", "chdir"), ("HAVE_FCHMOD", "chmod"),
        ("MS_WINDOWS", "chmod"), ("HAVE_FCHOWN", "chown"),
        ("HAVE_FDOPENDIR", "listdir"), ("HAVE_FDOPENDIR", "scandir"),
        ("HAVE_FEXECVE", "execve"), ("HAVE_FTRUNCATE", "truncate"),
        ("HAVE_FUTIMENS", "utime"), ("HAVE_FUTIMES", "utime"),
        ("HAVE_FPATHCONF", "pathconf"),
    ])
    supports_fd.add(stat)
    if _exists("statvfs") and _exists("fstatvfs"):
        supports_fd.update(_capabilities([("HAVE_FSTATVFS", "statvfs")]))
    supports_follow_symlinks = _capabilities([
        ("HAVE_FACCESSAT", "access"), ("HAVE_FCHOWNAT", "chown"),
        ("HAVE_FSTATAT", "stat"), ("HAVE_LINKAT", "link"),
        ("HAVE_UTIMENSAT", "utime"),
    ])
    del _have_functions
del _capabilities

SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2


def makedirs(name, mode=0o777, exist_ok=False, *, parent_mode=None):
    """makedirs(name [, mode=0o777][, exist_ok=False][, parent_mode=None])

    Create a directory and any missing parent directories.  If the leaf
    already exists, OSError is raised unless exist_ok is true and the leaf
    is a directory.  Missing parents are created with *parent_mode* (default
    0o777); like *mode*, it is combined with the umask.
    """
    head, tail = path.split(name)
    if not tail:
        head, tail = path.split(head)
    if head and tail and not path.exists(head):
        try:
            makedirs(head, 0o777 if parent_mode is None else parent_mode,
                     exist_ok=exist_ok, parent_mode=parent_mode)
        except FileExistsError:
            pass  # created concurrently
        here = curdir if isinstance(tail, str) else curdir.encode('ascii')
        if tail == here:
            return
    try:
        mkdir(name, mode)
    except OSError:
        if not exist_ok or not path.isdir(name):
            raise


def removedirs(name):
    """removedirs(name)

    Remove a leaf directory, then remove each now-empty parent directory
    named in the path, stopping silently at the first one that cannot be
    removed.
    """
    rmdir(name)
    head, tail = path.split(name)
    if not tail:
        head, tail = path.split(head)
    while head and tail:
        try:
            rmdir(head)
        except OSError:
            return
        head, tail = path.split(head)


def renames(old, new):
    """renames(old, new)

    Rename a file or directory, creating intermediate directories for the
    new name as needed and pruning directories of the old name that became
    empty.
    """
    head, tail = path.split(new)
    if head and tail and not path.exists(head):
        makedirs(head)
    rename(old, new)
    head, tail = path.split(old)
    if head and tail:
        try:
            removedirs(head)
        except OSError:
            pass

__all__.extend(["makedirs", "removedirs", "renames"])

# Sentinel for walk(followlinks=...) that classifies symlinks as files.
_walk_symlinks_as_files = object()


def _audit(*args):
    hook = getattr(sys, 'audit', None)
    if hook is not None:
        hook(*args)


def walk(top, topdown=True, onerror=None, followlinks=False):
    """Directory tree generator yielding (dirpath, dirnames, filenames).

    With topdown true (the default) a directory is reported before its
    subdirectories, and the caller may prune or reorder dirnames in place.
    Errors from scandir() are ignored unless onerror is given, in which
    case it is called with the OSError.  Symbolic links to directories are
    only descended into when followlinks is true.
    """
    _audit("os.walk", top, topdown, onerror, followlinks)

    pending = [fspath(top)]
    islink, join = path.islink, path.join
    while pending:
        item = pending.pop()
        if isinstance(item, tuple):
            yield item
            continue
        top = item
        dirs = []
        nondirs = []
        descend_into = []
        try:
            it = scandir(top)
        except OSError as error:
            if onerror is not None:
                onerror(error)
            continue

        failed = False
        with it:
            while True:
                try:
                    entry = next(it)
                except StopIteration:
                    break
                except OSError as error:
                    if onerror is not None:
                        onerror(error)
                    failed = True
                    break
                try:
                    if followlinks is _walk_symlinks_as_files:
                        is_dir = (entry.is_dir(follow_symlinks=False)
                                  and not entry.is_junction())
                    else:
                        is_dir = entry.is_dir()
                except OSError:
                    is_dir = False
                (dirs if is_dir else nondirs).append(entry.name)
                if is_dir and not topdown:
                    if followlinks is True:
                        descend = True
                    else:
                        try:
                            descend = not entry.is_symlink()
                        except OSError:
                            descend = True
                    if descend:
                        descend_into.append(entry.path)
        if failed:
            continue

        if topdown:
            yield top, dirs, nondirs
            for d in reversed(dirs):
                sub = join(top, d)
                if followlinks or not islink(sub):
                    pending.append(sub)
        else:
            pending.append((top, dirs, nondirs))
            pending.extend(reversed(descend_into))

__all__.append("walk")

if {open, stat} <= supports_dir_fd and {scandir, stat} <= supports_fd:

    _FW_WALK, _FW_YIELD, _FW_CLOSE = range(3)

    def fwalk(top=".", topdown=True, onerror=None, *, follow_symlinks=False, dir_fd=None):
        """Directory tree generator yielding (dirpath, dirnames, filenames, dirfd).

        Like walk(), but also yields a file descriptor for each directory,
        which stays valid until the next iteration step.  When
        follow_symlinks is false the walk is safe against symlink races.
        If dir_fd is given, top is interpreted relative to it.
        """
        _audit("os.fwalk", top, topdown, onerror, follow_symlinks, dir_fd)
        top = fspath(top)
        isbytes = isinstance(top, bytes)
        todo = [(_FW_WALK, (True, dir_fd, top, top, None))]
        try:
            while todo:
                yield from _fwalk_step(todo, isbytes, topdown, onerror, follow_symlinks)
        finally:
            while todo:
                action, value = todo.pop()
                if action == _FW_CLOSE:
                    close(value)

    def _fwalk_step(todo, isbytes, topdown, onerror, follow_symlinks):
        action, value = todo.pop()
        if action == _FW_CLOSE:
            close(value)
            return
        if action == _FW_YIELD:
            yield value
            return
        isroot, dirfd, toppath, topname, entry = value
        try:
            if not follow_symlinks:
                if entry is None:
                    before = stat(topname, follow_symlinks=False, dir_fd=dirfd)
                else:
                    before = entry.stat(follow_symlinks=False)
            topfd = open(topname, O_RDONLY | O_NONBLOCK, dir_fd=dirfd)
        except OSError as err:
            if isroot:
                raise
            if onerror is not None:
                onerror(err)
            return
        todo.append((_FW_CLOSE, topfd))
        if not follow_symlinks:
            if isroot and not st.S_ISDIR(before.st_mode):
                return
            if not path.samestat(before, stat(topfd)):
                return

        dirs = []
        nondirs = []
        dir_entries = None if (topdown or follow_symlinks) else []
        for entry in scandir(topfd):
            ename = entry.name
            if isbytes:
                ename = fsencode(ename)
            try:
                if entry.is_dir():
                    dirs.append(ename)
                    if dir_entries is not None:
                        dir_entries.append(entry)
                else:
                    nondirs.append(ename)
            except OSError:
                try:
                    if entry.is_symlink():
                        nondirs.append(ename)
                except OSError:
                    pass

        if topdown:
            yield toppath, dirs, nondirs, topfd
        else:
            todo.append((_FW_YIELD, (toppath, dirs, nondirs, topfd)))

        prefix = path.join(toppath, toppath[:0])
        if dir_entries is None:
            for d in reversed(dirs):
                todo.append((_FW_WALK, (False, topfd, prefix + d, d, None)))
        else:
            for d, e in zip(reversed(dirs), reversed(dir_entries)):
                todo.append((_FW_WALK, (False, topfd, prefix + d, d, e)))

    __all__.append("fwalk")


def execl(file, *args):
    """execl(file, *args)

    Replace the current process by running file with the given arguments."""
    execv(file, args)


def execle(file, *args):
    """execle(file, *args, env)

    Like execl(), but the last argument is the environment mapping."""
    execve(file, args[:-1], args[-1])


def execlp(file, *args):
    """execlp(file, *args)

    Like execl(), but file is looked up along $PATH."""
    execvp(file, args)


def execlpe(file, *args):
    """execlpe(file, *args, env)

    Like execlp(), but the last argument is the environment mapping."""
    execvpe(file, args[:-1], args[-1])


def execvp(file, args):
    """execvp(file, args)

    Replace the current process by running file (looked up along $PATH)
    with the argument list or tuple args."""
    _execvpe(file, args)


def execvpe(file, args, env):
    """execvpe(file, args, env)

    Like execvp(), using env as the environment of the new program."""
    _execvpe(file, args, env)

__all__.extend(["execl", "execle", "execlp", "execlpe", "execvp", "execvpe"])


def _execvpe(file, args, env=None):
    if env is None:
        run = lambda f: execv(f, args)
        search_env = environ
    else:
        run = lambda f: execve(f, args, env)
        search_env = env
    if path.dirname(file):
        run(file)
        return
    first_interesting = None
    last = None
    dirs = get_exec_path(search_env)
    if name != 'nt':
        file = fsencode(file)
        dirs = [fsencode(d) for d in dirs]
    for d in dirs:
        try:
            run(path.join(d, file))
        except (FileNotFoundError, NotADirectoryError) as e:
            last = e
        except OSError as e:
            last = e
            if first_interesting is None:
                first_interesting = e
    if first_interesting is not None:
        raise first_interesting
    raise last


def get_exec_path(env=None):
    """Return the list of directories searched for executables.

    env is an environment mapping (os.environ when None)."""
    import warnings
    if env is None:
        env = environ
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", BytesWarning)
        try:
            found = env.get('PATH')
        except TypeError:
            found = None
        if supports_bytes_environ:
            try:
                found_b = env[b'PATH']
            except (KeyError, TypeError):
                pass
            else:
                if found is not None:
                    raise ValueError("env cannot contain 'PATH' and b'PATH' keys")
                found = found_b
            if isinstance(found, bytes):
                found = fsdecode(found)
    if found is None:
        found = defpath
    return found.split(pathsep)


from _collections_abc import MutableMapping, Mapping


class _Environ(MutableMapping):
    """Mapping view of the process environment that keeps the real
    environment in sync through putenv()/unsetenv()."""

    def __init__(self, data, encodekey, decodekey, encodevalue, decodevalue):
        self.encodekey = encodekey
        self.decodekey = decodekey
        self.encodevalue = encodevalue
        self.decodevalue = decodevalue
        self._data = data

    def __getitem__(self, key):
        try:
            raw = self._data[self.encodekey(key)]
        except KeyError:
            raise KeyError(key) from None
        return self.decodevalue(raw)

    def __setitem__(self, key, value):
        k = self.encodekey(key)
        v = self.encodevalue(value)
        putenv(k, v)
        self._data[k] = v

    def __delitem__(self, key):
        k = self.encodekey(key)
        unsetenv(k)
        try:
            del self._data[k]
        except KeyError:
            raise KeyError(key) from None

    def __iter__(self):
        for k in list(self._data):
            yield self.decodekey(k)

    def __len__(self):
        return len(self._data)

    def __repr__(self):
        body = ", ".join(f"{self.decodekey(k)!r}: {self.decodevalue(v)!r}"
                         for k, v in self._data.items())
        return "environ({" + body + "})"

    def copy(self):
        return dict(self)

    def setdefault(self, key, value):
        if key not in self:
            self[key] = value
        return self[key]

    def clear(self):
        for k in list(self._data):
            unsetenv(k)
        self._data.clear()

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, Mapping):
            return NotImplemented
        merged = dict(self)
        merged.update(other)
        return merged

    def __ror__(self, other):
        if not isinstance(other, Mapping):
            return NotImplemented
        merged = dict(other)
        merged.update(self)
        return merged


def _make_environ(raw):
    if name == 'nt':
        def check(value):
            if not isinstance(value, str):
                raise TypeError("str expected, not %s" % type(value).__name__)
            return value
        def upper_key(key):
            return check(key).upper()
        data = {upper_key(k): v for k, v in raw.items()}
        return _Environ(data, upper_key, str, check, str)
    fsenc = sys.getfilesystemencoding()
    def encode(value):
        if not isinstance(value, str):
            raise TypeError("str expected, not %s" % type(value).__name__)
        return value.encode(fsenc, 'surrogateescape')
    def decode(value):
        return value.decode(fsenc, 'surrogateescape')
    return _Environ(raw, encode, decode, encode, decode)

environ = _make_environ(environ)
del _make_environ


if _exists("_create_environ"):
    def reload_environ():
        """Refresh os.environ (and os.environb) from the process environment."""
        fresh = _create_environ()
        if name == 'nt':
            fresh = {environ.encodekey(k): v for k, v in fresh.items()}
        data = environ._data
        data.clear()
        data.update(fresh)

    __all__.append("reload_environ")


def getenv(key, default=None):
    """Return the value of environment variable key, or default."""
    return environ.get(key, default)

supports_bytes_environ = (name != 'nt')
__all__.extend(("getenv", "supports_bytes_environ"))

if supports_bytes_environ:
    def _require_bytes(value):
        if not isinstance(value, bytes):
            raise TypeError("bytes expected, not %s" % type(value).__name__)
        return value

    environb = _Environ(environ._data, _require_bytes, bytes, _require_bytes, bytes)
    del _require_bytes

    def getenvb(key, default=None):
        """Return the value of environment variable key (bytes), or default."""
        return environb.get(key, default)

    __all__.extend(("environb", "getenvb"))


def _make_fs_codec():
    enc = sys.getfilesystemencoding()
    errs = sys.getfilesystemencodeerrors()

    def fsencode(filename):
        """Encode a str or os.PathLike to bytes using the filesystem
        encoding and error handler; bytes are returned unchanged."""
        filename = fspath(filename)
        if isinstance(filename, str):
            return filename.encode(enc, errs)
        return filename

    def fsdecode(filename):
        """Decode bytes or os.PathLike to str using the filesystem
        encoding and error handler; str is returned unchanged."""
        filename = fspath(filename)
        if isinstance(filename, bytes):
            return filename.decode(enc, errs)
        return filename

    return fsencode, fsdecode

fsencode, fsdecode = _make_fs_codec()
del _make_fs_codec


if _exists("fork") and not _exists("spawnv") and _exists("execv"):
    P_WAIT = 0
    P_NOWAIT = P_NOWAITO = 1
    __all__.extend(["P_WAIT", "P_NOWAIT", "P_NOWAITO"])

    def _spawn(mode, file, args, env, func):
        if not isinstance(args, (tuple, list)):
            raise TypeError('argv must be a tuple or a list')
        if not args or not args[0]:
            raise ValueError('argv first element cannot be empty')
        pid = fork()
        if pid == 0:
            try:
                if env is None:
                    func(file, args)
                else:
                    func(file, args, env)
            except BaseException:
                _exit(127)
            _exit(127)
        if mode == P_NOWAIT:
            return pid
        while True:
            _, status = waitpid(pid, 0)
            if not WIFSTOPPED(status):
                return waitstatus_to_exitcode(status)

    def spawnv(mode, file, args):
        """spawnv(mode, file, args) -> integer

Run file with arguments args in a new process.  With P_NOWAIT the pid is
returned; with P_WAIT the exit code (or -signal) of the process."""
        return _spawn(mode, file, args, None, execv)

    def spawnve(mode, file, args, env):
        """spawnve(mode, file, args, env) -> integer

Like spawnv(), with env as the environment of the new process."""
        return _spawn(mode, file, args, env, execve)

    def spawnvp(mode, file, args):
        """spawnvp(mode, file, args) -> integer

Like spawnv(), but file is looked up along $PATH."""
        return _spawn(mode, file, args, None, execvp)

    def spawnvpe(mode, file, args, env):
        """spawnvpe(mode, file, args, env) -> integer

Like spawnvp(), with env as the environment of the new process."""
        return _spawn(mode, file, args, env, execvpe)

    __all__.extend(["spawnv", "spawnve", "spawnvp", "spawnvpe"])


if _exists("spawnv"):
    def spawnl(mode, file, *args):
        """spawnl(mode, file, *args) -> integer

Like spawnv() with the arguments given individually."""
        return spawnv(mode, file, args)

    def spawnle(mode, file, *args):
        """spawnle(mode, file, *args, env) -> integer

Like spawnve() with the arguments given individually."""
        return spawnve(mode, file, args[:-1], args[-1])

    __all__.extend(["spawnl", "spawnle"])


if _exists("spawnvp"):
    def spawnlp(mode, file, *args):
        """spawnlp(mode, file, *args) -> integer

Like spawnvp() with the arguments given individually."""
        return spawnvp(mode, file, args)

    def spawnlpe(mode, file, *args):
        """spawnlpe(mode, file, *args, env) -> integer

Like spawnvpe() with the arguments given individually."""
        return spawnvpe(mode, file, args[:-1], args[-1])

    __all__.extend(["spawnlp", "spawnlpe"])


if sys.platform != 'vxworks':
    def popen(cmd, mode="r", buffering=-1):
        if not isinstance(cmd, str):
            raise TypeError("invalid cmd type (%s, expected string)" % type(cmd))
        if mode not in ("r", "w"):
            raise ValueError("invalid mode %r" % mode)
        if buffering == 0 or buffering is None:
            raise ValueError("popen() does not support unbuffered streams")
        import subprocess
        if mode == "r":
            proc = subprocess.Popen(cmd, shell=True, text=True,
                                    stdout=subprocess.PIPE, bufsize=buffering)
            return _wrap_close(proc.stdout, proc)
        proc = subprocess.Popen(cmd, shell=True, text=True,
                                stdin=subprocess.PIPE, bufsize=buffering)
        return _wrap_close(proc.stdin, proc)

    class _wrap_close:
        """File proxy whose close() also waits for the child process."""

        def __init__(self, stream, proc):
            self._stream = stream
            self._proc = proc

        def close(self):
            self._stream.close()
            code = self._proc.wait()
            if code == 0:
                return None
            return code if name == 'nt' else code << 8

        def __enter__(self):
            return self

        def __exit__(self, *args):
            self.close()

        def __getattr__(self, attr):
            return getattr(self._stream, attr)

        def __iter__(self):
            return iter(self._stream)

    __all__.append("popen")


def fdopen(fd, mode="r", buffering=-1, encoding=None, *args, **kwargs):
    if not isinstance(fd, int):
        raise TypeError("invalid fd type (%s, expected integer)" % type(fd))
    import io
    if "b" not in mode:
        encoding = io.text_encoding(encoding)
    return io.open(fd, mode, buffering, encoding, *args, **kwargs)


def _fspath(path):
    """Return the file system representation of path (str or bytes).

    str and bytes are returned unchanged; other objects must implement
    __fspath__() returning str or bytes."""
    if isinstance(path, (str, bytes)):
        return path
    cls = type(path)
    method = getattr(cls, '__fspath__', None)
    if method is None:
        raise TypeError("expected str, bytes or os.PathLike object, not "
                        + cls.__name__)
    result = method(path)
    if isinstance(result, (str, bytes)):
        return result
    raise TypeError("expected {}.__fspath__() to return str or bytes, not {}"
                    .format(cls.__name__, type(result).__name__))

if not _exists('fspath'):
    fspath = _fspath
    fspath.__name__ = "fspath"


class PathLike(abc.ABC):
    """Abstract base class for implementing the file system path protocol."""

    __slots__ = ()

    @abc.abstractmethod
    def __fspath__(self):
        """Return the file system path representation of the object."""
        raise NotImplementedError

    @classmethod
    def __subclasshook__(cls, subclass):
        if cls is PathLike:
            return _check_methods(subclass, '__fspath__')
        return NotImplemented

    __class_getitem__ = classmethod(GenericAlias)


def _cpu_count_override():
    getter = getattr(sys, '_get_cpu_count_config', None)
    return -1 if getter is None else getter()

if _exists('sched_getaffinity') and _cpu_count_override() < 0:
    def process_cpu_count():
        """Return the number of logical CPUs usable by the calling thread,
        or None if it cannot be determined."""
        return len(sched_getaffinity(0))
else:
    process_cpu_count = cpu_count
del _cpu_count_override
