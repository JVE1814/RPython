# (perf6) base64_small's building blocks: per call costs of the base64 module on tiny data.
# usage: b64.py N KIND   KIND: enc | dec | enc_raw | dec_raw
import base64
import binascii
import sys

DATA = bytes(range(20))
B64 = base64.b64encode(DATA)


def run(n, kind):
    if kind == 'enc':
        for _ in range(n):
            base64.b64encode(DATA)
    elif kind == 'dec':
        for _ in range(n):
            base64.b64decode(B64)
    elif kind == 'enc_raw':
        for _ in range(n):
            binascii.b2a_base64(DATA, newline=False)
    elif kind == 'dec_raw':
        for _ in range(n):
            binascii.a2b_base64(B64, strict_mode=False)
    else:
        raise SystemExit('unknown kind ' + kind)


run(int(sys.argv[1]), sys.argv[2])
