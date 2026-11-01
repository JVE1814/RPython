#!/bin/bash
# Generates Lib/html/entities.py from the WHATWG entities.json and the W3C HTML 4.01
# entity sets (HTMLlat1.ent, HTMLsymbol.ent, HTMLspecial.ent).
#   curl -O https://html.spec.whatwg.org/entities.json
#   curl -O https://www.w3.org/TR/html4/HTMLlat1.ent  (and HTMLsymbol.ent, HTMLspecial.ent)
# usage: gen_html_entities.sh entities.json DIR_WITH_ENT_FILES > Lib/html/entities.py
set -e
json=$1; entdir=$2
esc() { # space separated code points -> python string literal body
  awk '{ s=""; for (i=1;i<=NF;i++) { c=$i+0;
      if (c==39) s=s "\\x27"; else if (c==92) s=s "\\\\";
      else if (c>=32 && c<127) s=s sprintf("%c", c);
      else if (c<65536) s=s sprintf("\\u%04x", c); else s=s sprintf("\\U%08x", c) }
    print s }'
}
h4=$(cat "$entdir"/HTMLlat1.ent "$entdir"/HTMLsymbol.ent "$entdir"/HTMLspecial.ent |
     grep -o '<!ENTITY [A-Za-z0-9]* *CDATA "&#[0-9]*;"' |
     sed -E 's/<!ENTITY ([A-Za-z0-9]+) +CDATA "&#([0-9]+);"/\1 \2/')
cat <<'HDR'
"""Tables of HTML character entity references.

Generated from the HTML 4.01 entity sets (W3C) and the WHATWG HTML named
character reference table; see tools/markup/gen_html_entities.sh.
"""

__all__ = ['html5', 'name2codepoint', 'codepoint2name', 'entitydefs']


# HTML 4.01 entity name -> Unicode code point
name2codepoint = {
HDR
echo "$h4" | awk '{ printf "    \x27%s\x27: 0x%04x,\n", $1, $2 }'
cat <<'HDR'
}

# WHATWG named character references (names include the trailing ';' where the
# reference requires it) -> replacement text
html5 = {
HDR
jq -r 'to_entries[] | "\(.key[1:])\t\(.value.codepoints | map(tostring) | join(" "))"' "$json" |
while IFS=$'\t' read -r name cps; do
  printf "    '%s': '%s',\n" "$name" "$(echo "$cps" | esc)"
done
cat <<'HDR'
}

# Unicode code point -> HTML 4.01 entity name
codepoint2name = {cp: name for name, cp in name2codepoint.items()}

# HTML 4.01 entity name -> replacement character
entitydefs = {name: chr(cp) for name, cp in name2codepoint.items()}
HDR
