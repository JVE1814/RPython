"""TaskGroup: run tasks in a scope that waits for all of them."""

__all__ = ("TaskGroup",)

from . import events
from . import exceptions
from . import futures
from . import tasks


class TaskGroup:
    """Async context manager owning the tasks created through it.

    Exiting the block waits for every task.  The first failing task makes
    the group cancel the others (and the body of the block); all failures
    are then raised together as an ExceptionGroup.  KeyboardInterrupt and
    SystemExit are re-raised as they are.
    """

    def __init__(self):
        self._entered = False
        self._exiting = False
        self._aborting = False
        self._loop = None
        self._parent_task = None
        self._parent_cancel_requested = False
        self._tasks = set()
        self._errors = []
        self._base_error = None
        self._all_done = None   # future resolved when _tasks empties
        self._cancel_on_enter = False

    def __repr__(self):
        words = ['<TaskGroup']
        if self._tasks:
            words.append(f'tasks={len(self._tasks)}')
        if self._errors:
            words.append(f'errors={len(self._errors)}')
        if self._aborting:
            words.append('cancelling')
        elif self._entered:
            words.append('entered')
        return ' '.join(words) + '>'

    async def __aenter__(self):
        if self._entered:
            raise RuntimeError(f"TaskGroup {self!r} has already been entered")
        if self._loop is None:
            self._loop = events.get_running_loop()
        self._parent_task = tasks.current_task(self._loop)
        if self._parent_task is None:
            raise RuntimeError(
                f'TaskGroup {self!r} cannot determine the parent task')
        self._entered = True
        if self._cancel_on_enter:
            self._cancel_parent()
        return self

    async def __aexit__(self, et, exc, tb):
        tb = None
        try:
            return await self._aexit(et, exc)
        finally:
            # exceptions and their tracebacks form cycles: drop them
            self._parent_task = self._errors = self._base_error = None
            exc = None

    @staticmethod
    def _is_base_error(exc):
        return isinstance(exc, (SystemExit, KeyboardInterrupt))

    async def _aexit(self, et, exc):
        self._exiting = True
        if exc is not None and self._is_base_error(exc) \
                and self._base_error is None:
            self._base_error = exc
        body_cancelled = et is not None and \
            issubclass(et, exceptions.CancelledError)
        cancellation = exc if body_cancelled else None

        if et is not None and not self._aborting:
            # the body raised (or the parent task got cancelled)
            self._abort()

        while self._tasks:
            if self._all_done is None:
                self._all_done = self._loop.create_future()
            try:
                await self._all_done
            except exceptions.CancelledError as again:
                # the parent was cancelled while we wait for the children
                # keep the most recent cancellation (it may carry a
                # message from an outside caller)
                cancellation = again
                if not self._aborting:
                    self._abort()
            self._all_done = None

        if self._base_error is not None:
            try:
                raise self._base_error
            finally:
                exc = cancellation = None

        if self._parent_cancel_requested and self._parent_task.uncancel() == 0:
            # we were the only one asking for the parent's cancellation
            cancellation = None

        if (et is not None and issubclass(et, GeneratorExit) and
                not self._errors):
            return None     # closing an async generator: let it propagate
        if et is not None and not body_cancelled:
            self._errors.append(exc)

        try:
            if cancellation is not None and not self._errors:
                raise cancellation
        finally:
            exc = cancellation = None
        if not self._errors:
            # a cancellation caused by the group itself ends here
            return True if body_cancelled else None
        if self._parent_task.cancelling():
            # keep an external cancellation request pending
            self._parent_task.uncancel()
            self._parent_task.cancel()
        try:
            raise BaseExceptionGroup('unhandled errors in a TaskGroup',
                                     self._errors) from None
        finally:
            exc = None

    def create_task(self, coro, **kwargs):
        """Create a task belonging to this group (same arguments as
        asyncio.create_task())."""
        if not self._entered:
            problem = "has not been entered"
        elif self._exiting and not self._tasks:
            problem = "is finished"
        elif self._aborting:
            problem = "is shutting down"
        else:
            problem = None
        if problem is not None:
            coro.close()
            raise RuntimeError(f"TaskGroup {self!r} {problem}")
        task = self._loop.create_task(coro, **kwargs)
        futures.future_add_to_awaited_by(task, self._parent_task)
        # registered even if already finished (eager start), so that a
        # failure goes through _on_task_done like any other
        self._tasks.add(task)
        task.add_done_callback(self._on_task_done)
        if self._aborting and not task.done():
            # an eagerly started task cancelled the group before it was
            # registered here
            task.cancel()
        try:
            return task
        finally:
            del task

    def _cancel_parent(self):
        self._abort()
        if not self._parent_cancel_requested:
            self._parent_cancel_requested = True
            self._parent_task.cancel()

    def cancel(self):
        """Cancel the group: its tasks and the body of the "async with"
        block are cancelled, and the block exits without an error (unless
        tasks failed).  Calling it again or after the block is harmless."""
        if self._aborting:
            return
        if not self._entered:
            self._cancel_on_enter = True
            return
        if self._exiting:
            if self._tasks:
                self._abort()   # the body is done: only stop the children
            return
        self._cancel_parent()

    def _abort(self):
        self._aborting = True
        for task in self._tasks:
            if not task.done():
                task.cancel()

    def _on_task_done(self, task):
        self._tasks.discard(task)
        futures.future_discard_from_awaited_by(task, self._parent_task)
        if not self._tasks and self._all_done is not None \
                and not self._all_done.done():
            self._all_done.set_result(True)

        if task.cancelled():
            return
        exc = task.exception()
        if exc is None:
            return
        self._errors.append(exc)
        if self._is_base_error(exc) and self._base_error is None:
            self._base_error = exc

        if self._parent_task.done():
            self._loop.call_exception_handler({
                'message': f'Task {task!r} has errored out but its parent '
                           f'task {self._parent_task} is already completed',
                'exception': exc,
                'task': task,
            })
            return
        if not self._aborting and not self._parent_cancel_requested:
            # stop the body of the "async with"; _aexit() undoes the
            # cancellation request with uncancel()
            self._abort()
            self._parent_cancel_requested = True
            self._parent_task.cancel()
