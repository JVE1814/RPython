"""
The Python Debugger Pdb
=======================

pdb is an interactive source-level debugger for Python programs.  It lets
you stop a program at chosen places (breakpoints, conditional breakpoints),
walk through its execution one line or one call at a time, look at the
stack, evaluate arbitrary Python expressions in the context of any frame,
and examine programs after they crashed (post-mortem debugging).

There are several ways to start it.  From the command line:

    python -m pdb [-c command] (-m module | -p pid | pyfile) [args ...]

runs a script (or a module, or attaches to a running process) under the
debugger.  From Python code, the usual entry point is a call placed where
you want to stop:

    >>> import pdb
    >>> pdb.set_trace()      # or simply: breakpoint()

and after an uncaught exception, in an interactive session:

    >>> import pdb
    >>> pdb.pm()

The prompt is "(Pdb) ".  Commands may be abbreviated to the letters shown
outside parentheses in the command list ("s(tep)" can be typed "s").  An
empty line repeats the previous command (except for "list", which then goes
on listing).  Input that is not a debugger command is executed as Python
code in the current frame; prefix a line with "!" to force that, e.g. when
a variable has the name of a command.  Several commands may be put on one
line, separated by ";;".

Names of the form $name are "convenience variables": they live as long as
the debugger is stopped at one place.  pdb sets a few itself:

    $_frame      the frame being examined
    $_retval     the return value, when stopped at a return
    $_exception  the exception, when stopped at an exception
    $_asynctask  the asyncio task, when stopped inside one

Files named .pdbrc in the home directory and in the current directory are
read when the debugger starts; their lines are executed as if they had been
typed at the prompt (lines starting with "#" are ignored).

Type "help" at the prompt for the list of commands and "help <command>"
for the description of one of them.
"""

import os
import io
import json
import re
import sys
import cmd
import bdb
import dis
import code
import glob
import types
import codeop
import pprint
import signal
import inspect
import keyword
import textwrap
import tokenize
import itertools
import traceback
import linecache
import _colorize

from contextlib import contextmanager


class Restart(Exception):
    """Raised by the run/restart command to start the program again."""


__all__ = ["run", "pm", "Pdb", "runeval", "runctx", "runcall", "set_trace",
           "post_mortem", "help"]


# ---------------------------------------------------------------------------
# helpers

def _prologue_opcodes():
    names = ('RESUME', 'RETURN_GENERATOR', 'POP_TOP', 'MAKE_CELL',
             'COPY_FREE_VARS', 'NOP', 'EXTENDED_ARG', 'CACHE')
    return {dis.opmap[name] for name in names if name in dis.opmap}


def _first_body_line(code):
    """The line of the first instruction of `code` that belongs to its body
    (not to the frame set-up done before the first statement)."""
    skip = _prologue_opcodes()
    raw = code.co_code
    for start, end, line in code.co_lines():
        if line is None:
            continue
        for offset in range(start, end, 2):
            if raw[offset] not in skip:
                return line
    return code.co_firstlineno


def _read_source_lines(filename):
    try:
        with tokenize.open(filename) as fp:
            return fp.read().splitlines(keepends=True)
    except (OSError, SyntaxError, UnicodeDecodeError):
        return linecache.getlines(filename)


def find_function(funcname, filename):
    """Locate the top-level definition of `funcname` in the file.

    Returns (funcname, filename, lineno) where lineno is the first line of
    the body, or None if there is no such definition."""
    lines = _read_source_lines(filename)
    if not lines:
        return None
    pattern = re.compile(r'(async\s+)?def\s+%s\s*(\[.*\])?\s*\('
                         % re.escape(funcname))
    for index, line in enumerate(lines):
        if not pattern.match(line):
            continue
        def_line = index + 1
        # the definition ends with the first non-blank line that is not
        # indented (a comment or a deeper indented line belongs to it)
        block = [line]
        for following in lines[index + 1:]:
            stripped = following.strip()
            if stripped and not following[0].isspace() and not stripped.startswith('#'):
                break
            block.append(following)
        try:
            module = compile(''.join(block), filename, 'exec')
        except Exception:
            return funcname, filename, def_line
        for const in module.co_consts:
            if isinstance(const, types.CodeType) and const.co_name == funcname:
                return funcname, filename, _first_body_line(const) + index
        return funcname, filename, def_line
    return None


def lasti2lineno(code, lasti):
    """The source line of the instruction at offset `lasti` of `code`."""
    best = 0
    for start, end, line in code.co_lines():
        if start <= lasti < end:
            return line if line is not None else 0
        if start <= lasti and line is not None:
            best = line
    return best


class _rstr(str):
    """A string whose repr() is itself (shown unquoted)."""

    def __repr__(self):
        return self


# the text put between a stack entry and its source line
line_prefix = '\n-> '


# ---------------------------------------------------------------------------
# what `python -m pdb` runs

class _ExecutableTarget:
    filename: str
    code: types.CodeType | str
    namespace: dict


class _ScriptTarget(_ExecutableTarget):
    def __init__(self, target):
        resolved = os.path.realpath(target)
        if not os.path.exists(resolved):
            # e.g. /proc/self/fd/N of a pipe resolves to a pseudo name
            resolved = os.path.abspath(target)
        if not os.path.exists(resolved):
            print(f'Error: {target} does not exist')
            sys.exit(1)
        if os.path.isdir(resolved):
            print(f'Error: {target} is a directory')
            sys.exit(1)
        self._target = resolved
        # like `python script.py`: the script's directory comes first
        if not sys.flags.safe_path:
            sys.path[0] = os.path.dirname(self._target)

    def __repr__(self):
        return self._target

    __str__ = __repr__

    @property
    def filename(self):
        return self._target

    @property
    def code(self):
        # read again on every run: the file may have been edited
        with io.open_code(self._target) as fp:
            source = fp.read()
        return compile(source, self._target, 'exec')

    @property
    def namespace(self):
        return dict(
            __name__='__main__',
            __file__=self._target,
            __builtins__=__builtins__,
            __spec__=None,
        )


class _ModuleTarget(_ExecutableTarget):
    def __init__(self, target):
        self._target = target
        import runpy
        try:
            _, self._spec, self._code = runpy._get_module_details(target)
        except ImportError as exc:
            print(f'ImportError: {exc}')
            sys.exit(1)
        except Exception:
            traceback.print_exc()
            sys.exit(1)

    def __repr__(self):
        return self._target

    __str__ = __repr__

    @property
    def filename(self):
        return self._code.co_filename

    @property
    def code(self):
        return self._code

    @property
    def namespace(self):
        return dict(
            __name__='__main__',
            __file__=os.path.normcase(os.path.abspath(self.filename)),
            __package__=self._spec.parent,
            __loader__=self._spec.loader,
            __spec__=self._spec,
            __builtins__=__builtins__,
        )


class _ZipTarget(_ExecutableTarget):
    def __init__(self, target):
        import runpy
        self._target = os.path.realpath(target)
        sys.path.insert(0, self._target)
        try:
            _, self._spec, self._code = runpy._get_main_module_details()
        except ImportError as exc:
            print(f'ImportError: {exc}')
            sys.exit(1)
        except Exception:
            traceback.print_exc()
            sys.exit(1)

    def __repr__(self):
        return self._target

    __str__ = __repr__

    @property
    def filename(self):
        return self._code.co_filename

    @property
    def code(self):
        return self._code

    @property
    def namespace(self):
        return dict(
            __name__='__main__',
            __file__=os.path.normcase(os.path.abspath(self.filename)),
            __package__=self._spec.parent,
            __loader__=self._spec.loader,
            __spec__=self._spec,
            __builtins__=__builtins__,
        )


# ---------------------------------------------------------------------------

class _PdbInteractiveConsole(code.InteractiveConsole):
    """The console of the 'interact' command: output goes to pdb."""

    def __init__(self, ns, message):
        self._message = message
        super().__init__(locals=ns, local_exit=True)

    def write(self, data):
        self._message(data, end='')


_default_backend = 'settrace'


def set_default_backend(backend):
    """Choose the event source new Pdb instances use by default:
    'settrace' or 'monitoring'."""
    global _default_backend
    if backend not in ('settrace', 'monitoring'):
        raise ValueError("Invalid backend: %s" % backend)
    _default_backend = backend


def get_default_backend():
    """The event source new Pdb instances use by default."""
    return _default_backend


def _pyrepl_available():
    """Whether pdb may read its input with _pyrepl (the new REPL)."""
    if os.getenv('PYTHON_BASIC_REPL'):
        return False
    try:
        from _pyrepl.main import CAN_USE_PYREPL
    except ImportError:
        return False
    return bool(CAN_USE_PYREPL)


class _BpCommand(str):
    """A command queued by a breakpoint's command list."""


_SILENCE = '_pdbcmd_silence_frame_status'


