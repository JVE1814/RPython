"""Subprocesses with accessible I/O streams.

Spawn new processes, connect to their input/output/error pipes and obtain
their return codes.  Main entry points: run(), Popen, call(), check_call(),
check_output(), getstatusoutput() and getoutput().
"""

import builtins
import errno
import io
import locale
import os
import time
import signal
import sys
import threading
import warnings
import contextlib
from time import monotonic as _time
import types

try:
    import fcntl
except ImportError:
    fcntl = None


__all__ = ["Popen", "PIPE", "STDOUT", "call", "check_call", "getstatusoutput",
           "getoutput", "check_output", "run", "CalledProcessError", "DEVNULL",
           "SubprocessError", "TimeoutExpired", "CompletedProcess"]

_mswindows = (sys.platform == "win32")
_can_fork_exec = sys.platform not in {"emscripten", "wasi", "ios", "tvos", "watchos"}

if _mswindows:
    raise ImportError("subprocess on Windows is not supported by this implementation")

if _can_fork_exec:
    from _posixsubprocess import fork_exec as _fork_exec
    _waitpid = os.waitpid
    _waitstatus_to_exitcode = os.waitstatus_to_exitcode
    _WIFSTOPPED = os.WIFSTOPPED
    _WSTOPSIG = os.WSTOPSIG
    _WNOHANG = os.WNOHANG
else:
    _fork_exec = None
    _waitpid = None
    _waitstatus_to_exitcode = None
    _WIFSTOPPED = None
    _WSTOPSIG = None
    _WNOHANG = None

import select
import selectors


class SubprocessError(Exception): pass


class CalledProcessError(SubprocessError):
    """Raised when run() is called with check=True and the process
    returns a non-zero exit status.

    Attributes:
      cmd, returncode, stdout, stderr, output
    """
    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode
        self.cmd = cmd
        self.output = output
        self.stderr = stderr

    def __str__(self):
        if self.returncode and self.returncode < 0:
            try:
                return "Command '%s' died with %r." % (
                        self.cmd, signal.Signals(-self.returncode))
            except ValueError:
                return "Command '%s' died with unknown signal %d." % (
                        self.cmd, -self.returncode)
        else:
            return "Command '%s' returned non-zero exit status %d." % (
                    self.cmd, self.returncode)

    @property
    def stdout(self):
        """Alias for output attribute, to match stderr"""
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class TimeoutExpired(SubprocessError):
    """Raised when the timeout expires while waiting for a child process.

    Attributes:
        cmd, output, stdout, stderr, timeout
    """
    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr

    def __str__(self):
        return ("Command '%s' timed out after %s seconds" %
                (self.cmd, self.timeout))

    @property
    def stdout(self):
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


_PIPE_BUF = getattr(select, 'PIPE_BUF', 512)

if hasattr(selectors, 'PollSelector'):
    _PopenSelector = selectors.PollSelector
else:
    _PopenSelector = selectors.SelectSelector


_active = []

def _cleanup():
    if _active is None:
        return
    for inst in _active[:]:
        res = inst._internal_poll(_deadstate=sys.maxsize)
        if res is not None:
            try:
                _active.remove(inst)
            except ValueError:
                pass

PIPE = -1
STDOUT = -2
DEVNULL = -3


def _optim_args_from_interpreter_flags():
    """Command-line arguments reproducing the current optimization level
    (sys.flags.optimize)."""
    args = []
    value = sys.flags.optimize
    if value > 0:
        args.append('-' + 'O' * value)
    return args


