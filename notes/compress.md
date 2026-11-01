# compress workstream notes

## Layout / decisions
- `crates/compress` (rpy-compress): pure-Rust algorithms, no Python objects, own unit tests
  (`cargo test -p rpy-compress --release`; the deflate tests cross-check against the system
  `gzip` binary when present).  Runtime modules are thin bindings.
  - checksum.rs: CRC-32 (slice-by-16 + PCLMULQDQ folding), Adler-32 (zlib-exact reduction
    behaviour for out-of-range init values), crc32/adler32_combine, CRC-64 (XZ).
  - inflate.rs: resumable inflater (raw/zlib/gzip/auto), unit checkpoints + internal pending
    buffer, fast 64-bit-refill decode loop, output limit (max_length) support.
  - deflate.rs: hash-chain matcher (greedy 1-3, lazy 4-9), zlib-like level table, strategies,
    exact-cost block choice (stored/fixed/dynamic), all flush modes, zdict, wbits 9..15.
    Output depends only on input bytes + flush calls (compress() == compressobj chunks).
    Not byte-identical to libz (no test needs that).
  - hash.rs: MD5, SHA-1, SHA-2 (SHA-NI accelerated SHA-256), SHA-512/t, SHA-3/SHAKE,
    BLAKE2b/s with all parameters, HMAC, PBKDF2, scrypt.
- `_hashlib` IS provided (decision): an OpenSSL-API-compatible module backed by rpy-compress
  (new, openssl_*, HASH/HASHXOF/HMAC, pbkdf2_hmac, scrypt, hmac_new/hmac_digest,
  compare_digest, get_fips_mode()==0, openssl_md_meth_names, _constructors mapping
  constructor->name, UnsupportedDigestmodError).  Reason: CPython builds always have it and
  many tests (hashlib, hmac, ...) only run their OpenSSL variants when it exists.
- `_hmac` (3.14 builtin HMAC) provided with compute_* functions and UnknownHashError.
- modules/compress_common.rs: `StateObj<T>` (object holding `Mutex<T>`), lock acquired in
  blocked thread mode; `maybe_blocking()` releases the thread state for >= 64 KiB of work.
- Lib/compression/ package: `_common/_streams.py` (BaseStream, DecompressReader,
  BUFFER_SIZE), re-export modules gzip/zlib/bz2/lzma; `Lib/_compression.py` alias kept.

## Status (spec tests, run with local stubs for missing array/py_compile/unittest.mock)
- test_zlib 88 run, 1 failure (descr.rs tp_new_wrapper, see below)
- test_gzip: all pass except test_fileobj_mode (io mode) and test_issue44439 (array stub)
- test_bz2 102 run, only test_issue44439 (array stub) fails
- test_hmac: only the 2 check_disallow_instantiation failures (descr.rs)
- test_base64, test_binascii, test_secrets: pass
- test_hashlib needs http.client; test_zipfile needs pathlib (not in master yet) — untested
  beyond manual scripts.

## Not done yet
- _lzma / lzma.py (LZMA/LZMA2/XZ codec), _zstd / compression.zstd, tarfile, zipimport,
  uuid.  zipfile: ZIP_LZMA/ZIP_ZSTANDARD members need those modules.

## Issues found in other areas (not fixed here)
- `types/descr.rs` tp_new_wrapper: for types with tp_new == NULL (DISALLOW_INSTANTIATION),
  `T.__new__(T)` must raise "cannot create 'mod.T' instances" before the "is not safe" check
  (CPython checks `staticbase->tp_new == NULL` first).  Breaks check_disallow_instantiation
  in test_zlib/test_hashlib/test_hmac.
- io.open(): CPython sets the raw/buffered `.mode` attribute to the mode string passed to
  open() ('wb+'), ours reports FileIO's 'rb+'.  Breaks test_gzip.test_fileobj_mode.
- Missing in master at the time of writing: set.union / set `|`, array, unittest.mock,
  py_compile, http (tests were run with local stubs outside the repo).