class Pdb(bdb.Bdb, cmd.Cmd):
    """The debugger: a bdb.Bdb that talks to the user through cmd.Cmd."""

    _previous_sigint_handler = None
    _last_pdb_instance = None
    # mtimes of the files seen while stopped, shared by all instances
    _file_mtime_table = {}

    # commands after which a breakpoint command list ends implicitly
    _resuming_commands = ('do_continue', 'do_step', 'do_next', 'do_return',
                          'do_until', 'do_quit', 'do_jump')

    def __init__(self, completekey='tab', stdin=None, stdout=None, skip=None,
                 nosigint=False, readrc=True, mode=None, backend=None,
                 colorize=False):
        bdb.Bdb.__init__(self, skip=skip,
                         backend=backend if backend else get_default_backend())
        cmd.Cmd.__init__(self, completekey, stdin, stdout)
        sys.audit("pdb.Pdb")
        if stdin is not None or stdout is not None:
            self.use_rawinput = 0
        self.prompt = '(Pdb) '
        self.aliases = {}
        self.displaying = {}
        self.mainpyfile = ''
        self._wait_for_mainpyfile = False
        self._main_frame_started = False
        self.tb_lineno = {}
        self.mode = mode
        self.colorize = bool(colorize) and _colorize.can_colorize(
            file=stdout if stdout is not None else sys.stdout)
        self.nosigint = nosigint
        self.allow_kbdint = False
        self._user_requested_quit = False
        self._chained_exceptions = ()
        self._chained_exception_index = 0
        self._status_pending = False
        self._convenience_globals = None
        self._stop_variables = {}
        self._continuing_block = False
        self._readline_active = False
        self._resumed_coroutine = None
        self._async_frame = None
        self._pending_awaitable = None
        self.lineno = None
        self.stack = []
        self.curindex = 0
        self.curframe = None

        self.rcLines = []
        if readrc:
            self._read_rc_files()

        # breakpoint number -> list of commands
        self.commands = {}
        self.commands_defining = False
        self.commands_bnum = None

        self.pyrepl_input = None
        if (_pyrepl_available() and stdin is None and stdout is None):
            try:
                self.pyrepl_input = PdbPyReplInput(self, colorize)
            except Exception:
                self.pyrepl_input = None

    def _read_rc_files(self):
        home_rc = None
        try:
            home_rc = os.path.expanduser('~/.pdbrc')
        except Exception:
            pass
        seen = []
        for path in (home_rc, '.pdbrc'):
            if not path:
                continue
            try:
                real = os.path.realpath(path)
            except OSError:
                real = path
            if real in seen:
                continue
            seen.append(real)
            try:
                with open(path, encoding='utf-8') as fp:
                    self.rcLines.extend(line.rstrip('\r\n') for line in fp)
            except OSError:
                pass

    # ---- signals

    def sigint_handler(self, signum, frame):
        if self.allow_kbdint:
            raise KeyboardInterrupt
        self.message("\nInterrupted; type 'continue' to go on.")
        self.set_step()
        self.set_trace(frame)

    def _install_sigint_handler(self):
        if self.nosigint:
            return
        try:
            Pdb._previous_sigint_handler = signal.signal(
                signal.SIGINT, self.sigint_handler)
        except ValueError:
            # not the main thread (or signals unavailable)
            pass

    def _restore_sigint_handler(self):
        if Pdb._previous_sigint_handler:
            try:
                signal.signal(signal.SIGINT, Pdb._previous_sigint_handler)
            except ValueError:
                pass
            Pdb._previous_sigint_handler = None

    # ---- tracing entry points

    def set_trace(self, frame=None, *, commands=None):
        """Stop at `frame` (default: the caller) as soon as possible."""
        Pdb._last_pdb_instance = self
        if frame is None:
            frame = sys._getframe().f_back
        if commands is not None:
            self.rcLines.extend(commands)
        super().set_trace(frame)

    async def set_trace_async(self, frame=None, *, commands=None):
        """Like set_trace() for a coroutine: 'await' may be used at the
        prompt; the awaitable then runs in the coroutine's context."""
        if self._async_frame is not None:
            return
        if frame is None:
            frame = sys._getframe().f_back
        Pdb._last_pdb_instance = self
        if commands is not None:
            self.rcLines.extend(commands)
        # the prompt runs inside this coroutine: no events until we leave
        self._prepare_trace(frame)
        self.stopframe = frame
        self._async_frame = frame
        try:
            while True:
                self._pending_awaitable = None
                saved_enter = self.enterframe
                self.enterframe = frame
                try:
                    self.interaction(frame, None)
                finally:
                    self.enterframe = saved_enter
                awaitable = self._pending_awaitable
                self._pending_awaitable = None
                if awaitable is None:
                    break
                try:
                    if self.breaks:
                        self.enterframe = frame
                        try:
                            self.set_continue()
                        finally:
                            self.enterframe = None
                        self.start_trace()
                    await awaitable
                except Exception:
                    self._error_exc()
                finally:
                    self.stop_trace()
        finally:
            self._async_frame = None
        if self.quitting:
            return
        if (self.returnframe is None and self.stoplineno == -1 and
                not self.breaks):
            # continue without breakpoints: nothing left to watch
            return
        # if the coroutine was suspended by an awaitable run at the prompt,
        # its resumption may be reported only now: it is not a new call
        self._resumed_coroutine = frame
        self.start_trace()

    # ---- bdb hooks

    def _is_main_file(self, frame):
        return self.canonic(frame.f_code.co_filename) == self.mainpyfile

    def user_call(self, frame, argument_list):
        """Called when a function is entered and we might stop in it."""
        if self._wait_for_mainpyfile:
            if self._is_main_file(frame):
                self._main_frame_started = True
            return
        if frame is self._resumed_coroutine:
            self._resumed_coroutine = None
            return
        if self.stop_here(frame):
            self.message('--Call--')
            self.interaction(frame, None)

    def user_line(self, frame):
        """Called when we stop or break at a line."""
        if self._wait_for_mainpyfile:
            if not self._is_main_file(frame) or not frame.f_lineno or frame.f_lineno <= 0:
                return
            self._wait_for_mainpyfile = False
        self._queue_breakpoint_commands()
        self.interaction(frame, None)

    def user_opcode(self, frame):
        """Called when we stop at an instruction (set_trace())."""
        if self._wait_for_mainpyfile:
            return
        if self._is_jump_target(frame):
            # an instruction where several paths join (the end of `a and
            # f()`) belongs to no statement of its own: stop at the next one
            return
        self.interaction(frame, None)

    _jump_target_cache = {}

    @classmethod
    def _is_jump_target(cls, frame):
        code = frame.f_code
        targets = cls._jump_target_cache.get(code)
        if targets is None:
            try:
                targets = frozenset(dis.findlabels(code.co_code))
            except Exception:
                targets = frozenset()
            if len(cls._jump_target_cache) > 2000:
                cls._jump_target_cache.clear()
            cls._jump_target_cache[code] = targets
        return frame.f_lasti in targets

    def user_return(self, frame, return_value):
        """Called when a function we stop in returns."""
        if self._wait_for_mainpyfile:
            # an empty main script never runs a line: stop at its end
            if not (self._is_main_file(frame) and frame.f_back is self.botframe):
                return
            self._wait_for_mainpyfile = False
        frame.f_locals['__return__'] = return_value
        self._stop_variables['_retval'] = return_value
        self.message('--Return--')
        self.interaction(frame, None)

    def user_exception(self, frame, exc_info):
        """Called when an exception occurs where we stop."""
        if self._wait_for_mainpyfile:
            return
        exc_type, exc_value, exc_tb = exc_info
        frame.f_locals['__exception__'] = exc_type, exc_value
        # a StopIteration without traceback is the interpreter ending a
        # sub-iterator (for loop, yield from, await), not an error
        internal = exc_tb is None and exc_type is StopIteration
        self.message('%s%s' % ('Internal ' if internal else '',
                                self._format_exc(exc_value)))
        self.interaction(frame, exc_value if isinstance(exc_value, BaseException)
                         and exc_tb is not None and not internal else exc_tb)

    def _queue_breakpoint_commands(self):
        number = self.currentbp
        self.currentbp = None
        if not number or number not in self.commands:
            return False
        self.cmdqueue[:0] = [_BpCommand(line) for line in self.commands[number]]
        return True

    def break_anywhere(self, frame):
        """Whether a breakpoint is set on a line of the frame's own code
        (not merely in the same file)."""
        filename = self.canonic(frame.f_code.co_filename)
        lines = self.breaks.get(filename)
        if not lines:
            return False
        own = self._code_lines(frame.f_code)
        return any(line in own for line in lines)

    _code_line_cache = {}

    @classmethod
    def _code_lines(cls, code):
        lines = cls._code_line_cache.get(code)
        if lines is None:
            lines = {line for _, _, line in code.co_lines() if line is not None}
            if len(cls._code_line_cache) > 2000:
                cls._code_line_cache.clear()
            cls._code_line_cache[code] = lines
        return lines

    def bp_commands(self, frame):
        """Queue the commands of the breakpoint just hit; True if any."""
        return self._queue_breakpoint_commands()

    # ---- stopping

    def _chain_of(self, exc):
        """The exceptions related to `exc` (causes and contexts), oldest
        first, `exc` last."""
        chain = []
        current = exc
        while current is not None and len(chain) < 999:
            if any(current is seen for seen in chain):
                break
            chain.append(current)
            if current.__cause__ is not None:
                current = current.__cause__
            elif current.__context__ is not None and not current.__suppress_context__:
                current = current.__context__
            else:
                current = None
        chain.reverse()
        return tuple(chain)

    @contextmanager
    def _hold_exceptions(self, exceptions):
        self._chained_exceptions = exceptions
        self._chained_exception_index = len(exceptions) - 1
        try:
            yield
        finally:
            self._chained_exceptions = ()
            self._chained_exception_index = 0

    def interaction(self, frame, tb_or_exc):
        """Stop: show where we are and run the command loop."""
        self._restore_sigint_handler()
        self._cmd_frame = None
        self._resumed_coroutine = None
        exceptions = ()
        if isinstance(tb_or_exc, BaseException):
            exc = tb_or_exc
            if exc.__traceback__ is None:
                raise ValueError("A valid traceback must be passed if no "
                                 "exception is being handled")
            exceptions = self._chain_of(exc)
            tb = exc.__traceback__
        else:
            tb = tb_or_exc
        with self._hold_exceptions(exceptions):
            if self.setup(frame, tb):
                self.forget()
                return
            if self.cmdqueue:
                self._status_pending = True
            else:
                self.print_stack_entry(self.stack[self.curindex])
            self._cmdloop()
            self.forget()

    def setup(self, f, tb):
        """Prepare the stack for frame `f` / traceback `tb`."""
        self.forget()
        self.stack, self.curindex = self.get_stack(f, tb)
        while tb is not None:
            self.tb_lineno[tb.tb_frame] = tb.tb_lineno
            tb = tb.tb_next
        self.curframe = self.stack[self.curindex][0]
        self._reset_convenience_variables()
        self.set_convenience_variable(self.curframe, '_frame', self.curframe)
        if self._chained_exceptions:
            self.set_convenience_variable(
                self.curframe, '_exception',
                self._chained_exceptions[self._chained_exception_index])
        for name, value in self._stop_variables.items():
            self.set_convenience_variable(self.curframe, name, value)
        self._stop_variables = {}
        task = self._current_task()
        if task is not None:
            self.set_convenience_variable(self.curframe, '_asynctask', task)
        self._validate_file_mtime()
        return self._queue_rc_lines()

    def _current_task(self):
        if 'asyncio' not in sys.modules:
            return None
        import asyncio
        try:
            return asyncio.current_task()
        except RuntimeError:
            return None

    def forget(self):
        self.lineno = None
        self.stack = []
        self.curindex = 0
        self.curframe = None
        self.tb_lineno.clear()
        self._status_pending = False
        self._reset_convenience_variables()

    def _queue_rc_lines(self):
        if not self.rcLines:
            return False
        lines = [line.strip() for line in self.rcLines]
        self.rcLines = []
        self.cmdqueue.extend(line for line in lines
                             if line and not line.startswith('#'))
        return False

    def execRcLines(self):
        return self._queue_rc_lines()

    # ---- convenience variables

    def set_convenience_variable(self, frame, name, value):
        store = frame.f_globals.get('__pdb_convenience_variables')
        if store is None:
            store = frame.f_globals['__pdb_convenience_variables'] = {}
            self._convenience_globals = frame.f_globals
        store[name] = value

    def _reset_convenience_variables(self):
        globs = self._convenience_globals
        if globs is not None:
            globs.pop('__pdb_convenience_variables', None)
            self._convenience_globals = None

    @staticmethod
    def _replace_convenience_variables(source):
        """Rewrite $name (outside string literals) as a lookup in the
        convenience variable store."""
        if '$' not in source:
            return source
        out = []
        i = 0
        n = len(source)
        # stack of open f-string contexts: (quote, depth of braces)
        fstack = []
        depth = 0
        ident_start = re.compile(r'[^\W\d]\w*')
        prefix_re = re.compile(r'([rRbBuUfFtT]{1,2})?("""|\'\'\'|"|\')')

        def emit_string(start):
            # copy a (non f-) string literal starting at `start`; return end
            m = prefix_re.match(source, start)
            prefix, quote = m.group(1) or '', m.group(2)
            raw = 'r' in prefix.lower()
            j = m.end()
            while j < n:
                if not raw and source[j] == '\\':
                    j += 2
                    continue
                if source.startswith(quote, j):
                    return j + len(quote)
                j += 1
            return n

        def scan_fstring(start):
            # returns end of the f-string, rewriting $names in fields
            m = prefix_re.match(source, start)
            prefix, quote = m.group(1) or '', m.group(2)
            raw = 'r' in prefix.lower()
            out.append(source[start:m.end()])
            j = m.end()
            text_start = j
            while j < n:
                c = source[j]
                if not raw and c == '\\':
                    j += 2
                    continue
                if source.startswith(quote, j):
                    out.append(source[text_start:j + len(quote)])
                    return j + len(quote)
                if c == '{':
                    if source.startswith('{{', j):
                        j += 2
                        continue
                    out.append(source[text_start:j + 1])
                    j = scan_code(j + 1, closing='}')
                    text_start = j
                    continue
                j += 1
            out.append(source[text_start:])
            return n

        def scan_code(j, closing=None):
            depth = 0
            while j < n:
                c = source[j]
                if closing and depth == 0 and c == closing:
                    out.append(c)
                    return j + 1
                if c in '([{':
                    depth += 1
                elif c in ')]}':
                    depth -= 1
                if c == '#':
                    k = source.find('\n', j)
                    k = n if k < 0 else k
                    out.append(source[j:k])
                    j = k
                    continue
                m = prefix_re.match(source, j)
                if m and (j == 0 or not (source[j - 1].isalnum() or source[j - 1] == '_')):
                    prefix = (m.group(1) or '').lower()
                    if 'f' in prefix or 't' in prefix:
                        j = scan_fstring(j)
                    else:
                        end = emit_string(j)
                        out.append(source[j:end])
                        j = end
                    continue
                if c == '$':
                    name = ident_start.match(source, j + 1)
                    if name:
                        out.append('__pdb_convenience_variables["%s"]'
                                   % name.group())
                        j = name.end()
                        continue
                if c.isalnum() or c == '_':
                    k = j
                    while k < n and (source[k].isalnum() or source[k] == '_'):
                        k += 1
                    out.append(source[j:k])
                    j = k
                    continue
                out.append(c)
                j += 1
            return j

        scan_code(0)
        return ''.join(out)

    # ---- output

    def message(self, msg, end='\n'):
        print(msg, end=end, file=self.stdout)

    def error(self, msg):
        print('***', msg, file=self.stdout)

    def _format_exc(self, exc):
        return traceback.format_exception_only(exc)[-1].strip()

    def _error_exc(self):
        self.error(self._format_exc(sys.exception()))

    def _safe_repr(self, obj, expr):
        try:
            return repr(obj)
        except Exception as exc:
            return _rstr('*** repr(%s) failed: %s ***' % (expr, self._format_exc(exc)))

    def displayhook(self, obj):
        """Show the value of an expression typed at the prompt."""
        if obj is not None:
            self.message(repr(obj))

    def _colorize_code(self, text):
        if not self.colorize or not text:
            return text
        try:
            from _pyrepl.utils import gen_colors, style_tags
        except ImportError:
            return text
        try:
            colors = list(gen_colors(text))
        except Exception:
            return text
        theme = _colorize.get_theme(force_color=True).syntax
        tags = style_tags(text, colors)
        pieces = []
        current = None
        for char, tag in zip(text, tags):
            if tag != current:
                if current is not None:
                    pieces.append(theme.reset)
                if tag is not None:
                    pieces.append(theme[tag])
                current = tag
            pieces.append(char)
        if current is not None:
            pieces.append(theme.reset)
        return ''.join(pieces)

    # ---- the command loop

    def _cmdloop(self):
        while True:
            try:
                self.allow_kbdint = True
                self.cmdloop()
                self.allow_kbdint = False
                break
            except KeyboardInterrupt:
                self.message('--interrupted--')

    def _flush_status(self):
        if self._status_pending:
            self._status_pending = False
            if self.stack:
                self.print_stack_entry(self.stack[self.curindex])
                self._show_displays()

    def _readline_completion(self):
        """Install our completer in readline while reading commands."""
        if not (self.use_rawinput and self.completekey):
            return None
        if self.pyrepl_input is not None:
            return None
        try:
            if not sys.stdin.isatty():
                return None
        except (AttributeError, ValueError):
            return None
        try:
            import readline
        except ImportError:
            return None
        # importing rlcompleter installs its own completer: do it first
        import rlcompleter
        previous = readline.get_completer()
        previous_delims = readline.get_completer_delims()
        readline.set_completer(self.complete)
        readline.set_completer_delims(' \t\n`@#%^&*()=+[{]}\\|;:\'",<>?')
        if getattr(readline, 'backend', 'readline') == 'editline':
            readline.parse_and_bind('bind ^I rl_complete')
        else:
            readline.parse_and_bind(self.completekey + ': complete')
        self._readline_active = True
        return readline, previous, previous_delims

    def _read_line(self, prompt, state='pdb'):
        """One line of input (without its newline); EOFError at the end."""
        if self.pyrepl_input is not None and self.use_rawinput:
            return self.pyrepl_input.readline(prompt)
        if self.use_rawinput:
            return input(prompt)
        self.stdout.write(prompt)
        self.stdout.flush()
        line = self.stdin.readline()
        if not line:
            raise EOFError
        return line.rstrip('\r\n')

    def cmdloop(self, intro=None):
        self.preloop()
        saved = self._readline_completion()
        try:
            stop = None
            while not stop:
                if self.cmdqueue:
                    line = self.cmdqueue.pop(0)
                else:
                    self._flush_status()
                    line = self._read_command()
                from_breakpoint = isinstance(line, _BpCommand)
                saved_lastcmd = self.lastcmd
                line = self.precmd(line)
                stop = self.onecmd(line)
                stop = self.postcmd(stop, line)
                if from_breakpoint:
                    self.lastcmd = saved_lastcmd
            self.postloop()
        finally:
            if saved is not None:
                self._readline_active = False
                readline, previous, delims = saved
                readline.set_completer(previous)
                readline.set_completer_delims(delims)

    def _read_command(self):
        """The next command typed by the user ('EOF' at the end)."""
        try:
            return self._read_line(self.prompt)
        except EOFError:
            return 'EOF'

    def preloop(self):
        if not self._status_pending:
            self._show_displays()

    def _show_displays(self):
        displaying = self.displaying.get(self.curframe)
        if not displaying:
            return
        for expr, old in displaying.items():
            new = self._getval_except(expr)
            try:
                changed = new is not old and new != old
            except Exception:
                changed = True
            if changed:
                displaying[expr] = new
                self.message('display %s: %s  [old: %s]' % (
                    expr, self._safe_repr(new, expr), self._safe_repr(old, expr)))

    @staticmethod
    def _split_commands(line):
        """Split at the first ';;' that is not inside a string literal:
        (first, rest or None)."""
        quote = None
        i = 0
        while i < len(line):
            c = line[i]
            if quote:
                if c == '\\':
                    i += 2
                    continue
                if c == quote:
                    quote = None
            elif c in '\'"':
                quote = c
            elif line.startswith(';;', i):
                return line[:i].rstrip(), line[i + 2:].lstrip()
            i += 1
        return line, None

    def _expand_alias(self, line):
        """Replace a leading alias (repeatedly); None after an error."""
        args = line.split()
        seen = 0
        while args and args[0] in self.aliases and seen < 100:
            seen += 1
            name = args[0]
            text = self.aliases[name]
            for index in range(1, 10):
                marker = '%' + str(index)
                if marker in text:
                    if index >= len(args):
                        self.error("Not enough arguments for alias '%s'" % name)
                        return None
                    text = text.replace(marker, args[index])
                elif '%*' not in text:
                    if index < len(args):
                        self.error("Too many arguments for alias '%s'" % name)
                        return None
                    break
            text = text.replace('%*', ' '.join(args[1:]))
            line = text
            args = line.split()
        return line

    def precmd(self, line):
        """Expand aliases and split ';;'-separated commands."""
        if not line.strip():
            return line
        if line.lstrip().startswith('!'):
            return line
        expanded = self._expand_alias(line)
        if expanded is None:
            return '!'
        line = expanded
        first, rest = self._split_commands(line)
        if rest is not None:
            self.cmdqueue.insert(0, rest)
            line = first
        return line

    def parseline(self, line):
        command, arg, line = super().parseline(line)
        if command:
            # "list(x)", "c['a']", "n = 1": Python, not a command
            stripped = line.lstrip()
            after = stripped[len(command):len(command) + 1]
            if after and not after.isspace():
                return None, None, line
        return command, arg, line

    def onecmd(self, line):
        """Run one command; true means: leave the command loop."""
        if line.startswith('_pdbcmd'):
            handler = getattr(self, line, None)
            if handler is not None:
                return handler()
        return cmd.Cmd.onecmd(self, line)

    def emptyline(self):
        if self.lastcmd:
            return self.onecmd(self.lastcmd)

    # internal commands queued with breakpoint command lists
    def _pdbcmd_silence_frame_status(self):
        self._status_pending = False

    def _pdbcmd_print_frame_status(self):
        self._flush_status()

    # ---- executing Python code

    @property
    def curframe_locals(self):
        return self.curframe.f_locals

    def _compile_error_message(self, expr):
        """'' if `expr` compiles as an expression, else the error."""
        try:
            compile(expr, "<stdin>", "eval")
        except Exception as exc:
            return _rstr(self._format_exc(exc))
        return ''

    def _locals_are_globals(self):
        frame = self.curframe
        return frame is None or frame.f_locals is frame.f_globals or \
            frame.f_code.co_flags & inspect.CO_NEWLOCALS == 0

    def _run_in_closure(self, source, mode):
        """Run `source` as the body of a function that sees the frame's
        locals as closure variables (so that lambdas, comprehensions and
        nested functions find them).  `mode` is 'single' or 'eval'.

        Returns (True, value) if done, (False, None) if not needed."""
        import ast
        frame = self.curframe
        if self._locals_are_globals():
            return False, None
        try:
            tree = ast.parse(source, mode='eval' if mode == 'eval' else 'exec')
        except SyntaxError:
            return False, None
        # only code with nested scopes needs it
        scoped = (ast.Lambda, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef,
                  ast.GeneratorExp, ast.ListComp, ast.SetComp, ast.DictComp)
        if not any(isinstance(node, scoped) for node in ast.walk(tree)):
            return False, None
        frame_locals = frame.f_locals
        snapshot = dict(frame_locals)
        declared_global = set()
        if mode != 'eval':
            for node in tree.body:
                for sub in ast.walk(node):
                    if isinstance(sub, (ast.FunctionDef, ast.AsyncFunctionDef,
                                        ast.ClassDef, ast.Lambda)):
                        continue
                    if isinstance(sub, ast.Global):
                        declared_global.update(sub.names)
        names = [name for name in snapshot
                 if isinstance(name, str) and name.isidentifier()
                 and not keyword.iskeyword(name) and name not in declared_global]
        show, keep = '__pdb_show', '__pdb_keep'
        cell_names = names + [show, keep]
        if mode == 'eval':
            body = [ast.Return(value=tree.body)]
        else:
            statements = []
            for node in tree.body:
                if isinstance(node, ast.Expr):
                    node = ast.Expr(value=ast.Call(
                        func=ast.Name(id=show, ctx=ast.Load()),
                        args=[node.value], keywords=[]))
                statements.append(node)
            capture = ast.Expr(value=ast.Call(
                func=ast.Name(id=keep, ctx=ast.Load()),
                args=[ast.Call(func=ast.Name(id='locals', ctx=ast.Load()),
                               args=[], keywords=[])],
                keywords=[]))
            body = [ast.Try(body=statements, handlers=[], orelse=[],
                            finalbody=[capture])]
        scope = ast.FunctionDef(
            name='__pdb_scope', args=ast.arguments(), decorator_list=[],
            body=[ast.Nonlocal(names=cell_names)] + body, returns=None,
            type_params=[])
        outer = ast.FunctionDef(
            name='__pdb_outer', args=ast.arguments(), decorator_list=[],
            body=[ast.Assign(targets=[ast.Name(id=name, ctx=ast.Store())],
                             value=ast.Constant(None)) for name in cell_names]
                 + [scope], returns=None, type_params=[])
        module = ast.Module(body=[outer], type_ignores=[])
        ast.fix_missing_locations(module)
        try:
            compiled = compile(module, '<stdin>', 'exec')
        except Exception:
            return False, None
        outer_code = next(c for c in compiled.co_consts
                          if isinstance(c, types.CodeType))
        scope_code = next(c for c in outer_code.co_consts
                          if isinstance(c, types.CodeType))
        captured = {}
        values = dict(snapshot)
        values[show] = self.displayhook
        values[keep] = captured.update
        cells = {name: types.CellType(values[name]) if name in values
                 else types.CellType() for name in scope_code.co_freevars}
        function = types.FunctionType(
            scope_code, frame.f_globals, '__pdb_scope',
            closure=tuple(cells[name] for name in scope_code.co_freevars))
        try:
            result = function()
        finally:
            for name, cell in cells.items():
                if name in (show, keep):
                    continue
                try:
                    value = cell.cell_contents
                except ValueError:
                    continue
                if name not in snapshot or snapshot[name] is not value:
                    frame_locals[name] = value
            for name, value in captured.items():
                if name in (show, keep) or name in cells:
                    continue
                if name not in snapshot or snapshot[name] is not value:
                    frame_locals[name] = value
        return True, result

    def _eval(self, expr, frame=None):
        """Evaluate `expr` in the current (or given) frame."""
        expr = self._replace_convenience_variables(expr)
        if frame is None or frame is self.curframe:
            done, value = self._run_in_closure(expr, 'eval')
            if done:
                return value
            frame = self.curframe
            return eval(expr, frame.f_globals, frame.f_locals)
        return eval(expr, frame.f_globals, frame.f_locals)

    def _getval(self, arg):
        try:
            return self._eval(arg)
        except:
            self._error_exc()
            raise

    def _getval_except(self, arg, frame=None):
        try:
            return self._eval(arg, frame)
        except:
            return _rstr('** raised %s **' % self._format_exc(sys.exception()))

    def _read_continuation(self, prompt):
        return self._read_line(prompt, state='pdb')


    def _compile_input(self, source):
        """Compile what was typed at the prompt: a code object, or None if
        the statement is not complete yet (SyntaxError if invalid)."""
        source = self._replace_convenience_variables(source)
        try:
            return codeop.compile_command(source, '<stdin>', 'single')
        except SyntaxError:
            if self._async_frame is None:
                raise
            import ast
            return compile(source, '<stdin>', 'exec',
                           ast.PyCF_ALLOW_TOP_LEVEL_AWAIT)

    def default(self, line):
        """Execute a line that is not a command as Python code."""
        if line[:1] == '!':
            line = line[1:].strip()
        if not line:
            return
        frame = self.curframe
        if frame is None:
            return
        source = line
        try:
            # a single line followed by a newline is complete; a block
            # needs an empty line after its last line
            compiled = self._compile_input(source + "\n")
            if compiled is None:
                # an incomplete statement: read until it is complete
                prompt = '...'.ljust(len(self.prompt))
                lines = [source]
                with self._block_editing(lines):
                    while compiled is None:
                        try:
                            more = self._read_continuation(prompt)
                        except (EOFError, KeyboardInterrupt):
                            self.lastcmd = ''
                            self.message('')
                            return
                        if not more.strip():
                            more = ''
                        lines.append(more)
                        source += '\n' + more
                        compiled = self._compile_input(source)
                self.lastcmd = source
        except:
            self._error_exc()
            return
        if compiled.co_flags & inspect.CO_COROUTINE:
            # `await` at the prompt of set_trace_async()
            self._pending_awaitable = eval(compiled, frame.f_globals, frame.f_locals)
            return True
        saved = sys.stdin, sys.stdout, sys.displayhook
        try:
            sys.stdin, sys.stdout, sys.displayhook = self.stdin, self.stdout, self.displayhook
            done, _ = self._run_in_closure(
                self._replace_convenience_variables(source), 'single')
            if not done:
                exec(compiled, frame.f_globals, frame.f_locals)
        except:
            self._error_exc()
        finally:
            sys.stdin, sys.stdout, sys.displayhook = saved

    # ---- help

    def _help_message_from_doc(self, doc, usage_only=False):
        lines = [line.strip() for line in doc.rstrip().splitlines()]
        while lines and not lines[0]:
            lines.pop(0)
        if not lines:
            return 'No help message found.'
        usage_end = lines.index('') if '' in lines else 1
        indent = ' ' * 6 if usage_only else ''
        out = []
        for index, line in enumerate(lines):
            if index == 0:
                out.append(indent + 'Usage: ' + line)
            elif index < usage_end:
                out.append(indent + '       ' + line)
            elif usage_only:
                break
            else:
                out.append(line)
        return '\n'.join(out)

    def _command_doc(self):
        caller = sys._getframe(2).f_code.co_name
        method = getattr(self, caller, None)
        return getattr(method, '__doc__', None) or ''

    def _print_invalid_arg(self, arg):
        doc = self._command_doc()
        self.error('Invalid argument: %s\n%s' % (
            arg, self._help_message_from_doc(doc, usage_only=True)))

    def _print_arg_required(self):
        doc = self._command_doc()
        self.error('Argument is required for this command\n%s'
                   % self._help_message_from_doc(doc, usage_only=True))

    def do_help(self, arg):
        """h(elp) [command]

        Without argument, list the available commands.  With a command
        name, show its documentation.  "help pdb" shows the whole manual,
        "help exec" explains how to run Python code at the prompt.
        """
        if not arg:
            return cmd.Cmd.do_help(self, arg)
        topic = getattr(self, 'help_' + arg, None)
        if topic is not None:
            return topic()
        command = getattr(self, 'do_' + arg, None)
        if command is None:
            self.error('No help for %r' % arg)
            return
        if sys.flags.optimize >= 2:
            self.error('No help for %r (docstrings are stripped under -OO)' % arg)
            return
        if command.__doc__ is None:
            self.error('No help for %r; __doc__ string missing' % arg)
            return
        self.message(self._help_message_from_doc(command.__doc__))

    do_h = do_help

    def help_exec(self):
        """(!) statement

        Anything typed at the prompt that is not a debugger command runs as
        Python code in the frame being examined; a block (def, for, if...)
        goes on until an empty line.  Start the line with "!" when its first
        word is also the name of a command:
        (Pdb) !c = 3
        Assignments go to the frame's local variables; use a global statement
        to assign a module variable instead:
        (Pdb) global counter; counter = 0
        """
        self.message(self._help_message_from_doc(self.help_exec.__doc__))

    def help_pdb(self):
        if __doc__ is None:
            self.error("No help for 'pdb'")
            return
        self.message(__doc__.strip())

    # ---- breakpoints

    def defaultFile(self):
        """The file breakpoints refer to when none is given."""
        frame = self.curframe
        if frame is None:
            return self.mainpyfile
        filename = self._source_file(frame)
        if filename == '<string>' and self.mainpyfile:
            filename = self.mainpyfile
        return filename

    def _source_file(self, frame):
        filename = frame.f_code.co_filename
        if filename.startswith('<frozen ') and filename.endswith('>'):
            path = frame.f_globals.get('__file__')
            if isinstance(path, str) and os.path.exists(path):
                return path
        return filename

    def lookupmodule(self, filename):
        """Resolve a file or module name given to break/clear to a file
        name (None if not found)."""
        if os.path.isabs(filename) and os.path.exists(filename):
            return filename
        candidate = os.path.join(sys.path[0], filename)
        if os.path.exists(candidate) and self.canonic(candidate) == self.mainpyfile:
            return candidate
        root, ext = os.path.splitext(filename)
        names = [filename] if ext else [filename + '.py', filename]
        if os.path.isabs(names[0]):
            return names[0]
        for dirname in sys.path:
            if not isinstance(dirname, str):
                continue
            while os.path.islink(dirname):
                dirname = os.readlink(dirname)
            for name in names:
                full = os.path.join(dirname, name)
                if os.path.isfile(full):
                    return full
        if ext != '.py' and all(part.isidentifier() for part in filename.split('.')):
            import importlib.util
            try:
                spec = importlib.util.find_spec(filename)
            except Exception:
                spec = None
            if spec is not None and spec.origin and os.path.exists(spec.origin):
                return spec.origin
        return None

    def lineinfo(self, identifier):
        """(function name, file, line) of a dotted function name, or
        (None, None, None)."""
        failed = (None, None, None)
        pieces = identifier.split("'")
        if len(pieces) == 1:
            name = pieces[0].strip()
        elif len(pieces) == 3:
            name = pieces[1].strip()
        else:
            return failed
        if not name:
            return failed
        parts = name.split('.')
        if parts[0] == 'self':
            del parts[0]
            if not parts:
                return failed
        fname = self.defaultFile()
        if len(parts) == 1:
            item = parts[0]
        elif len(parts) == 2:
            found = self.lookupmodule(parts[0])
            if not found:
                return failed
            fname = found
            item = parts[1]
        else:
            return failed
        return find_function(item, self.canonic(fname)) or failed

    def checkline(self, filename, lineno, module_globals=None):
        """`lineno` if a breakpoint there makes sense, else 0 (blank line,
        comment, docstring or beyond the end of the file)."""
        frame = getattr(self, 'curframe', None)
        if module_globals is None and frame is not None:
            module_globals = frame.f_globals
        line = linecache.getline(filename, lineno, module_globals)
        if not line:
            self.message('Line %d is past the end of the file' % lineno)
            return 0
        line = line.strip()
        if not line or line[0] == '#' or line[:3] in ('"""', "'''"):
            self.error('Line %d holds no code (blank, comment or string)' % lineno)
            return 0
        return lineno

    def do_break(self, arg, temporary=False):
        """b(reak) [ ([filename:]lineno | function) [, condition] ]

        Without argument, list all breakpoints.  With a line number, set a
        breakpoint at that line of the current file (or of the given file,
        which is looked up along sys.path, and may also be a module name).
        With a function name, set it at the first line of the function's
        body.  A condition is a Python expression: the breakpoint only stops
        when it is true.  Each breakpoint gets a number, which the other
        breakpoint commands use.
        """
        if not arg:
            if self.breaks:
                self.message("Num Type         Disp Enb   Where")
                for bp in bdb.Breakpoint.bpbynumber:
                    if bp:
                        self.message(bp.bpformat())
            return
        filename = None
        lineno = None
        cond = None
        funcname = None
        module_globals = None
        comma = arg.find(',')
        if comma > 0:
            cond = arg[comma + 1:].lstrip()
            err = self._compile_error_message(cond)
            if err:
                self.error('Invalid condition %s: %s' % (cond, err))
                return
            arg = arg[:comma].rstrip()
        colon = arg.rfind(':')
        if colon > 0:
            filename = arg[:colon].rstrip()
            found = self.lookupmodule(filename)
            if not found:
                self.error('Cannot find %r (neither a file nor a module on sys.path)'
                           % filename)
                return
            filename = found
            arg = arg[colon + 1:].lstrip()
            try:
                lineno = int(arg)
            except ValueError:
                self.error('Line number expected, got %r' % arg)
                return
        else:
            try:
                lineno = int(arg)
            except ValueError:
                try:
                    func = self._eval(arg)
                except Exception:
                    func = None
                func = getattr(func, '__func__', func)
                code = getattr(func, '__code__', None)
                if isinstance(code, types.CodeType):
                    funcname = code.co_name
                    lineno = _first_body_line(code)
                    filename = code.co_filename
                    module_globals = getattr(func, '__globals__', None)
                else:
                    ok, found, line = self.lineinfo(arg)
                    if not ok:
                        self.error('The specified object %r is not a function '
                                   'or was not found along sys.path.' % arg)
                        return
                    funcname = ok
                    filename = found
                    lineno = int(line)
        if not filename:
            filename = self.defaultFile()
        filename = self.canonic(filename)
        line = self.checkline(filename, lineno, module_globals)
        if not line:
            return
        err = self.set_break(filename, line, temporary, cond, funcname)
        if err:
            self.error(err)
            return
        bp = self.get_breaks(filename, line)[-1]
        self.message("Breakpoint %d at %s:%d" % (bp.number, bp.file, bp.line))

    do_b = do_break

    def do_tbreak(self, arg):
        """tbreak [ ([filename:]lineno | function) [, condition] ]

        Like "break", but the breakpoint is deleted when it is first hit.
        """
        self.do_break(arg, True)

    def do_enable(self, arg):
        """enable bpnumber [bpnumber ...]

        Enable the breakpoints with the given numbers.
        """
        if not arg:
            self._print_arg_required()
            return
        for number in arg.split():
            try:
                bp = self.get_bpbynumber(number)
            except ValueError as err:
                self.error(err)
            else:
                bp.enable()
                self.message('Enabled %s' % bp)

    def do_disable(self, arg):
        """disable bpnumber [bpnumber ...]

        Disable the breakpoints with the given numbers.  A disabled
        breakpoint stays defined (its hit and ignore counts are kept) but
        does not stop the program until it is enabled again.
        """
        if not arg:
            self._print_arg_required()
            return
        for number in arg.split():
            try:
                bp = self.get_bpbynumber(number)
            except ValueError as err:
                self.error(err)
            else:
                bp.disable()
                self.message('Disabled %s' % bp)

    def do_condition(self, arg):
        """condition bpnumber [condition]

        Make the breakpoint stop only when the condition (a Python
        expression) is true; without a condition the breakpoint becomes
        unconditional.
        """
        if not arg:
            self._print_arg_required()
            return
        number, _, cond = arg.partition(' ')
        cond = cond.strip() or None
        if cond is not None:
            err = self._compile_error_message(cond)
            if err:
                self.error('Invalid condition %s: %s' % (cond, err))
                return
        try:
            bp = self.get_bpbynumber(number.strip())
        except ValueError as err:
            self.error(err)
            return
        bp.cond = cond
        if cond is None:
            self.message('Breakpoint %d is now unconditional.' % bp.number)
        else:
            self.message('New condition set for breakpoint %d.' % bp.number)

    def do_ignore(self, arg):
        """ignore bpnumber [count]

        The breakpoint will not stop the next `count` times its condition
        holds (count 0: stop next time).
        """
        args = arg.split()
        if not args:
            self._print_arg_required()
            return
        count = 0
        if len(args) > 1:
            try:
                count = int(args[1])
            except ValueError:
                self._print_invalid_arg(arg)
                return
        try:
            bp = self.get_bpbynumber(args[0])
        except ValueError as err:
            self.error(err)
            return
        bp.ignore = count
        if count > 0:
            what = '%d crossings' % count if count > 1 else '1 crossing'
            self.message('Will ignore next %s of breakpoint %d.' % (what, bp.number))
        else:
            self.message('Breakpoint %d will stop when it is next reached.' % bp.number)

    def _confirm(self, prompt):
        try:
            if self.use_rawinput:
                return input(prompt)
            return self._read_line(prompt, state='confirm')
        except EOFError:
            return ''

    def do_clear(self, arg):
        """cl(ear) [filename:lineno | bpnumber ...]

        With breakpoint numbers, delete those breakpoints; with
        filename:lineno, delete the breakpoints at that line.  Without
        argument, delete all breakpoints (after confirmation).
        """
        if not arg:
            reply = self._confirm('Clear all breaks? ').strip().lower()
            if reply in ('y', 'yes'):
                deleted = [bp for bp in bdb.Breakpoint.bpbynumber if bp]
                self.clear_all_breaks()
                for bp in deleted:
                    self.message('Deleted %s' % bp)
            return
        if ':' in arg:
            colon = arg.rfind(':')
            filename, number = arg[:colon], arg[colon + 1:]
            try:
                lineno = int(number)
            except ValueError:
                err = 'Invalid line number (%s)' % number
            else:
                deleted = list(self.get_breaks(filename, lineno))
                err = self.clear_break(filename, lineno)
            if err:
                self.error(err)
            else:
                for bp in deleted:
                    self.message('Deleted %s' % bp)
            return
        for number in arg.split():
            try:
                bp = self.get_bpbynumber(number)
            except ValueError as err:
                self.error(err)
            else:
                self.clear_bpbynumber(number)
                self.message('Deleted %s' % bp)

    do_cl = do_clear

    def _last_breakpoint_number(self):
        for bp in reversed(bdb.Breakpoint.bpbynumber):
            if bp:
                return bp.number
        return None

    def _next_command_def_line(self):
        if self.cmdqueue:
            line = self.cmdqueue.pop(0)
        else:
            try:
                line = self._read_line('(com) ', state='commands')
            except EOFError:
                line = 'EOF'
        first, rest = self._split_commands(line)
        if rest is not None:
            self.cmdqueue.insert(0, rest)
        return first

    def do_commands(self, arg):
        """(Pdb) commands [bpnumber]
        (com) ...
        (com) end
        (Pdb)

        Give a list of commands to run when the breakpoint (by default the
        last one set) is hit; enter them one per line and finish with
        "end".  The command "silent" suppresses the usual display of the
        location.  A command that resumes execution (continue, step, next,
        return, until, jump, quit) ends the list and runs last.  "commands"
        followed directly by "end" removes the commands.
        """
        if not arg:
            number = self._last_breakpoint_number()
            if number is None:
                self.error('cannot set commands: no existing breakpoint')
                return
        else:
            try:
                number = int(arg)
            except ValueError:
                self._print_invalid_arg(arg)
                return
        try:
            self.get_bpbynumber(number)
        except ValueError as err:
            self.error('cannot set commands: %s' % err)
            return
        self.commands_bnum = number
        self.commands_defining = True
        collected = []
        try:
            while True:
                line = self._next_command_def_line()
                command, _, _ = self.parseline(line)
                if command == 'end':
                    break
                if command == 'EOF' or line == 'EOF':
                    self.message('')
                    break
                if command == 'silent':
                    collected.append(_SILENCE)
                    continue
                collected.append(line)
                handler = getattr(self, 'do_' + command, None) if command else None
                if handler is not None and getattr(handler, '__name__', '') in self._resuming_commands:
                    break
        except KeyboardInterrupt:
            self.commands_defining = False
            self.commands_bnum = None
            raise
        self.commands_defining = False
        self.commands_bnum = None
        self.commands[number] = collected

    # ---- stack navigation

    def print_stack_entry(self, frame_lineno, prompt_prefix=None):
        if prompt_prefix is None:
            prompt_prefix = line_prefix
        frame, lineno = frame_lineno
        marker = '> ' if frame is self.curframe else '  '
        self.message(marker + self.format_stack_entry(frame_lineno, prompt_prefix))

    def format_stack_entry(self, frame_lineno, lprefix=': '):
        if not self.colorize:
            return super().format_stack_entry(frame_lineno, lprefix)
        import reprlib
        frame, lineno = frame_lineno
        filename = self.canonic(frame.f_code.co_filename)
        text = '%s(%r)%s()' % (filename, lineno, frame.f_code.co_name or '<lambda>')
        if '__return__' in frame.f_locals:
            text += '->' + reprlib.repr(frame.f_locals['__return__'])
        if lineno is not None:
            line = linecache.getline(filename, lineno, frame.f_globals)
            if line:
                text += lprefix + self._colorize_code(line.strip())
        return text

    def print_stack_trace(self, count=None):
        if count is None:
            entries = self.stack
        elif count == 0:
            entries = [self.stack[self.curindex]]
        elif count < 0:
            entries = self.stack[:-count]
        else:
            entries = self.stack[-count:]
        try:
            for entry in entries:
                self.print_stack_entry(entry)
        except KeyboardInterrupt:
            pass

    def do_where(self, arg):
        """w(here) [count]

        Print the stack, most recent frame last; an arrow marks the current
        frame.  With a count, only that many of the most recent frames are
        shown (a negative count shows the oldest ones, 0 the current one).
        """
        if not arg:
            count = None
        else:
            try:
                count = int(arg)
            except ValueError:
                self.error('Invalid count (%s)' % arg)
                return
        self.print_stack_trace(count)

    do_w = do_where
    do_bt = do_where

    def _select_frame(self, number):
        assert 0 <= number < len(self.stack)
        self.curindex = number
        self.curframe = self.stack[number][0]
        self.set_convenience_variable(self.curframe, '_frame', self.curframe)
        self.print_stack_entry(self.stack[number])
        self.lineno = None

    def do_up(self, arg):
        """u(p) [count]

        Move the current frame count (default 1) levels up the stack (to
        older frames).
        """
        if self.curindex == 0:
            self.error('Already at the oldest frame')
            return
        try:
            count = int(arg or 1)
        except ValueError:
            self.error('Invalid frame count (%s)' % arg)
            return
        target = 0 if count < 0 else max(0, self.curindex - count)
        self._select_frame(target)

    do_u = do_up

    def do_down(self, arg):
        """d(own) [count]

        Move the current frame count (default 1) levels down the stack (to
        newer frames).
        """
        if self.curindex + 1 == len(self.stack):
            self.error('Already at the newest frame')
            return
        try:
            count = int(arg or 1)
        except ValueError:
            self.error('Invalid frame count (%s)' % arg)
            return
        if count < 0:
            target = len(self.stack) - 1
        else:
            target = min(len(self.stack) - 1, self.curindex + count)
        self._select_frame(target)

    do_d = do_down

    def do_exceptions(self, arg):
        """exceptions [number]

        Without argument, list the chain of exceptions being examined
        (post-mortem); with a number, examine that exception instead.
        """
        if not self._chained_exceptions:
            self.error('There is no chain of exceptions to examine (post-mortem '
                       'debugging of an exception object provides one).')
            return
        if not arg:
            for index, exc in enumerate(self._chained_exceptions):
                marker = '>' if index == self._chained_exception_index else ' '
                text = repr(exc)
                if len(text) > 80:
                    text = text[:77] + '...'
                label = '%3d' % index if exc.__traceback__ is not None else '  -'
                self.message('%s %s %s' % (marker, label, text))
            return
        try:
            number = int(arg)
        except ValueError:
            self.error('Argument must be an integer')
            return
        if not 0 <= number < len(self._chained_exceptions):
            self.error('No exception with that number')
            return
        exc = self._chained_exceptions[number]
        if exc.__traceback__ is None:
            self.error('This exception does not have a traceback, cannot jump to it')
            return
        self._chained_exception_index = number
        self.setup(None, exc.__traceback__)
        self.print_stack_entry(self.stack[self.curindex])

    # ---- execution control

    def do_step(self, arg):
        """s(tep)

        Execute the current line and stop at the first possible occasion
        (in a function that is called or on the next line).
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_step()
        return 1

    do_s = do_step

    def do_next(self, arg):
        """n(ext)

        Continue until the next line of the current function is reached
        or the function returns.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_next(self.curframe)
        return 1

    do_n = do_next

    def do_until(self, arg):
        """unt(il) [lineno]

        Without argument, continue until a line greater than the current
        one is reached (to leave a loop).  With a line number, continue
        until a line at least that large is reached.  Either way, also stop
        when the current frame returns.
        """
        if arg:
            try:
                lineno = int(arg)
            except ValueError:
                self.error('Cannot use argument %r' % arg)
                return
            if lineno <= self.curframe.f_lineno:
                self.error('"until" needs a line number after the current one')
                return
        else:
            lineno = None
        self.set_until(self.curframe, lineno)
        return 1

    do_unt = do_until

    def do_return(self, arg):
        """r(eturn)

        Continue until the current function returns.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_return(self.curframe)
        return 1

    do_r = do_return

    def do_continue(self, arg):
        """c(ont(inue))

        Continue until a breakpoint is reached.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        self._install_sigint_handler()
        self.set_continue()
        return 1

    do_c = do_cont = do_continue

    def do_jump(self, arg):
        """j(ump) lineno

        Make the given line the next one to run (bottom frame only).  It
        is possible to jump back to run code again, or forward to skip it;
        jumping into a block (a loop body, an except clause...) is not.
        """
        if not arg:
            self._print_arg_required()
            return
        if self.curindex + 1 != len(self.stack):
            self.error('Jumps are only possible in the newest frame')
            return
        try:
            lineno = int(arg)
        except ValueError:
            self.error("'jump' takes a line number")
            return
        try:
            self.curframe.f_lineno = lineno
        except ValueError as err:
            self.error('Cannot jump there: %s' % err)
            return
        self.stack[self.curindex] = self.stack[self.curindex][0], lineno
        self.print_stack_entry(self.stack[self.curindex])

    do_j = do_jump

    def do_debug(self, arg):
        """debug code

        Run the code (a statement or expression) under a new debugger,
        in the context of the current frame.
        """
        if not arg:
            self._print_arg_required()
            return
        self.stop_trace()
        frame = self.curframe
        nested = Pdb(self.completekey, self.stdin, self.stdout,
                     readrc=False, backend=self.backend)
        nested.prompt = '(%s) ' % self.prompt.strip()
        nested.use_rawinput = self.use_rawinput
        self.message('ENTERING RECURSIVE DEBUGGER')
        try:
            sys.call_tracing(nested.run, (self._replace_convenience_variables(arg),
                                          frame.f_globals, frame.f_locals))
        except Exception:
            self._error_exc()
        self.message('LEAVING RECURSIVE DEBUGGER')
        self.start_trace()
        self.lastcmd = nested.lastcmd

    def do_run(self, arg):
        """run [args...]

        Restart the debugged program (only when pdb runs it, as in
        "python -m pdb script.py").  With arguments, they replace sys.argv[1:].
        Breakpoints and other debugger settings are kept.
        """
        if self.mode == 'inline':
            self.error('run/restart command is disabled when pdb is running in inline mode.\n'
                       'Use the command line interface to enable restarting your program\n'
                       'e.g. "python -m pdb myscript.py"')
            return
        if arg:
            import shlex
            program = sys.argv[:1]
            try:
                new_args = shlex.split(arg)
            except ValueError as err:
                self.error('Cannot run %s: %s' % (arg, err))
                return
            sys.argv = program + new_args
        raise Restart

    do_restart = do_run

    def do_quit(self, arg):
        """q(uit) | exit

        Leave the debugger; the program being debugged is aborted.
        """
        if self.mode == 'inline' and not hasattr(sys, 'ps1'):
            while True:
                try:
                    reply = self._read_line('Quit anyway (the program stops)? y/n: ', state='confirm')
                    reply = reply.strip().lower()
                except EOFError:
                    reply = 'y'
                    self.message('')
                if reply in ('y', ''):
                    sys.exit(1)
                if reply == 'n':
                    return
        self._user_requested_quit = True
        self.set_quit()
        return 1

    do_q = do_quit
    do_exit = do_quit

    def do_EOF(self, arg):
        """EOF

        End of input (Ctrl-D): quit like "quit".
        """
        self.message('')
        return self.do_quit(arg)

    # ---- looking at data

    def do_args(self, arg):
        """a(rgs)

        Print the arguments of the current function and their values.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        code = self.curframe.f_code
        values = self.curframe.f_locals
        count = code.co_argcount + code.co_kwonlyargcount
        if code.co_flags & inspect.CO_VARARGS:
            count += 1
        if code.co_flags & inspect.CO_VARKEYWORDS:
            count += 1
        for name in code.co_varnames[:count]:
            if name in values:
                self.message('%s = %s' % (name, self._safe_repr(values[name], name)))
            else:
                self.message('%s = *** undefined ***' % (name,))

    do_a = do_args

    def do_retval(self, arg):
        """retval

        Print the value the current function is returning.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        values = self.curframe.f_locals
        if '__return__' in values:
            self.message(self._safe_repr(values['__return__'], 'retval'))
        else:
            self.error('The function has not returned yet')

    do_rv = do_retval

    def _show_value(self, arg, formatter):
        try:
            value = self._getval(arg)
        except:
            return
        try:
            self.message(formatter(value))
        except:
            self._error_exc()

    def do_p(self, arg):
        """p expression

        Print the value of the expression.
        """
        if not arg:
            self._print_arg_required()
            return
        self._show_value(arg, repr)

    def do_pp(self, arg):
        """pp expression

        Pretty-print the value of the expression.
        """
        if not arg:
            self._print_arg_required()
            return
        self._show_value(arg, pprint.pformat)

    def do_whatis(self, arg):
        """whatis expression

        Print the type of the value of the expression.
        """
        if not arg:
            self._print_arg_required()
            return
        try:
            value = self._eval(arg)
        except:
            self._error_exc()
            return
        func = getattr(value, '__func__', None)
        if func is not None and hasattr(func, '__code__'):
            self.message('Method %s' % func.__code__.co_name)
            return
        code = getattr(value, '__code__', None)
        if isinstance(code, types.CodeType):
            self.message('Function %s' % code.co_name)
            return
        if isinstance(value, type):
            self.message('Class %s.%s' % (value.__module__, value.__qualname__))
            return
        self.message(type(value))

    def do_display(self, arg):
        """display [expression]

        Show the value of the expression now and, at each stop in the
        current frame, again whenever it changed.  Without argument, list
        the expressions displayed in the current frame.
        """
        if not arg:
            shown = self.displaying.get(self.curframe)
            if shown:
                self.message('Currently displaying:')
                for expr, value in shown.items():
                    self.message('%s: %s' % (expr, self._safe_repr(value, expr)))
            else:
                self.message('No expression is being displayed')
            return
        err = self._compile_error_message(arg)
        if err:
            self.error('Unable to display %s: %s' % (arg, err))
            return
        value = self._getval_except(arg)
        self.displaying.setdefault(self.curframe, {})[arg] = value
        self.message('display %s: %s' % (arg, self._safe_repr(value, arg)))

    def do_undisplay(self, arg):
        """undisplay [expression]

        Stop displaying the expression in the current frame (all of them
        without argument).
        """
        if arg:
            try:
                del self.displaying.get(self.curframe, {})[arg]
            except KeyError:
                self.error('%s is not being displayed' % arg)
        else:
            self.displaying.pop(self.curframe, None)

    def do_interact(self, arg):
        """interact

        Start an interactive Python interpreter whose namespace holds the
        current frame's globals and locals (assignments there do not change
        the frame).  Leave it with exit() or end of input.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        ns = {**self.curframe.f_globals, **self.curframe.f_locals}
        console = _PdbInteractiveConsole(ns, message=self.message)
        had_ps1 = hasattr(sys, 'ps1')
        saved_ps = getattr(sys, 'ps1', None), getattr(sys, 'ps2', None)
        try:
            with self._python_completion(ns):
                console.interact(banner='*pdb interact start*',
                                 exitmsg='*exit from pdb interact command*')
        except SystemExit:
            pass
        finally:
            if had_ps1:
                sys.ps1, sys.ps2 = saved_ps
            else:
                for name in ('ps1', 'ps2'):
                    try:
                        delattr(sys, name)
                    except AttributeError:
                        pass

    def do_alias(self, arg):
        """alias [name [command]]

        Make `name` stand for `command`.  In the command, %1, %2, ...
        are replaced by the arguments given to the alias, %* by all of
        them.  Without command, show the alias; without argument, show all
        aliases.
        """
        args = arg.split()
        if not args:
            for name in sorted(self.aliases):
                self.message('%s = %s' % (name, self.aliases[name]))
            return
        name = args[0]
        if len(args) == 1:
            if name in self.aliases:
                self.message('%s = %s' % (name, self.aliases[name]))
            else:
                self.error("Unknown alias '%s'" % name)
            return
        if not name.isidentifier() and not all(c in self.identchars for c in name):
            self.error('Invalid alias name %r' % name)
            return
        text = arg[len(name):].strip()
        used = sorted({int(m) for m in re.findall(r'%(\d)', text)})
        if used and "%*" not in text and used != list(range(1, len(used) + 1)):
            self.error('Replaceable parameters must be consecutive')
            return
        self.aliases[name] = text

    def do_unalias(self, arg):
        """unalias name

        Delete the alias.
        """
        args = arg.split()
        if not args:
            self._print_arg_required()
            return
        if args[0] in self.aliases:
            del self.aliases[args[0]]

    # ---- source listing

    def _validate_file_mtime(self):
        frame = self.curframe
        if frame is None:
            return
        filename = frame.f_code.co_filename
        try:
            mtime = os.path.getmtime(filename)
        except (OSError, TypeError, ValueError):
            return
        known = self._file_mtime_table.setdefault(filename, mtime)
        if known != mtime:
            self.message('*** WARNING: %r was edited after the program started; '
                         'the code that runs is the old one' % filename)

    def _print_lines(self, lines, start, breaks=(), frame=None):
        if frame is not None:
            current = frame.f_lineno
            exc_line = self.tb_lineno.get(frame, -1)
        else:
            current = exc_line = -1
        for lineno, line in enumerate(lines, start):
            mark = 'B' if lineno in breaks else ' '
            if lineno == current:
                mark += '->'
            elif lineno == exc_line:
                mark += '>>'
            text = self._colorize_code(line.rstrip('\r\n').rstrip())
            self.message('%3d %s\t%s' % (lineno, mark, text))

    def _frame_source(self, frame):
        """(lines, first line number) of the code run by `frame`."""
        filename = self._source_file(frame)
        lines = linecache.getlines(filename, frame.f_globals)
        if not lines:
            raise OSError('source code not available')
        code = frame.f_code
        if code.co_name == '<module>' or frame.f_globals is frame.f_locals:
            return lines, 1
        first = code.co_firstlineno
        return inspect.getblock(lines[first - 1:]), first

    def do_list(self, arg):
        """l(ist) [first[, last] | .]

        List source code of the current file: without argument, eleven
        lines around the current line, or the next eleven lines if the
        previous command was also "list".  With "." list around the current
        line; with one number, around that line; with two numbers, that
        range (a second number smaller than the first is a count).
        Breakpoints are marked "B", the current line "->" and the line of
        the exception being examined ">>".
        """
        self.lastcmd = 'list'
        last = None
        if arg and arg != '.':
            try:
                if ',' in arg:
                    first, last = arg.split(',')
                    first = int(first.strip())
                    last = int(last.strip())
                    if last < first:
                        last = first + last
                else:
                    first = max(1, int(arg.strip()) - 5)
            except ValueError:
                self.error('Cannot use argument %r' % arg)
                return
        elif self.lineno is None or arg == '.':
            first = max(1, self.curframe.f_lineno - 5)
        else:
            first = self.lineno + 1
        if last is None:
            last = first + 10
        filename = self._source_file(self.curframe)
        self._validate_file_mtime()
        breaks = self.get_file_breaks(filename)
        try:
            lines = linecache.getlines(filename, self.curframe.f_globals)
            self._print_lines(lines[first - 1:last], first, breaks, self.curframe)
            self.lineno = min(last, len(lines))
            if len(lines) < last:
                self.message('[EOF]')
        except KeyboardInterrupt:
            pass

    do_l = do_list

    def do_longlist(self, arg):
        """ll | longlist

        List the whole source code of the current function or frame.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        filename = self._source_file(self.curframe)
        self._validate_file_mtime()
        breaks = self.get_file_breaks(filename)
        try:
            lines, first = self._frame_source(self.curframe)
        except OSError as err:
            self.error(err)
            return
        self._print_lines(lines, first, breaks, self.curframe)

    do_ll = do_longlist

    def do_source(self, arg):
        """source expression

        Show the source code of the object (a function, class, module...).
        """
        if not arg:
            self._print_arg_required()
            return
        try:
            obj = self._getval(arg)
        except:
            return
        try:
            lines, first = inspect.getsourcelines(obj)
        except (OSError, TypeError) as err:
            self.error(err)
            return
        self._print_lines(lines, first)

    # ---- completion

    def _command_names(self):
        return sorted({name[3:] for name in self.get_names()
                       if name.startswith('do_')} | set(self.aliases))

    def completenames(self, text, line, begidx, endidx):
        if self._continuing_block:
            # inside a multi-line statement: Python only; a tab before any
            # text indents to the next multiple of four columns
            if not text.strip():
                typed = self._readline_line_before_cursor()
                return [' ' * (4 - len(typed) % 4)]
            return self._complete_expression(text, line, begidx, endidx)
        commands = [name for name in self._command_names() if name.startswith(text)]
        if commands:
            return commands
        return self._complete_expression(text, line, begidx, endidx)

    @staticmethod
    def _readline_line_before_cursor():
        try:
            import readline
            return readline.get_line_buffer()[:readline.get_endidx()]
        except Exception:
            return ''

    def _completion_namespace(self):
        if self.curframe is None:
            return {}
        return {**self.curframe.f_globals, **self.curframe.f_locals}

    def _complete_expression(self, text, line, begidx, endidx):
        """Names (or attributes, after a dot) of the current frame; builtins
        and keywords only when nothing else matches."""
        if self.curframe is None:
            return []
        ns = self._completion_namespace()
        store = self.curframe.f_globals.get('__pdb_convenience_variables', {})
        if '.' in text:
            parts = text.split('.')
            head = parts[0]
            try:
                if head.startswith('$'):
                    obj = store[head[1:]]
                else:
                    obj = ns[head]
                for part in parts[1:-1]:
                    obj = getattr(obj, part)
            except (KeyError, AttributeError):
                return []
            prefix = '.'.join(parts[:-1]) + '.'
            return [prefix + name for name in dir(obj) if name.startswith(parts[-1])]
        if text.startswith('$'):
            return ['$' + name for name in store if name.startswith(text[1:])]
        names = [name for name in ns if isinstance(name, str) and name.startswith(text)]
        if names:
            return names
        import rlcompleter
        completer = rlcompleter.Completer(ns)
        matches = []
        state = 0
        while True:
            try:
                match = completer.complete(text, state)
            except Exception:
                break
            if match is None:
                break
            matches.append(match)
            state += 1
        return matches

    def completedefault(self, text, line, begidx, endidx):
        return self._complete_expression(text, line, begidx, endidx)

    def _complete_location(self, text, line, begidx, endidx):
        if re.match(r'.*,\s*', line):
            return self._complete_expression(text, line, begidx, endidx)
        matches = self._complete_expression(text, line, begidx, endidx)
        if not text or os.sep in text or text.endswith('.py'):
            matches += [path + (os.sep if os.path.isdir(path) else '')
                        for path in glob.glob(glob.escape(text) + '*')]
        return matches

    def _complete_bpnumber(self, text, line, begidx, endidx):
        return [str(i) for i, bp in enumerate(bdb.Breakpoint.bpbynumber)
                if bp is not None and str(i).startswith(text)]

    complete_print = complete_p = complete_pp = complete_whatis = \
        complete_display = complete_source = complete_debug = _complete_expression
    complete_break = complete_b = complete_tbreak = complete_clear = \
        complete_cl = _complete_location
    complete_enable = complete_disable = complete_condition = \
        complete_ignore = complete_commands = _complete_bpnumber

    def complete_undisplay(self, text, line, begidx, endidx):
        shown = self.displaying.get(self.curframe, {})
        return [expr for expr in shown if expr.startswith(text)]

    def complete_unalias(self, text, line, begidx, endidx):
        return [name for name in self.aliases if name.startswith(text)]

    @contextmanager
    def _block_editing(self, lines):
        """While the rest of a multi-line statement is read: indent each
        new line like the previous one (one level more after a colon)."""
        self._continuing_block = True
        readline = None
        if self._readline_active:
            try:
                import readline
            except ImportError:
                readline = None
        if readline is not None:
            def indent():
                last = lines[-1] if lines else ''
                if not last.strip():
                    return
                width = len(last) - len(last.lstrip())
                if last.rstrip().endswith(':'):
                    width += 4
                if width:
                    readline.insert_text(' ' * width)
                    readline.redisplay()
            readline.set_pre_input_hook(indent)
        try:
            yield
        finally:
            self._continuing_block = False
            if readline is not None:
                readline.set_pre_input_hook(None)

    @contextmanager
    def _python_completion(self, ns):
        """Complete Python names of `ns` (and nothing else) in readline."""
        if not self._readline_active:
            yield
            return
        import readline
        import rlcompleter
        previous = readline.get_completer()
        readline.set_completer(rlcompleter.Completer(ns).complete)
        try:
            yield
        finally:
            readline.set_completer(previous)

    # ---- running programs

    def _run(self, target):
        self._wait_for_mainpyfile = True
        self._main_frame_started = False
        self._user_requested_quit = False
        self.mainpyfile = self.canonic(target.filename)
        import __main__
        __main__.__dict__.clear()
        __main__.__dict__.update(target.namespace)
        # a restarted program is up to date again
        self._file_mtime_table.clear()
        self.run(target.code)


