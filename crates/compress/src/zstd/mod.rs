//! Zstandard (RFC 8878): frame decoder, encoder with compression levels, dictionaries and
//! dictionary training.

pub mod bits;
pub mod dec;
pub mod dict;
pub mod enc;
pub mod fse;
pub mod huf;
pub mod xxh;

pub const FRAME_MAGIC: u32 = 0xFD2F_B528;
pub const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
pub const DICT_MAGIC: u32 = 0xEC30_A437;
pub const BLOCK_MAX: usize = 1 << 17;

pub const MAX_LL_CODE: usize = 35;
pub const MAX_ML_CODE: usize = 52;
pub const MAX_OF_CODE: usize = 31;
pub const LL_MAX_LOG: u32 = 9;
pub const ML_MAX_LOG: u32 = 9;
pub const OF_MAX_LOG: u32 = 8;

pub const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024, 2048, 4096, 8192,
    16384, 32768, 65536,
];
pub const LL_BITS: [u8; 36] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
pub const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41,
    43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195, 16387, 32771, 65539,
];
pub const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9,
    10, 11, 12, 13, 14, 15, 16,
];
pub const LL_DEFAULT: [i16; 36] = [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
pub const ML_DEFAULT: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    -1, -1, -1, -1, -1, -1, -1,
];
pub const OF_DEFAULT: [i16; 29] = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Corrupt,
    Checksum,
    UnknownFrame,
    DictMismatch,
    DictCorrupt,
    WindowTooLarge,
    SrcSizeWrong,
    Parameter,
    DictTraining,
}

impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Error::Corrupt => "Data corruption detected",
            Error::Checksum => "Restored data doesn't match checksum",
            Error::UnknownFrame => "Unknown frame descriptor",
            Error::DictMismatch => "Dictionary mismatch",
            Error::DictCorrupt => "Dictionary is corrupted",
            Error::WindowTooLarge => "Frame requires too much memory for decoding",
            Error::SrcSizeWrong => "Src size is incorrect",
            Error::Parameter => "Unsupported parameter",
            Error::DictTraining => "Dictionary training failed",
        }
    }
}

/// Library version reported to Python (format-compatible with zstd 1.5.x).
pub const VERSION: (u32, u32, u32) = (1, 5, 7);

pub const CLEVEL_MIN: i32 = -(1 << 17);
pub const CLEVEL_MAX: i32 = 22;
pub const CLEVEL_DEFAULT: i32 = 3;

/// One-shot helpers.
pub fn compress(data: &[u8], level: i32) -> Vec<u8> {
    let mut p = enc::Params::from_level(level);
    p.content_size = true;
    let mut e = enc::Encoder::new(p, None);
    let mut out = Vec::new();
    e.compress(data, enc::Mode::End, &mut out).unwrap();
    out
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut rest = data;
    loop {
        let mut d = dec::Decoder::new(None, 27);
        d.feed(rest);
        out.extend(d.read(usize::MAX)?);
        if !d.is_done() {
            return Err(Error::Corrupt);
        }
        let used = rest.len() - d.unused().len();
        rest = &rest[used..];
        if rest.is_empty() {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ref_tests;
