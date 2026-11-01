"""Helpers for building and combining context managers (``with`` and
``async with`` support)."""

import abc
import os
import sys
import _collections_abc
from collections import deque
from functools import wraps
from types import GenericAlias

__all__ = ["asynccontextmanager", "contextmanager", "closing", "nullcontext",
           "AbstractContextManager", "AbstractAsyncContextManager",
           "AsyncExitStack", "ContextDecorator", "ExitStack",
           "redirect_stdout", "redirect_stderr", "suppress", "aclosing",
           "chdir"]


# ---------------------------------------------------------------------------
# Abstract base classes
# ---------------------------------------------------------------------------

class AbstractContextManager(abc.ABC):
    """Base class for context managers; any class with __enter__ and
    __exit__ counts as a (virtual) subclass."""

    __class_getitem__ = classmethod(GenericAlias)

    __slots__ = ()

    def __enter__(self):
        """Return the context manager itself."""
        return self

    @abc.abstractmethod
    def __exit__(self, exc_type, exc_value, traceback):
        """Clean up; a true result suppresses the exception."""
        return None

    @classmethod
    def __subclasshook__(cls, C):
        if cls is AbstractContextManager:
            return _collections_abc._check_methods(C, "__enter__", "__exit__")
        return NotImplemented


class AbstractAsyncContextManager(abc.ABC):
    """Base class for asynchronous context managers; any class with
    __aenter__ and __aexit__ counts as a (virtual) subclass."""

    __class_getitem__ = classmethod(GenericAlias)

    __slots__ = ()

    async def __aenter__(self):
        """Return the context manager itself."""
        return self

    @abc.abstractmethod
    async def __aexit__(self, exc_type, exc_value, traceback):
        """Clean up; a true result suppresses the exception."""
        return None

    @classmethod
    def __subclasshook__(cls, C):
        if cls is AbstractAsyncContextManager:
            return _collections_abc._check_methods(C, "__aenter__",
                                                   "__aexit__")
        return NotImplemented


# ---------------------------------------------------------------------------
# Using context managers as function decorators
# ---------------------------------------------------------------------------

def _function_kind(func):
    """Classify *func* as 'asyncgen', 'coroutine', 'generator' or 'plain'."""
    import inspect
    if inspect.isasyncgenfunction(func):
        return "asyncgen"
    if inspect.iscoroutinefunction(func):
        return "coroutine"
    if inspect.isgeneratorfunction(func):
        return "generator"
    return "plain"


class ContextDecorator(object):
    """Mixin that lets a context manager be used as a function decorator:
    each call of the decorated function runs inside the context.

    Decorating a generator (or async) function keeps the context active
    for as long as the generator (or coroutine) runs.
    """

    def _recreate_cm(self):
        """Return the context manager to use for one call of a decorated
        function.  One-shot managers override this to make a fresh copy."""
        return self

    def __call__(self, func):
        kind = _function_kind(func)

        if kind == "generator":
            @wraps(func)
            def inner(*args, **kwds):
                with self._recreate_cm():
                    return (yield from func(*args, **kwds))
        elif kind == "coroutine":
            @wraps(func)
            async def inner(*args, **kwds):
                with self._recreate_cm():
                    return await func(*args, **kwds)
        elif kind == "asyncgen":
            @wraps(func)
            async def inner(*args, **kwds):
                with self._recreate_cm():
                    agen = func(*args, **kwds)
                    try:
                        async for item in agen:
                            yield item
                    finally:
                        await agen.aclose()
        else:
            @wraps(func)
            def inner(*args, **kwds):
                with self._recreate_cm():
                    return func(*args, **kwds)
        return inner


