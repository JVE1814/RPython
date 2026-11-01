#!/usr/bin/env bash
# Builds tools/gencodecs/data/<module>.map (one "HH UUUU" line per defined byte)
# for every single-byte charmap codec in Lib/encodings, from public mapping
# tables:
#   * https://www.unicode.org/Public/MAPPINGS/ (ISO 8859, Microsoft, Apple, misc.)
#   * glibc's locale charmaps (/usr/share/i18n/charmaps) for the IBM/HP/KOI8-T/
#     PT154/CP1125/RK1048 code pages that unicode.org does not publish.
# The .map files are committed; run tools/gencodecs (cargo run) afterwards to
# regenerate the Python modules.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cache="${GENCODECS_CACHE:-/tmp/gencodecs-cache}"
out="$here/data"
charmaps="${GLIBC_CHARMAPS:-/usr/share/i18n/charmaps}"
mkdir -p "$cache" "$out"
U=https://www.unicode.org/Public/MAPPINGS

fetch() { # url file
    [ -s "$cache/$2" ] || curl -sS -o "$cache/$2" "$1"
}

# unicode.org format: "0xHH<tab>[<LR>+]0xUUUU<tab># name"; byte lines without a
# code point are undefined.
from_unicode() { # file module
    awk '
        /^0x[0-9A-Fa-f][0-9A-Fa-f][ \t]/ {
            b = tolower(substr($1, 3, 2));
            u = $2; gsub(/<[A-Z]+>\+/, "", u);
            if (u !~ /^0x[0-9A-Fa-f]+/) next;
            split(u, parts, "+"); u = parts[1];
            u = toupper(substr(u, 3));
            if (!(b in seen)) { seen[b] = 1; printf "%s %s\n", toupper(b), u }
        }' "$cache/$1" | sort > "$out/$2.map"
}

# glibc charmap format: "<UXXXX> /xHH NAME"
from_glibc() { # charmap module
    zcat "$charmaps/$1.gz" | awk '
        /^<U[0-9A-Fa-f]+>[ \t]+\/x[0-9a-fA-F][0-9a-fA-F][ \t]/ {
            u = substr($1, 3, length($1) - 3);
            b = toupper(substr($2, 3, 2));
            if ($2 ~ /^\/x..$/ && !(b in seen)) { seen[b] = 1; printf "%s %s\n", b, toupper(u) }
        }' | sort > "$out/$2.map"
}

# Add identity mappings for C0 controls and DEL when missing (Apple tables).
add_controls() { # module
    f="$out/$1.map"
    { cat "$f"; for i in $(seq 0 31) 127; do printf '%02X %04X\n' "$i" "$i"; done; } |
        sort -u -k1,1 | sort > "$f.tmp"
    mv "$f.tmp" "$f"
}

# Replace / add / remove single entries.
set_entry() { # module byte code|-
    f="$out/$1.map"
    grep -v "^$2 " "$f" > "$f.tmp" || true
    if [ "$3" != "-" ]; then printf '%s %s\n' "$2" "$3" >> "$f.tmp"; fi
    sort "$f.tmp" > "$f"
    rm -f "$f.tmp"
}

for n in 1 2 3 4 5 6 7 8 9 10 11 13 14 15 16; do
    fetch "$U/ISO8859/8859-$n.TXT" "8859-$n.TXT"
    from_unicode "8859-$n.TXT" "iso8859_$n"
done
for n in 1250 1251 1252 1253 1254 1255 1256 1257 1258 874; do
    fetch "$U/VENDORS/MICSFT/WINDOWS/CP$n.TXT" "CP$n.TXT"
    from_unicode "CP$n.TXT" "cp$n"
done
for n in 437 737 775 850 852 855 857 860 861 862 863 864 865 866 869; do
    fetch "$U/VENDORS/MICSFT/PC/CP$n.TXT" "CP$n.TXT"
    from_unicode "CP$n.TXT" "cp$n"
done
for n in 037 500 875 1026; do
    fetch "$U/VENDORS/MICSFT/EBCDIC/CP$n.TXT" "EBCDIC-CP$n.TXT"
    from_unicode "EBCDIC-CP$n.TXT" "cp$n"
done
for n in CP424:cp424 CP856:cp856 CP1006:cp1006 KOI8-R:koi8_r KOI8-U:koi8_u KZ1048:kz1048; do
    fetch "$U/VENDORS/MISC/${n%%:*}.TXT" "${n%%:*}.TXT"
    from_unicode "${n%%:*}.TXT" "${n##*:}"
done
for n in ROMAN:mac_roman CYRILLIC:mac_cyrillic GREEK:mac_greek ICELAND:mac_iceland \
         TURKISH:mac_turkish CROATIAN:mac_croatian ROMANIAN:mac_romanian \
         ARABIC:mac_arabic FARSI:mac_farsi; do
    fetch "$U/VENDORS/APPLE/${n%%:*}.TXT" "APPLE-${n%%:*}.TXT"
    from_unicode "APPLE-${n%%:*}.TXT" "${n##*:}"
    add_controls "${n##*:}"
done
fetch "$U/VENDORS/MICSFT/MAC/LATIN2.TXT" "MSMAC-LATIN2.TXT"
from_unicode "MSMAC-LATIN2.TXT" mac_latin2

from_glibc IBM273 cp273
from_glibc CP1125 cp1125
from_glibc HP-ROMAN8 hp_roman8
from_glibc KOI8-T koi8_t
from_glibc PT154 ptcp154
from_glibc IBM858 cp858
from_glibc TIS-620 tis_620

# IBM-1140 is IBM-037 with the euro sign replacing the international currency sign.
cp "$out/cp037.map" "$out/cp1140.map"
set_entry cp1140 9F 20AC

# Palm OS: Windows-1252 with the card suits in 0x8D-0x90.
cp "$out/cp1252.map" "$out/palmos.map"
set_entry palmos 81 0081
set_entry palmos 8D 2666
set_entry palmos 8E 2663
set_entry palmos 8F 2665
set_entry palmos 90 2660
set_entry palmos 9D 009D
set_entry palmos 9E 009E

# CP720 (MS-DOS Arabic) is not published in machine readable form; its table
# upper half is kept in cp720.src (written from the code page chart).
{ for i in $(seq 0 127); do printf '%02X %04X\n' "$i" "$i"; done; cat "$here/cp720.src"; } > "$out/cp720.map"

echo "wrote $(ls "$out" | wc -l) maps to $out"
