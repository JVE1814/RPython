#!/bin/bash
# Rebuild crates/runtime/src/modules/mod.rs as the union of the given branches' versions:
# all `pub mod` lines, then `register_all` with the union of all bodies.
F=crates/runtime/src/modules/mod.rs
{
  for b in "$@"; do git show "$b:$F" 2>/dev/null | grep -E "^(pub )?mod [A-Za-z_0-9]+;"; done | awk '!seen[$0]++'
  echo
  echo "/// Register all builtin (Rust-implemented) modules with the import system."
  echo "pub fn register_all() {"
  for b in "$@"; do git show "$b:$F" 2>/dev/null | sed -n '/^pub fn register_all/,/^}/p' | grep -v "^pub fn\|^}"; done | awk 'NF && !seen[$0]++'
  echo "}"
} > $F.new
# keep any other items (helper fns) from the current file that are outside the register fn
mv $F.new $F
