"""Run child processes and talk to them through pipes.

High-level helpers: run(), call(), check_call(), check_output(),
getstatusoutput(), getoutput().  The Popen class gives full control.
PIPE, STDOUT and DEVNULL are special values for the stdin/stdout/stderr
arguments.  Errors: SubprocessError and its subclasses CalledProcessError
and TimeoutExpired.
"""

import builtins
import contextlib
import errno
import io
import locale
import os
import signal
import sys
import threading
import time
import types
import warnings
from time import monotonic as _time

try:
    import fcntl
except ImportError:
    fcntl = None

__all__ = ["Popen", "PIPE", "STDOUT", "call", "check_call", "getstatusoutput",
           "getoutput", "check_output", "run", "CalledProcessError", "DEVNULL",
           "SubprocessError", "TimeoutExpired", "CompletedProcess"]

_mswindows = sys.platform == "win32"
_can_fork_exec = sys.platform not in {"emscripten", "wasi", "ios", "tvos",
                                      "watchos"}

if _can_fork_exec:
    try:
        from _posixsubprocess import fork_exec as _fork_exec
    except ImportError:
        _fork_exec = None
else:
    _fork_exec = None
# fork_exec() decides itself how to create the child; the flag is kept for
# code that inspects it.
_USE_VFORK = _fork_exec is not None

import select
import selectors

PIPE = -1
STDOUT = -2
DEVNULL = -3

_PIPE_BUF = getattr(select, 'PIPE_BUF', 512)
_PopenSelector = getattr(selectors, 'PollSelector', selectors.SelectSelector)


def _probe_pidfd():
    if not hasattr(os, 'pidfd_open') or not hasattr(select, 'poll'):
        return False
    try:
        fd = os.pidfd_open(os.getpid())
    except OSError:
        return False
    os.close(fd)
    return True


_CAN_USE_PIDFD_OPEN = _probe_pidfd()
# Longest single kernel wait for a child (seconds); longer timeouts are
# waited for in slices of this size.
_MAXIMUM_WAIT_TIMEOUT = 86400.0
_CAN_USE_KQUEUE = (hasattr(select, 'kqueue')
                   and hasattr(select, 'KQ_FILTER_PROC'))

# posix_spawn() is not used: fork_exec() reports every failure precisely.
_USE_POSIX_SPAWN = False
_HAVE_POSIX_SPAWN_CLOSEFROM = hasattr(os, 'POSIX_SPAWN_CLOSEFROM')


# ---------------------------------------------------------------- errors

class SubprocessError(Exception):
    pass


class _OutputAlias:
    """Mixin: 'stdout' is another name for the 'output' attribute."""

    @property
    def stdout(self):
        """Alias for output attribute, to match stderr"""
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class CalledProcessError(_OutputAlias, SubprocessError):
    """A checked command exited with a non-zero status.

    Attributes: returncode, cmd, output (= stdout), stderr.
    """

    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode
        self.cmd = cmd
        self.output = output
        self.stderr = stderr

    def __str__(self):
        code = self.returncode
        if not isinstance(code, int):
            # e.g. a mocked Popen: describe whatever value is there
            return "Command '%s' returned non-zero exit status %s." % (
                self.cmd, code)
        if code < 0:
            try:
                return "Command '%s' died with %r." % (self.cmd,
                                                       signal.Signals(-code))
            except ValueError:
                return "Command '%s' died with unknown signal %d." % (self.cmd,
                                                                      -code)
        return "Command '%s' returned non-zero exit status %d." % (self.cmd,
                                                                   code)


class TimeoutExpired(_OutputAlias, SubprocessError):
    """Waiting for a child process timed out.

    Attributes: cmd, timeout, output (= stdout), stderr.
    """

    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr

    def __str__(self):
        return "Command '%s' timed out after %s seconds" % (self.cmd,
                                                            self.timeout)


# ---------------------------------------------------------------- bookkeeping

# Popen objects garbage-collected while their child was still running; they
# are reaped opportunistically by _cleanup() to avoid zombies.
_active = []


def _cleanup():
    if _active is None:
        return
    for proc in list(_active):
        if proc._internal_poll(_deadstate=sys.maxsize) is not None:
            try:
                _active.remove(proc)
            except ValueError:
                pass  # removed concurrently


def _optim_args_from_interpreter_flags():
    """-O options matching sys.flags.optimize."""
    level = sys.flags.optimize
    return ['-' + 'O' * level] if level > 0 else []


