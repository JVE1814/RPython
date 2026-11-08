"""Start programs as child processes, connect to their standard streams and
collect their exit status.

Convenience functions: run() (the general one, returning a
CompletedProcess), call(), check_call(), check_output(), getstatusoutput()
and getoutput().  Popen is the underlying class.  PIPE, STDOUT and DEVNULL
are the special values accepted for stdin/stdout/stderr.  Failures are
reported as SubprocessError, or as its subclasses CalledProcessError (a
checked command failed) and TimeoutExpired.
"""

import builtins
import errno
import io
import locale
import os
import select
import selectors
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

PIPE = -1
STDOUT = -2
DEVNULL = -3

# Platforms where a process cannot start another one at all.
_NO_PROCESSES = {"emscripten", "wasi", "ios", "tvos", "watchos"}
_can_spawn = sys.platform not in _NO_PROCESSES

if _can_spawn:
    try:
        from _posixsubprocess import fork_exec as _fork_exec
    except ImportError:
        _fork_exec = None
else:
    _fork_exec = None

# Children are always started through fork_exec(), never posix_spawn():
# fork_exec() reports exactly which step failed in the child.
_USE_POSIX_SPAWN = False

# Writes of at most this size to a pipe are atomic and, once select() says
# the pipe is writable, do not block.
_PIPE_BUF = getattr(select, 'PIPE_BUF', 512)

# The selector communicate() multiplexes the pipes with (tests swap it).
_PopenSelector = getattr(selectors, 'PollSelector', selectors.SelectSelector)


def _pidfd_works():
    # pidfd_open() can exist in os but be refused by the kernel or a
    # seccomp filter, so try it once on ourselves.
    if not (hasattr(os, 'pidfd_open') and hasattr(select, 'poll')):
        return False
    try:
        os.close(os.pidfd_open(os.getpid()))
    except OSError:
        return False
    return True


_CAN_USE_PIDFD_OPEN = _pidfd_works()
_CAN_USE_KQUEUE = hasattr(select, 'kqueue') and hasattr(select, 'KQ_FILTER_PROC')

# Upper bound for a single sleep in the kernel while waiting for a child;
# longer timeouts are covered by repeating it.
_MAXIMUM_WAIT_TIMEOUT = 86400.0


# ---------------------------------------------------------------- exceptions

class SubprocessError(Exception):
    """Base class for the exceptions raised by this module."""


def _output_alias():
    # `stdout` is another name for `output` on the exceptions below, so that
    # they read like CompletedProcess (stdout/stderr).
    def get(self):
        return self.output

    def put(self, value):
        self.output = value

    return property(get, put, doc="Same as output: the captured standard output.")


class CalledProcessError(SubprocessError):
    """A command run by check_call(), check_output() or run(check=True)
    exited with a non-zero status.

    Attributes: returncode, cmd, output (alias stdout) and stderr; the last
    two are None unless the stream was captured.
    """

    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode
        self.cmd = cmd
        self.output = output
        self.stderr = stderr

    stdout = _output_alias()

    def __str__(self):
        status = self.returncode
        if isinstance(status, int) and status < 0:
            # A negative status is the number of the signal that ended it.
            try:
                cause = repr(signal.Signals(-status))
            except ValueError:
                cause = "unknown signal %d" % -status
            return "Command '%s' died with %s." % (self.cmd, cause)
        # Anything that is not an int (a mock, say) is shown as it is.
        shown = "%d" % status if isinstance(status, int) else "%s" % (status,)
        return "Command '%s' returned non-zero exit status %s." % (self.cmd, shown)


class TimeoutExpired(SubprocessError):
    """A timeout passed before the child process finished.

    Attributes: cmd, timeout, output (alias stdout) and stderr, the latter
    holding whatever was captured before the timeout (or None).
    """

    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr

    stdout = _output_alias()

    def __str__(self):
        return "Command '%s' timed out after %s seconds" % (self.cmd, self.timeout)


# ------------------------------------------------- abandoned Popen objects

# Popen objects that were garbage collected while their child still ran.
# They stay here until a later Popen() or _cleanup() manages to reap them,
# so that no zombie is left behind.
_active = []


def _cleanup():
    if _active is None:
        return
    for proc in _active[:]:
        if proc._poll_nowait(lost_status=sys.maxsize) is None:
            continue
        try:
            _active.remove(proc)
        except ValueError:
            pass        # another thread got there first


# ------------------------------------------- reproducing interpreter flags

def _optim_args_from_interpreter_flags():
    """The -O option equivalent to the current optimization level."""
    level = sys.flags.optimize
    return ['-' + 'O' * level] if level else []


