//! Message digests: MD5 (RFC 1321), SHA-1 / SHA-2 (FIPS 180-4), SHA-3 / SHAKE (FIPS 202),
//! BLAKE2b / BLAKE2s (RFC 7693).  All hashers are `Clone` so `digest()` can finalize a copy.

/// Common interface used by the Python bindings.
pub trait Digest: Send {
    fn update(&mut self, data: &[u8]);
    /// Finalize a copy of the state (does not disturb `self`).
    fn digest(&self) -> Vec<u8>;
    fn digest_size(&self) -> usize;
    fn block_size(&self) -> usize;
    fn box_clone(&self) -> Box<dyn Digest>;
}

/// Buffers partial blocks for Merkle-Damgard style hashes.
#[derive(Clone)]
struct BlockBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
    total: u128,
}

impl<const N: usize> BlockBuf<N> {
    fn new() -> Self {
        BlockBuf { buf: [0; N], len: 0, total: 0 }
    }
    #[inline]
    fn input(&mut self, mut data: &[u8], mut f: impl FnMut(&[u8])) {
        self.total += data.len() as u128;
        if self.len > 0 {
            let n = (N - self.len).min(data.len());
            self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
            self.len += n;
            data = &data[n..];
            if self.len < N {
                return;
            }
            let b = self.buf;
            f(&b);
            self.len = 0;
        }
        let full = data.len() / N * N;
        if full > 0 {
            f(&data[..full]);
        }
        let rest = &data[full..];
        self.buf[..rest.len()].copy_from_slice(rest);
        self.len = rest.len();
    }
    /// MD-style padding: 0x80, zeros, length (`len_bytes` bytes, big or little endian).
    fn pad(&self, len_bytes: usize, big_endian: bool) -> Vec<u8> {
        let bits = self.total.wrapping_mul(8);
        let mut tail = self.buf[..self.len].to_vec();
        tail.push(0x80);
        while (tail.len() + len_bytes) % N != 0 {
            tail.push(0);
        }
        let lb = if big_endian { bits.to_be_bytes() } else { bits.to_le_bytes() };
        if big_endian {
            tail.extend_from_slice(&lb[16 - len_bytes..]);
        } else {
            tail.extend_from_slice(&lb[..len_bytes]);
        }
        tail
    }
}

// ------------------------------------------------------------------ MD5

#[derive(Clone)]
pub struct Md5 {
    s: [u32; 4],
    b: BlockBuf<64>,
}

static MD5_K: [u32; 64] = {
    // floor(abs(sin(i+1)) * 2^32)
    [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ]
};
static MD5_R: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16,
    23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

fn md5_blocks(s: &mut [u32; 4], data: &[u8]) {
    for blk in data.chunks_exact(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes(blk[4 * i..4 * i + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (s[0], s[1], s[2], s[3]);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = d;
            d = c;
            c = b;
            b = b.wrapping_add(a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(m[g]).rotate_left(MD5_R[i]));
            a = t;
        }
        s[0] = s[0].wrapping_add(a);
        s[1] = s[1].wrapping_add(b);
        s[2] = s[2].wrapping_add(c);
        s[3] = s[3].wrapping_add(d);
    }
}

impl Md5 {
    pub fn new() -> Md5 {
        Md5 { s: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], b: BlockBuf::new() }
    }
}

