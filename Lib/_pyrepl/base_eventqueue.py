"""Turning input bytes into key events.

Bytes are matched against a tree of known key sequences (see
keymap.compile_keymap); a complete sequence produces one event named after
the key, anything else is decoded and produces one event per character.
"""

from __future__ import annotations

from collections import deque

from . import keymap
from .console import Event
from .trace import trace


class BaseEventQueue:
    def __init__(self, encoding: str, keymap_dict: dict[bytes, str]) -> None:
        self.compiled_keymap = keymap.compile_keymap(keymap_dict)
        self.keymap = self.compiled_keymap
        trace("keymap {k!r}", k=self.keymap)
        self.encoding = encoding
        self.events: deque[Event] = deque()
        self.buf = bytearray()

    def get(self) -> Event | None:
        if self.events:
            return self.events.popleft()
        return None

    def empty(self) -> bool:
        return not self.events

    def flush_buf(self) -> bytearray:
        old = self.buf
        self.buf = bytearray()
        return old

    def insert(self, event: Event) -> None:
        trace("added event {event}", event=event)
        self.events.append(event)

    def push(self, char: int | bytes) -> None:
        assert isinstance(char, (int, bytes)), f"expected int or bytes, got {char!r}"
        byte = bytes([char]) if isinstance(char, int) else bytes(char)
        node = self.keymap.get(byte) if isinstance(self.keymap, dict) else None
        if node is not None:
            self.buf.extend(byte)
            if isinstance(node, dict):
                self.keymap = node
                self._partial_key = True
            else:
                self.insert(Event("key", node, bytes(self.flush_buf())))
                self.keymap = self.compiled_keymap
                self._partial_key = False
            return
        partial = getattr(self, "_partial_key", False)
        self._partial_key = False
        if self.keymap is not self.compiled_keymap and not partial:
            self.keymap = self.compiled_keymap
        if partial:
            # an unfinished key sequence: what was buffered is plain text;
            # the new byte starts over from the top of the keymap
            self.keymap = self.compiled_keymap
            self._emit_text()
            self.push(byte)
            return
        self.buf.extend(byte)
        self._emit_text()

    def _emit_text(self) -> None:
        if not self.buf:
            return
        try:
            text = self.buf.decode(self.encoding)
        except UnicodeDecodeError as exc:
            if exc.end == len(self.buf) and len(self.buf) < 4 and "end of data" in str(exc.reason):
                return  # wait for the rest of a multi-byte character
            text = self.buf.decode(self.encoding, "replace")
        self.flush_buf()
        for ch in text:
            self.insert(Event("key", ch, ch.encode(self.encoding, "replace")))
