"""Multiple interpreters in one process (PEP 734).

Each interpreter has its own modules, builtins and __main__; code runs in an
interpreter with Interpreter.exec() or Interpreter.call(), and values move
between interpreters as copies (see is_shareable()) or through queues.
"""

import threading
import weakref

import _interpreters
from _interpreters import (
    InterpreterError, InterpreterNotFoundError, NotShareableError,
    is_shareable,
)
from ._queues import (
    create as create_queue,
    Queue, QueueEmpty, QueueFull,
)


__all__ = [
    'get_current', 'get_main', 'create', 'list_all', 'is_shareable',
    'Interpreter',
    'InterpreterError', 'InterpreterNotFoundError', 'ExecutionFailed',
    'NotShareableError',
    'create_queue', 'Queue', 'QueueEmpty', 'QueueFull',
]


_EXEC_FAILURE_STR = """
{superstr}

Uncaught in the interpreter:

{formatted}
""".strip()


class ExecutionFailed(InterpreterError):
    """An exception was raised, and not caught, in another interpreter.

    "excinfo" describes it: .type (with __name__, __qualname__, __module__),
    .msg, .formatted and .errdisplay (the traceback as text).
    """

    def __init__(self, excinfo):
        msg = getattr(excinfo, 'formatted', None)
        if not msg:
            exctype = getattr(excinfo, 'type', None)
            excmsg = getattr(excinfo, 'msg', None)
            name = getattr(exctype, '__name__', None)
            if name and excmsg:
                msg = f'{name}: {excmsg}'
            else:
                msg = name or excmsg or ''
        super().__init__(msg)
        self.excinfo = excinfo

    def __str__(self):
        display = getattr(self.excinfo, 'errdisplay', None)
        if not display:
            return super().__str__()
        return _EXEC_FAILURE_STR.format(
            superstr=super().__str__(),
            formatted=display.rstrip('\n'),
        )

    def resolve(self):
        """The original exception, re-created here, or None if that was not
        possible."""
        return getattr(self.excinfo, 'exc', None)


def create():
    """Return a new (idle) interpreter."""
    interpid = _interpreters.create(reqrefs=True)
    return Interpreter(interpid, _ownsref=True)


def list_all():
    """Return all existing interpreters."""
    return [Interpreter(interpid, _whence=whence)
            for interpid, whence in _interpreters.list_all(require_ready=True)]


def get_current():
    """Return the interpreter running the calling code."""
    interpid, whence = _interpreters.get_current()
    return Interpreter(interpid, _whence=whence)


def get_main():
    """Return the main interpreter."""
    interpid, whence = _interpreters.get_main()
    assert whence == _interpreters.WHENCE_RUNTIME, repr(whence)
    return Interpreter(interpid, _whence=whence)


_WHENCE_TO_STR = {
    _interpreters.WHENCE_UNKNOWN: 'unknown',
    _interpreters.WHENCE_RUNTIME: 'runtime init',
    _interpreters.WHENCE_LEGACY_CAPI: 'legacy C-API',
    _interpreters.WHENCE_CAPI: 'C-API',
    _interpreters.WHENCE_XI: 'cross-interpreter C-API',
    _interpreters.WHENCE_STDLIB: '_interpreters module',
}


# one object per interpreter (and per interpreter that holds them)
_known = weakref.WeakValueDictionary()
_known_lock = threading.Lock()


class Interpreter:
    """A single interpreter in the current process."""

    def __new__(cls, id, /, _whence=None, _ownsref=None):
        # an int, not just anything with __index__
        if not isinstance(id, int):
            raise TypeError(f'id must be an int, got {id!r}')
        if _whence is None:
            if _ownsref:
                _whence = _interpreters.WHENCE_STDLIB
            else:
                _whence = _interpreters.whence(id)
        assert _whence in _WHENCE_TO_STR, repr(_whence)
        if _ownsref is None:
            _ownsref = (_whence == _interpreters.WHENCE_STDLIB)
        with _known_lock:
            try:
                self = _known[id]
                assert hasattr(self, '_ownsref')
            except KeyError:
                self = super().__new__(cls)
                self._id = id
                self._whence = _whence
                self._ownsref = _ownsref
                if _ownsref:
                    # keeps the interpreter alive as long as this object
                    _interpreters.incref(id)
                _known[id] = self
        return self

    def __repr__(self):
        return f'{type(self).__name__}({self.id})'

    def __hash__(self):
        return hash(self._id)

    def __eq__(self, other):
        if not isinstance(other, Interpreter):
            return NotImplemented
        return self._id == other._id

    def __reduce__(self):
        return (type(self), (self._id,))

    def __del__(self):
        self._decref()

    # bound at class creation: may run during an interpreter's teardown, after
    # this module's globals were cleared (see _queues.Queue.__del__)
    def _decref(self, _getattr=getattr, _decref=_interpreters.decref,
                _notfound=InterpreterNotFoundError):
        if not _getattr(self, '_ownsref', False):
            return
        self._ownsref = False
        try:
            _decref(self._id)
        except _notfound:
            pass

    @property
    def id(self):
        return self._id

    @property
    def whence(self):
        return _WHENCE_TO_STR[self._whence]

    def is_running(self):
        """Is the interpreter currently running code in its __main__?"""
        return _interpreters.is_running(self._id)

    def close(self):
        """Finalize and destroy the interpreter.

        Attempting to destroy the current interpreter raises InterpreterError.
        """
        return _interpreters.destroy(self._id, restrict=True)

    def prepare_main(self, ns=None, /, **kwargs):
        """Bind the given values in the interpreter's __main__ module.

        The values must be shareable.
        """
        updates = {}
        if ns is not None:
            updates.update(ns)
        updates.update(kwargs)
        if not updates:
            raise ValueError('at least one argument required')
        _interpreters.set___main___attrs(self._id, updates, restrict=True)

    def exec(self, code, /):
        """Run the code (a string, or a function or code object that takes no
        arguments) in the interpreter's __main__ module.

        An exception not caught there is raised here as ExecutionFailed.
        Only one thread may run code in an interpreter's __main__ at a time.
        """
        excinfo = _interpreters.exec(self._id, code, restrict=True)
        if excinfo is not None:
            raise ExecutionFailed(excinfo)

    def _call(self, callable, args, kwargs):
        res, excinfo = _interpreters.call(self._id, callable, args, kwargs,
                                          preserve_exc=True, restrict=True)
        if excinfo is not None:
            raise ExecutionFailed(excinfo)
        return res

    def call(self, callable, /, *args, **kwargs):
        """Call the object in the interpreter and return the result.

        The callable, the arguments and the return value must be shareable
        or picklable.  An uncaught exception is raised here as
        ExecutionFailed.
        """
        return self._call(callable, args, kwargs)

    def call_in_thread(self, callable, /, *args, **kwargs):
        """Like call(), but in a new thread, which is returned (started)."""
        t = threading.Thread(target=self._call, args=(callable, args, kwargs))
        t.start()
        return t