impl Digest for Md5 {
    fn update(&mut self, data: &[u8]) {
        let s = &mut self.s;
        self.b.input(data, |d| md5_blocks(s, d));
    }
    fn digest(&self) -> Vec<u8> {
        let mut s = self.s;
        md5_blocks(&mut s, &self.b.pad(8, false));
        s.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
    fn digest_size(&self) -> usize {
        16
    }
    fn block_size(&self) -> usize {
        64
    }
    fn box_clone(&self) -> Box<dyn Digest> {
        Box::new(self.clone())
    }
}

// ------------------------------------------------------------------ SHA-1

#[derive(Clone)]
pub struct Sha1 {
    s: [u32; 5],
    b: BlockBuf<64>,
}

fn sha1_blocks(s: &mut [u32; 5], data: &[u8]) {
    for blk in data.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(blk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (s[0], s[1], s[2], s[3], s[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A827999),
                1 => (b ^ c ^ d, 0x6ED9EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        s[0] = s[0].wrapping_add(a);
        s[1] = s[1].wrapping_add(b);
        s[2] = s[2].wrapping_add(c);
        s[3] = s[3].wrapping_add(d);
        s[4] = s[4].wrapping_add(e);
    }
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 { s: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0], b: BlockBuf::new() }
    }
}

impl Digest for Sha1 {
    fn update(&mut self, data: &[u8]) {
        let s = &mut self.s;
        self.b.input(data, |d| sha1_blocks(s, d));
    }
    fn digest(&self) -> Vec<u8> {
        let mut s = self.s;
        sha1_blocks(&mut s, &self.b.pad(8, true));
        s.iter().flat_map(|w| w.to_be_bytes()).collect()
    }
    fn digest_size(&self) -> usize {
        20
    }
    fn block_size(&self) -> usize {
        64
    }
    fn box_clone(&self) -> Box<dyn Digest> {
        Box::new(self.clone())
    }
}

// ------------------------------------------------------------------ SHA-256 / SHA-224

static K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_blocks_soft(s: &mut [u32; 8], data: &[u8]) {
    for blk in data.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(blk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut h = *s;
        for i in 0..64 {
            let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
            let ch = (h[4] & h[5]) ^ (!h[4] & h[6]);
            let t1 = h[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
            let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
            let t2 = s0.wrapping_add(maj);
            h[7] = h[6];
            h[6] = h[5];
            h[5] = h[4];
            h[4] = h[3].wrapping_add(t1);
            h[3] = h[2];
            h[2] = h[1];
            h[1] = h[0];
            h[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            s[i] = s[i].wrapping_add(h[i]);
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod shani {
    use std::arch::x86_64::*;

    /// SHA-256 compression using the x86 SHA extensions.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn sha256_blocks(state: &mut [u32; 8], data: &[u8]) {
        unsafe {
            let mask = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0bu64 as i64, 0x0405_0607_0001_0203u64 as i64);
            let mut tmp = _mm_loadu_si128(state.as_ptr() as *const __m128i); // a b c d (dcba)
            let mut s1 = _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i); // e f g h
            tmp = _mm_shuffle_epi32(tmp, 0xB1); // CDAB
            s1 = _mm_shuffle_epi32(s1, 0x1B); // EFGH
            let mut s0 = _mm_alignr_epi8(tmp, s1, 8); // ABEF
            s1 = _mm_blend_epi16(s1, tmp, 0xF0); // CDGH
            let k = super::K256.as_ptr() as *const __m128i;
            for blk in data.chunks_exact(64) {
                let abef = s0;
                let cdgh = s1;
                let p = blk.as_ptr() as *const __m128i;
                let mut m = [
                    _mm_shuffle_epi8(_mm_loadu_si128(p), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(p.add(1)), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(p.add(2)), mask),
                    _mm_shuffle_epi8(_mm_loadu_si128(p.add(3)), mask),
                ];
                for i in 0..16 {
                    if i >= 4 {
                        // m[i%4] = schedule(m[i-4], m[i-3], m[i-2], m[i-1])
                        let a = m[i % 4];
                        let b = m[(i + 1) % 4];
                        let c = m[(i + 2) % 4];
                        let d = m[(i + 3) % 4];
                        let t = _mm_sha256msg1_epu32(a, b);
                        let t = _mm_add_epi32(t, _mm_alignr_epi8(d, c, 4));
                        m[i % 4] = _mm_sha256msg2_epu32(t, d);
                    }
                    let msg = _mm_add_epi32(m[i % 4], _mm_loadu_si128(k.add(i)));
                    s1 = _mm_sha256rnds2_epu32(s1, s0, msg);
                    let msg2 = _mm_shuffle_epi32(msg, 0x0E);
                    s0 = _mm_sha256rnds2_epu32(s0, s1, msg2);
                }
                s0 = _mm_add_epi32(s0, abef);
                s1 = _mm_add_epi32(s1, cdgh);
            }
            tmp = _mm_shuffle_epi32(s0, 0x1B); // FEBA
            s1 = _mm_shuffle_epi32(s1, 0xB1); // DCHG
            s0 = _mm_blend_epi16(tmp, s1, 0xF0); // DCBA
            s1 = _mm_alignr_epi8(s1, tmp, 8); // ABEF -> HGFE
            _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, s0);
            _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, s1);
        }
    }
}

fn have_shani() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *F.get_or_init(|| {
            std::arch::is_x86_feature_detected!("sha")
                && std::arch::is_x86_feature_detected!("sse4.1")
                && std::arch::is_x86_feature_detected!("ssse3")
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

pub fn sha256_blocks(s: &mut [u32; 8], data: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    {
        if have_shani() {
            return unsafe { shani::sha256_blocks(s, data) };
        }
    }
    sha256_blocks_soft(s, data)
}

#[derive(Clone)]
pub struct Sha256 {
    s: [u32; 8],
    b: BlockBuf<64>,
    out: usize,
}

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256 {
            s: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            b: BlockBuf::new(),
            out: 32,
        }
    }
    pub fn new224() -> Sha256 {
        Sha256 {
            s: [0xc1059ed8, 0x367cd507, 0x3070dd17, 0xf70e5939, 0xffc00b31, 0x68581511, 0x64f98fa7, 0xbefa4fa4],
            b: BlockBuf::new(),
            out: 28,
        }
    }
}

impl Digest for Sha256 {
    fn update(&mut self, data: &[u8]) {
        let s = &mut self.s;
        self.b.input(data, |d| sha256_blocks(s, d));
    }
    fn digest(&self) -> Vec<u8> {
        let mut s = self.s;
        sha256_blocks(&mut s, &self.b.pad(8, true));
        let v: Vec<u8> = s.iter().flat_map(|w| w.to_be_bytes()).collect();
        v[..self.out].to_vec()
    }
    fn digest_size(&self) -> usize {
        self.out
    }
    fn block_size(&self) -> usize {
        64
    }
    fn box_clone(&self) -> Box<dyn Digest> {
        Box::new(self.clone())
    }
}

// ------------------------------------------------------------------ SHA-512 family

static K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b,
    0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2, 0x72be5d74f27b896f, 0x80deb1fe3b1696b1,
    0x9bdc06a725c71235, 0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65, 0x2de92c6f592b0275,
    0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5, 0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df, 0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b, 0xa2bfe8a14cf10364, 0xa81a664bbc423001,
    0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8, 0x19a4c116b8d2d0c8,
    0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec, 0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915,
    0xc67178f2e372532b, 0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6,
    0x113f9804bef90dae, 0x1b710b35131c471b, 0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c, 0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_blocks(s: &mut [u64; 8], data: &[u8]) {
    for blk in data.chunks_exact(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            w[i] = u64::from_be_bytes(blk[8 * i..8 * i + 8].try_into().unwrap());
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut h = *s;
        for i in 0..80 {
            let s1 = h[4].rotate_right(14) ^ h[4].rotate_right(18) ^ h[4].rotate_right(41);
            let ch = (h[4] & h[5]) ^ (!h[4] & h[6]);
            let t1 = h[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = h[0].rotate_right(28) ^ h[0].rotate_right(34) ^ h[0].rotate_right(39);
            let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
            let t2 = s0.wrapping_add(maj);
            h[7] = h[6];
            h[6] = h[5];
            h[5] = h[4];
            h[4] = h[3].wrapping_add(t1);
            h[3] = h[2];
            h[2] = h[1];
            h[1] = h[0];
            h[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            s[i] = s[i].wrapping_add(h[i]);
        }
    }
}

#[derive(Clone)]
pub struct Sha512 {
    s: [u64; 8],
    b: BlockBuf<128>,
    out: usize,
}

impl Sha512 {
    pub fn with_iv(iv: [u64; 8], out: usize) -> Sha512 {
        Sha512 { s: iv, b: BlockBuf::new(), out }
    }
    pub fn new() -> Sha512 {
        Self::with_iv(
            [
                0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
            64,
        )
    }
    pub fn new384() -> Sha512 {
        Self::with_iv(
            [
                0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939, 0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7,
                0x47b5481dbefa4fa4,
            ],
            48,
        )
    }
    /// SHA-512/t initial values (FIPS 180-4 5.3.6).
    pub fn new_t(t: usize) -> Sha512 {
        let mut iv = Sha512::new().s;
        for v in iv.iter_mut() {
            *v ^= 0xa5a5a5a5a5a5a5a5;
        }
        let mut h = Sha512::with_iv(iv, 64);
        h.update(format!("SHA-512/{}", t).as_bytes());
        let d = h.digest();
        let mut s = [0u64; 8];
        for i in 0..8 {
            s[i] = u64::from_be_bytes(d[8 * i..8 * i + 8].try_into().unwrap());
        }
        Sha512::with_iv(s, t / 8)
    }
}

impl Digest for Sha512 {
    fn update(&mut self, data: &[u8]) {
        let s = &mut self.s;
        self.b.input(data, |d| sha512_blocks(s, d));
    }
    fn digest(&self) -> Vec<u8> {
        let mut s = self.s;
        sha512_blocks(&mut s, &self.b.pad(16, true));
        let v: Vec<u8> = s.iter().flat_map(|w| w.to_be_bytes()).collect();
        v[..self.out].to_vec()
    }
    fn digest_size(&self) -> usize {
        self.out
    }
    fn block_size(&self) -> usize {
        128
    }
    fn box_clone(&self) -> Box<dyn Digest> {
        Box::new(self.clone())
    }
}

// ------------------------------------------------------------------ SHA-3 / Keccak

static KECCAK_RC: [u64; 24] = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808a, 0x8000000080008000, 0x000000000000808b, 0x0000000080000001, 0x8000000080008081,
    0x8000000000008009, 0x000000000000008a, 0x0000000000000088, 0x0000000080008009, 0x000000008000000a, 0x000000008000808b, 0x800000000000008b,
    0x8000000000008089, 0x8000000000008003, 0x8000000000008002, 0x8000000000000080, 0x000000000000800a, 0x800000008000000a, 0x8000000080008081,
    0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
];
static KECCAK_ROT: [u32; 25] = [0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14];

pub fn keccak_f(a: &mut [u64; 25]) {
    for &rc in &KECCAK_RC {
        // theta
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                a[x + 5 * y] ^= d;
            }
        }
        // rho + pi
        let mut b = [0u64; 25];
        for x in 0..5 {
            for y in 0..5 {
                let i = x + 5 * y;
                b[y + 5 * ((2 * x + 3 * y) % 5)] = a[i].rotate_left(KECCAK_ROT[i]);
            }
        }
        // chi
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] = b[x + 5 * y] ^ (!b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y]);
            }
        }
        // iota
        a[0] ^= rc;
    }
}

