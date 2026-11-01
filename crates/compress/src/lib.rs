//! Compression, checksum and hash algorithms written from the public format specifications
//! (RFC 1950/1951/1952 DEFLATE/zlib/gzip, bzip2, LZMA/XZ, RFC 8878 Zstandard, RFC 1321 MD5,
//! FIPS 180-4 SHA-1/SHA-2, FIPS 202 SHA-3, RFC 7693 BLAKE2).  No Python-object code here:
//! the runtime modules (`zlib`, `_bz2`, `_lzma`, `_zstd`, `_md5`, ...) are thin bindings.

pub mod checksum;
pub mod huffman;
pub mod inflate;
pub mod deflate;
pub mod hash;
