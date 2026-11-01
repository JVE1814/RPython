#!/bin/bash
# Build a stdlib overlay in /tmp/mklib for local testing: the coordinator's
# integration checkout (/home/user/core/Lib) plus the modules that only exist
# in this worktree's Lib/.  Nothing here is committed anywhere.
W=$(cd "$(dirname "$0")/../.." && pwd)
CORE=${CORE:-/home/user/core/Lib}
OUT=/tmp/mklib
rm -rf "$OUT"; mkdir -p "$OUT"
for e in "$CORE"/*; do ln -s "$e" "$OUT/$(basename "$e")"; done
for e in "$W"/Lib/*; do
  b=$(basename "$e")
  [ -e "$OUT/$b" ] || ln -s "$e" "$OUT/$b"
done
# packages owned by this workstream always come from the worktree
for b in html xml email tomllib xmlrpc _markupbase.py quopri.py mimetypes.py mailbox.py \
         smtplib.py imaplib.py poplib.py ftplib.py optparse.py webbrowser.py; do
  if [ -e "$W/Lib/$b" ]; then rm -f "$OUT/$b"; ln -s "$W/Lib/$b" "$OUT/$b"; fi
done
# optional extra Lib dirs (other workstreams' worktrees) for missing modules
for x in $EXTRA; do
  for e in "$x"/*; do
    b=$(basename "$e")
    [ -e "$OUT/$b" ] || ln -s "$e" "$OUT/$b"
  done
done
echo "overlay ready: $OUT"
