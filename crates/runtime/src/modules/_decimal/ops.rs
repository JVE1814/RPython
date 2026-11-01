//! Arithmetic, comparison, quantization and digit-wise operations.

use super::dec::*;
use super::nat::{self, Rest};
use std::cmp::Ordering;

// ------------------------------- NaN helpers ----------------------------------------

pub fn invalid(ctx: &mut Ctx) -> Dec {
    ctx.status |= INVALID_OPERATION;
    Dec::nan()
}

fn quiet(d: &Dec, ctx: &Ctx) -> Dec {
    let mut r = d.clone();
    r.kind = Kind::NaN;
    fix_nan(r, ctx)
}

/// NaN result for a one-operand operation.
pub fn nan1(a: &Dec, ctx: &mut Ctx) -> Option<Dec> {
    if a.is_snan() {
        ctx.status |= INVALID_OPERATION;
        Some(quiet(a, ctx))
    } else if a.is_qnan() {
        Some(quiet(a, ctx))
    } else {
        None
    }
}

/// NaN result for a two-operand operation (sNaN takes precedence, then first operand).
pub fn nan2(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Option<Dec> {
    if !a.is_nan() && !b.is_nan() {
        return None;
    }
    if a.is_snan() {
        ctx.status |= INVALID_OPERATION;
        return Some(quiet(a, ctx));
    }
    if b.is_snan() {
        ctx.status |= INVALID_OPERATION;
        return Some(quiet(b, ctx));
    }
    Some(quiet(if a.is_nan() { a } else { b }, ctx))
}

pub fn nan3(a: &Dec, b: &Dec, c: &Dec, ctx: &mut Ctx) -> Option<Dec> {
    for x in [a, b, c] {
        if x.is_snan() {
            ctx.status |= INVALID_OPERATION;
            return Some(quiet(x, ctx));
        }
    }
    for x in [a, b, c] {
        if x.is_qnan() {
            return Some(quiet(x, ctx));
        }
    }
    None
}

// ------------------------------- unary ----------------------------------------

/// Convert/round an operand to the context (create_decimal / _apply semantics).
pub fn apply(a: &Dec, ctx: &mut Ctx) -> Dec {
    if a.is_nan() {
        return fix_nan(a.clone(), ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    finalize(a.clone(), ctx, Rest::Zero)
}

pub fn plus(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        return a.clone();
    }
    let mut r = a.clone();
    if r.is_zero() && r.neg {
        r.neg = ctx.round == Round::Floor;
    }
    finalize(r, ctx, Rest::Zero)
}

pub fn minus(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        return a.negated();
    }
    let mut r = a.negated();
    if r.is_zero() {
        r.neg = ctx.round == Round::Floor && r.neg;
        if a.is_zero() && !a.neg {
            // 0 - 0: signs differ
            r.neg = ctx.round == Round::Floor;
        }
    }
    finalize(r, ctx, Rest::Zero)
}

pub fn abs(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.neg { minus(a, ctx) } else { plus(a, ctx) }
}

// ------------------------------- add / sub ----------------------------------------

pub fn add(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    add_signed(a, b, b.neg, ctx)
}

pub fn sub(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    add_signed(a, b, !b.neg, ctx)
}

/// a + b where b's sign is taken as `bneg`.
pub fn add_signed(a: &Dec, b: &Dec, bneg: bool, ctx: &mut Ctx) -> Dec {
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() {
            if b.is_inf() && a.neg != bneg {
                return invalid(ctx);
            }
            return Dec::inf(a.neg);
        }
        return Dec::inf(bneg);
    }
    let (r, rest) = add_exact(a, a.neg, b, bneg, ctx);
    finalize(r, ctx, rest)
}

