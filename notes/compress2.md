# compress2 workstream notes

Owner of `crates/compress` (rpy-compress) and the compression/archive modules from now on.

## What was added
- `crates/compress/src/lzma/` — LZMA/LZMA2 codec and containers, from the .xz format
  description and the LZMA algorithm:
  - `model.rs`: probability model, range encoder/decoder.
  - `dec.rs`: LZMA1 decoder (streaming; symbols near the end of the buffered input are
    decoded speculatively with an undo log so arbitrary input splits work), LZMA2 decoder
    (chunks decoded as their bytes arrive; stored and LZMA chunks, all reset modes).
  - `enc.rs`: hash-chain match finder (hash3 + hash4 chains, depth by preset), greedy
    (presets 0-3) / lazy (4-9, EXTREME = deeper search) parser with repeat-distance
    preference and short reps; LZMA1 (end marker) and LZMA2 (2 MiB / 64 KiB chunks, stored
    fallback for incompressible chunks) encoders.
  - `filters.rs`: Delta, BCJ x86/PowerPC/IA-64/ARM/ARM-Thumb/SPARC/ARM64 (encode+decode,
    split-invariant buffering stage).
  - `mod.rs`: `.xz` (CRC32/CRC64/SHA-256/none checks, block headers, index, footer
    validation), `.lzma` (alone; picky header check in auto mode), raw filter chains,
    filter property encode/decode, memory limit, streaming `Encoder`/`Decoder`.
  - Tests (`cargo test -p rpy-compress --release lzma`) cross-check against the system
    `xz` CLI in both directions (all formats/filters).  Ratio at -6 is ~20% worse than xz
    (no optimal parser yet).
- `crates/compress/src/zstd/` — Zstandard (RFC 8878):
  - `dec.rs`: frame decoder (raw/RLE/compressed blocks, Huffman literals 1/4 streams,
    FSE sequences incl. repeat/RLE modes, repeat offsets, skippable frames, XXH64 content
    checksum, window limit, formatted and raw-content dictionaries, frame-size/header
    helpers).
  - `enc.rs`: levels -131072..22 mapped to hash-chain search depth and greedy/lazy parsing;
    Huffman literals (direct or FSE-compressed weights), per-block predefined/RLE/custom FSE
    tables chosen by estimated cost; content size, checksum, dict id flags; streaming
    CONTINUE / FLUSH_BLOCK / FLUSH_FRAME with lazily written frame header; pledged size.
  - `dict.rs`: dictionary training (frequent cross-sample substrings, best last) and
    finalisation (entropy tables gathered by compressing the samples, all length codes and
    the needed offset codes kept representable).
  - `ref_tests.rs`: interop tests that dlopen the system libzstd if present (ours<->libzstd
    for levels, streaming+checksum, dictionaries).  libzstd is only a test oracle.
- Runtime modules: `_lzma` (modules/_lzma.rs), `_zstd` (modules/_zstd.rs; parameter
  bounds/names table, ZstdDict with digested/undigested/prefix modes, train/finalize),
  `_uuid` (modules/_uuid.rs: monotonic version-1 generation, random multicast node).
  All long operations release the thread state (`threads::blocking` via compress_common).
- Lib: `lzma.py`, `compression/zstd/{__init__,_zstdfile}.py`, `tarfile.py`, `uuid.py`,
  `zipimport.py`; zipfile: rewrote the central-directory/end-record writer, PyZipFile and
  the CLI in our own design (rule 9), plus fixes (archive-relative offsets when appending to
  non-zip data, ZIP64 with prepended data/extensible data, bounded decompression in
  `_read1`, direct seeks for stored members, SOURCE_DATE_EPOCH, mkdir checks).
- `crates/runtime/src/import.rs`: `zipimport.zipimporter` is inserted first in
  `sys.path_hooks` after importlib is installed (failure only disables zip imports).
- `crates/runtime/src/modules/pymain.rs`: running a zip/directory puts it on sys.path even
  with -I / -P (it is where `__main__` comes from); zip detection looks for the end record
  (archives with a prepended stub work).

## Decisions
- zstd version reported as 1.5.7; `nb_workers`/`job_size`/`overlap_log` bounds are (0, 0)
  (no multi-threaded compression), so the multithreading tests are skipped as on a
  single-threaded libzstd build.
- `_lzma` error messages use liblzma's wording; `_zstd` errors contain libzstd's error names
  ("Unknown frame descriptor", "Restored data doesn't match checksum", ...).
- tarfile extraction filters follow the documented PEP 706 semantics plus the hardening the
  3.15 tests require: names containing `..` are rewritten to the resolved relative path,
  link members named like the destination itself are refused, symlink targets are checked
  after normalisation (and relative to the link's parent ignoring a trailing slash), and a
  hard-link copy fallback is also checked at the link's own location.
- `_uuid.has_stable_extractable_node` is False (the node is random), so getnode() still
  asks `ip`/`ifconfig`.

## Test status (release build, this branch)
- test_lzma: all pass (1 skip: gettotalrefcount).
- test_zstd: all pass (multithreading tests skipped by design, see above).
- test_uuid: all pass (only the Windows/libuuid-node specific ones skip).
- test_tarfile: 779/781; the 2 failures (CommandLineTest list) need PYTHONIOENCODING
  support in the runtime (our interpreter ignores it).
- test_zipimport: all pass; test_zipimport_support: 3/5 — the remaining two fail inside
  doctest itself (doctest output/parsing differences), not in zipimport.
- test_zipfile (core): every test passes when run from a local copy; the spec module
  itself cannot be imported yet because the `shift_jis` codec is missing in master
  (the copy substitutes utf-8 for it; only its EncodedMetadataTests are then invalid).
- test_zipfile64: skipped (resource).

## Known gaps / ideas
- LZMA encoder has no optimal (price-based) parser; xz -6 output is ~20% smaller.
- Zstd encoder has no long-distance matching (the parameter is accepted and ignored) and
  no optimal parsing for btopt/btultra levels (they use deeper lazy search).
- `PYTHONIOENCODING` is ignored by the runtime (affects tarfile CLI tests).
