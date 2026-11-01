//! CRC-32 (IEEE, reflected 0xEDB88320), Adler-32 and CRC-64 (ECMA-182, used by XZ).

const POLY: u32 = 0xEDB8_8320;

const fn make_crc_tables() -> [[u32; 256]; 16] {
    let mut t = [[0u32; 256]; 16];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 16 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

static CRC_TABLES: [[u32; 256]; 16] = make_crc_tables();

/// Update a CRC-32 with `data` (zlib's `crc32(crc, buf, len)` semantics).
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        if data.len() >= 64 && std::arch::is_x86_feature_detected!("pclmulqdq") && std::arch::is_x86_feature_detected!("sse4.1") {
            return unsafe { clmul::crc32_clmul(crc, data) };
        }
    }
    crc32_slice16(crc, data)
}

/// Portable table-driven CRC-32 (slicing by 16).
pub fn crc32_slice16(crc: u32, data: &[u8]) -> u32 {
    let t = &CRC_TABLES;
    let mut c = !crc;
    let mut chunks = data.chunks_exact(16);
    for ch in &mut chunks {
        let a = u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]) ^ c;
        let b = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        let d = u32::from_le_bytes([ch[8], ch[9], ch[10], ch[11]]);
        let e = u32::from_le_bytes([ch[12], ch[13], ch[14], ch[15]]);
        c = t[15][(a & 0xff) as usize]
            ^ t[14][((a >> 8) & 0xff) as usize]
            ^ t[13][((a >> 16) & 0xff) as usize]
            ^ t[12][(a >> 24) as usize]
            ^ t[11][(b & 0xff) as usize]
            ^ t[10][((b >> 8) & 0xff) as usize]
            ^ t[9][((b >> 16) & 0xff) as usize]
            ^ t[8][(b >> 24) as usize]
            ^ t[7][(d & 0xff) as usize]
            ^ t[6][((d >> 8) & 0xff) as usize]
            ^ t[5][((d >> 16) & 0xff) as usize]
            ^ t[4][(d >> 24) as usize]
            ^ t[3][(e & 0xff) as usize]
            ^ t[2][((e >> 8) & 0xff) as usize]
            ^ t[1][((e >> 16) & 0xff) as usize]
            ^ t[0][(e >> 24) as usize];
    }
    for &b in chunks.remainder() {
        c = t[0][((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(target_arch = "x86_64")]
mod clmul {
    //! Carry-less multiplication folding (Intel, "Fast CRC Computation for Generic Polynomials
    //! Using PCLMULQDQ").  Constants are x^n mod P for the bit-reflected IEEE polynomial.
    use std::arch::x86_64::*;

    const K1: i64 = 0x1_5444_2bd4;
    const K2: i64 = 0x1_c6e4_1596;
    const K3: i64 = 0x1_7519_97d0;
    const K4: i64 = 0x0_ccaa_009e;
    const K5: i64 = 0x1_63cd_6124;
    const P_X: i64 = 0x1_DB71_0641;
    const U_PRIME: i64 = 0x1_F701_1641;

    #[target_feature(enable = "pclmulqdq,sse2,sse4.1")]
    pub unsafe fn crc32_clmul(crc: u32, data: &[u8]) -> u32 {
        unsafe {
            let mut p = data.as_ptr();
            let mut len = data.len();
            let k1k2 = _mm_set_epi64x(K2, K1);
            let mut x0 = _mm_loadu_si128(p as *const __m128i);
            let mut x1 = _mm_loadu_si128(p.add(16) as *const __m128i);
            let mut x2 = _mm_loadu_si128(p.add(32) as *const __m128i);
            let mut x3 = _mm_loadu_si128(p.add(48) as *const __m128i);
            x0 = _mm_xor_si128(x0, _mm_cvtsi32_si128(!crc as i32));
            p = p.add(64);
            len -= 64;
            while len >= 64 {
                macro_rules! fold {
                    ($x:ident, $off:expr) => {{
                        let lo = _mm_clmulepi64_si128($x, k1k2, 0x00);
                        let hi = _mm_clmulepi64_si128($x, k1k2, 0x11);
                        let y = _mm_loadu_si128(p.add($off) as *const __m128i);
                        $x = _mm_xor_si128(_mm_xor_si128(lo, hi), y);
                    }};
                }
                fold!(x0, 0);
                fold!(x1, 16);
                fold!(x2, 32);
                fold!(x3, 48);
                p = p.add(64);
                len -= 64;
            }
            let k3k4 = _mm_set_epi64x(K4, K3);
            macro_rules! fold1 {
                ($a:expr, $b:expr) => {{
                    let lo = _mm_clmulepi64_si128($a, k3k4, 0x00);
                    let hi = _mm_clmulepi64_si128($a, k3k4, 0x11);
                    _mm_xor_si128(_mm_xor_si128(lo, hi), $b)
                }};
            }
            let mut x = fold1!(x0, x1);
            x = fold1!(x, x2);
            x = fold1!(x, x3);
            while len >= 16 {
                let y = _mm_loadu_si128(p as *const __m128i);
                x = fold1!(x, y);
                p = p.add(16);
                len -= 16;
            }
            let mask32 = _mm_set_epi32(0, 0, 0, -1);
            let t = _mm_clmulepi64_si128(x, k3k4, 0x10);
            x = _mm_xor_si128(_mm_srli_si128(x, 8), t);
            let k5 = _mm_set_epi64x(0, K5);
            let t = _mm_clmulepi64_si128(_mm_and_si128(x, mask32), k5, 0x00);
            x = _mm_xor_si128(_mm_srli_si128(x, 4), t);
            let pu = _mm_set_epi64x(U_PRIME, P_X);
            let t1 = _mm_clmulepi64_si128(_mm_and_si128(x, mask32), pu, 0x10);
            let t2 = _mm_clmulepi64_si128(_mm_and_si128(t1, mask32), pu, 0x00);
            let c = _mm_extract_epi32(_mm_xor_si128(x, t2), 1) as u32;
            let rest = std::slice::from_raw_parts(p, len);
            super::crc32_slice16(!c, rest)
        }
    }
}

// ---- CRC-32 combination (polynomial arithmetic modulo P over GF(2)) ----

fn multmodp(a: u32, mut b: u32) -> u32 {
    let mut m: u32 = 1 << 31;
    let mut p: u32 = 0;
    while m != 0 {
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                break;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
    p
}

static X2N: std::sync::OnceLock<[u32; 64]> = std::sync::OnceLock::new();

/// x^(n * 2^k) mod P.
fn x2nmodp(mut n: u64, mut k: u32) -> u32 {
    let t = X2N.get_or_init(|| {
        let mut t = [0u32; 64];
        let mut p: u32 = 1 << 30; // x^1
        for e in t.iter_mut() {
            *e = p;
            p = multmodp(p, p);
        }
        t
    });
    let mut p: u32 = 1 << 31;
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(t[(k & 63) as usize], p);
        }
        n >>= 1;
        k += 1;
    }
    p
}

/// zlib `crc32_combine(crc1, crc2, len2)`.
pub fn crc32_combine(crc1: u32, crc2: u32, len2: u64) -> u32 {
    multmodp(x2nmodp(len2, 3), crc1) ^ crc2
}

// ---- Adler-32 (zlib's reduction behaviour is observable for init values whose halves are
// >= 65521, so the short-input paths reduce exactly like zlib) ----

const BASE: u32 = 65521;
const NMAX: usize = 5552;

pub fn adler32(adler: u32, data: &[u8]) -> u32 {
    let mut sum2 = (adler >> 16) & 0xffff;
    let mut a = adler & 0xffff;
    let len = data.len();
    if len == 1 {
        a += data[0] as u32;
        if a >= BASE {
            a -= BASE;
        }
        sum2 += a;
        if sum2 >= BASE {
            sum2 -= BASE;
        }
        return a | (sum2 << 16);
    }
    if len < 16 {
        for &b in data {
            a += b as u32;
            sum2 += a;
        }
        if a >= BASE {
            a -= BASE;
        }
        sum2 %= BASE;
        return a | (sum2 << 16);
    }
    let mut a = a as u64;
    let mut s2 = sum2 as u64;
    for block in data.chunks(NMAX) {
        let mut ch = block.chunks_exact(16);
        for c in &mut ch {
            let mut sa = 0u64;
            let mut sw = 0u64;
            for (i, &b) in c.iter().enumerate() {
                sa += b as u64;
                sw += (16 - i as u64) * b as u64;
            }
            s2 += 16 * a + sw;
            a += sa;
        }
        for &b in ch.remainder() {
            a += b as u64;
            s2 += a;
        }
        a %= BASE as u64;
        s2 %= BASE as u64;
    }
    (a as u32) | ((s2 as u32) << 16)
}

/// zlib `adler32_combine(adler1, adler2, len2)`.
pub fn adler32_combine(adler1: u32, adler2: u32, len2: u64) -> u32 {
    let rem = (len2 % BASE as u64) as u32;
    let mut sum1 = adler1 & 0xffff;
    let mut sum2 = ((rem as u64 * sum1 as u64) % BASE as u64) as u32;
    sum1 += (adler2 & 0xffff) + BASE - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + BASE - rem;
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum1 >= BASE {
        sum1 -= BASE;
    }
    if sum2 >= (BASE << 1) {
        sum2 -= BASE << 1;
    }
    if sum2 >= BASE {
        sum2 -= BASE;
    }
    sum1 | (sum2 << 16)
}

// ---- CRC-64/XZ (ECMA-182 reflected, poly 0xC96C5795D7870F42) ----

const POLY64: u64 = 0xC96C_5795_D787_0F42;

const fn make_crc64_tables() -> [[u64; 256]; 8] {
    let mut t = [[0u64; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u64;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY64 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

static CRC64_TABLES: [[u64; 256]; 8] = make_crc64_tables();

pub fn crc64(crc: u64, data: &[u8]) -> u64 {
    let t = &CRC64_TABLES;
    let mut c = !crc;
    let mut chunks = data.chunks_exact(8);
    for ch in &mut chunks {
        let v = u64::from_le_bytes(ch.try_into().unwrap()) ^ c;
        c = t[7][(v & 0xff) as usize]
            ^ t[6][((v >> 8) & 0xff) as usize]
            ^ t[5][((v >> 16) & 0xff) as usize]
            ^ t[4][((v >> 24) & 0xff) as usize]
            ^ t[3][((v >> 32) & 0xff) as usize]
            ^ t[2][((v >> 40) & 0xff) as usize]
            ^ t[1][((v >> 48) & 0xff) as usize]
            ^ t[0][(v >> 56) as usize];
    }
    for &b in chunks.remainder() {
        c = t[0][((c ^ b as u64) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_vectors() {
        assert_eq!(crc32(0, b"penguin"), 0x0e5c1a120);
        assert_eq!(crc32(1, b"penguin"), 0x43b6aa94);
        assert_eq!(crc32(0, b"abcdefghijklmnop"), 2486878355);
        assert_eq!(crc32(0, b"spam"), 1138425661);
        assert_eq!(crc32(0, b"123456789"), 0xCBF43926);
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        for n in [64, 65, 100, 127, 128, 1000, 99_999, 100_000] {
            for start in [0u32, 1, 0xdeadbeef] {
                assert_eq!(crc32(start, &big[..n]), crc32_slice16(start, &big[..n]), "n={n}");
            }
        }
        assert_eq!(crc64(0, b"123456789"), 0x995DC9BBDF1939FA);
    }
    #[test]
    fn adler_vectors() {
        assert_eq!(adler32(0, b"penguin"), 0x0bcf02f6);
        assert_eq!(adler32(1, b"penguin"), 0x0bd602f7);
        let foo = b"abcdefghijklmnopabcdefghijklmnop";
        assert_eq!(adler32(1, foo), 3573550353);
        assert_eq!(adler32(1, b"spam"), 72286642);
        let big: Vec<u8> = (0..20_000u32).map(|i| (i * 31 + 7) as u8).collect();
        let mut a = 1u32;
        let mut b = 0u32;
        for &x in &big {
            a = (a + x as u32) % BASE;
            b = (b + a) % BASE;
        }
        assert_eq!(adler32(1, &big), a | (b << 16));
    }
    #[test]
    fn combine() {
        let a = b"hello world, this is a test of crc";
        let b = b"and the second part of the message goes here!";
        let mut ab = a.to_vec();
        ab.extend_from_slice(b);
        assert_eq!(crc32_combine(crc32(0, a), crc32(0, b), b.len() as u64), crc32(0, &ab));
        assert_eq!(adler32_combine(adler32(1, a), adler32(1, b), b.len() as u64), adler32(1, &ab));
        assert_eq!(crc32_combine(12345, crc32(0, a), a.len() as u64), crc32(12345, a));
    }
}
