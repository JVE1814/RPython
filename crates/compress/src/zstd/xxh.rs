//! XXH64 (seed 0 by default), used for the Zstandard content checksum.

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

#[inline]
fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2)).rotate_left(31).wrapping_mul(P1)
}

#[inline]
fn merge(h: u64, v: u64) -> u64 {
    (h ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

#[derive(Clone)]
pub struct Xxh64 {
    v: [u64; 4],
    buf: [u8; 32],
    blen: usize,
    total: u64,
    seed: u64,
}

impl Xxh64 {
    pub fn new(seed: u64) -> Xxh64 {
        Xxh64 {
            v: [seed.wrapping_add(P1).wrapping_add(P2), seed.wrapping_add(P2), seed, seed.wrapping_sub(P1)],
            buf: [0; 32],
            blen: 0,
            total: 0,
            seed,
        }
    }

    fn stripe(v: &mut [u64; 4], s: &[u8]) {
        for i in 0..4 {
            let lane = u64::from_le_bytes(s[i * 8..i * 8 + 8].try_into().unwrap());
            v[i] = round(v[i], lane);
        }
    }

    pub fn update(&mut self, mut d: &[u8]) {
        self.total += d.len() as u64;
        if self.blen > 0 {
            let take = (32 - self.blen).min(d.len());
            self.buf[self.blen..self.blen + take].copy_from_slice(&d[..take]);
            self.blen += take;
            d = &d[take..];
            if self.blen < 32 {
                return;
            }
            let b = self.buf;
            Self::stripe(&mut self.v, &b);
            self.blen = 0;
        }
        while d.len() >= 32 {
            Self::stripe(&mut self.v, &d[..32]);
            d = &d[32..];
        }
        self.buf[..d.len()].copy_from_slice(d);
        self.blen = d.len();
    }

    pub fn digest(&self) -> u64 {
        let v = self.v;
        let mut h = if self.total >= 32 {
            let mut h = v[0].rotate_left(1).wrapping_add(v[1].rotate_left(7)).wrapping_add(v[2].rotate_left(12)).wrapping_add(v[3].rotate_left(18));
            for x in v {
                h = merge(h, x);
            }
            h
        } else {
            self.seed.wrapping_add(P5)
        };
        h = h.wrapping_add(self.total);
        let mut t = &self.buf[..self.blen];
        while t.len() >= 8 {
            let k = round(0, u64::from_le_bytes(t[..8].try_into().unwrap()));
            h = (h ^ k).rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
            t = &t[8..];
        }
        if t.len() >= 4 {
            let k = u32::from_le_bytes(t[..4].try_into().unwrap()) as u64;
            h = (h ^ k.wrapping_mul(P1)).rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
            t = &t[4..];
        }
        for &b in t {
            h = (h ^ (b as u64).wrapping_mul(P5)).rotate_left(11).wrapping_mul(P1);
        }
        h ^= h >> 33;
        h = h.wrapping_mul(P2);
        h ^= h >> 29;
        h = h.wrapping_mul(P3);
        h ^= h >> 32;
        h
    }
}

pub fn xxh64(d: &[u8], seed: u64) -> u64 {
    let mut h = Xxh64::new(seed);
    h.update(d);
    h.digest()
}
