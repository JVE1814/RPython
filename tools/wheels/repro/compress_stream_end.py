# Root causes (test_free_threading.test_bz2/test_lzma):
#  * BZ2Compressor.compress() returned the "BZh9" stream header on its first call;
#    libbz2 keeps everything buffered until a block is complete (b"" for small input).
#  * With max_length, the call that returned the last output bytes did not go on
#    to the stream trailer (bz2) / end marker, check, index and footer (xz), so eof
#    stayed False and unused_data empty until one more call.
import bz2
import lzma

c = bz2.BZ2Compressor()
assert c.compress(b"x" * 1000) == b""
assert bz2.decompress(c.flush()) == b"x" * 1000

for mod, D in ((bz2, bz2.BZ2Decompressor), (lzma, lzma.LZMADecompressor)):
    data = b"".join(bytes([97 + i]) * 128 for i in range(4))
    comp = mod.compress(data)
    d = D()
    out = d.decompress(comp + b"tail", 128)
    while not d.eof:
        out += d.decompress(b"", 128)
        assert len(out) <= len(data)
    assert out == data, mod.__name__
    # the end of the stream is seen in the call that returned the last 128 bytes
    d = D()
    parts = [d.decompress(comp + b"tail", 128)] + [d.decompress(b"", 128) for _ in range(3)]
    assert b"".join(parts) == data and d.eof and d.unused_data == b"tail", mod.__name__
print("OK")
