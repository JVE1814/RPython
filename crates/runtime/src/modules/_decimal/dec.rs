//! Decimal numbers, contexts, rounding/finalisation, parsing and string forms
//! (General Decimal Arithmetic Specification semantics).

use super::nat::{self, Rest};

// Condition bits (values identical to the `Dec*` constants exposed by `_decimal`).
pub const CLAMPED: u32 = 1;
pub const CONVERSION_SYNTAX: u32 = 2;
pub const DIVISION_BY_ZERO: u32 = 4;
pub const DIVISION_IMPOSSIBLE: u32 = 8;
pub const DIVISION_UNDEFINED: u32 = 16;
pub const FPU_ERROR: u32 = 32;
pub const INEXACT: u32 = 64;
pub const INVALID_CONTEXT: u32 = 128;
pub const INVALID_OPERATION: u32 = 256;
pub const MALLOC_ERROR: u32 = 512;
pub const FLOAT_OPERATION: u32 = 1024;
pub const OVERFLOW: u32 = 2048;
pub const ROUNDED: u32 = 4096;
pub const SUBNORMAL: u32 = 8192;
pub const UNDERFLOW: u32 = 16384;
pub const IEEE_INVALID: u32 =
    CONVERSION_SYNTAX | DIVISION_IMPOSSIBLE | DIVISION_UNDEFINED | FPU_ERROR | INVALID_CONTEXT | INVALID_OPERATION | MALLOC_ERROR;
pub const ALL_CONDITIONS: u32 = 0x7fff;

pub const MAX_PREC: i64 = 999_999_999_999_999_999;
pub const MAX_EMAX: i64 = 999_999_999_999_999_999;
pub const MIN_EMIN: i64 = -999_999_999_999_999_999;
pub const MIN_ETINY: i64 = MIN_EMIN - (MAX_PREC - 1);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Round {
    Up,
    Down,
    Ceiling,
    Floor,
    HalfUp,
    HalfDown,
    HalfEven,
    Up05,
}

impl Round {
    pub const ALL: [Round; 8] =
        [Round::Up, Round::Down, Round::Ceiling, Round::Floor, Round::HalfUp, Round::HalfDown, Round::HalfEven, Round::Up05];
    pub fn name(self) -> &'static str {
        match self {
            Round::Up => "ROUND_UP",
            Round::Down => "ROUND_DOWN",
            Round::Ceiling => "ROUND_CEILING",
            Round::Floor => "ROUND_FLOOR",
            Round::HalfUp => "ROUND_HALF_UP",
            Round::HalfDown => "ROUND_HALF_DOWN",
            Round::HalfEven => "ROUND_HALF_EVEN",
            Round::Up05 => "ROUND_05UP",
        }
    }
    pub fn from_name(s: &str) -> Option<Round> {
        Round::ALL.iter().copied().find(|r| r.name() == s)
    }
}

#[derive(Clone, Debug)]
pub struct Ctx {
    pub prec: i64,
    pub emax: i64,
    pub emin: i64,
    pub round: Round,
    pub clamp: bool,
    pub status: u32,
}

