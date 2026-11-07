"""InterpreterPoolExecutor: a thread pool where every worker thread runs its
tasks in its own isolated interpreter (concurrent.interpreters)."""

from concurrent import futures
from concurrent.futures import thread as _thread_pool


class BrokenInterpreterPool(_thread_pool.BrokenThreadPool):
    """A worker's initializer failed: the pool cannot be used any more."""


def _interpreters():
    # imported on first use: the module needs subinterpreter support
    from concurrent import interpreters
    return interpreters


class WorkerContext(_thread_pool.WorkerContext):
    """The interpreter owned by one worker thread."""

    @classmethod
    def prepare(cls, initializer, initargs):
        def resolve_task(fn, args, kwargs):
            if isinstance(fn, str):
                raise TypeError(f'scripts not supported ({fn!r})')
            return (fn, args, kwargs)

        if initializer is not None:
            try:
                initdata = resolve_task(initializer, initargs, {})
            except ValueError:
                if isinstance(initializer, str) and initargs:
                    raise ValueError(f'an initializer script does not take '
                                     f'args, got {initargs!r}')
                raise
        else:
            initdata = None

        def create_context():
            return cls(initdata)
        return create_context, resolve_task

    def __init__(self, initdata):
        self.initdata = initdata
        self.interp = None

    def __del__(self):
        if self.interp is not None:
            self.finalize()

    def initialize(self):
        assert self.interp is None, self.interp
        self.interp = _interpreters().create()
        try:
            if self.initdata:
                self.run(self.initdata)
        except BaseException:
            self.finalize()
            raise

    def finalize(self):
        interp, self.interp = self.interp, None
        if interp is not None:
            interp.close()

    def run(self, task):
        fn, args, kwargs = task
        try:
            return self.interp.call(fn, *args, **kwargs)
        except _interpreters().ExecutionFailed as wrapper:
            exc = wrapper.resolve()
            if exc is None:
                raise
            raise exc from wrapper


class InterpreterPoolExecutor(_thread_pool.ThreadPoolExecutor):

    BROKEN = BrokenInterpreterPool

    @classmethod
    def prepare_context(cls, initializer, initargs):
        return WorkerContext.prepare(initializer, initargs)

    def __init__(self, max_workers=None, thread_name_prefix='',
                 initializer=None, initargs=()):
        """Like ThreadPoolExecutor, but each worker thread runs the tasks in
        its own interpreter.  Tasks and their results must be shareable
        between interpreters (in practice: picklable)."""
        thread_name_prefix = (thread_name_prefix or
                              f'InterpreterPoolExecutor-{self._counter()}')
        super().__init__(max_workers, thread_name_prefix,
                         initializer, initargs)