def _args_from_interpreter_flags():
    """Command line options reproducing this interpreter's settings."""
    flags = sys.flags
    args = _optim_args_from_interpreter_flags()
    for attr, letter in (('debug', 'd'), ('dont_write_bytecode', 'B'),
                         ('no_site', 'S'), ('verbose', 'v'),
                         ('bytes_warning', 'b'), ('quiet', 'q')):
        count = getattr(flags, attr)
        if count > 0:
            args.append('-' + letter * count)
    if flags.isolated:
        args.append('-I')
    else:
        for attr, opt in (('ignore_environment', '-E'),
                          ('no_user_site', '-s'), ('safe_path', '-P')):
            if getattr(flags, attr):
                args.append(opt)

    # -W options, minus those implied by -b/-bb and -X dev
    implied = set()
    if flags.bytes_warning > 1:
        implied.add("error::BytesWarning")
    elif flags.bytes_warning:
        implied.add("default::BytesWarning")
    if flags.dev_mode:
        implied.add('default')
    for opt in sys.warnoptions:
        if opt in implied:
            implied.discard(opt)
            continue
        args.append('-W' + opt)

    xoptions = getattr(sys, '_xoptions', {})
    if flags.dev_mode:
        args += ['-X', 'dev']
    for name in ('faulthandler', 'tracemalloc', 'importtime',
                 'frozen_modules', 'showrefcount', 'utf8', 'gil'):
        if name in xoptions:
            value = xoptions[name]
            args += ['-X', name if value is True else f'{name}={value}']
    return args


def _text_encoding():
    # Same default as open() without encoding (EncodingWarning aware).
    if sys.flags.warn_default_encoding:
        frame = sys._getframe()
        here = frame.f_code.co_filename
        level = 2
        while (frame := frame.f_back) is not None:
            if frame.f_code.co_filename != here:
                break
            level += 1
        warnings.warn("'encoding' argument not specified.",
                      EncodingWarning, level)
    if sys.flags.utf8_mode:
        return "utf-8"
    return locale.getencoding()


# ---------------------------------------------------------------- functions

def call(*popenargs, timeout=None, **kwargs):
    """Run a command and return its exit status (Popen arguments)."""
    with Popen(*popenargs, **kwargs) as proc:
        try:
            return proc.wait(timeout=timeout)
        except:
            proc.kill()   # __exit__ then reaps it
            raise


def check_call(*popenargs, **kwargs):
    """Run a command; raise CalledProcessError if it fails, else return 0."""
    status = call(*popenargs, **kwargs)
    if status:
        cmd = kwargs.get("args")
        raise CalledProcessError(status, popenargs[0] if cmd is None else cmd)
    return 0


def check_output(*popenargs, timeout=None, **kwargs):
    """Run a command and return its standard output.

    CalledProcessError is raised on a non-zero exit status.  Takes the
    arguments of run() except stdout and check.
    """
    for forbidden in ('stdout', 'check'):
        if forbidden in kwargs:
            raise ValueError(f'{forbidden} argument not allowed, it will be '
                             f'overridden.')
    if 'input' in kwargs and kwargs['input'] is None:
        # explicit input=None means empty input
        textual = any(kwargs.get(k) for k in
                      ('universal_newlines', 'text', 'encoding', 'errors'))
        kwargs['input'] = '' if textual else b''
    return run(*popenargs, stdout=PIPE, timeout=timeout, check=True,
               **kwargs).stdout


class CompletedProcess(object):
    """Outcome of run(): args, returncode, stdout, stderr."""

    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        fields = [f'args={self.args!r}', f'returncode={self.returncode!r}']
        for name in ('stdout', 'stderr'):
            value = getattr(self, name)
            if value is not None:
                fields.append(f'{name}={value!r}')
        return f"{type(self).__name__}({', '.join(fields)})"

    __class_getitem__ = classmethod(types.GenericAlias)

    def check_returncode(self):
        """Raise CalledProcessError if returncode is non-zero."""
        if self.returncode:
            raise CalledProcessError(self.returncode, self.args,
                                     self.stdout, self.stderr)


def run(*popenargs, input=None, capture_output=False, timeout=None,
        check=False, **kwargs):
    """Run a command, wait for it, and return a CompletedProcess.

    input: data for stdin; capture_output: capture stdout and stderr;
    timeout: kill the child and raise TimeoutExpired; check: raise
    CalledProcessError on failure.  Other arguments go to Popen.
    """
    if input is not None:
        if kwargs.get('stdin') is not None:
            raise ValueError('stdin and input arguments may not both be used.')
        kwargs['stdin'] = PIPE
    if capture_output:
        if kwargs.get('stdout') is not None or kwargs.get('stderr') is not None:
            raise ValueError('stdout and stderr arguments may not be used '
                             'with capture_output.')
        kwargs['stdout'] = kwargs['stderr'] = PIPE

    with Popen(*popenargs, **kwargs) as proc:
        try:
            out, err = proc.communicate(input, timeout=timeout)
        except TimeoutExpired:
            proc.kill()
            proc.wait()   # the partial output is attached to the exception
            raise
        except:
            proc.kill()
            raise
        status = proc.poll()
        if check and status:
            raise CalledProcessError(status, proc.args, output=out, stderr=err)
    return CompletedProcess(proc.args, status, out, err)


