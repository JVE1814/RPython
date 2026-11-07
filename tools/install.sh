#!/bin/bash
# Install a CPython-like prefix:  tools/install.sh <prefix> [release|debug]
# <prefix>/bin/python3.15t (+python3, python3.15 symlinks), <prefix>/lib/python3.15t (stdlib),
# <prefix>/include/python3.15t (public headers + pyconfig.h).
set -e
PREFIX="${1:?prefix}"; MODE="${2:-release}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
if [ "$MODE" = release ]; then (cd "$ROOT" && cargo build --release -p python); BIN="$TARGET/release/python3";
else (cd "$ROOT" && cargo build -p python); BIN="$TARGET/debug/python3"; fi
mkdir -p "$PREFIX/bin" "$PREFIX/lib/python3.15t/site-packages" "$PREFIX/lib/python3.15t/lib-dynload" "$PREFIX/include/python3.15t"
install -m 755 "$BIN" "$PREFIX/bin/python3.15t"
ln -sf python3.15t "$PREFIX/bin/python3.15"; ln -sf python3.15t "$PREFIX/bin/python3"; ln -sf python3.15t "$PREFIX/bin/python"
rm -rf "$PREFIX/lib/python3.15t.tmp"; cp -r "$ROOT/Lib" "$PREFIX/lib/python3.15t.tmp"
find "$PREFIX/lib/python3.15t.tmp" -name __pycache__ -prune -exec rm -rf {} +
cp -r "$PREFIX/lib/python3.15t.tmp/." "$PREFIX/lib/python3.15t/"; rm -rf "$PREFIX/lib/python3.15t.tmp"
cp -r "$ROOT/spec/Include/." "$PREFIX/include/python3.15t/"
cp "$ROOT/tools/pyconfig.h" "$PREFIX/include/python3.15t/pyconfig.h"
echo "installed to $PREFIX"
