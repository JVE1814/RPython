use super::*;
use std::sync::Arc;

fn sample(n: usize, seed: u32) -> Vec<u8> {
    let words: [&[u8]; 8] = [b"red ", b"green ", b"blue = ", b"silver\n", b"lion ", b"tiger, ", b"cat. ", b"dog "];
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(7);
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        if s % 5 == 0 {
            v.push((s >> 8) as u8);
        } else {
            v.extend_from_slice(words[(s >> 4) as usize % 8]);
        }
    }
    v.truncate(n);
    v
}

#[test]
fn xxh64_vectors() {
    assert_eq!(xxh::xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
    let d = sample(1000, 1);
    let mut h = xxh::Xxh64::new(0);
    for c in d.chunks(7) {
        h.update(c);
    }
    assert_eq!(h.digest(), xxh::xxh64(&d, 0));
}

#[test]
fn roundtrip_levels() {
    for (i, n) in [0usize, 1, 10, 100, 1000, 70_000, 400_000].iter().enumerate() {
        let d = sample(*n, i as u32);
        for level in [-5, 1, 3, 9, 19, 22] {
            let c = compress(&d, level);
            assert_eq!(decompress(&c).unwrap(), d, "n={} level={}", n, level);
        }
    }
}

#[test]
fn random_bytes_and_runs() {
    let mut noise = Vec::new();
    let mut s = 99u64;
    for _ in 0..200_000 {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        noise.push((s >> 56) as u8);
    }
    let c = compress(&noise, 3);
    assert_eq!(decompress(&c).unwrap(), noise);
    assert!(c.len() < noise.len() + 100);
    let low: Vec<u8> = noise.iter().map(|b| b & 0x7F).collect();
    assert_eq!(decompress(&compress(&low, 3)).unwrap(), low);
    let runs = vec![b'a'; 300_000];
    let c = compress(&runs, 3);
    assert!(c.len() < 100);
    assert_eq!(decompress(&c).unwrap(), runs);
}

#[test]
fn streaming_modes_and_checksum() {
    let d = sample(300_000, 5);
    let mut p = enc::Params::from_level(3);
    p.checksum = true;
    let mut e = enc::Encoder::new(p, None);
    let mut out = Vec::new();
    for c in d.chunks(50_000) {
        e.compress(c, enc::Mode::Continue, &mut out).unwrap();
    }
    e.compress(&[], enc::Mode::Flush, &mut out).unwrap();
    e.compress(b"tail", enc::Mode::End, &mut out).unwrap();
    let mut want = d.clone();
    want.extend_from_slice(b"tail");
    let mut dec = dec::Decoder::new(None, 27);
    let mut got = Vec::new();
    for c in out.chunks(3) {
        dec.feed(c);
        got.extend(dec.read(1000).unwrap());
    }
    while !dec.is_done() {
        let x = dec.read(1000).unwrap();
        assert!(!x.is_empty());
        got.extend(x);
    }
    assert_eq!(got, want);
    let mut bad = out.clone();
    let n = bad.len();
    bad[n - 1] ^= 1;
    assert_eq!(decompress(&bad), Err(Error::Checksum));
}

#[test]
fn long_streams_slide_the_window() {
    // More than twice the level-1 window: history is dropped while encoding.
    let d = sample(3_500_000, 21);
    let mut e = enc::Encoder::new(enc::Params::from_level(1), None);
    for _ in 0..2 {
        let mut out = Vec::new();
        for c in d.chunks(300_000) {
            e.compress(c, enc::Mode::Continue, &mut out).unwrap();
        }
        e.compress(&[], enc::Mode::End, &mut out).unwrap();
        assert_eq!(decompress(&out).unwrap(), d);
    }
    let c = compress(&d, 1);
    assert_eq!(decompress(&c).unwrap(), d);
}

#[test]
fn dictionaries() {
    let samples: Vec<Vec<u8>> = (0..300).map(|i| sample(200, i)).collect();
    let mut cat = Vec::new();
    let sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();
    for s in &samples {
        cat.extend_from_slice(s);
    }
    let d = dict::train(&cat, &sizes, 3072).unwrap();
    assert!(d.len() <= 3072);
    let cd = Arc::new(enc::CDict::load(&d, false).unwrap());
    let dd = Arc::new(dec::DDict::load(&d, false).unwrap());
    assert_ne!(cd.id, 0);
    let mut plain = 0;
    let mut with = 0;
    for s in &samples[..50] {
        let mut e = enc::Encoder::new(enc::Params::from_level(3), Some((cd.clone(), false)));
        let mut out = Vec::new();
        e.compress(s, enc::Mode::End, &mut out).unwrap();
        with += out.len();
        plain += compress(s, 3).len();
        let mut dc = dec::Decoder::new(Some(dd.clone()), 27);
        dc.feed(&out);
        assert_eq!(&dc.read(usize::MAX).unwrap(), s);
        assert!(dc.is_done());
        let mut wrong = dec::Decoder::new(None, 27);
        wrong.feed(&out);
        assert_eq!(wrong.read(usize::MAX), Err(Error::DictMismatch));
    }
    assert!(with < plain, "dict {} plain {}", with, plain);
    let f = dict::finalize(&d, &cat, &sizes, 200 * 1024, 6).unwrap();
    assert!(dec::DDict::load(&f, false).is_ok());
}

#[test]
fn skippable_and_frame_info() {
    let mut f = (SKIPPABLE_MAGIC + 3).to_le_bytes().to_vec();
    f.extend_from_slice(&5u32.to_le_bytes());
    f.extend_from_slice(b"hello");
    let c = compress(b"abc", 3);
    let mut all = f.clone();
    all.extend_from_slice(&c);
    all.extend_from_slice(&f);
    assert_eq!(decompress(&all).unwrap(), b"abc");
    assert_eq!(dec::frame_size(&c).unwrap(), Some(c.len()));
    let (h, _) = dec::parse_frame_header(&c).unwrap().unwrap();
    assert_eq!(h.content_size, Some(3));
    assert_eq!(decompress(b"aaaaaaaaa"), Err(Error::UnknownFrame));
}