#[derive(Clone)]
pub struct Sha3 {
    st: [u64; 25],
    rate: usize,
    buf: Vec<u8>,
    out: usize, // 0 for SHAKE
    pad: u8,
}

impl Sha3 {
    pub fn new(bits: usize) -> Sha3 {
        Sha3 { st: [0; 25], rate: 200 - 2 * bits / 8, buf: Vec::new(), out: bits / 8, pad: 0x06 }
    }
    pub fn shake(bits: usize) -> Sha3 {
        Sha3 { st: [0; 25], rate: 200 - 2 * bits / 8, buf: Vec::new(), out: 0, pad: 0x1f }
    }
    pub fn rate(&self) -> usize {
        self.rate
    }
    pub fn is_shake(&self) -> bool {
        self.out == 0
    }
    fn absorb_block(st: &mut [u64; 25], blk: &[u8]) {
        for (i, ch) in blk.chunks_exact(8).enumerate() {
            st[i] ^= u64::from_le_bytes(ch.try_into().unwrap());
        }
        keccak_f(st);
    }
    /// Squeeze `n` bytes from a finalized copy.
    pub fn squeeze(&self, n: usize) -> Vec<u8> {
        let mut st = self.st;
        let mut blk = self.buf.clone();
        blk.resize(self.rate, 0);
        blk[self.buf.len()] ^= self.pad;
        blk[self.rate - 1] ^= 0x80;
        Sha3::absorb_block(&mut st, &blk);
        let mut out = Vec::with_capacity(n);
        loop {
            for i in 0..self.rate / 8 {
                for b in st[i].to_le_bytes() {
                    if out.len() == n {
                        return out;
                    }
                    out.push(b);
                }
            }
            if out.len() == n {
                return out;
            }
            keccak_f(&mut st);
        }
    }
}