class PdbPyReplInput:
    """Line input through _pyrepl, with syntax colouring of commands."""

    def __init__(self, pdb_instance, colorize):
        self.pdb = pdb_instance
        self.colorize = colorize

    def gen_colors(self, buffer):
        from _pyrepl.utils import gen_colors, ColorSpan, Span
        spans = list(gen_colors(buffer))
        stripped = buffer.lstrip()
        start = len(buffer) - len(stripped)
        word = re.match(r'\w+', stripped)
        if word and not buffer[start:].startswith('!'):
            name = word.group()
            end = start + len(name)
            following = buffer[end:end + 1]
            commands = self.pdb._command_names()
            if name in commands and (not following or following.isspace() or
                                     following not in '(.[='):
                spans = [s for s in spans if not (s.span.start >= start and
                                                  s.span.end < end)]
                spans.insert(0, ColorSpan(Span(start, end - 1), 'soft_keyword'))
        return iter(spans)

    def readline(self, prompt):
        try:
            from _pyrepl.readline import multiline_input
        except ImportError:
            return input(prompt)

        def more_lines(text):
            # a Python statement that is not complete yet goes on
            first = text.split('\n', 1)[0].strip()
            word = re.match(r'\w+', first)
            if word and word.group() in self.pdb._command_names() and \
                    first[len(word.group()):len(word.group()) + 1] in ('', ' ', '\t'):
                return False
            try:
                return codeop.compile_command(text.lstrip('!') + '\n', '<stdin>',
                                              'single') is None
            except (SyntaxError, ValueError, OverflowError):
                return False

        continuation = '...'.ljust(len(prompt))
        return multiline_input(more_lines, prompt, continuation)

    def __repr__(self):
        return '<PdbPyReplInput>'


