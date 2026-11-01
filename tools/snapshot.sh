#!/bin/bash
# Make an installed-layout snapshot of a build for suite/benchmark runs:
#   <dest>/bin/python3, <dest>/lib/python3.15t/ (stdlib + C test helper extensions), <dest>/REV
# usage: tools/snapshot.sh <python3-binary> <worktree-with-Lib> <dest>
set -e
BIN="$1"; WT="$2"; DEST="$3"
rm -rf "$DEST.tmp"; mkdir -p "$DEST.tmp/bin" "$DEST.tmp/lib"
cp "$BIN" "$DEST.tmp/bin/python3"
cp -r "$WT/Lib" "$DEST.tmp/lib/python3.15t"
# the spec's C test helpers (_testcapi, _testlimitedcapi, _testbuffer, ...) are installed next
# to the stdlib, as in a CPython installation
bash "$WT/tools/build_test_exts.sh" "$DEST.tmp/lib/python3.15t" > "$DEST.tmp/build_test_exts.log" 2>&1 || echo "warning: test extensions failed to build (see log)"
rm -rf "$DEST.tmp/lib/python3.15t/obj"
(cd "$WT" && git log -1 --format=%h) > "$DEST.tmp/REV"
rm -rf "$DEST"; mv "$DEST.tmp" "$DEST"
echo "snapshot $(cat "$DEST/REV") -> $DEST"