impl Digest for Sha3 {
    fn update(&mut self, mut data: &[u8]) {
        if !self.buf.is_empty() {
            let n = (self.rate - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.buf.len() < self.rate {
                return;
            }
            let b = std::mem::take(&mut self.buf);
            Sha3::absorb_block(&mut self.st, &b);
        }
        let mut ch = data.chunks_exact(self.rate);
        for blk in &mut ch {
            Sha3::absorb_block(&mut self.st, blk);
        }
        self.buf.extend_from_slice(ch.remainder());
    }
    fn digest(&self) -> Vec<u8> {
        self.squeeze(self.out)
    }
    fn digest_size(&self) -> usize {
        self.out
    }
    fn block_size(&self) -> usize {
        self.rate
    }
    fn box_clone(&self) -> Box<dyn Digest> {
        Box::new(self.clone())
    }
}

// ------------------------------------------------------------------ BLAKE2

static B2B_IV: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];
static B2S_IV: [u32; 8] = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19];
static SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

/// BLAKE2 parameters (shared by the b and s variants; sizes are checked by the caller).
#[derive(Clone, Default)]
pub struct Blake2Params {
    pub digest_size: usize,
    pub key: Vec<u8>,
    pub salt: Vec<u8>,
    pub person: Vec<u8>,
    pub fanout: u8,
    pub depth: u8,
    pub leaf_size: u32,
    pub node_offset: u64,
    pub node_depth: u8,
    pub inner_size: u8,
    pub last_node: bool,
}