impl Ctx {
    pub fn new(prec: i64, round: Round, emin: i64, emax: i64, clamp: bool) -> Ctx {
        Ctx { prec, emax, emin, round, clamp, status: 0 }
    }
    pub fn default_ctx() -> Ctx {
        Ctx::new(28, Round::HalfEven, -999999, 999999, false)
    }
    /// Context used for exact conversions.
    pub fn max_ctx() -> Ctx {
        Ctx::new(MAX_PREC, Round::HalfEven, MIN_EMIN, MAX_EMAX, false)
    }
    #[inline]
    pub fn etiny(&self) -> i64 {
        self.emin - self.prec + 1
    }
    #[inline]
    pub fn etop(&self) -> i64 {
        self.emax - self.prec + 1
    }
    pub fn with_prec(&self, prec: i64) -> Ctx {
        let mut c = self.clone();
        c.prec = prec;
        c.status = 0;
        c
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Finite,
    Inf,
    NaN,
    SNaN,
}

#[derive(Clone, Debug)]
pub struct Dec {
    pub neg: bool,
    pub kind: Kind,
    pub exp: i64,
    /// Coefficient (for NaNs: the payload); empty = 0.
    pub coef: Vec<u64>,
}

impl Dec {
    #[inline]
    pub fn finite(neg: bool, coef: Vec<u64>, exp: i64) -> Dec {
        Dec { neg, kind: Kind::Finite, exp, coef }
    }
    pub fn from_u64(neg: bool, c: u64, exp: i64) -> Dec {
        Dec::finite(neg, nat::from_u64(c), exp)
    }
    pub fn from_i64(v: i64) -> Dec {
        Dec::from_u64(v < 0, v.unsigned_abs(), 0)
    }
    pub fn zero() -> Dec {
        Dec::finite(false, vec![], 0)
    }
    pub fn one() -> Dec {
        Dec::from_u64(false, 1, 0)
    }
    pub fn inf(neg: bool) -> Dec {
        Dec { neg, kind: Kind::Inf, exp: 0, coef: vec![] }
    }
    pub fn nan() -> Dec {
        Dec { neg: false, kind: Kind::NaN, exp: 0, coef: vec![] }
    }
    #[inline]
    pub fn is_nan(&self) -> bool {
        matches!(self.kind, Kind::NaN | Kind::SNaN)
    }
    #[inline]
    pub fn is_snan(&self) -> bool {
        self.kind == Kind::SNaN
    }
    #[inline]
    pub fn is_qnan(&self) -> bool {
        self.kind == Kind::NaN
    }
    #[inline]
    pub fn is_inf(&self) -> bool {
        self.kind == Kind::Inf
    }
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.kind == Kind::Finite
    }
    #[inline]
    pub fn is_special(&self) -> bool {
        self.kind != Kind::Finite
    }
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.kind == Kind::Finite && self.coef.is_empty()
    }
    /// Number of coefficient digits (1 for zero).
    #[inline]
    pub fn ndigits(&self) -> i64 {
        if self.coef.is_empty() { 1 } else { nat::digits(&self.coef) as i64 }
    }
    /// Adjusted exponent (exponent of the most significant digit).
    #[inline]
    pub fn adjusted(&self) -> i64 {
        if self.kind != Kind::Finite {
            return 0;
        }
        self.exp + self.ndigits() - 1
    }
    pub fn is_integer(&self) -> bool {
        if self.kind != Kind::Finite {
            return false;
        }
        if self.exp >= 0 || self.coef.is_empty() {
            return true;
        }
        let tz = nat::trailing_zeros(&self.coef) as i64;
        tz >= -self.exp
    }
    pub fn negated(&self) -> Dec {
        let mut d = self.clone();
        d.neg = !d.neg;
        d
    }
    pub fn abs(&self) -> Dec {
        let mut d = self.clone();
        d.neg = false;
        d
    }
    pub fn with_sign(mut self, neg: bool) -> Dec {
        self.neg = neg;
        self
    }
    pub fn is_odd_integer_last(&self) -> bool {
        // last coefficient digit odd
        !self.coef.is_empty() && self.coef[0] % 2 == 1
    }
    /// Coefficient as digit string.
    pub fn coef_string(&self) -> String {
        nat::to_string(&self.coef)
    }
}

// ------------------------------- rounding ----------------------------------------

#[inline]
pub fn combine_rest(removed: Rest, below: Rest) -> Rest {
    if below == Rest::Zero {
        return removed;
    }
    match removed {
        Rest::Zero => Rest::Lt,
        Rest::Half => Rest::Gt,
        r => r,
    }
}

/// Whether to increment the truncated coefficient (last digit `last`).
#[inline]
pub fn round_up(mode: Round, neg: bool, last: u64, rest: Rest) -> bool {
    match rest {
        Rest::Zero => false,
        _ => match mode {
            Round::Down => false,
            Round::Up => true,
            Round::Ceiling => !neg,
            Round::Floor => neg,
            Round::HalfUp => rest >= Rest::Half,
            Round::HalfDown => rest == Rest::Gt,
            Round::HalfEven => rest == Rest::Gt || (rest == Rest::Half && last % 2 == 1),
            Round::Up05 => last % 5 == 0,
        },
    }
}

/// Overflow result for the given sign and context.
pub fn overflow_result(neg: bool, ctx: &mut Ctx) -> Dec {
    ctx.status |= OVERFLOW | INEXACT | ROUNDED;
    let to_inf = match ctx.round {
        Round::HalfUp | Round::HalfEven | Round::HalfDown | Round::Up => true,
        Round::Down | Round::Up05 => false,
        Round::Ceiling => !neg,
        Round::Floor => neg,
    };
    if to_inf {
        Dec::inf(neg)
    } else {
        max_finite(neg, ctx)
    }
}

pub fn max_finite(neg: bool, ctx: &Ctx) -> Dec {
    let c = nat::sub(&nat::pow10(ctx.prec as u64), &[1]);
    Dec::finite(neg, c, ctx.emax - ctx.prec + 1)
}

