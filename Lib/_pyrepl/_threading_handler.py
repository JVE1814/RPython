"""Showing exceptions from other threads without garbling the input line.

While the reader waits for input, uncaught exceptions in threads are not
printed directly; they are queued and printed above the prompt the next
time the reader polls its hooks.
"""

from __future__ import annotations

import threading
import traceback
from collections import deque


class ExceptHookHandler:
    def __init__(self, reader) -> None:
        self.reader = reader
        self.messages: deque[str] = deque()
        self.lock = threading.Lock()

    def exception(self, args) -> None:
        lines = traceback.format_exception(
            args.exc_type, args.exc_value, args.exc_traceback, colorize=self.reader.can_colorize)
        name = args.thread.name if args.thread is not None else "<unknown>"
        message = f"Exception in thread {name}:\n" + "".join(lines)
        with self.lock:
            self.messages.append(message)

    def __call__(self) -> None:
        if not self.messages:
            return
        with self.lock:
            pending = list(self.messages)
            self.messages.clear()
        reader = self.reader
        console = reader.console
        try:
            console.restore()
            import sys
            for message in pending:
                sys.stderr.write("\n" + message)
            sys.stderr.flush()
        finally:
            console.prepare()
            reader.invalidate_full()
            reader.refresh()


def install_threading_hook(reader) -> None:
    handler = ExceptHookHandler(reader)
    reader.threading_hook = handler
    previous = threading.excepthook

    def hook(args):
        if args.exc_type is SystemExit:
            return
        try:
            handler.exception(args)
        except Exception:
            previous(args)

    if threading.excepthook is threading.__excepthook__:
        threading.excepthook = hook
