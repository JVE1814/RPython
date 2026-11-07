"""Building blocks for read-eval-print loops.

InteractiveInterpreter compiles and runs source in a namespace;
InteractiveConsole adds line buffering and the prompt loop; interact()
starts a console.
"""

import builtins
import sys
import traceback

try:
    from codeop import CommandCompiler, compile_command
except ImportError:          # minimal stand-in until codeop is available
    from _codeop_fallback import CommandCompiler, compile_command

__all__ = ["InteractiveInterpreter", "InteractiveConsole", "interact",
           "compile_command"]


class InteractiveInterpreter:
    """Compiles and executes source code in the namespace `locals`
    (default: a fresh namespace with __name__ '__console__')."""

    def __init__(self, locals=None):
        if locals is None:
            locals = {"__name__": "__console__", "__doc__": None}
        self.locals = locals
        self.compile = CommandCompiler()

    def runsource(self, source, filename="<input>", symbol="single"):
        """Compile and run `source`.

        Returns True if the source is incomplete (more input is needed),
        False otherwise; syntax errors and exceptions are reported with
        showsyntaxerror()/showtraceback()."""
        try:
            code = self.compile(source, filename, symbol)
        except Exception:
            # any compile() failure (also MemoryError, RecursionError) is
            # reported like a syntax error: without a traceback
            self.showsyntaxerror(filename, source=source)
            return False
        if code is None:
            return True
        self.runcode(code)
        return False

    def runcode(self, code):
        """Execute a code object; exceptions (except SystemExit) are
        reported with showtraceback()."""
        try:
            exec(code, self.locals)
        except (SystemExit, _LocalExit):
            raise
        except BaseException:
            self.showtraceback()

    def showsyntaxerror(self, filename=None, **kwargs):
        """Report the syntax error being handled (with `filename` as its
        file name if given)."""
        exc_type, exc, tb = sys.exc_info()
        source = kwargs.pop('source', None)
        try:
            exc.__traceback__ = None
            if filename and issubclass(exc_type, SyntaxError):
                exc.filename = filename
            if (source and issubclass(exc_type, SyntaxError)
                    and exc.text is None and exc.lineno):
                # errors found after parsing (e.g. compiling an AST) carry
                # no source line: take it from the text that was compiled
                lines = source.splitlines()
                if 0 < exc.lineno <= len(lines):
                    exc.text = lines[exc.lineno - 1]
            self._report(exc_type, exc, None, **kwargs)
        finally:
            exc = None

    def showtraceback(self):
        """Report the exception being handled, without the frame of
        runcode() itself."""
        exc_type, exc, tb = sys.exc_info()
        try:
            exc.__traceback__ = tb.tb_next if tb is not None else None
            self._report(exc_type, exc, exc.__traceback__)
        finally:
            exc = tb = None

    def _report(self, exc_type, exc, tb, **kwargs):
        sys.last_type = exc_type
        sys.last_value = exc
        sys.last_traceback = tb
        sys.last_exc = exc
        hook = sys.excepthook
        if hook is sys.__excepthook__:
            colorize = kwargs.pop('colorize', False)
            # reported like the interpreter does (sys.tracebacklimit keeps
            # the innermost entries)
            lines = traceback.format_exception(
                exc_type, exc, tb, colorize=colorize,
                limit=traceback.BUILTIN_EXCEPTION_LIMIT)
            self.write(''.join(lines))
            return
        try:
            hook(exc_type, exc, tb)
        except SystemExit:
            raise
        except BaseException as hook_error:
            hook_error.__context__ = None
            failure = hook_error.__traceback__.tb_next if hook_error.__traceback__ else None
            lines = traceback.format_exception(type(hook_error), hook_error, failure)
            self.write('Error in sys.excepthook:\n')
            self.write(''.join(lines))
            self.write('\nOriginal exception was:\n')
            self.write(''.join(traceback.format_exception(exc_type, exc, tb)))

    def write(self, data):
        """Output channel for errors (sys.stderr by default)."""
        sys.stderr.write(data)


