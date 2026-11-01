//! Substring search over PEP 393 code-unit slices.
//!
//! Strategy (written from the textbook algorithms):
//! * needle of length 1: a plain scan (word-at-a-time for UCS1);
//! * otherwise a Horspool-style scan with a 64-bit "bloom" mask of the
//!   needle's code units; when the work done verifying candidates grows
//!   beyond a linear budget the search switches to Crochemore–Perrin
//!   two-way matching, so the worst case stays O(n + m).
//!
//! Every routine works on a *view* that is either the slice itself or the
//! slice reversed (`REV`), which gives `rfind`/`rsplit`/`rpartition` the same
//! guarantees as the forward search.

use crate::types::str::StrData;

pub trait Unit: Copy + Eq + Ord + Into<u32> + Default + 'static {
    fn from_u32(c: u32) -> Self;
}
impl Unit for u8 {
    #[inline(always)]
    fn from_u32(c: u32) -> Self {
        c as u8
    }
}
impl Unit for u16 {
    #[inline(always)]
    fn from_u32(c: u32) -> Self {
        c as u16
    }
}
impl Unit for u32 {
    #[inline(always)]
    fn from_u32(c: u32) -> Self {
        c
    }
}

#[derive(Clone, Copy)]
struct View<'a, T, const REV: bool> {
    s: &'a [T],
}
impl<'a, T: Unit, const REV: bool> View<'a, T, REV> {
    #[inline(always)]
    fn len(&self) -> usize {
        self.s.len()
    }
    #[inline(always)]
    fn at(&self, i: usize) -> T {
        if REV {
            unsafe { *self.s.get_unchecked(self.s.len() - 1 - i) }
        } else {
            unsafe { *self.s.get_unchecked(i) }
        }
    }
}

#[inline(always)]
fn bloom_bit<T: Unit>(c: T) -> u64 {
    1u64 << (c.into() & 63)
}

/// Single code unit search (forward).
#[inline]
pub fn find_unit<T: Unit>(h: &[T], c: T) -> Option<usize> {
    if std::mem::size_of::<T>() == 1 {
        let hb = unsafe { std::slice::from_raw_parts(h.as_ptr() as *const u8, h.len()) };
        return memchr_u8(hb, c.into() as u8);
    }
    h.iter().position(|&x| x == c)
}

#[inline]
pub fn rfind_unit<T: Unit>(h: &[T], c: T) -> Option<usize> {
    h.iter().rposition(|&x| x == c)
}

#[inline]
pub fn count_unit<T: Unit>(h: &[T], c: T, max: usize) -> usize {
    if max == usize::MAX {
        // branch-free: vectorizes
        return h.iter().fold(0usize, |n, &x| n + (x == c) as usize);
    }
    let mut n = 0;
    for &x in h {
        if x == c {
            n += 1;
            if n >= max {
                break;
            }
        }
    }
    n
}