def list2cmdline(seq):
    """Quote a sequence of arguments the way the MS C runtime parses them."""
    pieces = []
    for arg in map(os.fsdecode, seq):
        quote = not arg or ' ' in arg or '\t' in arg
        out = []
        slashes = 0
        for ch in arg:
            if ch == '\\':
                slashes += 1
                continue
            if ch == '"':
                out.append('\\' * (2 * slashes) + '\\"')
            else:
                out.append('\\' * slashes + ch)
            slashes = 0
        # trailing backslashes are doubled only before the closing quote
        out.append('\\' * (slashes * 2 if quote else slashes))
        body = ''.join(out)
        pieces.append(f'"{body}"' if quote else body)
    return ' '.join(pieces)


def getstatusoutput(cmd, *, encoding=None, errors=None):
    """Run *cmd* through the shell; return (exitcode, combined output)
    with one trailing newline stripped."""
    try:
        output = check_output(cmd, shell=True, text=True, stderr=STDOUT,
                              encoding=encoding, errors=errors)
        status = 0
    except CalledProcessError as exc:
        output, status = exc.output, exc.returncode
    if output.endswith('\n'):
        output = output[:-1]
    return status, output


def getoutput(cmd, *, encoding=None, errors=None):
    """Output of getstatusoutput(cmd) without the status."""
    return getstatusoutput(cmd, encoding=encoding, errors=errors)[1]


def _decode_child_error(data, cwd, executable):
    """Exception for the b"Name:hexerrno:message" report of a failed child."""
    try:
        name, code, message = data.split(b':', 2)
        message = message.decode()
    except ValueError:
        name, code = b'SubprocessError', b'0'
        message = 'Bad exception data from child: {!r}'.format(bytes(data))
    exc_type = getattr(builtins, name.decode('ascii'), SubprocessError)
    if not (isinstance(exc_type, type) and issubclass(exc_type, BaseException)):
        exc_type = SubprocessError
    if issubclass(exc_type, OSError) and code:
        number = int(code, 16)
        if message == "noexec:chdir":
            filename = cwd          # chdir(cwd) failed
        elif message == "noexec":
            filename = None         # failure before exec()
        else:
            filename = executable   # exec() itself failed
        text = os.strerror(number) if number else ""
        if filename is None:
            return exc_type(number, text)
        return exc_type(number, text, filename)
    return exc_type(message)


# Names Popen.__del__ and _internal_poll need even during interpreter
# shutdown, when module globals may already be gone.
_reap = types.SimpleNamespace(
    waitpid=getattr(os, 'waitpid', None),
    WNOHANG=getattr(os, 'WNOHANG', 0),
    ECHILD=errno.ECHILD,
    WIFSTOPPED=getattr(os, 'WIFSTOPPED', None),
    WSTOPSIG=getattr(os, 'WSTOPSIG', None),
    waitstatus_to_exitcode=getattr(os, 'waitstatus_to_exitcode', None),
)


def _as_id(value, lookup, kind):
    """Resolve a user/group given as name or number."""
    if isinstance(value, str):
        return lookup(value)
    if isinstance(value, int):
        if value < 0:
            raise ValueError(f"{kind} ID cannot be negative, got {value}")
        if value >= 1 << 32:
            raise ValueError(f"{kind} ID is too large: {value}")
        return value
    return None