macro_rules! blake2_impl {
    ($name:ident, $w:ty, $iv:ident, $bb:expr, $rounds:expr, $r1:expr, $r2:expr, $r3:expr, $r4:expr, $saltlen:expr) => {
        #[derive(Clone)]
        pub struct $name {
            h: [$w; 8],
            t: u128,
            buf: [u8; $bb],
            len: usize,
            out: usize,
            last_node: bool,
        }

        impl $name {
            pub const BLOCK: usize = $bb;
            pub const SALT: usize = $saltlen;
            pub fn new(p: &Blake2Params) -> $name {
                // Parameter block.
                let mut pb = [0u8; 64];
                pb[0] = p.digest_size as u8;
                pb[1] = p.key.len() as u8;
                pb[2] = p.fanout;
                pb[3] = p.depth;
                pb[4..8].copy_from_slice(&p.leaf_size.to_le_bytes());
                let wb = std::mem::size_of::<$w>();
                if wb == 8 {
                    pb[8..16].copy_from_slice(&p.node_offset.to_le_bytes());
                    pb[16] = p.node_depth;
                    pb[17] = p.inner_size;
                    pb[32..32 + p.salt.len()].copy_from_slice(&p.salt);
                    pb[48..48 + p.person.len()].copy_from_slice(&p.person);
                } else {
                    pb[8..14].copy_from_slice(&p.node_offset.to_le_bytes()[..6]);
                    pb[14] = p.node_depth;
                    pb[15] = p.inner_size;
                    pb[16..16 + p.salt.len()].copy_from_slice(&p.salt);
                    pb[24..24 + p.person.len()].copy_from_slice(&p.person);
                }
                let mut h = $iv;
                for i in 0..8 {
                    let mut v = [0u8; 8];
                    v[..wb].copy_from_slice(&pb[i * wb..(i + 1) * wb]);
                    h[i] ^= u64::from_le_bytes(v) as $w;
                }
                let mut s = $name { h, t: 0, buf: [0; $bb], len: 0, out: p.digest_size, last_node: p.last_node };
                if !p.key.is_empty() {
                    let mut k = [0u8; $bb];
                    k[..p.key.len()].copy_from_slice(&p.key);
                    s.buf = k;
                    s.len = $bb;
                }
                s
            }

            fn compress(h: &mut [$w; 8], blk: &[u8], t: u128, f0: bool, f1: bool) {
                let wb = std::mem::size_of::<$w>();
                let mut m = [0 as $w; 16];
                for i in 0..16 {
                    let mut v = [0u8; 8];
                    v[..wb].copy_from_slice(&blk[i * wb..(i + 1) * wb]);
                    m[i] = u64::from_le_bytes(v) as $w;
                }
                let mut v = [0 as $w; 16];
                v[..8].copy_from_slice(h);
                v[8..].copy_from_slice(&$iv);
                v[12] ^= t as $w;
                v[13] ^= (t >> (wb * 8)) as $w;
                if f0 {
                    v[14] = !v[14];
                }
                if f1 {
                    v[15] = !v[15];
                }
                macro_rules! g {
                    ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $y:expr) => {
                        v[$a] = v[$a].wrapping_add(v[$b]).wrapping_add($x);
                        v[$d] = (v[$d] ^ v[$a]).rotate_right($r1);
                        v[$c] = v[$c].wrapping_add(v[$d]);
                        v[$b] = (v[$b] ^ v[$c]).rotate_right($r2);
                        v[$a] = v[$a].wrapping_add(v[$b]).wrapping_add($y);
                        v[$d] = (v[$d] ^ v[$a]).rotate_right($r3);
                        v[$c] = v[$c].wrapping_add(v[$d]);
                        v[$b] = (v[$b] ^ v[$c]).rotate_right($r4);
                    };
                }
                for r in 0..$rounds {
                    let s = &SIGMA[r % 10];
                    g!(0, 4, 8, 12, m[s[0]], m[s[1]]);
                    g!(1, 5, 9, 13, m[s[2]], m[s[3]]);
                    g!(2, 6, 10, 14, m[s[4]], m[s[5]]);
                    g!(3, 7, 11, 15, m[s[6]], m[s[7]]);
                    g!(0, 5, 10, 15, m[s[8]], m[s[9]]);
                    g!(1, 6, 11, 12, m[s[10]], m[s[11]]);
                    g!(2, 7, 8, 13, m[s[12]], m[s[13]]);
                    g!(3, 4, 9, 14, m[s[14]], m[s[15]]);
                }
                for i in 0..8 {
                    h[i] ^= v[i] ^ v[i + 8];
                }
            }
        }

        impl Digest for $name {
            fn update(&mut self, mut data: &[u8]) {
                while !data.is_empty() {
                    if self.len == $bb {
                        // Only compress a full buffer once we know more data follows.
                        self.t += $bb as u128;
                        let b = self.buf;
                        Self::compress(&mut self.h, &b, self.t, false, false);
                        self.len = 0;
                    }
                    if self.len == 0 {
                        while data.len() > $bb {
                            self.t += $bb as u128;
                            Self::compress(&mut self.h, &data[..$bb], self.t, false, false);
                            data = &data[$bb..];
                        }
                    }
                    let n = ($bb - self.len).min(data.len());
                    self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
                    self.len += n;
                    data = &data[n..];
                }
            }
            fn digest(&self) -> Vec<u8> {
                let mut h = self.h;
                let mut b = self.buf;
                for x in b[self.len..].iter_mut() {
                    *x = 0;
                }
                let t = self.t + self.len as u128;
                Self::compress(&mut h, &b, t, true, self.last_node);
                let wb = std::mem::size_of::<$w>();
                let mut out = Vec::with_capacity(8 * wb);
                for w in h.iter() {
                    out.extend_from_slice(&(*w as u64).to_le_bytes()[..wb]);
                }
                out.truncate(self.out);
                out
            }
            fn digest_size(&self) -> usize {
                self.out
            }
            fn block_size(&self) -> usize {
                $bb
            }
            fn box_clone(&self) -> Box<dyn Digest> {
                Box::new(self.clone())
            }
        }
    };
}