/// Exact (or sticky-reduced) sum of two finite numbers; the result is ready for
/// finalize under `ctx` (sticky reduction only uses ctx.prec).
pub fn add_exact(a: &Dec, aneg: bool, b: &Dec, bneg: bool, ctx: &Ctx) -> (Dec, Rest) {
    // fast path: small aligned values
    if a.exp == b.exp {
        if let (Some(x), Some(y)) = (nat::to_u128(&a.coef), nat::to_u128(&b.coef)) {
            if x < (1u128 << 126) && y < (1u128 << 126) {
                return (sum_small(x, aneg, y, bneg, a.exp, ctx), Rest::Zero);
            }
        }
    }
    let (hi, hneg, lo, lneg) = if a.exp >= b.exp { (a, aneg, b, bneg) } else { (b, bneg, a, aneg) };
    let diff = (hi.exp as i128) - (lo.exp as i128);
    if hi.coef.is_empty() {
        // only lo contributes a coefficient; exponent is lo's
        let mut r = Dec::finite(lneg, lo.coef.clone(), lo.exp);
        if lo.coef.is_empty() {
            r.neg = zero_sum_sign(hneg, lneg, ctx);
        }
        return (r, Rest::Zero);
    }
    let dh = nat::digits(&hi.coef) as i128;
    let prec = ctx.prec as i128;
    let mut lo_coef: std::borrow::Cow<'_, [u64]> = std::borrow::Cow::Borrowed(&lo.coef);
    let mut lo_exp = lo.exp as i128;
    let kpos = (hi.exp as i128).min(hi.exp as i128 + dh - prec) - 2;
    if lo.coef.is_empty() {
        if diff > dh.max(prec) + 3 {
            lo_exp = hi.exp as i128 - (dh.max(prec) + 3);
        }
    } else {
        let adj_lo = lo.exp as i128 + nat::digits(&lo.coef) as i128 - 1;
        if adj_lo < kpos && lo_exp < kpos {
            lo_coef = std::borrow::Cow::Owned(vec![1]);
            lo_exp = kpos;
        }
    }
    let shift = (hi.exp as i128 - lo_exp) as u64;
    if let (Some(x), Some(y)) = (nat::to_u128(&hi.coef), nat::to_u128(&lo_coef)) {
        if shift < 38 {
            let p = 10u128.pow(shift as u32);
            if let Some(xs) = x.checked_mul(p) {
                if xs < (1u128 << 126) && y < (1u128 << 126) {
                    return (sum_small(xs, hneg, y, lneg, lo_exp as i64, ctx), Rest::Zero);
                }
            }
        }
    }
    let hc = nat::shl(&hi.coef, shift);
    let exp = lo_exp as i64;
    if hneg == lneg {
        (Dec::finite(hneg, nat::add(&hc, &lo_coef), exp), Rest::Zero)
    } else {
        match nat::cmp(&hc, &lo_coef) {
            Ordering::Greater => (Dec::finite(hneg, nat::sub(&hc, &lo_coef), exp), Rest::Zero),
            Ordering::Less => (Dec::finite(lneg, nat::sub(&lo_coef, &hc), exp), Rest::Zero),
            Ordering::Equal => (Dec::finite(zero_sum_sign(hneg, lneg, ctx), vec![], exp), Rest::Zero),
        }
    }
}

#[inline]
fn zero_sum_sign(an: bool, bn: bool, ctx: &Ctx) -> bool {
    if an == bn { an } else { ctx.round == Round::Floor }
}

#[inline]
fn sum_small(x: u128, xn: bool, y: u128, yn: bool, exp: i64, ctx: &Ctx) -> Dec {
    if xn == yn {
        Dec::finite(xn, Coef::from_u128(x + y), exp)
    } else if x > y {
        Dec::finite(xn, Coef::from_u128(x - y), exp)
    } else if y > x {
        Dec::finite(yn, Coef::from_u128(y - x), exp)
    } else {
        Dec::finite(zero_sum_sign(xn, yn, ctx), vec![], exp)
    }
}

// ------------------------------- multiply ----------------------------------------

pub fn mul(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    let neg = a.neg ^ b.neg;
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if (a.is_inf() && b.is_zero()) || (b.is_inf() && a.is_zero()) {
            return invalid(ctx);
        }
        return Dec::inf(neg);
    }
    let r = mul_exact(a, b);
    finalize(r, ctx, Rest::Zero)
}

pub fn mul_exact(a: &Dec, b: &Dec) -> Dec {
    let neg = a.neg ^ b.neg;
    let exp = a.exp + b.exp;
    if a.coef.len() == 1 && b.coef.len() == 1 {
        let p = a.coef[0] as u128 * b.coef[0] as u128;
        return Dec::finite(neg, Coef::from_u128(p), exp);
    }
    Dec::finite(neg, nat::mul(&a.coef, &b.coef), exp)
}

// ------------------------------- divide ----------------------------------------

