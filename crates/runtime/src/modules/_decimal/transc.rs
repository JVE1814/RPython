//! Square root, exponential, logarithms and power, correctly rounded.
//!
//! The transcendental functions evaluate a fixed-point approximation (integer scaled
//! by 10^w) with a known error bound and accept the result once both ends of the
//! error interval round to the same decimal (Ziv's strategy); otherwise the working
//! precision is increased.  Cases where the exact result is representable are
//! detected beforehand so the loop always terminates.

use super::dec::*;
use super::nat::{self, Rest};
use super::ops::{self, invalid, nan1, nan2};
use std::cmp::Ordering;

// ------------------------------- signed fixed point ----------------------------------------

#[derive(Clone, Debug)]
struct SInt {
    neg: bool,
    mag: Vec<u64>,
}

impl SInt {
    fn new(neg: bool, mag: Vec<u64>) -> SInt {
        let neg = neg && !mag.is_empty();
        SInt { neg, mag }
    }
    fn add(&self, o: &SInt) -> SInt {
        if self.neg == o.neg {
            SInt::new(self.neg, nat::add(&self.mag, &o.mag))
        } else {
            match nat::cmp(&self.mag, &o.mag) {
                Ordering::Less => SInt::new(o.neg, nat::sub(&o.mag, &self.mag)),
                _ => SInt::new(self.neg, nat::sub(&self.mag, &o.mag)),
            }
        }
    }
    fn sub(&self, o: &SInt) -> SInt {
        self.add(&SInt::new(!o.neg, o.mag.clone()))
    }
    fn mul(&self, o: &SInt) -> SInt {
        SInt::new(self.neg ^ o.neg, nat::mul(&self.mag, &o.mag))
    }
    /// truncate toward zero by 10^n
    fn shr(&self, n: u64) -> SInt {
        SInt::new(self.neg, nat::shr_trunc(&self.mag, n))
    }
    fn shl(&self, n: u64) -> SInt {
        SInt::new(self.neg, nat::shl(&self.mag, n))
    }
}

/// value of a finite Dec scaled by 10^w, truncated toward zero.
fn dec_to_fixed(d: &Dec, w: i64) -> SInt {
    let e = d.exp as i128 + w as i128;
    let mag = if e >= 0 { nat::shl(&d.coef, e as u64) } else { nat::shr_trunc(&d.coef, (-e).min(u64::MAX as i128 / 2) as u64) };
    SInt::new(d.neg, mag)
}

/// Approximate f64 of a nat (relative error ~1e-16) together with a decimal exponent:
/// value = m * 10^e10.
fn nat_f64(a: &[u64]) -> (f64, i64) {
    let nd = nat::digits(a);
    if nd <= 17 {
        return (nat::to_u64(a).unwrap_or(0) as f64, 0);
    }
    let top = nat::shr_trunc(a, nd - 17);
    (nat::to_u64(&top).unwrap() as f64, (nd - 17) as i64)
}

/// log10(|d|) for finite nonzero d (accurate to ~1e-15 absolute in the fraction).
fn log10_abs(d: &Dec) -> f64 {
    let (m, e) = nat_f64(&d.coef);
    m.log10() + (e as f64) + d.exp as f64
}

/// (mantissa, exp10) approximation of a finite Dec value.
fn dec_f64(d: &Dec) -> (f64, i64) {
    let (m, e) = nat_f64(&d.coef);
    let m = if d.neg { -m } else { m };
    (m, e + d.exp)
}

fn to_f64_clamped(m: f64, e: i64) -> f64 {
    if m == 0.0 {
        return 0.0;
    }
    if e > 400 {
        return m.signum() * f64::INFINITY;
    }
    if e < -400 {
        return 0.0;
    }
    m * 10f64.powi(e as i32)
}

// ------------------------------- core series ----------------------------------------

