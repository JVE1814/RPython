#!/usr/bin/env bash
# Writes the multibyte (CJK) codec wrapper modules of Lib/encodings from
# cjk_template.py.  Each entry is <module>:<_codecs_xx suffix>.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/../../Lib/encodings"
for entry in big5:tw big5hkscs:hk cp932:jp cp949:kr cp950:tw \
             euc_jis_2004:jp euc_jisx0213:jp euc_jp:jp euc_kr:kr \
             gb18030:cn gb2312:cn gbk:cn hz:cn \
             iso2022_jp:iso2022 iso2022_jp_1:iso2022 iso2022_jp_2:iso2022 \
             iso2022_jp_2004:iso2022 iso2022_jp_3:iso2022 iso2022_jp_ext:iso2022 \
             iso2022_kr:iso2022 johab:kr shift_jis:jp shift_jis_2004:jp \
             shift_jisx0213:jp; do
    mod="${entry%%:*}"
    suffix="${entry##*:}"
    sed -e "s/@NAME@/$mod/g" -e "s/@MOD@/$suffix/g" "$here/cjk_template.py" > "$out/$mod.py"
done