blake2_impl!(Blake2b, u64, B2B_IV, 128, 12, 32, 24, 16, 63, 16);
blake2_impl!(Blake2s, u32, B2S_IV, 64, 10, 16, 12, 8, 7, 8);

/// Construct a hasher by (lower-case, canonical) name.
pub fn by_name(name: &str) -> Option<Box<dyn Digest>> {
    Some(match name {
        "md5" => Box::new(Md5::new()),
        "sha1" => Box::new(Sha1::new()),
        "sha224" => Box::new(Sha256::new224()),
        "sha256" => Box::new(Sha256::new()),
        "sha384" => Box::new(Sha512::new384()),
        "sha512" => Box::new(Sha512::new()),
        "sha512_224" => Box::new(Sha512::new_t(224)),
        "sha512_256" => Box::new(Sha512::new_t(256)),
        "sha3_224" => Box::new(Sha3::new(224)),
        "sha3_256" => Box::new(Sha3::new(256)),
        "sha3_384" => Box::new(Sha3::new(384)),
        "sha3_512" => Box::new(Sha3::new(512)),
        "blake2b" => Box::new(Blake2b::new(&Blake2Params { digest_size: 64, fanout: 1, depth: 1, ..Default::default() })),
        "blake2s" => Box::new(Blake2s::new(&Blake2Params { digest_size: 32, fanout: 1, depth: 1, ..Default::default() })),
        _ => return None,
    })
}

