"""Protocols describing what importlib.metadata needs from metadata and
path objects."""

from __future__ import annotations

import os
from collections.abc import Iterator
from typing import Any, Protocol, TypeVar, overload

_T = TypeVar("_T")


class PackageMetadata(Protocol):
    """Read access to the core metadata fields of a distribution."""

    def __len__(self) -> int: ...  # pragma: no cover

    def __contains__(self, item: str) -> bool: ...  # pragma: no cover

    def __getitem__(self, key: str) -> str: ...  # pragma: no cover

    def __iter__(self) -> Iterator[str]: ...  # pragma: no cover

    @overload
    def get(self, name: str, failobj: None = None) -> str | None: ...

    @overload
    def get(self, name: str, failobj: _T) -> str | _T: ...

    def get(self, name, failobj=None): ...  # pragma: no cover

    @overload
    def get_all(self, name: str, failobj: None = None) -> list[Any] | None: ...

    @overload
    def get_all(self, name: str, failobj: _T) -> list[Any] | _T: ...

    def get_all(self, name, failobj=None):
        """Return all values of the field *name*."""

    @property
    def json(self) -> dict[str, str | list[str]]:
        """The metadata as a JSON-compatible dict (PEP 566 conversion)."""


class SimplePath(Protocol):
    """The path operations a PathDistribution needs (pathlib.Path and
    zipfile.Path both qualify)."""

    def joinpath(
        self, other: str | os.PathLike[str]
    ) -> SimplePath: ...  # pragma: no cover

    def __truediv__(
        self, other: str | os.PathLike[str]
    ) -> SimplePath: ...  # pragma: no cover

    @property
    def parent(self) -> SimplePath: ...  # pragma: no cover

    def read_text(self, encoding=None) -> str: ...  # pragma: no cover

    def read_bytes(self) -> bytes: ...  # pragma: no cover

    def exists(self) -> bool: ...  # pragma: no cover
