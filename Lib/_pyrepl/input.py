"""Translate key events into commands with a keymap.

A keymap is a sequence of ``(key specification, command)`` pairs (see
keymap.parse_keys); a command may itself be a dict, meaning "these
further keys are expected next".  ``KeymapTranslator`` consumes key events
one at a time and queues ``(command, [keys])`` results.
"""

from __future__ import annotations

import unicodedata
from collections import deque

from .keymap import parse_keys


def _build_tree(keymap) -> dict:
    tree: dict = {}
    items = keymap.items() if isinstance(keymap, dict) else keymap
    for spec, command in items:
        keys = parse_keys(spec)
        if not keys:
            continue
        node = tree
        for key in keys[:-1]:
            child = node.get(key)
            if not isinstance(child, dict):
                child = node[key] = {}
            node = child
        if isinstance(command, dict):
            sub = _build_tree(command)
            existing = node.get(keys[-1])
            if isinstance(existing, dict):
                existing.update(sub)
            else:
                node[keys[-1]] = sub
        else:
            node[keys[-1]] = command
    return tree


def _is_character(key: str) -> bool:
    return len(key) == 1 and not unicodedata.category(key).startswith("C")


class KeymapTranslator:
    def __init__(self, keymap, verbose: bool = False,
                 invalid_cls=None, character_cls=None) -> None:
        self.verbose = verbose
        self.keymap = keymap
        self.invalid_cls = invalid_cls
        self.character_cls = character_cls
        self.k = self.ck = _build_tree(keymap)
        self.stack: list[str] = []
        self.results: deque = deque()

    def push(self, evt) -> None:
        key = evt.data
        node = self.k.get(key)
        if node is None:
            if not self.stack and _is_character(key):
                self.results.append((self.character_cls, [key]))
            else:
                self.results.append((self.invalid_cls, [*self.stack, key]))
            self._reset()
        elif isinstance(node, dict):
            self.stack.append(key)
            self.k = node
        else:
            self.results.append((node, [*self.stack, key]))
            self._reset()

    def _reset(self) -> None:
        self.stack = []
        self.k = self.ck

    def get(self):
        if self.results:
            return self.results.popleft()
        return None

    def empty(self) -> bool:
        return not self.results