pub fn div(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    let neg = a.neg ^ b.neg;
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() {
            if b.is_inf() {
                return invalid(ctx);
            }
            return Dec::inf(neg);
        }
        // b infinite
        ctx.status |= CLAMPED;
        return Dec::finite(neg, vec![], ctx.etiny());
    }
    if b.is_zero() {
        if a.is_zero() {
            ctx.status |= DIVISION_UNDEFINED;
            return Dec::nan();
        }
        ctx.status |= DIVISION_BY_ZERO;
        return Dec::inf(neg);
    }
    let ideal = a.exp as i128 - b.exp as i128;
    if a.is_zero() {
        return finalize(Dec::finite(neg, vec![], clamp_i64(ideal)), ctx, Rest::Zero);
    }
    let (q, exp, rest) = div_coef(&a.coef, &b.coef, ideal, ctx.prec);
    finalize(Dec::finite(neg, q, clamp_i64(exp)), ctx, rest)
}

pub fn clamp_i64(v: i128) -> i64 {
    v.clamp(i64::MIN as i128 / 2, i64::MAX as i128 / 2) as i64
}

/// Quotient of coefficients with at least `prec` digits; exact results are
/// brought as close as possible to the ideal exponent.
pub fn div_coef(ca: &[u64], cb: &[u64], ideal: i128, prec: i64) -> (Coef, i128, Rest) {
    let da = nat::digits(ca) as i128;
    let db = nat::digits(cb) as i128;
    let shift = prec as i128 + db - da;
    // fast path
    if shift >= 0 && shift <= 19 {
        if let (Some(x), Some(y)) = (nat::to_u64(ca), nat::to_u64(cb)) {
            let xs = x as u128 * 10u128.pow(shift as u32);
            let (q, r) = (xs / y as u128, xs % y as u128);
            let mut exp = ideal - shift;
            let mut q = q;
            let rest = if r == 0 {
                while exp < ideal && q % 10 == 0 {
                    q /= 10;
                    exp += 1;
                }
                Rest::Zero
            } else {
                match (2 * r).cmp(&(y as u128)) {
                    Ordering::Less => Rest::Lt,
                    Ordering::Equal => Rest::Half,
                    Ordering::Greater => Rest::Gt,
                }
            };
            return (Coef::from_u128(q), exp, rest);
        }
    }
    let (num, den) = if shift > 0 {
        (nat::shl(ca, shift as u64), std::borrow::Cow::Borrowed(cb))
    } else {
        (ca.to_vec(), std::borrow::Cow::Owned(nat::shl(cb, (-shift) as u64)))
    };
    let (mut q, r) = nat::divrem(&num, &den);
    let mut exp = ideal - shift;
    let rest = if r.is_empty() {
        if exp < ideal {
            let tz = nat::trailing_zeros(&q) as i128;
            let k = tz.min(ideal - exp);
            if k > 0 {
                q = nat::shr_trunc(&q, k as u64);
                exp += k;
            }
        }
        Rest::Zero
    } else {
        half_cmp(&r, &den)
    };
    (q.into(), exp, rest)
}

/// Classify remainder r relative to divisor d.
pub fn half_cmp(r: &[u64], d: &[u64]) -> Rest {
    if r.is_empty() {
        return Rest::Zero;
    }
    let r2 = nat::mul_small(r, 2);
    match nat::cmp(&r2, d) {
        Ordering::Less => Rest::Lt,
        Ordering::Equal => Rest::Half,
        Ordering::Greater => Rest::Gt,
    }
}

/// Integer division core: returns (q, r, exp_r) with |a| = q*|b| + r*10^exp_r,
/// or Err(()) if the quotient needs more than prec digits.
fn int_divrem(a: &Dec, b: &Dec, prec: i64) -> Result<(Vec<u64>, Vec<u64>, i64), ()> {
    let e = a.exp.min(b.exp);
    let adj_a = a.adjusted() as i128;
    let adj_b = b.adjusted() as i128;
    if a.coef.is_empty() || adj_a < adj_b {
        // |a| < |b|
        let r = nat::shl(&a.coef, (a.exp - e) as u64);
        return Ok((vec![], r, e));
    }
    if adj_a - adj_b > prec as i128 {
        return Err(());
    }
    let ac = nat::shl(&a.coef, (a.exp - e) as u64);
    let bc = nat::shl(&b.coef, (b.exp - e) as u64);
    let (q, r) = nat::divrem(&ac, &bc);
    if nat::digits(&q) as i128 > prec as i128 {
        return Err(());
    }
    Ok((q, r, e))
}

