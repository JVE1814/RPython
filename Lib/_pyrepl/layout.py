"""Wrapping content lines to the terminal width and mapping between
buffer positions and screen coordinates."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

from .content import ContentFragment, ContentLine

WRAP_MARKER = "\\"


@dataclass(frozen=True, slots=True)
class LayoutRow:
    """Geometry of one screen row: prompt width, the width of each buffer
    character shown on it, an optional wrap marker, and how many buffer
    positions the row accounts for (characters plus a trailing newline)."""
    prompt_width: int
    char_widths: tuple[int, ...]
    suffix_width: int = 0
    buffer_advance: int = 0

    @property
    def width(self) -> int:
        return self.prompt_width + sum(self.char_widths) + self.suffix_width

    @property
    def screeninfo(self) -> tuple[int, list[int]]:
        return self.prompt_width, list(self.char_widths) + [1] * self.suffix_width

    @property
    def prompt_only(self) -> bool:
        return not self.char_widths and self.buffer_advance == 0


@dataclass(frozen=True, slots=True)
class LayoutMap:
    rows: tuple[LayoutRow, ...]

    @classmethod
    def empty(cls) -> "LayoutMap":
        return cls((LayoutRow(0, ()),))

    @property
    def screeninfo(self) -> list[tuple[int, list[int]]]:
        return [row.screeninfo for row in self.rows]

    def max_row(self) -> int:
        return len(self.rows) - 1

    def max_column(self, y: int) -> int:
        return self.rows[y].width

    def pos_to_xy(self, pos: int) -> tuple[int, int]:
        rows = self.rows
        if not rows:
            return 0, 0
        last = len(rows) - 1
        remaining = pos
        for y, row in enumerate(rows):
            if row.prompt_only and y < last:
                continue
            n = len(row.char_widths)
            if remaining <= n:
                return row.prompt_width + sum(row.char_widths[:remaining]), y
            if y == last:
                break
            remaining -= row.buffer_advance
            if remaining < 0:
                remaining = 0
        row = rows[last]
        return row.prompt_width + sum(row.char_widths), last

    def xy_to_pos(self, x: int, y: int) -> int:
        rows = self.rows
        if not rows:
            return 0
        y = max(0, min(y, len(rows) - 1))
        pos = sum(row.buffer_advance for row in rows[:y])
        row = rows[y]
        widths = row.char_widths
        cur = row.prompt_width
        i = 0
        while i < len(widths) and cur < x:
            cur += widths[i]
            i += 1
        while i < len(widths) and widths[i] == 0:
            i += 1
        return pos + i

    def row_bounds(self, y: int) -> tuple[int, int]:
        """Buffer positions at the start and end of row *y*."""
        start = sum(row.buffer_advance for row in self.rows[:y])
        return start, start + len(self.rows[y].char_widths)


@dataclass(frozen=True, slots=True)
class WrappedRow:
    prompt_text: str
    prompt_width: int
    fragments: tuple[ContentFragment, ...]
    suffix: str = ""
    suffix_width: int = 0
    buffer_advance: int = 0
    line_index: int = 0


@dataclass(frozen=True, slots=True)
class LayoutResult:
    wrapped_rows: tuple[WrappedRow, ...]
    layout_map: LayoutMap
    line_end_offsets: tuple[int, ...]


def _wrap_body(body: Sequence[ContentFragment], first_width: int, width: int):
    """Split *body* into runs that fit; yields (start, end) indexes.

    A row holds characters while they leave room for the wrap marker;
    zero-width characters always stay with their base, and every row takes
    at least one character so wrapping always progresses."""
    i = 0
    n = len(body)
    x = first_width
    while True:
        start = i
        while i < n:
            w = body[i].width
            if w == 0 or x + w < width or i == start:
                x += w
                i += 1
            else:
                break
        yield start, i
        if i >= n:
            return
        x = 0


def layout_content_lines(lines: Sequence[ContentLine], width: int,
                         start_offset: int) -> LayoutResult:
    if width <= 0:
        return LayoutResult((), LayoutMap(()), ())
    wrapped: list[WrappedRow] = []
    rows: list[LayoutRow] = []
    ends: list[int] = []
    offset = start_offset
    for index, line in enumerate(lines):
        for frag in line.prompt.leading_lines:
            wrapped.append(WrappedRow("", 0, (frag,), line_index=index))
            rows.append(LayoutRow(0, (), 0, 0))
        body = line.body
        prompt = line.prompt
        first = True
        for start, end in _wrap_body(body, prompt.width, width):
            frags = tuple(body[start:end])
            widths = tuple(f.width for f in frags)
            pw = prompt.width if first else 0
            ptext = prompt.text if first else ""
            if end >= len(body):
                advance = (end - start) + (1 if line.source.has_newline else 0)
                wrapped.append(WrappedRow(ptext, pw, frags, "", 0, advance, index))
                rows.append(LayoutRow(pw, widths, 0, advance))
            else:
                wrapped.append(WrappedRow(ptext, pw, frags, WRAP_MARKER, 1, end - start, index))
                rows.append(LayoutRow(pw, widths, 1, end - start))
            first = False
        offset += len(line.source.text) + (1 if line.source.has_newline else 0)
        ends.append(offset)
    return LayoutResult(tuple(wrapped), LayoutMap(tuple(rows)), tuple(ends))
