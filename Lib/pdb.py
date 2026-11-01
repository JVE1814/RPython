"""The Python debugger.

Use pdb.set_trace() (or the built-in breakpoint()) to enter the debugger,
pdb.run()/runeval()/runcall() to debug code from the start, pdb.pm() for a
post-mortem session, or 'python -m pdb script' from the command line.
Type 'help' at the (Pdb) prompt for the list of commands.
"""

import bdb
import cmd
import dis
import inspect
import linecache
import os
import pprint
import re
import reprlib
import sys
import tokenize
import traceback

__all__ = ["run", "pm", "Pdb", "runeval", "runctx", "runcall", "set_trace",
           "post_mortem", "help", "set_default_backend", "get_default_backend"]


class Restart(Exception):
    """Raised to restart the debugged program."""


_default_backend = 'settrace'


def set_default_backend(backend):
    """Set the trace backend ('settrace' or 'monitoring') for new Pdb
    instances."""
    global _default_backend
    if backend not in ("settrace", "monitoring"):
        raise ValueError("Invalid backend: %s" % backend)
    _default_backend = backend


def get_default_backend():
    """The trace backend used by new Pdb instances."""
    return _default_backend


line_prefix = '\n-> '


def find_function(funcname, filename):
    """(funcname, filename, lineno) of the definition of funcname in the
    file, or None."""
    pattern = re.compile(r'def\s+%s\s*[(]' % re.escape(funcname))
    try:
        stream = tokenize.open(filename)
    except OSError:
        return None
    with stream:
        for lineno, line in enumerate(stream, start=1):
            if pattern.match(line):
                return funcname, filename, lineno
    return None


def _rstr(text):
    return str(text)