def _args_from_interpreter_flags():
    """Command-line arguments reproducing the current sys.flags,
    sys.warnoptions and sys._xoptions settings."""
    flag_opt_map = {
        'debug': 'd',
        'dont_write_bytecode': 'B',
        'no_site': 'S',
        'verbose': 'v',
        'bytes_warning': 'b',
        'quiet': 'q',
    }
    args = _optim_args_from_interpreter_flags()
    for flag, opt in flag_opt_map.items():
        v = getattr(sys.flags, flag)
        if v > 0:
            args.append('-' + opt * v)

    if sys.flags.isolated:
        args.append('-I')
    else:
        if sys.flags.ignore_environment:
            args.append('-E')
        if sys.flags.no_user_site:
            args.append('-s')
        if sys.flags.safe_path:
            args.append('-P')

    warnopts = sys.warnoptions[:]
    xoptions = getattr(sys, '_xoptions', {})
    bytes_warning = sys.flags.bytes_warning
    dev_mode = sys.flags.dev_mode

    if bytes_warning > 1:
        warnopts.remove("error::BytesWarning")
    elif bytes_warning:
        warnopts.remove("default::BytesWarning")
    if dev_mode:
        warnopts.remove('default')
    for opt in warnopts:
        args.append('-W' + opt)

    if dev_mode:
        args.extend(('-X', 'dev'))
    for opt in ('faulthandler', 'tracemalloc', 'importtime',
                'frozen_modules', 'showrefcount', 'utf8', 'gil'):
        if opt in xoptions:
            value = xoptions[opt]
            if value is True:
                arg = opt
            else:
                arg = '%s=%s' % (opt, value)
            args.extend(('-X', arg))

    return args


def _text_encoding():
    if sys.flags.warn_default_encoding:
        f = sys._getframe()
        filename = f.f_code.co_filename
        stacklevel = 2
        while f := f.f_back:
            if f.f_code.co_filename != filename:
                break
            stacklevel += 1
        warnings.warn("'encoding' argument not specified.",
                      EncodingWarning, stacklevel)

    if sys.flags.utf8_mode:
        return "utf-8"
    else:
        return locale.getencoding()


def call(*popenargs, timeout=None, **kwargs):
    """Run a command, wait for it and return its return code.  Arguments
    are those of Popen()."""
    with Popen(*popenargs, **kwargs) as p:
        try:
            return p.wait(timeout=timeout)
        except:
            p.kill()
            raise


def check_call(*popenargs, **kwargs):
    """Like call(), but raise CalledProcessError for a non-zero return
    code."""
    retcode = call(*popenargs, **kwargs)
    if retcode:
        cmd = kwargs.get("args")
        if cmd is None:
            cmd = popenargs[0]
        raise CalledProcessError(retcode, cmd)
    return 0


def check_output(*popenargs, timeout=None, **kwargs):
    r"""Run a command and return its standard output; a non-zero return
    code raises CalledProcessError (with the output attached).  Arguments
    are those of run() (input, encoding, errors, text, ...)."""
    for kw in ('stdout', 'check'):
        if kw in kwargs:
            raise ValueError(f'{kw} argument not allowed, it will be overridden.')

    if 'input' in kwargs and kwargs['input'] is None:
        if kwargs.get('universal_newlines') or kwargs.get('text') or kwargs.get('encoding') \
                or kwargs.get('errors'):
            empty = ''
        else:
            empty = b''
        kwargs['input'] = empty

    return run(*popenargs, stdout=PIPE, timeout=timeout, check=True,
               **kwargs).stdout


class CompletedProcess(object):
    """A process that has finished running (returned by run()).

    Attributes: args, returncode, stdout, stderr (the latter two are None
    unless captured).
    """
    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        args = ['args={!r}'.format(self.args),
                'returncode={!r}'.format(self.returncode)]
        if self.stdout is not None:
            args.append('stdout={!r}'.format(self.stdout))
        if self.stderr is not None:
            args.append('stderr={!r}'.format(self.stderr))
        return "{}({})".format(type(self).__name__, ', '.join(args))

    __class_getitem__ = classmethod(types.GenericAlias)


    def check_returncode(self):
        """Raise CalledProcessError if the exit code is non-zero."""
        if self.returncode:
            raise CalledProcessError(self.returncode, self.args, self.stdout,
                                     self.stderr)