/// e^(r / 10^w) * 10^w for |r| < ~4*10^w; absolute error <= 3 units.
fn exp_fixed(r: &SInt, w: u64) -> Vec<u64> {
    if r.mag.is_empty() {
        return nat::pow10(w);
    }
    let k = ((w as f64 * 3.3).sqrt() * 0.75).ceil().max(1.0) as u64;
    let g = ((k as f64) * 0.30103).ceil() as u64 + 5;
    let w2 = w + g;
    let one = nat::pow10(w2);
    // R' = r * 10^g / 2^k
    let mut rr = nat::shl(&r.mag, g);
    let mut kk = k;
    while kk > 0 {
        let s = kk.min(60);
        rr = nat::divrem_small(&rr, 1u64 << s).0;
        kk -= s;
    }
    let mut pos = one.clone();
    let mut negs: Vec<u64> = vec![];
    let mut term = one;
    let mut i = 1u64;
    loop {
        term = nat::shr_trunc(&nat::mul(&term, &rr), w2);
        term = nat::divrem_small(&term, i).0;
        if term.is_empty() {
            break;
        }
        if r.neg && i % 2 == 1 {
            nat::add_into(&mut negs, &term, 0);
        } else {
            nat::add_into(&mut pos, &term, 0);
        }
        i += 1;
    }
    let mut s = nat::sub(&pos, &negs);
    for _ in 0..k {
        s = nat::shr_trunc(&nat::sqr(&s), w2);
    }
    nat::shr_trunc(&s, g)
}

/// ln(m / 10^w) * 10^w for m/10^w in (1e-3, 1e3); absolute error <= ~4 units.
fn ln_fixed(m: &[u64], w: u64) -> SInt {
    let g = 6u64;
    let w2 = w + g;
    let mm = nat::shl(m, g);
    // f64 estimate of ln(v)
    let (mf, me) = nat_f64(&mm);
    let v = mf * 10f64.powi((me - w2 as i64) as i32);
    let y0 = v.ln();
    // y0 rounded to 17 decimals as an exact rational Y0/10^17
    let y0i = (y0 * 1e17).round() as i128;
    let y0s = SInt::new(y0i < 0, nat::from_u128(y0i.unsigned_abs()));
    let y0w = y0s.shl(w2 - 17);
    // z = v * e^{-y0}
    let einv = exp_fixed(&SInt::new(!y0w.neg, y0w.mag.clone()), w2);
    let z = nat::shr_trunc(&nat::mul(&mm, &einv), w2);
    let one = nat::pow10(w2);
    // s = (z - 1) / (z + 1)
    let zm1 = SInt::new(false, z.clone()).sub(&SInt::new(false, one.clone()));
    let zp1 = nat::add(&z, &one);
    let s = SInt::new(zm1.neg, nat::divrem(&nat::shl(&zm1.mag, w2), &zp1).0);
    let s2 = nat::shr_trunc(&nat::sqr(&s.mag), w2);
    let mut sum = s.mag.clone();
    let mut term = s.mag.clone();
    let mut i = 3u64;
    loop {
        term = nat::shr_trunc(&nat::mul(&term, &s2), w2);
        if term.is_empty() {
            break;
        }
        let t = nat::divrem_small(&term, i).0;
        if t.is_empty() {
            break;
        }
        nat::add_into(&mut sum, &t, 0);
        i += 2;
    }
    let lnz = SInt::new(s.neg, nat::mul_small(&sum, 2));
    y0w.add(&lnz).shr(g)
}

thread_local! {
    static LN10: std::cell::RefCell<(u64, Vec<u64>)> = const { std::cell::RefCell::new((0, Vec::new())) };
}

/// ln(10) * 10^w (error <= ~5 units).
fn ln10(w: u64) -> Vec<u64> {
    let cached = LN10.with(|c| {
        let c = c.borrow();
        if c.0 >= w && !c.1.is_empty() { Some(nat::shr_trunc(&c.1, c.0 - w)) } else { None }
    });
    if let Some(v) = cached {
        return v;
    }
    let ww = w.max(64) + 16;
    let v = ln_fixed(&nat::shl(&[10], ww), ww).mag;
    LN10.with(|c| *c.borrow_mut() = (ww, v.clone()));
    nat::shr_trunc(&v, ww - w)
}

// ------------------------------- Ziv rounding ----------------------------------------