fn memchr_u8(h: &[u8], c: u8) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let n = h.len();
    let mut i = 0;
    if n >= 16 {
        let pat = LO.wrapping_mul(c as u64);
        while i + 8 <= n {
            let w = u64::from_le_bytes(h[i..i + 8].try_into().unwrap()) ^ pat;
            if (w.wrapping_sub(LO) & !w & HI) != 0 {
                break;
            }
            i += 8;
        }
    }
    while i < n {
        if h[i] == c {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Preprocessed needle for repeated searches in one direction.
pub struct Finder<'n, T: Unit, const REV: bool> {
    needle: View<'n, T, REV>,
    mask: u64,
    skip: usize,
    tw: Option<TwoWay>,
}

#[derive(Clone, Copy)]
struct TwoWay {
    crit: usize,
    period: usize,
    periodic: bool,
}

impl<'n, T: Unit, const REV: bool> Finder<'n, T, REV> {
    pub fn new(needle: &'n [T]) -> Self {
        let nv = View::<T, REV> { s: needle };
        let m = nv.len();
        let mut mask = 0u64;
        let mut skip = m.saturating_sub(1);
        if m > 0 {
            let last = nv.at(m - 1);
            for i in 0..m - 1 {
                let c = nv.at(i);
                mask |= bloom_bit(c);
                if c == last {
                    skip = m - 2 - i;
                }
            }
            mask |= bloom_bit(last);
        }
        Finder { needle: nv, mask, skip, tw: None }
    }

    /// First match at view position >= from (view coordinates of `hay`).
    pub fn find(&mut self, hay: &[T], from: usize) -> Option<usize> {
        let h = View::<T, REV> { s: hay };
        let n = h.len();
        let m = self.needle.len();
        if m == 0 {
            return if from <= n { Some(from) } else { None };
        }
        if from > n || n - from < m {
            return None;
        }
        if m == 1 {
            let c = self.needle.at(0);
            return if REV {
                let sub = &hay[..n - from];
                rfind_unit(sub, c).map(|p| n - 1 - p)
            } else {
                find_unit(&hay[from..], c).map(|p| p + from)
            };
        }
        if let Some(tw) = self.tw {
            return two_way(h, self.needle, tw, from);
        }
        // Horspool with bloom filter; bail out to two-way if verification
        // work exceeds a linear budget.
        let mlast = m - 1;
        let last = self.needle.at(mlast);
        let w = n - m;
        let mut i = from;
        let mut work: usize = 0;
        let budget_base = 4 * m + 64;
        while i <= w {
            let c = h.at(i + mlast);
            if c == last {
                let mut j = 0;
                while j < mlast && h.at(i + j) == self.needle.at(j) {
                    j += 1;
                }
                if j == mlast {
                    return Some(i);
                }
                work += j + 1;
                if work > budget_base + 2 * (i - from) {
                    let tw = two_way_prepare(self.needle);
                    self.tw = Some(tw);
                    return two_way(h, self.needle, tw, i);
                }
                if i + m < n && self.mask & bloom_bit(h.at(i + m)) == 0 {
                    i += m + 1;
                } else {
                    i += self.skip + 1;
                }
            } else if i + m < n && self.mask & bloom_bit(h.at(i + m)) == 0 {
                i += m + 1;
            } else {
                i += 1;
            }
        }
        None
    }
}

/// Maximal suffix of `x` under `<` (or reversed order when `rev_order`).
/// Returns (start, period).
fn maximal_suffix<T: Unit, const REV: bool>(x: View<T, REV>, rev_order: bool) -> (usize, usize) {
    let n = x.len();
    let mut left = 0usize;
    let mut right = 1usize;
    let mut offset = 0usize;
    let mut period = 1usize;
    while right + offset < n {
        let a = x.at(right + offset);
        let b = x.at(left + offset);
        let less = if rev_order { a > b } else { a < b };
        if less {
            right += offset + 1;
            offset = 0;
            period = right - left;
        } else if a == b {
            if offset + 1 == period {
                right += offset + 1;
                offset = 0;
            } else {
                offset += 1;
            }
        } else {
            left = right;
            right += 1;
            offset = 0;
            period = 1;
        }
    }
    (left, period)
}

fn two_way_prepare<T: Unit, const REV: bool>(nd: View<T, REV>) -> TwoWay {
    let m = nd.len();
    let (l1, p1) = maximal_suffix(nd, false);
    let (l2, p2) = maximal_suffix(nd, true);
    let (crit, period) = if l1 > l2 { (l1, p1) } else { (l2, p2) };
    // periodic iff needle[..crit] == needle[period..period+crit]
    let mut periodic = period + crit <= m;
    if periodic {
        for i in 0..crit {
            if nd.at(i) != nd.at(i + period) {
                periodic = false;
                break;
            }
        }
    }
    if periodic {
        TwoWay { crit, period, periodic: true }
    } else {
        TwoWay { crit, period: crit.max(m - crit) + 1, periodic: false }
    }
}

fn two_way<T: Unit, const REV: bool>(h: View<T, REV>, nd: View<T, REV>, tw: TwoWay, from: usize) -> Option<usize> {
    let n = h.len();
    let m = nd.len();
    let mut pos = from;
    let mut memory = 0usize;
    let last = nd.at(m - 1);
    let mut mask = 0u64;
    for i in 0..m {
        mask |= bloom_bit(nd.at(i));
    }
    'outer: while pos + m <= n {
        // quick skip: last unit not in needle at all
        let tail = h.at(pos + m - 1);
        if tail != last && mask & bloom_bit(tail) == 0 {
            pos += m;
            memory = 0;
            continue;
        }
        let start = if tw.periodic { tw.crit.max(memory) } else { tw.crit };
        let mut i = start;
        while i < m {
            if nd.at(i) != h.at(pos + i) {
                pos += i - tw.crit + 1;
                memory = 0;
                continue 'outer;
            }
            i += 1;
        }
        let lo = if tw.periodic { memory } else { 0 };
        let mut j = tw.crit;
        while j > lo {
            j -= 1;
            if nd.at(j) != h.at(pos + j) {
                pos += tw.period;
                if tw.periodic {
                    memory = m - tw.period;
                }
                continue 'outer;
            }
        }
        return Some(pos);
    }
    None
}

