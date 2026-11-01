#!/bin/bash
# Rebuild crates/runtime/src/modules/mod.rs as the union of the given branches' versions:
# all `pub mod` lines, then `register_all` with the union of all bodies.
# Module lines (and register calls naming them) whose source file is absent from the
# working tree are dropped, so a module deleted on one side is not resurrected.
F=crates/runtime/src/modules/mod.rs
D=crates/runtime/src/modules
exists() { [ -f "$D/$1.rs" ] || [ -d "$D/$1" ]; }
{
  for b in "$@"; do git show "$b:$F" 2>/dev/null | grep -E "^(pub )?mod [A-Za-z_0-9]+;"; done | awk '!seen[$0]++' |
    while read -r line; do m=$(echo "$line" | sed -E 's/^(pub )?mod ([A-Za-z_0-9]+);/\2/'); exists "$m" && echo "$line"; done
  echo
  echo "/// Register all builtin (Rust-implemented) modules with the import system."
  echo "pub fn register_all() {"
  for b in "$@"; do git show "$b:$F" 2>/dev/null | sed -n '/^pub fn register_all/,/^}/p' | grep -v "^pub fn\|^}"; done | awk 'NF && !seen[$0]++' |
    while IFS= read -r line; do
      m=$(echo "$line" | sed -nE 's/.*[ (]([A-Za-z_0-9]+)::[A-Za-z_0-9]+[(,)].*/\1/p')
      if [ -n "$m" ] && [ "$m" != crate ] && ! exists "$m"; then continue; fi
      echo "$line"
    done
  echo "}"
} > $F.new
mv $F.new $F
