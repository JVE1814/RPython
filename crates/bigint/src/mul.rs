//! Multiplication: lazy-carry schoolbook, Karatsuba, Toom-3 and a two-prime NTT.

use crate::nat::{self, MASK, SHIFT};

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static KARATSUBA_T: AtomicUsize = AtomicUsize::new(96);
static KARATSUBA_SQR_T: AtomicUsize = AtomicUsize::new(96);
static TOOM3_T: AtomicUsize = AtomicUsize::new(300);
static NTT_T: AtomicUsize = AtomicUsize::new(1200);
static NTT_SQR_T: AtomicUsize = AtomicUsize::new(1500);

/// Override the algorithm thresholds (digits of the shorter operand): schoolbook below
/// `kara`, Karatsuba below `toom3`, Toom-3 below `ntt`, NTT above. For tuning and tests.
pub fn set_thresholds(kara: usize, kara_sqr: usize, toom3: usize, ntt: usize, ntt_sqr: usize) {
    KARATSUBA_T.store(kara.max(2), Relaxed);
    KARATSUBA_SQR_T.store(kara_sqr.max(2), Relaxed);
    TOOM3_T.store(toom3.max(8), Relaxed);
    NTT_T.store(ntt.max(2), Relaxed);
    NTT_SQR_T.store(ntt_sqr.max(2), Relaxed);
}

/// Current thresholds (kara, kara_sqr, toom3, ntt, ntt_sqr).
pub fn thresholds() -> (usize, usize, usize, usize, usize) {
    (KARATSUBA_T.load(Relaxed), KARATSUBA_SQR_T.load(Relaxed), TOOM3_T.load(Relaxed), NTT_T.load(Relaxed), NTT_SQR_T.load(Relaxed))
}

#[inline(always)]
fn kara_t() -> usize {
    KARATSUBA_T.load(Relaxed)
}
#[inline(always)]
fn kara_sqr_t() -> usize {
    KARATSUBA_SQR_T.load(Relaxed)
}
#[inline(always)]
fn toom3_t() -> usize {
    TOOM3_T.load(Relaxed)
}
#[inline(always)]
fn ntt_t() -> usize {
    NTT_T.load(Relaxed)
}
#[inline(always)]
fn ntt_sqr_t() -> usize {
    NTT_SQR_T.load(Relaxed)
}

const BATCH: usize = 15;
const STACK_ACC: usize = 2 * 128;

// ---------------------------------------------------------------------------------------------
// Schoolbook kernels with lazy carry propagation in u64 accumulators.
//
// Each product of two 30-bit digits is < 2^60, so up to 15 of them can be accumulated into a
// u64 on top of a (partially) normalized value before a carry pass is needed. The inner loop is
// a plain multiply-add over u64 lanes and auto-vectorizes (pmuludq).
// ---------------------------------------------------------------------------------------------