pub fn divint(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    let neg = a.neg ^ b.neg;
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() {
            if b.is_inf() {
                return invalid(ctx);
            }
            return Dec::inf(neg);
        }
        return Dec::finite(neg, vec![], 0);
    }
    if b.is_zero() {
        if a.is_zero() {
            ctx.status |= DIVISION_UNDEFINED;
            return Dec::nan();
        }
        ctx.status |= DIVISION_BY_ZERO;
        return Dec::inf(neg);
    }
    match int_divrem(a, b, ctx.prec) {
        Ok((q, _, _)) => finalize(Dec::finite(neg, q, 0), ctx, Rest::Zero),
        Err(()) => {
            ctx.status |= DIVISION_IMPOSSIBLE;
            Dec::nan()
        }
    }
}

pub fn rem(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() {
            return invalid(ctx);
        }
        return finalize(a.clone(), ctx, Rest::Zero);
    }
    if b.is_zero() {
        if a.is_zero() {
            ctx.status |= DIVISION_UNDEFINED;
        } else {
            ctx.status |= INVALID_OPERATION;
        }
        return Dec::nan();
    }
    match int_divrem(a, b, ctx.prec) {
        Ok((_, r, e)) => finalize(Dec::finite(a.neg, r, e), ctx, Rest::Zero),
        Err(()) => {
            ctx.status |= DIVISION_IMPOSSIBLE;
            Dec::nan()
        }
    }
}

pub fn rem_near(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() {
            return invalid(ctx);
        }
        return finalize(a.clone(), ctx, Rest::Zero);
    }
    if b.is_zero() {
        if a.is_zero() {
            ctx.status |= DIVISION_UNDEFINED;
        } else {
            ctx.status |= INVALID_OPERATION;
        }
        return Dec::nan();
    }
    let e = a.exp.min(b.exp);
    if !a.coef.is_empty() && (a.adjusted() as i128) < b.adjusted() as i128 - 1 {
        let r = nat::shl(&a.coef, (a.exp - e) as u64);
        return finalize(Dec::finite(a.neg, r, e), ctx, Rest::Zero);
    }
    let (q, r, e) = match int_divrem(a, b, ctx.prec) {
        Ok(x) => x,
        Err(()) => {
            ctx.status |= DIVISION_IMPOSSIBLE;
            return Dec::nan();
        }
    };
    let bc = nat::shl(&b.coef, (b.exp - e) as u64);
    let h = half_cmp(&r, &bc);
    let odd = !q.is_empty() && q[0] % 2 == 1;
    let (mut q, mut r, mut neg) = (q, r, a.neg);
    if h == Rest::Gt || (h == Rest::Half && odd) {
        r = nat::sub(&bc, &r);
        neg = !neg;
        nat::incr(&mut q);
    }
    if nat::digits(&q) as i128 > ctx.prec as i128 {
        ctx.status |= DIVISION_IMPOSSIBLE;
        return Dec::nan();
    }
    if r.is_empty() {
        neg = a.neg;
    }
    finalize(Dec::finite(neg, r, e), ctx, Rest::Zero)
}

/// Python divmod: (a // b, a % b) with the decimal (truncating) semantics.
pub fn divmod(a: &Dec, b: &Dec, ctx: &mut Ctx) -> (Dec, Dec) {
    let neg = a.neg ^ b.neg;
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return (r.clone(), r);
        }
        if a.is_inf() {
            if b.is_inf() {
                let n = invalid(ctx);
                return (n.clone(), n);
            }
            return (Dec::inf(neg), invalid(ctx));
        }
        // b infinite
        return (Dec::finite(neg, vec![], 0), finalize(a.clone(), ctx, Rest::Zero));
    }
    if b.is_zero() {
        if a.is_zero() {
            ctx.status |= DIVISION_UNDEFINED;
            return (Dec::nan(), Dec::nan());
        }
        ctx.status |= DIVISION_BY_ZERO | INVALID_OPERATION;
        return (Dec::inf(neg), Dec::nan());
    }
    match int_divrem(a, b, ctx.prec) {
        Ok((q, r, e)) => {
            let qd = finalize(Dec::finite(neg, q, 0), ctx, Rest::Zero);
            let rd = finalize(Dec::finite(a.neg, r, e), ctx, Rest::Zero);
            (qd, rd)
        }
        Err(()) => {
            ctx.status |= DIVISION_IMPOSSIBLE;
            (Dec::nan(), Dec::nan())
        }
    }
}

