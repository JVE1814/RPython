# Two root causes found with tornado's property tests:
# 1. hypothesis' from_regex reads re._constants' node codes (ATOMIC_GROUP, ...) and
#    re._parser.parse(); this implementation's `re` had neither (its parser lives in
#    _sre). re._parser.parse() now builds the parse tree third-party code walks
#    (re/_parse_tree.py).
# 2. zlib: decompress(data, max_length) stopped decoding as soon as the output limit was
#    reached, leaving a trailing end-of-block code and an empty stored block (a websocket
#    message's sync-flush marker) as unconsumed_tail; zlib consumes input that needs no
#    output room.
import re
import re._constants as sre
import re._parser as sre_parse
import zlib

tree = sre_parse.parse(r"(?P<a>x+?)[^b-d\d](?:y|z){2,3}\1(?>q)$")
codes = [code for code, _ in tree]
assert codes == [sre.SUBPATTERN, sre.IN, sre.MAX_REPEAT, sre.GROUPREF, sre.ATOMIC_GROUP, sre.AT], codes
grp = tree[0][1]
assert grp[0] == 1 and grp[3][0][0] == sre.MIN_REPEAT and tree.state.groupdict == {"a": 1}
assert tree[1][1][0] == (sre.NEGATE, None) and (sre.RANGE, (98, 100)) in tree[1][1]
rep = tree[2]
assert rep[1][:2] == (2, 3) and rep[1][2][0][0] == sre.SUBPATTERN and rep[1][2][0][1][3][0][0] == sre.BRANCH
assert tree.getwidth()[0] == 1 + 1 + 2 + 0 + 1

for message in (b"\x00", b"hello", bytes(range(256)) * 3):
    c = zlib.compressobj(wbits=-15)
    data = c.compress(message) + c.flush(zlib.Z_SYNC_FLUSH)
    d = zlib.decompressobj(wbits=-15)
    out = d.decompress(data, len(message))
    assert out == message and d.unconsumed_tail == b"", (message[:8], d.unconsumed_tail)
print("OK")
