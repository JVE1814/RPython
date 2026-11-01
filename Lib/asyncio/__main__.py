"""python -m asyncio: an interactive console with top-level await, and the
'ps' / 'pstree' commands showing the tasks of another process.

The event loop runs in the main thread (so that Ctrl-C reaches it); the
prompt loop runs in a daemon thread and hands each compiled statement to
the loop, waiting for its outcome.
"""

import ast
import asyncio
import code
import contextvars
import inspect
import os
import site
import sys
import threading
import warnings



class _Outcome:
    """Result of a statement run on the loop thread, waited for by the
    console thread."""

    def __init__(self):
        self._done = threading.Event()
        self._value = None
        self._error = None

    def set(self, value=None, error=None):
        self._value, self._error = value, error
        self._done.set()

    def wait(self):
        self._done.wait()
        error, self._error = self._error, None
        if error is not None:
            try:
                raise error
            finally:
                error = None
        return self._value


_WRAPPER_NAME = "<module>"
_INTERNAL_FILES = frozenset([__file__, "<_asyncio>"])


class _BoundNames(ast.NodeVisitor):
    """Names a block of top-level statements binds in its own scope (not
    inside nested functions, classes, lambdas or comprehensions)."""

    def __init__(self):
        self.names = set()

    def visit_Name(self, node):
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            self.names.add(node.id)

    def _scope(self, node):
        self.names.add(node.name)
        for deco in node.decorator_list:
            self.visit(deco)

    visit_FunctionDef = visit_AsyncFunctionDef = visit_ClassDef = _scope

    def visit_Lambda(self, node):
        pass

    def _comprehension(self, node):
        # only assignment expressions leak out of a comprehension
        for sub in ast.walk(node):
            if isinstance(sub, ast.NamedExpr) and \
                    isinstance(sub.target, ast.Name):
                self.names.add(sub.target.id)

    visit_ListComp = visit_SetComp = visit_DictComp = visit_GeneratorExp = \
        _comprehension

    def visit_alias(self, node):
        if node.name != "*":
            self.names.add(node.asname or node.name.partition(".")[0])

    def visit_ExceptHandler(self, node):
        if node.name:
            self.names.add(node.name)
        self.generic_visit(node)

    def visit_MatchAs(self, node):
        if node.name:
            self.names.add(node.name)
        self.generic_visit(node)

    def visit_MatchStar(self, node):
        if node.name:
            self.names.add(node.name)

    def visit_MatchMapping(self, node):
        if node.rest:
            self.names.add(node.rest)
        self.generic_visit(node)


def _display_call(value):
    # sys.displayhook(value), as the interactive mode does for expressions
    hook = ast.Attribute(
        value=ast.Call(func=ast.Name(id="__import__", ctx=ast.Load()),
                       args=[ast.Constant(value="sys")], keywords=[]),
        attr="displayhook", ctx=ast.Load())
    return ast.Call(func=hook, args=[value], keywords=[])


def _compile_awaiting(source, filename):
    """Compile interactive input containing top-level 'await' into a module
    defining a coroutine function that runs it: the statements bind names
    in the console namespace (declared global) and expression statements
    are displayed."""
    tree = compile(source, filename, "single",
                   ast.PyCF_ONLY_AST | ast.PyCF_ALLOW_TOP_LEVEL_AWAIT)
    finder = _BoundNames()
    for stmt in tree.body:
        finder.visit(stmt)
    body = []
    for stmt in tree.body:
        if isinstance(stmt, ast.Expr):
            stmt = ast.copy_location(ast.Expr(value=_display_call(stmt.value)),
                                     stmt)
        body.append(stmt)
    if finder.names:
        body.insert(0, ast.copy_location(
            ast.Global(names=sorted(finder.names)), tree.body[0]))
    func = ast.AsyncFunctionDef(
        name=_WRAPPER_NAME,
        args=ast.arguments(posonlyargs=[], args=[], vararg=None,
                           kwonlyargs=[], kw_defaults=[], kwarg=None,
                           defaults=[]),
        body=body, decorator_list=[], returns=None, type_params=[])
    ast.copy_location(func, tree.body[0])
    module = ast.Module(body=[func], type_ignores=[])
    ast.fix_missing_locations(module)
    return compile(module, filename, "exec")