class AsyncContextDecorator(object):
    """Mixin that lets an asynchronous context manager be used as a
    function decorator.  The decorated function always becomes
    asynchronous: a coroutine function, or an asynchronous generator
    function when it was a (sync or async) generator function.

    Values passed to asend() and exceptions passed to athrow() of a
    decorated generator are not forwarded to the wrapped generator.
    """

    def _recreate_cm(self):
        """Return the context manager to use for one call of a decorated
        function.  One-shot managers override this to make a fresh copy."""
        return self

    def __call__(self, func):
        kind = _function_kind(func)

        if kind == "asyncgen":
            @wraps(func)
            async def inner(*args, **kwds):
                async with self._recreate_cm():
                    agen = func(*args, **kwds)
                    try:
                        async for item in agen:
                            yield item
                    finally:
                        await agen.aclose()
        elif kind == "generator":
            @wraps(func)
            async def inner(*args, **kwds):
                async with self._recreate_cm():
                    gen = func(*args, **kwds)
                    try:
                        for item in gen:
                            yield item
                    finally:
                        gen.close()
        elif kind == "coroutine":
            @wraps(func)
            async def inner(*args, **kwds):
                async with self._recreate_cm():
                    return await func(*args, **kwds)
        else:
            @wraps(func)
            async def inner(*args, **kwds):
                async with self._recreate_cm():
                    return func(*args, **kwds)
        return inner


# ---------------------------------------------------------------------------
# @contextmanager / @asynccontextmanager
# ---------------------------------------------------------------------------

class _GeneratorCMCommon:
    # State shared by the sync and async generator-based managers.

    def __init__(self, func, args, kwds):
        self.gen = func(*args, **kwds)
        self.func, self.args, self.kwds = func, args, kwds
        # Instances document themselves with the generator function's
        # docstring (falling back to the class docstring).
        doc = getattr(func, "__doc__", None)
        if doc is None:
            doc = type(self).__doc__
        self.__doc__ = doc

    def _recreate_cm(self):
        # The generator can only run once, so a decorated function needs a
        # fresh manager per call.
        return self.__class__(self.func, self.args, self.kwds)

    def _forget_call(self):
        # Once entered the manager can no longer be recreated; drop the
        # call arguments so they are not kept alive by the manager.
        del self.args, self.kwds, self.func


class _GeneratorContextManager(_GeneratorCMCommon, AbstractContextManager,
                               ContextDecorator):
    """Context manager produced by a @contextmanager generator function."""

    def __enter__(self):
        self._forget_call()
        try:
            return next(self.gen)
        except StopIteration:
            raise RuntimeError("generator didn't yield") from None

    def __exit__(self, typ, value, traceback):
        gen = self.gen
        if typ is None:
            try:
                next(gen)
            except StopIteration:
                return False
            try:
                raise RuntimeError("generator didn't stop")
            finally:
                gen.close()

        if value is None:
            # The exception was passed as a bare type.
            value = typ()
        try:
            gen.throw(value)
        except StopIteration as stop:
            # The generator handled the exception and finished: suppress,
            # unless what finished it is the StopIteration we threw in.
            return stop is not value
        except RuntimeError as err:
            if err is value:
                err.__traceback__ = traceback
                return False
            if (isinstance(value, StopIteration)
                    and err.__cause__ is value):
                # The generator machinery turned the StopIteration we threw
                # into a RuntimeError; report the original instead.
                value.__traceback__ = traceback
                return False
            raise
        except BaseException as err:
            if err is not value:
                raise
            # Same exception came back out: let the with statement re-raise
            # it, with the traceback it had on entry.
            err.__traceback__ = traceback
            return False
        try:
            raise RuntimeError("generator didn't stop after throw()")
        finally:
            gen.close()


class _AsyncGeneratorContextManager(_GeneratorCMCommon,
                                    AbstractAsyncContextManager,
                                    AsyncContextDecorator):
    """Context manager produced by an @asynccontextmanager function."""

    async def __aenter__(self):
        self._forget_call()
        try:
            return await anext(self.gen)
        except StopAsyncIteration:
            raise RuntimeError("generator didn't yield") from None

    async def __aexit__(self, typ, value, traceback):
        gen = self.gen
        if typ is None:
            try:
                await anext(gen)
            except StopAsyncIteration:
                return False
            try:
                raise RuntimeError("generator didn't stop")
            finally:
                await gen.aclose()

        if value is None:
            value = typ()
        try:
            await gen.athrow(value)
        except StopAsyncIteration as stop:
            return stop is not value
        except RuntimeError as err:
            if err is value:
                err.__traceback__ = traceback
                return False
            if (isinstance(value, (StopIteration, StopAsyncIteration))
                    and err.__cause__ is value):
                value.__traceback__ = traceback
                return False
            raise
        except BaseException as err:
            if err is not value:
                raise
            err.__traceback__ = traceback
            return False
        try:
            raise RuntimeError("generator didn't stop after athrow()")
        finally:
            await gen.aclose()