class Pdb(bdb.Bdb, cmd.Cmd):
    """The interactive debugger."""

    _previous_sigint_handler = None
    _last_pdb_instance = None

    def __init__(self, completekey='tab', stdin=None, stdout=None, skip=None,
                 nosigint=False, readrc=True, mode=None, backend=None,
                 colorize=False):
        bdb.Bdb.__init__(self, skip=skip, backend=backend or get_default_backend())
        cmd.Cmd.__init__(self, completekey, stdin, stdout)
        sys.audit("pdb.Pdb")
        if stdout is not None:
            self.use_rawinput = 0
        self.prompt = '(Pdb) '
        self.aliases = {}
        self.displaying = {}
        self.mainpyfile = ''
        self._wait_for_mainpyfile = False
        self.tb_lineno = {}
        self.mode = mode
        self.colorize = colorize
        self.nosigint = nosigint
        self.rcLines = []
        if readrc:
            for path in (os.path.expanduser('~/.pdbrc'), '.pdbrc'):
                try:
                    with open(path, encoding='utf-8') as rc:
                        self.rcLines.extend(rc)
                except OSError:
                    pass
        self.commands = {}
        self.commands_doprompt = {}
        self.commands_silent = {}
        self.commands_defining = False
        self.commands_bnum = None
        self.stack = []
        self.curindex = 0
        self.curframe = None
        self._chained_exceptions = ()

    def set_trace(self, frame=None, *, commands=None):
        """Start debugging at `frame` (default: the caller); `commands`
        are run at the first stop."""
        Pdb._last_pdb_instance = self
        if commands is not None:
            self.rcLines.extend(commands)
        if frame is None:
            frame = sys._getframe().f_back
        bdb.Bdb.set_trace(self, frame)
        # stop right away at the line that called set_trace(); the
        # debugger's own frames must not be traced meanwhile
        self._paused = True
        try:
            self.interaction(frame, None)
        finally:
            self._paused = False

    _paused = False

    def trace_dispatch(self, frame, event, arg):
        if self._paused:
            return None
        return bdb.Bdb.trace_dispatch(self, frame, event, arg)

    # ---- interaction

    def forget(self):
        self.lineno = None
        self.stack = []
        self.curindex = 0
        self.curframe = None
        self.tb_lineno.clear()

    def setup(self, f, tb):
        self.forget()
        self.stack, self.curindex = self.get_stack(f, tb)
        while tb:
            # remember the line where each frame raised
            lineno = tb.tb_lineno
            self.tb_lineno[tb.tb_frame] = lineno
            tb = tb.tb_next
        self.curframe = self.stack[self.curindex][0]
        self.curframe_locals = self.curframe.f_locals
        self.set_convenience_variable(self.curframe, '_frame', self.curframe)
        return False

    def set_convenience_variable(self, frame, name, value):
        frame.f_globals.setdefault('__pdb_convenience_variables', {})[name] = value

    def user_call(self, frame, argument_list):
        if self._wait_for_mainpyfile:
            return
        if self.stop_here(frame):
            self.message('--Call--')
            self.interaction(frame, None)

    def user_line(self, frame):
        if self._wait_for_mainpyfile:
            if (self.mainpyfile != self.canonic(frame.f_code.co_filename)
                    or frame.f_lineno <= 0):
                return
            self._wait_for_mainpyfile = False
        if self.bp_commands(frame):
            self.interaction(frame, None)

    def bp_commands(self, frame):
        """Run the commands of the breakpoint just hit; returns whether to
        enter the interactive prompt."""
        number = getattr(self, 'currentbp', None)
        if number and number in self.commands:
            self.setup(frame, None)
            for line in self.commands[number]:
                self.onecmd(line)
            self.forget()
            return self.commands_doprompt[number]
        return True

    def user_return(self, frame, return_value):
        if self._wait_for_mainpyfile:
            return
        frame.f_locals['__return__'] = return_value
        self.set_convenience_variable(frame, '_retval', return_value)
        self.message('--Return--')
        self.interaction(frame, None)

    def user_exception(self, frame, exc_info):
        if self._wait_for_mainpyfile:
            return
        exc_type, exc_value, exc_tb = exc_info
        frame.f_locals['__exception__'] = exc_type, exc_value
        self.set_convenience_variable(frame, '_exception', exc_value)
        prefix = 'Internal ' if (not exc_tb and exc_type is StopIteration) else ''
        self.message('%s%s' % (prefix, self._format_exc(exc_value)))
        self.interaction(frame, exc_tb)

    def _format_exc(self, exc):
        return traceback.format_exception_only(exc)[-1].strip()

    def preloop(self):
        for name, value in list(self.displaying.get(self.curframe, {}).items()):
            pass

    def _cmdloop(self):
        while True:
            try:
                self.allow_kbdint = True
                self.cmdloop()
                self.allow_kbdint = False
                break
            except KeyboardInterrupt:
                self.message('--KeyboardInterrupt--')

    def interaction(self, frame, tb_or_exc):
        """Show the current position and run the command loop."""
        if isinstance(tb_or_exc, BaseException):
            tb = tb_or_exc.__traceback__
            self._chained_exceptions = (tb_or_exc,)
        else:
            tb = tb_or_exc
        if self.setup(frame, tb):
            self.forget()
            return
        self.print_stack_entry(self.stack[self.curindex])
        self._run_rc_lines()
        self._cmdloop()
        self.forget()

    def _run_rc_lines(self):
        while self.rcLines:
            line = self.rcLines.pop(0).strip()
            if line and line[0] != '#':
                if self.onecmd(line):
                    self.rcLines = []
                    return

    def displayhook(self, obj):
        """Print the value of an expression statement entered at the prompt."""
        if obj is not None:
            self.message(repr(obj))

    def default(self, line):
        """Execute a Python statement in the current frame."""
        if line[:1] == '!':
            line = line[1:].strip()
        locals_ = self.curframe.f_locals
        globals_ = self.curframe.f_globals
        try:
            code = compile(line + '\n', '<stdin>', 'single')
            saved_stdout = sys.stdout
            saved_stdin = sys.stdin
            saved_hook = sys.displayhook
            try:
                sys.stdin = self.stdin
                sys.stdout = self.stdout
                sys.displayhook = self.displayhook
                exec(code, globals_, locals_)
            finally:
                sys.stdout = saved_stdout
                sys.stdin = saved_stdin
                sys.displayhook = saved_hook
        except:
            self._error_exc()

    def precmd(self, line):
        """Expand aliases and split ';;' command separators."""
        if not line.strip():
            return line
        words = line.split()
        while words and words[0] in self.aliases:
            line = self.aliases[words[0]]
            for index, word in enumerate(words[1:], start=1):
                line = line.replace('%' + str(index), word)
            line = line.replace('%*', ' '.join(words[1:]))
            words = line.split()
        if words and words[0] != 'alias':
            marker = line.find(';;')
            if marker >= 0:
                rest = line[marker + 2:].lstrip()
                self.cmdqueue.insert(0, rest)
                line = line[:marker].rstrip()
        return line

    def onecmd(self, line):
        if not self.commands_defining:
            return cmd.Cmd.onecmd(self, line)
        return self.handle_command_def(line)

    def handle_command_def(self, line):
        """Collect the lines of a 'commands' definition."""
        command, arg, line = self.parseline(line)
        if not command:
            return False
        if command == 'silent':
            self.commands_silent[self.commands_bnum] = True
            return False
        if command == 'end':
            self.cmdqueue = []
            return True
        self.commands[self.commands_bnum].append(line)
        if command in ('c', 'cont', 'continue', 'step', 's', 'next', 'n',
                       'return', 'r', 'quit', 'q', 'exit', 'jump', 'j'):
            self.commands_doprompt[self.commands_bnum] = False
            self.cmdqueue = []
            return True
        return False

    # ---- output

    def message(self, msg, end='\n'):
        print(msg, end=end, file=self.stdout)

    def error(self, msg):
        print('***', msg, file=self.stdout)

    def _error_exc(self):
        exc = sys.exception()
        self.error(self._format_exc(exc))

    def print_stack_entry(self, frame_lineno, prompt_prefix=line_prefix):
        frame, lineno = frame_lineno
        prefix = '> ' if frame is self.curframe else '  '
        self.message(prefix + self.format_stack_entry(frame_lineno, prompt_prefix))

    def print_stack_trace(self, count=None):
        frames = self.stack
        if count is not None:
            frames = frames[-count:] if count > 0 else frames[:-count]
        for entry in frames:
            self.print_stack_entry(entry)

    # ---- evaluation helpers

    def _getval(self, arg):
        try:
            return eval(arg, self.curframe.f_globals, self.curframe.f_locals)
        except:
            self._error_exc()
            raise

    def _getval_except(self, arg, frame=None):
        frame = frame or self.curframe
        try:
            return eval(arg, frame.f_globals, frame.f_locals)
        except BaseException as exc:
            return '** raised %s **' % self._format_exc(exc)

    # ---- commands: stepping

    def do_step(self, arg):
        """s(tep)
        Execute the current line, stop at the first possible occasion
        (either in a function that is called or in the current function)."""
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_step()
        return 1
    do_s = do_step

    def do_next(self, arg):
        """n(ext)
        Continue execution until the next line in the current function is
        reached or it returns."""
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_next(self.curframe)
        return 1
    do_n = do_next

    def do_until(self, arg):
        """unt(il) [lineno]
        Without argument, continue execution until a line with a number
        greater than the current one is reached.  With a line number,
        continue until a line with a number greater or equal to it."""
        if arg:
            try:
                lineno = int(arg)
            except ValueError:
                self.error('Error in argument: %r' % arg)
                return
            if lineno <= self.curframe.f_lineno:
                self.error('"until" line number is smaller than current line number')
                return
        else:
            lineno = None
        self.set_until(self.curframe, lineno)
        return 1
    do_unt = do_until

    def do_return(self, arg):
        """r(eturn)
        Continue execution until the current function returns."""
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_return(self.curframe)
        return 1
    do_r = do_return

    def do_continue(self, arg):
        """c(ont(inue))
        Continue execution, only stop when a breakpoint is encountered."""
        if arg:
            self._print_invalid_arg(arg)
            return
        self.set_continue()
        return 1
    do_c = do_cont = do_continue

    def do_jump(self, arg):
        """j(ump) lineno
        Set the next line that will be executed (bottom frame only)."""
        if self.curindex + 1 != len(self.stack):
            self.error('You can only jump within the bottom frame')
            return
        try:
            target = int(arg)
        except ValueError:
            self.error("The 'jump' command requires a line number")
            return
        try:
            self.curframe.f_lineno = target
            self.stack[self.curindex] = self.stack[self.curindex][0], target
            self.print_stack_entry(self.stack[self.curindex])
        except ValueError as exc:
            self.error('Jump failed: %s' % exc)
    do_j = do_jump

    def do_quit(self, arg):
        """q(uit) | exit
        Quit from the debugger. The program being executed is aborted."""
        self._user_requested_quit = True
        self.set_quit()
        return 1
    do_q = do_exit = do_quit

    def do_EOF(self, arg):
        """EOF
        Handles the receipt of EOF as a command."""
        self.message('')
        return self.do_quit(arg)

    def do_debug(self, arg):
        """debug code
        Enter a recursive debugger that steps through the code argument."""
        sys.settrace(None)
        inner = Pdb(self.completekey, self.stdin, self.stdout)
        inner.prompt = "(%s) " % self.prompt.strip()
        self.message("ENTERING RECURSIVE DEBUGGER")
        try:
            sys.call_tracing(inner.run, (arg, self.curframe.f_globals,
                                         self.curframe.f_locals))
        except Exception:
            self._error_exc()
        self.message("LEAVING RECURSIVE DEBUGGER")
        sys.settrace(self.trace_dispatch)
        self.lastcmd = inner.lastcmd

    def _print_invalid_arg(self, arg):
        self.error(f"Invalid argument: {arg}")

    # ---- commands: stack

    def _select_frame(self, number):
        self.curindex = number
        self.curframe = self.stack[number][0]
        self.curframe_locals = self.curframe.f_locals
        self.print_stack_entry(self.stack[number])
        self.lineno = None

    def do_up(self, arg):
        """u(p) [count]
        Move the current frame count (default one) levels up in the
        stack trace (to an older frame)."""
        if self.curindex == 0:
            self.error('Oldest frame')
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
        Move the current frame count (default one) levels down in the
        stack trace (to a newer frame)."""
        if self.curindex + 1 == len(self.stack):
            self.error('Newest frame')
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

    def do_where(self, arg):
        """w(here) [count]
        Print a stack trace, with the most recent frame at the bottom."""
        count = None
        if arg:
            try:
                count = int(arg)
            except ValueError:
                self.error('Invalid count (%s)' % arg)
                return
        self.print_stack_trace(count)
    do_w = do_bt = do_where

    # ---- commands: inspection

    def do_args(self, arg):
        """a(rgs)
        Print the argument list of the current function."""
        if arg:
            self._print_invalid_arg(arg)
            return
        code = self.curframe.f_code
        names = code.co_varnames[:code.co_argcount + code.co_kwonlyargcount +
                                 bool(code.co_flags & inspect.CO_VARARGS) +
                                 bool(code.co_flags & inspect.CO_VARKEYWORDS)]
        values = self.curframe.f_locals
        for name in names:
            if name in values:
                self.message('%s = %s' % (name, repr(values[name])))
            else:
                self.message('%s = *** undefined ***' % (name,))
    do_a = do_args

    def do_retval(self, arg):
        """retval
        Print the return value for the last return of a function."""
        if arg:
            self._print_invalid_arg(arg)
            return
        locals_ = self.curframe.f_locals
        if '__return__' in locals_:
            self.message(repr(locals_['__return__']))
        else:
            self.error('Not yet returned!')
    do_rv = do_retval

    def do_p(self, arg):
        """p expression
        Print the value of the expression."""
        if not arg:
            self._print_invalid_arg(arg)
            return
        try:
            self.message(repr(self._getval(arg)))
        except:
            pass

    def do_pp(self, arg):
        """pp expression
        Pretty-print the value of the expression."""
        if not arg:
            self._print_invalid_arg(arg)
            return
        try:
            self.message(pprint.pformat(self._getval(arg)))
        except:
            pass

    def do_whatis(self, arg):
        """whatis expression
        Print the type of the argument."""
        if not arg:
            self.error("The 'whatis' command requires an argument")
            return
        try:
            value = self._getval(arg)
        except:
            return
        code = getattr(value, '__code__', None)
        if code is not None and inspect.ismethod(value):
            self.message('Method %s' % code.co_name)
        elif code is not None:
            self.message('Function %s' % code.co_name)
        elif value.__class__ is type:
            self.message('Class %s.%s' % (value.__module__, value.__qualname__))
        else:
            self.message(type(value))

    def do_display(self, arg):
        """display [expression]
        Display the value of the expression if it changed, each time
        execution stops in the current frame."""
        if not arg:
            for expr, value in self.displaying.get(self.curframe, {}).items():
                self.message('%s: %s' % (expr, repr(value)))
            return
        value = self._getval_except(arg)
        self.displaying.setdefault(self.curframe, {})[arg] = value
        self.message('display %s: %s' % (arg, repr(value)))

    def do_undisplay(self, arg):
        """undisplay [expression]
        Do not display the expression any more in the current frame."""
        if arg:
            try:
                del self.displaying.get(self.curframe, {})[arg]
            except KeyError:
                self.error('not displaying %s' % arg)
        else:
            self.displaying.pop(self.curframe, None)

    def do_interact(self, arg):
        """interact
        Start an interactive interpreter in the current frame's namespace."""
        import code
        namespace = {**self.curframe.f_globals, **self.curframe.f_locals}
        console = code.InteractiveConsole(namespace)
        console.interact("*pdb interact start*", "*exit from pdb interact command*")

    def do_alias(self, arg):
        """alias [name [command]]
        Create an alias called 'name' that executes 'command'."""
        words = arg.split()
        if not words:
            for name in sorted(self.aliases):
                self.message("%s = %s" % (name, self.aliases[name]))
            return
        if len(words) == 1:
            if words[0] in self.aliases:
                self.message("%s = %s" % (words[0], self.aliases[words[0]]))
            else:
                self.error(f"Unknown alias '{words[0]}'")
            return
        self.aliases[words[0]] = ' '.join(words[1:])

    def do_unalias(self, arg):
        """unalias name
        Delete the specified alias."""
        words = arg.split()
        if words:
            self.aliases.pop(words[0], None)

    def do_source(self, arg):
        """source expression
        Try to get source code for the given object and display it."""
        try:
            value = self._getval(arg)
        except:
            return
        try:
            lines, lineno = inspect.getsourcelines(value)
        except (OSError, TypeError) as exc:
            self.error(str(exc))
            return
        self._print_lines(lines, lineno)

    # ---- commands: listing

    def _print_lines(self, lines, start, breaks=(), frame=None):
        current = frame.f_lineno if frame else -1
        special = self.tb_lineno.get(frame, -1) if frame else -1
        for lineno, line in enumerate(lines, start):
            text = str(lineno).rjust(3)
            text += ' B' if lineno in breaks else '  '
            if lineno == current:
                text += '->'
            elif lineno == special:
                text += '>>'
            else:
                text += '  '
            self.message((text + '\t').expandtabs() + line.rstrip())

    def do_list(self, arg):
        """l(ist) [first[, last] | .]
        List source code for the current file."""
        self.lastcmd = 'list'
        filename = self.curframe.f_code.co_filename
        last = None
        if arg and arg != '.':
            try:
                if ',' in arg:
                    first_text, last_text = arg.split(',')
                    first = int(first_text)
                    last = int(last_text)
                    if last < first:
                        last = first + last
                else:
                    first = max(1, int(arg.strip()) - 5)
            except ValueError:
                self.error('Error in argument: %r' % arg)
                return
        elif self.lineno is None or arg == '.':
            first = max(1, self.curframe.f_lineno - 5)
        else:
            first = self.lineno + 1
        if last is None:
            last = first + 10
        lines = linecache.getlines(filename, self.curframe.f_globals)
        chunk = lines[first - 1:last]
        if not chunk:
            self.message('[EOF]')
            return
        self._print_lines(chunk, first, self.get_file_breaks(filename), self.curframe)
        self.lineno = min(last, len(lines))
        if last >= len(lines):
            self.message('[EOF]')
    do_l = do_list

    def do_longlist(self, arg):
        """ll | longlist
        List the whole source code for the current function or frame."""
        filename = self.curframe.f_code.co_filename
        breaks = self.get_file_breaks(filename)
        try:
            lines, lineno = inspect.getsourcelines(self.curframe)
        except OSError as exc:
            self.error(str(exc))
            return
        self._print_lines(lines, lineno, breaks, self.curframe)
    do_ll = do_longlist

    # ---- commands: breakpoints

    def do_break(self, arg, temporary=False):
        """b(reak) [ ([filename:]lineno | function) [, condition] ]
        Without argument, list all breaks."""
        if not arg:
            if self.breaks:
                self.message("Num Type         Disp Enb   Where")
                for bp in bdb.Breakpoint.bpbynumber:
                    if bp:
                        self.message(bp.bpformat())
            return
        cond = None
        comma = arg.find(',')
        if comma > 0:
            cond = arg[comma + 1:].lstrip()
            arg = arg[:comma].rstrip()
        colon = arg.rfind(':')
        funcname = None
        if colon >= 0:
            filename = arg[:colon].rstrip()
            found = self.lookupmodule(filename)
            if not found:
                self.error('%r not found from sys.path' % filename)
                return
            filename = found
            try:
                lineno = int(arg[colon + 1:].lstrip())
            except ValueError:
                self.error('Bad lineno: %s' % arg[colon + 1:])
                return
        else:
            try:
                lineno = int(arg)
                filename = self.defaultFile()
            except ValueError:
                try:
                    func = eval(arg, self.curframe.f_globals, self.curframe.f_locals)
                except:
                    func = arg
                try:
                    if hasattr(func, '__func__'):
                        func = func.__func__
                    code = func.__code__
                    funcname = code.co_name
                    lineno = code.co_firstlineno
                    filename = code.co_filename
                except:
                    found = self.lineinfo(arg)
                    if not found[0]:
                        self.error('The specified object %r is not a function '
                                   'or was not found along sys.path.' % arg)
                        return
                    funcname, filename, lineno = found
        if not filename:
            filename = self.defaultFile()
        lineno = self.checkline(filename, lineno)
        if lineno:
            err = self.set_break(filename, lineno, temporary, cond, funcname)
            if err:
                self.error(err)
            else:
                bp = self.get_breaks(filename, lineno)[-1]
                self.message("Breakpoint %d at %s:%d" % (bp.number, bp.file, bp.line))
    do_b = do_break

    def do_tbreak(self, arg):
        """tbreak [ ([filename:]lineno | function) [, condition] ]
        Same arguments as break, but sets a temporary breakpoint."""
        self.do_break(arg, True)

    def defaultFile(self):
        filename = self.curframe.f_code.co_filename
        if filename == '<string>' and self.mainpyfile:
            filename = self.mainpyfile
        return filename

    def lineinfo(self, identifier):
        failed = (None, None, None)
        parts = identifier.split("'")
        item = parts[1] if len(parts) == 3 else identifier
        pieces = item.split('.')
        filename = self.defaultFile()
        if len(pieces) > 1:
            found = self.lookupmodule(pieces[0])
            if not found:
                return failed
            filename = found
            item = pieces[1]
        return find_function(item, filename) or failed

    def checkline(self, filename, lineno):
        """lineno if it looks executable, else 0 (with an error message)."""
        globs = self.curframe.f_globals if self.curframe else None
        line = linecache.getline(filename, lineno, globs)
        if not line:
            self.message('End of file')
            return 0
        line = line.strip()
        if not line or line[0] == '#' or line[:3] in ('"""', "'''"):
            self.error('Blank or comment')
            return 0
        return lineno

    def lookupmodule(self, filename):
        """Full path of a (module or file) name, or None."""
        if os.path.isabs(filename) and os.path.exists(filename):
            return filename
        candidate = os.path.join(sys.path[0], filename)
        if os.path.exists(candidate) and self.canonic(candidate) == self.mainpyfile:
            return candidate
        root, ext = os.path.splitext(filename)
        if ext == '':
            filename = filename + '.py'
        if os.path.isabs(filename):
            return filename
        for directory in sys.path:
            while os.path.islink(directory):
                directory = os.readlink(directory)
            path = os.path.join(directory, filename)
            if os.path.exists(path):
                return path
        return None

    def do_enable(self, arg):
        """enable bpnumber [bpnumber ...]
        Enables the breakpoints given as a space separated list."""
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
        Disables the breakpoints given as a space separated list."""
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
        Set a new condition for the breakpoint (none: unconditional)."""
        words = arg.split(' ', 1)
        cond = words[1] if len(words) > 1 else None
        try:
            bp = self.get_bpbynumber(words[0].strip())
        except IndexError:
            self.error('Breakpoint number expected')
            return
        except ValueError as err:
            self.error(err)
            return
        bp.cond = cond
        if not cond:
            self.message('Breakpoint %d is now unconditional.' % bp.number)
        else:
            self.message('New condition set for breakpoint %d.' % bp.number)

    def do_ignore(self, arg):
        """ignore bpnumber [count]
        Set the ignore count for the given breakpoint number."""
        words = arg.split()
        if not words:
            self.error('Breakpoint number expected')
            return
        try:
            count = int(words[1].strip()) if len(words) > 1 else 0
            bp = self.get_bpbynumber(words[0].strip())
        except ValueError as err:
            self.error(err)
            return
        bp.ignore = count
        if count > 0:
            if count > 1:
                countstr = '%d crossings' % count
            else:
                countstr = '1 crossing'
            self.message('Will ignore next %s of breakpoint %d.' % (countstr, bp.number))
        else:
            self.message('Will stop next time breakpoint %d is reached.' % bp.number)

    def do_clear(self, arg):
        """cl(ear) [filename:lineno | bpnumber ...]
        Without argument, clear all breaks (but first ask confirmation)."""
        if not arg:
            try:
                reply = input('Clear all breaks? ')
            except EOFError:
                reply = 'no'
            if reply.strip().lower() in ('y', 'yes'):
                deleted = [bp for bp in bdb.Breakpoint.bpbynumber if bp]
                self.clear_all_breaks()
                for bp in deleted:
                    self.message('Deleted %s' % bp)
            return
        if ':' in arg:
            colon = arg.rfind(':')
            filename = arg[:colon]
            try:
                lineno = int(arg[colon + 1:])
            except ValueError:
                err = "Invalid line number (%s)" % arg
            else:
                deleted = self.get_breaks(filename, lineno)[:]
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

    def do_commands(self, arg):
        """commands [bpnumber]
        Specify a list of commands for the breakpoint (end with 'end')."""
        if not arg:
            bnum = len(bdb.Breakpoint.bpbynumber) - 1
        else:
            try:
                bnum = int(arg)
            except ValueError:
                self._print_invalid_arg(arg)
                return
        try:
            self.get_bpbynumber(bnum)
        except ValueError as err:
            self.error('cannot set commands: %s' % err)
            return
        self.commands_bnum = bnum
        self.commands[bnum] = []
        self.commands_doprompt[bnum] = True
        self.commands_silent[bnum] = False
        saved_prompt = self.prompt
        self.prompt = '(com) '
        self.commands_defining = True
        try:
            self.cmdloop()
        finally:
            self.commands_defining = False
            self.prompt = saved_prompt

    def do_exceptions(self, arg):
        """exceptions [number]
        List or change the current exception in a chain (post mortem)."""
        if not self._chained_exceptions:
            self.message('Did not find chained exceptions. To move between'
                         ' exceptions, pdb/post_mortem must be given an exception'
                         ' object rather than a traceback.')
            return
        for index, exc in enumerate(self._chained_exceptions):
            self.message('> %d %r' % (index, exc))

    # ---- help

    def do_help(self, arg):
        """h(elp)
        Without argument, print the list of available commands.
        With a command name as argument, print help about that command."""
        if not arg:
            return cmd.Cmd.do_help(self, arg)
        try:
            helper = getattr(self, 'help_' + arg)
            return helper()
        except AttributeError:
            command = getattr(self, 'do_' + arg, None)
            if command is None or not command.__doc__:
                self.error('No help for %r' % arg)
                return
            self.message(inspect.cleandoc(command.__doc__))
    do_h = do_help

    def help_exec(self):
        self.message(inspect.cleandoc("""(!) statement
        Execute the (one-line) statement in the context of the current
        stack frame."""))

    def help_pdb(self):
        help()

    # ---- running

    def _runmodule(self, module_name):
        self._wait_for_mainpyfile = True
        self._user_requested_quit = False
        import runpy
        mod_name, mod_spec, code = runpy._get_module_details(module_name)
        self.mainpyfile = self.canonic(code.co_filename)
        import __main__
        __main__.__dict__.clear()
        __main__.__dict__.update({
            "__name__": "__main__",
            "__file__": self.mainpyfile,
            "__package__": mod_spec.parent,
            "__loader__": mod_spec.loader,
            "__spec__": mod_spec,
            "__builtins__": __builtins__,
        })
        self.run(code)

    def _runscript(self, filename):
        import __main__
        __main__.__dict__.clear()
        __main__.__dict__.update({"__name__": "__main__", "__file__": filename,
                                  "__builtins__": __builtins__})
        self._wait_for_mainpyfile = True
        self.mainpyfile = self.canonic(filename)
        self._user_requested_quit = False
        with io_open(filename) as stream:
            statement = compile(stream.read(), self.mainpyfile, 'exec')
        self.run(statement)