/// Round `coef` (with trailing `rest`) by removing `shift` digits. Returns the new
/// coefficient and the combined rest (before applying the increment).
pub fn shift_round(coef: &[u64], shift: u64, rest: Rest) -> (Vec<u64>, Rest) {
    let (q, r) = nat::shr(coef, shift);
    (q, combine_rest(r, rest))
}

/// Apply context precision, exponent limits and rounding to a finite result.
/// `rest` classifies any discarded digits beyond the coefficient.
pub fn finalize(mut d: Dec, ctx: &mut Ctx, rest: Rest) -> Dec {
    if d.kind != Kind::Finite {
        return d;
    }
    let prec = ctx.prec as i128;
    let emin = ctx.emin as i128;
    let emax = ctx.emax as i128;
    let etiny = emin - prec + 1;
    if d.coef.is_empty() && rest == Rest::Zero {
        let emax_eff = if ctx.clamp { emax - prec + 1 } else { emax };
        let e = d.exp as i128;
        if e > emax_eff {
            d.exp = emax_eff as i64;
            ctx.status |= CLAMPED;
        } else if e < etiny {
            d.exp = etiny as i64;
            ctx.status |= CLAMPED;
        }
        return d;
    }
    let nd = nat::digits(&d.coef) as i128;
    let mut exp = d.exp as i128;
    let adj = exp + nd.max(1) - 1;
    let subnormal = adj < emin;
    let mut target = exp;
    if nd > prec {
        target = exp + nd - prec;
    }
    if subnormal && target < etiny {
        target = etiny;
    }
    let mut rest = rest;
    let mut rounded = false;
    if target > exp {
        let shift = target - exp;
        let shift = if shift > u64::MAX as i128 / 2 { u64::MAX / 2 } else { shift as u64 };
        let (q, r) = shift_round(&d.coef, shift, rest);
        d.coef = q;
        rest = r;
        exp = target;
        rounded = true;
    }
    if rest != Rest::Zero {
        rounded = true;
        let last = if d.coef.is_empty() { 0 } else { d.coef[0] % 10 };
        if round_up(ctx.round, d.neg, last, rest) {
            nat::incr(&mut d.coef);
            if nat::digits(&d.coef) as i128 > prec {
                d.coef = nat::shr_trunc(&d.coef, 1);
                exp += 1;
            }
        }
    }
    if rounded {
        ctx.status |= ROUNDED;
    }
    if rest != Rest::Zero {
        ctx.status |= INEXACT;
    }
    if subnormal {
        ctx.status |= SUBNORMAL;
        if rest != Rest::Zero {
            ctx.status |= UNDERFLOW;
            if d.coef.is_empty() {
                ctx.status |= CLAMPED;
            }
        }
    }
    // overflow
    if !d.coef.is_empty() {
        let adj = exp + nat::digits(&d.coef) as i128 - 1;
        if adj > emax {
            return overflow_result(d.neg, ctx);
        }
        if ctx.clamp {
            let etop = emax - prec + 1;
            if exp > etop {
                d.coef = nat::shl(&d.coef, (exp - etop) as u64);
                exp = etop;
                ctx.status |= CLAMPED;
            }
        }
    } else {
        // rounded to zero
        let emax_eff = if ctx.clamp { emax - prec + 1 } else { emax };
        if exp > emax_eff {
            exp = emax_eff;
            ctx.status |= CLAMPED;
        }
        if exp < etiny {
            exp = etiny;
            ctx.status |= CLAMPED;
        }
    }
    d.exp = exp as i64;
    d
}

/// Truncate a NaN payload to fit the context.
pub fn fix_nan(mut d: Dec, ctx: &Ctx) -> Dec {
    if d.is_nan() && !d.coef.is_empty() {
        let maxlen = (ctx.prec - ctx.clamp as i64).max(0) as u64;
        if nat::digits(&d.coef) > maxlen {
            d.coef = nat::low_digits(&d.coef, maxlen);
        }
    }
    d
}

// ------------------------------- parsing ----------------------------------------

