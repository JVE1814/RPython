# Root cause (pandas io/test_compression::test_xz_compression_level_read): our LZMA
# encoder's "normal" mode (presets 4..9) was a greedy/lazy parser that compressed no
# better than the fast presets (pickled frames: identical sizes for presets 0..9).
# Normal mode now chooses symbols by price over 4096-byte windows; the windows never
# depend on how the input is fed (streaming == one shot, test_lzma test_write_10).
import io
import lzma

text = ("".join(f"{i:5d}, {i * 0.123456:.6f}, name_{i % 7}\n" for i in range(400))).encode()
blob = bytes(range(256)) * 4 + b"pandas.core.frame DataFrame numpy ndarray " * 20
for data in (text, text + blob):
    fast = len(lzma.compress(data, preset=1))
    normal = len(lzma.compress(data))
    assert normal < fast, (normal, fast)
    out = io.BytesIO()
    with lzma.LZMAFile(out, "w") as f:
        for k in range(0, len(data), 7):
            f.write(data[k:k + 7])
    assert out.getvalue() == lzma.compress(data)
    assert lzma.decompress(out.getvalue()) == data
print("OK")
