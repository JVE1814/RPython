# Structured concurrency: a group of tasks awaited together.

__all__ = ("TaskGroup",)

from . import events
from . import exceptions
from . import futures
from . import tasks


class TaskGroup:
    """Asynchronous context manager owning a group of tasks.

        async with asyncio.TaskGroup() as group:
            task1 = group.create_task(some_coroutine(...))
            task2 = group.create_task(other_coroutine(...))

    Leaving the block waits for all tasks.  If a task fails, the remaining
    tasks (and the body of the block) are cancelled and the errors are
    raised together as an ExceptionGroup.
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
        self._on_completed_fut = None

    def __repr__(self):
        info = ['']
        if self._tasks:
            info.append(f'tasks={len(self._tasks)}')
        if self._errors:
            info.append(f'errors={len(self._errors)}')
        if self._aborting:
            info.append('cancelling')
        elif self._entered:
            info.append('entered')

        info_str = ' '.join(info)
        return f'<TaskGroup{info_str}>'

    async def __aenter__(self):
        if self._entered:
            raise RuntimeError(
                f"TaskGroup {self!r} has already been entered")
        if self._loop is None:
            self._loop = events.get_running_loop()
        self._parent_task = tasks.current_task(self._loop)
        if self._parent_task is None:
            raise RuntimeError(
                f'TaskGroup {self!r} cannot determine the parent task')
        self._entered = True

        return self

    async def __aexit__(self, et, exc, tb):
        tb = None
        try:
            return await self._aexit(et, exc)
        finally:
            # drop references to (cyclic, heavy) exceptions
            self._parent_task = None
            self._errors = None
            self._base_error = None
            exc = None

    async def _aexit(self, et, exc):
        self._exiting = True

        if (exc is not None and
                self._is_base_error(exc) and
                self._base_error is None):
            self._base_error = exc

        if et is not None and issubclass(et, exceptions.CancelledError):
            propagate_cancellation_error = exc
        else:
            propagate_cancellation_error = None

        if et is not None:
            if not self._aborting:
                # the body failed or the parent task is being cancelled
                self._abort()

        # _on_completed_fut may be cancelled repeatedly (the parent task
        # can be cancelled several times): loop until all tasks are done.
        while self._tasks:
            if self._on_completed_fut is None:
                self._on_completed_fut = self._loop.create_future()

            try:
                await self._on_completed_fut
            except exceptions.CancelledError as ex:
                if not self._aborting:
                    # the parent task got cancelled while waiting for
                    # the children
                    propagate_cancellation_error = ex
                    self._abort()

            self._on_completed_fut = None

        assert not self._tasks

        if self._base_error is not None:
            try:
                raise self._base_error
            finally:
                exc = None

        if self._parent_cancel_requested:
            # balance our own cancel() of the parent task
            if self._parent_task.uncancel() == 0:
                # no other cancellation pending: don't propagate
                propagate_cancellation_error = None

        # A CancelledError propagates, unless there are real errors.
        try:
            if propagate_cancellation_error is not None and not self._errors:
                try:
                    raise propagate_cancellation_error
                finally:
                    exc = None
        finally:
            propagate_cancellation_error = None

        if et is not None and not issubclass(et, exceptions.CancelledError):
            self._errors.append(exc)

        if self._errors:
            # Keep the cancellation count of an externally cancelled parent
            # stable: uncancel() then cancel() again.
            if self._parent_task.cancelling():
                self._parent_task.uncancel()
                self._parent_task.cancel()
            try:
                raise BaseExceptionGroup(
                    'unhandled errors in a TaskGroup',
                    self._errors,
                ) from None
            finally:
                exc = None

    def create_task(self, coro, **kwargs):
        """Create a new task in this group and return it.

        Same arguments as asyncio.create_task().
        """
        if not self._entered:
            coro.close()
            raise RuntimeError(f"TaskGroup {self!r} has not been entered")
        if self._exiting and not self._tasks:
            coro.close()
            raise RuntimeError(f"TaskGroup {self!r} is finished")
        if self._aborting:
            coro.close()
            raise RuntimeError(f"TaskGroup {self!r} is shutting down")
        task = self._loop.create_task(coro, **kwargs)

        futures.future_add_to_awaited_by(task, self._parent_task)

        # Register the callback even for an (eagerly) finished task, so a
        # failure is handled through the normal path.
        self._tasks.add(task)
        task.add_done_callback(self._on_task_done)
        try:
            return task
        finally:
            # avoid a cycle task.exception().__traceback__ -> frame -> task
            del task

    # SystemExit and KeyboardInterrupt are not wrapped in an ExceptionGroup:
    # they are re-raised as-is.
    def _is_base_error(self, exc: BaseException) -> bool:
        assert isinstance(exc, BaseException)
        return isinstance(exc, (SystemExit, KeyboardInterrupt))

    def _abort(self):
        self._aborting = True

        for t in self._tasks:
            if not t.done():
                t.cancel()

    def _on_task_done(self, task):
        self._tasks.discard(task)

        futures.future_discard_from_awaited_by(task, self._parent_task)

        if self._on_completed_fut is not None and not self._tasks:
            if not self._on_completed_fut.done():
                self._on_completed_fut.set_result(True)

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
            # Cancel the parent so that the body of the "async with" stops;
            # __aexit__ will uncancel() it again.
            self._abort()
            self._parent_cancel_requested = True
            self._parent_task.cancel()