class InteractiveConsole(InteractiveInterpreter):
    """An interpreter with input buffering and an interaction loop."""

    def __init__(self, locals=None, filename="<console>", *, local_exit=False):
        InteractiveInterpreter.__init__(self, locals)
        self.filename = filename
        self.local_exit = local_exit
        self.resetbuffer()

    def resetbuffer(self):
        """Forget any buffered input lines."""
        self.buffer = []

    def interact(self, banner=None, exitmsg=None):
        """Run the prompt loop until end of file (or exit() with
        local_exit=True).  `banner` defaults to a version message; an
        empty exitmsg suppresses the farewell message."""
        # the prompts are set for the session only if sys has none of its own
        added_ps1 = getattr(sys, 'ps1', None) is None
        if added_ps1:
            sys.ps1 = ">>> "
        added_ps2 = getattr(sys, 'ps2', None) is None
        if added_ps2:
            sys.ps2 = "... "
        name = type(self).__name__
        if banner is None:
            self.write("Python %s on %s\n%s\n(%s)\n" % (
                sys.version, sys.platform,
                'Type "help", "copyright", "credits" or "license" for more information.',
                name))
        elif banner:
            self.write("%s\n" % str(banner))

        saved_exits = self._install_local_exit()
        try:
            more = False
            while True:
                try:
                    prompt = sys.ps2 if more else sys.ps1
                    try:
                        line = self.raw_input(prompt)
                    except EOFError:
                        self.write("\n")
                        break
                    more = self.push(line)
                except KeyboardInterrupt:
                    self.write("\nKeyboardInterrupt\n")
                    self.resetbuffer()
                    more = False
                except _LocalExit:
                    break
        finally:
            self._restore_exits(saved_exits)
            if added_ps1:
                del sys.ps1
            if added_ps2:
                del sys.ps2

        if exitmsg is None:
            self.write('now exiting %s...\n' % name)
        elif exitmsg != '':
            self.write('%s\n' % exitmsg)

    def _install_local_exit(self):
        if not self.local_exit:
            return None
        saved = {}
        for name in ('exit', 'quit'):
            if hasattr(builtins, name):
                saved[name] = getattr(builtins, name)
                setattr(builtins, name, _LocalExitCallable(name))
        return saved

    def _restore_exits(self, saved):
        if saved:
            for name, value in saved.items():
                setattr(builtins, name, value)

    def push(self, line, filename=None, _symbol="single"):
        """Add a line to the buffer and try to run the buffered source.

        Returns True if more input is needed."""
        self.buffer.append(line)
        source = "\n".join(self.buffer)
        if filename is None:
            filename = self.filename
        more = self.runsource(source, filename, symbol=_symbol)
        if not more:
            self.resetbuffer()
        return more

    def raw_input(self, prompt=""):
        """Read a line (built-in input() by default; override to change)."""
        return input(prompt)


class _LocalExit(Exception):
    pass


class _LocalExitCallable:
    """exit()/quit() replacement that only leaves the console."""

    def __init__(self, name):
        self.name = name

    def __repr__(self):
        return f'Use {self.name}() or Ctrl-D (i.e. EOF) to exit'

    def __call__(self, code=None):
        raise _LocalExit


def interact(banner=None, readfunc=None, local=None, exitmsg=None, local_exit=False):
    """Run an InteractiveConsole on `local` (readfunc replaces
    raw_input(); readline is used if available)."""
    console = InteractiveConsole(local, local_exit=local_exit)
    if readfunc is not None:
        console.raw_input = readfunc
    else:
        try:
            import readline  # noqa: enables line editing for input()
        except ImportError:
            pass
    console.interact(banner, exitmsg)


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument('-q', action='store_true',
                        help="don't print version and copyright messages")
    args = parser.parse_args()
    banner = '' if (args.q or sys.flags.quiet) else None
    interact(banner)