// ------------------------------- fma ----------------------------------------

pub fn fma(a: &Dec, b: &Dec, c: &Dec, ctx: &mut Ctx) -> Dec {
    // multiplication part (exact)
    let prod = if a.is_special() || b.is_special() {
        if a.is_snan() || b.is_snan() {
            return nan2(a, b, ctx).unwrap();
        }
        if a.is_nan() || b.is_nan() {
            if c.is_snan() {
                ctx.status |= INVALID_OPERATION;
                return quiet(c, ctx);
            }
            return nan2(a, b, ctx).unwrap();
        }
        if (a.is_inf() && b.is_zero()) || (b.is_inf() && a.is_zero()) {
            return invalid(ctx);
        }
        Dec::inf(a.neg ^ b.neg)
    } else {
        mul_exact(a, b)
    };
    add(&prod, c, ctx)
}

// ------------------------------- comparison ----------------------------------------

/// |a| vs |b| for finite numbers.
pub fn cmp_abs(a: &Dec, b: &Dec) -> Ordering {
    match (a.coef.is_empty(), b.coef.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let (aa, ba) = (a.adjusted(), b.adjusted());
    if aa != ba {
        return aa.cmp(&ba);
    }
    if a.exp == b.exp {
        return nat::cmp(&a.coef, &b.coef);
    }
    if a.exp > b.exp {
        nat::cmp(&nat::shl(&a.coef, (a.exp - b.exp) as u64), &b.coef)
    } else {
        nat::cmp(&a.coef, &nat::shl(&b.coef, (b.exp - a.exp) as u64))
    }
}

/// Numeric comparison of non-NaN values.
pub fn cmp_num(a: &Dec, b: &Dec) -> Ordering {
    let sa = sign_class(a);
    let sb = sign_class(b);
    if sa != sb {
        return sa.cmp(&sb);
    }
    if sa == 0 {
        return Ordering::Equal;
    }
    let m = match (a.is_inf(), b.is_inf()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => cmp_abs(a, b),
    };
    if sa < 0 { m.reverse() } else { m }
}

fn sign_class(a: &Dec) -> i32 {
    if a.is_zero() {
        0
    } else if a.neg {
        -1
    } else {
        1
    }
}

fn ord_dec(o: Ordering) -> Dec {
    match o {
        Ordering::Less => Dec::from_i64(-1),
        Ordering::Equal => Dec::zero(),
        Ordering::Greater => Dec::one(),
    }
}

pub fn compare(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(a, b, ctx) {
        return r;
    }
    ord_dec(cmp_num(a, b))
}

pub fn compare_signal(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if a.is_nan() || b.is_nan() {
        ctx.status |= INVALID_OPERATION;
        return nan2(a, b, ctx).unwrap();
    }
    ord_dec(cmp_num(a, b))
}

fn total_rank(a: &Dec) -> u8 {
    match a.kind {
        Kind::Finite => 0,
        Kind::Inf => 1,
        Kind::SNaN => 2,
        Kind::NaN => 3,
    }
}

pub fn total_cmp_abs(a: &Dec, b: &Dec) -> Ordering {
    let (ra, rb) = (total_rank(a), total_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match a.kind {
        Kind::Finite => match cmp_abs(a, b) {
            Ordering::Equal => a.exp.cmp(&b.exp),
            o => o,
        },
        Kind::Inf => Ordering::Equal,
        _ => nat::cmp(&a.coef, &b.coef),
    }
}

pub fn total_cmp(a: &Dec, b: &Dec) -> Ordering {
    if a.neg != b.neg {
        return if a.neg { Ordering::Less } else { Ordering::Greater };
    }
    let r = total_cmp_abs(a, b);
    if a.neg { r.reverse() } else { r }
}

pub fn compare_total(a: &Dec, b: &Dec) -> Dec {
    ord_dec(total_cmp(a, b))
}

pub fn compare_total_mag(a: &Dec, b: &Dec) -> Dec {
    ord_dec(total_cmp_abs(a, b))
}

fn minmax(a: &Dec, b: &Dec, ctx: &mut Ctx, want_max: bool, mag: bool) -> Dec {
    if a.is_nan() || b.is_nan() {
        if a.is_snan() || b.is_snan() || (a.is_nan() && b.is_nan()) {
            return nan2(a, b, ctx).unwrap();
        }
        let x = if a.is_nan() { b } else { a };
        return apply(x, ctx);
    }
    let mut c = if mag {
        let (x, y) = (a.abs(), b.abs());
        cmp_num(&x, &y)
    } else {
        Ordering::Equal
    };
    if c == Ordering::Equal {
        c = cmp_num(a, b);
    }
    if c == Ordering::Equal {
        // tie-break on sign then exponent
        c = if a.neg != b.neg {
            if a.neg { Ordering::Less } else { Ordering::Greater }
        } else {
            let e = a.exp.cmp(&b.exp);
            if a.neg { e.reverse() } else { e }
        };
    }
    let pick_a = if want_max { c != Ordering::Less } else { c != Ordering::Greater };
    let x = if pick_a { a } else { b };
    apply(x, ctx)
}

pub fn max(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    minmax(a, b, ctx, true, false)
}
pub fn min(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    minmax(a, b, ctx, false, false)
}
pub fn max_mag(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    minmax(a, b, ctx, true, true)
}
pub fn min_mag(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    minmax(a, b, ctx, false, true)
}

// ------------------------------- quantize & co ----------------------------------------

/// Rescale finite `a` to exponent `exp` using rounding `round` (no limits checked).
/// Returns the new value and the rest.
pub fn rescale_raw(a: &Dec, exp: i64, round: Round) -> (Dec, Rest) {
    if exp >= a.exp {
        let shift = (exp as i128 - a.exp as i128) as u64;
        let (mut q, rest) = nat::shr(&a.coef, shift);
        if round_up(round, a.neg, if q.is_empty() { 0 } else { q[0] % 10 }, rest) {
            nat::incr(&mut q);
        }
        (Dec::finite(a.neg, q, exp), rest)
    } else {
        let c = nat::shl(&a.coef, (a.exp as i128 - exp as i128) as u64);
        (Dec::finite(a.neg, c, exp), Rest::Zero)
    }
}

pub fn quantize(a: &Dec, b: &Dec, round: Round, ctx: &mut Ctx) -> Dec {
    if a.is_special() || b.is_special() {
        if let Some(r) = nan2(a, b, ctx) {
            return r;
        }
        if a.is_inf() && b.is_inf() {
            return a.clone();
        }
        return invalid(ctx);
    }
    quantize_exp(a, b.exp, round, ctx)
}

pub fn quantize_exp(a: &Dec, exp: i64, round: Round, ctx: &mut Ctx) -> Dec {
    if exp < ctx.etiny() || exp > ctx.emax {
        return invalid(ctx);
    }
    if a.is_zero() {
        return finalize(Dec::finite(a.neg, vec![], exp), ctx, Rest::Zero);
    }
    let adj = a.adjusted() as i128;
    if adj - exp as i128 + 1 > ctx.prec as i128 {
        return invalid(ctx);
    }
    let (r, rest) = rescale_raw(a, exp, round);
    if nat::digits(&r.coef) as i128 > ctx.prec as i128 {
        return invalid(ctx);
    }
    if !r.coef.is_empty() && r.adjusted() > ctx.emax {
        return invalid(ctx);
    }
    if exp > a.exp {
        ctx.status |= ROUNDED;
        if rest != Rest::Zero {
            ctx.status |= INEXACT;
        }
    }
    if !r.coef.is_empty() && r.adjusted() < ctx.emin {
        ctx.status |= SUBNORMAL;
    }
    finalize(r, ctx, Rest::Zero)
}

pub fn same_quantum(a: &Dec, b: &Dec) -> bool {
    if a.is_special() || b.is_special() {
        return (a.is_nan() && b.is_nan()) || (a.is_inf() && b.is_inf());
    }
    a.exp == b.exp
}

pub fn reduce(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        return a.clone();
    }
    let mut r = finalize(a.clone(), ctx, Rest::Zero);
    if r.is_inf() {
        return r;
    }
    if r.coef.is_empty() {
        r.exp = 0;
        return r;
    }
    let emax_eff = if ctx.clamp { ctx.etop() } else { ctx.emax } as i128;
    let tz = nat::trailing_zeros(&r.coef) as i128;
    let k = tz.min(emax_eff - r.exp as i128);
    if k > 0 {
        r.coef = nat::shr_trunc(&r.coef, k as u64).into();
        r.exp += k as i64;
    }
    r
}

pub fn to_integral(a: &Dec, round: Round, exact: bool, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() || a.exp >= 0 {
        return a.clone();
    }
    if a.coef.is_empty() {
        return Dec::finite(a.neg, vec![], 0);
    }
    let (r, rest) = rescale_raw(a, 0, round);
    if exact {
        ctx.status |= ROUNDED;
        if rest != Rest::Zero {
            ctx.status |= INEXACT;
        }
    }
    r
}

pub fn logb(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        return Dec::inf(false);
    }
    if a.is_zero() {
        ctx.status |= DIVISION_BY_ZERO;
        return Dec::inf(true);
    }
    finalize(Dec::from_i64(a.adjusted()), ctx, Rest::Zero)
}