def run(*popenargs,
        input=None, capture_output=False, timeout=None, check=False, **kwargs):
    """Run a command with arguments and return a CompletedProcess.

    *input* is sent to stdin; *capture_output* captures stdout and stderr;
    *timeout* kills the child and raises TimeoutExpired; *check* raises
    CalledProcessError for a non-zero exit code.  Other arguments are
    passed to Popen().
    """
    if input is not None:
        if kwargs.get('stdin') is not None:
            raise ValueError('stdin and input arguments may not both be used.')
        kwargs['stdin'] = PIPE

    if capture_output:
        if kwargs.get('stdout') is not None or kwargs.get('stderr') is not None:
            raise ValueError('stdout and stderr arguments may not be used '
                             'with capture_output.')
        kwargs['stdout'] = PIPE
        kwargs['stderr'] = PIPE

    with Popen(*popenargs, **kwargs) as process:
        try:
            stdout, stderr = process.communicate(input, timeout=timeout)
        except TimeoutExpired as exc:
            process.kill()
            process.wait()
            raise
        except:
            process.kill()
            raise
        retcode = process.poll()
        if check and retcode:
            raise CalledProcessError(retcode, process.args,
                                     output=stdout, stderr=stderr)
    return CompletedProcess(process.args, retcode, stdout, stderr)


def list2cmdline(seq):
    """Translate a sequence of arguments into a command line string using
    the MS C runtime quoting rules."""
    result = []
    needquote = False
    for arg in map(os.fsdecode, seq):
        bs_buf = []

        if result:
            result.append(' ')

        needquote = (" " in arg) or ("\t" in arg) or not arg
        if needquote:
            result.append('"')

        for c in arg:
            if c == '\\':
                bs_buf.append(c)
            elif c == '"':
                result.append('\\' * len(bs_buf)*2)
                bs_buf = []
                result.append('\\"')
            else:
                if bs_buf:
                    result.extend(bs_buf)
                    bs_buf = []
                result.append(c)

        if bs_buf:
            result.extend(bs_buf)

        if needquote:
            result.extend(bs_buf)
            result.append('"')

    return ''.join(result)


def getstatusoutput(cmd, *, encoding=None, errors=None):
    """Return (exitcode, output) of executing *cmd* in a shell; stderr is
    merged into the output and a trailing newline is stripped."""
    try:
        data = check_output(cmd, shell=True, text=True, stderr=STDOUT,
                            encoding=encoding, errors=errors)
        exitcode = 0
    except CalledProcessError as ex:
        data = ex.output
        exitcode = ex.returncode
    if data[-1:] == '\n':
        data = data[:-1]
    return exitcode, data


def getoutput(cmd, *, encoding=None, errors=None):
    """Return the output (stdout and stderr) of executing *cmd* in a
    shell, like getstatusoutput() without the exit code."""
    return getstatusoutput(cmd, encoding=encoding, errors=errors)[1]


_USE_POSIX_SPAWN = False
_HAVE_POSIX_SPAWN_CLOSEFROM = False
_USE_VFORK = False