def contextmanager(func):
    """Turn a generator function into a context manager factory.

    The generator must yield exactly once: the code before the yield runs
    on entry, the yielded value is bound by ``as``, and the code after it
    runs on exit.  An exception raised in the with block is re-raised at
    the yield, so try/except/finally around the yield works as expected::

        @contextmanager
        def opened(path):
            f = open(path)
            try:
                yield f
            finally:
                f.close()
    """
    @wraps(func)
    def helper(*args, **kwds):
        return _GeneratorContextManager(func, args, kwds)
    return helper


def asynccontextmanager(func):
    """Turn an asynchronous generator function into a factory of
    asynchronous context managers (the ``async with`` counterpart of
    contextmanager())."""
    @wraps(func)
    def helper(*args, **kwds):
        return _AsyncGeneratorContextManager(func, args, kwds)
    return helper


# ---------------------------------------------------------------------------
# Small ready-made context managers
# ---------------------------------------------------------------------------

class closing(AbstractContextManager):
    """Call the close() method of the given object when the block ends.

        with closing(make_connection()) as conn:
            use(conn)
    """

    def __init__(self, thing):
        self.thing = thing

    def __enter__(self):
        return self.thing

    def __exit__(self, *exc_info):
        self.thing.close()


class aclosing(AbstractAsyncContextManager):
    """Await the aclose() method of the given object when the block ends
    (typically used to finalise asynchronous generators deterministically).

        async with aclosing(agen()) as gen:
            async for item in gen:
                ...
    """

    def __init__(self, thing):
        self.thing = thing

    async def __aenter__(self):
        return self.thing

    async def __aexit__(self, *exc_info):
        await self.thing.aclose()


class _StreamSwap(AbstractContextManager):
    # Temporarily replaces one of the sys streams; nests and can be reused.

    _stream = None

    def __init__(self, new_target):
        self._new_target = new_target
        self._saved = []

    def __enter__(self):
        self._saved.append(getattr(sys, self._stream))
        setattr(sys, self._stream, self._new_target)
        return self._new_target

    def __exit__(self, exctype, excinst, exctb):
        setattr(sys, self._stream, self._saved.pop())


class redirect_stdout(_StreamSwap):
    """Send output for sys.stdout to another file-like object while the
    block runs.

        with redirect_stdout(io.StringIO()) as buf:
            help(pow)
        text = buf.getvalue()
    """

    _stream = "stdout"


class redirect_stderr(_StreamSwap):
    """Send output for sys.stderr to another file-like object while the
    block runs."""

    _stream = "stderr"


class suppress(AbstractContextManager):
    """Silently swallow the listed exception types raised in the block.

    Execution continues after the with statement.  For exception groups,
    the matching leaves are dropped and any remainder is re-raised.

        with suppress(FileNotFoundError):
            os.remove(path)
    """

    def __init__(self, *exceptions):
        self._exceptions = exceptions

    def __enter__(self):
        pass

    def __exit__(self, exctype, excinst, exctb):
        if exctype is None:
            return False
        if issubclass(exctype, self._exceptions):
            return True
        if issubclass(exctype, BaseExceptionGroup):
            _, rest = excinst.split(self._exceptions)
            if rest is None:
                return True
            raise rest
        return False


