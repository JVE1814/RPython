"""One-shot stack dumps: the stacks of a sample shown like tracebacks."""

import linecache
import os

import _colorize

from .collector import extract_lineno
from .constants import (
    THREAD_STATUS_GIL_REQUESTED,
    THREAD_STATUS_HAS_EXCEPTION,
    THREAD_STATUS_HAS_GIL,
    THREAD_STATUS_MAIN_THREAD,
    THREAD_STATUS_ON_CPU,
    THREAD_STATUS_UNKNOWN,
)

__all__ = ('format_stack_dump',)

# files of the profiler itself, never shown
_INTERNAL_FILES = ('_sync_coordinator.py',)

_STATUS_LABELS = (
    (THREAD_STATUS_MAIN_THREAD, 'main thread'),
    (THREAD_STATUS_HAS_GIL, 'has GIL'),
    (THREAD_STATUS_ON_CPU, 'on CPU'),
    (THREAD_STATUS_GIL_REQUESTED, 'waiting for GIL'),
    (THREAD_STATUS_HAS_EXCEPTION, 'has exception'),
)
_ACTIVE = THREAD_STATUS_HAS_GIL | THREAD_STATUS_ON_CPU | THREAD_STATUS_GIL_REQUESTED


def _status_text(status):
    if status is None:
        return ''
    labels = [label for flag, label in _STATUS_LABELS if status & flag]
    if not status & THREAD_STATUS_UNKNOWN and not status & _ACTIVE:
        labels.append('idle')
    return ', '.join(labels)


def _opcode_name(number):
    import opcode
    names = opcode.opname
    if isinstance(number, int) and 0 <= number < len(names) and \
            not names[number].startswith('<'):
        return names[number]
    return '<%s>' % (number,)


def _frame_parts(frame):
    """(filename, location, function name, opcode) of a frame record."""
    if hasattr(frame, 'filename'):
        name = getattr(frame, 'qualname', None) or getattr(frame, 'funcname', None)
        return frame.filename, frame.location, name, getattr(frame, 'opcode', None)
    filename, location, funcname, opcode_number = tuple(frame)[:4]
    return filename, location, funcname, opcode_number


def _source_line(filename, location, theme, colorize):
    lineno = extract_lineno(location)
    if not filename or lineno <= 0:
        return None
    line = linecache.getline(filename, lineno)
    if not line:
        return None
    line = line.rstrip('\r\n')
    text = line.lstrip()
    trimmed = len(line) - len(text)
    text = text.rstrip()
    if not text:
        return None
    if colorize and location is not None and not isinstance(location, int):
        start, end = location[2], location[3]
        same_line = len(location) < 2 or location[1] in (None, lineno)
        if start is not None and end is not None and start >= 0 and \
                end > start and same_line:
            start = max(0, start - trimmed)
            end = min(len(text), end - trimmed)
            if end > start:
                text = (text[:start] + theme.source_highlight + text[start:end] +
                        theme.reset + text[end:])
    return '    ' + text


def _format_frames(frames, theme, colorize):
    lines = []
    for frame in reversed(list(frames or ())):
        filename, location, name, opcode_number = _frame_parts(frame)
        if filename and os.path.basename(filename) in _INTERNAL_FILES:
            continue
        lineno = extract_lineno(location)
        entry = '  File %s"%s"%s, line %s%d%s, in %s%s%s' % (
            theme.filename, filename, theme.reset,
            theme.line_no, lineno, theme.reset,
            theme.frame, name, theme.reset)
        if opcode_number is not None:
            entry += ', %sopcode=%s%s' % (theme.opcode, _opcode_name(opcode_number),
                                          theme.reset)
        lines.append(entry)
        source = _source_line(filename, location, theme, colorize)
        if source is not None:
            lines.append(source)
    return lines


def _dump_units(stack_frames):
    """(thread id, extra header parts, status, frames) for each stack."""
    records = list(stack_frames or ())
    interpreters = [r for r in records if hasattr(r, 'threads')]
    show_interpreter = len(interpreters) > 1
    for record in records:
        if hasattr(record, 'threads'):
            for thread in record.threads or ():
                extra = []
                if show_interpreter:
                    extra.append('interpreter %s' % record.interpreter_id)
                yield (thread.thread_id, extra, getattr(thread, 'status', None),
                       thread.frame_info)
        elif hasattr(record, 'awaited_by'):
            for task in record.awaited_by or ():
                frames = []
                for coro in getattr(task, 'coroutine_stack', ()) or ():
                    frames.extend(coro.call_stack or ())
                yield (record.thread_id, ['task %s' % task.task_id], None, frames)


def format_stack_dump(stack_frames, pid, colorize=False):
    """The text of a stack dump for the sampled process `pid`."""
    theme = _colorize.get_theme(force_color=colorize,
                                force_no_color=not colorize).profiler_dump
    blocks = []
    for thread_id, extra, status, frames in _dump_units(stack_frames):
        parts = list(extra)
        status_text = _status_text(status)
        if status_text:
            parts.append(status_text)
        parts.append('most recent call last')
        if not blocks:
            prefix = 'Stack dump for PID %s, thread %s' % (pid, thread_id)
        else:
            prefix = 'Stack dump, thread %s' % (thread_id,)
        header = '%s%s (%s):%s' % (theme.header, prefix, '; '.join(parts), theme.reset)
        blocks.append('\n'.join([header] + _format_frames(frames, theme, colorize)))
    if not blocks:
        return 'No Python stacks found for PID %s\n' % (pid,)
    return '\n\n'.join(blocks) + '\n'
