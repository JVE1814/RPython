"""asyncio.run() and the Runner context manager."""

__all__ = ('Runner', 'run')

import contextvars
import enum
import functools
import inspect
import signal
import threading

from . import constants
from . import coroutines
from . import events
from . import exceptions
from . import tasks


class _State(enum.Enum):
    CREATED = "created"
    INITIALIZED = "initialized"
    CLOSED = "closed"


def _as_coroutine(main):
    if coroutines.iscoroutine(main):
        return main
    if inspect.isawaitable(main):
        async def _adapter():
            return await main
        return _adapter()
    raise TypeError('An asyncio.Future, a coroutine or an awaitable is '
                    'required')


class Runner:
    """Owns an event loop (created lazily) and runs awaitables in it.

    All runs share one context.  close() cancels leftover tasks, finalizes
    async generators, stops the default executor and closes the loop.
    """

    def __init__(self, *, debug=None, loop_factory=None):
        self._state = _State.CREATED
        self._debug = debug
        self._loop_factory = loop_factory
        self._loop = None
        self._context = None
        self._interrupt_count = 0
        self._set_event_loop = False

    def __enter__(self):
        self._lazy_init()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()

    def get_loop(self):
        """The loop used by this runner."""
        self._lazy_init()
        return self._loop

    def close(self):
        if self._state is not _State.INITIALIZED:
            return
        loop = self._loop
        try:
            _cancel_all_tasks(loop)
            loop.run_until_complete(loop.shutdown_asyncgens())
            loop.run_until_complete(
                loop.shutdown_default_executor(constants.THREAD_JOIN_TIMEOUT))
        finally:
            if self._set_event_loop:
                events.set_event_loop(None)
            loop.close()
            self._loop = None
            self._state = _State.CLOSED

    def _lazy_init(self):
        if self._state is _State.CLOSED:
            raise RuntimeError("Runner is closed")
        if self._state is _State.INITIALIZED:
            return
        if self._loop_factory is not None:
            self._loop = self._loop_factory()
        else:
            self._loop = events.new_event_loop()
            if not self._set_event_loop:
                events.set_event_loop(self._loop)
                self._set_event_loop = True
        if self._debug is not None:
            self._loop.set_debug(self._debug)
        self._context = contextvars.copy_context()
        self._state = _State.INITIALIZED

    def _install_sigint(self, main_task):
        if threading.current_thread() is not threading.main_thread():
            return None
        if signal.getsignal(signal.SIGINT) is not signal.default_int_handler:
            return None
        handler = functools.partial(self._on_sigint, main_task=main_task)
        try:
            signal.signal(signal.SIGINT, handler)
        except ValueError:
            return None   # signals unusable here (e.g. embedded)
        return handler

    def run(self, coro, *, context=None):
        """Run *coro* (any awaitable) to completion and return its result."""
        if events._get_running_loop() is not None:
            raise RuntimeError(
                "Runner.run() cannot be called from a running event loop")
        self._lazy_init()
        coro = _as_coroutine(coro)
        task = self._loop.create_task(
            coro, context=self._context if context is None else context)
        handler = self._install_sigint(task)
        self._interrupt_count = 0
        try:
            return self._loop.run_until_complete(task)
        except exceptions.CancelledError:
            # Ctrl-C cancelled the main task: report it as such
            if self._interrupt_count > 0:
                uncancel = getattr(task, "uncancel", None)
                if uncancel is not None and uncancel() == 0:
                    raise KeyboardInterrupt()
            raise
        finally:
            if handler is not None and \
                    signal.getsignal(signal.SIGINT) is handler:
                signal.signal(signal.SIGINT, signal.default_int_handler)

    def _on_sigint(self, signum, frame, main_task):
        self._interrupt_count += 1
        if self._interrupt_count == 1 and not main_task.done():
            main_task.cancel()
            # make sure a loop blocked in select() notices
            self._loop.call_soon_threadsafe(lambda: None)
            return
        raise KeyboardInterrupt()


def run(main, *, debug=None, loop_factory=None):
    """Run the coroutine *main* in a fresh event loop and return its result.

    The loop is closed afterwards.  Cannot be used while another loop runs
    in the same thread.
    """
    if events._get_running_loop() is not None:
        raise RuntimeError(
            "asyncio.run() cannot be called from a running event loop")
    with Runner(debug=debug, loop_factory=loop_factory) as runner:
        return runner.run(main)


def _cancel_all_tasks(loop):
    leftovers = tasks.all_tasks(loop)
    if not leftovers:
        return
    for task in leftovers:
        task.cancel()
    loop.run_until_complete(tasks.gather(*leftovers, return_exceptions=True))
    for task in leftovers:
        if not task.cancelled() and task.exception() is not None:
            loop.call_exception_handler({
                'message': 'unhandled exception during asyncio.run() shutdown',
                'exception': task.exception(),
                'task': task,
            })
