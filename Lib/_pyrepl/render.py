"""Cell-level model of what is on the screen.

A ``RenderLine`` is a row of ``RenderCell``s: one cell per displayed
character (with its width and SGR style) plus zero-width "control" cells
for non-SGR escape sequences.  Consoles diff old and new lines cell by
cell so that only the changed part of a row is rewritten.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Iterable, Sequence

from .utils import str_width

RESET = "\x1b[0m"
_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_SGR_RESETS = ("\x1b[0m", "\x1b[m")


def _is_sgr(seq: str) -> bool:
    return seq.startswith("\x1b[") and seq.endswith("m")


@dataclass(frozen=True, slots=True)
class StyleRef:
    """A display style: the SGR sequence that selects it (and the theme
    tag it came from, if any)."""
    sgr: str = ""
    tag: str | None = None

    @classmethod
    def from_sgr(cls, sgr: str | None) -> "StyleRef":
        if not sgr or sgr in _SGR_RESETS:
            return _NO_STYLE
        return cls(sgr)

    @classmethod
    def from_tag(cls, tag: str | None, sgr: str | None = None) -> "StyleRef":
        if tag is None:
            return _NO_STYLE
        if sgr is None:
            from .utils import THEME
            try:
                sgr = THEME(force_color=True)[tag]
            except (KeyError, AttributeError):
                sgr = ""
        if sgr in _SGR_RESETS:
            sgr = ""
        return cls(sgr, tag)

    def __bool__(self) -> bool:
        return bool(self.sgr)


_NO_STYLE = StyleRef()


def _style(value) -> StyleRef:
    if value is None:
        return _NO_STYLE
    if isinstance(value, StyleRef):
        return value
    if isinstance(value, str):
        if value.startswith("\x1b") or not value:
            return StyleRef.from_sgr(value)
        return StyleRef.from_tag(value)
    raise TypeError(f"invalid style: {value!r}")


@dataclass(frozen=True, slots=True)
class RenderCell:
    text: str
    width: int
    style: StyleRef = _NO_STYLE
    controls: tuple[str, ...] = ()

    @property
    def terminal_text(self) -> str:
        return render_cells((self,))

    @property
    def is_control(self) -> bool:
        return bool(self.controls)


def render_cells(cells: Iterable[RenderCell], visual_style: str | None = None) -> str:
    """The terminal text that draws *cells* (styles are reset at the end)."""
    out: list[str] = []
    active = ""
    extra = visual_style or ""
    for cell in cells:
        if cell.controls:
            out.extend(cell.controls)
        if not cell.text:
            continue
        want = extra + cell.style.sgr if (extra or cell.style.sgr) else ""
        if want != active:
            if active:
                out.append(RESET)
            if want:
                out.append(want)
            active = want
        out.append(cell.text)
    if active:
        out.append(RESET)
    return "".join(out)


def requires_cursor_resync(cells: Iterable[RenderCell]) -> bool:
    """Whether drawing *cells* may move the cursor unpredictably (they
    contain escape sequences other than SGR)."""
    return any(not _is_sgr(c) for cell in cells for c in cell.controls)


@dataclass(frozen=True, slots=True)
class RenderLine:
    cells: tuple[RenderCell, ...]
    text: str
    width: int

    @classmethod
    def from_cells(cls, cells: Iterable[RenderCell]) -> "RenderLine":
        cells = tuple(cells)
        return cls(cells, render_cells(cells), sum(c.width for c in cells))

    @classmethod
    def from_parts(cls, texts: Sequence[str], widths: Sequence[int],
                   styles: Sequence | None = None) -> "RenderLine":
        if styles is None:
            styles = [None] * len(texts)
        cells = [RenderCell(t, w, _style(s)) for t, w, s in zip(texts, widths, styles)]
        return cls.from_cells(cells)

    @classmethod
    def from_rendered_text(cls, text: str) -> "RenderLine":
        cells: list[RenderCell] = []
        pending: list[str] = []
        sgr = ""
        i = 0
        n = len(text)
        while i < n:
            c = text[i]
            if c == "\x1b":
                m = _CSI.match(text, i)
                if m is not None:
                    seq = m.group()
                    i = m.end()
                else:
                    seq = text[i:i + 2]
                    i += len(seq)
                if _is_sgr(seq):
                    sgr = "" if seq in _SGR_RESETS else sgr + seq
                else:
                    pending.append(seq)
                continue
            if pending:
                cells.append(RenderCell("", 0, controls=tuple(pending)))
                pending = []
            cells.append(RenderCell(c, str_width(c), StyleRef.from_sgr(sgr)))
            i += 1
        if pending:
            cells.append(RenderCell("", 0, controls=tuple(pending)))
        return cls.from_cells(cells)

    @classmethod
    def empty(cls) -> "RenderLine":
        return _EMPTY_LINE


_EMPTY_LINE = RenderLine((), "", 0)


@dataclass(frozen=True, slots=True)
class LineDiff:
    start_cell: int
    start_x: int
    old_cells: tuple[RenderCell, ...]
    new_cells: tuple[RenderCell, ...]
    old_text: str
    new_text: str
    old_changed_width: int
    new_changed_width: int


def _plain_zero_width(cell: RenderCell) -> bool:
    return cell.width == 0 and not cell.controls and bool(cell.text)


def diff_render_lines(old: RenderLine, new: RenderLine) -> LineDiff | None:
    """The smallest run of cells that changed between *old* and *new*,
    or None if they are identical."""
    a, b = old.cells, new.cells
    if a == b:
        return None
    limit = min(len(a), len(b))
    p = 0
    while p < limit and a[p] == b[p] and not a[p].controls:
        p += 1
    # never separate a base character from the combining marks after it
    while p > 0 and ((p < len(a) and _plain_zero_width(a[p]))
                     or (p < len(b) and _plain_zero_width(b[p]))):
        p -= 1
    s = 0
    while (s < len(a) - p and s < len(b) - p
           and a[len(a) - 1 - s] == b[len(b) - 1 - s]
           and not a[len(a) - 1 - s].controls):
        s += 1
    while s > 0 and (_plain_zero_width(a[len(a) - s]) or _plain_zero_width(b[len(b) - s])):
        s -= 1
    old_cells = a[p:len(a) - s]
    new_cells = b[p:len(b) - s]
    return LineDiff(
        start_cell=p,
        start_x=sum(c.width for c in a[:p]),
        old_cells=old_cells,
        new_cells=new_cells,
        old_text=render_cells(old_cells),
        new_text=render_cells(new_cells),
        old_changed_width=sum(c.width for c in old_cells),
        new_changed_width=sum(c.width for c in new_cells),
    )


@dataclass(frozen=True, slots=True)
class LineUpdate:
    """One drawing operation a console performs on row *y*."""
    kind: str
    y: int
    start_cell: int
    start_x: int
    cells: tuple[RenderCell, ...]
    char_width: int = 0
    clear_eol: bool = False
    reset_to_margin: bool = False
    text: str = field(init=False, default="")

    def __post_init__(self) -> None:
        object.__setattr__(self, "cells", tuple(self.cells))
        object.__setattr__(self, "text", render_cells(self.cells))


@dataclass(frozen=True, slots=True)
class ScreenOverlay:
    """Lines drawn over (or, with *insert*, between) the base lines."""
    y: int
    lines: tuple[RenderLine, ...]
    insert: bool = False


@dataclass(frozen=True, slots=True)
class RenderedScreen:
    lines: tuple[RenderLine, ...]
    cursor: tuple[int, int]
    overlays: tuple[ScreenOverlay, ...] = ()

    @classmethod
    def empty(cls) -> "RenderedScreen":
        return cls((), (0, 0), ())

    @classmethod
    def from_screen_lines(cls, lines: Iterable[str], cursor: tuple[int, int]) -> "RenderedScreen":
        return cls(tuple(RenderLine.from_rendered_text(l) for l in lines), tuple(cursor))

    @property
    def screen_lines(self) -> tuple[str, ...]:
        return tuple(line.text for line in self.lines)

    def with_overlay(self, y: int, lines: Iterable[RenderLine], insert: bool = False) -> "RenderedScreen":
        overlay = ScreenOverlay(y, tuple(lines), insert)
        return RenderedScreen(self.lines, self.cursor, self.overlays + (overlay,))

    @property
    def composed_lines(self) -> tuple[RenderLine, ...]:
        if not self.overlays:
            return self.lines
        result = list(self.lines)
        for ov in self.overlays:
            if ov.insert:
                result[ov.y:ov.y] = ov.lines
                continue
            while len(result) < ov.y:
                result.append(_EMPTY_LINE)
            for i, line in enumerate(ov.lines):
                if ov.y + i < len(result):
                    result[ov.y + i] = line
                else:
                    result.append(line)
        return tuple(result)
