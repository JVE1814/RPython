use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

fn sample(n: usize, seed: u32) -> Vec<u8> {
    // Mixture of text-like repetitions and noise.
    let words: [&[u8]; 8] = [b"alpha ", b"beta ", b"gamma ", b"delta\n", b"epsilon ", b"zeta, ", b"eta. ", b"theta "];
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        if s % 7 == 0 {
            v.push((s >> 8) as u8);
        } else {
            v.extend_from_slice(words[(s >> 4) as usize % 8]);
        }
    }
    v.truncate(n);
    v
}

fn xz_tool(args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut c = Command::new("xz").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdin = c.stdin.take().unwrap();
    let inp = input.to_vec();
    let t = std::thread::spawn(move || {
        let _ = stdin.write_all(&inp);
    });
    let o = c.wait_with_output().ok()?;
    t.join().unwrap();
    if !o.status.success() {
        return None;
    }
    Some(o.stdout)
}

fn have_xz() -> bool {
    Command::new("xz").arg("--version").stdout(Stdio::null()).status().is_ok()
}

fn enc(format: Format, check: u32, chain: Vec<FilterSpec>, data: &[u8], pieces: usize) -> Vec<u8> {
    let mut e = Encoder::new(format, check, chain).unwrap();
    let mut out = Vec::new();
    if pieces <= 1 {
        e.compress(data, &mut out);
    } else {
        for c in data.chunks((data.len() / pieces).max(1)) {
            e.compress(c, &mut out);
        }
    }
    e.finish(&mut out);
    out
}

fn lz2(preset: u32) -> FilterSpec {
    FilterSpec::lzma(FILTER_LZMA2, LzOptions::preset(preset).unwrap())
}
fn lz1(preset: u32) -> FilterSpec {
    FilterSpec::lzma(FILTER_LZMA1, LzOptions::preset(preset).unwrap())
}

#[test]
fn roundtrip_xz_presets() {
    for (i, n) in [0usize, 1, 5, 100, 5000, 300_000].iter().enumerate() {
        let d = sample(*n, i as u32);
        for p in [0, 1, 6, 9 | PRESET_EXTREME] {
            for check in [CHECK_NONE, CHECK_CRC32, CHECK_CRC64, CHECK_SHA256] {
                let c = enc(Format::Xz, check, vec![lz2(p)], &d, 3);
                let (o, unused) = decompress(&c, Format::Auto, None).unwrap();
                assert_eq!(o, d, "n={} p={} check={}", n, p, check);
                assert_eq!(unused, 0);
            }
        }
    }
}

#[test]
fn roundtrip_alone_and_raw() {
    let d = sample(70_000, 9);
    let c = enc(Format::Alone, 0, vec![lz1(6)], &d, 1);
    assert_eq!(decompress(&c, Format::Auto, None).unwrap().0, d);
    assert_eq!(decompress(&c, Format::Alone, None).unwrap().0, d);
    for chain in [
        vec![lz1(2)],
        vec![lz2(3)],
        vec![FilterSpec { dist: 4, ..FilterSpec::simple(FILTER_DELTA) }, FilterSpec { start_offset: 0x40, ..FilterSpec::simple(FILTER_X86) }, lz2(4)],
        vec![FilterSpec::simple(FILTER_ARM), FilterSpec::simple(FILTER_ARMTHUMB), FilterSpec::simple(FILTER_SPARC), lz2(1)],
        vec![FilterSpec::simple(FILTER_IA64), FilterSpec::simple(FILTER_POWERPC), FilterSpec::simple(FILTER_ARM64), lz1(1)],
    ] {
        let c = enc(Format::Raw, 0, chain.clone(), &d, 7);
        let (o, _) = decompress(&c, Format::Raw, Some(chain.clone())).unwrap();
        assert_eq!(o, d);
    }
}

#[test]
fn incremental_decode_small_pieces() {
    let d = sample(40_000, 3);
    for (fmt, chain) in [(Format::Xz, vec![lz2(6)]), (Format::Alone, vec![lz1(6)])] {
        let c = enc(fmt, CHECK_CRC64, chain, &d, 1);
        let mut dec = Decoder::new(Format::Auto, None, None).unwrap();
        let mut out = Vec::new();
        for piece in c.chunks(7) {
            dec.feed(piece);
            out.extend(dec.read(13).unwrap());
            out.extend(dec.read(usize::MAX).unwrap());
        }
        assert!(dec.is_done());
        assert_eq!(out, d);
    }
}

#[test]
#[ignore]
fn bench_dec_only() {
    let d = sample(2_000_000, 78);
    let c = enc(Format::Xz, CHECK_CRC64, vec![lz2(0)], &d, 1);
    let t = std::time::Instant::now();
    for _ in 0..3 {
        let (o, _) = decompress(&c, Format::Xz, None).unwrap();
        assert_eq!(o.len(), d.len());
    }
    eprintln!("lzma dec x3: {:?}", t.elapsed());
}

