"""Typing helpers for WSGI (PEP 3333)."""

from collections.abc import Callable, Iterable, Iterator
from types import TracebackType
from typing import Any, Protocol, TypeAlias

__all__ = [
    'StartResponse', 'WSGIEnvironment', 'WSGIApplication',
    'InputStream', 'ErrorStream', 'FileWrapper',
]

_ExcInfo: TypeAlias = tuple[type[BaseException], BaseException, TracebackType]
_OptExcInfo: TypeAlias = _ExcInfo | tuple[None, None, None]


class StartResponse(Protocol):
    """The start_response() callable given to a WSGI application."""

    def __call__(self, status: str, headers: list[tuple[str, str]],
                 exc_info: _OptExcInfo | None = ...,
                 /) -> Callable[[bytes], object]: ...


WSGIEnvironment: TypeAlias = dict[str, Any]
WSGIApplication: TypeAlias = Callable[[WSGIEnvironment, StartResponse],
                                      Iterable[bytes]]


class InputStream(Protocol):
    """The type of wsgi.input."""

    def read(self, size: int = ..., /) -> bytes: ...

    def readline(self, size: int = ..., /) -> bytes: ...

    def readlines(self, hint: int = ..., /) -> list[bytes]: ...

    def __iter__(self) -> Iterator[bytes]: ...


class ErrorStream(Protocol):
    """The type of wsgi.errors."""

    def flush(self) -> object: ...

    def write(self, s: str, /) -> object: ...

    def writelines(self, seq: list[str], /) -> object: ...


class _Readable(Protocol):
    def read(self, size: int = ..., /) -> bytes: ...


class FileWrapper(Protocol):
    """The type of wsgi.file_wrapper."""

    def __call__(self, file: _Readable,
                 block_size: int = ..., /) -> Iterable[bytes]: ...
