__all__ = ('Runner', 'run')

import contextvars
import enum
import functools
import inspect
import threading
import signal
from . import coroutines
from . import constants
from . import events
from . import exceptions
from . import tasks


class _State(enum.Enum):
    CREATED = "created"
    INITIALIZED = "initialized"
    CLOSED = "closed"


class Runner:
    """Context manager running several async functions in the same loop
    and context.

        with asyncio.Runner(debug=True) as runner:
            runner.run(main())

    On close, remaining tasks are cancelled, async generators are finalized
    and the default executor is shut down.
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

    def close(self):
        """Shutdown and close the event loop."""
        if self._state is not _State.INITIALIZED:
            return
        try:
            loop = self._loop
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

    def get_loop(self):
        """Return the embedded event loop."""
        self._lazy_init()
        return self._loop

    def run(self, coro, *, context=None):
        """Run an awaitable in the embedded loop and return its result."""
        if events._get_running_loop() is not None:
            # fail fast, with a short traceback
            raise RuntimeError(
                "Runner.run() cannot be called from a running event loop")

        self._lazy_init()

        if not coroutines.iscoroutine(coro):
            if inspect.isawaitable(coro):
                async def _wrap_awaitable(awaitable):
                    return await awaitable

                coro = _wrap_awaitable(coro)
            else:
                raise TypeError('An asyncio.Future, a coroutine or an '
                                'awaitable is required')

        if context is None:
            context = self._context

        task = self._loop.create_task(coro, context=context)

        if (threading.current_thread() is threading.main_thread()
            and signal.getsignal(signal.SIGINT) is signal.default_int_handler
        ):
            sigint_handler = functools.partial(self._on_sigint, main_task=task)
            try:
                signal.signal(signal.SIGINT, sigint_handler)
            except ValueError:
                # signals may be unusable even in the main thread
                # (e.g. embedded interpreters)
                sigint_handler = None
        else:
            sigint_handler = None

        self._interrupt_count = 0
        try:
            return self._loop.run_until_complete(task)
        except exceptions.CancelledError:
            if self._interrupt_count > 0:
                uncancel = getattr(task, "uncancel", None)
                if uncancel is not None and uncancel() == 0:
                    raise KeyboardInterrupt()
            raise  # CancelledError
        finally:
            if (sigint_handler is not None
                and signal.getsignal(signal.SIGINT) is sigint_handler
            ):
                signal.signal(signal.SIGINT, signal.default_int_handler)

    def _lazy_init(self):
        if self._state is _State.CLOSED:
            raise RuntimeError("Runner is closed")
        if self._state is _State.INITIALIZED:
            return
        if self._loop_factory is None:
            self._loop = events.new_event_loop()
            if not self._set_event_loop:
                # set the current loop only once per runner
                events.set_event_loop(self._loop)
                self._set_event_loop = True
        else:
            self._loop = self._loop_factory()
        if self._debug is not None:
            self._loop.set_debug(self._debug)
        self._context = contextvars.copy_context()
        self._state = _State.INITIALIZED

    def _on_sigint(self, signum, frame, main_task):
        self._interrupt_count += 1
        if self._interrupt_count == 1 and not main_task.done():
            main_task.cancel()
            # wake the loop up if it sleeps in select()
            self._loop.call_soon_threadsafe(lambda: None)
            return
        raise KeyboardInterrupt()


def run(main, *, debug=None, loop_factory=None):
    """Run a coroutine in a new event loop and return its result.

    The loop is closed afterwards; this cannot be called while another
    event loop is running in the same thread.
    """
    if events._get_running_loop() is not None:
        # fail fast, with a short traceback
        raise RuntimeError(
            "asyncio.run() cannot be called from a running event loop")

    with Runner(debug=debug, loop_factory=loop_factory) as runner:
        return runner.run(main)


def _cancel_all_tasks(loop):
    to_cancel = tasks.all_tasks(loop)
    if not to_cancel:
        return

    for task in to_cancel:
        task.cancel()

    loop.run_until_complete(tasks.gather(*to_cancel, return_exceptions=True))

    for task in to_cancel:
        if task.cancelled():
            continue
        if task.exception() is not None:
            loop.call_exception_handler({
                'message': 'unhandled exception during asyncio.run() shutdown',
                'exception': task.exception(),
                'task': task,
            })
