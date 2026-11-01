"""What the reader wants to show, before it is wrapped to the terminal.

Every buffer line becomes a ``ContentLine``: the source text it came from,
the prompt in front of it (prompts may carry extra lines above the line
itself) and the displayed body, one ``ContentFragment`` per buffer
character.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class ContentFragment:
    """Displayed form of one buffer character (or one prompt line)."""
    text: str
    width: int
    style: str = ""


@dataclass(frozen=True, slots=True)
class SourceLine:
    lineno: int
    text: str
    start_offset: int
    has_newline: bool
    cursor_index: int | None = None

    @property
    def end_offset(self) -> int:
        return self.start_offset + len(self.text) + (1 if self.has_newline else 0)


@dataclass(frozen=True, slots=True)
class PromptContent:
    leading_lines: tuple[ContentFragment, ...]
    text: str
    width: int


@dataclass(frozen=True, slots=True)
class ContentLine:
    source: SourceLine
    prompt: PromptContent
    body: tuple[ContentFragment, ...]


def split_source_lines(text: str, cursor: int | None = None) -> list[SourceLine]:
    """Split buffer *text* into SourceLines (the last one has no newline)."""
    result = []
    offset = 0
    parts = text.split("\n")
    for lineno, part in enumerate(parts):
        has_newline = lineno < len(parts) - 1
        cursor_index = None
        if cursor is not None and offset <= cursor <= offset + len(part):
            cursor_index = cursor - offset
        result.append(SourceLine(lineno, part, offset, has_newline, cursor_index))
        offset += len(part) + 1
    return result