/// The true magnitude lies strictly within (e-k, e+k) * 10^exp. If every value of
/// that interval rounds to the same result, return it (flags set as inexact).
fn ziv(e: &[u64], k: u64, exp: i64, neg: bool, ctx: &mut Ctx) -> Option<Dec> {
    let kk = nat::from_u64(k);
    if nat::cmp(e, &kk) != Ordering::Greater {
        return None;
    }
    let lo = nat::sub(e, &kk);
    let mut hi = nat::add(e, &kk);
    nat::decr(&mut hi);
    let mut c1 = ctx.clone();
    c1.status = 0;
    let r1 = finalize(Dec::finite(neg, lo, exp), &mut c1, Rest::Lt);
    let mut c2 = ctx.clone();
    c2.status = 0;
    let r2 = finalize(Dec::finite(neg, hi, exp), &mut c2, Rest::Gt);
    if r1.kind == r2.kind && r1.exp == r2.exp && r1.coef == r2.coef && r1.neg == r2.neg {
        ctx.status |= c1.status | c2.status | INEXACT | ROUNDED;
        Some(r1)
    } else {
        None
    }
}

/// Result for a value strictly between 1 and 1 + 10^-(prec+2) (above=true) or
/// strictly between 1 - 10^-(prec+2) and 1.
fn near_one(above: bool, ctx: &mut Ctx) -> Dec {
    let p = ctx.prec.max(1) as u64 + 3;
    let mut c = nat::pow10(p);
    if above {
        nat::incr(&mut c);
    } else {
        nat::decr(&mut c);
    }
    let r = finalize(Dec::finite(false, c, -(p as i64)), ctx, Rest::Lt);
    ctx.status |= INEXACT | ROUNDED;
    r
}

fn overflow_or_underflow(over: bool, neg: bool, ctx: &mut Ctx) -> Dec {
    if over {
        overflow_result(neg, ctx)
    } else {
        // a value far below the subnormal range
        let r = finalize(Dec::finite(neg, vec![1], ctx.etiny().saturating_sub(2)), ctx, Rest::Lt);
        ctx.status |= INEXACT | ROUNDED;
        r
    }
}

fn digits_i128(v: i128) -> u64 {
    let mut v = v.unsigned_abs();
    let mut n = 1;
    while v >= 10 {
        v /= 10;
        n += 1;
    }
    n
}

// ------------------------------- sqrt ----------------------------------------

