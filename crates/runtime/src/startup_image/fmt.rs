//! (perf6) Byte format of the start-up image, shared by the build script that writes it
//! (`crates/runtime/build/main.rs`) and the runtime that reads it (`startup_image`).
//!
//! Layout (all integers are LEB128-style unsigned varints unless noted):
//! ```text
//! magic  "RPYIMG02"
//! nstr   then per string: kind byte (0 = UTF-8, 1 = code points as u32 LE), varint length
//!        (bytes resp. code points), the data
//! nmod   then per module: string id of its path below Lib/, varint offset of its code record
//!        (relative to the start of the image)
//! code records (nested code objects are inline constants of their parent)
//! ```
//! A code record: argcount, posonlyargcount, kwonlyargcount, stacksize, flags, firstlineno;
//! co_code (blob); consts (count + tagged constants); names (count + string ids);
//! localsplusnames (count + string ids); localspluskinds (blob); name, qualname (string ids);
//! linetable, exceptiontable (blobs); then the translated form as one blob (empty = the
//! translator failed at build time: the runtime translates it itself) holding the internal words
//! (count + u32 LE each), the exception entries (count + start, end, target, depth, lasti),
//! the extra constants (count + tagged), pub_of_internal (delta varints, one per word),
//! internal_of_pub (count + delta varints), eager_prologue byte, gen_start, and the line of
//! each internal word (zigzag delta varints; -1 = none).
#![allow(dead_code)]

pub const MAGIC: &[u8; 8] = b"RPYIMG02";

// constant tags
pub const C_NONE: u8 = b'N';
pub const C_TRUE: u8 = b'T';
pub const C_FALSE: u8 = b'F';
pub const C_ELLIPSIS: u8 = b'E';
pub const C_INT: u8 = b'i';
pub const C_BIGINT: u8 = b'I';
pub const C_FLOAT: u8 = b'f';
pub const C_COMPLEX: u8 = b'c';
pub const C_STR: u8 = b's';
pub const C_BYTES: u8 = b'b';
pub const C_TUPLE: u8 = b't';
pub const C_FROZENSET: u8 = b'z';
pub const C_SLICE: u8 = b'l';
pub const C_CODE: u8 = b'C';
// extra constants of the translated form
pub const X_INT: u8 = b'i';
pub const X_NONE: u8 = b'N';
pub const X_EMPTYSTR: u8 = b'e';
pub const X_TRUE: u8 = b'T';
pub const X_FALSE: u8 = b'F';

// string kinds
pub const S_ASCII: u8 = 0;
pub const S_CPS: u8 = 1;
pub const S_UTF8: u8 = 2;

/// Sequences of small steps (instruction offsets, line numbers): each element is the
/// zigzag delta to its predecessor, one byte when below 255, else 255 and a varint.
pub fn put_deltas(o: &mut Out, vals: impl Iterator<Item = i64>) {
    let mut prev = 0i64;
    for v in vals {
        let z = zigzag(v - prev);
        prev = v;
        if z < 255 {
            o.byte(z as u8);
        } else {
            o.byte(255);
            o.u(z);
        }
    }
}

pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
pub fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

#[derive(Default)]
pub struct Out {
    pub buf: Vec<u8>,
}

impl Out {
    pub fn u(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }
    pub fn i(&mut self, v: i64) {
        self.u(zigzag(v));
    }
    pub fn byte(&mut self, b: u8) {
        self.buf.push(b);
    }
    pub fn blob(&mut self, b: &[u8]) {
        self.u(b.len() as u64);
        self.buf.extend_from_slice(b);
    }
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

/// Reader over the image bytes. The image is produced by our own build script, so
/// malformed data is a build bug: reads past the end panic.
#[derive(Clone, Copy)]
pub struct In<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> In<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> In<'a> {
        In { data, pos }
    }
    #[inline]
    pub fn u(&mut self) -> u64 {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.data[self.pos];
            self.pos += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }
    #[inline]
    pub fn n(&mut self) -> usize {
        self.u() as usize
    }
    #[inline]
    pub fn i(&mut self) -> i64 {
        unzigzag(self.u())
    }
    #[inline]
    pub fn byte(&mut self) -> u8 {
        let b = self.data[self.pos];
        self.pos += 1;
        b
    }
    #[inline]
    pub fn take(&mut self, n: usize) -> &'a [u8] {
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        s
    }
    #[inline]
    pub fn blob(&mut self) -> &'a [u8] {
        let n = self.n();
        self.take(n)
    }
    /// `n` elements written by `put_deltas`, each converted with `f`.
    pub fn deltas<T>(&mut self, n: usize, f: impl Fn(i64) -> T) -> Vec<T> {
        let mut v = Vec::with_capacity(n);
        let mut prev = 0i64;
        for _ in 0..n {
            let b = self.data[self.pos];
            self.pos += 1;
            let z = if b < 255 { b as u64 } else { self.u() };
            prev += unzigzag(z);
            v.push(f(prev));
        }
        v
    }
}
