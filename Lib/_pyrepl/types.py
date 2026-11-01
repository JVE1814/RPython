"""Type aliases shared by the REPL modules."""

from __future__ import annotations

from typing import Callable, Iterator

type Callback = Callable[[], object]
type SimpleContextManager = Iterator[None]
type KeySpec = str
type CommandName = str
type EventTuple = tuple[CommandName, str]
type Completer = Callable[[str, int], str | None]
type CharBuffer = list[str]
type CharWidths = list[int]