// ------------------------------------------------------------------ StrData level API

/// Needle converted to the haystack's code unit width.  Returns None if the
/// needle contains a code point that cannot occur in the haystack.
pub enum Conv<'a, T> {
    Borrowed(&'a [T]),
    Owned(Vec<T>),
}
impl<'a, T> std::ops::Deref for Conv<'a, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Conv::Borrowed(s) => s,
            Conv::Owned(v) => v,
        }
    }
}

pub trait AsUnits<'a> {
    fn units<T: Unit>(&self) -> Option<Conv<'a, T>>;
}
impl<'a> AsUnits<'a> for StrData<'a> {
    fn units<T: Unit>(&self) -> Option<Conv<'a, T>> {
        let w = std::mem::size_of::<T>();
        let k = self.kind() as usize;
        if k == w {
            // same width: reinterpret
            let s = self.as_bytes();
            return Some(Conv::Borrowed(unsafe { std::slice::from_raw_parts(s.as_ptr() as *const T, self.len()) }));
        }
        if k > w {
            // canonical strings: a wider needle holds a code point that the
            // narrower haystack cannot contain.  Be robust anyway.
            let lim: u32 = if w == 1 { 0xff } else { 0xffff };
            let mut v = Vec::with_capacity(self.len());
            for c in self.iter() {
                if c > lim {
                    return None;
                }
                v.push(T::from_u32(c));
            }
            return Some(Conv::Owned(v));
        }
        Some(Conv::Owned(self.iter().map(T::from_u32).collect()))
    }
}

macro_rules! dispatch {
    ($h:expr, |$hs:ident : $t:ident| $body:expr) => {
        match $h {
            StrData::L1($hs) => {
                type $t = u8;
                $body
            }
            StrData::U2($hs) => {
                type $t = u16;
                $body
            }
            StrData::U4($hs) => {
                type $t = u32;
                $body
            }
        }
    };
}
pub(crate) use dispatch;

/// Find `n` in `h[start..end]`; returns absolute index.
pub fn find(h: StrData, n: StrData, start: usize, end: usize) -> Option<usize> {
    if start > end || end > h.len() {
        return None;
    }
    if n.len() == 0 {
        return Some(start);
    }
    if n.len() > end - start {
        return None;
    }
    dispatch!(h, |hs: T| {
        let nd = n.units::<T>()?;
        let mut f = Finder::<T, false>::new(&nd);
        f.find(&hs[start..end], 0).map(|p| p + start)
    })
}

/// Last occurrence of `n` in `h[start..end]`; absolute index.
pub fn rfind(h: StrData, n: StrData, start: usize, end: usize) -> Option<usize> {
    if start > end || end > h.len() {
        return None;
    }
    let m = n.len();
    if m == 0 {
        return Some(end);
    }
    if m > end - start {
        return None;
    }
    dispatch!(h, |hs: T| {
        let nd = n.units::<T>()?;
        let mut f = Finder::<T, true>::new(&nd);
        f.find(&hs[start..end], 0).map(|p| end - p - m)
    })
}

/// Non-overlapping occurrence count in `h[start..end]`, at most `max`.
pub fn count(h: StrData, n: StrData, start: usize, end: usize, max: usize) -> usize {
    if start > end || end > h.len() {
        return 0;
    }
    let m = n.len();
    if m == 0 {
        return (end - start + 1).min(max);
    }
    if m > end - start || max == 0 {
        return 0;
    }
    dispatch!(h, |hs: T| {
        let nd = match n.units::<T>() {
            Some(x) => x,
            None => return 0,
        };
        let hs = &hs[start..end];
        if m == 1 {
            return count_unit(hs, nd[0], max);
        }
        let mut f = Finder::<T, false>::new(&nd);
        let mut pos = 0;
        let mut cnt = 0;
        while let Some(p) = f.find(hs, pos) {
            cnt += 1;
            if cnt >= max {
                break;
            }
            pos = p + m;
        }
        cnt
    })
}