# ---------------------------------------------------------------------------
# remote debugging
#
# A debugged process runs a _PdbServer, which talks to a _PdbClient (in the
# process the user types into) over a socket.  Every message is one line of
# JSON.  The server sends:
#     {"message": text, "type": "info" | "error"}   output to show
#     {"help": topic}                                show help (client side)
#     {"command_list": [names]}                      the debugger commands
#     {"prompt": text, "state": state}               read a reply; state is
#                                                    "pdb", "interact",
#                                                    "commands", "confirm"...
#     {"completions": [items]}                       answer to "complete"
# and the client answers with:
#     {"reply": text}
#     {"signal": "INT" | "EOF"}
#     {"complete": {"text", "line", "begidx", "endidx"}}


class _SocketOutput:
    """File-like object: what is written goes to the client as messages."""

    def __init__(self, server):
        self._server = server

    def write(self, text):
        if text:
            self._server.message(text, end='')
        return len(text)

    def flush(self):
        pass

    def isatty(self):
        return False


class _PdbServer(Pdb):
    """The debugger of a process debugged from another one."""

    def __init__(self, sockfile, signal_server=None, owns_sockfile=True,
                 colorize=False, **kwargs):
        self._sockfile = sockfile
        self._owns_sockfile = owns_sockfile
        self._signal_server = signal_server
        self._write_failed = False
        self._interact_state = None
        kwargs.setdefault('readrc', False)
        super().__init__(stdout=_SocketOutput(self), colorize=False, **kwargs)
        # the client decides whether its terminal shows colours
        self.colorize = bool(colorize)
        self.pyrepl_input = None
        self.prompt = '(Pdb) '

    @staticmethod
    def protocol_version():
        """A number both sides must agree on: the Python version
        (major, minor) plus a revision of the message format."""
        major, minor = sys.version_info[:2]
        revision = 0
        return (major << 24) | (minor << 16) | (revision << 8) | 0xF0

    # -- transport

    def _send(self, **payload):
        if self._write_failed:
            return
        data = json.dumps(payload).encode() + b'\n'
        try:
            self._sockfile.write(data)
            self._sockfile.flush()
        except OSError:
            # the client went away: the next read reports end of input
            self._write_failed = True

    def message(self, msg, end='\n'):
        self._send(message=str(msg) + end, type='info')

    def error(self, msg):
        self._send(message=str(msg), type='error')

    def _read_reply(self):
        """The next reply of the client (answering completion requests
        on the way); EOFError / KeyboardInterrupt for its signals."""
        while True:
            if self._write_failed:
                raise EOFError
            line = self._sockfile.readline()
            if not line:
                raise EOFError
            try:
                payload = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                self.error('Invalid JSON from the client: %r' % (line,))
                continue
            if not isinstance(payload, dict):
                continue
            if 'reply' in payload:
                return payload['reply']
            if 'signal' in payload:
                if payload['signal'] == 'INT':
                    raise KeyboardInterrupt
                if payload['signal'] == 'EOF':
                    raise EOFError
                continue
            if 'complete' in payload:
                request = payload['complete']
                try:
                    items = self._complete_request(request['text'], request['line'],
                                                   request['begidx'], request['endidx'])
                except Exception:
                    items = []
                self._send(completions=list(items))
                continue

    def _complete_request(self, text, line, begidx, endidx):
        if self._interact_state is not None:
            import rlcompleter
            completer = rlcompleter.Completer(self._interact_state['ns'])
            found = []
            while (item := completer.complete(text, len(found))) is not None:
                found.append(item)
            return found
        if begidx == 0:
            return self.completenames(text, line, begidx, endidx)
        command = self.parseline(line)[0]
        func = getattr(self, 'complete_' + command, None) if command else None
        if func is None:
            func = self.completedefault
        return func(text, line, begidx, endidx)

    def _read_line(self, prompt, state='pdb'):
        self._send(prompt=prompt, state=state)
        return self._read_reply()

    def _confirm(self, prompt):
        try:
            return self._read_line(prompt, state='confirm')
        except EOFError:
            return ''

    # -- the command loop

    def cmdloop(self, intro=None):
        self._send(command_list=self._command_names())
        return super().cmdloop(intro)

    def _read_command(self):
        while self._interact_state is not None:
            try:
                code = self._read_line('>>> ', state='interact')
            except EOFError:
                self._interact_state = None
                self.message('*exit from pdb interact command*')
                break
            except KeyboardInterrupt:
                self.message('\nKeyboardInterrupt')
                continue
            self._run_in_python_repl(code)
        return super()._read_command()

    def _run_in_python_repl(self, lines):
        """Run code typed at the remote 'interact' prompt."""
        state = self._interact_state
        captured = io.StringIO()
        saved = sys.stdout, sys.displayhook

        def show(value):
            if value is not None:
                captured.write(repr(value) + '\n')
        try:
            code_obj = state['compiler'](lines + '\n', '<console>', 'single')
            if code_obj is None:
                raise SyntaxError('incomplete input')
            sys.stdout, sys.displayhook = captured, show
            try:
                exec(code_obj, state['ns'])
            finally:
                sys.stdout, sys.displayhook = saved
        except SystemExit:
            self._interact_state = None
            self.message('*exit from pdb interact command*')
        except:
            self._error_exc()
        output = captured.getvalue()
        if output:
            self.message(output, end='')

    # -- commands that behave differently remotely

    def do_help(self, arg):
        """h(elp) [command]

        Without argument, list the available commands.  With a command
        name, show its documentation.
        """
        self._send(help=arg)

    do_h = do_help

    def do_interact(self, arg):
        """interact

        Start an interactive Python interpreter whose namespace holds the
        current frame's globals and locals.
        """
        if arg:
            self._print_invalid_arg(arg)
            return
        self._interact_state = dict(
            compiler=codeop.CommandCompiler(),
            ns={**self.curframe.f_globals, **self.curframe.f_locals},
        )
        self.message('*pdb interact start*')

    def do_run(self, arg):
        """run [args...]

        Not available when debugging another process.
        """
        self.error('remote PDB cannot restart the program')

    do_restart = do_run

    def _detach_and_continue(self):
        self.detach()
        self.clear_all_breaks()
        self.set_continue()
        return 1

    def do_quit(self, arg):
        """q(uit) | exit

        Detach from the process, which goes on running.
        """
        return self._detach_and_continue()

    do_q = do_quit
    do_exit = do_quit

    def do_EOF(self, arg):
        """EOF

        The client went away: detach.
        """
        self.message('')
        return self._detach_and_continue()

    def detach(self):
        """Close the connection; the program is not aborted."""
        self.quitting = False
        if self._owns_sockfile:
            if Pdb._last_pdb_instance is self:
                Pdb._last_pdb_instance = None
            try:
                self._sockfile.close()
            except OSError:
                pass