def io_open(filename):
    import io
    return io.open_code(filename)


# ---------------------------------------------------------------- module functions

def run(statement, globals=None, locals=None):
    """Execute the statement under debugger control."""
    Pdb().run(statement, globals, locals)


def runeval(expression, globals=None, locals=None):
    """Evaluate the expression under debugger control."""
    return Pdb().runeval(expression, globals, locals)


def runctx(statement, globals, locals):
    run(statement, globals, locals)


def runcall(*args, **kwds):
    """Call a function under debugger control."""
    return Pdb().runcall(*args, **kwds)


def set_trace(*, header=None, commands=None):
    """Enter the debugger at the calling stack frame."""
    if Pdb._last_pdb_instance is not None:
        debugger = Pdb._last_pdb_instance
    else:
        debugger = Pdb(mode='inline', backend='monitoring')
    if header is not None:
        debugger.message(header)
    debugger.set_trace(sys._getframe().f_back)


def post_mortem(t=None):
    """Enter post-mortem debugging of a traceback or exception (default:
    the exception being handled)."""
    if isinstance(t, BaseException):
        exc = t
        tb = exc.__traceback__
    else:
        tb = t
        if tb is None:
            exc = sys.exception()
            tb = exc.__traceback__ if exc is not None else None
    if tb is None:
        raise ValueError("A valid traceback must be passed if no "
                         "exception is being handled")
    debugger = Pdb()
    debugger.reset()
    debugger.interaction(None, t if isinstance(t, BaseException) else tb)