class Popen:
    """Execute a program in a child process (see the module documentation
    for the arguments)."""

    _child_created = False

    def __init__(self, args, bufsize=-1, executable=None,
                 stdin=None, stdout=None, stderr=None,
                 preexec_fn=None, close_fds=True,
                 shell=False, cwd=None, env=None, universal_newlines=None,
                 startupinfo=None, creationflags=0,
                 restore_signals=True, start_new_session=False,
                 pass_fds=(), *, user=None, group=None, extra_groups=None,
                 encoding=None, errors=None, text=None, umask=-1, pipesize=-1,
                 process_group=None):
        _cleanup()
        if not _can_fork_exec:
            raise OSError(errno.ENOTSUP,
                          f"{sys.platform} does not support processes.")

        self._waitpid_lock = threading.Lock()
        self._input = None
        self._communication_started = False

        bufsize = -1 if bufsize is None else bufsize
        if not isinstance(bufsize, int):
            raise TypeError("bufsize must be an integer")
        if stdout is STDOUT:
            raise ValueError("STDOUT can only be used for stderr")
        pipesize = -1 if pipesize is None else pipesize
        if not isinstance(pipesize, int):
            raise TypeError("pipesize must be an integer")
        if pass_fds and not close_fds:
            warnings.warn("pass_fds overriding close_fds.", RuntimeWarning)
            close_fds = True
        if startupinfo is not None:
            raise ValueError("startupinfo is only supported on Windows "
                             "platforms")
        if creationflags != 0:
            raise ValueError("creationflags is only supported on Windows "
                             "platforms")

        self.args = args
        self.stdin = self.stdout = self.stderr = None
        self.pid = None
        self.returncode = None
        self.encoding = encoding
        self.errors = errors
        self.pipesize = pipesize

        if (text is not None and universal_newlines is not None
                and bool(universal_newlines) != bool(text)):
            raise SubprocessError('Cannot disambiguate when both text '
                                  'and universal_newlines are supplied but '
                                  'different. Pass one or the other.')
        self.text_mode = encoding or errors or text or universal_newlines
        if self.text_mode and encoding is None:
            self.encoding = encoding = _text_encoding()

        # grace period granted to the child after a KeyboardInterrupt
        self._sigint_wait_secs = 0.25

        line_buffering = False
        if bufsize == 1:
            if self.text_mode:
                line_buffering, bufsize = True, -1
            else:
                warnings.warn("line buffering (buffering=1) isn't supported "
                              "in binary mode, the default buffer size will "
                              "be used", RuntimeWarning)

        if process_group is None:
            process_group = -1
        gid, gids, uid = self._resolve_ids(user, group, extra_groups)

        fds = self._get_handles(stdin, stdout, stderr)
        p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite = fds
        if p2cwrite != -1:
            self.stdin = io.open(p2cwrite, 'wb', bufsize)
            if self.text_mode:
                self.stdin = io.TextIOWrapper(
                    self.stdin, write_through=True,
                    line_buffering=line_buffering,
                    encoding=encoding, errors=errors)
        for attr, fd in (('stdout', c2pread), ('stderr', errread)):
            if fd != -1:
                stream = io.open(fd, 'rb', bufsize)
                if self.text_mode:
                    stream = io.TextIOWrapper(stream, encoding=encoding,
                                              errors=errors)
                setattr(self, attr, stream)

        self._closed_child_pipe_fds = False
        try:
            self._execute_child(args, executable, preexec_fn, close_fds,
                                pass_fds, cwd, env,
                                startupinfo, creationflags, shell,
                                p2cread, p2cwrite, c2pread, c2pwrite,
                                errread, errwrite, restore_signals,
                                gid, gids, uid, umask,
                                start_new_session, process_group)
        except:
            self._abandon_streams()
            if not self._closed_child_pipe_fds:
                leftovers = [fd for wanted, fd in ((stdin, p2cread),
                                                   (stdout, c2pwrite),
                                                   (stderr, errwrite))
                             if wanted == PIPE]
                if hasattr(self, '_devnull'):
                    leftovers.append(self._devnull)
                for fd in leftovers:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
            raise

    def _abandon_streams(self):
        for stream in (self.stdin, self.stdout, self.stderr):
            if stream is not None:
                try:
                    stream.close()
                except OSError:
                    pass

    @staticmethod
    def _resolve_ids(user, group, extra_groups):
        def by_group_name(name):
            try:
                import grp
            except ImportError:
                raise ValueError("The group parameter cannot be a string "
                                 "on systems without the grp module")
            return grp.getgrnam(name).gr_gid

        gid = gids = uid = None
        if group is not None:
            if not hasattr(os, 'setregid'):
                raise ValueError("The 'group' parameter is not supported on "
                                 "the current platform")
            gid = _as_id(group, by_group_name, "Group")
            if gid is None:
                raise TypeError("Group must be a string or an integer, not {}"
                                .format(type(group)))
        if extra_groups is not None:
            if not hasattr(os, 'setgroups'):
                raise ValueError("The 'extra_groups' parameter is not "
                                 "supported on the current platform")
            if isinstance(extra_groups, str):
                raise ValueError("Groups must be a list, not a string")
            gids = []
            for item in extra_groups:
                value = _as_id(item, by_group_name, "Group")
                if value is None:
                    raise TypeError("Items in extra_groups must be a string "
                                    "or integer, not {}".format(type(item)))
                gids.append(value)
        if user is not None:
            if not hasattr(os, 'setreuid'):
                raise ValueError("The 'user' parameter is not supported on "
                                 "the current platform")

            def by_user_name(name):
                try:
                    import pwd
                except ImportError:
                    raise ValueError("The user parameter cannot be a string "
                                     "on systems without the pwd module")
                return pwd.getpwnam(name).pw_uid

            uid = _as_id(user, by_user_name, "User")
            if uid is None:
                raise TypeError("User must be a string or an integer")
        return gid, gids, uid

    def __repr__(self):
        text = (f"<{self.__class__.__name__}: returncode: {self.returncode} "
                f"args: {self.args!r}>")
        return text if len(text) <= 80 else text[:76] + "...>"

    __class_getitem__ = classmethod(types.GenericAlias)

    @property
    def universal_newlines(self):
        return self.text_mode

    @universal_newlines.setter
    def universal_newlines(self, universal_newlines):
        self.text_mode = bool(universal_newlines)

    def _translate_newlines(self, data, encoding, errors):
        text = data.decode(encoding, errors)
        return text.replace("\r\n", "\n").replace("\r", "\n")

    def __enter__(self):
        return self

    def __exit__(self, exc_type, value, traceback):
        for stream in (self.stdout, self.stderr):
            if stream:
                stream.close()
        try:
            if self.stdin:
                self.stdin.close()
        finally:
            if exc_type is KeyboardInterrupt:
                # The child probably got SIGINT too; give it a short grace
                # period unless an interrupted wait() already did.
                if self._sigint_wait_secs > 0:
                    try:
                        self._wait(timeout=self._sigint_wait_secs)
                    except TimeoutExpired:
                        pass
                self._sigint_wait_secs = 0
            else:
                self.wait()

    def __del__(self, _maxsize=sys.maxsize, _warn=warnings.warn):
        if not self._child_created:
            return
        if self.returncode is None:
            _warn("subprocess %s is still running" % self.pid,
                  ResourceWarning, source=self)
        self._internal_poll(_deadstate=_maxsize)
        if self.returncode is None and _active is not None:
            _active.append(self)   # reaped later by _cleanup()

    # ------------------------------------------------------------ pipes

    def _get_devnull(self):
        if not hasattr(self, '_devnull'):
            self._devnull = os.open(os.devnull, os.O_RDWR)
        return self._devnull

    def _new_pipe(self):
        r, w = os.pipe()
        if self.pipesize > 0 and fcntl is not None and \
                hasattr(fcntl, "F_SETPIPE_SZ"):
            fcntl.fcntl(w, fcntl.F_SETPIPE_SZ, self.pipesize)
        return r, w

    def _target_fd(self, spec):
        if spec == DEVNULL:
            return self._get_devnull()
        if isinstance(spec, int):
            return spec
        return spec.fileno()

    def _get_handles(self, stdin, stdout, stderr):
        """Descriptors (p2cread, p2cwrite, c2pread, c2pwrite, errread,
        errwrite); -1 where nothing is redirected."""
        p2cread = p2cwrite = c2pread = c2pwrite = errread = errwrite = -1
        if stdin == PIPE:
            p2cread, p2cwrite = self._new_pipe()
        elif stdin is not None:
            p2cread = self._target_fd(stdin)
        if stdout == PIPE:
            c2pread, c2pwrite = self._new_pipe()
        elif stdout is not None:
            c2pwrite = self._target_fd(stdout)
        if stderr == PIPE:
            errread, errwrite = self._new_pipe()
        elif stderr == STDOUT:
            errwrite = c2pwrite if c2pwrite != -1 else sys.__stdout__.fileno()
        elif stderr is not None:
            errwrite = self._target_fd(stderr)
        return p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite

    def _close_pipe_fds(self, p2cread, p2cwrite, c2pread, c2pwrite,
                        errread, errwrite):
        # the parent no longer needs the child's ends
        devnull = getattr(self, '_devnull', None)
        pending = [child for child, parent in ((p2cread, p2cwrite),
                                               (c2pwrite, c2pread),
                                               (errwrite, errread))
                   if child != -1 and parent != -1 and child != devnull]
        if devnull is not None:
            pending.append(devnull)
        with contextlib.ExitStack() as stack:
            for fd in pending:
                stack.callback(os.close, fd)
        self._closed_child_pipe_fds = True

    # ------------------------------------------------------------ spawning

    def _execute_child(self, args, executable, preexec_fn, close_fds,
                       pass_fds, cwd, env,
                       startupinfo, creationflags, shell,
                       p2cread, p2cwrite, c2pread, c2pwrite,
                       errread, errwrite, restore_signals,
                       gid, gids, uid, umask,
                       start_new_session, process_group):
        """Start the child process (POSIX)."""
        if isinstance(args, (str, bytes)):
            args = [args]
        elif isinstance(args, os.PathLike):
            if shell:
                raise TypeError('path-like args is not allowed when '
                                'shell is true')
            args = [args]
        else:
            args = list(args)
        if shell:
            sh = ('/system/bin/sh' if hasattr(sys, 'getandroidapilevel')
                  else '/bin/sh')
            args = [executable or sh, "-c", *args]
        if executable is None:
            executable = args[0]

        sys.audit("subprocess.Popen", executable, args, cwd, env)

        if _fork_exec is None:
            raise OSError(errno.ENOTSUP, "fork_exec() is not available")

        # Everything is converted before forking: the child must not
        # allocate or run Python code (except preexec_fn).
        env_list = None
        if env is not None:
            env_list = []
            for key, value in env.items():
                key = os.fsencode(key)
                if b'=' in key:
                    raise ValueError("illegal environment variable name")
                env_list.append(key + b'=' + os.fsencode(value))
        program = os.fsencode(executable)
        if os.path.dirname(program):
            candidates = (program,)
        else:
            candidates = tuple(os.path.join(os.fsencode(d), program)
                               for d in os.get_exec_path(env))

        report_r, report_w = os.pipe()
        # keep the report pipe above 0-2, which the child overwrites
        spares = []
        while report_w < 3:
            spares.append(report_w)
            report_w = os.dup(report_w)
        for fd in spares:
            os.close(fd)

        try:
            try:
                keep = sorted({*map(int, pass_fds), report_w})
                self.pid = _fork_exec(
                    args, candidates, close_fds, tuple(keep), cwd, env_list,
                    p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite,
                    report_r, report_w, restore_signals, start_new_session,
                    process_group, gid, gids, uid, umask, preexec_fn)
                self._child_created = True
            finally:
                os.close(report_w)
            self._close_pipe_fds(p2cread, p2cwrite, c2pread, c2pwrite,
                                 errread, errwrite)
            # The report pipe is close-on-exec: EOF without data means the
            # exec succeeded.
            report = bytearray()
            while len(report) <= 50000:
                chunk = os.read(report_r, 50000)
                if not chunk:
                    break
                report += chunk
        finally:
            os.close(report_r)

        if report:
            try:
                pid, status = os.waitpid(self.pid, 0)
            except ChildProcessError:
                pass
            else:
                if pid == self.pid:
                    self._handle_exitstatus(status)
                else:
                    self.returncode = sys.maxsize
            raise _decode_child_error(report, cwd, executable)

    # ------------------------------------------------------------ waiting

    def _handle_exitstatus(self, sts, _reap=_reap):
        if _reap.WIFSTOPPED(sts):
            self.returncode = -_reap.WSTOPSIG(sts)
        else:
            self.returncode = _reap.waitstatus_to_exitcode(sts)

    def _internal_poll(self, _deadstate=None, _reap=_reap):
        """Non-blocking reap attempt (usable from __del__ at shutdown)."""
        if self.returncode is not None:
            return self.returncode
        if not self._waitpid_lock.acquire(False):
            return None   # someone else is waiting on the child right now
        try:
            if self.returncode is None:
                pid, sts = _reap.waitpid(self.pid, _reap.WNOHANG)
                if pid == self.pid:
                    self._handle_exitstatus(sts)
        except OSError as exc:
            if _deadstate is not None:
                self.returncode = _deadstate
            elif exc.errno == _reap.ECHILD:
                # reaped behind our back (e.g. SIGCHLD ignored): status lost
                self.returncode = 0
        finally:
            self._waitpid_lock.release()
        return self.returncode

    def poll(self):
        """Return the returncode if the child has exited, else None."""
        return self._internal_poll()

    def _try_wait(self, wait_flags):
        try:
            return os.waitpid(self.pid, wait_flags)
        except ChildProcessError:
            return self.pid, 0   # already reaped elsewhere

    def _reap_nowait(self):
        """One WNOHANG reap attempt under the lock; True when done."""
        if not self._waitpid_lock.acquire(False):
            return False
        try:
            if self.returncode is not None:
                return True
            pid, sts = self._try_wait(os.WNOHANG)
            if pid == self.pid:
                self._handle_exitstatus(sts)
                return True
            return False
        finally:
            self._waitpid_lock.release()

    def _wait_pidfd(self, timeout):
        """Block until the child exits or *timeout* expires, using a pidfd.

        Returns True if the child (probably) exited, False on timeout.
        Raises OSError if pidfds cannot be used.
        """
        pidfd = os.pidfd_open(self.pid)
        try:
            poller = select.poll()
            poller.register(pidfd, select.POLLIN)
            # poll() takes an int of milliseconds: clamp huge timeouts
            ms = min(max(timeout, 0) * 1000, 2 ** 31 - 1)
            return bool(poller.poll(int(-(-ms // 1))))
        finally:
            os.close(pidfd)

    def _wait_kqueue(self, timeout):
        """Like _wait_pidfd(), with a kqueue process filter (BSD)."""
        kq = select.kqueue()
        try:
            ev = select.kevent(self.pid, filter=select.KQ_FILTER_PROC,
                               flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                               fflags=select.KQ_NOTE_EXIT)
            timeout = min(max(timeout, 0), 1e8)
            return bool(kq.control([ev], 1, timeout))
        finally:
            kq.close()

    def _wait(self, timeout):
        """wait() without KeyboardInterrupt handling."""
        if self.returncode is not None:
            return self.returncode
        if timeout is None:
            while self.returncode is None:
                with self._waitpid_lock:
                    if self.returncode is not None:
                        break
                    pid, sts = self._try_wait(0)
                    # waitpid() may spuriously return another value: retry
                    if pid == self.pid:
                        self._handle_exitstatus(sts)
            return self.returncode

        deadline = _time() + timeout
        if self._reap_nowait():
            return self.returncode
        # Fast path: sleep in the kernel until the child exits.
        fast = None
        if _CAN_USE_PIDFD_OPEN:
            fast = self._wait_pidfd
        elif _CAN_USE_KQUEUE:
            fast = self._wait_kqueue
        if fast is not None:
            try:
                # one kernel wait is at most _MAXIMUM_WAIT_TIMEOUT long; only
                # the real deadline ends the wait
                while True:
                    remaining = deadline - _time()
                    if fast(max(min(remaining, _MAXIMUM_WAIT_TIMEOUT), 0)):
                        break
                    if deadline - _time() <= 0:
                        raise TimeoutExpired(self.args, timeout)
            except OSError:
                pass   # fall back to polling below
            else:
                if self._reap_nowait():
                    return self.returncode
        # Polling with an increasing delay (capped at 50 ms).
        delay = 0.0005
        while not self._reap_nowait():
            remaining = deadline - _time()
            if remaining <= 0:
                raise TimeoutExpired(self.args, timeout)
            delay = min(delay * 2, remaining, 0.05)
            time.sleep(delay)
        return self.returncode

    def _remaining_time(self, endtime):
        return None if endtime is None else endtime - _time()

    def _interrupted_wait(self, endtime):
        # After Ctrl-C give the child a short chance to exit, then re-raise.
        grace = self._sigint_wait_secs
        if endtime is not None:
            grace = min(grace, self._remaining_time(endtime))
        self._sigint_wait_secs = 0
        try:
            self._wait(timeout=grace)
        except TimeoutExpired:
            pass

    def wait(self, timeout=None):
        """Wait for the child to exit and return its returncode."""
        endtime = None if timeout is None else _time() + timeout
        try:
            return self._wait(timeout=timeout)
        except KeyboardInterrupt:
            self._interrupted_wait(endtime)
            raise

    # ------------------------------------------------------------ communicate

    def _stdin_write(self, input):
        def tolerant(action):
            try:
                action()
            except BrokenPipeError:
                pass   # the child stopped reading
            except OSError as exc:
                if exc.errno != errno.EINVAL:
                    raise
        if input:
            tolerant(lambda: self.stdin.write(input))
        tolerant(self.stdin.close)

    def communicate(self, input=None, timeout=None):
        """Feed *input* to stdin, collect stdout/stderr until EOF and wait
        for the child.  Returns (stdout_data, stderr_data).

        On timeout TimeoutExpired is raised; the child keeps running and
        communicate() can be called again.
        """
        if self._communication_started and input:
            raise ValueError("Cannot send input after starting communication")

        streams = [s for s in (self.stdin, self.stdout, self.stderr) if s]
        if timeout is None and not self._communication_started \
                and len(streams) <= 1:
            # a single (or no) pipe: plain blocking I/O is enough
            out = err = None
            if self.stdin:
                self._stdin_write(input)
            elif self.stdout:
                out = self.stdout.read()
                self.stdout.close()
            elif self.stderr:
                err = self.stderr.read()
                self.stderr.close()
            self.wait()
            return out, err

        endtime = None if timeout is None else _time() + timeout
        try:
            out, err = self._communicate(input, endtime, timeout)
        except KeyboardInterrupt:
            self._interrupted_wait(endtime)
            raise
        finally:
            self._communication_started = True
        self.wait(timeout=self._remaining_time(endtime))
        return out, err

    def _save_input(self, input):
        # The encoded input and the write offset survive a timeout so that
        # a later communicate() resumes where the previous one stopped.
        if self.stdin and self._input is None:
            self._input_offset = 0
            self._input = input
            if input is not None and self.text_mode:
                self._input = input.encode(self.stdin.encoding,
                                           self.stdin.errors)
            elif input is not None and not isinstance(input, bytes):
                # offsets count bytes, whatever the buffer's item size
                try:
                    self._input = memoryview(input).cast('B')
                except TypeError:
                    self._input = bytes(memoryview(input))

    def _check_timeout(self, endtime, orig_timeout, stdout_seq, stderr_seq,
                       skip_check_and_raise=False):
        if endtime is None:
            return
        if skip_check_and_raise or _time() > endtime:
            raise TimeoutExpired(
                self.args, orig_timeout,
                output=b''.join(stdout_seq) if stdout_seq else None,
                stderr=b''.join(stderr_seq) if stderr_seq else None)

    def _communicate(self, input, endtime, orig_timeout):
        if self.stdin and not self._communication_started:
            try:
                self.stdin.flush()
            except BrokenPipeError:
                pass
            except ValueError:
                if not self.stdin.closed:
                    raise
            if not input:
                try:
                    self.stdin.close()
                except BrokenPipeError:
                    pass

        if not self._communication_started:
            self._chunks = {s: [] for s in (self.stdout, self.stderr) if s}
        out_chunks = self._chunks.get(self.stdout) if self.stdout else None
        err_chunks = self._chunks.get(self.stderr) if self.stderr else None

        self._save_input(input)
        pending = memoryview(self._input) if self._input else None

        with _PopenSelector() as sel:
            # a call resumed after a timeout passes no input but still owes
            # the child the rest of the earlier input (and the EOF after it)
            if self.stdin and self._input and not self.stdin.closed:
                if self._input_offset < len(self._input):
                    sel.register(self.stdin, selectors.EVENT_WRITE)
                else:
                    try:
                        self.stdin.close()
                    except BrokenPipeError:
                        pass
            for stream in (self.stdout, self.stderr):
                if stream and not stream.closed:
                    sel.register(stream, selectors.EVENT_READ)

            while sel.get_map():
                remaining = self._remaining_time(endtime)
                if remaining is not None and remaining < 0:
                    self._check_timeout(endtime, orig_timeout, out_chunks,
                                        err_chunks, skip_check_and_raise=True)
                ready = sel.select(remaining)
                self._check_timeout(endtime, orig_timeout, out_chunks,
                                    err_chunks)
                for key, _ in ready:
                    stream = key.fileobj
                    if stream is self.stdin:
                        piece = pending[self._input_offset:
                                        self._input_offset + _PIPE_BUF]
                        try:
                            self._input_offset += os.write(key.fd, piece)
                        except BrokenPipeError:
                            done = True
                        else:
                            done = self._input_offset >= len(self._input)
                        if done:
                            sel.unregister(stream)
                            stream.close()
                    else:
                        data = os.read(key.fd, 32768)
                        if not data:
                            sel.unregister(stream)
                            stream.close()
                        self._chunks[stream].append(data)

        try:
            self.wait(timeout=self._remaining_time(endtime))
        except TimeoutExpired as exc:
            exc.timeout = orig_timeout
            raise

        out = b''.join(out_chunks) if out_chunks is not None else None
        err = b''.join(err_chunks) if err_chunks is not None else None
        if self.text_mode:
            if out is not None:
                out = self._translate_newlines(out, self.stdout.encoding,
                                               self.stdout.errors)
            if err is not None:
                err = self._translate_newlines(err, self.stderr.encoding,
                                               self.stderr.errors)
        return out, err

    # ------------------------------------------------------------ signals

    def send_signal(self, sig):
        """Send *sig* to the child unless it is already known to be dead."""
        # reap first so that a recycled pid is never signalled
        self.poll()
        if self.returncode is not None:
            return
        try:
            os.kill(self.pid, sig)
        except ProcessLookupError:
            pass

    def terminate(self):
        """Terminate the child (SIGTERM)."""
        self.send_signal(signal.SIGTERM)

    def kill(self):
        """Kill the child (SIGKILL)."""
        self.send_signal(signal.SIGKILL)