# sys.flags fields that map to a (repeatable) single-letter option.
_COUNTED_FLAGS = (('debug', 'd'), ('dont_write_bytecode', 'B'),
                  ('no_site', 'S'), ('verbose', 'v'), ('bytes_warning', 'b'),
                  ('quiet', 'q'))
# Options implied by -I, given separately only when -I is not.
_ISOLATION_PARTS = (('ignore_environment', '-E'), ('no_user_site', '-s'),
                    ('safe_path', '-P'))


def _implied_warning_filters(flags):
    # Filters that -b/-bb and -X dev add to sys.warnoptions by themselves.
    implied = []
    if flags.bytes_warning:
        implied.append("error::BytesWarning" if flags.bytes_warning > 1
                       else "default::BytesWarning")
    if flags.dev_mode:
        implied.append("default")
    return implied


def _args_from_interpreter_flags():
    """Command line options that start a new interpreter with the settings
    of this one (as given by sys.flags, sys.warnoptions and -X options)."""
    flags = sys.flags
    args = _optim_args_from_interpreter_flags()
    args += ['-' + letter * getattr(flags, name)
             for name, letter in _COUNTED_FLAGS if getattr(flags, name) > 0]
    if flags.isolated:
        args.append('-I')
    else:
        args += [opt for name, opt in _ISOLATION_PARTS if getattr(flags, name)]

    implied = _implied_warning_filters(flags)
    for spec in sys.warnoptions:
        if spec in implied:
            implied.remove(spec)
        else:
            args.append('-W' + spec)

    if flags.dev_mode:
        args += ['-X', 'dev']
    for name, value in getattr(sys, '_xoptions', {}).items():
        if name == 'dev':
            continue
        args += ['-X', name if value is True else '%s=%s' % (name, value)]
    return args


def _text_encoding():
    # The encoding text mode uses when none was given: the one open() would
    # pick, including the EncodingWarning of -X warn_default_encoding,
    # attributed to the first caller outside this module.
    if sys.flags.warn_default_encoding:
        level = 1
        frame = sys._getframe()
        while frame is not None and frame.f_globals is globals():
            level += 1
            frame = frame.f_back
        warnings.warn("'encoding' argument not specified.",
                      EncodingWarning, level)
    return "utf-8" if sys.flags.utf8_mode else locale.getencoding()


# ------------------------------------------------------- convenience API

def call(*popenargs, timeout=None, **kwargs):
    """Run a command, wait for it and return its exit status.

    Takes Popen's arguments, plus a timeout after which the child is killed
    and TimeoutExpired raised.
    """
    with Popen(*popenargs, **kwargs) as proc:
        try:
            return proc.wait(timeout=timeout)
        except:
            # Timeout, ^C or anything else: do not leave it running.  The
            # with statement reaps it.
            proc.kill()
            raise


def check_call(*popenargs, **kwargs):
    """Like call(), but raise CalledProcessError for a non-zero status.

    Returns 0 otherwise.
    """
    status = call(*popenargs, **kwargs)
    if status == 0:
        return 0
    cmd = kwargs.get("args")
    raise CalledProcessError(status, cmd if cmd is not None else popenargs[0])


def _empty_input_for(kwargs):
    # check_output(input=None) means "an empty stdin", in the stream's mode.
    textual = any(kwargs.get(key) for key in
                  ('universal_newlines', 'text', 'encoding', 'errors'))
    return '' if textual else b''


def check_output(*popenargs, timeout=None, **kwargs):
    """Run a command and return what it wrote to standard output.

    A non-zero exit status raises CalledProcessError (with the output
    attached).  Accepts run()'s arguments except stdout and check, which it
    sets itself.
    """
    for taken in ('stdout', 'check'):
        if taken in kwargs:
            raise ValueError(f'{taken} argument not allowed, it will be overridden.')
    if 'input' in kwargs and kwargs['input'] is None:
        kwargs['input'] = _empty_input_for(kwargs)
    return run(*popenargs, stdout=PIPE, timeout=timeout, check=True,
               **kwargs).stdout


class CompletedProcess(object):
    """What run() returns: the args, returncode, stdout and stderr of a
    finished process (the streams are None unless captured)."""

    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        shown = ['args={!r}'.format(self.args),
                 'returncode={!r}'.format(self.returncode)]
        shown += ['{}={!r}'.format(name, getattr(self, name))
                  for name in ('stdout', 'stderr')
                  if getattr(self, name) is not None]
        return '{}({})'.format(type(self).__name__, ', '.join(shown))

    __class_getitem__ = classmethod(types.GenericAlias)

    def check_returncode(self):
        """Raise CalledProcessError if the process failed."""
        if self.returncode:
            raise CalledProcessError(self.returncode, self.args, self.stdout,
                                     self.stderr)