class Popen:
    """Execute a child program in a new process.

    See the subprocess module documentation for the meaning of the
    arguments.
    """
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
        """Create new Popen instance."""
        if not _can_fork_exec:
            raise OSError(
                errno.ENOTSUP, f"{sys.platform} does not support processes."
            )

        _cleanup()
        self._waitpid_lock = threading.Lock()

        self._input = None
        self._communication_started = False
        if bufsize is None:
            bufsize = -1
        if not isinstance(bufsize, int):
            raise TypeError("bufsize must be an integer")

        if stdout is STDOUT:
            raise ValueError("STDOUT can only be used for stderr")

        if pipesize is None:
            pipesize = -1
        if not isinstance(pipesize, int):
            raise TypeError("pipesize must be an integer")

        if startupinfo is not None:
            raise ValueError("startupinfo is only supported on Windows "
                             "platforms")
        if creationflags != 0:
            raise ValueError("creationflags is only supported on Windows "
                             "platforms")

        self.args = args
        self.stdin = None
        self.stdout = None
        self.stderr = None
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

        self._sigint_wait_secs = 0.25

        self._closed_child_pipe_fds = False

        if self.text_mode:
            if bufsize == 1:
                line_buffering = True
                bufsize = -1
            else:
                line_buffering = False

        if process_group is None:
            process_group = -1

        gid = None
        if group is not None:
            if not hasattr(os, 'setregid'):
                raise ValueError("The 'group' parameter is not supported on the "
                                 "current platform")

            elif isinstance(group, str):
                try:
                    import grp
                except ImportError:
                    raise ValueError("The group parameter cannot be a string "
                                     "on systems without the grp module")

                gid = grp.getgrnam(group).gr_gid
            elif isinstance(group, int):
                gid = group
            else:
                raise TypeError("Group must be a string or an integer, not {}"
                                .format(type(group)))

            if gid < 0:
                raise ValueError(f"Group ID cannot be negative, got {gid}")

        gids = None
        if extra_groups is not None:
            if not hasattr(os, 'setgroups'):
                raise ValueError("The 'extra_groups' parameter is not "
                                 "supported on the current platform")

            elif isinstance(extra_groups, str):
                raise ValueError("Groups must be a list, not a string")

            gids = []
            for extra_group in extra_groups:
                if isinstance(extra_group, str):
                    try:
                        import grp
                    except ImportError:
                        raise ValueError("Items in extra_groups cannot be "
                                         "strings on systems without the "
                                         "grp module")

                    gids.append(grp.getgrnam(extra_group).gr_gid)
                elif isinstance(extra_group, int):
                    gids.append(extra_group)
                else:
                    raise TypeError("Items in extra_groups must be a string "
                                    "or integer, not {}"
                                    .format(type(extra_group)))

            for gid_check in gids:
                if gid_check < 0:
                    raise ValueError(f"Group ID cannot be negative, got {gid_check}")

        uid = None
        if user is not None:
            if not hasattr(os, 'setreuid'):
                raise ValueError("The 'user' parameter is not supported on "
                                 "the current platform")

            elif isinstance(user, str):
                try:
                    import pwd
                except ImportError:
                    raise ValueError("The user parameter cannot be a string "
                                     "on systems without the pwd module")
                uid = pwd.getpwnam(user).pw_uid
            elif isinstance(user, int):
                uid = user
            else:
                raise TypeError("User must be a string or an integer")

            if uid < 0:
                raise ValueError(f"User ID cannot be negative, got {uid}")

        (p2cread, p2cwrite,
         c2pread, c2pwrite,
         errread, errwrite) = self._get_handles(stdin, stdout, stderr)

        try:
            if p2cwrite != -1:
                self.stdin = io.open(p2cwrite, 'wb', bufsize)
                if self.text_mode:
                    self.stdin = io.TextIOWrapper(self.stdin,
                            write_through=True,
                            line_buffering=line_buffering,
                            encoding=encoding, errors=errors)
            if c2pread != -1:
                self.stdout = io.open(c2pread, 'rb', bufsize)
                if self.text_mode:
                    self.stdout = io.TextIOWrapper(self.stdout,
                            encoding=encoding, errors=errors)
            if errread != -1:
                self.stderr = io.open(errread, 'rb', bufsize)
                if self.text_mode:
                    self.stderr = io.TextIOWrapper(self.stderr,
                            encoding=encoding, errors=errors)

            self._execute_child(args, executable, preexec_fn, close_fds,
                                pass_fds, cwd, env,
                                startupinfo, creationflags, shell,
                                p2cread, p2cwrite,
                                c2pread, c2pwrite,
                                errread, errwrite,
                                restore_signals,
                                gid, gids, uid, umask,
                                start_new_session, process_group)
        except:
            for f in filter(None, (self.stdin, self.stdout, self.stderr)):
                try:
                    f.close()
                except OSError:
                    pass

            if not self._closed_child_pipe_fds:
                to_close = []
                if stdin == PIPE:
                    to_close.append(p2cread)
                if stdout == PIPE:
                    to_close.append(c2pwrite)
                if stderr == PIPE:
                    to_close.append(errwrite)
                if hasattr(self, '_devnull'):
                    to_close.append(self._devnull)
                for fd in to_close:
                    try:
                        os.close(fd)
                    except OSError:
                        pass

            raise

    def __repr__(self):
        obj_repr = (
            f"<{self.__class__.__name__}: "
            f"returncode: {self.returncode} args: {self.args!r}>"
        )
        if len(obj_repr) > 80:
            obj_repr = obj_repr[:76] + "...>"
        return obj_repr

    __class_getitem__ = classmethod(types.GenericAlias)

    @property
    def universal_newlines(self):
        return self.text_mode

    @universal_newlines.setter
    def universal_newlines(self, universal_newlines):
        self.text_mode = bool(universal_newlines)

    def _translate_newlines(self, data, encoding, errors):
        data = data.decode(encoding, errors)
        return data.replace("\r\n", "\n").replace("\r", "\n")

    def __enter__(self):
        return self

    def __exit__(self, exc_type, value, traceback):
        if self.stdout:
            self.stdout.close()
        if self.stderr:
            self.stderr.close()
        try:
            if self.stdin:
                self.stdin.close()
        finally:
            if exc_type == KeyboardInterrupt:
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
            _active.append(self)

    def _get_devnull(self):
        if not hasattr(self, '_devnull'):
            self._devnull = os.open(os.devnull, os.O_RDWR)
        return self._devnull

    def _stdin_write(self, input):
        if input:
            try:
                self.stdin.write(input)
            except BrokenPipeError:
                pass
            except OSError as exc:
                if exc.errno == errno.EINVAL:
                    pass
                else:
                    raise

        try:
            self.stdin.close()
        except BrokenPipeError:
            pass
        except OSError as exc:
            if exc.errno == errno.EINVAL:
                pass
            else:
                raise

    def communicate(self, input=None, timeout=None):
        """Send *input* to stdin (then close it), read stdout/stderr until
        EOF and wait for the process; returns (stdout_data, stderr_data).
        On timeout TimeoutExpired is raised and the call may be retried."""

        if self._communication_started and input:
            raise ValueError("Cannot send input after starting communication")

        if (timeout is None and not self._communication_started and
            [self.stdin, self.stdout, self.stderr].count(None) >= 2):
            stdout = None
            stderr = None
            if self.stdin:
                self._stdin_write(input)
            elif self.stdout:
                stdout = self.stdout.read()
                self.stdout.close()
            elif self.stderr:
                stderr = self.stderr.read()
                self.stderr.close()
            self.wait()
        else:
            if timeout is not None:
                endtime = _time() + timeout
            else:
                endtime = None

            try:
                stdout, stderr = self._communicate(input, endtime, timeout)
            except KeyboardInterrupt:
                if timeout is not None:
                    sigint_timeout = min(self._sigint_wait_secs,
                                         self._remaining_time(endtime))
                else:
                    sigint_timeout = self._sigint_wait_secs
                self._sigint_wait_secs = 0
                try:
                    self._wait(timeout=sigint_timeout)
                except TimeoutExpired:
                    pass
                raise

            finally:
                self._communication_started = True
            try:
                sts = self.wait(timeout=self._remaining_time(endtime))
            except TimeoutExpired as exc:
                exc.timeout = timeout
                raise

        return (stdout, stderr)

    def poll(self):
        """Return the exit code if the child has terminated, else None."""
        return self._internal_poll()

    def _remaining_time(self, endtime):
        """Convenience for _communicate when computing timeouts."""
        if endtime is None:
            return None
        else:
            return endtime - _time()

    def _check_timeout(self, endtime, orig_timeout, stdout_seq, stderr_seq,
                       skip_check_and_raise=False):
        """Convenience for checking if a timeout has expired."""
        if endtime is None:
            return
        if skip_check_and_raise or _time() > endtime:
            raise TimeoutExpired(
                    self.args, orig_timeout,
                    output=b''.join(stdout_seq) if stdout_seq else None,
                    stderr=b''.join(stderr_seq) if stderr_seq else None)

    def wait(self, timeout=None):
        """Wait for child process to terminate; returns self.returncode."""
        if timeout is not None:
            endtime = _time() + timeout
        try:
            return self._wait(timeout=timeout)
        except KeyboardInterrupt:
            if timeout is not None:
                sigint_timeout = min(self._sigint_wait_secs,
                                     self._remaining_time(endtime))
            else:
                sigint_timeout = self._sigint_wait_secs
            self._sigint_wait_secs = 0
            try:
                self._wait(timeout=sigint_timeout)
            except TimeoutExpired:
                pass
            raise

    def _close_pipe_fds(self,
                        p2cread, p2cwrite,
                        c2pread, c2pwrite,
                        errread, errwrite):
        devnull_fd = getattr(self, '_devnull', None)

        with contextlib.ExitStack() as stack:
            if p2cread != -1 and p2cwrite != -1 and p2cread != devnull_fd:
                stack.callback(os.close, p2cread)
            if c2pwrite != -1 and c2pread != -1 and c2pwrite != devnull_fd:
                stack.callback(os.close, c2pwrite)
            if errwrite != -1 and errread != -1 and errwrite != devnull_fd:
                stack.callback(os.close, errwrite)

            if devnull_fd is not None:
                stack.callback(os.close, devnull_fd)

        self._closed_child_pipe_fds = True

    def _get_handles(self, stdin, stdout, stderr):
        """Construct and return tuple with IO objects:
        p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite
        """
        p2cread, p2cwrite = -1, -1
        c2pread, c2pwrite = -1, -1
        errread, errwrite = -1, -1

        if stdin is None:
            pass
        elif stdin == PIPE:
            p2cread, p2cwrite = os.pipe()
            if self.pipesize > 0 and hasattr(fcntl, "F_SETPIPE_SZ"):
                fcntl.fcntl(p2cwrite, fcntl.F_SETPIPE_SZ, self.pipesize)
        elif stdin == DEVNULL:
            p2cread = self._get_devnull()
        elif isinstance(stdin, int):
            p2cread = stdin
        else:
            p2cread = stdin.fileno()

        if stdout is None:
            pass
        elif stdout == PIPE:
            c2pread, c2pwrite = os.pipe()
            if self.pipesize > 0 and hasattr(fcntl, "F_SETPIPE_SZ"):
                fcntl.fcntl(c2pwrite, fcntl.F_SETPIPE_SZ, self.pipesize)
        elif stdout == DEVNULL:
            c2pwrite = self._get_devnull()
        elif isinstance(stdout, int):
            c2pwrite = stdout
        else:
            c2pwrite = stdout.fileno()

        if stderr is None:
            pass
        elif stderr == PIPE:
            errread, errwrite = os.pipe()
            if self.pipesize > 0 and hasattr(fcntl, "F_SETPIPE_SZ"):
                fcntl.fcntl(errwrite, fcntl.F_SETPIPE_SZ, self.pipesize)
        elif stderr == STDOUT:
            if c2pwrite != -1:
                errwrite = c2pwrite
            else: # child's stdout is not set, use parent's stdout
                errwrite = sys.__stdout__.fileno()
        elif stderr == DEVNULL:
            errwrite = self._get_devnull()
        elif isinstance(stderr, int):
            errwrite = stderr
        else:
            errwrite = stderr.fileno()

        return (p2cread, p2cwrite,
                c2pread, c2pwrite,
                errread, errwrite)

    def _execute_child(self, args, executable, preexec_fn, close_fds,
                       pass_fds, cwd, env,
                       startupinfo, creationflags, shell,
                       p2cread, p2cwrite,
                       c2pread, c2pwrite,
                       errread, errwrite,
                       restore_signals,
                       gid, gids, uid, umask,
                       start_new_session, process_group):
        """Execute program (POSIX version)"""

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
            unix_shell = ('/system/bin/sh' if
                      hasattr(sys, 'getandroidapilevel') else '/bin/sh')
            args = [unix_shell, "-c"] + args
            if executable:
                args[0] = executable

        if executable is None:
            executable = args[0]

        sys.audit("subprocess.Popen", executable, args, cwd, env)

        orig_executable = executable

        errpipe_read, errpipe_write = os.pipe()
        low_fds_to_close = []
        while errpipe_write < 3:
            low_fds_to_close.append(errpipe_write)
            errpipe_write = os.dup(errpipe_write)
        for low_fd in low_fds_to_close:
            os.close(low_fd)
        try:
            try:

                if env is not None:
                    env_list = []
                    for k, v in env.items():
                        k = os.fsencode(k)
                        if b'=' in k:
                            raise ValueError("illegal environment variable name")
                        env_list.append(k + b'=' + os.fsencode(v))
                else:
                    env_list = None
                executable = os.fsencode(executable)
                if os.path.dirname(executable):
                    executable_list = (executable,)
                else:
                    executable_list = tuple(
                        os.path.join(os.fsencode(dir), executable)
                        for dir in os.get_exec_path(env))
                fds_to_keep = set(pass_fds)
                fds_to_keep.add(errpipe_write)
                self.pid = _fork_exec(
                        args, executable_list,
                        close_fds, tuple(sorted(map(int, fds_to_keep))),
                        cwd, env_list,
                        p2cread, p2cwrite, c2pread, c2pwrite,
                        errread, errwrite,
                        errpipe_read, errpipe_write,
                        restore_signals, start_new_session,
                        process_group, gid, gids, uid, umask,
                        preexec_fn)
                self._child_created = True
            finally:
                os.close(errpipe_write)

            self._close_pipe_fds(p2cread, p2cwrite,
                                 c2pread, c2pwrite,
                                 errread, errwrite)

            errpipe_data = bytearray()
            while True:
                part = os.read(errpipe_read, 50000)
                errpipe_data += part
                if not part or len(errpipe_data) > 50000:
                    break
        finally:
            os.close(errpipe_read)

        if errpipe_data:
            try:
                pid, sts = os.waitpid(self.pid, 0)
                if pid == self.pid:
                    self._handle_exitstatus(sts)
                else:
                    self.returncode = sys.maxsize
            except ChildProcessError:
                pass

            try:
                exception_name, hex_errno, err_msg = (
                        errpipe_data.split(b':', 2))
                err_msg = err_msg.decode()
            except ValueError:
                exception_name = b'SubprocessError'
                hex_errno = b'0'
                err_msg = 'Bad exception data from child: {!r}'.format(
                              bytes(errpipe_data))
            child_exception_type = getattr(
                    builtins, exception_name.decode('ascii'),
                    SubprocessError)
            if issubclass(child_exception_type, OSError) and hex_errno:
                errno_num = int(hex_errno, 16)
                if err_msg == "noexec:chdir":
                    err_msg = ""
                    err_filename = cwd
                elif err_msg == "noexec":
                    err_msg = ""
                    err_filename = None
                else:
                    err_filename = orig_executable
                if errno_num != 0:
                    err_msg = os.strerror(errno_num)
                if err_filename is not None:
                    raise child_exception_type(errno_num, err_msg, err_filename)
                else:
                    raise child_exception_type(errno_num, err_msg)
            raise child_exception_type(err_msg)


    def _handle_exitstatus(self, sts, _del_safe=None):
        """All callers to this function MUST hold self._waitpid_lock."""
        if _del_safe is None:
            _del_safe = _DEL_SAFE
        if _del_safe.WIFSTOPPED(sts):
            self.returncode = -_del_safe.WSTOPSIG(sts)
        else:
            self.returncode = _del_safe.waitstatus_to_exitcode(sts)

    def _internal_poll(self, _deadstate=None, _del_safe=None):
        """Check if child process has terminated.  Returns returncode
        attribute.

        This method is called by __del__, so it cannot reference anything
        outside of the local scope (nor can any methods it calls).

        """
        if _del_safe is None:
            _del_safe = _DEL_SAFE
        if self.returncode is None:
            if not self._waitpid_lock.acquire(False):
                return None
            try:
                if self.returncode is not None:
                    return self.returncode
                pid, sts = _del_safe.waitpid(self.pid, _del_safe.WNOHANG)
                if pid == self.pid:
                    self._handle_exitstatus(sts, _del_safe)
            except OSError as e:
                if _deadstate is not None:
                    self.returncode = _deadstate
                elif e.errno == _del_safe.ECHILD:
                    self.returncode = 0
            finally:
                self._waitpid_lock.release()
        return self.returncode

    def _try_wait(self, wait_flags):
        """All callers to this function MUST hold self._waitpid_lock."""
        try:
            (pid, sts) = os.waitpid(self.pid, wait_flags)
        except ChildProcessError:
            pid = self.pid
            sts = 0
        return (pid, sts)

    def _wait(self, timeout):
        """Internal implementation of wait() on POSIX."""
        if self.returncode is not None:
            return self.returncode

        if timeout is not None:
            endtime = _time() + timeout
            delay = 0.0005
            while True:
                if self._waitpid_lock.acquire(False):
                    try:
                        if self.returncode is not None:
                            break
                        (pid, sts) = self._try_wait(os.WNOHANG)
                        assert pid == self.pid or pid == 0
                        if pid == self.pid:
                            self._handle_exitstatus(sts)
                            break
                    finally:
                        self._waitpid_lock.release()
                remaining = self._remaining_time(endtime)
                if remaining <= 0:
                    raise TimeoutExpired(self.args, timeout)
                delay = min(delay * 2, remaining, .05)
                time.sleep(delay)
        else:
            while self.returncode is None:
                with self._waitpid_lock:
                    if self.returncode is not None:
                        break
                    (pid, sts) = self._try_wait(0)
                    if pid == self.pid:
                        self._handle_exitstatus(sts)
        return self.returncode


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

        stdout = None
        stderr = None

        if not self._communication_started:
            self._fileobj2output = {}
            if self.stdout:
                self._fileobj2output[self.stdout] = []
            if self.stderr:
                self._fileobj2output[self.stderr] = []

        if self.stdout:
            stdout = self._fileobj2output[self.stdout]
        if self.stderr:
            stderr = self._fileobj2output[self.stderr]

        self._save_input(input)

        if self._input:
            input_view = memoryview(self._input)

        with _PopenSelector() as selector:
            if self.stdin and input:
                selector.register(self.stdin, selectors.EVENT_WRITE)
            if self.stdout and not self.stdout.closed:
                selector.register(self.stdout, selectors.EVENT_READ)
            if self.stderr and not self.stderr.closed:
                selector.register(self.stderr, selectors.EVENT_READ)

            while selector.get_map():
                timeout = self._remaining_time(endtime)
                if timeout is not None and timeout < 0:
                    self._check_timeout(endtime, orig_timeout,
                                        stdout, stderr,
                                        skip_check_and_raise=True)
                    raise RuntimeError(
                        '_check_timeout(..., skip_check_and_raise=True) '
                        'failed to raise TimeoutExpired.')

                ready = selector.select(timeout)
                self._check_timeout(endtime, orig_timeout, stdout, stderr)


                for key, events in ready:
                    if key.fileobj is self.stdin:
                        chunk = input_view[self._input_offset :
                                           self._input_offset + _PIPE_BUF]
                        try:
                            self._input_offset += os.write(key.fd, chunk)
                        except BrokenPipeError:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                        else:
                            if self._input_offset >= len(self._input):
                                selector.unregister(key.fileobj)
                                key.fileobj.close()
                    elif key.fileobj in (self.stdout, self.stderr):
                        data = os.read(key.fd, 32768)
                        if not data:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                        self._fileobj2output[key.fileobj].append(data)

        self.wait(timeout=self._remaining_time(endtime))

        if stdout is not None:
            stdout = b''.join(stdout)
        if stderr is not None:
            stderr = b''.join(stderr)

        if self.text_mode:
            if stdout is not None:
                stdout = self._translate_newlines(stdout,
                                                  self.stdout.encoding,
                                                  self.stdout.errors)
            if stderr is not None:
                stderr = self._translate_newlines(stderr,
                                                  self.stderr.encoding,
                                                  self.stderr.errors)

        return (stdout, stderr)


    def _save_input(self, input):
        if self.stdin and self._input is None:
            self._input_offset = 0
            self._input = input
            if input is not None and self.text_mode:
                self._input = self._input.encode(self.stdin.encoding,
                                                 self.stdin.errors)


    def send_signal(self, sig):
        """Send a signal to the process."""
        self.poll()
        if self.returncode is not None:
            return
        try:
            os.kill(self.pid, sig)
        except ProcessLookupError:
            pass

    def terminate(self):
        """Terminate the process with SIGTERM
        """
        self.send_signal(signal.SIGTERM)

    def kill(self):
        """Kill the process with SIGKILL
        """
        self.send_signal(signal.SIGKILL)


class _DelSafe:
    waitpid = _waitpid
    waitstatus_to_exitcode = _waitstatus_to_exitcode
    WIFSTOPPED = _WIFSTOPPED
    WSTOPSIG = _WSTOPSIG
    WNOHANG = _WNOHANG
    ECHILD = errno.ECHILD

_DEL_SAFE = _DelSafe()