#[test]
#[ignore]
fn bench_speed() {
    let d = sample(12_000_000, 77);
    let t = std::time::Instant::now();
    let c = enc(Format::Xz, CHECK_CRC64, vec![lz2(6)], &d, 1);
    let t1 = t.elapsed();
    let t = std::time::Instant::now();
    let (o, _) = decompress(&c, Format::Xz, None).unwrap();
    let t2 = t.elapsed();
    assert_eq!(o, d);
    eprintln!("lzma6: ratio {:.3} comp {:?} dec {:?}", c.len() as f64 / d.len() as f64, t1, t2);
    let t = std::time::Instant::now();
    let c = crate::zstd::compress(&d, 3);
    let t1 = t.elapsed();
    let t = std::time::Instant::now();
    let o = crate::zstd::decompress(&c).unwrap();
    let t2 = t.elapsed();
    assert_eq!(o, d);
    eprintln!("zstd3: ratio {:.3} comp {:?} dec {:?}", c.len() as f64 / d.len() as f64, t1, t2);
}

#[test]
fn x86_filter_is_split_invariant() {
    let mut d = Vec::new();
    for i in 0..20_000u32 {
        d.push(if i % 7 == 0 { 0xE8 } else { (i * 31) as u8 });
        if i % 11 == 0 {
            d.extend_from_slice(&[0, 0, 0xFF]);
        }
    }
    let mut f = Bcj::new(FILTER_X86, 0, true);
    let mut whole = d.clone();
    let n = f.convert(&mut whole);
    let mut st = Stage::new(Box::new(Bcj::new(FILTER_X86, 0, true)));
    let mut pieces = Vec::new();
    for c in d.chunks(13) {
        st.push(c, false, &mut pieces);
    }
    st.push(&[], true, &mut pieces);
    assert_eq!(&pieces[..n], &whole[..n]);
    let mut back = Stage::new(Box::new(Bcj::new(FILTER_X86, 0, false)));
    let mut o = Vec::new();
    for c in pieces.chunks(5) {
        back.push(c, false, &mut o);
    }
    back.push(&[], true, &mut o);
    assert_eq!(o, d);
}

#[test]
fn filter_props() {
    let mut o = LzOptions::preset(6).unwrap();
    o.dict_size = 8 << 20;
    let f = FilterSpec::lzma(FILTER_LZMA1, o);
    assert_eq!(f.encode_props().unwrap(), b"]\x00\x00\x80\x00");
    assert!(FilterSpec::decode_props(FILTER_DELTA, b"too long").is_err());
}

#[test]
fn cross_check_with_xz_tool() {
    if !have_xz() {
        return;
    }
    let d = sample(1_500_000, 11);
    // ours -> xz
    for p in [0, 6] {
        for check in [CHECK_NONE, CHECK_CRC32, CHECK_CRC64, CHECK_SHA256] {
            let c = enc(Format::Xz, check, vec![lz2(p)], &d, 5);
            assert_eq!(xz_tool(&["-dc"], &c).expect("xz rejected our stream"), d);
        }
    }
    let c = enc(Format::Alone, 0, vec![lz1(6)], &d, 2);
    assert_eq!(xz_tool(&["-dc", "--format=lzma"], &c).expect("xz rejected .lzma"), d);
    let chain = vec![FilterSpec { dist: 2, ..FilterSpec::simple(FILTER_DELTA) }, FilterSpec::simple(FILTER_X86), lz2(5)];
    let c = enc(Format::Raw, 0, chain.clone(), &d, 3);
    assert_eq!(xz_tool(&["-dc", "--format=raw", "--delta=dist=2", "--x86", "--lzma2=preset=5"], &c).expect("raw"), d);
    // xz -> ours
    for args in [&["-zc", "-0"][..], &["-zc", "-6", "--check=sha256"], &["-zc", "-9e", "--check=crc32"], &["-zc", "--check=none", "--arm", "--lzma2=preset=1,lc=1,lp=3,pb=0"]] {
        let c = xz_tool(args, &d).unwrap();
        assert_eq!(decompress(&c, Format::Xz, None).unwrap().0, d, "{:?}", args);
    }
    let c = xz_tool(&["-zc", "--format=lzma", "-7"], &d).unwrap();
    assert_eq!(decompress(&c, Format::Alone, None).unwrap().0, d);
    let c = xz_tool(&["-zc", "--format=raw", "--ia64", "--delta=dist=7", "--lzma1=preset=3"], &d).unwrap();
    let chain = vec![FilterSpec::simple(FILTER_IA64), FilterSpec { dist: 7, ..FilterSpec::simple(FILTER_DELTA) }, lz1(3)];
    assert_eq!(decompress(&c, Format::Raw, Some(chain)).unwrap().0, d);
    // incompressible data exercises stored LZMA2 chunks
    let mut noise = Vec::new();
    let mut s = 12345u64;
    for _ in 0..300_000 {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        noise.push((s >> 56) as u8);
    }
    let c = enc(Format::Xz, CHECK_CRC64, vec![lz2(6)], &noise, 2);
    assert_eq!(xz_tool(&["-dc"], &c).unwrap(), noise);
    assert!(c.len() < noise.len() + 200);
    // compression ratio sanity vs xz
    let ours = enc(Format::Xz, CHECK_CRC64, vec![lz2(6)], &d, 1).len();
    let theirs = xz_tool(&["-zc", "-6"], &d).unwrap().len();
    eprintln!("ratio: ours {} xz {}", ours, theirs);
    assert!(ours < theirs * 13 / 10, "ours {} theirs {}", ours, theirs);
}
