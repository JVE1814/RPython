"""Generic debugger machinery (the base of pdb).

Bdb installs itself with sys.settrace() and decides, for every trace
event, whether to stop; when it stops it calls user_call(), user_line(),
user_return() or user_exception(), which subclasses implement (pdb runs
its command loop there).  Breakpoint objects are shared by all Bdb
instances.
"""

import fnmatch
import os
import sys
from inspect import CO_GENERATOR, CO_COROUTINE, CO_ASYNC_GENERATOR

__all__ = ["BdbQuit", "Bdb", "Breakpoint"]

GENERATOR_AND_COROUTINE_FLAGS = CO_GENERATOR | CO_COROUTINE | CO_ASYNC_GENERATOR


class BdbQuit(Exception):
    """Raised to leave the debugged program when the user quits."""


class Bdb:
    """Base debugger: stepping, breakpoints and stack inspection."""

    def __init__(self, skip=None, backend='settrace'):
        """skip: glob patterns of module names in which never to stop."""
        self.skip = set(skip) if skip else None
        self.backend = backend
        self.breaks = {}
        self.fncache = {}
        self.frame_trace_lines_opcodes = {}
        self.frame_returning = None
        self.trace_opcodes = False
        self.enterframe = None
        self.quitting = False
        self.botframe = None
        self._load_breaks()

    # ---- file names

    def canonic(self, filename):
        """Absolute, case-normalized form of a file name; pseudo names
        like '<string>' are returned unchanged."""
        if filename.startswith('<') and filename.endswith('>'):
            return filename
        known = self.fncache.get(filename)
        if known is None:
            known = os.path.normcase(os.path.abspath(filename))
            self.fncache[filename] = known
        return known

    def reset(self):
        """Prepare for a new debugging session."""
        import linecache
        linecache.checkcache()
        self.botframe = None
        self._set_stopinfo(None, None)

    # ---- event dispatch

    def trace_dispatch(self, frame, event, arg):
        """The trace function: dispatches to dispatch_<event>()."""
        if self.quitting:
            return None
        self.enterframe = frame
        handler = {
            'line': self.dispatch_line,
            'call': self.dispatch_call,
            'return': self.dispatch_return,
            'exception': self.dispatch_exception,
            'opcode': self.dispatch_opcode,
        }.get(event)
        if handler is not None:
            return handler(frame) if event == 'line' else handler(frame, arg)
        if event in ('c_call', 'c_exception', 'c_return'):
            return self.trace_dispatch
        print('bdb.Bdb.dispatch: unknown debugging event:', repr(event))
        return self.trace_dispatch

    def _maybe_quit(self):
        if self.quitting:
            raise BdbQuit

    def dispatch_line(self, frame):
        """Stop at a line if stepping says so or a breakpoint is hit."""
        if self.stop_here(frame) or self.break_here(frame):
            self.user_line(frame)
            self._maybe_quit()
        return self.trace_dispatch

    def dispatch_call(self, frame, arg):
        """A function is entered; returns the local trace function or None
        to skip tracing it."""
        if self.botframe is None:
            # the first frame: everything below it is the debugger's
            self.botframe = frame.f_back
            return self.trace_dispatch
        if not (self.stop_here(frame) or self.break_anywhere(frame)):
            return None
        if self.stopframe and frame.f_code.co_flags & GENERATOR_AND_COROUTINE_FLAGS:
            return self.trace_dispatch
        self.user_call(frame, arg)
        self._maybe_quit()
        return self.trace_dispatch

    def dispatch_return(self, frame, arg):
        """A function returns `arg`."""
        if self.stop_here(frame) or frame is self.returnframe:
            if self.stopframe and frame.f_code.co_flags & GENERATOR_AND_COROUTINE_FLAGS:
                return self.trace_dispatch
            try:
                self.frame_returning = frame
                self.user_return(frame, arg)
            finally:
                self.frame_returning = None
            self._maybe_quit()
            if self.stopframe is frame and self.stoplineno != -1:
                self._set_stopinfo(None, None)
        return self.trace_dispatch

    def dispatch_exception(self, frame, arg):
        """An exception (arg = (type, value, traceback)) occurred."""
        generator = frame.f_code.co_flags & GENERATOR_AND_COROUTINE_FLAGS
        if self.stop_here(frame):
            # a generator finishing through StopIteration is not an error
            if not (generator and arg[0] is StopIteration and arg[2] is None):
                self.user_exception(frame, arg)
                self._maybe_quit()
        elif (self.stopframe and frame is not self.stopframe and
              self.stopframe.f_code.co_flags & GENERATOR_AND_COROUTINE_FLAGS and
              arg[0] in (StopIteration, GeneratorExit)):
            self.user_exception(frame, arg)
            self._maybe_quit()
        return self.trace_dispatch

    def dispatch_opcode(self, frame, arg=None):
        """An instruction is about to run (instruction stepping)."""
        if self.stop_here(frame) or self.break_here(frame):
            self.user_opcode(frame)
            self._maybe_quit()
        return self.trace_dispatch

    # ---- stop decisions

    def is_skipped_module(self, module_name):
        """True if the module matches one of the skip patterns."""
        if module_name is None:
            return False
        return any(fnmatch.fnmatch(module_name, pattern) for pattern in self.skip)

    def stop_here(self, frame):
        """Whether stepping should stop in this frame."""
        if self.skip and self.is_skipped_module(frame.f_globals.get('__name__')):
            return False
        if frame is self.stopframe:
            if self.stoplineno == -1:
                return False
            return frame.f_lineno >= self.stoplineno
        if not self.stopframe:
            return True
        return False

    def break_here(self, frame):
        """Whether an effective breakpoint is set at this line (also counts
        hits and deletes temporary breakpoints)."""
        filename = self.canonic(frame.f_code.co_filename)
        lines = self.breaks.get(filename)
        if not lines:
            return False
        lineno = frame.f_lineno
        if lineno not in lines:
            # breakpoints set on a function name use the first line
            lineno = frame.f_code.co_firstlineno
            if lineno not in lines:
                return False
        bp, active = effective(filename, lineno, frame)
        if bp is None:
            return False
        self.currentbp = bp.number
        if active and bp.temporary:
            self.do_clear(str(bp.number))
        return True

    def do_clear(self, arg):
        """Remove a temporary breakpoint (implemented by subclasses)."""
        raise NotImplementedError("subclass of bdb must implement do_clear()")

    def break_anywhere(self, frame):
        """Whether there is any breakpoint in the frame's file."""
        return self.canonic(frame.f_code.co_filename) in self.breaks

    # ---- hooks for subclasses

    def user_call(self, frame, argument_list):
        """Called when stopping at a function call."""

    def user_line(self, frame):
        """Called when stopping at a line."""

    def user_return(self, frame, return_value):
        """Called when stopping at a function return."""

    def user_exception(self, frame, exc_info):
        """Called when stopping at an exception."""

    def user_opcode(self, frame):
        """Called when stopping at an instruction."""

    # ---- stepping commands

    def _set_stopinfo(self, stopframe, returnframe, stoplineno=0, opcode=False):
        """Stop next in `stopframe` (any frame if None) at a line >=
        stoplineno (-1: do not stop there) or when `returnframe` returns."""
        self.stopframe = stopframe
        self.returnframe = returnframe
        self.quitting = False
        self.stoplineno = stoplineno
        self._set_trace_opcodes(opcode)

    def _set_trace_opcodes(self, trace_opcodes):
        if trace_opcodes == self.trace_opcodes:
            return
        self.trace_opcodes = trace_opcodes
        frame = self.enterframe
        while frame is not None:
            frame.f_trace_opcodes = trace_opcodes
            if frame is self.botframe:
                break
            frame = frame.f_back

    def set_until(self, frame, lineno=None):
        """Stop at a line greater than the current one (or `lineno`) in this
        frame, or when it returns."""
        if lineno is None:
            lineno = frame.f_lineno + 1
        self._set_stopinfo(frame, frame, lineno)

    def set_step(self):
        """Stop at the next event anywhere."""
        if self.frame_returning:
            caller = self.frame_returning.f_back
            if caller is not None and not caller.f_trace:
                caller.f_trace = self.trace_dispatch
        self._set_stopinfo(None, None)

    def set_stepinstr(self):
        """Stop at the next instruction."""
        self._set_stopinfo(None, None, opcode=True)

    def set_next(self, frame):
        """Stop at the next line in or above `frame`."""
        self._set_stopinfo(frame, None)

    def set_return(self, frame):
        """Stop when `frame` returns."""
        if frame.f_code.co_flags & GENERATOR_AND_COROUTINE_FLAGS:
            self._set_stopinfo(frame, None, -1)
        else:
            self._set_stopinfo(frame.f_back, frame)

    def set_trace(self, frame=None):
        """Start debugging at `frame` (default: the caller)."""
        sys.settrace(None)
        if frame is None:
            frame = sys._getframe().f_back
        self.reset()
        self.enterframe = frame
        while frame is not None:
            frame.f_trace = self.trace_dispatch
            self.botframe = frame
            self.frame_trace_lines_opcodes[frame] = (frame.f_trace_lines,
                                                     frame.f_trace_opcodes)
            frame = frame.f_back
        self.set_step()
        self.enterframe = None
        sys.settrace(self.trace_dispatch)

    def set_continue(self):
        """Run until a breakpoint (stops tracing if there are none)."""
        self._set_stopinfo(self.botframe, None, -1)
        if not self.breaks:
            sys.settrace(None)
            frame = sys._getframe().f_back
            while frame is not None and frame is not self.botframe:
                del frame.f_trace
                frame = frame.f_back
            for frame, (lines, opcodes) in self.frame_trace_lines_opcodes.items():
                frame.f_trace_lines, frame.f_trace_opcodes = lines, opcodes
            self.frame_trace_lines_opcodes = {}

    def set_quit(self):
        """Stop debugging: the program is terminated with BdbQuit."""
        self.stopframe = self.botframe
        self.returnframe = None
        self.quitting = True
        sys.settrace(None)

    # ---- breakpoints

    def _add_to_breaks(self, filename, lineno):
        lines = self.breaks.setdefault(filename, [])
        if lineno not in lines:
            lines.append(lineno)

    def set_break(self, filename, lineno, temporary=False, cond=None,
                  funcname=None):
        """Set a breakpoint; returns an error message or None."""
        filename = self.canonic(filename)
        import linecache
        if not linecache.getline(filename, lineno):
            return 'Line %s:%d does not exist' % (filename, lineno)
        self._add_to_breaks(filename, lineno)
        Breakpoint(filename, lineno, temporary, cond, funcname)
        return None

    def _load_breaks(self):
        # breakpoints are shared between debugger instances
        for (filename, lineno) in Breakpoint.bplist.keys():
            self._add_to_breaks(filename, lineno)

    def _prune_breaks(self, filename, lineno):
        if (filename, lineno) not in Breakpoint.bplist:
            lines = self.breaks[filename]
            lines.remove(lineno)
            if not lines:
                del self.breaks[filename]

    def clear_break(self, filename, lineno):
        """Delete the breakpoints at filename:lineno; returns an error
        message or None."""
        filename = self.canonic(filename)
        if filename not in self.breaks:
            return 'There are no breakpoints in %s' % filename
        if lineno not in self.breaks[filename]:
            return 'There is no breakpoint at %s:%d' % (filename, lineno)
        for bp in list(Breakpoint.bplist[filename, lineno]):
            bp.deleteMe()
        self._prune_breaks(filename, lineno)
        return None

    def clear_bpbynumber(self, arg):
        """Delete a breakpoint by number; returns an error message or None."""
        try:
            bp = self.get_bpbynumber(arg)
        except ValueError as err:
            return str(err)
        bp.deleteMe()
        self._prune_breaks(bp.file, bp.line)
        return None

    def clear_all_file_breaks(self, filename):
        """Delete all breakpoints in a file; returns an error message or None."""
        filename = self.canonic(filename)
        if filename not in self.breaks:
            return 'There are no breakpoints in %s' % filename
        for lineno in self.breaks[filename]:
            for bp in list(Breakpoint.bplist[filename, lineno]):
                bp.deleteMe()
        del self.breaks[filename]
        return None

    def clear_all_breaks(self):
        """Delete all breakpoints; returns an error message or None."""
        if not self.breaks:
            return 'There are no breakpoints'
        for bp in Breakpoint.bpbynumber:
            if bp:
                bp.deleteMe()
        self.breaks = {}
        return None

    def get_bpbynumber(self, arg):
        """The Breakpoint numbered `arg` (ValueError if invalid)."""
        if not arg:
            raise ValueError('Breakpoint number expected')
        try:
            number = int(arg)
        except ValueError:
            raise ValueError('Non-numeric breakpoint number %s' % arg) from None
        try:
            bp = Breakpoint.bpbynumber[number]
        except IndexError:
            raise ValueError('Breakpoint number %d out of range' % number) from None
        if bp is None:
            raise ValueError('Breakpoint %d already deleted' % number)
        return bp

    def get_break(self, filename, lineno):
        """Whether there is a breakpoint at filename:lineno."""
        filename = self.canonic(filename)
        return filename in self.breaks and lineno in self.breaks[filename]

    def get_breaks(self, filename, lineno):
        """The Breakpoints at filename:lineno."""
        filename = self.canonic(filename)
        if filename in self.breaks and lineno in self.breaks[filename]:
            return Breakpoint.bplist[filename, lineno]
        return []

    def get_file_breaks(self, filename):
        """The line numbers with breakpoints in a file."""
        return self.breaks.get(self.canonic(filename), [])

    def get_all_breaks(self):
        """{filename: [line numbers]} of all breakpoints."""
        return self.breaks

    # ---- stack

    def get_stack(self, f, t):
        """(stack, index): the (frame, lineno) pairs from the bottom frame
        to `f` followed by those of traceback `t`; index is that of the
        current frame."""
        stack = []
        if t and t.tb_frame is f:
            t = t.tb_next
        while f is not None:
            stack.append((f, f.f_lineno))
            if f is self.botframe:
                break
            f = f.f_back
        stack.reverse()
        index = max(0, len(stack) - 1)
        while t is not None:
            stack.append((t.tb_frame, t.tb_lineno))
            t = t.tb_next
        if f is None:
            index = max(0, len(stack) - 1)
        return stack, index

    def format_stack_entry(self, frame_lineno, lprefix=': '):
        """'file(line)function()->return value: source line'."""
        import linecache
        import reprlib
        frame, lineno = frame_lineno
        filename = self.canonic(frame.f_code.co_filename)
        text = '%s(%r)%s()' % (filename, lineno, frame.f_code.co_name or "<lambda>")
        if '__return__' in frame.f_locals:
            text += '->' + reprlib.repr(frame.f_locals['__return__'])
        if lineno is None:
            text += lprefix + "Warning: lineno is None"
        else:
            line = linecache.getline(filename, lineno, frame.f_globals)
            if line:
                text += lprefix + line.strip()
        return text

    # ---- running code

    def run(self, cmd, globals=None, locals=None):
        """Debug the statement(s) `cmd` (string or code object)."""
        if globals is None:
            import __main__
            globals = __main__.__dict__
        if locals is None:
            locals = globals
        self.reset()
        if isinstance(cmd, str):
            cmd = compile(cmd, "<string>", "exec")
        sys.settrace(self.trace_dispatch)
        try:
            exec(cmd, globals, locals)
        except BdbQuit:
            pass
        finally:
            self.quitting = True
            sys.settrace(None)

    def runeval(self, expr, globals=None, locals=None):
        """Debug the expression `expr`; returns its value."""
        if globals is None:
            import __main__
            globals = __main__.__dict__
        if locals is None:
            locals = globals
        self.reset()
        sys.settrace(self.trace_dispatch)
        try:
            return eval(expr, globals, locals)
        except BdbQuit:
            pass
        finally:
            self.quitting = True
            sys.settrace(None)

    def runctx(self, cmd, globals, locals):
        """Same as run()."""
        self.run(cmd, globals, locals)

    def runcall(self, func, /, *args, **kwds):
        """Debug a function call; returns its result."""
        self.reset()
        sys.settrace(self.trace_dispatch)
        result = None
        try:
            result = func(*args, **kwds)
        except BdbQuit:
            pass
        finally:
            self.quitting = True
            sys.settrace(None)
        return result