class _PdbClient:
    """The user's side of a remote debugging session."""

    def __init__(self, pid, server_socket, interrupt_sock):
        self.pid = pid
        self.server_socket = server_socket
        self.interrupt_sock = interrupt_sock
        self.read_buf = b''
        self.pdb_commands = set()
        self.completion_matches = []
        self.state = 'dumb'
        self.write_failed = False
        self._prompting = False
        self._continuing = False

    # -- transport

    def _send(self, **payload):
        data = json.dumps(payload).encode() + b'\n'
        try:
            self.server_socket.sendall(data)
        except OSError:
            self.write_failed = True

    def _read_message(self):
        """The next line from the server (bytes, with its newline), or
        b'' at the end."""
        while b'\n' not in self.read_buf:
            try:
                chunk = self.server_socket.recv(16 * 1024)
            except OSError:
                chunk = b''
            if not chunk:
                rest, self.read_buf = self.read_buf, b''
                return rest
            self.read_buf += chunk
        line, _, self.read_buf = self.read_buf.partition(b'\n')
        return line + b'\n'

    def _interrupt_remote(self):
        if self.interrupt_sock is not None:
            self.interrupt_sock.sendall(signal.SIGINT.to_bytes())
        else:
            os.kill(self.pid, signal.SIGINT)

    def _sigint(self, signum, frame):
        if self._prompting:
            raise KeyboardInterrupt
        self._interrupt_remote()

    # -- the loop

    def cmdloop(self):
        try:
            previous = signal.signal(signal.SIGINT, self._sigint)
        except ValueError:
            previous = None
        try:
            with self._completion():
                while not self.write_failed:
                    line = self._read_message()
                    if not line:
                        break
                    try:
                        payload = json.loads(line)
                    except ValueError:
                        print('***', 'Invalid JSON from remote: %r' % (line,), flush=True)
                        continue
                    self._handle(payload)
        finally:
            if previous is not None:
                signal.signal(signal.SIGINT, previous)

    def _handle(self, payload):
        if not isinstance(payload, dict):
            raise RuntimeError('Unrecognized payload %r' % (json.dumps(payload).encode(),))
        if 'command_list' in payload:
            self.pdb_commands = set(payload['command_list'])
        elif 'message' in payload and 'type' in payload:
            if payload['type'] == 'error':
                print('***', payload['message'], flush=True)
            else:
                sys.stdout.write(payload['message'])
                sys.stdout.flush()
        elif 'help' in payload:
            helper = Pdb(nosigint=True, readrc=False)
            helper.do_help(payload['help'])
        elif 'prompt' in payload and 'state' in payload:
            self._prompt(payload['prompt'], payload['state'])
        else:
            raise RuntimeError('Unrecognized payload %r' % (json.dumps(payload).encode(),))

    def _input(self, prompt):
        while True:
            self._prompting = True
            try:
                return input(prompt)
            except (KeyboardInterrupt, EOFError):
                raise
            except Exception as exc:
                # e.g. a failure while completing: report it, ask again
                print('***', traceback.format_exception_only(exc)[-1].strip(),
                      flush=True)
            finally:
                self._prompting = False

    def _is_command(self, line):
        word = line.split(maxsplit=1)[0] if line.strip() else ''
        if not word:
            return True
        name = re.match(r'\w*', word).group()
        return name in self.pdb_commands and word == name

    def _prompt(self, prompt, state):
        self.state = state if state in ('pdb', 'interact') else 'dumb'
        try:
            reply = self._collect_reply(prompt)
        except KeyboardInterrupt:
            self._send(signal='INT')
            return
        except EOFError:
            self._send(signal='EOF')
            return
        self._send(reply=reply)

    def _collect_reply(self, prompt):
        while True:
            line = self._input(prompt)
            if self.state == 'dumb':
                return line
            prefix = ''
            if self.state == 'pdb':
                if line.startswith('!'):
                    prefix = '!'
                    line = line[1:].lstrip()
                elif self._is_command(line):
                    return line
            lines = [line]
            try:
                complete = codeop.compile_command(line + '\n', '<stdin>', 'single')
            except (SyntaxError, ValueError, OverflowError) as exc:
                print('***', traceback.format_exception_only(exc)[-1].strip(), flush=True)
                continue
            if complete is not None:
                return prefix + line
            continuation = '...'.ljust(len(prompt))
            self._continuing = True
            try:
                while True:
                    more = self._input(continuation)
                    lines.append(more)
                    try:
                        done = codeop.compile_command('\n'.join(lines), '<stdin>', 'single')
                    except (SyntaxError, ValueError, OverflowError):
                        done = True
                    if done is not None:
                        return prefix + '\n'.join(lines)
            finally:
                self._continuing = False

    # -- completion

    @contextmanager
    def _completion(self):
        try:
            import readline
        except ImportError:
            yield
            return
        if not hasattr(readline, 'set_completer'):
            yield
            return
        previous = readline.get_completer()
        readline.set_completer(self.complete)
        try:
            yield
        finally:
            readline.set_completer(previous)

    def complete(self, text, state):
        import readline
        if state == 0:
            self.completion_matches = []
            if self.state not in ('pdb', 'interact'):
                return None
            buffer = readline.get_line_buffer()
            begidx = readline.get_begidx()
            endidx = readline.get_endidx()
            stripped = len(buffer) - len(buffer.lstrip())
            line = buffer.lstrip()
            begidx -= stripped
            endidx -= stripped
            if self._continuing and self.state == 'pdb':
                # the rest of a statement: it is Python code
                line = '! ' + line
                begidx += 2
                endidx += 2
            self._send(complete={'text': text, 'line': line,
                                 'begidx': begidx, 'endidx': endidx})
            if self.write_failed:
                return None
            data = self._read_message()
            if not data:
                return None
            # a malformed answer is an error of the input in progress
            payload = json.loads(data)
            if not isinstance(payload, dict) or 'completions' not in payload:
                raise RuntimeError('Failed to get valid completions. Got: %s'
                                   % (payload,))
            self.completion_matches = payload['completions']
        if state < len(self.completion_matches):
            return self.completion_matches[state]
        return None


