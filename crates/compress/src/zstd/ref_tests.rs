//! Interoperability tests against the system libzstd, loaded dynamically when present
//! (skipped otherwise).  Only used as an external reference codec.

use super::*;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

unsafe extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
}

type Compress = unsafe extern "C" fn(*mut u8, usize, *const u8, usize, c_int) -> usize;
type Decompress = unsafe extern "C" fn(*mut u8, usize, *const u8, usize) -> usize;
type IsError = unsafe extern "C" fn(usize) -> u32;
type CreateCtx = unsafe extern "C" fn() -> *mut c_void;
type DecUsingDict = unsafe extern "C" fn(*mut c_void, *mut u8, usize, *const u8, usize, *const u8, usize) -> usize;
type CompUsingDict = unsafe extern "C" fn(*mut c_void, *mut u8, usize, *const u8, usize, *const u8, usize, c_int) -> usize;

struct Lib {
    compress: Compress,
    decompress: Decompress,
    is_error: IsError,
    dctx: CreateCtx,
    cctx: CreateCtx,
    dec_dict: DecUsingDict,
    comp_dict: CompUsingDict,
}

fn lib() -> Option<Lib> {
    unsafe {
        let mut h = dlopen(c"libzstd.so.1".as_ptr(), 2);
        if h.is_null() {
            h = dlopen(c"libzstd.so".as_ptr(), 2);
        }
        if h.is_null() {
            return None;
        }
        let f = |n: &std::ffi::CStr| dlsym(h, n.as_ptr());
        let ptrs = [
            f(c"ZSTD_compress"),
            f(c"ZSTD_decompress"),
            f(c"ZSTD_isError"),
            f(c"ZSTD_createDCtx"),
            f(c"ZSTD_createCCtx"),
            f(c"ZSTD_decompress_usingDict"),
            f(c"ZSTD_compress_usingDict"),
        ];
        if ptrs.iter().any(|p| p.is_null()) {
            return None;
        }
        Some(Lib {
            compress: std::mem::transmute::<*mut c_void, Compress>(ptrs[0]),
            decompress: std::mem::transmute::<*mut c_void, Decompress>(ptrs[1]),
            is_error: std::mem::transmute::<*mut c_void, IsError>(ptrs[2]),
            dctx: std::mem::transmute::<*mut c_void, CreateCtx>(ptrs[3]),
            cctx: std::mem::transmute::<*mut c_void, CreateCtx>(ptrs[4]),
            dec_dict: std::mem::transmute::<*mut c_void, DecUsingDict>(ptrs[5]),
            comp_dict: std::mem::transmute::<*mut c_void, CompUsingDict>(ptrs[6]),
        })
    }
}

fn data(n: usize, seed: u64) -> Vec<u8> {
    let words: [&[u8]; 6] = [b"alpha ", b"beta\n", b"gamma = 12; ", b"delta(x) ", b"{epsilon} ", b"zeta, "];
    let mut s = seed.wrapping_add(17);
    let mut v = Vec::new();
    while v.len() < n {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if (s >> 60) == 0 {
            v.push((s >> 40) as u8);
        } else {
            v.extend_from_slice(words[(s >> 33) as usize % 6]);
        }
    }
    v.truncate(n);
    v
}

#[test]
fn reference_decodes_ours() {
    let Some(l) = lib() else { eprintln!("libzstd not found: skipped"); return };
    for (i, n) in [0usize, 5, 300, 5000, 200_000, 1_000_000].iter().enumerate() {
        let d = data(*n, i as u64);
        for level in [-3, 1, 3, 7, 15, 22] {
            let c = compress(&d, level);
            let mut out = vec![0u8; d.len() + 16];
            let r = unsafe { (l.decompress)(out.as_mut_ptr(), out.len(), c.as_ptr(), c.len()) };
            assert_eq!(unsafe { (l.is_error)(r) }, 0, "n={} level={}", n, level);
            assert_eq!(&out[..r], &d[..]);
        }
    }
    // checksum + streaming frame
    let d = data(500_000, 9);
    let mut p = enc::Params::from_level(5);
    p.checksum = true;
    let mut e = enc::Encoder::new(p, None);
    let mut c = Vec::new();
    for ch in d.chunks(70_000) {
        e.compress(ch, enc::Mode::Continue, &mut c).unwrap();
    }
    e.compress(&[], enc::Mode::End, &mut c).unwrap();
    let mut out = vec![0u8; d.len() + 16];
    let r = unsafe { (l.decompress)(out.as_mut_ptr(), out.len(), c.as_ptr(), c.len()) };
    assert_eq!(unsafe { (l.is_error)(r) }, 0);
    assert_eq!(&out[..r], &d[..]);
}

#[test]
fn ours_decodes_reference() {
    let Some(l) = lib() else { eprintln!("libzstd not found: skipped"); return };
    for (i, n) in [1usize, 100, 4000, 150_000, 2_000_000].iter().enumerate() {
        let mut d = data(*n, 100 + i as u64);
        if i == 3 {
            // binary-looking data with all byte values
            for (k, b) in d.iter_mut().enumerate() {
                if k % 3 == 0 {
                    *b = (k * 7) as u8;
                }
            }
        }
        for level in [-5, 1, 3, 9, 19] {
            let mut c = vec![0u8; d.len() + d.len() / 100 + 1024];
            let r = unsafe { (l.compress)(c.as_mut_ptr(), c.len(), d.as_ptr(), d.len(), level) };
            assert_eq!(unsafe { (l.is_error)(r) }, 0);
            c.truncate(r);
            assert_eq!(decompress(&c).unwrap(), d, "n={} level={}", n, level);
        }
    }
}

#[test]
fn dictionaries_interoperate() {
    let Some(l) = lib() else { eprintln!("libzstd not found: skipped"); return };
    let samples: Vec<Vec<u8>> = (0..200).map(|i| data(300, 1000 + i)).collect();
    let sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();
    let cat: Vec<u8> = samples.concat();
    let d = dict::train(&cat, &sizes, 4096).unwrap();
    let cd = Arc::new(enc::CDict::load(&d, false).unwrap());
    let dd = Arc::new(dec::DDict::load(&d, false).unwrap());
    for s in &samples[..20] {
        // ours -> reference
        let mut e = enc::Encoder::new(enc::Params::from_level(3), Some((cd.clone(), false)));
        let mut c = Vec::new();
        e.compress(s, enc::Mode::End, &mut c).unwrap();
        let mut out = vec![0u8; s.len() + 16];
        let r = unsafe { (l.dec_dict)((l.dctx)(), out.as_mut_ptr(), out.len(), c.as_ptr(), c.len(), d.as_ptr(), d.len()) };
        assert_eq!(unsafe { (l.is_error)(r) }, 0);
        assert_eq!(&out[..r], &s[..]);
        // reference -> ours
        let mut c = vec![0u8; s.len() + 512];
        let r = unsafe { (l.comp_dict)((l.cctx)(), c.as_mut_ptr(), c.len(), s.as_ptr(), s.len(), d.as_ptr(), d.len(), 3) };
        assert_eq!(unsafe { (l.is_error)(r) }, 0);
        c.truncate(r);
        let mut dc = dec::Decoder::new(Some(dd.clone()), 27);
        dc.feed(&c);
        assert_eq!(&dc.read(usize::MAX).unwrap(), s);
    }
}