class nullcontext(AbstractContextManager, AbstractAsyncContextManager):
    """A context manager that does nothing except hand back *enter_result*;
    handy where a context manager is optional:

        cm = lock if need_lock else nullcontext()
        with cm:
            ...
    """

    def __init__(self, enter_result=None):
        self.enter_result = enter_result

    def __enter__(self):
        return self.enter_result

    def __exit__(self, *excinfo):
        pass

    async def __aenter__(self):
        return self.enter_result

    async def __aexit__(self, *excinfo):
        pass


class chdir(AbstractContextManager):
    """Change the working directory for the duration of the block (not
    thread-safe: the working directory is process-wide)."""

    def __init__(self, path):
        self.path = path
        self._old_cwd = []

    def __enter__(self):
        self._old_cwd.append(os.getcwd())
        os.chdir(self.path)

    def __exit__(self, *excinfo):
        os.chdir(self._old_cwd.pop())


# ---------------------------------------------------------------------------
# ExitStack / AsyncExitStack
# ---------------------------------------------------------------------------

_NOT_FOUND = object()


def _bound_special_method(obj, name):
    """Find *name* on type(obj) and bind it to *obj* the way the
    interpreter binds special methods; _NOT_FOUND if the type lacks it."""
    owner = type(obj)
    for klass in owner.__mro__:
        if name in klass.__dict__:
            attr = klass.__dict__[name]
            break
    else:
        return _NOT_FOUND
    getter = getattr(type(attr), "__get__", None)
    if getter is None:
        return attr
    return getter(attr, obj, owner)


def _protocol_error(obj, kind, missing):
    cls = type(obj)
    return TypeError(f"'{cls.__module__}.{cls.__qualname__}' object does "
                     f"not support the {kind} protocol "
                     f"(missed {missing} method)")


def _relink_context(new_exc, old_exc, frame_exc):
    """Make the __context__ chain of *new_exc* (raised by an exit callback)
    lead to *old_exc*, the exception that callback was handling, as it
    would with nested with statements.

    Callbacks run while *frame_exc* (the exception the stack itself was
    exited with) is being handled, so the interpreter links new exceptions
    to that one instead.
    """
    if old_exc is frame_exc or new_exc is old_exc:
        return
    seen = set()
    node = new_exc
    while id(node) not in seen:
        seen.add(id(node))
        ctx = node.__context__
        if ctx is old_exc:
            return
        if ctx is frame_exc:
            node.__context__ = old_exc
            return
        if ctx is None:
            # Explicitly cleared by the callback: leave it alone.
            return
        node = ctx


class _ExitStackCommon:
    # Callback registry shared by ExitStack and AsyncExitStack.  Entries
    # of _exit_callbacks are (is_sync, callback) pairs, run in LIFO order.

    def __init__(self):
        self._exit_callbacks = deque()

    def pop_all(self):
        """Move every registered callback to a new stack and return it;
        this stack is left empty."""
        new_stack = type(self)()
        new_stack._exit_callbacks = self._exit_callbacks
        self._exit_callbacks = deque()
        return new_stack

    def push(self, exit):
        """Register an exit callback.

        *exit* is either an object with an __exit__ method (which is
        registered without calling __enter__) or a callable with the
        __exit__ signature.  Returns *exit*, so this works as a decorator.
        """
        method = _bound_special_method(exit, "__exit__")
        if method is _NOT_FOUND:
            self._exit_callbacks.append((True, exit))
        else:
            self._exit_callbacks.append((True, method))
        return exit

    def enter_context(self, cm):
        """Enter *cm* and register its __exit__; returns what __enter__
        returned."""
        enter = _bound_special_method(cm, "__enter__")
        if enter is _NOT_FOUND:
            raise _protocol_error(cm, "context manager", "__enter__")
        exit = _bound_special_method(cm, "__exit__")
        if exit is _NOT_FOUND:
            raise _protocol_error(cm, "context manager", "__exit__")
        result = enter()
        self._exit_callbacks.append((True, exit))
        return result

    def callback(self, callback, /, *args, **kwds):
        """Register ``callback(*args, **kwds)`` to run on exit; its result
        never suppresses exceptions.  Returns *callback*."""
        def _exit_wrapper(exc_type, exc, tb):
            callback(*args, **kwds)
        _exit_wrapper.__wrapped__ = callback
        self._exit_callbacks.append((True, _exit_wrapper))
        return callback