def _connect(*, host, port, frame, commands, version, signal_raising_thread,
             colorize):
    """Run by the debugged process (through sys.remote_exec): connect to
    the client and stop in `frame`."""
    import socket
    import weakref
    with socket.create_connection((host, port)) as conn:
        sockfile = conn.makefile('rwb')
    signal_server = None
    if signal_raising_thread:
        signal_server = socket.create_connection((host, port))
        _start_signal_thread(signal_server)
    remote = _PdbServer(sockfile, signal_server=signal_server, colorize=colorize)
    weakref.finalize(remote, sockfile.close)
    if Pdb._last_pdb_instance is not None:
        remote.error('A debugger is already attached to this process.')
    elif version != remote.protocol_version():
        remote.error(
            'The debugger and the process speak incompatible protocol versions '
            '(process: protocol version 0x%08X, debugger: protocol version 0x%08X); '
            'detaching.'
            % (remote.protocol_version(), version))
    else:
        remote.rcLines.extend(commands.splitlines())
        remote.set_trace(frame=frame)


def _start_signal_thread(sock):
    import threading

    def forward():
        try:
            while True:
                data = sock.recv(1024)
                if not data:
                    break
                for number in data:
                    signal.raise_signal(number)
        except OSError:
            pass
        finally:
            sock.close()

    threading.Thread(target=forward, daemon=True, name='pdb signals').start()