def set_trace():
    """Start a Bdb session at the caller's frame."""
    Bdb().set_trace(sys._getframe().f_back)


class Breakpoint:
    """A breakpoint; all instances are kept in the class attributes
    `bpbynumber` (indexed by number) and `bplist` ({(file, line): [...]})."""

    next = 1
    bplist = {}
    bpbynumber = [None]

    def __init__(self, file, line, temporary=False, cond=None, funcname=None):
        self.funcname = funcname
        self.func_first_executable_line = None
        self.file = file
        self.line = line
        self.temporary = temporary
        self.cond = cond
        self.enabled = True
        self.ignore = 0
        self.hits = 0
        self.number = Breakpoint.next
        Breakpoint.next += 1
        self.bpbynumber.append(self)
        self.bplist.setdefault((file, line), []).append(self)

    @staticmethod
    def clearBreakpoints():
        Breakpoint.next = 1
        Breakpoint.bplist = {}
        Breakpoint.bpbynumber = [None]

    def deleteMe(self):
        """Remove this breakpoint from the shared lists."""
        key = (self.file, self.line)
        self.bpbynumber[self.number] = None
        self.bplist[key].remove(self)
        if not self.bplist[key]:
            del self.bplist[key]

    def enable(self):
        self.enabled = True

    def disable(self):
        self.enabled = False

    def bpprint(self, out=None):
        """Print bpformat() to `out` (default stdout)."""
        print(self.bpformat(), file=out if out is not None else sys.stdout)

    def bpformat(self):
        """Description as used by pdb's 'break' listing."""
        disposition = 'del  ' if self.temporary else 'keep '
        disposition += 'yes  ' if self.enabled else 'no   '
        text = '%-4dbreakpoint   %s at %s:%d' % (self.number, disposition,
                                                  self.file, self.line)
        if self.cond:
            text += '\n\tstop only if %s' % (self.cond,)
        if self.ignore:
            text += '\n\tignore next %d hits' % (self.ignore,)
        if self.hits:
            plural = 's' if self.hits > 1 else ''
            text += '\n\tbreakpoint already hit %d time%s' % (self.hits, plural)
        return text

    def __str__(self):
        return 'breakpoint %s at %s:%s' % (self.number, self.file, self.line)


def checkfuncname(b, frame):
    """Whether breakpoint b (possibly set on a function name) applies to
    the current line of frame."""
    if not b.funcname:
        return b.line == frame.f_lineno
    if frame.f_code.co_name != b.funcname:
        return False
    # stop only at the first executable line of the function
    if not b.func_first_executable_line:
        b.func_first_executable_line = frame.f_lineno
    return b.func_first_executable_line == frame.f_lineno


def effective(file, line, frame):
    """(breakpoint, delete_if_temporary) for the breakpoint that should
    stop at file:line in frame, or (None, None)."""
    for b in Breakpoint.bplist[file, line]:
        if not b.enabled or not checkfuncname(b, frame):
            continue
        b.hits += 1
        if b.cond:
            try:
                triggered = eval(b.cond, frame.f_globals, frame.f_locals)
            except:
                # a broken condition stops, but does not delete
                return (b, False)
            if not triggered:
                continue
        if b.ignore > 0:
            b.ignore -= 1
            continue
        return (b, True)
    return (None, None)