def pm():
    """Post-mortem debugging of the last exception (sys.last_exc)."""
    exc = getattr(sys, 'last_exc', None)
    if exc is None:
        raise RuntimeError("No last exception available")
    post_mortem(exc)


TESTCMD = 'import x; x.main()'


def test():
    run(TESTCMD)


def help():
    """Print the module documentation."""
    import pydoc
    pydoc.pager(__doc__)


_usage = """\
usage: pdb.py [-c command] ... [-m module | pyfile] [arg] ...

Debug the Python program given by pyfile. Alternatively,
an executable module or package to debug can be specified using
the -m switch.
"""


def main():
    import getopt
    opts, args = getopt.getopt(sys.argv[1:], 'mhc:', ['help', 'command='])
    if not args:
        print(_usage)
        sys.exit(2)
    if any(opt in ('-h', '--help') for opt, _ in opts):
        print(_usage)
        sys.exit()
    commands = [value for opt, value in opts if opt in ('-c', '--command')]
    module = any(opt == '-m' for opt, _ in opts)
    target = args[0]
    if not module and not os.path.exists(target):
        print('Error:', target, 'does not exist')
        sys.exit(1)
    sys.argv[:] = args
    if not module:
        sys.path[0] = os.path.dirname(os.path.realpath(target))
    debugger = Pdb()
    debugger.rcLines.extend(commands)
    while True:
        try:
            if module:
                debugger._runmodule(target)
            else:
                debugger._runscript(target)
            if debugger._user_requested_quit:
                break
            print("The program finished and will be restarted")
        except Restart:
            print("Restarting", target, "with arguments:")
            print("\t" + " ".join(sys.argv[1:]))
        except SystemExit as exc:
            print("The program exited via sys.exit(). Exit status:", exc.code)
        except BaseException:
            traceback.print_exc()
            print("Uncaught exception. Entering post mortem debugging")
            debugger.interaction(None, sys.exception())
            print("Post mortem debugger finished. The " + target +
                  " will be restarted")


if __name__ == '__main__':
    import pdb
    pdb.main()