def attach(pid, commands=()):
    """Debug the running process `pid` (needs sys.remote_exec)."""
    import json as _json
    import socket
    import stat
    import tempfile
    from contextlib import ExitStack, closing
    with ExitStack() as stack:
        server = stack.enter_context(
            closing(socket.create_server(('localhost', 0))))
        port = server.getsockname()[1]
        script = stack.enter_context(
            tempfile.NamedTemporaryFile('w', suffix='.py', delete_on_close=False))
        use_signal_thread = sys.platform == 'win32'
        colorize = _colorize.can_colorize()
        script.write(textwrap.dedent(f"""\
            import pdb, sys
            pdb._connect(
                host='localhost',
                port={port},
                frame=sys._getframe(1),
                commands={_json.dumps(chr(10).join(commands))},
                version={_PdbServer.protocol_version()},
                signal_raising_thread={use_signal_thread!r},
                colorize={colorize!r},
            )
            """))
        script.close()
        mode = os.stat(script.name).st_mode
        os.chmod(script.name, mode | stat.S_IRGRP | stat.S_IROTH)
        sys.remote_exec(pid, script.name)
        client_sock, _ = server.accept()
        stack.enter_context(closing(client_sock))
        interrupt_sock = None
        if use_signal_thread:
            interrupt_sock, _ = server.accept()
            stack.enter_context(closing(interrupt_sock))
        _PdbClient(pid, client_sock, interrupt_sock).cmdloop()