macro_rules! basecase_impls {
    ($mul:ident, $sqr:ident $(, #[$attr:meta])?) => {
        $(#[$attr])?
        unsafe fn $mul(out: &mut [u32], a: &[u32], b: &[u32], acc: &mut [u64]) {
            // a is the longer operand (inner loop), b the shorter (rows).
            let n = a.len();
            let m = b.len();
            let acc = &mut acc[..n + m];
            acc.fill(0);
            let mut i0 = 0;
            while i0 < m {
                let i1 = if i0 + BATCH < m { i0 + BATCH } else { m };
                for i in i0..i1 {
                    let bi = b[i] as u64;
                    let row = &mut acc[i..i + n];
                    for (r, &x) in row.iter_mut().zip(a.iter()) {
                        *r += bi * x as u64;
                    }
                }
                let end = i1 - 1 + n;
                let mut carry = 0u64;
                for k in i0..end {
                    let v = acc[k] + carry;
                    acc[k] = v & MASK as u64;
                    carry = v >> SHIFT;
                }
                acc[end] += carry;
                i0 = i1;
            }
            for (o, &v) in out[..n + m].iter_mut().zip(acc.iter()) {
                *o = v as u32;
            }
        }

        $(#[$attr])?
        unsafe fn $sqr(out: &mut [u32], a: &[u32], acc: &mut [u64]) {
            let n = a.len();
            let acc = &mut acc[..2 * n];
            acc.fill(0);
            // Off-diagonal products a[i]*a[j], j > i, at position i+j.
            let mut i0 = 0;
            while i0 + 1 < n {
                let i1 = if i0 + BATCH < n - 1 { i0 + BATCH } else { n - 1 };
                for i in i0..i1 {
                    let ai = a[i] as u64;
                    let row = &mut acc[2 * i + 1..i + n];
                    for (r, &x) in row.iter_mut().zip(a[i + 1..].iter()) {
                        *r += ai * x as u64;
                    }
                }
                let end = i1 - 1 + n;
                let mut carry = 0u64;
                for k in (2 * i0 + 1)..end {
                    let v = acc[k] + carry;
                    acc[k] = v & MASK as u64;
                    carry = v >> SHIFT;
                }
                acc[end] += carry;
                i0 = i1;
            }
            // Full normalization pass (stray carries).
            let mut carry = 0u64;
            for k in 0..2 * n {
                let v = acc[k] + carry;
                acc[k] = v & MASK as u64;
                carry = v >> SHIFT;
            }
            debug_assert_eq!(carry, 0);
            // Double and add the diagonal.
            let mut carry = 0u64;
            for i in 0..n {
                let sq = a[i] as u64 * a[i] as u64;
                let v0 = 2 * acc[2 * i] + (sq & MASK as u64) + carry;
                out[2 * i] = (v0 as u32) & MASK;
                carry = v0 >> SHIFT;
                let v1 = 2 * acc[2 * i + 1] + (sq >> SHIFT) + carry;
                out[2 * i + 1] = (v1 as u32) & MASK;
                carry = v1 >> SHIFT;
            }
            debug_assert_eq!(carry, 0);
        }
    };
}

basecase_impls!(mul_basecase_generic, sqr_basecase_generic);
#[cfg(target_arch = "x86_64")]
basecase_impls!(mul_basecase_avx2, sqr_basecase_avx2, #[target_feature(enable = "avx2")]);
#[cfg(target_arch = "x86_64")]
basecase_impls!(mul_basecase_avx512, sqr_basecase_avx512, #[target_feature(enable = "avx512f,avx512bw,avx512vl,avx2")]);

use crate::cpu::{avx2, avx512};

/// Schoolbook product into `out[..a.len()+b.len()]`. Inputs nonempty.
pub fn mul_basecase(out: &mut [u32], a: &[u32], b: &[u32]) {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if b.len() == 1 {
        let c = nat::mul_digit_to(out, a, b[0], 0);
        out[a.len()] = c as u32;
        return;
    }
    let total = a.len() + b.len();
    let mut stack = [0u64; STACK_ACC];
    let mut heap;
    let acc: &mut [u64] = if total <= STACK_ACC {
        &mut stack[..total]
    } else {
        heap = vec![0u64; total];
        &mut heap[..]
    };
    #[cfg(target_arch = "x86_64")]
    {
        if avx512() {
            unsafe { mul_basecase_avx512(out, a, b, acc) };
            return;
        }
        if avx2() {
            unsafe { mul_basecase_avx2(out, a, b, acc) };
            return;
        }
    }
    unsafe { mul_basecase_generic(out, a, b, acc) }
}

/// Schoolbook square into `out[..2*a.len()]`.
pub fn sqr_basecase(out: &mut [u32], a: &[u32]) {
    let n = a.len();
    if n == 1 {
        let s = a[0] as u64 * a[0] as u64;
        out[0] = (s as u32) & MASK;
        out[1] = (s >> SHIFT) as u32;
        return;
    }
    let mut stack = [0u64; STACK_ACC];
    let mut heap;
    let acc: &mut [u64] = if 2 * n <= STACK_ACC {
        &mut stack[..2 * n]
    } else {
        heap = vec![0u64; 2 * n];
        &mut heap[..]
    };
    #[cfg(target_arch = "x86_64")]
    {
        if avx512() {
            unsafe { sqr_basecase_avx512(out, a, acc) };
            return;
        }
        if avx2() {
            unsafe { sqr_basecase_avx2(out, a, acc) };
            return;
        }
    }
    unsafe { sqr_basecase_generic(out, a, acc) }
}

// ---------------------------------------------------------------------------------------------
// Recursive multiplication on slices.
// ---------------------------------------------------------------------------------------------

/// Scratch size needed by `mul_rec` for operands of length up to `n`.
fn scratch_len(n: usize) -> usize {
    8 * n + 256
}

/// `out[..a.len()+b.len()] = a * b`. Operands may contain high zero digits; both nonempty.
/// `out` must not alias. `scratch` must be at least `scratch_len(max(len))`.
fn mul_rec(out: &mut [u32], a: &[u32], b: &[u32], scratch: &mut [u32]) {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let n = a.len();
    let m = b.len();
    debug_assert!(m >= 1);
    if m < kara_t() {
        mul_basecase(out, a, b);
        return;
    }
    if m >= ntt_t() && n + m <= crate::ntt::MAX_LEN && 2 * m > n {
        mul_ntt_into(out, a, b);
        return;
    }
    if 2 * m <= n + 1 {
        // Unbalanced: split a into chunks of length m.
        mul_unbalanced(out, a, b, scratch);
        return;
    }
    if m >= toom3_t() {
        toom3(out, a, b, scratch);
        return;
    }
    karatsuba(out, a, b, scratch);
}

fn mul_ntt_into(out: &mut [u32], a: &[u32], b: &[u32]) {
    let total = a.len() + b.len();
    let (ta, tb) = (nat::trim(a), nat::trim(b));
    out[..total].fill(0);
    if ta.is_empty() || tb.is_empty() {
        return;
    }
    let p = crate::ntt::mul(ta, tb);
    out[..p.len()].copy_from_slice(&p);
}

fn mul_unbalanced(out: &mut [u32], a: &[u32], b: &[u32], scratch: &mut [u32]) {
    let n = a.len();
    let m = b.len();
    out[..n + m].fill(0);
    let (tmp, rest) = scratch.split_at_mut(2 * m);
    let mut off = 0;
    while off < n {
        let end = (off + m).min(n);
        let chunk = &a[off..end];
        let t = &mut tmp[..chunk.len() + m];
        mul_rec(t, chunk, b, rest);
        nat::add_assign(&mut out[off..n + m], t);
        off = end;
    }
}

/// |x - y| into out (len = x.len() >= y.len()); returns true if x < y.
fn abs_sub(out: &mut [u32], x: &[u32], y: &[u32]) -> bool {
    debug_assert!(x.len() >= y.len());
    // Compare x vs y (y zero-extended).
    let mut less = false;
    let mut decided = false;
    for i in (y.len()..x.len()).rev() {
        if x[i] != 0 {
            decided = true;
            break;
        }
    }
    if !decided {
        let mut i = y.len();
        while i > 0 {
            i -= 1;
            if x[i] != y[i] {
                less = x[i] < y[i];
                break;
            }
        }
    }
    if less {
        // out = y - x where x is longer but its high part is zero
        let l = y.len();
        nat::sub_to(&mut out[..l], y, &x[..l]);
        out[l..x.len()].fill(0);
    } else {
        nat::sub_to(out, x, y);
    }
    less
}

/// Karatsuba for n/2 < m <= n.
fn karatsuba(out: &mut [u32], a: &[u32], b: &[u32], scratch: &mut [u32]) {
    let n = a.len();
    let m = b.len();
    let h = n.div_ceil(2);
    debug_assert!(m > h);
    let (a0, a1) = a.split_at(h);
    let (b0, b1) = b.split_at(h);
    {
        let (lo, hi) = out[..n + m].split_at_mut(2 * h);
        mul_rec(lo, a0, b0, scratch);
        mul_rec(hi, a1, b1, scratch);
    }
    let (da, rest) = scratch.split_at_mut(h);
    let (db, rest) = rest.split_at_mut(h);
    let (prod, rest) = rest.split_at_mut(2 * h);
    let (t, rest) = rest.split_at_mut(2 * h + 1);
    let sa = abs_sub(da, a0, a1);
    let sb = abs_sub(db, b0, b1);
    mul_rec(prod, da, db, rest);
    // t = z0 + z2
    t[..2 * h].copy_from_slice(&out[..2 * h]);
    t[2 * h] = 0;
    let z2 = &out[2 * h..n + m];
    nat::add_assign(t, z2);
    // z1 = z0 + z2 - (a0-a1)(b0-b1)
    if sa == sb {
        let br = nat::sub_assign(t, prod);
        debug_assert_eq!(br, 0);
    } else {
        let c = nat::add_assign(t, prod);
        debug_assert_eq!(c, 0);
    }
    let tl = nat::trimmed_len(t);
    let c = nat::add_assign(&mut out[h..n + m], &t[..tl]);
    debug_assert_eq!(c, 0);
}

fn sqr_rec(out: &mut [u32], a: &[u32], scratch: &mut [u32]) {
    let n = a.len();
    if n < kara_sqr_t() {
        sqr_basecase(out, a);
        return;
    }
    if n >= ntt_sqr_t() && 2 * n <= crate::ntt::MAX_LEN {
        let t = nat::trim(a);
        out[..2 * n].fill(0);
        if !t.is_empty() {
            let p = crate::ntt::sqr(t);
            out[..p.len()].copy_from_slice(&p);
        }
        return;
    }
    if n >= toom3_t() {
        toom3_sqr(out, a, scratch);
        return;
    }
    let h = n.div_ceil(2);
    let (a0, a1) = a.split_at(h);
    {
        let (lo, hi) = out[..2 * n].split_at_mut(2 * h);
        sqr_rec(lo, a0, scratch);
        sqr_rec(hi, a1, scratch);
    }
    let (da, rest) = scratch.split_at_mut(h);
    let (prod, rest) = rest.split_at_mut(2 * h);
    let (t, rest) = rest.split_at_mut(2 * h + 1);
    abs_sub(da, a0, a1);
    sqr_rec(prod, da, rest);
    t[..2 * h].copy_from_slice(&out[..2 * h]);
    t[2 * h] = 0;
    nat::add_assign(t, &out[2 * h..2 * n]);
    let br = nat::sub_assign(t, prod);
    debug_assert_eq!(br, 0);
    let tl = nat::trimmed_len(t);
    let c = nat::add_assign(&mut out[h..2 * n], &t[..tl]);
    debug_assert_eq!(c, 0);
}

// ---------------------------------------------------------------------------------------------
// Toom-3 (evaluation at 0, 1, -1, -2, inf) using owned signed temporaries for clarity.
// ---------------------------------------------------------------------------------------------

/// Signed magnitude helper for Toom interpolation.
#[derive(Clone)]
struct S {
    neg: bool,
    m: Vec<u32>,
}

impl S {
    fn from(m: &[u32]) -> S {
        S { neg: false, m: nat::trim(m).to_vec() }
    }
    fn add(&self, o: &S) -> S {
        if self.neg == o.neg {
            S { neg: self.neg, m: nat::add(&self.m, &o.m) }
        } else {
            let (d, lt) = nat::abs_diff(&self.m, &o.m);
            let neg = if lt { o.neg } else { self.neg };
            S { neg: neg && !d.is_empty(), m: d }
        }
    }
    fn sub(&self, o: &S) -> S {
        let no = S { neg: !o.neg && !o.m.is_empty(), m: o.m.clone() };
        self.add(&no)
    }
    fn shl1(&self) -> S {
        S { neg: self.neg, m: nat::shl(&self.m, 1) }
    }
    fn shr1(&self) -> S {
        // exact division by 2
        debug_assert!(self.m.first().map_or(true, |d| d & 1 == 0));
        S { neg: self.neg, m: nat::shr(&self.m, 1) }
    }
    fn div3(&self) -> S {
        let (q, r) = nat::divrem_u32(&self.m, 3);
        debug_assert_eq!(r, 0);
        S { neg: self.neg && !q.is_empty(), m: q }
    }
    fn mul(&self, o: &S, scratch: &mut [u32]) -> S {
        if self.m.is_empty() || o.m.is_empty() {
            return S { neg: false, m: Vec::new() };
        }
        let mut out = vec![0u32; self.m.len() + o.m.len()];
        mul_rec(&mut out, &self.m, &o.m, scratch);
        nat::normalize(&mut out);
        S { neg: self.neg != o.neg, m: out }
    }
    fn sqr(&self, scratch: &mut [u32]) -> S {
        if self.m.is_empty() {
            return S { neg: false, m: Vec::new() };
        }
        let mut out = vec![0u32; 2 * self.m.len()];
        sqr_rec(&mut out, &self.m, scratch);
        nat::normalize(&mut out);
        S { neg: false, m: out }
    }
}

fn toom3_split(a: &[u32], k: usize) -> (S, S, S) {
    let p0 = &a[..k.min(a.len())];
    let p1 = if a.len() > k { &a[k..(2 * k).min(a.len())] } else { &[][..] };
    let p2 = if a.len() > 2 * k { &a[2 * k..] } else { &[][..] };
    (S::from(p0), S::from(p1), S::from(p2))
}

/// Evaluate at 0, 1, -1, -2, inf.
fn toom3_eval(p0: &S, p1: &S, p2: &S) -> [S; 5] {
    let t = p0.add(p2);
    let e1 = t.add(p1);
    let em1 = t.sub(p1);
    // p(-2) = p0 - 2 p1 + 4 p2 = 2*(p(-1) + p2) - p0
    let e2 = em1.add(p2).shl1().sub(p0);
    [p0.clone(), e1, em1, e2, p2.clone()]
}

fn toom3_interp(out: &mut [u32], r: [S; 5], k: usize) {
    // Bodrato sequence for points 0, 1, -1, -2, inf.
    let [r0, r1, rm1, rm2, rinf] = r;
    let mut r3 = rm2.sub(&r1).div3(); // (r(-2) - r(1))/3
    let mut r1b = r1.sub(&rm1).shr1(); // (r(1) - r(-1))/2
    let mut r2b = rm1.sub(&r0); // r(-1) - r(0)
    r3 = r2b.sub(&r3).shr1().add(&rinf.shl1()); // (r2b - r3)/2 + 2 rinf
    r2b = r2b.add(&r1b).sub(&rinf); // r2b + r1b - rinf
    r1b = r1b.sub(&r3);
    // result = r0 + r1b x + r2b x^2 + r3 x^3 + rinf x^4, x = B^k
    let total = out.len();
    out.fill(0);
    let place = |out: &mut [u32], s: &S, off: usize| {
        if s.m.is_empty() {
            return;
        }
        if s.neg {
            let br = nat::sub_assign(&mut out[off..], &s.m);
            debug_assert_eq!(br, 0);
        } else {
            let c = nat::add_assign(&mut out[off..], &s.m);
            debug_assert_eq!(c, 0);
        }
    };
    // Add positives first to avoid transient negatives.
    let parts = [(&r0, 0usize), (&r1b, k), (&r2b, 2 * k), (&r3, 3 * k), (&rinf, 4 * k)];
    for (s, off) in parts.iter() {
        if !s.neg {
            place(out, s, *off);
        }
    }
    for (s, off) in parts.iter() {
        if s.neg {
            place(out, s, *off);
        }
    }
    let _ = total;
}

fn toom3(out: &mut [u32], a: &[u32], b: &[u32], scratch: &mut [u32]) {
    let n = a.len();
    let m = b.len();
    let k = n.div_ceil(3);
    if m <= 2 * k {
        // b too short for a 3-way split; Karatsuba handles it.
        karatsuba(out, a, b, scratch);
        return;
    }
    let (a0, a1, a2) = toom3_split(a, k);
    let (b0, b1, b2) = toom3_split(b, k);
    let ea = toom3_eval(&a0, &a1, &a2);
    let eb = toom3_eval(&b0, &b1, &b2);
    let r: [S; 5] = [
        ea[0].mul(&eb[0], scratch),
        ea[1].mul(&eb[1], scratch),
        ea[2].mul(&eb[2], scratch),
        ea[3].mul(&eb[3], scratch),
        ea[4].mul(&eb[4], scratch),
    ];
    toom3_interp(&mut out[..n + m], r, k);
}

fn toom3_sqr(out: &mut [u32], a: &[u32], scratch: &mut [u32]) {
    let n = a.len();
    let k = n.div_ceil(3);
    let (a0, a1, a2) = toom3_split(a, k);
    let ea = toom3_eval(&a0, &a1, &a2);
    let r: [S; 5] = [
        ea[0].sqr(scratch),
        ea[1].sqr(scratch),
        ea[2].sqr(scratch),
        ea[3].sqr(scratch),
        ea[4].sqr(scratch),
    ];
    toom3_interp(&mut out[..2 * n], r, k);
}

// ---------------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------------

/// Write `a * b` into `out` (len >= a.len()+b.len()); inputs normalized and nonempty or empty.
/// Returns the normalized length of the product.
pub fn mul_into(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let total = a.len() + b.len();
    let small = a.len().min(b.len());
    let big = a.len().max(b.len());
    if small < kara_t() {
        mul_basecase(out, a, b);
    } else if small >= ntt_t() && total <= crate::ntt::MAX_LEN && 2 * small > big {
        mul_ntt_into(out, a, b);
    } else {
        let mut scratch = vec![0u32; scratch_len(a.len().max(b.len()))];
        mul_rec(out, a, b, &mut scratch);
    }
    nat::trimmed_len(&out[..total])
}

/// `a * b` for normalized inputs.
pub fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    if a.len() == 1 {
        return nat::mul_u32(b, a[0]);
    }
    if b.len() == 1 {
        return nat::mul_u32(a, b[0]);
    }
    let mut out = vec![0u32; a.len() + b.len()];
    let n = mul_into(&mut out, a, b);
    out.truncate(n);
    out
}

/// `a * a` for a normalized input.
pub fn sqr(a: &[u32]) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    if a.len() >= ntt_sqr_t() && 2 * a.len() <= crate::ntt::MAX_LEN {
        return crate::ntt::sqr(a);
    }
    let mut out = vec![0u32; 2 * a.len()];
    if a.len() < kara_sqr_t() {
        sqr_basecase(&mut out, a);
    } else {
        let mut scratch = vec![0u32; scratch_len(a.len())];
        sqr_rec(&mut out, a, &mut scratch);
    }
    nat::normalize(&mut out);
    out
}

/// Product without NTT / with explicit algorithm selection (for testing).
pub fn mul_schoolbook(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    mul_basecase(&mut out, a, b);
    nat::normalize(&mut out);
    out
}

/// Product using the recursive (Karatsuba/Toom) path regardless of size (for testing).
pub fn mul_recursive(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    let mut scratch = vec![0u32; scratch_len(a.len().max(b.len()))];
    mul_rec(&mut out, a, b, &mut scratch);
    nat::normalize(&mut out);
    out
}

/// Square using the recursive path regardless of size (for testing).
pub fn sqr_recursive(a: &[u32]) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; 2 * a.len()];
    let mut scratch = vec![0u32; scratch_len(a.len())];
    sqr_rec(&mut out, a, &mut scratch);
    nat::normalize(&mut out);
    out
}