pub fn sqrt(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    let ideal = (a.exp as i128).div_euclid(2) as i64;
    if a.is_zero() {
        return finalize(Dec::finite(a.neg, vec![], ideal), ctx, Rest::Zero);
    }
    if a.neg {
        return invalid(ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    let prec = ctx.prec as i128;
    let d = nat::digits(&a.coef) as i128;
    let mut s = (2 * (prec + 1) - d).max(0);
    if (a.exp as i128 - s).rem_euclid(2) != 0 {
        s += 1;
    }
    let (n, truncated) = if s >= 0 {
        (nat::shl(&a.coef, s as u64), false)
    } else {
        let (q, r) = nat::shr(&a.coef, (-s) as u64);
        (q, r != Rest::Zero)
    };
    let r = nat::isqrt(&n);
    let rr = nat::mul(&r, &r);
    let mut exp = (a.exp as i128 - s) / 2;
    let mut root = r;
    let rest = if rr == n && !truncated {
        if exp < ideal as i128 {
            let tz = nat::trailing_zeros(&root) as i128;
            let k = tz.min(ideal as i128 - exp);
            if k > 0 {
                root = nat::shr_trunc(&root, k as u64);
                exp += k;
            }
        }
        Rest::Zero
    } else {
        // n - r^2 <= r  <=> sqrt(n) < r + 1/2
        let diff = nat::sub(&n, &rr);
        if nat::cmp(&diff, &root) != Ordering::Greater && !(truncated && diff == root) {
            Rest::Lt
        } else {
            Rest::Gt
        }
    };
    let mut c = ctx.clone();
    c.round = Round::HalfEven;
    c.status = 0;
    let res = finalize(Dec::finite(false, root, ops::clamp_i64(exp)), &mut c, rest);
    ctx.status |= c.status;
    res
}

// ------------------------------- exp ----------------------------------------

pub fn exp(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        return if a.neg { Dec::zero() } else { a.clone() };
    }
    if a.is_zero() {
        return finalize(Dec::one(), ctx, Rest::Zero);
    }
    let mut c = ctx.clone();
    c.round = Round::HalfEven;
    c.status = 0;
    let r = exp_inner(a, &mut c);
    ctx.status |= c.status;
    r
}

fn exp_inner(a: &Dec, ctx: &mut Ctx) -> Dec {
    let p = ctx.prec;
    if (a.adjusted() as i128) < -(p as i128 + 3) {
        return near_one(!a.neg, ctx);
    }
    if a.adjusted() >= 20 {
        return overflow_or_underflow(!a.neg, false, ctx);
    }
    let (m, e) = dec_f64(a);
    let xf = to_f64_clamped(m, e);
    let n_est = xf / std::f64::consts::LN_10;
    if n_est > ctx.emax as f64 + 2.0 {
        return overflow_or_underflow(true, false, ctx);
    }
    if n_est < ctx.etiny() as f64 - 2.0 {
        return overflow_or_underflow(false, false, ctx);
    }
    let n = n_est.floor() as i64;
    let mut w = p as u64 + 10;
    loop {
        let w2 = w + digits_i128(n as i128) + 4;
        let x = dec_to_fixed(a, w2 as i64);
        let l = SInt::new(n < 0, nat::mul(&ln10(w2), &nat::from_u64(n.unsigned_abs())));
        let r = x.sub(&l);
        let ev = nat::shr_trunc(&exp_fixed(&r, w2), w2 - w);
        if let Some(d) = ziv(&ev, 3, n - w as i64, false, ctx) {
            return d;
        }
        w = w * 2;
    }
}

// ------------------------------- ln / log10 ----------------------------------------

/// Exact x - 1 for finite x.
fn minus_one(a: &Dec) -> Dec {
    let mut c = Ctx::max_ctx();
    let (d, _) = ops::add_exact(a, a.neg, &Dec::one(), true, &c);
    c.status = 0;
    d
}

/// Number of extra fractional digits needed when ln(a) is close to zero.
fn ln_extra_digits(a: &Dec) -> u64 {
    let adj = a.adjusted();
    if adj == 0 || adj == -1 {
        let d = minus_one(a);
        if d.is_zero() {
            return 0;
        }
        let da = d.adjusted();
        if da < 0 {
            return (-da) as u64;
        }
    }
    0
}

/// ln(a) as fixed point with w fractional digits (a finite positive).
fn ln_value(a: &Dec, w: u64) -> SInt {
    let t = a.adjusted();
    let dg = digits_i128(t as i128);
    let w2 = w + dg + 4;
    // m = a / 10^t scaled by 10^w2
    let nd = nat::digits(&a.coef) as i64;
    let sh = w2 as i64 - (nd - 1);
    let mm = if sh >= 0 { nat::shl(&a.coef, sh as u64) } else { nat::shr_trunc(&a.coef, (-sh) as u64) };
    let l1 = ln_fixed(&mm, w2);
    let l2 = SInt::new(t < 0, nat::mul(&ln10(w2), &nat::from_u64(t.unsigned_abs())));
    l1.add(&l2).shr(w2 - w)
}

pub fn ln(a: &Dec, ctx: &mut Ctx) -> Dec {
    log_common(a, ctx, false)
}

pub fn log10(a: &Dec, ctx: &mut Ctx) -> Dec {
    log_common(a, ctx, true)
}

fn log_common(a: &Dec, ctx: &mut Ctx, base10: bool) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_zero() {
        return Dec::inf(true);
    }
    if a.neg {
        return invalid(ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    let mut c = ctx.clone();
    c.round = Round::HalfEven;
    c.status = 0;
    let r = log_inner(a, &mut c, base10);
    ctx.status |= c.status;
    r
}

fn log_inner(a: &Dec, ctx: &mut Ctx, base10: bool) -> Dec {
    // exact cases
    let tz = nat::trailing_zeros(&a.coef);
    let stripped_is_one = nat::shr_trunc(&a.coef, tz) == [1];
    if stripped_is_one {
        let t = a.adjusted();
        if t == 0 {
            return finalize(Dec::zero(), ctx, Rest::Zero);
        }
        if base10 {
            return finalize(Dec::from_i64(t), ctx, Rest::Zero);
        }
    }
    let p = ctx.prec as u64;
    let extra = ln_extra_digits(a);
    let mut w = p + extra + 10;
    loop {
        let mut v = ln_value(a, w + 3);
        let mut k = 3u64;
        if base10 {
            let w3 = w + 3;
            let l10 = ln10(w3 + 2);
            v = SInt::new(v.neg, nat::divrem(&nat::shl(&v.mag, w3 + 2), &l10).0);
            k = 4;
        }
        let v = v.shr(3);
        if let Some(d) = ziv(&v.mag, k, -(w as i64), v.neg, ctx) {
            return d;
        }
        w *= 2;
    }
}

// ------------------------------- power ----------------------------------------

fn is_odd_integer(y: &Dec) -> bool {
    if !y.is_integer() || y.exp > 0 || y.coef.is_empty() {
        return false;
    }
    nat::digit_at(&y.coef, (-y.exp) as u64) % 2 == 1
}

/// Integer value of an integral Dec if it fits in i128.
fn int_value(y: &Dec) -> Option<i128> {
    if y.coef.is_empty() {
        return Some(0);
    }
    let c = if y.exp >= 0 {
        if y.exp > 40 {
            return None;
        }
        nat::shl(&y.coef, y.exp as u64)
    } else {
        nat::shr_trunc(&y.coef, (-y.exp) as u64)
    };
    let v = nat::to_u128(&c)?;
    if v > i128::MAX as u128 {
        return None;
    }
    Some(if y.neg { -(v as i128) } else { v as i128 })
}

pub fn power(x: &Dec, y: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(x, y, ctx) {
        return r;
    }
    let y_int = y.is_finite() && y.is_integer();
    let y_odd = y_int && is_odd_integer(y);
    if y.is_zero() {
        if x.is_zero() {
            return invalid(ctx);
        }
        return finalize(Dec::one(), ctx, Rest::Zero);
    }
    if x.is_zero() {
        let neg = x.neg && y_odd;
        return if y.neg { Dec::inf(neg) } else { Dec::finite(neg, vec![], 0) };
    }
    if x.neg && !y_int {
        return invalid(ctx);
    }
    if x.is_inf() {
        let neg = x.neg && y_odd;
        return if y.neg { Dec::finite(neg, vec![], 0) } else { Dec::inf(neg) };
    }
    if y.is_inf() {
        let one = Dec::one();
        let c = ops::cmp_abs(x, &one);
        return match c {
            Ordering::Less => {
                if y.neg { Dec::inf(false) } else { Dec::finite(false, vec![], 0) }
            }
            Ordering::Greater => {
                if y.neg { Dec::finite(false, vec![], 0) } else { Dec::inf(false) }
            }
            Ordering::Equal => {
                let p = ctx.prec as u64;
                ctx.status |= INEXACT | ROUNDED;
                Dec::finite(false, nat::pow10(p - 1), -(p as i64 - 1))
            }
        };
    }
    let neg = x.neg && y_odd;
    let ax = x.abs();
    let prec = ctx.prec as i128;
    // strip trailing zeros of x
    let tz = nat::trailing_zeros(&ax.coef);
    let xc = nat::shr_trunc(&ax.coef, tz);
    let xe = ax.exp as i128 + tz as i128;
    let log10_xc = {
        let (m, e) = nat_f64(&xc);
        m.log10() + e as f64
    };
    if y_int {
        if let Some(n) = int_value(y) {
            let est = log10_xc * n.unsigned_abs() as f64;
            if est <= (3 * prec + 1000) as f64 {
                let r = int_power(x, &xc, xe, tz, n, neg, ctx);
                return r;
            }
        }
    } else if let Some(ex) = power_exact(&xc, xe, y, prec as u64 + 1) {
        // exact result of a non-integral power: pad, round, and signal inexact
        let mut ex = ex;
        let nd = nat::digits(&ex.coef) as i128;
        if nd <= prec {
            let pad = (prec + 1 - nd) as u64;
            ex.coef = nat::shl(&ex.coef, pad).into();
            ex.exp = ops::clamp_i64(ex.exp as i128 - pad as i128);
        }
        let mut c = ctx.clone();
        c.status = 0;
        let r = finalize(ex, &mut c, Rest::Zero);
        let mut st = c.status | INEXACT | ROUNDED;
        if st & SUBNORMAL != 0 {
            st |= UNDERFLOW;
        }
        ctx.status |= st;
        return r;
    }
    power_general(&ax, y, neg, ctx)
}

/// x^n for integer n (exact computation, then rounding / division).
fn int_power(x: &Dec, xc: &[u64], xe: i128, tz: u64, n: i128, neg: bool, ctx: &mut Ctx) -> Dec {
    let m = n.unsigned_abs();
    let pc = nat::pow(xc, m as u64);
    // full coefficient is pc * 10^(tz*m) with exponent x.exp*m
    let zeros = tz as i128 * m as i128;
    let pd = nat::digits(&pc) as i128;
    let prec = ctx.prec as i128;
    let mut exp = x.exp as i128 * m as i128;
    let keep = if zeros > 0 {
        // never need more zeros than to exceed the precision
        let cap = (prec + 2 - pd).max(0);
        if zeros > cap {
            exp += zeros - cap;
            cap
        } else {
            zeros
        }
    } else {
        0
    };
    let _ = xe;
    let coef = nat::shl(&pc, keep as u64);
    let limit = 4_000_000_000_000_000_000i128;
    let exp = exp.clamp(-limit, limit) as i64;
    if n > 0 {
        finalize(Dec::finite(neg, coef, exp), ctx, Rest::Zero)
    } else {
        let p = Dec::finite(neg, coef, exp);
        ops::div(&Dec::one(), &p, ctx)
    }
}

/// Exact integer k-th root of a (k = 2 or 5), if a is a perfect power.
fn exact_root(a: &[u64], k: u32) -> Option<Vec<u64>> {
    if k == 2 {
        let r = nat::isqrt(a);
        return if nat::mul(&r, &r) == a { Some(r) } else { None };
    }
    // k == 5: Newton from a float estimate, then verify
    let (m, e) = nat_f64(a);
    let l = (m.log10() + e as f64) / 5.0;
    let est_digits = l.floor() as i64;
    let mut r: Vec<u64>;
    if est_digits < 15 {
        r = nat::from_u128(10f64.powf(l).round() as u128);
    } else {
        let lead = 10f64.powf(l - (est_digits - 14) as f64);
        r = nat::shl(&nat::from_u128(lead as u128), (est_digits - 14) as u64);
    }
    // Newton: r = (4r + a / r^4) / 5
    for _ in 0..200 {
        if r.is_empty() {
            r = vec![1];
        }
        let r4 = nat::pow(&r, 4);
        let q = nat::divrem(a, &r4).0;
        let nr = nat::divrem_small(&nat::add(&nat::mul_small(&r, 4), &q), 5).0;
        if nr == r {
            break;
        }
        // stop oscillation
        let diff = if nat::cmp(&nr, &r) == Ordering::Greater { nat::sub(&nr, &r) } else { nat::sub(&r, &nr) };
        r = nr;
        if diff == [1] {
            break;
        }
    }
    for cand in [nat::sub(&r, &[1]), r.clone(), nat::add(&r, &[1])] {
        if nat::pow(&cand, 5) == a {
            return Some(cand);
        }
    }
    None
}

/// x^y exactly (x = xc * 10^xe > 0 with xc not divisible by 10, y non-integral),
/// if representable with at most `maxdig` digits.
fn power_exact(xc: &[u64], xe: i128, y: &Dec, maxdig: u64) -> Option<Dec> {
    // y = p / q in lowest terms, q = 2^a 5^b
    let ytz = nat::trailing_zeros(&y.coef);
    let yc = nat::shr_trunc(&y.coef, ytz);
    let ye = y.exp as i128 + ytz as i128; // < 0
    if ye >= 0 {
        return None;
    }
    if xc == [1] && xe == 0 {
        return Some(Dec::finite(false, vec![1], 0));
    }
    let k = -ye; // y = yc / 10^k
    if k > 4000 {
        return None;
    }
    let k = k as u64;
    // remove common factors 2 and 5
    let mut p = yc;
    let mut twos = k; // q = 2^twos * 5^fives
    let mut fives = k;
    while twos > 0 && nat::rem_small(&p, 2) == 0 {
        p = nat::divrem_small(&p, 2).0;
        twos -= 1;
    }
    while fives > 0 && nat::rem_small(&p, 5) == 0 {
        p = nat::divrem_small(&p, 5).0;
        fives -= 1;
    }
    // q-th root of x: need q | xe and xc a perfect q-th power
    let q_log10 = twos as f64 * 2f64.log10() + fives as f64 * 5f64.log10();
    let xc_digits = nat::digits(xc) as f64;
    let mut root = xc.to_vec();
    if q_log10 > 30.0 || 10f64.powf(q_log10) > xc_digits * 3.33 + 1.0 {
        // root would be < 2 unless xc == 1
        if xc != [1] {
            return None;
        }
    } else {
        for _ in 0..twos {
            root = exact_root(&root, 2)?;
        }
        for _ in 0..fives {
            root = exact_root(&root, 5)?;
        }
    }
    // exponent: xe / q must be an integer
    let q: i128 = if q_log10 > 30.0 { return None } else { 2i128.pow(twos as u32) * 5i128.pow(fives as u32) };
    if xe % q != 0 {
        return None;
    }
    let re = xe / q;
    // result = root^p * 10^(re*p), p signed
    let pv = nat::to_u128(&p)?;
    if pv > 1_000_000_000_000 {
        if root != [1] {
            return None;
        }
    }
    let pneg = y.neg;
    let root_log = {
        let (m, e) = nat_f64(&root);
        m.log10() + e as f64
    };
    if root == [1] {
        let e = re.checked_mul(pv as i128)?;
        let e = if pneg { -e } else { e };
        if e.abs() > 4_000_000_000_000_000_000 {
            return None;
        }
        return Some(Dec::finite(false, vec![1], e as i64));
    }
    if !pneg {
        if root_log * pv as f64 > maxdig as f64 + 1.0 {
            return None;
        }
        let c = nat::pow(&root, pv as u64);
        if nat::digits(&c) > maxdig {
            return None;
        }
        let e = re * pv as i128;
        return Some(Dec::finite(false, c, e as i64));
    }
    // negative power: 1 / root^p; terminates only for root = 2^i or 5^j
    let mut r = root.clone();
    let mut i2 = 0u64;
    while nat::rem_small(&r, 2) == 0 {
        r = nat::divrem_small(&r, 2).0;
        i2 += 1;
    }
    let mut j5 = 0u64;
    while nat::rem_small(&r, 5) == 0 {
        r = nat::divrem_small(&r, 5).0;
        j5 += 1;
    }
    if r != [1] {
        return None;
    }
    // 1/2^i = 5^i / 10^i ; 1/5^j = 2^j / 10^j
    let (base, cnt) = if i2 > 0 { (5u64, i2) } else { (2u64, j5) };
    let tot = (cnt as u128).checked_mul(pv)?;
    if (tot as f64) * (base as f64).log10() > maxdig as f64 + 1.0 {
        return None;
    }
    let c = nat::pow(&[base], tot as u64);
    if nat::digits(&c) > maxdig {
        return None;
    }
    let e = -(re * pv as i128) - tot as i128;
    Some(Dec::finite(false, c, e as i64))
}

/// |x|^y via exp(y ln x) for x > 0 finite, y finite (non-trivial cases).
fn power_general(x: &Dec, y: &Dec, neg: bool, ctx: &mut Ctx) -> Dec {
    let p = ctx.prec;
    // magnitude of t = y * ln x as (mantissa, exp10)
    let (lm, le) = {
        let adj = x.adjusted();
        if adj == 0 || adj == -1 {
            let d = minus_one(x);
            if d.is_zero() {
                (0.0, 0)
            } else {
                // ln x ~ d (within a factor 2.3 for |d| < 9)
                let (m, e) = dec_f64(&d);
                if d.adjusted() >= -1 {
                    let xf = to_f64_clamped(dec_f64(x).0, dec_f64(x).1);
                    (xf.ln(), 0)
                } else {
                    (m, e)
                }
            }
        } else {
            (log10_abs(x) * std::f64::consts::LN_10, 0)
        }
    };
    if lm == 0.0 {
        // x == 1 exactly (non-integral y handled by power_exact; integral by int_power)
        return finalize(Dec::one(), ctx, Rest::Zero);
    }
    let (ym, ye) = dec_f64(y);
    let tm = lm * ym; // t = tm * 10^(le+ye)
    let te = le + ye;
    let t_log10 = tm.abs().log10() + te as f64;
    if t_log10 < -(p as f64 + 4.0) {
        return near_one(tm > 0.0, ctx).with_sign(neg);
    }
    if t_log10 > 21.0 {
        return overflow_or_underflow(tm > 0.0, neg, ctx);
    }
    let tf = tm * 10f64.powi(te as i32);
    let n_est = tf / std::f64::consts::LN_10;
    if n_est > ctx.emax as f64 + 2.0 {
        return overflow_or_underflow(true, neg, ctx);
    }
    if n_est < ctx.etiny() as f64 - 2.0 {
        return overflow_or_underflow(false, neg, ctx);
    }
    let n = n_est.floor() as i64;
    let yadj = y.adjusted().max(0) as u64;
    let extra = if t_log10 < 0.0 { (-t_log10).ceil() as u64 } else { 0 };
    let mut w = p as u64 + 10;
    loop {
        let w2 = w + digits_i128(n as i128) + 4;
        // ln x with enough digits that y*ln x is accurate to 10^-w2
        let wl = w2 + yadj + 2 + extra;
        let l = ln_value(x, wl);
        // t = l * y : l scaled 10^wl, y = yc*10^ye
        let ty = SInt::new(l.neg ^ y.neg, nat::mul(&l.mag, &y.coef));
        let shift = y.exp as i128 + w2 as i128 - wl as i128; // scale to 10^w2
        let t = if shift >= 0 { ty.shl(shift as u64) } else { ty.shr((-shift) as u64) };
        let ln10n = SInt::new(n < 0, nat::mul(&ln10(w2), &nat::from_u64(n.unsigned_abs())));
        let r = t.sub(&ln10n);
        if r.mag.len() as u64 > (w2 / 19 + 3) {
            // reduction estimate failed badly; should not happen
            return overflow_or_underflow(!r.neg, neg, ctx);
        }
        let ev = nat::shr_trunc(&exp_fixed(&r, w2), w2 - w);
        if let Some(d) = ziv(&ev, 5, n - w as i64, neg, ctx) {
            return d;
        }
        w *= 2;
    }
}

/// pow(a, b, m) for integers (Python's three-argument pow).
pub fn power_mod(a: &Dec, b: &Dec, m: &Dec, ctx: &mut Ctx) -> Dec {
    if a.is_nan() || b.is_nan() || m.is_nan() {
        return ops::nan3(a, b, m, ctx).unwrap();
    }
    if !(a.is_integer() && b.is_integer() && m.is_integer()) {
        return invalid(ctx);
    }
    if b.neg && !b.is_zero() {
        return invalid(ctx);
    }
    if m.is_zero() {
        return invalid(ctx);
    }
    // modulus must fit in precision
    let mint = |d: &Dec| -> Vec<u64> {
        if d.exp >= 0 { nat::shl(&d.coef, d.exp as u64) } else { nat::shr_trunc(&d.coef, (-d.exp) as u64) }
    };
    let mm = mint(m);
    if nat::digits(&mm) as i64 > ctx.prec {
        return invalid(ctx);
    }
    if a.is_zero() && b.is_zero() {
        return invalid(ctx);
    }
    let neg = a.neg && is_odd_integer(b);
    // base reduced mod m first (exponent of a may be large)
    let base = if a.exp >= 0 {
        // a = c * 10^e mod m = c * (10^e mod m)
        let t = powmod_nat(&[10], &nat::from_u64(a.exp as u64), &mm);
        nat::divrem(&nat::mul(&nat::divrem(&a.coef, &mm).1, &t), &mm).1
    } else {
        nat::divrem(&mint(a), &mm).1
    };
    let e = if b.exp >= 0 {
        if b.exp > 1000 {
            // huge exponent: compute via repeated exponentiation of base^(c) then ^(10^e)
            let mut r = powmod_nat(&base, &b.coef, &mm);
            for _ in 0..b.exp {
                r = powmod_nat(&r, &[10], &mm);
            }
            return Dec::finite(neg, r, 0);
        }
        nat::shl(&b.coef, b.exp as u64)
    } else {
        nat::shr_trunc(&b.coef, (-b.exp) as u64)
    };
    let r = powmod_nat(&base, &e, &mm);
    Dec::finite(neg, r, 0)
}

fn powmod_nat(base: &[u64], e: &[u64], m: &[u64]) -> Vec<u64> {
    let mut result: Vec<u64> = nat::divrem(&[1], m).1;
    if e.is_empty() {
        return result;
    }
    // process exponent digits from the most significant: binary via repeated halving
    let mut bits: Vec<bool> = Vec::new();
    let mut ee = e.to_vec();
    while !ee.is_empty() {
        let (q, r) = nat::divrem_small(&ee, 2);
        bits.push(r == 1);
        ee = q;
    }
    let b = nat::divrem(base, m).1;
    for bit in bits.iter().rev() {
        result = nat::divrem(&nat::sqr(&result), m).1;
        if *bit {
            result = nat::divrem(&nat::mul(&result, &b), m).1;
        }
    }
    result
}