class ExitStack(_ExitStackCommon, AbstractContextManager):
    """Collect cleanup actions and run them, newest first, when the
    with statement ends.

        with ExitStack() as stack:
            files = [stack.enter_context(open(name)) for name in names]
            # every file is closed at the end of the block
    """

    def __enter__(self):
        return self

    def __exit__(self, *exc_details):
        received = exc_details[1] is not None
        frame_exc = sys.exception()
        suppressed = False
        replaced = False
        callbacks = self._exit_callbacks
        while callbacks:
            is_sync, cb = callbacks.pop()
            assert is_sync
            try:
                if cb(*exc_details):
                    suppressed = True
                    replaced = False
                    exc_details = (None, None, None)
            except BaseException as new_exc:
                _relink_context(new_exc, exc_details[1], frame_exc)
                replaced = True
                exc_details = (type(new_exc), new_exc, new_exc.__traceback__)
        if replaced:
            exc = exc_details[1]
            context = exc.__context__
            try:
                raise exc
            finally:
                # Raising here would relink exc to frame_exc.
                exc.__context__ = context
        return received and suppressed

    def close(self):
        """Run all registered callbacks now."""
        self.__exit__(None, None, None)


class AsyncExitStack(_ExitStackCommon, AbstractAsyncContextManager):
    """Asynchronous counterpart of ExitStack: accepts both synchronous and
    asynchronous context managers and callbacks.

        async with AsyncExitStack() as stack:
            conns = [await stack.enter_async_context(connect(h))
                     for h in hosts]
    """

    async def enter_async_context(self, cm):
        """Enter the asynchronous context manager *cm* and register its
        __aexit__; returns what __aenter__ returned."""
        enter = _bound_special_method(cm, "__aenter__")
        if enter is _NOT_FOUND:
            raise _protocol_error(cm, "asynchronous context manager",
                                  "__aenter__")
        exit = _bound_special_method(cm, "__aexit__")
        if exit is _NOT_FOUND:
            raise _protocol_error(cm, "asynchronous context manager",
                                  "__aexit__")
        result = await enter()
        self._exit_callbacks.append((False, exit))
        return result

    def push_async_exit(self, exit):
        """Register an asynchronous exit callback: an object with an
        __aexit__ method, or a coroutine function with that signature.
        Returns *exit*."""
        method = _bound_special_method(exit, "__aexit__")
        if method is _NOT_FOUND:
            self._exit_callbacks.append((False, exit))
        else:
            self._exit_callbacks.append((False, method))
        return exit

    def push_async_callback(self, callback, /, *args, **kwds):
        """Register ``await callback(*args, **kwds)`` to run on exit.
        Returns *callback*."""
        async def _exit_wrapper(exc_type, exc, tb):
            await callback(*args, **kwds)
        _exit_wrapper.__wrapped__ = callback
        self._exit_callbacks.append((False, _exit_wrapper))
        return callback

    async def aclose(self):
        """Run all registered callbacks now."""
        await self.__aexit__(None, None, None)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc_details):
        received = exc_details[1] is not None
        frame_exc = sys.exception()
        suppressed = False
        replaced = False
        callbacks = self._exit_callbacks
        while callbacks:
            is_sync, cb = callbacks.pop()
            try:
                if is_sync:
                    cb_suppress = cb(*exc_details)
                else:
                    cb_suppress = await cb(*exc_details)
                if cb_suppress:
                    suppressed = True
                    replaced = False
                    exc_details = (None, None, None)
            except BaseException as new_exc:
                _relink_context(new_exc, exc_details[1], frame_exc)
                replaced = True
                exc_details = (type(new_exc), new_exc, new_exc.__traceback__)
        if replaced:
            exc = exc_details[1]
            context = exc.__context__
            try:
                raise exc
            finally:
                exc.__context__ = context
        return received and suppressed
