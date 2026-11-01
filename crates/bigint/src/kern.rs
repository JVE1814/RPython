//! Vectorizable linear-time kernels on 30-bit digits.
//!
//! The trick: with 30-bit digits in 32-bit (or 64-bit) lanes, the carry into digit i can be
//! computed from digit i-1 alone, except in the rare case of a carry ripple (a digit becoming
//! exactly 2^30 or -1). Each kernel computes all digits independently (SIMD friendly), ORs a
//! flag of out-of-range digits, and runs a sequential fix-up pass only when the flag is set.

use crate::nat::{MASK, SHIFT};

/// Sequentially normalize digits holding values in roughly [-4, 2^30 + 4] (as i32).
/// Returns the signed carry out of the top.
#[inline(never)]
fn ripple(out: &mut [u32]) -> i64 {
    let mut c: i64 = 0;
    for x in out.iter_mut() {
        let v = *x as i32 as i64 + c;
        *x = (v as u32) & MASK;
        c = v >> SHIFT;
    }
    c
}

macro_rules! kernels {
    ($add_n:ident, $sub_n:ident, $mul1:ident, $submul1:ident $(, #[$attr:meta])?) => {
        /// out = a + b (all length n >= 2); returns carry.
        $(#[$attr])?
        unsafe fn $add_n(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
            let n = a.len();
            let out = &mut out[..n];
            let b = &b[..n];
            out[0] = (a[0] + b[0]) & MASK;
            let mut flag = 0u32;
            for (((o, &a1), &a0), (&b1, &b0)) in
                out[1..].iter_mut().zip(&a[1..]).zip(&a[..n - 1]).zip(b[1..].iter().zip(&b[..n - 1]))
            {
                let v = ((a1 + b1) & MASK) + ((a0 + b0) >> SHIFT);
                *o = v;
                flag |= v;
            }
            let mut carry = (a[n - 1] + b[n - 1]) >> SHIFT;
            if flag & !MASK != 0 {
                carry = (carry as i64 + ripple(out)) as u32;
            }
            carry
        }

        /// out = a - b (all length n >= 2); returns borrow (0 or 1).
        $(#[$attr])?
        unsafe fn $sub_n(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
            let n = a.len();
            let out = &mut out[..n];
            let b = &b[..n];
            out[0] = a[0].wrapping_sub(b[0]) & MASK;
            let mut flag = 0u32;
            for (((o, &a1), &a0), (&b1, &b0)) in
                out[1..].iter_mut().zip(&a[1..]).zip(&a[..n - 1]).zip(b[1..].iter().zip(&b[..n - 1]))
            {
                let d1 = a1.wrapping_sub(b1);
                let d0 = a0.wrapping_sub(b0);
                let v = (d1 & MASK).wrapping_sub(d0 >> 31);
                *o = v;
                flag |= v;
            }
            let mut borrow = a[n - 1].wrapping_sub(b[n - 1]) >> 31;
            if flag & !MASK != 0 {
                borrow = (borrow as i64 - ripple(out)) as u32;
            }
            borrow
        }

        /// out = a * d (n >= 2, d < 2^30); returns carry (< 2^30 + 2).
        $(#[$attr])?
        unsafe fn $mul1(out: &mut [u32], a: &[u32], d: u32) -> u64 {
            let n = a.len();
            let out = &mut out[..n];
            let d = d as u64;
            let m = MASK as u64;
            let p0 = a[0] as u64 * d;
            let p1 = a[1] as u64 * d;
            out[0] = (p0 & m) as u32;
            let t1 = (p1 & m) + (p0 >> SHIFT);
            out[1] = ((t1 & m) as u32) + 0;
            let mut flag = 0u64;
            // t_i = lo(p_i) + hi(p_{i-1}) < 2^31; out_i = lo30(t_i) + (t_{i-1} >> 30)
            for (((o, &x2), &x1), &x0) in out[2..].iter_mut().zip(&a[2..]).zip(&a[1..n - 1]).zip(&a[..n - 2]) {
                let q2 = x2 as u64 * d;
                let q1 = x1 as u64 * d;
                let q0 = x0 as u64 * d;
                let t2 = (q2 & m) + (q1 >> SHIFT);
                let tp = (q1 & m) + (q0 >> SHIFT);
                let v = (t2 & m) + (tp >> SHIFT);
                *o = v as u32;
                flag |= v;
            }
            // carry: t_{n-1} >> 30 plus hi(p_{n-1})
            let pl = a[n - 1] as u64 * d;
            let pl1 = a[n - 2] as u64 * d;
            let tl = (pl & m) + (pl1 >> SHIFT);
            let mut carry = (tl >> SHIFT) + (pl >> SHIFT);
            if flag & !m != 0 {
                carry = (carry as i64 + ripple(out)) as u64;
            }
            carry
        }

        /// out = x - a * d (n >= 2, d < 2^30); returns the amount to subtract from the next
        /// digit (< 2^30 + 4).
        $(#[$attr])?
        unsafe fn $submul1(out: &mut [u32], x: &[u32], a: &[u32], d: u32) -> u64 {
            let n = a.len();
            let out = &mut out[..n];
            let x = &x[..n];
            let d = d as u64;
            let m = MASK as u64;
            // y_i = x_i - lo(p_i) - hi(p_{i-1})  in (-2^31, 2^30)
            // out_i = (y_i mod 2^30) - borrow_{i-1}, borrow = -(y >> 30) in {0,1,2}
            let p0 = a[0] as u64 * d;
            let p1 = a[1] as u64 * d;
            let y0 = x[0] as i64 - (p0 & m) as i64;
            let y1 = x[1] as i64 - (p1 & m) as i64 - (p0 >> SHIFT) as i64;
            out[0] = (y0 as u32) & MASK;
            let v1 = ((y1 as u32) & MASK).wrapping_sub((-(y0 >> SHIFT)) as u32);
            out[1] = v1;
            let mut flag = v1;
            for (((((o, &xi), &xp), &a2), &a1), &a0) in out[2..]
                .iter_mut()
                .zip(&x[2..])
                .zip(&x[1..n - 1])
                .zip(&a[2..])
                .zip(&a[1..n - 1])
                .zip(&a[..n - 2])
            {
                let q2 = a2 as u64 * d;
                let q1 = a1 as u64 * d;
                let q0 = a0 as u64 * d;
                let y = xi as i64 - (q2 & m) as i64 - (q1 >> SHIFT) as i64;
                let yp = xp as i64 - (q1 & m) as i64 - (q0 >> SHIFT) as i64;
                let v = ((y as u32) & MASK).wrapping_sub((-(yp >> SHIFT)) as u32);
                *o = v;
                flag |= v;
            }
            let pl = a[n - 1] as u64 * d;
            let pl1 = if n >= 2 { a[n - 2] as u64 * d } else { 0 };
            let yl = x[n - 1] as i64 - (pl & m) as i64 - (pl1 >> SHIFT) as i64;
            let mut amount = (pl >> SHIFT) as i64 - (yl >> SHIFT);
            if flag & !MASK != 0 {
                amount -= ripple(out);
            }
            amount as u64
        }
    };
}

kernels!(add_n_generic, sub_n_generic, mul1_generic, submul1_generic);
#[cfg(target_arch = "x86_64")]
kernels!(add_n_avx2, sub_n_avx2, mul1_avx2, submul1_avx2, #[target_feature(enable = "avx2")]);
#[cfg(target_arch = "x86_64")]
kernels!(add_n_avx512, sub_n_avx512, mul1_avx512, submul1_avx512, #[target_feature(enable = "avx512f,avx512bw,avx512vl,avx2")]);

use crate::cpu::{avx2, avx512};

/// out[..n] = a + b for equal lengths n >= 2. Returns carry.
#[inline]
pub fn add_n(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
    assert!(a.len() >= 2 && b.len() >= a.len() && out.len() >= a.len());
    #[cfg(target_arch = "x86_64")]
    if avx512() {
        return unsafe { add_n_avx512(out, a, b) };
    }
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        return unsafe { add_n_avx2(out, a, b) };
    }
    unsafe { add_n_generic(out, a, b) }
}

/// out[..n] = a - b for equal lengths n >= 2. Returns borrow.
#[inline]
pub fn sub_n(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
    assert!(a.len() >= 2 && b.len() >= a.len() && out.len() >= a.len());
    #[cfg(target_arch = "x86_64")]
    if avx512() {
        return unsafe { sub_n_avx512(out, a, b) };
    }
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        return unsafe { sub_n_avx2(out, a, b) };
    }
    unsafe { sub_n_generic(out, a, b) }
}

/// out[..n] = a * d for n >= 2, d < 2^30. Returns carry.
#[inline]
pub fn mul1(out: &mut [u32], a: &[u32], d: u32) -> u64 {
    assert!(a.len() >= 2 && out.len() >= a.len() && d <= MASK);
    #[cfg(target_arch = "x86_64")]
    if avx512() {
        return unsafe { mul1_avx512(out, a, d) };
    }
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        return unsafe { mul1_avx2(out, a, d) };
    }
    unsafe { mul1_generic(out, a, d) }
}

/// out[..n] = x - a * d (n >= 2, d < 2^30). Returns the amount to subtract at position n.
#[inline]
pub fn submul1(out: &mut [u32], x: &[u32], a: &[u32], d: u32) -> u64 {
    assert!(a.len() >= 2 && x.len() >= a.len() && out.len() >= a.len() && d <= MASK);
    #[cfg(target_arch = "x86_64")]
    if avx512() {
        return unsafe { submul1_avx512(out, x, a, d) };
    }
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        return unsafe { submul1_avx2(out, x, a, d) };
    }
    unsafe { submul1_generic(out, x, a, d) }
}