# ---------------------------------------------------------------------------
# module-level entry points

def run(statement, globals=None, locals=None):
    """Run the statement (a string or code object) under the debugger."""
    Pdb().run(statement, globals, locals)


def runeval(expression, globals=None, locals=None):
    """Evaluate the expression under the debugger and return its value."""
    return Pdb().runeval(expression, globals, locals)


def runctx(statement, globals, locals):
    # kept for backwards compatibility
    run(statement, globals, locals)


def runcall(*args, **kwds):
    """Call the function (the first argument) under the debugger."""
    return Pdb().runcall(*args, **kwds)


def set_trace(*, header=None, commands=None):
    """Enter the debugger at the calling stack frame.

    `header` is printed first; `commands` are run as if typed at the
    prompt.  The same debugger instance is reused by later calls."""
    pdb = Pdb._last_pdb_instance
    if pdb is None:
        pdb = Pdb(mode='inline')
    if header is not None:
        pdb.message(header)
    pdb.set_trace(sys._getframe().f_back, commands=commands)


async def set_trace_async(*, header=None, commands=None):
    """The coroutine version of set_trace(): use "await set_trace_async()"."""
    pdb = Pdb._last_pdb_instance
    if pdb is None:
        pdb = Pdb(mode='inline')
    if header is not None:
        pdb.message(header)
    await pdb.set_trace_async(sys._getframe().f_back, commands=commands)


def _post_mortem(t, pdb_instance):
    if isinstance(t, BaseException):
        if t.__traceback__ is None:
            raise ValueError("A valid traceback must be passed if no "
                             "exception is being handled")
    elif t is None:
        raise ValueError("A valid traceback must be passed if no "
                         "exception is being handled")
    pdb_instance.reset()
    pdb_instance.interaction(None, t)


def post_mortem(t=None):
    """Examine a traceback or exception (default: the one being handled)."""
    if t is None:
        exc = sys.exception()
        if exc is not None:
            t = exc
    _post_mortem(t, Pdb())


def pm():
    """Examine the last uncaught exception (sys.last_exc)."""
    if hasattr(sys, 'last_exc'):
        target = sys.last_exc
    else:
        target = sys.last_traceback
    post_mortem(target)


TESTCMD = 'import x; x.main()'


def test():
    run(TESTCMD)


def help():
    """Print the pdb manual."""
    import pydoc
    pydoc.pager(__doc__)


_usage = """\
Debug a Python program: a script (pyfile), a module (-m), or a running
process (-p, by its pid).  The program stops before its first line;
type "help" at the prompt for the commands.

Commands are first read from .pdbrc files in your home and current
directories, then from -c options, as if they were typed at the prompt.
For instance, "-c continue" runs the program until an exception occurs,
and "-c 'until X'" runs it up to line X of the debugged file."""


def exit_with_permission_help_text():
    print("Error: The specified process cannot be attached to due to "
          "insufficient permissions.\n"
          "Attaching needs the right to inspect the other process (for "
          "instance the same user and a permissive ptrace policy, or "
          "elevated privileges).\n")
    sys.exit(1)


def _make_arg_parser():
    import argparse
    prog = 'pdb'
    main_module = sys.modules.get('__main__')
    if getattr(main_module, '__spec__', None) is not None and \
            main_module.__spec__.name == 'pdb':
        prog = os.path.basename(sys.executable) + ' -m pdb'
    return argparse.ArgumentParser(
        prog=prog,
        usage='%(prog)s [-h] [-c command] (-m module | -p pid | pyfile) [args ...]',
        description=_usage,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        allow_abbrev=False,
    )


def _parse_command_line(argv):
    """(commands, module, pid, pyfile, program arguments) from pdb's own
    command line; options end at the program to run."""
    parser = _make_arg_parser()
    parser.add_argument('-c', '--command', action='append', default=[],
                        metavar='command', dest='commands',
                        help='pdb commands to execute as if given in a .pdbrc file')
    parser.add_argument('-m', metavar='module', dest='module',
                        help='debug a module instead of a script')
    parser.add_argument('-p', '--pid', type=int, default=None,
                        help='attach to the specified PID')
    if not argv:
        parser.print_help()
        sys.exit(2)
    commands = []
    module = pid = pyfile = None
    unknown = []
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == '--':
            i += 1
            if i >= len(argv):
                parser.error('missing script or module to run')
            pyfile = argv[i]
            i += 1
            break
        if arg in ('-h', '--help'):
            parser.print_help()
            sys.exit(0)
        if arg in ('-c', '--command'):
            if i + 1 >= len(argv):
                parser.error('argument -c/--command: expected one argument')
            commands.append(argv[i + 1])
            i += 2
            continue
        if arg.startswith('--command='):
            commands.append(arg.partition('=')[2])
            i += 1
            continue
        if arg.startswith('-c') and len(arg) > 2:
            commands.append(arg[2:])
            i += 1
            continue
        if arg == '-m':
            if i + 1 >= len(argv):
                parser.error('argument -m: expected one argument')
            module = argv[i + 1]
            i += 2
            break
        if arg.startswith('-m') and len(arg) > 2:
            module = arg[2:]
            i += 1
            break
        if arg in ('-p', '--pid') or arg.startswith('--pid='):
            if '=' in arg:
                value = arg.partition('=')[2]
                i += 1
            else:
                if i + 1 >= len(argv):
                    parser.error('argument -p/--pid: expected one argument')
                value = argv[i + 1]
                i += 2
            try:
                pid = int(value)
            except ValueError:
                parser.error("argument -p/--pid: invalid int value: %r" % value)
            continue
        if arg.startswith('-') and arg != '-':
            unknown.append(arg)
            i += 1
            continue
        pyfile = arg
        i += 1
        break
    rest = argv[i:]
    if unknown:
        parser.error('unrecognized arguments: %s' % ' '.join(unknown))
    if pid is not None:
        if module or pyfile:
            parser.error('argument -p/--pid: not allowed with a module or script')
        return commands, None, pid, None, rest
    if module is None and pyfile is None:
        parser.error('missing script or module to run')
    return commands, module, None, pyfile, rest


def main():
    commands, module, pid, pyfile, rest = _parse_command_line(sys.argv[1:])
    if pid is not None:
        try:
            attach(pid, commands)
        except PermissionError:
            exit_with_permission_help_text()
        except (RuntimeError, ProcessLookupError, OSError):
            print('Cannot attach to pid %d, please make sure that the process '
                  'exists and runs the same Python version.' % pid)
            sys.exit(1)
        return
    if module is not None:
        target = _ModuleTarget(module)
        name = module
    else:
        import zipfile
        if os.path.isfile(pyfile) and zipfile.is_zipfile(pyfile):
            target = _ZipTarget(pyfile)
        else:
            target = _ScriptTarget(pyfile)
        name = pyfile
    sys.argv[:] = [name] + rest

    debugger = Pdb(mode='cli', backend='monitoring' if _default_backend == 'monitoring'
                   else None)
    debugger.rcLines.extend(commands)
    while True:
        try:
            debugger._run(target)
        except Restart:
            print('Restarting', target, 'with arguments:')
            print('\t' + ' '.join(sys.argv[1:]))
            continue
        except SystemExit as exc:
            print('The program called sys.exit(); exit status:', end=' ')
            print(exc)
        except BaseException as exc:
            traceback.print_exception(exc, colorize=_colorize.can_colorize())
            print('The program raised an exception: post-mortem debugging starts')
            print("('continue' or 'step' runs the program again)")
            try:
                debugger.interaction(None, exc)
            except Restart:
                print('Restarting', target, 'with arguments:')
                print('\t' + ' '.join(sys.argv[1:]))
                continue
            if debugger._user_requested_quit:
                break
            print('Leaving post-mortem debugging; %s starts again' % target)
            continue
        if debugger._user_requested_quit:
            break
        print('The program finished; it starts again')


# when invoked as a script (python -m pdb), the work is done by the module
# imported under its own name
if __name__ == '__main__':
    import pdb
    pdb.main()