/// Integer value of an operand that must be an integer with exponent 0.
fn small_int_arg(b: &Dec) -> Option<i128> {
    if !b.is_finite() || b.exp != 0 {
        return None;
    }
    let v = nat::to_u128(&b.coef)?;
    if v > i64::MAX as u128 {
        return Some(if b.neg { -(i64::MAX as i128) } else { i64::MAX as i128 });
    }
    Some(if b.neg { -(v as i128) } else { v as i128 })
}

pub fn scaleb(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(a, b, ctx) {
        return r;
    }
    let n = match small_int_arg(b) {
        Some(n) => n,
        None => return invalid(ctx),
    };
    let lim = 2 * (ctx.emax as i128 + ctx.prec as i128);
    if n > lim || n < -lim {
        return invalid(ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    let mut r = a.clone();
    r.exp = clamp_i64(r.exp as i128 + n);
    finalize(r, ctx, Rest::Zero)
}

pub fn shift(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(a, b, ctx) {
        return r;
    }
    let n = match small_int_arg(b) {
        Some(n) => n,
        None => return invalid(ctx),
    };
    let prec = ctx.prec as i128;
    if n > prec || n < -prec {
        return invalid(ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    let c = nat::low_digits(&a.coef, ctx.prec as u64);
    let c = if n >= 0 {
        nat::low_digits(&nat::shl(&c, n as u64), ctx.prec as u64)
    } else {
        nat::shr_trunc(&c, (-n) as u64)
    };
    Dec::finite(a.neg, c, a.exp)
}

pub fn rotate(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(a, b, ctx) {
        return r;
    }
    let n = match small_int_arg(b) {
        Some(n) => n,
        None => return invalid(ctx),
    };
    let prec = ctx.prec as i128;
    if n > prec || n < -prec {
        return invalid(ctx);
    }
    if a.is_inf() {
        return a.clone();
    }
    let p = ctx.prec as u64;
    let c = nat::low_digits(&a.coef, p);
    let k = if n >= 0 { n as u64 } else { (prec + n) as u64 };
    // rotate left by k within p digits
    let left = nat::low_digits(&nat::shl(&c, k), p);
    let right = nat::shr_trunc(&c, p - k);
    let r = nat::add(&left, &right);
    let mut r = r;
    nat::trim(&mut r);
    Dec::finite(a.neg, r, a.exp)
}

// ------------------------------- logical ----------------------------------------

fn logical_digits(a: &Dec, prec: u64) -> Option<Vec<u8>> {
    if !a.is_finite() || a.neg || a.exp != 0 {
        return None;
    }
    let s = nat::to_string(&a.coef);
    let b = s.as_bytes();
    if b.iter().any(|&c| c != b'0' && c != b'1') {
        return None;
    }
    // little-endian digits, truncated/padded to prec
    let mut v: Vec<u8> = b.iter().rev().map(|c| c - b'0').collect();
    v.resize(prec as usize, 0);
    Some(v)
}

fn from_logical(v: &[u8]) -> Dec {
    let s: Vec<u8> = v.iter().rev().map(|d| d + b'0').collect();
    Dec::finite(false, nat::from_digits(&s), 0)
}

pub fn logical_op(a: &Dec, b: &Dec, op: u8, ctx: &mut Ctx) -> Dec {
    let p = ctx.prec as u64;
    if p > 100_000_000 {
        // only possible with absurd precisions; operate on operand length instead
    }
    let plen = p.min(nat::digits(&a.coef).max(nat::digits(&b.coef)).max(1));
    let (x, y) = match (logical_digits(a, plen), logical_digits(b, plen)) {
        (Some(x), Some(y)) => (x, y),
        _ => return invalid(ctx),
    };
    let r: Vec<u8> = x
        .iter()
        .zip(y.iter())
        .map(|(&p, &q)| match op {
            b'&' => p & q,
            b'|' => p | q,
            _ => p ^ q,
        })
        .collect();
    from_logical(&r)
}

pub fn logical_invert(a: &Dec, ctx: &mut Ctx) -> Dec {
    let p = ctx.prec as u64;
    let x = match logical_digits(a, p) {
        Some(x) => x,
        None => return invalid(ctx),
    };
    let r: Vec<u8> = x.iter().map(|&d| 1 - d).collect();
    from_logical(&r)
}

// ------------------------------- next ----------------------------------------

pub fn next_plus(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        if a.neg {
            return max_finite(true, ctx);
        }
        return a.clone();
    }
    let mut c = ctx.clone();
    c.round = Round::Ceiling;
    c.status = 0;
    let fixed = finalize(a.clone(), &mut c, Rest::Zero);
    if fixed.is_inf() || cmp_num(&fixed, a) != Ordering::Equal {
        return fixed;
    }
    let tiny = Dec::from_u64(false, 1, ctx.etiny() - 1);
    add(a, &tiny, &mut c)
}

pub fn next_minus(a: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan1(a, ctx) {
        return r;
    }
    if a.is_inf() {
        if !a.neg {
            return max_finite(false, ctx);
        }
        return a.clone();
    }
    let mut c = ctx.clone();
    c.round = Round::Floor;
    c.status = 0;
    let fixed = finalize(a.clone(), &mut c, Rest::Zero);
    if fixed.is_inf() || cmp_num(&fixed, a) != Ordering::Equal {
        return fixed;
    }
    let tiny = Dec::from_u64(true, 1, ctx.etiny() - 1);
    add(a, &tiny, &mut c)
}

pub fn next_toward(a: &Dec, b: &Dec, ctx: &mut Ctx) -> Dec {
    if let Some(r) = nan2(a, b, ctx) {
        return r;
    }
    let c = cmp_num(a, b);
    if c == Ordering::Equal {
        return a.clone().with_sign(b.neg);
    }
    let r = if c == Ordering::Less { next_plus(a, ctx) } else { next_minus(a, ctx) };
    if r.is_inf() {
        ctx.status |= OVERFLOW | INEXACT | ROUNDED;
    } else if r.adjusted() < ctx.emin {
        ctx.status |= UNDERFLOW | SUBNORMAL | INEXACT | ROUNDED;
        if r.is_zero() {
            ctx.status |= CLAMPED;
        }
    }
    r
}

// ------------------------------- classification ----------------------------------------

pub fn is_normal(a: &Dec, ctx: &Ctx) -> bool {
    a.is_finite() && !a.is_zero() && a.adjusted() >= ctx.emin
}

pub fn is_subnormal(a: &Dec, ctx: &Ctx) -> bool {
    a.is_finite() && !a.is_zero() && a.adjusted() < ctx.emin
}

pub fn number_class(a: &Dec, ctx: &Ctx) -> &'static str {
    match a.kind {
        Kind::SNaN => "sNaN",
        Kind::NaN => "NaN",
        Kind::Inf => {
            if a.neg { "-Infinity" } else { "+Infinity" }
        }
        Kind::Finite => {
            if a.is_zero() {
                if a.neg { "-Zero" } else { "+Zero" }
            } else if a.adjusted() < ctx.emin {
                if a.neg { "-Subnormal" } else { "+Subnormal" }
            } else if a.neg {
                "-Normal"
            } else {
                "+Normal"
            }
        }
    }
}

pub fn copy_sign(a: &Dec, b: &Dec) -> Dec {
    a.clone().with_sign(b.neg)
}