class AsyncIOInteractiveConsole(code.InteractiveConsole):
    """Console whose statements may use 'await'; they run on *loop* (as
    tasks when they await) in one shared contextvars context."""

    def __init__(self, locals, loop):
        super().__init__(locals, filename="<stdin>")
        self.compile.compiler.flags |= ast.PyCF_ALLOW_TOP_LEVEL_AWAIT
        self.loop = loop
        self.context = contextvars.copy_context()
        self._source = None

    def runsource(self, source, filename="<input>", symbol="single"):
        self._source = (source, filename)
        return super().runsource(source, filename, symbol)

    def _start(self, code_obj, outcome):
        # runs on the loop thread
        global _current_task, _interrupted
        _current_task = None
        _interrupted = False
        try:
            if code_obj.co_flags & inspect.CO_COROUTINE:
                source, filename = self._source
                wrapper = _compile_awaiting(source, filename)
                exec(wrapper, self.locals)
                result = self.locals.pop(_WRAPPER_NAME)()
            else:
                result = exec(code_obj, self.locals)
        except SystemExit as exc:
            _request_exit(exc.code)
            return
        except KeyboardInterrupt as exc:
            _interrupted = True
            outcome.set(error=exc)
            return
        except BaseException as exc:
            outcome.set(error=exc)
            return
        if not inspect.iscoroutine(result):
            outcome.set(value=result)
            return
        try:
            task = self.loop.create_task(result, context=self.context)
        except BaseException as exc:
            outcome.set(error=exc)
            return
        _current_task = task

        def finished(task):
            if task.cancelled():
                outcome.set(error=KeyboardInterrupt() if _interrupted
                            else asyncio.CancelledError())
                return
            exc = task.exception()
            if isinstance(exc, SystemExit):
                _request_exit(exc.code)
            elif exc is not None:
                outcome.set(error=exc)
            else:
                outcome.set(value=task.result())

        task.add_done_callback(finished)

    def runcode(self, code_obj):
        outcome = _Outcome()
        self.loop.call_soon_threadsafe(self._start, code_obj, outcome,
                                       context=self.context)
        try:
            return outcome.wait()
        except SystemExit:
            raise
        except BaseException as exc:
            if _interrupted:
                self.write("\nKeyboardInterrupt\n")
            else:
                self._report_error(exc)

    def _report_error(self, exc):
        # hide the frames of this module and of the task machinery
        tb = exc.__traceback__
        while tb is not None and \
                tb.tb_frame.f_code.co_filename in _INTERNAL_FILES:
            tb = tb.tb_next
        exc.__traceback__ = tb
        sys.last_type, sys.last_value, sys.last_traceback = \
            type(exc), exc, tb
        sys.last_exc = exc
        if sys.excepthook is sys.__excepthook__:
            import traceback
            self.write("".join(traceback.format_exception(exc)))
        else:
            sys.excepthook(type(exc), exc, tb)


def _request_exit(status):
    global _exit_status
    _exit_status = status
    _loop.stop()


def _run_startup_file(console):
    if sys.flags.ignore_environment:
        return
    path = os.environ.get("PYTHONSTARTUP")
    if not path:
        return
    sys.audit("cpython.run_startup", path)
    import tokenize
    try:
        with tokenize.open(path) as f:
            startup = compile(f.read(), path, "exec")
    except SyntaxError:
        console.showsyntaxerror()
        return
    except OSError:
        console.showtraceback()
        return
    try:
        exec(startup, console.locals)
    except SystemExit:
        raise
    except BaseException:
        console.showtraceback()


def _pyrepl_usable():
    if os.environ.get("PYTHON_BASIC_REPL"):
        return False
    try:
        from _pyrepl.main import CAN_USE_PYREPL
    except ImportError:
        return False
    return CAN_USE_PYREPL


class REPLThread(threading.Thread):

    def run(self):
        try:
            if not sys.flags.quiet:
                console.write(
                    f'asyncio REPL {sys.version} on {sys.platform}\n'
                    f'Use "await" directly instead of "asyncio.run()".\n'
                    f'Type "help", "copyright", "credits" or "license" '
                    f'for more information.\n')
            _run_startup_file(console)
            if _pyrepl_usable():
                from _pyrepl.simple_interact import (
                    run_multiline_interactive_console)
                run_multiline_interactive_console(console)
            else:
                console.interact(banner="", exitmsg="")
        except SystemExit as exc:
            global _exit_status
            _exit_status = exc.code
        finally:
            # coroutines typed but never awaited are not worth a warning
            # at exit
            warnings.filterwarnings(
                'ignore', message=r'^coroutine .* was never awaited$',
                category=RuntimeWarning)
            _loop.call_soon_threadsafe(_loop.stop)


def _inspect_other_process(argv):
    import argparse
    parser = argparse.ArgumentParser(
        prog="python -m asyncio",
        description="Interactive asyncio shell and tools for inspecting "
                    "the tasks of a running process")
    commands = parser.add_subparsers(dest="command", help="sub-commands")
    for name, what in (("ps", "a table"), ("pstree", "a tree")):
        sub = commands.add_parser(
            name, help=f"Display {what} of all pending tasks in a process")
        sub.add_argument("pid", type=int, help="Process ID to inspect")
    args = parser.parse_args(argv)
    if args.command is None:
        return False
    from asyncio import tools
    if args.command == "ps":
        tools.display_awaited_by_tasks_table(args.pid)
    else:
        tools.display_awaited_by_tasks_tree(args.pid)
    return True


_exit_status = 0
_current_task = None
_interrupted = False

if __name__ == '__main__':
    if _inspect_other_process(sys.argv[1:]):
        sys.exit(0)

    sys.audit("cpython.run_stdin")

    _loop = asyncio.new_event_loop()
    asyncio.set_event_loop(_loop)

    repl_locals = {'asyncio': asyncio}
    for key in ('__name__', '__package__', '__loader__', '__spec__',
                '__builtins__', '__file__'):
        if key in globals():
            repl_locals[key] = globals()[key]
    console = AsyncIOInteractiveConsole(repl_locals, _loop)

    try:
        import readline
    except ImportError:
        readline = None
    hook = getattr(sys, "__interactivehook__", None)
    if hook is not None:
        sys.audit("cpython.run_interactivehook", hook)
        hook()
    if readline is not None and hook is site.register_readline:
        # complete names from the console's namespace
        try:
            import rlcompleter
        except ImportError:
            pass
        else:
            readline.set_completer(
                rlcompleter.Completer(console.locals).complete)

    repl_thread = REPLThread(name="Interactive thread", daemon=True)
    repl_thread.start()

    while True:
        try:
            _loop.run_forever()
        except KeyboardInterrupt:
            # interrupt the statement being awaited, keep the console
            _interrupted = True
            if _current_task is not None and not _current_task.done():
                _current_task.cancel()
            continue
        break

    console.write('exiting asyncio REPL...\n')
    _loop.close()
    sys.exit(_exit_status)
