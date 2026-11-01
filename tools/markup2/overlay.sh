#!/bin/bash
# Build a test stdlib overlay in /tmp/m2lib: this worktree's Lib/ plus the
# entries it lacks taken from other worktrees (EXTRA="dir1 dir2 ...", each a
# Lib directory), e.g. http/urllib/pydoc that are not merged yet.
# Use with: RPYTHON_STDLIB=/tmp/m2lib tools/markup2/rt.sh test_xxx
W=$(cd "$(dirname "$0")/../.." && pwd)
OUT=/tmp/m2lib
rm -rf "$OUT"; mkdir -p "$OUT"
for e in "$W"/Lib/*; do ln -s "$e" "$OUT/$(basename "$e")"; done
for x in $EXTRA; do
  for e in "$x"/*; do
    b=$(basename "$e")
    [ -e "$OUT/$b" ] || ln -s "$e" "$OUT/$b"
  done
done
echo "overlay ready: $OUT"