/// Parse a numeric string (ASCII, already stripped of whitespace and underscores).
/// Returns None for invalid syntax. The result is exact (not rounded).
pub fn parse(s: &[u8]) -> Option<Dec> {
    let mut i = 0;
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let rest = &s[i..];
    if rest.is_empty() {
        return None;
    }
    let lower: Vec<u8> = rest.iter().map(|c| c.to_ascii_lowercase()).collect();
    if lower == b"inf" || lower == b"infinity" {
        return Some(Dec::inf(neg));
    }
    let (kind, payload) = if lower.starts_with(b"nan") {
        (Kind::NaN, &rest[3..])
    } else if lower.starts_with(b"snan") {
        (Kind::SNaN, &rest[4..])
    } else {
        (Kind::Finite, rest)
    };
    if kind != Kind::Finite {
        if !payload.iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        return Some(Dec { neg, kind, exp: 0, coef: nat::from_digits(payload) });
    }
    // finite number
    let mut digits: Vec<u8> = Vec::with_capacity(rest.len());
    let mut j = 0;
    let mut frac = 0i128;
    let mut seen_digit = false;
    while j < rest.len() && rest[j].is_ascii_digit() {
        digits.push(rest[j]);
        j += 1;
        seen_digit = true;
    }
    if j < rest.len() && rest[j] == b'.' {
        j += 1;
        while j < rest.len() && rest[j].is_ascii_digit() {
            digits.push(rest[j]);
            frac += 1;
            j += 1;
            seen_digit = true;
        }
    }
    if !seen_digit {
        return None;
    }
    let mut exp: i128 = 0;
    if j < rest.len() && (rest[j] == b'e' || rest[j] == b'E') {
        j += 1;
        let mut eneg = false;
        if j < rest.len() && (rest[j] == b'+' || rest[j] == b'-') {
            eneg = rest[j] == b'-';
            j += 1;
        }
        let start = j;
        while j < rest.len() && rest[j].is_ascii_digit() {
            if exp < 1_000_000_000_000_000_000_000_000 {
                exp = exp * 10 + (rest[j] - b'0') as i128;
            }
            j += 1;
        }
        if j == start {
            return None;
        }
        if eneg {
            exp = -exp;
        }
    }
    if j != rest.len() {
        return None;
    }
    let mut e = exp - frac;
    // saturate: values beyond these bounds are outside every context anyway
    const LIM: i128 = 4_000_000_000_000_000_000;
    if e > LIM {
        e = LIM;
    } else if e < -LIM {
        e = -LIM;
    }
    Some(Dec::finite(neg, nat::from_digits(&digits), e as i64))
}

// ------------------------------- to string ----------------------------------------

/// to-scientific-string / to-engineering-string.
pub fn to_string(d: &Dec, eng: bool, capitals: bool) -> String {
    let sign = if d.neg { "-" } else { "" };
    match d.kind {
        Kind::Inf => return format!("{sign}Infinity"),
        Kind::NaN | Kind::SNaN => {
            let s = if d.kind == Kind::SNaN { "s" } else { "" };
            let payload = if d.coef.is_empty() { String::new() } else { nat::to_string(&d.coef) };
            return format!("{sign}{s}NaN{payload}");
        }
        Kind::Finite => {}
    }
    let digits = nat::to_string(&d.coef);
    let n = digits.len() as i128;
    let exp = d.exp as i128;
    let adj = exp + n - 1;
    let mut body = String::with_capacity(digits.len() + 8);
    // Plain notation: no exponent needed and not too many leading zeros.
    if exp <= 0 && adj >= -6 {
        let int_len = n + exp; // digits in front of the point
        if exp == 0 {
            body.push_str(&digits);
        } else if int_len > 0 {
            body.push_str(&digits[..int_len as usize]);
            body.push('.');
            body.push_str(&digits[int_len as usize..]);
        } else {
            body.push_str("0.");
            body.extend(std::iter::repeat_n('0', (-int_len) as usize));
            body.push_str(&digits);
        }
        return format!("{sign}{body}");
    }
    // Exponential notation: `lead` digits in front of the point, exponent `shown`.
    let (lead, shown, zero_frac): (i128, i128, i128) = if !eng {
        (1, adj, 0)
    } else if d.coef.is_empty() {
        // a zero keeps its exponent's value: raise the exponent to a multiple of
        // three and show the difference as zeros after the point
        let up = (3 - exp.rem_euclid(3)) % 3;
        (1, exp + up, up)
    } else {
        let e3 = adj.div_euclid(3) * 3;
        (adj - e3 + 1, e3, 0)
    };
    if lead >= n {
        body.push_str(&digits);
        body.extend(std::iter::repeat_n('0', (lead - n) as usize));
    } else {
        body.push_str(&digits[..lead as usize]);
        body.push('.');
        body.push_str(&digits[lead as usize..]);
    }
    if zero_frac > 0 {
        body.push('.');
        body.extend(std::iter::repeat_n('0', zero_frac as usize));
    }
    if shown != 0 {
        body.push(if capitals { 'E' } else { 'e' });
        if shown > 0 {
            body.push('+');
        }
        body.push_str(&shown.to_string());
    }
    format!("{sign}{body}")
}
