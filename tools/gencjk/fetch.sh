#!/usr/bin/env bash
# Downloads the public CJK mapping tables used by tools/gencjk into a cache
# directory (default /tmp/gencjk-cache).  These are the reference tables the
# test suite's test_codecmaps_* compare against (pythontest.net mirrors of the
# unicode.org / Microsoft / x0213.org / HKSCS tables).  The JIS X 0213:2004
# additions come from glibc's EUC-JISX0213 charmap (/usr/share/i18n/charmaps).
#
# Then: cargo run --release --manifest-path tools/gencjk/Cargo.toml -- <cache> <repo-root>
set -euo pipefail
cache="${GENCJK_CACHE:-/tmp/gencjk-cache}"
mkdir -p "$cache"
for f in EUC-CN.TXT CP936.TXT gb-18030-2000.xml BIG5HKSCS-2004.TXT CP932.TXT \
         EUC-JP.TXT SHIFTJIS.TXT EUC-JISX0213.TXT SHIFT_JISX0213.TXT CP949.TXT \
         EUC-KR.TXT JOHAB.TXT BIG5.TXT CP950.TXT; do
    [ -s "$cache/$f" ] || curl -sS -o "$cache/$f" "http://www.pythontest.net/unicode/$f"
done
charmaps="${GLIBC_CHARMAPS:-/usr/share/i18n/charmaps}"
zcat "$charmaps/EUC-JISX0213.gz" > "$cache/glibc-EUC-JISX0213"
echo "cached in $cache"