def run(*popenargs, input=None, capture_output=False, timeout=None,
        check=False, **kwargs):
    """Run a command to completion and return a CompletedProcess.

    input is written to the child's stdin (bytes, or str in text mode);
    capture_output=True captures both stdout and stderr; when timeout
    expires the child is killed and TimeoutExpired raised; check=True turns
    a non-zero exit status into CalledProcessError.  Everything else is
    passed to Popen.
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
            # Reap it now; communicate() already put the partial output on
            # the exception.
            proc.wait()
            raise
        except:
            proc.kill()     # the with statement waits for it
            raise
        status = proc.poll()
    if check and status:
        raise CalledProcessError(status, proc.args, output=out, stderr=err)
    return CompletedProcess(proc.args, status, out, err)


def list2cmdline(seq):
    """Join arguments into one command line, quoted so that the Microsoft
    C runtime splits it back into the same arguments.

    An argument is put in double quotes when it is empty or contains a
    space or tab.  A literal double quote is escaped with a backslash;
    backslashes are literal except in front of a double quote (escaped or
    closing), where each one is doubled.
    """
    def quote(arg):
        enclose = arg == '' or ' ' in arg or '\t' in arg
        out = []
        run_of_backslashes = 0
        for ch in arg:
            if ch == '\\':
                run_of_backslashes += 1
                continue
            if ch == '"':
                out.append('\\' * (2 * run_of_backslashes + 1) + '"')
            else:
                out.append('\\' * run_of_backslashes + ch)
            run_of_backslashes = 0
        # Trailing backslashes only matter if a closing quote follows them.
        out.append('\\' * (run_of_backslashes * (2 if enclose else 1)))
        text = ''.join(out)
        return '"' + text + '"' if enclose else text

    return ' '.join(quote(arg) for arg in map(os.fsdecode, seq))


def getstatusoutput(cmd, *, encoding=None, errors=None):
    """Run cmd in a shell and return (exit status, output).

    stdout and stderr are combined; one trailing newline is removed.
    """
    try:
        text = check_output(cmd, shell=True, text=True, stderr=STDOUT,
                            encoding=encoding, errors=errors)
        status = 0
    except CalledProcessError as failure:
        text, status = failure.output, failure.returncode
    return status, text[:-1] if text.endswith('\n') else text


def getoutput(cmd, *, encoding=None, errors=None):
    """Like getstatusoutput(), returning the output only."""
    return getstatusoutput(cmd, encoding=encoding, errors=errors)[1]


# --------------------------------------------------- spawning helpers

def _child_failure(report, cwd, executable):
    """Turn the failure report a child wrote before exec() into an exception.

    The report reads b"ExceptionName:hex errno:detail".  The detail
    "noexec:chdir" means chdir(cwd) failed and "noexec" another step before
    exec(); otherwise exec() of the executable failed.
    """
    def garbled():
        return SubprocessError(
            'Bad exception data from child: {!r}'.format(bytes(report)))

    try:
        name, number, detail = report.split(b':', 2)
        name, detail = name.decode('ascii'), detail.decode()
    except ValueError:
        return garbled()
    exc_type = getattr(builtins, name, None)
    if not (isinstance(exc_type, type) and issubclass(exc_type, BaseException)):
        exc_type = SubprocessError
    if not (issubclass(exc_type, OSError) and number):
        return exc_type(detail)
    try:
        number = int(number, 16)
    except ValueError:
        return garbled()
    text = os.strerror(number) if number else ""
    if detail == "noexec":
        return exc_type(number, text)
    return exc_type(number, text, cwd if detail == "noexec:chdir" else executable)


def _checked_id(value, kind, by_name, overflow=OverflowError):
    """A user or group id given as a name or an integer, validated."""
    if isinstance(value, str):
        return by_name(value)
    if not isinstance(value, int):
        return None
    if value < 0:
        raise ValueError(f"{kind} ID cannot be negative, got {value}")
    if value >= 2 ** 32:
        raise overflow(f"{kind} ID is too large: {value}")
    return value


def _gid_by_name(name):
    try:
        import grp
    except ImportError:
        raise ValueError("The group parameter cannot be a string "
                         "on systems without the grp module")
    return grp.getgrnam(name).gr_gid


def _uid_by_name(name):
    try:
        import pwd
    except ImportError:
        raise ValueError("The user parameter cannot be a string "
                         "on systems without the pwd module")
    return pwd.getpwnam(name).pw_uid


def _identity(user, group, extra_groups):
    """Resolve user=, group= and extra_groups= to (uid, gid, gids)."""
    uid = gid = gids = None
    if group is not None:
        if not hasattr(os, 'setregid'):
            raise ValueError("The 'group' parameter is not supported on the "
                             "current platform")
        gid = _checked_id(group, "Group", _gid_by_name)
        if gid is None:
            raise TypeError("Group must be a string or an integer, not {}"
                            .format(type(group)))
    if extra_groups is not None:
        if not hasattr(os, 'setgroups'):
            raise ValueError("The 'extra_groups' parameter is not supported "
                             "on the current platform")
        if isinstance(extra_groups, str):
            raise ValueError("Groups must be a list, not a string")
        gids = []
        for item in extra_groups:
            # Unlike group=, an out-of-range list item is a ValueError.
            value = _checked_id(item, "Group", _gid_by_name, overflow=ValueError)
            if value is None:
                raise TypeError("Items in extra_groups must be a string or "
                                "integer, not {}".format(type(item)))
            gids.append(value)
    if user is not None:
        if not hasattr(os, 'setreuid'):
            raise ValueError("The 'user' parameter is not supported on the "
                             "current platform")
        uid = _checked_id(user, "User", _uid_by_name)
        if uid is None:
            raise TypeError("User must be a string or an integer")
    return uid, gid, gids


def _environment_block(env):
    # fork_exec() wants "NAME=value" byte strings.
    block = []
    for name, value in env.items():
        name = os.fsencode(name)
        if b'=' in name:
            raise ValueError("illegal environment variable name")
        block.append(name + b'=' + os.fsencode(value))
    return block


def _executable_candidates(executable, env):
    # A bare program name is looked up along PATH (of the child's
    # environment); fork_exec() tries each candidate in turn.
    program = os.fsencode(executable)
    if os.path.dirname(program):
        return (program,)
    return tuple(os.path.join(os.fsencode(folder), program)
                 for folder in os.get_exec_path(env))


def _fd_above_stdio(fd):
    # Move fd to a number above 2, where the child's stdio set-up cannot
    # clobber it.
    low = []
    while fd < 3:
        low.append(fd)
        fd = os.dup(fd)
    for spare in low:
        os.close(spare)
    return fd


def _read_all(fd, limit):
    data = bytearray()
    while len(data) <= limit:
        chunk = os.read(fd, limit)
        if not chunk:
            break
        data += chunk
    return bytes(data)


def _ignoring_broken_pipe(action, *args):
    """Run action(*args), treating "the reader went away" as success."""
    try:
        action(*args)
    except BrokenPipeError:
        pass
    except OSError as exc:
        # Writing to a pipe whose reader exited can also give EINVAL.
        if exc.errno != errno.EINVAL:
            raise


class _StdioPlan:
    """The descriptors arranged for the child's stdin, stdout and stderr.

    child[i] is what the child gets as descriptor i (-1: inherit ours);
    parent[i] is the end of a pipe this process keeps (-1: no pipe).
    `opened` lists the descriptors created here for the child's sake (pipe
    ends, /dev/null), which this process must close once the child has them.
    """

    def __init__(self, pipesize):
        self.child = [-1, -1, -1]
        self.parent = [-1, -1, -1]
        self.opened = []
        self._pipesize = pipesize
        self._devnull = None

    def _new_pipe(self):
        read_end, write_end = os.pipe()
        if (self._pipesize > 0 and fcntl is not None
                and hasattr(fcntl, "F_SETPIPE_SZ")):
            fcntl.fcntl(write_end, fcntl.F_SETPIPE_SZ, self._pipesize)
        return read_end, write_end

    def _target(self, spec):
        if spec == DEVNULL:
            if self._devnull is None:
                self._devnull = os.open(os.devnull, os.O_RDWR)
                self.opened.append(self._devnull)
            return self._devnull
        return spec if isinstance(spec, int) else spec.fileno()

    def arrange(self, stdin, stdout, stderr):
        if stdin == PIPE:
            self.child[0], self.parent[0] = self._new_pipe()
            self.opened.append(self.child[0])
        elif stdin is not None:
            self.child[0] = self._target(stdin)
        if stdout == PIPE:
            self.parent[1], self.child[1] = self._new_pipe()
            self.opened.append(self.child[1])
        elif stdout is not None:
            self.child[1] = self._target(stdout)
        if stderr == PIPE:
            self.parent[2], self.child[2] = self._new_pipe()
            self.opened.append(self.child[2])
        elif stderr == STDOUT:
            if self.child[1] != -1:
                self.child[2] = self.child[1]
            else:
                ours = sys.__stdout__
                self.child[2] = ours.fileno() if ours is not None else 1
        elif stderr is not None:
            self.child[2] = self._target(stderr)

    def release_child_side(self):
        """Close what was opened for the child (safe to call twice)."""
        pending, self.opened = self.opened, []
        failure = None
        for fd in pending:
            try:
                os.close(fd)
            except OSError as exc:
                failure = failure or exc
        if failure is not None:
            raise failure

    def fork_exec_fds(self):
        # The order fork_exec() takes them in.
        return (self.child[0], self.parent[0], self.parent[1], self.child[1],
                self.parent[2], self.child[2])


class _SpawnOptions(types.SimpleNamespace):
    """The Popen arguments that only matter while starting the child."""


class _Exchange:
    """communicate() state that survives a TimeoutExpired: the encoded
    input and how much of it was written, and the output read so far."""

    def __init__(self, outgoing, readers):
        self.outgoing = outgoing
        self.written = 0
        self.received = {stream: [] for stream in readers}

    def unsent(self):
        return self.outgoing is not None and self.written < len(self.outgoing)

    def collected(self, stream):
        if stream is None or stream not in self.received:
            return None
        return b''.join(self.received[stream])

    def partial(self, stream):
        # On timeout: what arrived so far, or None if nothing did.
        chunks = self.received.get(stream) if stream is not None else None
        return b''.join(chunks) if chunks else None


# --------------------------------------------------------------- Popen

class Popen:
    """A child process.

    The constructor starts the program; see the module documentation for
    its many arguments.  Attributes: args, pid, returncode (None while the
    child runs, negative for "killed by that signal"), and stdin, stdout
    and stderr (file objects for streams that are PIPEs, else None).
    """

    _child_created = False
    # Kept on the class so that a finalizer still finds it while the
    # module is being torn down.
    _os = os

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
        if not _can_spawn:
            raise OSError(errno.ENOTSUP,
                          f"{sys.platform} does not support processes.")

        # Serializes waitpid() between threads that wait or poll at once.
        self._reap_lock = threading.Lock()
        self._exchange = None
        self._communication_started = False
        # After ^C the child (in the same process group) most likely got
        # SIGINT too: give it this long to exit before re-raising.
        self._ctrl_c_grace = 0.25

        if bufsize is None:
            bufsize = -1
        if pipesize is None:
            pipesize = -1
        if not isinstance(bufsize, int):
            raise TypeError("bufsize must be an integer")
        if not isinstance(pipesize, int):
            raise TypeError("pipesize must be an integer")
        if stdout is STDOUT:
            raise ValueError("STDOUT can only be used for stderr")
        if startupinfo is not None:
            raise ValueError("startupinfo is only supported on Windows platforms")
        if creationflags != 0:
            raise ValueError("creationflags is only supported on Windows platforms")
        if pass_fds and not close_fds:
            warnings.warn("pass_fds overriding close_fds.", RuntimeWarning)
            close_fds = True
        if (text is not None and universal_newlines is not None
                and bool(text) != bool(universal_newlines)):
            raise SubprocessError('Cannot disambiguate when both text '
                                  'and universal_newlines are supplied but '
                                  'different. Pass one or the other.')

        self.args = args
        self.pid = None
        self.returncode = None
        self.stdin = self.stdout = self.stderr = None
        self.pipesize = pipesize
        self.text_mode = encoding or errors or text or universal_newlines
        if self.text_mode and encoding is None:
            encoding = _text_encoding()
        self.encoding = encoding
        self.errors = errors

        line_buffering = False
        if bufsize == 1:
            if self.text_mode:
                line_buffering, bufsize = True, -1
            else:
                warnings.warn("line buffering (buffering=1) isn't supported "
                              "in binary mode, the default buffer size will "
                              "be used", RuntimeWarning)

        uid, gid, gids = _identity(user, group, extra_groups)
        options = _SpawnOptions(
            preexec_fn=preexec_fn, close_fds=close_fds, pass_fds=pass_fds,
            restore_signals=restore_signals,
            start_new_session=start_new_session,
            process_group=-1 if process_group is None else process_group,
            uid=uid, gid=gid, gids=gids, umask=umask)

        plan = _StdioPlan(pipesize)
        try:
            plan.arrange(stdin, stdout, stderr)
            self._open_streams(plan, bufsize, line_buffering)
            self._execute_child(args, executable, shell, cwd, env, plan, options)
        except:
            for stream in (self.stdin, self.stdout, self.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except OSError:
                        pass
            try:
                plan.release_child_side()
            except OSError:
                pass
            raise

    def _open_streams(self, plan, bufsize, line_buffering):
        # File objects for the ends of the pipes this process keeps.
        wrap = io.TextIOWrapper
        if plan.parent[0] != -1:
            self.stdin = io.open(plan.parent[0], 'wb', bufsize)
            if self.text_mode:
                self.stdin = wrap(self.stdin, write_through=True,
                                  line_buffering=line_buffering,
                                  encoding=self.encoding, errors=self.errors)
        if plan.parent[1] != -1:
            self.stdout = io.open(plan.parent[1], 'rb', bufsize)
            if self.text_mode:
                self.stdout = wrap(self.stdout, encoding=self.encoding,
                                   errors=self.errors)
        if plan.parent[2] != -1:
            self.stderr = io.open(plan.parent[2], 'rb', bufsize)
            if self.text_mode:
                self.stderr = wrap(self.stderr, encoding=self.encoding,
                                   errors=self.errors)

    def __repr__(self):
        text = "<{}: returncode: {} args: {!r}>".format(
            type(self).__name__, self.returncode, self.args)
        if len(text) > 80:
            text = text[:76] + "...>"
        return text

    __class_getitem__ = classmethod(types.GenericAlias)

    @property
    def universal_newlines(self):
        # Older name of text_mode, kept for compatibility.
        return self.text_mode

    @universal_newlines.setter
    def universal_newlines(self, universal_newlines):
        self.text_mode = bool(universal_newlines)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, value, traceback):
        for stream in (self.stdout, self.stderr):
            if stream:
                stream.close()
        try:
            if self.stdin:
                self.stdin.close()      # may fail flushing into a dead pipe
        finally:
            if exc_type is KeyboardInterrupt:
                # No open-ended wait after ^C: only what is left of the
                # grace period, if an interrupted wait() did not use it.
                if self._ctrl_c_grace > 0:
                    try:
                        self._wait(timeout=self._ctrl_c_grace)
                    except TimeoutExpired:
                        pass
                self._ctrl_c_grace = 0
            else:
                self.wait()

    def __del__(self, _warn=warnings.warn, _lost=sys.maxsize):
        if not self._child_created:
            return      # the constructor failed, or never got that far
        if self.returncode is None:
            _warn("subprocess %s is still running" % self.pid,
                  ResourceWarning, source=self)
        self._poll_nowait(lost_status=_lost)
        if self.returncode is None and _active is not None:
            _active.append(self)

    # -- starting the child

    def _execute_child(self, args, executable, shell, cwd, env, plan, options):
        """Start the program in a new process (with fork_exec())."""
        if isinstance(args, (str, bytes)):
            argv = [args]
        elif isinstance(args, os.PathLike):
            if shell:
                raise TypeError('path-like args is not allowed when '
                                'shell is true')
            argv = [args]
        else:
            argv = list(args)
        if shell:
            sh = ('/system/bin/sh' if hasattr(sys, 'getandroidapilevel')
                  else '/bin/sh')
            argv = [executable or sh, "-c", *argv]
        if executable is None:
            executable = argv[0]

        sys.audit("subprocess.Popen", executable, argv, cwd, env)
        if _fork_exec is None:
            raise OSError(errno.ENOTSUP, "fork_exec() is not available")

        # Convert everything first: between fork and exec the child runs no
        # Python code (preexec_fn aside).
        env_block = None if env is None else _environment_block(env)
        candidates = _executable_candidates(executable, env)

        # The child writes a failure report into this pipe; its write end is
        # close-on-exec, so EOF without data means exec() succeeded.
        report_r, report_w = os.pipe()
        try:
            report_w = _fd_above_stdio(report_w)
            try:
                keep = tuple(sorted({*map(int, options.pass_fds), report_w}))
                self.pid = _fork_exec(
                    argv, candidates, options.close_fds, keep, cwd, env_block,
                    *plan.fork_exec_fds(), report_r, report_w,
                    options.restore_signals, options.start_new_session,
                    options.process_group, options.gid, options.gids,
                    options.uid, options.umask, options.preexec_fn)
                self._child_created = True
            finally:
                os.close(report_w)
            plan.release_child_side()
            report = _read_all(report_r, 50000)
        finally:
            os.close(report_r)

        if report:
            self._reap_failed_child()
            raise _child_failure(report, cwd, executable)

    def _reap_failed_child(self):
        # The child exits right after reporting; collect it so it does not
        # linger as a zombie.
        try:
            pid, status = os.waitpid(self.pid, 0)
        except ChildProcessError:
            return
        if pid == self.pid:
            self._record_status(status)
        else:
            self.returncode = sys.maxsize

    # -- exit status

    def _record_status(self, status):
        tools = self._os
        if tools.WIFSTOPPED(status):
            self.returncode = -tools.WSTOPSIG(status)
        else:
            self.returncode = tools.waitstatus_to_exitcode(status)

    def _reap_locked(self, flags):
        """One waitpid() call; the caller holds _reap_lock.

        True when the child has been collected.
        """
        if self.returncode is not None:
            return True
        try:
            pid, status = self._os.waitpid(self.pid, flags)
        except ChildProcessError:
            # Somebody else reaped it (or SIGCHLD is ignored): it is gone
            # and its status is lost.
            self.returncode = 0
            return True
        if pid == self.pid:
            self._record_status(status)
            return True
        return False    # still running (or a spurious wake-up)

    def _poll_nowait(self, lost_status=None):
        """Collect the child if it has exited, without blocking.

        Returns the returncode, or None.  If another thread is reaping it
        right now, gives up at once.  lost_status is recorded when
        waitpid() fails for another reason (used while finalizing).
        """
        if self.returncode is not None:
            return self.returncode
        if not self._reap_lock.acquire(False):
            return None
        try:
            self._reap_locked(self._os.WNOHANG)
        except OSError:
            if lost_status is not None:
                self.returncode = lost_status
        finally:
            self._reap_lock.release()
        return self.returncode

    def poll(self):
        """Return the returncode if the child has finished, else None."""
        return self._poll_nowait()

    def _try_reap(self):
        # A non-blocking attempt that skips (False) if another thread holds
        # the lock; unlike poll() it lets unexpected errors through.
        if not self._reap_lock.acquire(False):
            return self.returncode is not None
        try:
            return self._reap_locked(os.WNOHANG)
        finally:
            self._reap_lock.release()

    def _wait_pidfd(self, timeout):
        """Sleep until the child exits or timeout seconds pass, watching a
        pidfd.  True if it (probably) exited; OSError if pidfds fail."""
        pidfd = os.pidfd_open(self.pid)
        try:
            watcher = select.poll()
            watcher.register(pidfd, select.POLLIN)
            millis = -(-max(timeout, 0) * 1000 // 1)       # rounded up
            return bool(watcher.poll(int(min(millis, 2 ** 31 - 1))))
        finally:
            os.close(pidfd)

    def _wait_kqueue(self, timeout):
        """Like _wait_pidfd(), with a kqueue process-exit filter."""
        queue = select.kqueue()
        try:
            event = select.kevent(self.pid, filter=select.KQ_FILTER_PROC,
                                  flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                                  fflags=select.KQ_NOTE_EXIT)
            return bool(queue.control([event], 1, min(max(timeout, 0), 1e8)))
        finally:
            queue.close()

    def _kernel_waiter(self):
        if _CAN_USE_PIDFD_OPEN:
            return self._wait_pidfd
        if _CAN_USE_KQUEUE:
            return self._wait_kqueue
        return None

    def _wait(self, timeout):
        """wait() without the KeyboardInterrupt handling."""
        if self.returncode is not None:
            return self.returncode
        if timeout is None:
            while True:
                with self._reap_lock:
                    if self._reap_locked(0):
                        return self.returncode

        deadline = _time() + timeout
        if self._try_reap():
            return self.returncode
        # Sleep in the kernel until the child exits, if the platform can
        # tell us; if it cannot (or it fails), poll with growing pauses.
        waiter = self._kernel_waiter()
        while waiter is not None:
            left = deadline - _time()
            try:
                exited = waiter(min(max(left, 0), _MAXIMUM_WAIT_TIMEOUT))
            except OSError:
                break
            if exited:
                if self._try_reap():
                    return self.returncode
                break       # notified, but not reapable yet: poll
            if deadline - _time() <= 0:
                raise TimeoutExpired(self.args, timeout)
        pause = 0.0005
        while not self._try_reap():
            left = deadline - _time()
            if left <= 0:
                raise TimeoutExpired(self.args, timeout)
            pause = min(pause * 2, left, 0.05)
            time.sleep(pause)
        return self.returncode

    def _grace_after_interrupt(self, deadline):
        # ^C arrived while we were waiting: allow the child the grace period
        # (bounded by the caller's own deadline) once, then let it go.
        grace = self._ctrl_c_grace
        if deadline is not None:
            grace = min(grace, deadline - _time())
        self._ctrl_c_grace = 0
        try:
            self._wait(timeout=grace)
        except TimeoutExpired:
            pass

    def wait(self, timeout=None):
        """Wait for the child to finish and return its returncode.

        Raises TimeoutExpired if timeout seconds pass first.
        """
        deadline = None if timeout is None else _time() + timeout
        try:
            return self._wait(timeout=timeout)
        except KeyboardInterrupt:
            self._grace_after_interrupt(deadline)
            raise

    # -- talking to the child

    def _stdin_write(self, input):
        # Blocking write of the whole input, then EOF; a child that stops
        # reading early is not an error.
        if input:
            _ignoring_broken_pipe(self.stdin.write, input)
        _ignoring_broken_pipe(self.stdin.close)

    def communicate(self, input=None, timeout=None):
        """Send input to the child, read its output until EOF and wait for
        it to exit.  Returns (stdout data, stderr data); an element is None
        when that stream is not a PIPE.

        If timeout seconds pass first, TimeoutExpired is raised; the child
        keeps running and communicate() can be called again (without input)
        to carry on where this call stopped.
        """
        if self._communication_started and input:
            raise ValueError("Cannot send input after starting communication")

        pipes = [s for s in (self.stdin, self.stdout, self.stderr) if s]
        if timeout is None and not self._communication_started and len(pipes) < 2:
            # At most one pipe: simple blocking I/O cannot deadlock.
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

        deadline = None if timeout is None else _time() + timeout
        try:
            out, err = self._communicate(input, deadline, timeout)
        except KeyboardInterrupt:
            self._grace_after_interrupt(deadline)
            raise
        finally:
            self._communication_started = True
        try:
            self.wait(timeout=None if deadline is None else deadline - _time())
        except TimeoutExpired as exc:
            exc.timeout = timeout
            raise
        return self._decoded(out, self.stdout), self._decoded(err, self.stderr)

    def _decoded(self, data, stream):
        if data is None or not self.text_mode:
            return data
        text = data.decode(stream.encoding, stream.errors)
        return text.replace("\r\n", "\n").replace("\r", "\n")

    def _outgoing(self, input):
        # The input as a flat byte buffer whose offsets count bytes.
        if input is None:
            return None
        if self.text_mode:
            return memoryview(input.encode(self.stdin.encoding,
                                           self.stdin.errors))
        try:
            return memoryview(input).cast('B')
        except TypeError:
            return memoryview(bytes(memoryview(input)))

    def _start_exchange(self, input):
        stdin = self.stdin
        if stdin:
            # Push out whatever the caller wrote before; a closed or broken
            # pipe here is not an error.
            try:
                stdin.flush()
            except BrokenPipeError:
                pass
            except ValueError:
                if not stdin.closed:
                    raise
            if not input:
                _ignoring_broken_pipe(stdin.close)
        outgoing = self._outgoing(input) if stdin else None
        readers = [s for s in (self.stdout, self.stderr) if s]
        return _Exchange(outgoing, readers)

    def _communicate(self, input, deadline, timeout):
        """Multiplex the pipes until every one is closed or the deadline
        passes; returns the raw (stdout, stderr) bytes."""
        if self._exchange is None or not self._communication_started:
            self._exchange = self._start_exchange(input)
        exchange = self._exchange
        stdin = self.stdin

        with _PopenSelector() as selector:
            if exchange.outgoing is not None and stdin and not stdin.closed:
                if exchange.unsent():
                    selector.register(stdin, selectors.EVENT_WRITE)
                else:
                    # Everything went out before an earlier timeout; the
                    # EOF is still owed.
                    _ignoring_broken_pipe(stdin.close)
            for stream in (self.stdout, self.stderr):
                if stream and not stream.closed:
                    selector.register(stream, selectors.EVENT_READ)

            received = exchange.received
            outgoing = exchange.outgoing
            chunk = _PIPE_BUF
            while selector.get_map():
                left = None if deadline is None else deadline - _time()
                if left is not None and left < 0:
                    self._raise_timeout(timeout)
                ready = selector.select(left)
                if deadline is not None and _time() > deadline:
                    self._raise_timeout(timeout)
                for key, _ in ready:
                    stream = key.fileobj
                    if stream is stdin:
                        start = exchange.written
                        try:
                            exchange.written = start + os.write(
                                key.fd, outgoing[start:start + chunk])
                        except BrokenPipeError:
                            exchange.written = len(outgoing)    # nobody reads it
                        finished = exchange.written >= len(outgoing)
                    else:
                        data = os.read(key.fd, 32768)
                        received[stream].append(data)
                        finished = not data
                    if finished:
                        selector.unregister(stream)
                        stream.close()

        return exchange.collected(self.stdout), exchange.collected(self.stderr)

    def _raise_timeout(self, timeout):
        exchange = self._exchange
        raise TimeoutExpired(self.args, timeout,
                             output=exchange.partial(self.stdout),
                             stderr=exchange.partial(self.stderr))

    # -- signals

    def send_signal(self, sig):
        """Send signal sig to the child, unless it is known to have exited."""
        # Collect a finished child first: once reaped, its pid may already
        # belong to an unrelated process.
        self.poll()
        if self.returncode is not None:
            return
        try:
            os.kill(self.pid, sig)
        except ProcessLookupError:
            pass        # exited between poll() and kill()

    def terminate(self):
        """Ask the child to stop (SIGTERM)."""
        self.send_signal(signal.SIGTERM)

    def kill(self):
        """Kill the child (SIGKILL)."""
        self.send_signal(signal.SIGKILL)