pub fn hex(d: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(d.len() * 2);
    for &b in d {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// HMAC (RFC 2104) over any digest constructor.
pub fn hmac(make: &dyn Fn() -> Box<dyn Digest>, key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut inner = make();
    let bs = inner.block_size();
    let mut k = if key.len() > bs {
        let mut h = make();
        h.update(key);
        h.digest()
    } else {
        key.to_vec()
    };
    k.resize(bs, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    inner.update(&ipad);
    inner.update(msg);
    let ih = inner.digest();
    let mut outer = make();
    outer.update(&opad);
    outer.update(&ih);
    outer.digest()
}

/// PBKDF2-HMAC (RFC 8018).
pub fn pbkdf2_hmac(make: &dyn Fn() -> Box<dyn Digest>, password: &[u8], salt: &[u8], iterations: u64, dklen: usize) -> Vec<u8> {
    let probe = make();
    let bs = probe.block_size();
    let hlen = probe.digest_size();
    let mut k = if password.len() > bs {
        let mut h = make();
        h.update(password);
        h.digest()
    } else {
        password.to_vec()
    };
    k.resize(bs, 0);
    let mut ictx = make();
    ictx.update(&k.iter().map(|b| b ^ 0x36).collect::<Vec<u8>>());
    let mut octx = make();
    octx.update(&k.iter().map(|b| b ^ 0x5c).collect::<Vec<u8>>());
    let prf = |data: &[u8]| -> Vec<u8> {
        let mut i = ictx.box_clone();
        i.update(data);
        let ih = i.digest();
        let mut o = octx.box_clone();
        o.update(&ih);
        o.digest()
    };
    let mut out = Vec::with_capacity(dklen);
    let mut block = 1u32;
    while out.len() < dklen {
        let mut s = salt.to_vec();
        s.extend_from_slice(&block.to_be_bytes());
        let mut u = prf(&s);
        let mut t = u.clone();
        for _ in 1..iterations {
            u = prf(&u);
            for (a, b) in t.iter_mut().zip(&u) {
                *a ^= b;
            }
        }
        let n = (dklen - out.len()).min(hlen);
        out.extend_from_slice(&t[..n]);
        block += 1;
    }
    out
}

/// scrypt (RFC 7914) with HMAC-SHA256.
pub fn scrypt(password: &[u8], salt: &[u8], n: u64, r: usize, p: usize, dklen: usize) -> Vec<u8> {
    let mk = || -> Box<dyn Digest> { Box::new(Sha256::new()) };
    let blen = 128 * r;
    let mut b = pbkdf2_hmac(&mk, password, salt, 1, p * blen);
    let mut v = vec![0u32; (n as usize) * blen / 4];
    for chunk in b.chunks_mut(blen) {
        let mut x: Vec<u32> = chunk.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        let words = blen / 4;
        for i in 0..n as usize {
            v[i * words..(i + 1) * words].copy_from_slice(&x);
            blockmix(&mut x, r);
        }
        for _ in 0..n as usize {
            let j = (x[(2 * r - 1) * 16] as u64 % n) as usize;
            for k in 0..words {
                x[k] ^= v[j * words + k];
            }
            blockmix(&mut x, r);
        }
        for (i, w) in x.iter().enumerate() {
            chunk[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    pbkdf2_hmac(&mk, password, &b, 1, dklen)
}

fn salsa8(b: &mut [u32; 16]) {
    let mut x = *b;
    for _ in 0..4 {
        macro_rules! qr {
            ($a:expr, $b:expr, $c:expr, $d:expr) => {
                x[$b] ^= x[$a].wrapping_add(x[$d]).rotate_left(7);
                x[$c] ^= x[$b].wrapping_add(x[$a]).rotate_left(9);
                x[$d] ^= x[$c].wrapping_add(x[$b]).rotate_left(13);
                x[$a] ^= x[$d].wrapping_add(x[$c]).rotate_left(18);
            };
        }
        qr!(0, 4, 8, 12);
        qr!(5, 9, 13, 1);
        qr!(10, 14, 2, 6);
        qr!(15, 3, 7, 11);
        qr!(0, 1, 2, 3);
        qr!(5, 6, 7, 4);
        qr!(10, 11, 8, 9);
        qr!(15, 12, 13, 14);
    }
    for i in 0..16 {
        b[i] = b[i].wrapping_add(x[i]);
    }
}

fn blockmix(b: &mut Vec<u32>, r: usize) {
    let mut x = [0u32; 16];
    x.copy_from_slice(&b[(2 * r - 1) * 16..2 * r * 16]);
    let mut y = vec![0u32; b.len()];
    for i in 0..2 * r {
        for k in 0..16 {
            x[k] ^= b[i * 16 + k];
        }
        salsa8(&mut x);
        // Even blocks go to the first half, odd blocks to the second half.
        let dst = if i % 2 == 0 { (i / 2) * 16 } else { (r + i / 2) * 16 };
        y[dst..dst + 16].copy_from_slice(&x);
    }
    *b = y;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(mut d: Box<dyn Digest>, m: &[u8]) -> String {
        d.update(m);
        hex(&d.digest())
    }
    #[test]
    fn vectors() {
        assert_eq!(h(by_name("md5").unwrap(), b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(h(by_name("md5").unwrap(), b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h(by_name("sha1").unwrap(), b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h(by_name("sha256").unwrap(), b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(h(by_name("sha224").unwrap(), b"abc"), "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7");
        assert_eq!(
            h(by_name("sha512").unwrap(), b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            h(by_name("sha384").unwrap(), b"abc"),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(h(by_name("sha512_256").unwrap(), b"abc"), "53048e2681941ef99b2e29b76b4c7dabe4c2d0c634fc6d46e0e2f13107e7af23");
        assert_eq!(h(by_name("sha512_224").unwrap(), b"abc"), "4634270f707b6a54daae7530460842e20e37ed265ceee9a43e8924aa");
        assert_eq!(h(by_name("sha3_256").unwrap(), b""), "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a");
        assert_eq!(h(by_name("sha3_224").unwrap(), b"abc"), "e642824c3f8cf24ad09234ee7d3c766fc9a3a5168d0c94ad73b46fdf");
        let s = Sha3::shake(128);
        assert_eq!(hex(&s.squeeze(32)), "7f9c2ba4e88f827d616045507605853ed73b8093f6efbc88eb1a6eacfa66ef26");
        assert_eq!(
            h(by_name("blake2b").unwrap(), b"abc"),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
        assert_eq!(h(by_name("blake2s").unwrap(), b"abc"), "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982");
        // Long input exercising buffering.
        let big: Vec<u8> = (0..1_000_000u32).map(|i| b"a"[0] + (i % 1) as u8).collect();
        assert_eq!(h(by_name("sha1").unwrap(), &big), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
        assert_eq!(h(by_name("sha256").unwrap(), &big), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        let mut d = by_name("sha256").unwrap();
        for ch in big.chunks(777) {
            d.update(ch);
        }
        assert_eq!(hex(&d.digest()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        assert_eq!(h(by_name("md5").unwrap(), &big), "7707d6ae4e027c70eea2a935c2296f21");
    }
    #[test]
    fn hmac_pbkdf2() {
        let mk = || -> Box<dyn Digest> { Box::new(Sha256::new()) };
        assert_eq!(
            hex(&hmac(&mk, b"key", b"The quick brown fox jumps over the lazy dog")),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        let mk1 = || -> Box<dyn Digest> { Box::new(Sha1::new()) };
        assert_eq!(hex(&pbkdf2_hmac(&mk1, b"password", b"salt", 2, 20)), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        assert_eq!(
            hex(&scrypt(b"password", b"NaCl", 1024, 8, 16, 64)),
            "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640"
        );
    }
}