/// All non-overlapping match positions (forward), at most `max`.
pub fn find_all(h: StrData, n: StrData, max: usize) -> Vec<usize> {
    let m = n.len();
    let mut out = Vec::new();
    if m == 0 || m > h.len() || max == 0 {
        return out;
    }
    dispatch!(h, |hs: T| {
        let nd = match n.units::<T>() {
            Some(x) => x,
            None => return out,
        };
        if m == 1 {
            let c = nd[0];
            for (i, &x) in hs.iter().enumerate() {
                if x == c {
                    out.push(i);
                    if out.len() >= max {
                        break;
                    }
                }
            }
            return out;
        }
        let mut f = Finder::<T, false>::new(&nd);
        let mut pos = 0;
        while let Some(p) = f.find(hs, pos) {
            out.push(p);
            if out.len() >= max {
                break;
            }
            pos = p + m;
        }
        out
    })
}

/// All non-overlapping match positions scanning from the right (returned in
/// right-to-left order, absolute indices of match starts), at most `max`.
pub fn rfind_all(h: StrData, n: StrData, max: usize) -> Vec<usize> {
    let m = n.len();
    let len = h.len();
    let mut out = Vec::new();
    if m == 0 || m > len || max == 0 {
        return out;
    }
    dispatch!(h, |hs: T| {
        let nd = match n.units::<T>() {
            Some(x) => x,
            None => return out,
        };
        let mut f = Finder::<T, true>::new(&nd);
        let mut pos = 0;
        while let Some(p) = f.find(hs, pos) {
            out.push(len - p - m);
            if out.len() >= max {
                break;
            }
            pos = p + m;
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn naive(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(0);
        }
        h.windows(n.len()).position(|w| w == n)
    }
    fn naive_r(h: &[u8], n: &[u8]) -> Option<usize> {
        if n.is_empty() {
            return Some(h.len());
        }
        h.windows(n.len()).rposition(|w| w == n)
    }
    #[test]
    fn search_matches_naive() {
        let mut seed = 12345u64;
        let mut rnd = |k: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % k
        };
        for _ in 0..20000 {
            let hl = rnd(60) as usize;
            let nl = rnd(8) as usize;
            let alpha = 1 + rnd(3) as u8;
            let h: Vec<u8> = (0..hl).map(|_| b'a' + rnd(alpha as u64) as u8).collect();
            let n: Vec<u8> = (0..nl).map(|_| b'a' + rnd(alpha as u64) as u8).collect();
            assert_eq!(find(StrData::L1(&h), StrData::L1(&n), 0, h.len()), naive(&h, &n), "{:?} {:?}", h, n);
            assert_eq!(rfind(StrData::L1(&h), StrData::L1(&n), 0, h.len()), naive_r(&h, &n), "{:?} {:?}", h, n);
            // force two-way
            if !n.is_empty() && n.len() > 1 {
                let mut f = Finder::<u8, false>::new(&n);
                f.tw = Some(two_way_prepare(f.needle));
                assert_eq!(f.find(&h, 0), naive(&h, &n));
                let mut f = Finder::<u8, true>::new(&n);
                f.tw = Some(two_way_prepare(f.needle));
                assert_eq!(f.find(&h, 0).map(|p| h.len() - p - n.len()), naive_r(&h, &n));
            }
        }
    }
    #[test]
    fn adversarial() {
        let h: Vec<u8> = std::iter::repeat(b'a').take(200000).collect();
        let mut n: Vec<u8> = std::iter::repeat(b'a').take(300).collect();
        n.push(b'b');
        assert_eq!(find(StrData::L1(&h), StrData::L1(&n), 0, h.len()), None);
        let mut n2 = vec![b'b'];
        n2.extend(std::iter::repeat(b'a').take(300));
        assert_eq!(rfind(StrData::L1(&h), StrData::L1(&n2), 0, h.len()), None);
    }
}
