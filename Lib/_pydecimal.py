"""Pure-Python implementation of decimal arithmetic.

Used by the decimal module when the _decimal accelerator is unavailable.
Values keep an integer coefficient (a Python int) and an integer exponent;
operations compute an exact or sticky-truncated intermediate result and then
round it once to the context ("finalisation").  Conditions are collected as a
bit mask while an operation runs and are turned into flags/exceptions by the
context at the end, so signal precedence does not depend on the order in
which an operation discovers them.
"""

__all__ = [
    'Decimal', 'Context',
    'DecimalTuple',
    'DefaultContext', 'BasicContext', 'ExtendedContext',
    'DecimalException', 'Clamped', 'InvalidOperation', 'DivisionByZero',
    'Inexact', 'Rounded', 'Subnormal', 'Overflow', 'Underflow',
    'FloatOperation',
    'DivisionImpossible', 'InvalidContext', 'ConversionSyntax', 'DivisionUndefined',
    'ROUND_DOWN', 'ROUND_HALF_UP', 'ROUND_HALF_EVEN', 'ROUND_CEILING',
    'ROUND_FLOOR', 'ROUND_UP', 'ROUND_HALF_DOWN', 'ROUND_05UP',
    'setcontext', 'getcontext', 'localcontext', 'IEEEContext',
    'MAX_PREC', 'MAX_EMAX', 'MIN_EMIN', 'MIN_ETINY', 'IEEE_CONTEXT_MAX_BITS',
    'HAVE_THREADS', 'HAVE_CONTEXTVAR', 'SPEC_VERSION',
]

# Classes and functions of this module present themselves as part of 'decimal'
# (pickles and reprs are interchangeable with the accelerated module).
__name__ = 'decimal'

SPEC_VERSION = '1.70'
__libmpdec_version__ = '2.5.1'

import math as _math
import numbers as _numbers
import sys as _sys

try:
    from collections import namedtuple as _namedtuple
    DecimalTuple = _namedtuple('DecimalTuple', 'sign digits exponent', module='decimal')
except ImportError:
    DecimalTuple = lambda *args: args

import contextvars as _contextvars

ROUND_DOWN = 'ROUND_DOWN'
ROUND_HALF_UP = 'ROUND_HALF_UP'
ROUND_HALF_EVEN = 'ROUND_HALF_EVEN'
ROUND_CEILING = 'ROUND_CEILING'
ROUND_FLOOR = 'ROUND_FLOOR'
ROUND_UP = 'ROUND_UP'
ROUND_HALF_DOWN = 'ROUND_HALF_DOWN'
ROUND_05UP = 'ROUND_05UP'
_ROUNDINGS = (ROUND_UP, ROUND_DOWN, ROUND_CEILING, ROUND_FLOOR,
              ROUND_HALF_UP, ROUND_HALF_DOWN, ROUND_HALF_EVEN, ROUND_05UP)

HAVE_THREADS = True
HAVE_CONTEXTVAR = True

if _sys.maxsize == 2**63 - 1:
    MAX_PREC = 999999999999999999
    MAX_EMAX = 999999999999999999
    MIN_EMIN = -999999999999999999
    IEEE_CONTEXT_MAX_BITS = 512
else:
    MAX_PREC = 425000000
    MAX_EMAX = 425000000
    MIN_EMIN = -425000000
    IEEE_CONTEXT_MAX_BITS = 256
MIN_ETINY = MIN_EMIN - (MAX_PREC - 1)

# Fast digit-count bounds: log2(10) lies strictly between these.
_LOG_10_BASE_2_LO = 3.32
_LOG_10_BASE_2_HI = 3.33


# ----------------------------------------------------------------------------
# Signals

class DecimalException(ArithmeticError):
    """Root of the decimal signal hierarchy."""

class Clamped(DecimalException):
    """An exponent was adjusted to fit the context limits."""

class InvalidOperation(DecimalException):
    """The operation has no meaningful result; the result is NaN."""

class ConversionSyntax(InvalidOperation):
    """A string is not a valid number."""

class DivisionByZero(DecimalException, ZeroDivisionError):
    """A finite nonzero number was divided by zero."""

class DivisionImpossible(InvalidOperation):
    """An integer quotient does not fit the precision."""

class DivisionUndefined(InvalidOperation, ZeroDivisionError):
    """Zero was divided by zero."""

class Inexact(DecimalException):
    """Nonzero digits were lost by rounding."""

class InvalidContext(InvalidOperation):
    """The context settings are unusable."""

class Rounded(DecimalException):
    """Digits were removed from a result (possibly only zeros)."""

class Subnormal(DecimalException):
    """A result's adjusted exponent is below Emin."""

class Overflow(Inexact, Rounded):
    """A result is too large for the context."""

class Underflow(Inexact, Rounded, Subnormal):
    """A subnormal result was also inexact."""

class FloatOperation(DecimalException, TypeError):
    """A float was mixed with Decimals."""

# condition bits
_CLAMPED = 1
_CONV_SYNTAX = 2
_DIV_BY_ZERO = 4
_DIV_IMPOSSIBLE = 8
_DIV_UNDEFINED = 16
_INEXACT = 64
_INVALID_CONTEXT = 128
_INVALID = 256
_FLOAT_OP = 1024
_OVERFLOW = 2048
_ROUNDED = 4096
_SUBNORMAL = 8192
_UNDERFLOW = 16384
_INVALID_ANY = _CONV_SYNTAX | _DIV_IMPOSSIBLE | _DIV_UNDEFINED | _INVALID_CONTEXT | _INVALID

# (bits, signal) in precedence order (highest first) for raising
_SIGNAL_BITS = (
    (_INVALID_ANY, InvalidOperation),
    (_FLOAT_OP, FloatOperation),
    (_DIV_BY_ZERO, DivisionByZero),
    (_OVERFLOW, Overflow),
    (_UNDERFLOW, Underflow),
    (_SUBNORMAL, Subnormal),
    (_INEXACT, Inexact),
    (_ROUNDED, Rounded),
    (_CLAMPED, Clamped),
)
_SIGNALS = tuple(s for _, s in _SIGNAL_BITS)
_CONDITION_CLASS = (
    (_CONV_SYNTAX, ConversionSyntax),
    (_DIV_IMPOSSIBLE, DivisionImpossible),
    (_DIV_UNDEFINED, DivisionUndefined),
    (_INVALID_CONTEXT, InvalidContext),
    (_INVALID, InvalidOperation),
)
_CONDITION_SIGNAL = {
    ConversionSyntax: InvalidOperation, DivisionImpossible: InvalidOperation,
    DivisionUndefined: InvalidOperation, InvalidContext: InvalidOperation,
}
_REPR_ORDER = (Clamped, InvalidOperation, DivisionByZero, Inexact, FloatOperation,
               Overflow, Rounded, Subnormal, Underflow)


# ----------------------------------------------------------------------------
# Integer helpers (no str() round trips for big numbers: they are subject to
# the int max str digits limit)

_POW10_CACHE = {}

def _pow10(n):
    if n < 64:
        return 10 ** n
    p = _POW10_CACHE.get(n)
    if p is None:
        p = 10 ** n
        if len(_POW10_CACHE) > 256:
            _POW10_CACHE.clear()
        _POW10_CACHE[n] = p
    return p

def _is_less_than_pow10a_use_str(q, a):
    """Return (q < 10**a, decimal string of q or None) for q >= 0."""
    b = q.bit_length()
    if b < a * _LOG_10_BASE_2_LO:
        return True, None
    if b >= 1 + a * _LOG_10_BASE_2_HI:
        return False, None
    s = _int_to_str(q)
    return len(s) <= a, s

def _ndigits(c):
    """Number of decimal digits of c >= 0 (0 has one digit)."""
    if c < 10**18:
        return len(str(c))
    est = int(c.bit_length() * 0.30102999566398120)  # floor(log10) or one less
    return est + 1 if c >= _pow10(est) else est

def _int_to_str(c):
    if c.bit_length() <= 8000:
        return str(c)
    n = _ndigits(c)
    half = n // 2
    hi, lo = divmod(c, _pow10(half))
    return _int_to_str(hi) + _int_to_str(lo).rjust(half, '0')

def _str_to_int(s):
    if len(s) <= 2000:
        return int(s)
    half = len(s) // 2
    return _str_to_int(s[:-half]) * _pow10(half) + _str_to_int(s[-half:])

def _trailing_zeros(c):
    """Number of trailing decimal zeros of c > 0."""
    n = 0
    step = 16
    while True:
        p = _pow10(step)
        if c % p == 0:
            c //= p
            n += step
            step *= 2
        elif step > 1:
            step //= 2
        else:
            return n


# ----------------------------------------------------------------------------
# Rounding machinery.  A "rest" describes discarded digits relative to one unit
# of the last kept digit: 0 exactly zero, 1 below half, 2 exactly half, 3 above.

def _shift_right(c, k):
    """(c // 10**k, rest) for k > 0."""
    if k <= 0:
        return c, 0
    if k > _ndigits(c) + 1:
        return 0, (1 if c else 0)
    p = _pow10(k)
    q, r = divmod(c, p)
    if r == 0:
        return q, 0
    h = p // 2
    return q, (1 if r < h else 2 if r == h else 3)

def _combine(removed, below):
    if not below:
        return removed
    if removed == 0:
        return 1
    if removed == 2:
        return 3
    return removed

def _rounds_up(rnd, sign, last, rest):
    if not rest:
        return False
    if rnd is ROUND_HALF_EVEN or rnd == ROUND_HALF_EVEN:
        return rest == 3 or (rest == 2 and last & 1 == 1)
    if rnd == ROUND_DOWN:
        return False
    if rnd == ROUND_UP:
        return True
    if rnd == ROUND_HALF_UP:
        return rest >= 2
    if rnd == ROUND_HALF_DOWN:
        return rest == 3
    if rnd == ROUND_CEILING:
        return not sign
    if rnd == ROUND_FLOOR:
        return bool(sign)
    return last % 5 == 0  # ROUND_05UP


class _Env:
    """Arithmetic parameters of one operation plus the collected conditions."""
    __slots__ = ('prec', 'emin', 'emax', 'rnd', 'clamp', 'status')

    def __init__(self, prec, emin, emax, rnd, clamp):
        self.prec = prec
        self.emin = emin
        self.emax = emax
        self.rnd = rnd
        self.clamp = clamp
        self.status = 0

    def clone(self, rnd=None):
        return _Env(self.prec, self.emin, self.emax, rnd or self.rnd, self.clamp)

    def etiny(self):
        return self.emin - self.prec + 1

    def etop(self):
        return self.emax - self.prec + 1

def _max_env():
    return _Env(MAX_PREC, MIN_EMIN, MAX_EMAX, ROUND_HALF_EVEN, 0)


def _mk(sign, coef, exp):
    d = object.__new__(Decimal)
    d._sign = sign
    d._int = coef
    d._exp = exp
    d._is_special = False
    return d

def _mk_special(sign, kind, payload=0):
    d = object.__new__(Decimal)
    d._sign = sign
    d._int = payload
    d._exp = kind
    d._is_special = True
    return d

_NAN = None  # set after class creation

def _nan():
    return _mk_special(0, 'n', 0)

def _inf(sign):
    return _mk_special(sign, 'F', 0)


def _overflow_result(sign, env):
    env.status |= _OVERFLOW | _INEXACT | _ROUNDED
    rnd = env.rnd
    if rnd in (ROUND_HALF_UP, ROUND_HALF_EVEN, ROUND_HALF_DOWN, ROUND_UP):
        to_inf = True
    elif rnd in (ROUND_DOWN, ROUND_05UP):
        to_inf = False
    elif rnd == ROUND_CEILING:
        to_inf = not sign
    else:
        to_inf = bool(sign)
    if to_inf:
        return _inf(sign)
    return _mk(sign, _pow10(env.prec) - 1, env.emax - env.prec + 1)


def _finalize(sign, c, e, env, rest=0):
    """Round the finite value (-1)**sign * c * 10**e (plus `rest`) to env."""
    prec = env.prec
    etiny = env.emin - prec + 1
    if c == 0 and not rest:
        top = env.emax - prec + 1 if env.clamp else env.emax
        if e > top:
            e = top
            env.status |= _CLAMPED
        elif e < etiny:
            e = etiny
            env.status |= _CLAMPED
        return _mk(sign, 0, e)
    nd = _ndigits(c) if c else 0
    adj = e + max(nd, 1) - 1
    subnormal = adj < env.emin
    target = e
    if nd > prec:
        target = e + nd - prec
    if subnormal and target < etiny:
        target = etiny
    rounded = False
    if target > e:
        c, r = _shift_right(c, target - e)
        rest = _combine(r, rest)
        e = target
        rounded = True
    if rest:
        rounded = True
        if _rounds_up(env.rnd, sign, c % 10, rest):
            c += 1
            if c == _pow10(prec):
                c //= 10
                e += 1
    st = 0
    if rounded:
        st |= _ROUNDED
    if rest:
        st |= _INEXACT
    if subnormal:
        st |= _SUBNORMAL
        if rest:
            st |= _UNDERFLOW
            if c == 0:
                st |= _CLAMPED
    env.status |= st
    if c:
        if e + _ndigits(c) - 1 > env.emax:
            return _overflow_result(sign, env)
        if env.clamp:
            top = env.emax - prec + 1
            if e > top:
                c *= _pow10(e - top)
                e = top
                env.status |= _CLAMPED
    else:
        top = env.emax - prec + 1 if env.clamp else env.emax
        if e > top:
            e = top
            env.status |= _CLAMPED
        if e < etiny:
            e = etiny
            env.status |= _CLAMPED
    return _mk(sign, c, e)


def _fix_nan(d, env):
    """Quiet copy of NaN d with the payload cut to fit env."""
    keep = env.prec - env.clamp
    payload = d._int
    if payload and _ndigits(payload) > keep:
        payload = payload % _pow10(keep) if keep > 0 else 0
    return _mk_special(d._sign, 'n', payload)

def _nan1(a, env):
    if a._is_special and a._exp in 'nN':
        if a._exp == 'N':
            env.status |= _INVALID
        return _fix_nan(a, env)
    return None

def _nan2(a, b, env):
    an = a._is_special and a._exp in 'nN'
    bn = b._is_special and b._exp in 'nN'
    if not (an or bn):
        return None
    if an and a._exp == 'N':
        env.status |= _INVALID
        return _fix_nan(a, env)
    if bn and b._exp == 'N':
        env.status |= _INVALID
        return _fix_nan(b, env)
    return _fix_nan(a if an else b, env)

def _invalid(env, bit=_INVALID):
    env.status |= bit
    return _nan()

def _is_nan(a):
    return a._is_special and a._exp in 'nN'

def _is_inf(a):
    return a._is_special and a._exp == 'F'

def _adjusted(a):
    return a._exp + _ndigits(a._int) - 1


# ----------------------------------------------------------------------------
# Parsing and formatting of numeric strings

def _parse(s):
    """Parse an ASCII numeric string exactly; None if malformed."""
    if not s:
        return None
    sign = 0
    i = 0
    if s[0] in '+-':
        sign = 1 if s[0] == '-' else 0
        i = 1
    body = s[i:]
    low = body.lower()
    if low in ('inf', 'infinity'):
        return _inf(sign)
    for prefix, kind in (('snan', 'N'), ('nan', 'n')):
        if low.startswith(prefix):
            payload = body[len(prefix):]
            if payload and not (payload.isdigit() and payload.isascii()):
                return None
            return _mk_special(sign, kind, _str_to_int(payload) if payload else 0)
    mant, sep, expo = low.partition('e')
    if sep:
        if expo[:1] in ('+', '-'):
            esign, edigits = expo[0], expo[1:]
        else:
            esign, edigits = '+', expo
        if not edigits or not (edigits.isdigit() and edigits.isascii()):
            return None
        e = _str_to_int(edigits)
        if esign == '-':
            e = -e
    else:
        e = 0
    ip, dot, fp = mant.partition('.')
    if not ip and not fp:
        return None
    for part in (ip, fp):
        if part and not (part.isdigit() and part.isascii()):
            return None
    digits = (ip + fp).lstrip('0') or '0'
    return _mk(sign, _str_to_int(digits), e - len(fp))

def _to_str(d, eng=False, capitals=True):
    sign = '-' if d._sign else ''
    if d._is_special:
        if d._exp == 'F':
            return sign + 'Infinity'
        tag = 'sNaN' if d._exp == 'N' else 'NaN'
        return sign + tag + (_int_to_str(d._int) if d._int else '')
    digits = _int_to_str(d._int)
    n = len(digits)
    exp = d._exp
    adj = exp + n - 1
    if exp <= 0 and adj >= -6:
        if exp == 0:
            return sign + digits
        int_len = n + exp
        if int_len > 0:
            return sign + digits[:int_len] + '.' + digits[int_len:]
        return sign + '0.' + '0' * (-int_len) + digits
    extra_zeros = 0
    if not eng:
        lead, shown = 1, adj
    elif d._int == 0:
        up = (3 - exp % 3) % 3
        lead, shown, extra_zeros = 1, exp + up, up
    else:
        e3 = (adj // 3) * 3
        lead, shown = adj - e3 + 1, e3
    if lead >= n:
        body = digits + '0' * (lead - n)
    else:
        body = digits[:lead] + '.' + digits[lead:]
    if extra_zeros:
        body += '.' + '0' * extra_zeros
    if shown:
        body += ('E' if capitals else 'e') + ('+' if shown > 0 else '-') + str(abs(shown))
    return sign + body


# ----------------------------------------------------------------------------
# Core operations (operands are Decimal instances; results are Decimals)

def _zero_sum_sign(s1, s2, env):
    if s1 == s2:
        return s1
    return 1 if env.rnd == ROUND_FLOOR else 0

def _add_exact(a, asign, b, bsign, env):
    """Exact (or sticky-reduced) finite sum, ready for _finalize under env."""
    if a._exp >= b._exp:
        hi, hs, lo, ls = a, asign, b, bsign
    else:
        hi, hs, lo, ls = b, bsign, a, asign
    if hi._int == 0:
        if lo._int == 0:
            return _zero_sum_sign(hs, ls, env), 0, lo._exp
        return ls, lo._int, lo._exp
    lo_c, lo_e = lo._int, lo._exp
    dh = _ndigits(hi._int)
    prec = env.prec
    if lo_c == 0:
        lim = max(dh, prec) + 3
        if hi._exp - lo_e > lim:
            lo_e = hi._exp - lim
    else:
        kpos = min(hi._exp, hi._exp + dh - prec) - 2
        if lo_e < kpos and lo_e + _ndigits(lo_c) - 1 < kpos:
            lo_c, lo_e = 1, kpos
    hc = hi._int * _pow10(hi._exp - lo_e)
    if hs == ls:
        return hs, hc + lo_c, lo_e
    if hc > lo_c:
        return hs, hc - lo_c, lo_e
    if hc < lo_c:
        return ls, lo_c - hc, lo_e
    return _zero_sum_sign(hs, ls, env), 0, lo_e

def _op_add(a, b, env, negate_b=False):
    bsign = b._sign ^ 1 if negate_b else b._sign
    if a._is_special or b._is_special:
        r = _nan2(a, b, env)
        if r is not None:
            return r
        if _is_inf(a):
            if _is_inf(b) and a._sign != bsign:
                return _invalid(env)
            return _inf(a._sign)
        return _inf(bsign)
    s, c, e = _add_exact(a, a._sign, b, bsign, env)
    return _finalize(s, c, e, env)

def _op_mul(a, b, env):
    sign = a._sign ^ b._sign
    if a._is_special or b._is_special:
        r = _nan2(a, b, env)
        if r is not None:
            return r
        if (_is_inf(a) and not b._is_special and b._int == 0) or \
           (_is_inf(b) and not a._is_special and a._int == 0):
            return _invalid(env)
        return _inf(sign)
    return _finalize(sign, a._int * b._int, a._exp + b._exp, env)

def _div_coef(ca, cb, ideal, prec):
    """Quotient with at least prec digits: (q, exp, rest)."""
    shift = prec + _ndigits(cb) - _ndigits(ca)
    if shift > 0:
        q, r = divmod(ca * _pow10(shift), cb)
        den = cb
    else:
        den = cb * _pow10(-shift)
        q, r = divmod(ca, den)
    exp = ideal - shift
    if r == 0:
        if exp < ideal and q:
            k = min(_trailing_zeros(q), ideal - exp)
            if k:
                q //= _pow10(k)
                exp += k
        return q, exp, 0
    r2 = 2 * r
    return q, exp, (1 if r2 < den else 2 if r2 == den else 3)

def _op_div(a, b, env):
    sign = a._sign ^ b._sign
    if a._is_special or b._is_special:
        r = _nan2(a, b, env)
        if r is not None:
            return r
        if _is_inf(a):
            if _is_inf(b):
                return _invalid(env)
            return _inf(sign)
        env.status |= _CLAMPED
        return _mk(sign, 0, env.etiny())
    if b._int == 0:
        if a._int == 0:
            return _invalid(env, _DIV_UNDEFINED)
        env.status |= _DIV_BY_ZERO
        return _inf(sign)
    ideal = a._exp - b._exp
    if a._int == 0:
        return _finalize(sign, 0, ideal, env)
    q, e, rest = _div_coef(a._int, b._int, ideal, env.prec)
    return _finalize(sign, q, e, env, rest)

def _int_divrem(a, b, prec):
    """(q, r, exp_r) with |a| = q*|b| + r*10**exp_r, or None if q is too long."""
    e = min(a._exp, b._exp)
    if a._int == 0 or _adjusted(a) < _adjusted(b):
        return 0, a._int * _pow10(a._exp - e), e
    if _adjusted(a) - _adjusted(b) > prec:
        return None
    q, r = divmod(a._int * _pow10(a._exp - e), b._int * _pow10(b._exp - e))
    if _ndigits(q) > prec:
        return None
    return q, r, e

def _divint_specials(a, b, env, sign):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    if _is_inf(a):
        if _is_inf(b):
            return _invalid(env)
        return _inf(sign)
    return _mk(sign, 0, 0)

def _op_divint(a, b, env):
    sign = a._sign ^ b._sign
    if a._is_special or b._is_special:
        return _divint_specials(a, b, env, sign)
    if b._int == 0:
        if a._int == 0:
            return _invalid(env, _DIV_UNDEFINED)
        env.status |= _DIV_BY_ZERO
        return _inf(sign)
    t = _int_divrem(a, b, env.prec)
    if t is None:
        return _invalid(env, _DIV_IMPOSSIBLE)
    return _finalize(sign, t[0], 0, env)

def _rem_specials(a, b, env):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    if _is_inf(a):
        return _invalid(env)
    return _finalize(a._sign, a._int, a._exp, env)

def _op_rem(a, b, env):
    if a._is_special or b._is_special:
        return _rem_specials(a, b, env)
    if b._int == 0:
        return _invalid(env, _DIV_UNDEFINED if a._int == 0 else _INVALID)
    t = _int_divrem(a, b, env.prec)
    if t is None:
        return _invalid(env, _DIV_IMPOSSIBLE)
    return _finalize(a._sign, t[1], t[2], env)

def _op_rem_near(a, b, env):
    if a._is_special or b._is_special:
        return _rem_specials(a, b, env)
    if b._int == 0:
        return _invalid(env, _DIV_UNDEFINED if a._int == 0 else _INVALID)
    e = min(a._exp, b._exp)
    if a._int and _adjusted(a) < _adjusted(b) - 1:
        return _finalize(a._sign, a._int * _pow10(a._exp - e), e, env)
    t = _int_divrem(a, b, env.prec)
    if t is None:
        return _invalid(env, _DIV_IMPOSSIBLE)
    q, r, e = t
    bc = b._int * _pow10(b._exp - e)
    sign = a._sign
    r2 = 2 * r
    if r2 > bc or (r2 == bc and q & 1):
        r = bc - r
        sign ^= 1
        q += 1
    if _ndigits(q) > env.prec:
        return _invalid(env, _DIV_IMPOSSIBLE)
    if r == 0:
        sign = a._sign
    return _finalize(sign, r, e, env)

def _op_divmod(a, b, env):
    sign = a._sign ^ b._sign
    if a._is_special or b._is_special:
        r = _nan2(a, b, env)
        if r is not None:
            return r, r
        if _is_inf(a):
            if _is_inf(b):
                n = _invalid(env)
                return n, n
            return _inf(sign), _invalid(env)
        return _mk(sign, 0, 0), _finalize(a._sign, a._int, a._exp, env)
    if b._int == 0:
        if a._int == 0:
            n = _invalid(env, _DIV_UNDEFINED)
            return n, n
        env.status |= _DIV_BY_ZERO | _INVALID
        return _inf(sign), _nan()
    t = _int_divrem(a, b, env.prec)
    if t is None:
        n = _invalid(env, _DIV_IMPOSSIBLE)
        return n, n
    q, r, e = t
    return _finalize(sign, q, 0, env), _finalize(a._sign, r, e, env)

def _op_fma(a, b, c, env):
    if a._is_special or b._is_special:
        if (a._is_special and a._exp == 'N') or (b._is_special and b._exp == 'N'):
            return _nan2(a, b, env)
        if _is_nan(a) or _is_nan(b):
            if c._is_special and c._exp == 'N':
                env.status |= _INVALID
                return _fix_nan(c, env)
            return _nan2(a, b, env)
        if (_is_inf(a) and not b._is_special and b._int == 0) or \
           (_is_inf(b) and not a._is_special and a._int == 0):
            return _invalid(env)
        prod = _inf(a._sign ^ b._sign)
    else:
        prod = _mk(a._sign ^ b._sign, a._int * b._int, a._exp + b._exp)
    return _op_add(prod, c, env)

def _cmp_abs(a, b):
    """Compare |a| and |b| for finite a, b: -1, 0, 1."""
    if a._int == 0 or b._int == 0:
        return (a._int != 0) - (b._int != 0)
    aa, ba = _adjusted(a), _adjusted(b)
    if aa != ba:
        return 1 if aa > ba else -1
    if a._exp >= b._exp:
        x, y = a._int * _pow10(a._exp - b._exp), b._int
    else:
        x, y = a._int, b._int * _pow10(b._exp - a._exp)
    return (x > y) - (x < y)

def _sign_class(a):
    if not a._is_special and a._int == 0:
        return 0
    return -1 if a._sign else 1

def _cmp_num(a, b):
    """Numeric comparison of non-NaN values: -1, 0, 1."""
    sa, sb = _sign_class(a), _sign_class(b)
    if sa != sb:
        return 1 if sa > sb else -1
    if sa == 0:
        return 0
    ai, bi = _is_inf(a), _is_inf(b)
    if ai or bi:
        m = ai - bi
    else:
        m = _cmp_abs(a, b)
    return -m if sa < 0 else m

def _ord_dec(o):
    return _mk(1 if o < 0 else 0, abs(o), 0)

def _total_rank(a):
    if not a._is_special:
        return 0
    return {'F': 1, 'N': 2, 'n': 3}[a._exp]

def _total_cmp_abs(a, b):
    ra, rb = _total_rank(a), _total_rank(b)
    if ra != rb:
        return 1 if ra > rb else -1
    if ra == 0:
        o = _cmp_abs(a, b)
        if o == 0:
            o = (a._exp > b._exp) - (a._exp < b._exp)
        return o
    if ra == 1:
        return 0
    return (a._int > b._int) - (a._int < b._int)

def _total_cmp(a, b):
    if a._sign != b._sign:
        return -1 if a._sign else 1
    o = _total_cmp_abs(a, b)
    return -o if a._sign else o

def _apply(a, env):
    """Round a to env (create_decimal semantics; NaNs only get their payload cut)."""
    if a._is_special:
        if a._exp == 'F':
            return a
        keep = env.prec - env.clamp
        payload = a._int
        if payload and _ndigits(payload) > keep:
            payload = payload % _pow10(keep) if keep > 0 else 0
        return _mk_special(a._sign, a._exp, payload)
    return _finalize(a._sign, a._int, a._exp, env)

def _op_minmax(a, b, env, want_max, mag):
    if _is_nan(a) or _is_nan(b):
        if (a._is_special and a._exp == 'N') or (b._is_special and b._exp == 'N') or \
                (_is_nan(a) and _is_nan(b)):
            return _nan2(a, b, env)
        return _apply(b if _is_nan(a) else a, env)
    o = 0
    if mag:
        o = _cmp_num(_mk_abs(a), _mk_abs(b))
    if o == 0:
        o = _cmp_num(a, b)
    if o == 0:
        if a._sign != b._sign:
            o = -1 if a._sign else 1
        else:
            ea = 0 if a._is_special else a._exp
            eb = 0 if b._is_special else b._exp
            o = (ea > eb) - (ea < eb)
            if a._sign:
                o = -o
    pick_a = o >= 0 if want_max else o <= 0
    return _apply(a if pick_a else b, env)

def _mk_abs(a):
    if a._is_special:
        return _mk_special(0, a._exp, a._int)
    return _mk(0, a._int, a._exp)

def _rescale_raw(a, exp, rnd):
    """(value of finite a rescaled to exponent exp, rest)."""
    if exp >= a._exp:
        q, rest = _shift_right(a._int, exp - a._exp)
        if _rounds_up(rnd, a._sign, q % 10, rest):
            q += 1
        return _mk(a._sign, q, exp), rest
    return _mk(a._sign, a._int * _pow10(a._exp - exp), exp), 0

def _op_quantize_exp(a, exp, rnd, env):
    if exp < env.etiny() or exp > env.emax:
        return _invalid(env)
    if a._int == 0:
        return _finalize(a._sign, 0, exp, env)
    if _adjusted(a) - exp + 1 > env.prec:
        return _invalid(env)
    r, rest = _rescale_raw(a, exp, rnd)
    if _ndigits(r._int) > env.prec:
        return _invalid(env)
    if r._int and _adjusted(r) > env.emax:
        return _invalid(env)
    if exp > a._exp:
        env.status |= _ROUNDED
        if rest:
            env.status |= _INEXACT
    if r._int and _adjusted(r) < env.emin:
        env.status |= _SUBNORMAL
    return _finalize(r._sign, r._int, r._exp, env)

def _op_quantize(a, b, rnd, env):
    if a._is_special or b._is_special:
        r = _nan2(a, b, env)
        if r is not None:
            return r
        if _is_inf(a) and _is_inf(b):
            return a
        return _invalid(env)
    return _op_quantize_exp(a, b._exp, rnd, env)

def _op_reduce(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special:
        return a
    r = _finalize(a._sign, a._int, a._exp, env)
    if r._is_special:
        return r
    if r._int == 0:
        return _mk(r._sign, 0, 0)
    top = env.etop() if env.clamp else env.emax
    k = min(_trailing_zeros(r._int), top - r._exp)
    if k > 0:
        return _mk(r._sign, r._int // _pow10(k), r._exp + k)
    return r

def _op_to_integral(a, rnd, exact, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special or a._exp >= 0:
        return a
    if a._int == 0:
        return _mk(a._sign, 0, 0)
    r, rest = _rescale_raw(a, 0, rnd)
    if exact:
        env.status |= _ROUNDED
        if rest:
            env.status |= _INEXACT
    return r

def _op_plus(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special:
        return a
    sign = a._sign
    if a._int == 0 and sign:
        sign = 1 if env.rnd == ROUND_FLOOR else 0
    return _finalize(sign, a._int, a._exp, env)

def _op_minus(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special:
        return _inf(a._sign ^ 1)
    sign = a._sign ^ 1
    if a._int == 0:
        # 0 - a: only +0 - (+0) under ROUND_FLOOR gives -0
        sign = 1 if (env.rnd == ROUND_FLOOR and not a._sign) else 0
    return _finalize(sign, a._int, a._exp, env)

def _op_abs(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    return _op_minus(a, env) if a._sign else _op_plus(a, env)

def _op_logb(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special:
        return _inf(0)
    if a._int == 0:
        env.status |= _DIV_BY_ZERO
        return _inf(1)
    adj = _adjusted(a)
    return _finalize(1 if adj < 0 else 0, abs(adj), 0, env)

def _int_arg(b):
    """Integral value of b if it is finite with exponent 0, else None."""
    if b._is_special or b._exp != 0:
        return None
    return -b._int if b._sign else b._int

def _op_scaleb(a, b, env):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    n = _int_arg(b)
    if n is None:
        return _invalid(env)
    lim = 2 * (env.emax + env.prec)
    if n > lim or n < -lim:
        return _invalid(env)
    if a._is_special:
        return a
    return _finalize(a._sign, a._int, a._exp + n, env)

def _op_shift(a, b, env, rotate):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    n = _int_arg(b)
    if n is None or n > env.prec or n < -env.prec:
        return _invalid(env)
    if a._is_special:
        return a
    p = _pow10(env.prec)
    c = a._int % p
    if rotate:
        k = n % env.prec if env.prec else 0
        c = (c * _pow10(k)) % p + c // _pow10(env.prec - k)
    elif n >= 0:
        c = (c * _pow10(n)) % p
    else:
        c = c // _pow10(-n)
    return _mk(a._sign, c, a._exp)

def _logical_digits(a, width):
    if a._is_special or a._sign or a._exp != 0:
        return None
    s = _int_to_str(a._int)
    if s.strip('01'):
        return None
    if len(s) > width:
        s = s[-width:]
    return s.rjust(width, '0')

def _op_logical(a, b, env, op):
    width = min(env.prec, max(_ndigits(a._int) if not a._is_special else 1,
                              _ndigits(b._int) if not b._is_special else 1))
    x, y = _logical_digits(a, width), _logical_digits(b, width)
    if x is None or y is None:
        return _invalid(env)
    if op == '&':
        out = ''.join('1' if p == '1' and q == '1' else '0' for p, q in zip(x, y))
    elif op == '|':
        out = ''.join('1' if p == '1' or q == '1' else '0' for p, q in zip(x, y))
    else:
        out = ''.join('1' if p != q else '0' for p, q in zip(x, y))
    return _mk(0, int(out.lstrip('0') or '0'), 0)

def _op_invert(a, env):
    x = _logical_digits(a, env.prec)
    if x is None:
        return _invalid(env)
    out = ''.join('0' if ch == '1' else '1' for ch in x)
    return _mk(0, _str_to_int(out.lstrip('0') or '0'), 0)

def _op_next(a, env, up):
    r = _nan1(a, env)
    if r is not None:
        return r
    if _is_inf(a):
        if a._sign == (1 if up else 0):
            return _mk(a._sign, _pow10(env.prec) - 1, env.etop())
        return a
    e = env.clone(ROUND_CEILING if up else ROUND_FLOOR)
    fixed = _finalize(a._sign, a._int, a._exp, e)
    if fixed._is_special or _cmp_num(fixed, a) != 0:
        return fixed
    tiny = _mk(0 if up else 1, 1, env.etiny() - 1)
    return _op_add(a, tiny, e)

def _op_next_toward(a, b, env):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    o = _cmp_num(a, b)
    if o == 0:
        if a._is_special:
            return _mk_special(b._sign, a._exp, a._int)
        return _mk(b._sign, a._int, a._exp)
    r = _op_next(a, env, o < 0)
    if _is_inf(r):
        env.status |= _OVERFLOW | _INEXACT | _ROUNDED
    elif _adjusted(r) < env.emin:
        env.status |= _UNDERFLOW | _SUBNORMAL | _INEXACT | _ROUNDED
        if r._int == 0:
            env.status |= _CLAMPED
    return r

def _number_class(a, env):
    if a._is_special:
        if a._exp == 'N':
            return 'sNaN'
        if a._exp == 'n':
            return 'NaN'
        return '-Infinity' if a._sign else '+Infinity'
    s = '-' if a._sign else '+'
    if a._int == 0:
        return s + 'Zero'
    if _adjusted(a) < env.emin:
        return s + 'Subnormal'
    return s + 'Normal'


# ----------------------------------------------------------------------------
# Square root, exponential, logarithms, power.  Transcendental results are
# computed as fixed-point approximations with an error bound; a result is
# accepted when every value in the error interval rounds the same way.

def _op_sqrt(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    ideal = a._exp // 2 if not a._is_special else 0
    if not a._is_special and a._int == 0:
        return _finalize(a._sign, 0, ideal, env)
    if a._sign:
        return _invalid(env)
    if a._is_special:
        return a
    prec = env.prec
    d = _ndigits(a._int)
    s = max(2 * (prec + 1) - d, 0)
    if (a._exp - s) % 2:
        s += 1
    n = a._int * _pow10(s)
    root = _math.isqrt(n)
    exp = (a._exp - s) // 2
    rem = n - root * root
    if rem == 0:
        if exp < ideal:
            k = min(_trailing_zeros(root), ideal - exp)
            if k:
                root //= _pow10(k)
                exp += k
        rest = 0
    else:
        rest = 1 if rem <= root else 3
    e = env.clone(ROUND_HALF_EVEN)
    res = _finalize(0, root, exp, e, rest)
    env.status |= e.status
    return res

def _exp_fixed(r, w):
    """e**(r/10**w) * 10**w for |r| < ~4*10**w, error at most a few units."""
    if r == 0:
        return _pow10(w)
    k = max(1, int(_math.ceil(_math.sqrt(w * 3.3) * 0.75)))
    g = int(_math.ceil(k * 0.30103)) + 5
    w2 = w + g
    one = _pow10(w2)
    neg = r < 0
    x = (abs(r) * _pow10(g)) >> k
    total = one
    term = one
    i = 1
    while True:
        term = term * x // one // i
        if not term:
            break
        if neg and i & 1:
            total -= term
        else:
            total += term
        i += 1
    for _ in range(k):
        total = total * total // one
    return total // _pow10(g)

def _ln_fixed(m, w):
    """ln(m/10**w) * 10**w for m/10**w in (1e-3, 1e3), error at most a few units."""
    g = 6
    w2 = w + g
    mm = m * _pow10(g)
    y0 = _math.log(mm) - w2 * _math.log(10)
    y0i = round(y0 * 1e17)
    y0w = y0i * _pow10(w2 - 17)
    einv = _exp_fixed(-y0w, w2)
    one = _pow10(w2)
    z = mm * einv // one
    num = z - one
    s_neg = num < 0
    s = abs(num) * one // (z + one)
    s2 = s * s // one
    total = s
    term = s
    i = 3
    while True:
        term = term * s2 // one
        if not term:
            break
        t = term // i
        if not t:
            break
        total += t
        i += 2
    lnz = -2 * total if s_neg else 2 * total
    v = y0w + lnz
    return -((-v) // _pow10(g)) if v < 0 else v // _pow10(g)

_LN10 = [0, 0]

def _ln10(w):
    cw, cv = _LN10
    if cw >= w and cv:
        return cv // _pow10(cw - w)
    ww = max(w, 64) + 16
    v = _ln_fixed(10 * _pow10(ww), ww)
    _LN10[0], _LN10[1] = ww, v
    return v // _pow10(ww - w)

def _trunc_div(v, p):
    return -((-v) // p) if v < 0 else v // p

def _ziv(e, k, exp, sign, env):
    """Accept the approximation e*10**exp (|error| < k units) if it rounds uniquely."""
    if e <= k:
        return None
    c1 = env.clone()
    r1 = _finalize(sign, e - k, exp, c1, 1)
    c2 = env.clone()
    r2 = _finalize(sign, e + k - 1, exp, c2, 3)
    if r1._is_special != r2._is_special:
        return None
    if r1._is_special:
        same = r1._exp == r2._exp and r1._sign == r2._sign
    else:
        same = r1._int == r2._int and r1._exp == r2._exp and r1._sign == r2._sign
    if not same:
        return None
    env.status |= c1.status | c2.status | _INEXACT | _ROUNDED
    return r1

def _near_one(above, env):
    p = max(env.prec, 1) + 3
    c = _pow10(p) + (1 if above else -1)
    r = _finalize(0, c, -p, env, 1)
    env.status |= _INEXACT | _ROUNDED
    return r

def _out_of_range(over, sign, env):
    if over:
        return _overflow_result(sign, env)
    r = _finalize(sign, 1, env.etiny() - 2, env, 1)
    env.status |= _INEXACT | _ROUNDED
    return r

def _to_float_est(a):
    """Rough float value of finite a (inf on overflow)."""
    try:
        return float(_to_str(a))
    except (OverflowError, ValueError):
        return _math.inf if not a._sign else -_math.inf

def _op_exp(a, env):
    r = _nan1(a, env)
    if r is not None:
        return r
    if a._is_special:
        return _mk(0, 0, 0) if a._sign else a
    if a._int == 0:
        return _finalize(0, 1, 0, env)
    e = env.clone(ROUND_HALF_EVEN)
    res = _exp_inner(a, e)
    env.status |= e.status
    return res

def _exp_inner(a, env):
    p = env.prec
    adj = _adjusted(a)
    if adj < -(p + 3):
        return _near_one(not a._sign, env)
    if adj >= 20:
        return _out_of_range(not a._sign, 0, env)
    xf = _to_float_est(a)
    n_est = xf / _math.log(10)
    if n_est > env.emax + 2:
        return _out_of_range(True, 0, env)
    if n_est < env.etiny() - 2:
        return _out_of_range(False, 0, env)
    n = _math.floor(n_est)
    w = p + 10
    while True:
        w2 = w + len(str(abs(n))) + 4
        x = _fixed_value(a, w2)
        r = x - n * _ln10(w2)
        ev = _exp_fixed(r, w2) // _pow10(w2 - w)
        res = _ziv(ev, 3, n - w, 0, env)
        if res is not None:
            return res
        w *= 2

def _fixed_value(a, w):
    """Value of finite a times 10**w, truncated toward zero."""
    e = a._exp + w
    v = a._int * _pow10(e) if e >= 0 else a._int // _pow10(-e)
    return -v if a._sign else v

def _minus_one(a):
    s, c, e = _add_exact(a, a._sign, _mk(0, 1, 0), 1, _max_env())
    return _mk(s, c, e)

def _ln_extra(a):
    adj = _adjusted(a)
    if adj in (0, -1):
        d = _minus_one(a)
        if d._int and _adjusted(d) < 0:
            return -_adjusted(d)
    return 0

def _ln_value(a, w):
    """ln(a) * 10**w (finite a > 0), error at most a few units."""
    t = _adjusted(a)
    w2 = w + len(str(abs(t))) + 4
    nd = _ndigits(a._int)
    sh = w2 - (nd - 1)
    mm = a._int * _pow10(sh) if sh >= 0 else a._int // _pow10(-sh)
    v = _ln_fixed(mm, w2) + t * _ln10(w2)
    return _trunc_div(v, _pow10(w2 - w))

def _op_log(a, env, base10):
    r = _nan1(a, env)
    if r is not None:
        return r
    if not a._is_special and a._int == 0:
        return _inf(1)
    if a._sign:
        return _invalid(env)
    if a._is_special:
        return a
    e = env.clone(ROUND_HALF_EVEN)
    res = _log_inner(a, e, base10)
    env.status |= e.status
    return res

def _log_inner(a, env, base10):
    c = a._int
    if c == _pow10(_ndigits(c) - 1):
        t = _adjusted(a)
        if t == 0:
            return _finalize(0, 0, 0, env)
        if base10:
            return _finalize(1 if t < 0 else 0, abs(t), 0, env)
    w = env.prec + _ln_extra(a) + 10
    while True:
        v = _ln_value(a, w + 3)
        k = 3
        if base10:
            l10 = _ln10(w + 5)
            v = _trunc_div(v * _pow10(w + 5), l10)
            k = 4
        v = _trunc_div(v, 1000)
        res = _ziv(abs(v), k, -w, 1 if v < 0 else 0, env)
        if res is not None:
            return res
        w *= 2

def _is_integer(a):
    if a._is_special:
        return False
    if a._exp >= 0 or a._int == 0:
        return True
    return a._int % _pow10(-a._exp) == 0

def _is_odd_integer(a):
    if not _is_integer(a) or a._exp > 0 or a._int == 0:
        return False
    return (a._int // _pow10(-a._exp)) & 1 == 1

def _int_value(a):
    if a._exp >= 0:
        if a._exp > 40:
            return None
        v = a._int * _pow10(a._exp)
    else:
        v = a._int // _pow10(-a._exp)
    return -v if a._sign else v

def _iroot_exact(n, k):
    """Exact integer k-th root of n (k = 2 or 5) or None."""
    if k == 2:
        r = _math.isqrt(n)
        return r if r * r == n else None
    if n < 2:
        return n
    # Newton iteration from above
    x = 1 << (-(-n.bit_length() // 5) + 1)
    while True:
        y = (4 * x + n // (x ** 4)) // 5
        if y >= x:
            break
        x = y
    for cand in (x - 1, x, x + 1):
        if cand >= 0 and cand ** 5 == n:
            return cand
    return None

def _power_exact(xc, xe, y, maxdig):
    """x**y exactly (x = xc*10**xe > 0, xc not divisible by 10, y not integral)
    when it has at most maxdig digits; else None."""
    yc, ye = y._int, y._exp
    tz = _trailing_zeros(yc)
    yc //= _pow10(tz)
    ye += tz
    if ye >= 0:
        return None
    if xc == 1 and xe == 0:
        return (1, 0)
    k = -ye
    if k > 4000:
        return None
    p, twos, fives = yc, k, k
    while twos and p % 2 == 0:
        p //= 2
        twos -= 1
    while fives and p % 5 == 0:
        p //= 5
        fives -= 1
    q_log10 = twos * 0.30103 + fives * 0.69897
    if q_log10 > 30 or 10 ** q_log10 > _ndigits(xc) * 3.33 + 1:
        if xc != 1:
            return None
        root = 1
    else:
        root = xc
        for _ in range(twos):
            root = _iroot_exact(root, 2)
            if root is None:
                return None
        for _ in range(fives):
            root = _iroot_exact(root, 5)
            if root is None:
                return None
    q = 2 ** twos * 5 ** fives
    if xe % q:
        return None
    re = xe // q
    if p > 10**12 and root != 1:
        return None
    neg = y._sign
    if root == 1:
        e = re * p
        return (1, -e if neg else e)
    if not neg:
        if _math.log10(root) * p > maxdig + 1:
            return None
        c = root ** p
        if _ndigits(c) > maxdig:
            return None
        return (c, re * p)
    r, i2, j5 = root, 0, 0
    while r % 2 == 0:
        r //= 2
        i2 += 1
    while r % 5 == 0:
        r //= 5
        j5 += 1
    if r != 1:
        return None
    base, cnt = (5, i2) if i2 else (2, j5)
    tot = cnt * p
    if tot * _math.log10(base) > maxdig + 1:
        return None
    c = base ** tot
    if _ndigits(c) > maxdig:
        return None
    return (c, -(re * p) - tot)

def _op_power(x, y, env):
    r = _nan2(x, y, env)
    if r is not None:
        return r
    y_int = _is_integer(y)
    y_odd = y_int and _is_odd_integer(y)
    if not y._is_special and y._int == 0:
        if not x._is_special and x._int == 0:
            return _invalid(env)
        return _finalize(0, 1, 0, env)
    if not x._is_special and x._int == 0:
        sign = x._sign & y_odd
        return _inf(sign) if y._sign else _mk(sign, 0, 0)
    if x._sign and not y_int:
        return _invalid(env)
    if _is_inf(x):
        sign = x._sign & y_odd
        return _mk(sign, 0, 0) if y._sign else _inf(sign)
    if _is_inf(y):
        o = _cmp_abs(x, _mk(0, 1, 0))
        if o < 0:
            return _inf(0) if y._sign else _mk(0, 0, 0)
        if o > 0:
            return _mk(0, 0, 0) if y._sign else _inf(0)
        env.status |= _INEXACT | _ROUNDED
        return _mk(0, _pow10(env.prec - 1), -(env.prec - 1))
    sign = 1 if (x._sign and y_odd) else 0
    tz = _trailing_zeros(x._int)
    xc = x._int // _pow10(tz)
    xe = x._exp + tz
    prec = env.prec
    if y_int:
        n = _int_value(y)
        if n is not None:
            est = _math.log10(xc) * abs(n) if xc > 1 else 0.0
            if est <= 3 * prec + 1000:
                return _int_power(x, xc, tz, n, sign, env)
    else:
        ex = _power_exact(xc, xe, y, prec + 1)
        if ex is not None:
            c, e = ex
            nd = _ndigits(c)
            if nd <= prec:
                pad = prec + 1 - nd
                c *= _pow10(pad)
                e -= pad
            sub = env.clone()
            res = _finalize(sign, c, e, sub)
            st = sub.status | _INEXACT | _ROUNDED
            if st & _SUBNORMAL:
                st |= _UNDERFLOW
            env.status |= st
            return res
    return _power_general(_mk(0, x._int, x._exp), y, sign, env)

def _int_power(x, xc, tz, n, sign, env):
    m = abs(n)
    pc = xc ** m
    zeros = tz * m
    exp = x._exp * m
    if zeros:
        cap = max(env.prec + 2 - _ndigits(pc), 0)
        if zeros > cap:
            exp += zeros - cap
            zeros = cap
    coef = pc * _pow10(zeros)
    if n > 0:
        return _finalize(sign, coef, exp, env)
    return _op_div(_mk(0, 1, 0), _mk(sign, coef, exp), env)

def _power_general(x, y, sign, env):
    """|x|**y via exp(y*ln|x|) with correct rounding (x > 0 finite)."""
    p = env.prec
    adj = _adjusted(x)
    if adj in (0, -1):
        d = _minus_one(x)
        if d._int == 0:
            return _finalize(0, 1, 0, env)
        if _adjusted(d) >= -1:
            lm, le = _math.log(_to_float_est(x)), 0
        else:
            dl = _ndigits(d._int)
            top = d._int // _pow10(max(dl - 17, 0))
            lm = (-1 if d._sign else 1) * float(top)
            le = d._exp + max(dl - 17, 0)
    else:
        dl = _ndigits(x._int)
        top = x._int // _pow10(max(dl - 17, 0))
        lm = (_math.log10(top) + max(dl - 17, 0) + x._exp) * _math.log(10)
        le = 0
    yd = _ndigits(y._int)
    ytop = y._int // _pow10(max(yd - 17, 0))
    ym = (-1 if y._sign else 1) * float(ytop)
    ye = y._exp + max(yd - 17, 0)
    tm = lm * ym
    te = le + ye
    t_log10 = _math.log10(abs(tm)) + te
    if t_log10 < -(p + 4):
        r = _near_one(tm > 0, env)
        return _mk(sign, r._int, r._exp) if sign else r
    if t_log10 > 21:
        return _out_of_range(tm > 0, sign, env)
    tf = tm * 10.0 ** te
    n_est = tf / _math.log(10)
    if n_est > env.emax + 2:
        return _out_of_range(True, sign, env)
    if n_est < env.etiny() - 2:
        return _out_of_range(False, sign, env)
    n = _math.floor(n_est)
    yadj = max(_adjusted(y), 0)
    extra = int(_math.ceil(-t_log10)) if t_log10 < 0 else 0
    w = p + 10
    while True:
        w2 = w + len(str(abs(n))) + 4
        wl = w2 + yadj + 2 + extra
        lv = _ln_value(x, wl)
        ty = lv * y._int
        if y._sign:
            ty = -ty
        shift = y._exp + w2 - wl
        t = ty * _pow10(shift) if shift >= 0 else _trunc_div(ty, _pow10(-shift))
        r = t - n * _ln10(w2)
        ev = _exp_fixed(r, w2) // _pow10(w2 - w)
        res = _ziv(ev, 5, n - w, sign, env)
        if res is not None:
            return res
        w *= 2

def _op_power_mod(a, b, m, env):
    if _is_nan(a) or _is_nan(b) or _is_nan(m):
        for v in (a, b, m):
            if v._is_special and v._exp == 'N':
                env.status |= _INVALID
                return _fix_nan(v, env)
        for v in (a, b, m):
            if _is_nan(v):
                return _fix_nan(v, env)
    if not (_is_integer(a) and _is_integer(b) and _is_integer(m)):
        return _invalid(env)
    if b._sign and b._int:
        return _invalid(env)
    if m._int == 0:
        return _invalid(env)
    mod = m._int * _pow10(m._exp) if m._exp >= 0 else m._int // _pow10(-m._exp)
    if _ndigits(mod) > env.prec:
        return _invalid(env)
    if a._int == 0 and b._int == 0:
        return _invalid(env)
    sign = 1 if (a._sign and _is_odd_integer(b)) else 0
    if a._exp >= 0:
        base = (a._int % mod) * pow(10, a._exp, mod) % mod
    else:
        base = (a._int // _pow10(-a._exp)) % mod
    if b._exp >= 0:
        e = pow(10, b._exp) if b._exp < 1000 else None
        if e is None:
            r = pow(base, b._int, mod)
            for _ in range(b._exp):
                r = pow(r, 10, mod)
            return _mk(sign, r, 0)
        ev = b._int * e
    else:
        ev = b._int // _pow10(-b._exp)
    return _mk(sign, pow(base, ev, mod), 0)


# ----------------------------------------------------------------------------
# Format specification mini-language

_CHAR_MAX = 127

class _Spec:
    __slots__ = ('fill', 'align', 'sign', 'no_neg_0', 'alt', 'zeropad', 'width',
                 'thousands_sep', 'precision', 'frac_sep', 'type')

def _parse_spec(spec):
    def bad():
        return ValueError("Invalid format specifier %r for object of type 'Decimal'" % spec)
    s = _Spec()
    s.fill, s.align, s.sign = ' ', '>', '-'
    s.no_neg_0 = s.alt = s.zeropad = False
    s.width, s.thousands_sep, s.precision, s.frac_sep, s.type = 0, None, None, None, None
    i, n = 0, len(spec)
    have_fill = have_align = False
    if n >= 2 and spec[1] in '<>=^':
        s.fill, s.align = spec[0], spec[1]
        have_fill = have_align = True
        i = 2
    elif n >= 1 and spec[0] in '<>=^':
        s.align = spec[0]
        have_align = True
        i = 1
    if i < n and spec[i] in '+- ':
        s.sign = spec[i]
        i += 1
    if i < n and spec[i] == 'z':
        s.no_neg_0 = True
        i += 1
    if i < n and spec[i] == '#':
        s.alt = True
        i += 1
    if i < n and spec[i] == '0':
        s.zeropad = True
        i += 1
    j = i
    while i < n and spec[i] in '0123456789':
        i += 1
    if i > j:
        s.width = int(spec[j:i])
    if i < n and spec[i] in ',_':
        s.thousands_sep = spec[i]
        i += 1
    if i < n and spec[i] == '.':
        i += 1
        j = i
        while i < n and spec[i] in '0123456789':
            i += 1
        if i > j:
            s.precision = int(spec[j:i])
        if i < n and spec[i] in ',_':
            s.frac_sep = spec[i]
            i += 1
        if s.precision is None and s.frac_sep is None:
            raise bad()
    if i < n and spec[i] in 'eEfFgGn%':
        s.type = spec[i]
        i += 1
    if i != n:
        raise bad()
    if s.zeropad:
        if not have_fill:
            s.fill = '0'
        if not have_align:
            s.align = '='
    if s.type in (None, 'g', 'G', 'n') and s.precision == 0:
        s.precision = 1
    if s.type == 'n' and s.thousands_sep is not None:
        raise ValueError("Cannot specify ',' with 'n'.")
    return s

def _round_sig(d, n, rnd):
    if d._is_special or d._int == 0:
        return d
    r, _ = _rescale_raw(d, _adjusted(d) - n + 1, rnd)
    if _ndigits(r._int) > n:
        r = _mk(r._sign, r._int // 10, r._exp + 1)
    return r

def _rescale_any(d, exp, rnd):
    if d._int == 0:
        return _mk(d._sign, 0, exp)
    return _rescale_raw(d, exp, rnd)[0]

def _group_sizes(n, grouping):
    sizes = []
    remaining = n
    repeat = None
    pos = 0
    frozen = False
    while remaining > 0:
        if frozen or pos >= len(grouping):
            size = repeat if repeat is not None else remaining
        else:
            g = grouping[pos]
            pos += 1
            if g >= _CHAR_MAX:
                repeat, frozen, size = None, True, remaining
            elif g == 0:
                frozen = True
                size = repeat if repeat is not None else remaining
            else:
                repeat = size = g
        take = max(min(size, remaining), 1)
        sizes.append(take)
        remaining -= take
    return sizes or [0]

def _group_int(digits, sep, grouping, min_width):
    def length(k):
        return k + len(sep) * (len(_group_sizes(k, grouping)) - 1)
    n = len(digits)
    pad = 0
    if length(n) < min_width:
        lo, hi = 1, min_width
        while lo < hi:
            mid = (lo + hi) // 2
            if length(n + mid) >= min_width:
                hi = mid
            else:
                lo = mid + 1
        pad = lo
    full = '0' * pad + digits
    if not sep:
        return full
    parts = []
    end = len(full)
    for size in _group_sizes(len(full), grouping):
        parts.append(full[end - size:end])
        end -= size
    return sep.join(reversed(parts))

def _group_frac(frac, sep):
    return sep.join(frac[i:i + 3] for i in range(0, len(frac), 3))

def _pad(sign, body, s):
    missing = max(s.width - len(sign) - len(body), 0)
    if s.align == '<':
        return sign + body + s.fill * missing
    if s.align == '=':
        return sign + s.fill * missing + body
    if s.align == '^':
        half = missing // 2
        return s.fill * half + sign + body + s.fill * (missing - half)
    return s.fill * missing + sign + body

def _sign_text(neg, s):
    if neg:
        return '-'
    return {'+': '+', ' ': ' '}.get(s.sign, '')

def _layout_fixed(digits, exp):
    if exp >= 0:
        return digits + '0' * exp, '', None
    k = len(digits) + exp
    if k > 0:
        return digits[:k], digits[k:], None
    return '0', '0' * (-k) + digits, None

def _format(d, spec, context, localeconv):
    s = _parse_spec(spec)
    capitals = context.capitals
    rnd = context.rounding
    if d._is_special:
        body = _to_str(_mk_abs(d))
        if s.type == '%':
            body += '%'
        return _pad(_sign_text(d._sign, s), body, s)
    ty = s.type or ('G' if capitals else 'g')
    x = d
    if ty == '%':
        x = _mk(x._sign, x._int, x._exp + 2)
    p = s.precision
    if p is not None:
        if ty in 'eE':
            x = _round_sig(x, p + 1, rnd)
        elif ty in 'fF%':
            x = _rescale_any(x, -p, rnd)
        elif _ndigits(x._int) > p:
            x = _round_sig(x, p, rnd)
    if ty in 'fF%' and x._int == 0 and x._exp > 0:
        x = _rescale_any(x, 0, rnd)
    neg = bool(x._sign) and not (s.no_neg_0 and x._int == 0)
    digits = _int_to_str(x._int)
    exp = x._exp
    adj = exp + len(digits) - 1
    if ty in 'eE':
        if p is not None and x._int == 0:
            ip, fp, ex = '0', '0' * p, exp + p
        else:
            ip, fp, ex = digits[0], digits[1:], adj
    elif ty in 'fF%':
        ip, fp, ex = _layout_fixed(digits, exp)
    elif exp <= 0 and adj >= -6:
        ip, fp, ex = _layout_fixed(digits, exp)
    else:
        ip, fp, ex = digits[0], digits[1:], adj
    if ty == 'n':
        if localeconv is None:
            import locale as _locale
            localeconv = _locale.localeconv()
        point = localeconv['decimal_point']
        tsep = localeconv['thousands_sep']
        grouping = list(localeconv['grouping'])
    elif s.thousands_sep:
        point, tsep, grouping = '.', s.thousands_sep, [3]
    else:
        point, tsep, grouping = '.', '', []
    tail = ''
    if fp or s.alt:
        tail = point + (_group_frac(fp, s.frac_sep) if s.frac_sep else fp)
    if ex is not None and (ex != 0 or ty in 'eE'):
        tail += ('E' if ty in 'EG' else 'e') + ('-' if ex < 0 else '+') + str(abs(ex))
    if ty == '%':
        tail += '%'
    sign = _sign_text(neg, s)
    min_int = 0
    if s.zeropad and s.fill == '0' and s.align == '=':
        min_int = max(s.width - len(tail) - len(sign), 0)
    return _pad(sign, _group_int(ip, tsep, grouping, min_int) + tail, s)


# ----------------------------------------------------------------------------
# Decimal

def _float_to_parts(f):
    """Exact (sign, coefficient, exponent) of a finite float."""
    sign = 1 if _math.copysign(1.0, f) < 0 else 0
    if f == 0:
        return sign, 0, 0
    n, d = abs(f).as_integer_ratio()
    k = d.bit_length() - 1        # d is a power of two
    return sign, n * 5 ** k, -k

def _from_float(f):
    if f != f:
        return _mk_special(1 if _math.copysign(1.0, f) < 0 else 0, 'n', 0)
    if f in (_math.inf, -_math.inf):
        return _inf(1 if f < 0 else 0)
    s, c, e = _float_to_parts(f)
    return _mk(s, c, e)

def _from_int(i):
    return _mk(1 if i < 0 else 0, abs(int(i)), 0)

def _numeric_ascii(s, strip):
    """ASCII form of a numeric string (unicode digits translated), or None."""
    if strip:
        s = s.strip().replace('_', '')
    if s.isascii():
        return s
    out = []
    for ch in s:
        if ch < '\x80':
            out.append(ch)
        elif ch.isdecimal():
            out.append(str(int(ch)))
        else:
            return None
    return ''.join(out)

def _from_tuple(v):
    if len(v) != 3:
        raise ValueError('argument must be a sequence of length 3')
    sign, digits, exp = v
    if not (isinstance(sign, int) and sign in (0, 1)):
        raise ValueError('sign must be an integer with the value 0 or 1')
    if not isinstance(digits, (list, tuple)):
        raise ValueError('coefficient must be a tuple of digits')
    coef = 0
    for dg in digits:
        if not isinstance(dg, int) or not 0 <= dg <= 9:
            raise ValueError('coefficient must be a tuple of digits')
        coef = coef * 10 + dg
    if isinstance(exp, str):
        if exp == 'F':
            return _inf(sign)
        if exp in ('n', 'N'):
            return _mk_special(sign, exp, coef)
        raise ValueError("string argument in the third position must be 'F', 'n' or 'N'")
    if not isinstance(exp, int):
        raise ValueError('exponent must be an integer')
    if not -_sys.maxsize - 1 <= exp <= _sys.maxsize:
        raise OverflowError('exponent must be an integer')
    return _mk(sign, coef, exp)

# Decimal(...) of a value beyond the accelerator's limits (MAX_PREC digits, exponents
# within MIN_EMIN..MAX_EMAX) is an InvalidOperation in the `decimal` module, which this
# module serves when there is no _decimal (decimal.py sets this); imported as
# `_pydecimal` itself, exponents and coefficients are unbounded.
_EXACT_LIMITS = False

def _exact(d, env):
    """Decimal(...) result: the value unchanged, or NaN if it exceeds the limits."""
    if d._is_special or not _EXACT_LIMITS:
        return d
    m = _max_env()
    r = _finalize(d._sign, d._int, d._exp, m)
    if m.status & (_INEXACT | _ROUNDED | _CLAMPED | _OVERFLOW | _UNDERFLOW):
        env.status |= _INVALID
        return _nan()
    return r

def _rounded_value(d, env):
    """create_decimal() result for an exact value."""
    if d._is_special:
        if d._exp != 'F' and d._int and _ndigits(d._int) > env.prec - env.clamp:
            env.status |= _CONV_SYNTAX
            return _nan()
        return d
    return _finalize(d._sign, d._int, d._exp, env)

def _convert(value, context, exact):
    env = context._env()
    if isinstance(value, Decimal):
        r = _copy_of(value) if exact else _rounded_value(value, env)
    elif isinstance(value, str):
        s = _numeric_ascii(value, exact)
        d = _parse(s) if s is not None else None
        if d is None:
            env.status |= _CONV_SYNTAX
            r = _nan()
        else:
            r = _exact(d, env) if exact else _rounded_value(d, env)
    elif isinstance(value, int):
        d = _from_int(value)
        r = d if exact else _finalize(d._sign, d._int, d._exp, env)
    elif isinstance(value, float):
        context._signal(_FLOAT_OP)
        d = _from_float(value)
        r = d if exact else _rounded_value(d, env)
    elif isinstance(value, (list, tuple)):
        d = _from_tuple(value)
        r = _exact(d, env) if exact else _rounded_value(d, env)
    else:
        raise TypeError("conversion from %s to Decimal is not supported" % type(value).__name__)
    context._signal(env.status)
    return r

def _copy_of(v):
    if v._is_special:
        return _mk_special(v._sign, v._exp, v._int)
    return _mk(v._sign, v._int, v._exp)

def _operand(x):
    """Decimal for an operand of an arithmetic dunder, or None."""
    if isinstance(x, Decimal):
        return x
    if isinstance(x, int):
        return _from_int(x)
    return None

def _operand_or_raise(x):
    if isinstance(x, Decimal):
        return x
    if isinstance(x, int):
        return _from_int(x)
    raise TypeError("conversion from %s to Decimal is not supported" % type(x).__name__)

def _ctx(context):
    if context is None:
        return getcontext()
    if not isinstance(context, Context):
        raise TypeError('optional argument must be a context')
    return context

def _run(context, fn, *args):
    env = context._env()
    r = fn(*args, env)
    if env.status:
        context._signal(env.status)
    return r

def _rounding_arg(rounding, context):
    if rounding is None:
        return context.rounding
    if rounding not in _ROUNDINGS:
        raise TypeError('invalid rounding mode')
    return rounding

_HASH_MODULUS = _sys.hash_info.modulus
_HASH_INF = _sys.hash_info.inf


class Decimal(object):
    """An exact decimal number (sign, integer coefficient, power-of-ten
    exponent) or an infinity or NaN.  Decimal(value='0', context=None)
    converts str, int, float, Decimal and (sign, digits, exponent) tuples
    exactly."""

    __slots__ = ('_exp', '_int', '_sign', '_is_special')

    def __new__(cls, value="0", context=None):
        context = _ctx(context)
        d = _convert(value, context, True)
        if cls is Decimal:
            return d
        obj = object.__new__(cls)
        obj._sign, obj._int, obj._exp, obj._is_special = d._sign, d._int, d._exp, d._is_special
        return obj

    @classmethod
    def from_float(cls, f):
        """Exact Decimal for a float or int."""
        if isinstance(f, float):
            d = _from_float(f)
        elif isinstance(f, int):
            d = _from_int(f)
        else:
            raise TypeError('argument must be int or float')
        return d if cls is Decimal else cls(d)

    @classmethod
    def from_number(cls, number):
        """Exact Decimal for an int, float or Decimal."""
        if isinstance(number, Decimal):
            d = number
        elif isinstance(number, float):
            d = _from_float(number)
        elif isinstance(number, int):
            d = _from_int(number)
        else:
            raise TypeError("conversion from %s to Decimal is not supported" % type(number).__name__)
        return d if cls is Decimal else cls(d)

    # --- internal helpers kept for compatibility with tests

    def _rescale(self, exp, rounding):
        """Copy rescaled to exponent exp with the given rounding (no limits)."""
        if self._is_special:
            return _copy_of(self)
        if self._int == 0:
            return _mk(self._sign, 0, exp)
        return _rescale_raw(self, exp, rounding)[0]

    def _round(self, places, rounding):
        """Copy rounded to `places` significant digits."""
        if places <= 0:
            raise ValueError('argument should be at least 1 in _round')
        return _round_sig(self, places, rounding)

    # --- conversions

    def __str__(self, eng=False, context=None):
        return _to_str(self, eng, getcontext().capitals if context is None else context.capitals)

    def __repr__(self):
        return "Decimal('%s')" % str(self)

    def to_eng_string(self, context=None):
        """String form with an exponent that is a multiple of three."""
        return _to_str(self, True, _ctx(context).capitals)

    def __bool__(self):
        return self._is_special or self._int != 0

    def __float__(self):
        if self._is_special:
            if self._exp == 'N':
                raise ValueError('cannot convert signaling NaN to float')
            if self._exp == 'n':
                return -_math.nan if self._sign else _math.nan
            return -_math.inf if self._sign else _math.inf
        return float(_to_str(self))

    def _to_int(self, rounding):
        if self._is_special:
            if self._exp == 'F':
                raise OverflowError('cannot convert Infinity to integer')
            raise ValueError('cannot convert NaN to integer')
        x = self
        if x._exp < 0:
            x = _rescale_raw(x, 0, rounding)[0]
        v = x._int * _pow10(x._exp)
        return -v if x._sign else v

    def __int__(self):
        return self._to_int(ROUND_DOWN)

    __trunc__ = __int__

    def __floor__(self):
        return self._to_int(ROUND_FLOOR)

    def __ceil__(self):
        return self._to_int(ROUND_CEILING)

    def __round__(self, n=None):
        if n is None:
            return self._to_int(ROUND_HALF_EVEN)
        if not isinstance(n, int):
            raise TypeError('Second argument to round should be integral')
        context = getcontext()
        return _run(context, _op_quantize, self, _mk(0, 1, -n), context.rounding)

    def __complex__(self):
        return complex(float(self))

    @property
    def real(self):
        return self

    @property
    def imag(self):
        return Decimal(0)

    def conjugate(self):
        """The operand itself."""
        return self

    def as_tuple(self):
        """The (sign, digits, exponent) triple as a DecimalTuple."""
        if self._is_special:
            if self._exp == 'F':
                return DecimalTuple(self._sign, (0,), 'F')
            digits = tuple(map(int, _int_to_str(self._int))) if self._int else ()
            return DecimalTuple(self._sign, digits, self._exp)
        return DecimalTuple(self._sign, tuple(map(int, _int_to_str(self._int))), self._exp)

    def as_integer_ratio(self):
        """Exact value as a reduced (numerator, denominator) pair of ints."""
        if self._is_special:
            if self._exp == 'F':
                raise OverflowError('cannot convert Infinity to integer ratio')
            raise ValueError('cannot convert NaN to integer ratio')
        if self._int == 0:
            return 0, 1
        n = self._int
        if self._exp >= 0:
            num, den = n * _pow10(self._exp), 1
        else:
            k = -self._exp
            twos = min(k, (n & -n).bit_length() - 1)
            n >>= twos
            fives = 0
            while fives < k and n % 5 == 0:
                n //= 5
                fives += 1
            num, den = n, 2 ** (k - twos) * 5 ** (k - fives)
        return (-num if self._sign else num), den

    def __reduce__(self):
        return (self.__class__, (_to_str(self),))

    def __copy__(self):
        return self

    def __deepcopy__(self, memo):
        return self

    def __format__(self, specifier, context=None, _localeconv=None):
        if not isinstance(specifier, str):
            raise TypeError('format spec must be a string')
        return _format(self, specifier, _ctx(context), _localeconv)

    def __hash__(self):
        if self._is_special:
            if self._exp == 'N':
                raise TypeError('Cannot hash a signaling NaN value.')
            if self._exp == 'n':
                return object.__hash__(self)
            return -_HASH_INF if self._sign else _HASH_INF
        if self._exp >= 0:
            m = pow(10, self._exp, _HASH_MODULUS)
        else:
            m = pow(10, -self._exp, _HASH_MODULUS)
            m = pow(m, -1, _HASH_MODULUS)
        h = self._int % _HASH_MODULUS * m % _HASH_MODULUS
        if self._sign:
            h = -h
        return -2 if h == -1 else h

    # --- comparisons

    def _compare_other(self, other, equality):
        """Turn `other` into something comparable; returns (kind, value)."""
        if isinstance(other, Decimal):
            return 'd', other
        if isinstance(other, int):
            return 'd', _from_int(other)
        if isinstance(other, float):
            context = getcontext()
            if equality:
                context.flags[FloatOperation] = True
            else:
                context._signal(_FLOAT_OP)
            return 'd', _from_float(other)
        if isinstance(other, complex):
            if not equality:
                return None, None
            if other.imag:
                return 'ne', None
            getcontext().flags[FloatOperation] = True
            return 'd', _from_float(other.real)
        if isinstance(other, _numbers.Rational):
            return 'r', other
        return None, None

    def _cmp(self, other, op):
        kind, val = self._compare_other(other, op in ('eq', 'ne'))
        if kind is None:
            return NotImplemented
        if kind == 'ne':
            return op == 'ne'
        eq = op in ('eq', 'ne')
        o_nan = kind == 'd' and _is_nan(val)
        if _is_nan(self) or o_nan:
            context = getcontext()
            snan = (self._is_special and self._exp == 'N') or (o_nan and val._exp == 'N')
            if eq:
                if snan:
                    context._signal(_INVALID)
                return op == 'ne'
            context._signal(_INVALID)
            return False
        if kind == 'r':
            if _is_inf(self):
                o = -1 if self._sign else 1
            else:
                lhs = _mk(self._sign, self._int * abs(val.denominator), self._exp)
                o = _cmp_num(lhs, _from_int(val.numerator))
        else:
            o = _cmp_num(self, val)
        return {'lt': o < 0, 'le': o <= 0, 'eq': o == 0, 'ne': o != 0,
                'gt': o > 0, 'ge': o >= 0}[op]

    def __eq__(self, other):
        return self._cmp(other, 'eq')

    def __ne__(self, other):
        return self._cmp(other, 'ne')

    def __lt__(self, other):
        return self._cmp(other, 'lt')

    def __le__(self, other):
        return self._cmp(other, 'le')

    def __gt__(self, other):
        return self._cmp(other, 'gt')

    def __ge__(self, other):
        return self._cmp(other, 'ge')

    # --- arithmetic operators

    def _binary(self, other, fn, swap=False, context=None):
        o = _operand(other)
        if o is None:
            return NotImplemented
        context = _ctx(context)
        if swap:
            return _run(context, fn, o, self)
        return _run(context, fn, self, o)

    def __add__(self, other, context=None):
        return self._binary(other, _op_add, False, context)

    def __radd__(self, other, context=None):
        return self._binary(other, _op_add, True, context)

    def __sub__(self, other, context=None):
        return self._binary(other, _op_sub, False, context)

    def __rsub__(self, other, context=None):
        return self._binary(other, _op_sub, True, context)

    def __mul__(self, other, context=None):
        return self._binary(other, _op_mul, False, context)

    def __rmul__(self, other, context=None):
        return self._binary(other, _op_mul, True, context)

    def __truediv__(self, other, context=None):
        return self._binary(other, _op_div, False, context)

    def __rtruediv__(self, other, context=None):
        return self._binary(other, _op_div, True, context)

    def __floordiv__(self, other, context=None):
        return self._binary(other, _op_divint, False, context)

    def __rfloordiv__(self, other, context=None):
        return self._binary(other, _op_divint, True, context)

    def __mod__(self, other, context=None):
        return self._binary(other, _op_rem, False, context)

    def __rmod__(self, other, context=None):
        return self._binary(other, _op_rem, True, context)

    def __divmod__(self, other, context=None):
        return self._binary(other, _op_divmod, False, context)

    def __rdivmod__(self, other, context=None):
        return self._binary(other, _op_divmod, True, context)

    def __pow__(self, other, modulo=None, context=None):
        o = _operand(other)
        if o is None:
            return NotImplemented
        context = _ctx(context)
        if modulo is None:
            return _run(context, _op_power, self, o)
        m = _operand(modulo)
        if m is None:
            return NotImplemented
        return _run(context, _op_power_mod, self, o, m)

    def __rpow__(self, other, modulo=None, context=None):
        o = _operand(other)
        if o is None:
            return NotImplemented
        return o.__pow__(self, modulo, context)

    def __neg__(self, context=None):
        return _run(_ctx(context), _op_minus, self)

    def __pos__(self, context=None):
        return _run(_ctx(context), _op_plus, self)

    def __abs__(self, round=True, context=None):
        if not round:
            return self.copy_abs()
        return _run(_ctx(context), _op_abs, self)

    # --- named operations

    def exp(self, context=None):
        """Exponential function, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(_ctx(context), _op_exp, self)

    def ln(self, context=None):
        """Natural logarithm, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(_ctx(context), _op_ln, self)

    def log10(self, context=None):
        """Base-10 logarithm, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(_ctx(context), _op_log10, self)

    def logb(self, context=None):
        """Adjusted exponent of the operand as a Decimal (-Infinity for zero)."""
        return _run(_ctx(context), _op_logb, self)

    def logical_invert(self, context=None):
        """Digit-wise NOT of a logical operand (digits 0/1), padded to the precision."""
        return _run(_ctx(context), _op_invert, self)

    def next_minus(self, context=None):
        """Largest number representable in the context that is below the operand."""
        return _run(_ctx(context), _op_next_minus, self)

    def next_plus(self, context=None):
        """Smallest number representable in the context that is above the operand."""
        return _run(_ctx(context), _op_next_plus, self)

    def normalize(self, context=None):
        """Round to the context and strip trailing zeros from the coefficient."""
        return _run(_ctx(context), _op_reduce, self)

    def sqrt(self, context=None):
        """Square root, correctly rounded to the context precision."""
        return _run(_ctx(context), _op_sqrt, self)

    def is_normal(self, context=None):
        """True for finite nonzero operands whose adjusted exponent is at least Emin."""
        context = _ctx(context)
        return not self._is_special and self._int != 0 and _adjusted(self) >= context.Emin

    def is_subnormal(self, context=None):
        """True for finite nonzero operands whose adjusted exponent is below Emin."""
        context = _ctx(context)
        return not self._is_special and self._int != 0 and _adjusted(self) < context.Emin

    def number_class(self, context=None):
        """Name of the operand's class, e.g. '+Normal', '-Zero', 'sNaN'."""
        return _number_class(self, _ctx(context)._env())

    def compare(self, other, context=None):
        """Numeric comparison returning Decimal -1, 0 or 1 (NaN if a NaN is involved)."""
        return _run(_ctx(context), _op_compare, self, _operand_or_raise(other))

    def compare_signal(self, other, context=None):
        """Like compare(), but any NaN operand signals InvalidOperation."""
        return _run(_ctx(context), _op_compare_signal, self, _operand_or_raise(other))

    def compare_total(self, other, context=None):
        """Total ordering comparison on the representation (Decimal -1, 0 or 1)."""
        return _ord_dec(_total_cmp(self, _operand_or_raise(other)))

    def compare_total_mag(self, other, context=None):
        """compare_total() applied to the absolute values."""
        return _ord_dec(_total_cmp_abs(self, _operand_or_raise(other)))

    def copy_abs(self):
        """Copy with the sign cleared; no rounding and no signals."""
        return _mk_abs(self)

    def copy_negate(self):
        """Copy with the sign flipped; no rounding and no signals."""
        if self._is_special:
            return _mk_special(self._sign ^ 1, self._exp, self._int)
        return _mk(self._sign ^ 1, self._int, self._exp)

    def copy_sign(self, other, context=None):
        """Copy of the first operand carrying the sign of the second."""
        other = _operand_or_raise(other)
        if self._is_special:
            return _mk_special(other._sign, self._exp, self._int)
        return _mk(other._sign, self._int, self._exp)

    def max(self, other, context=None):
        """The numerically larger operand, rounded to the context."""
        return _run(_ctx(context), _op_max, self, _operand_or_raise(other))

    def max_mag(self, other, context=None):
        """The operand with the larger absolute value, rounded to the context."""
        return _run(_ctx(context), _op_max_mag, self, _operand_or_raise(other))

    def min(self, other, context=None):
        """The numerically smaller operand, rounded to the context."""
        return _run(_ctx(context), _op_min, self, _operand_or_raise(other))

    def min_mag(self, other, context=None):
        """The operand with the smaller absolute value, rounded to the context."""
        return _run(_ctx(context), _op_min_mag, self, _operand_or_raise(other))

    def next_toward(self, other, context=None):
        """Next representable number from the first operand in the direction of the second."""
        return _run(_ctx(context), _op_next_toward, self, _operand_or_raise(other))

    def remainder_near(self, other, context=None):
        """Remainder after division with the quotient rounded to the nearest integer."""
        return _run(_ctx(context), _op_rem_near, self, _operand_or_raise(other))

    def rotate(self, other, context=None):
        """Rotate the coefficient digits (within the precision) by the given count."""
        return _run(_ctx(context), _op_rotate, self, _operand_or_raise(other))

    def scaleb(self, other, context=None):
        """Add the integral second operand to the exponent."""
        return _run(_ctx(context), _op_scaleb, self, _operand_or_raise(other))

    def shift(self, other, context=None):
        """Shift the coefficient digits (within the precision) by the given count."""
        return _run(_ctx(context), _op_shift_, self, _operand_or_raise(other))

    def logical_and(self, other, context=None):
        """Digit-wise AND of two logical operands."""
        return _run(_ctx(context), _op_and, self, _operand_or_raise(other))

    def logical_or(self, other, context=None):
        """Digit-wise OR of two logical operands."""
        return _run(_ctx(context), _op_or, self, _operand_or_raise(other))

    def logical_xor(self, other, context=None):
        """Digit-wise XOR of two logical operands."""
        return _run(_ctx(context), _op_xor, self, _operand_or_raise(other))

    def same_quantum(self, other, context=None):
        """True if both operands have the same exponent (or are both NaN / both infinite)."""
        other = _operand_or_raise(other)
        if self._is_special or other._is_special:
            return (_is_nan(self) and _is_nan(other)) or (_is_inf(self) and _is_inf(other))
        return self._exp == other._exp

    def fma(self, other, third, context=None):
        """Multiply exactly, then add the third operand with a single rounding."""
        other = _operand_or_raise(other)
        third = _operand_or_raise(third)
        return _run(_ctx(context), _op_fma, self, other, third)

    def quantize(self, exp, rounding=None, context=None):
        """Round to the exponent of the second operand."""
        exp = _operand_or_raise(exp)
        context = _ctx(context)
        rnd = _rounding_arg(rounding, context)
        return _run(context, _op_quantize, self, exp, rnd)

    def to_integral_value(self, rounding=None, context=None):
        """Round to an integer without signaling Inexact or Rounded."""
        context = _ctx(context)
        rnd = _rounding_arg(rounding, context)
        return _run(context, _op_to_integral, self, rnd, False)

    def to_integral(self, rounding=None, context=None):
        """Round to an integer; same as to_integral_value()."""
        return self.to_integral_value(rounding, context)

    def to_integral_exact(self, rounding=None, context=None):
        """Round to an integer, signaling Inexact and Rounded when appropriate."""
        context = _ctx(context)
        rnd = _rounding_arg(rounding, context)
        return _run(context, _op_to_integral, self, rnd, True)

    def adjusted(self):
        """Exponent of the most significant digit."""
        if self._is_special:
            return 0
        return _adjusted(self)

    def canonical(self):
        """The operand itself (every Decimal is canonical)."""
        return self

    def radix(self):
        """Decimal(10)."""
        return Decimal(10)

    def is_canonical(self):
        """Always True."""
        return True

    def is_finite(self):
        """True unless the operand is infinite or a NaN."""
        return not self._is_special

    def is_infinite(self):
        """True for positive or negative infinity."""
        return _is_inf(self)

    def is_nan(self):
        """True for quiet and signaling NaNs."""
        return _is_nan(self)

    def is_qnan(self):
        """True for a quiet NaN."""
        return self._is_special and self._exp == 'n'

    def is_snan(self):
        """True for a signaling NaN."""
        return self._is_special and self._exp == 'N'

    def is_signed(self):
        """True if the sign is negative (including -0 and -NaN)."""
        return self._sign == 1

    def is_zero(self):
        """True for a zero of either sign."""
        return not self._is_special and self._int == 0


_numbers.Number.register(Decimal)


def _op_sub(a, b, env):
    return _op_add(a, b, env, True)

def _op_compare(a, b, env):
    r = _nan2(a, b, env)
    if r is not None:
        return r
    return _ord_dec(_cmp_num(a, b))

def _op_compare_signal(a, b, env):
    if _is_nan(a) or _is_nan(b):
        env.status |= _INVALID
        return _nan2(a, b, env)
    return _ord_dec(_cmp_num(a, b))

def _op_max(a, b, env):
    return _op_minmax(a, b, env, True, False)

def _op_min(a, b, env):
    return _op_minmax(a, b, env, False, False)

def _op_max_mag(a, b, env):
    return _op_minmax(a, b, env, True, True)

def _op_min_mag(a, b, env):
    return _op_minmax(a, b, env, False, True)

def _op_next_minus(a, env):
    return _op_next(a, env, False)

def _op_next_plus(a, env):
    return _op_next(a, env, True)

def _op_ln(a, env):
    return _op_log(a, env, False)

def _op_log10(a, env):
    return _op_log(a, env, True)

def _op_rotate(a, b, env):
    return _op_shift(a, b, env, True)

def _op_shift_(a, b, env):
    return _op_shift(a, b, env, False)

def _op_and(a, b, env):
    return _op_logical(a, b, env, '&')

def _op_or(a, b, env):
    return _op_logical(a, b, env, '|')

def _op_xor(a, b, env):
    return _op_logical(a, b, env, '^')


# ----------------------------------------------------------------------------
# Context

_CTX_ATTRS = ('prec', 'rounding', 'Emin', 'Emax', 'capitals', 'clamp', 'flags', 'traps')

def _check_int(name, value, lo, hi, message):
    if not isinstance(value, int):
        raise TypeError('%s must be an integer' % name)
    if not lo <= value <= hi:
        raise ValueError(message)
    return int(value)

def _signal_dict_from(value):
    """Validated {signal: bool} dict from a dict or a list of signals."""
    if isinstance(value, dict):
        if len(value) != len(_SIGNALS) or any(s not in value for s in _SIGNALS):
            raise KeyError('invalid signal dict')
        return {s: bool(value[s]) for s in _SIGNALS}
    if isinstance(value, list):
        out = dict.fromkeys(_SIGNALS, False)
        for s in value:
            if s not in out:
                raise KeyError('invalid error flag')
            out[s] = True
        return out
    raise TypeError('argument must be a list or a signal dict')


class Context(object):
    """Arithmetic settings (precision, rounding, exponent range, clamping and
    exponent letter case) plus sticky flags and enabled traps.  Arguments left
    as None are taken from DefaultContext."""

    __slots__ = ('prec', 'rounding', 'Emin', 'Emax', 'capitals', 'clamp', 'flags', 'traps')

    def __init__(self, prec=None, rounding=None, Emin=None, Emax=None,
                 capitals=None, clamp=None, flags=None, traps=None):
        try:
            base = DefaultContext
        except NameError:
            base = None
        def pick(value, attr, fallback):
            if value is not None:
                return value
            return getattr(base, attr) if base is not None else fallback
        self.prec = pick(prec, 'prec', 28)
        self.rounding = pick(rounding, 'rounding', ROUND_HALF_EVEN)
        self.Emin = pick(Emin, 'Emin', -999999)
        self.Emax = pick(Emax, 'Emax', 999999)
        self.capitals = pick(capitals, 'capitals', 1)
        self.clamp = pick(clamp, 'clamp', 0)
        if flags is None:
            flags = []
        self.flags = _signal_dict_from(flags)
        if traps is None:
            traps = dict(base.traps) if base is not None else \
                [InvalidOperation, DivisionByZero, Overflow]
        self.traps = _signal_dict_from(traps)

    def __setattr__(self, name, value):
        if name == 'prec':
            value = _check_int(name, value, 1, MAX_PREC, 'valid range for prec is [1, MAX_PREC]')
        elif name == 'Emin':
            value = _check_int(name, value, MIN_EMIN, 0, 'valid range for Emin is [MIN_EMIN, 0]')
        elif name == 'Emax':
            value = _check_int(name, value, 0, MAX_EMAX, 'valid range for Emax is [0, MAX_EMAX]')
        elif name == 'capitals':
            value = _check_int(name, value, 0, 1, 'valid values for capitals are 0 or 1')
        elif name == 'clamp':
            value = _check_int(name, value, 0, 1, 'valid values for clamp are 0 or 1')
        elif name == 'rounding':
            if value not in _ROUNDINGS or not isinstance(value, str):
                raise TypeError('rounding must be one of the ROUND_* string constants')
            value = _ROUNDINGS[_ROUNDINGS.index(value)]
        elif name in ('flags', 'traps'):
            if not isinstance(value, dict):
                raise TypeError('%s must be a signal dict' % name)
            value = _signal_dict_from(value)
        object.__setattr__(self, name, value)

    def __delattr__(self, name):
        raise AttributeError('context attributes cannot be deleted: %r' % name)

    def __reduce__(self):
        flags = [s for s in _SIGNALS if self.flags[s]]
        traps = [s for s in _SIGNALS if self.traps[s]]
        return (self.__class__, (self.prec, self.rounding, self.Emin, self.Emax,
                                 self.capitals, self.clamp, flags, traps))

    def __repr__(self):
        flags = ', '.join(s.__name__ for s in _REPR_ORDER if self.flags[s])
        traps = ', '.join(s.__name__ for s in _REPR_ORDER if self.traps[s])
        return ('Context(prec=%d, rounding=%s, Emin=%d, Emax=%d, capitals=%d, clamp=%d, '
                'flags=[%s], traps=[%s])' % (self.prec, self.rounding, self.Emin, self.Emax,
                                              self.capitals, self.clamp, flags, traps))

    # --- internal

    def _env(self):
        return _Env(self.prec, self.Emin, self.Emax, self.rounding, self.clamp)

    def _signal(self, status):
        """Record conditions; raise the most important trapped one."""
        if not status:
            return
        flags = self.flags
        traps = self.traps
        raise_cls = None
        for bits, sig in _SIGNAL_BITS:
            if status & bits:
                flags[sig] = True
                if raise_cls is None and traps[sig]:
                    raise_cls = sig
                    if sig is InvalidOperation:
                        for b, cond in _CONDITION_CLASS:
                            if status & b:
                                raise_cls = cond
                                break
        if raise_cls is not None:
            raise raise_cls()

    def _raise_error(self, condition, explanation=None, *args):
        """Flag `condition`; raise it if its signal is trapped."""
        sig = _CONDITION_SIGNAL.get(condition, condition)
        self.flags[sig] = True
        if self.traps[sig]:
            raise condition(explanation)
        if issubclass(condition, InvalidOperation):
            return _nan()
        return None

    def _apply(self, a):
        return _run(self, _apply, _operand_or_raise(a))

    # --- settings

    def Etiny(self):
        """Smallest exponent of a subnormal number: Emin - prec + 1."""
        return self.Emin - self.prec + 1

    def Etop(self):
        """Largest exponent of a clamped number: Emax - prec + 1."""
        return self.Emax - self.prec + 1

    def radix(self):
        """Decimal(10)."""
        return Decimal(10)

    def clear_flags(self):
        """Set every flag to False."""
        for s in self.flags:
            self.flags[s] = False

    def clear_traps(self):
        """Set every trap to False."""
        for s in self.traps:
            self.traps[s] = False

    def copy(self):
        """A new context with the same settings, flags and traps."""
        c = object.__new__(self.__class__)
        for name in ('prec', 'rounding', 'Emin', 'Emax', 'capitals', 'clamp'):
            object.__setattr__(c, name, getattr(self, name))
        object.__setattr__(c, 'flags', dict(self.flags))
        object.__setattr__(c, 'traps', dict(self.traps))
        return c

    __copy__ = copy

    def create_decimal(self, num='0'):
        """Convert num to a Decimal and round it to this context."""
        return _convert(num, self, False)

    def create_decimal_from_float(self, f):
        """Convert a float (or int) exactly, then round it to this context."""
        if isinstance(f, float):
            d = _from_float(f)
        elif isinstance(f, int):
            d = _from_int(f)
        else:
            raise TypeError('argument must be int or float')
        return _run(self, _rounded_value, d)

    # --- operations

    def abs(self, a):
        """Absolute value, rounded to the context."""
        return _run(self, _op_abs, _operand_or_raise(a))

    def add(self, a, b):
        """Sum, rounded to the context."""
        return _run(self, _op_add, _operand_or_raise(a), _operand_or_raise(b))

    def subtract(self, a, b):
        """Difference, rounded to the context."""
        return _run(self, _op_sub, _operand_or_raise(a), _operand_or_raise(b))

    def multiply(self, a, b):
        """Product, rounded to the context."""
        return _run(self, _op_mul, _operand_or_raise(a), _operand_or_raise(b))

    def divide(self, a, b):
        """Quotient, rounded to the context."""
        return _run(self, _op_div, _operand_or_raise(a), _operand_or_raise(b))

    def divide_int(self, a, b):
        """Integer part of the quotient (truncated)."""
        return _run(self, _op_divint, _operand_or_raise(a), _operand_or_raise(b))

    def divmod(self, a, b):
        """Pair (divide_int, remainder)."""
        return _run(self, _op_divmod, _operand_or_raise(a), _operand_or_raise(b))

    def remainder(self, a, b):
        """Remainder of truncating integer division; sign follows the dividend."""
        return _run(self, _op_rem, _operand_or_raise(a), _operand_or_raise(b))

    def remainder_near(self, a, b):
        """Remainder after division with the quotient rounded to the nearest integer."""
        return _run(self, _op_rem_near, _operand_or_raise(a), _operand_or_raise(b))

    def canonical(self, a):
        """The operand itself (every Decimal is canonical)."""
        if not isinstance(a, Decimal):
            raise TypeError('canonical requires a Decimal as an argument.')
        return a

    def is_canonical(self, a):
        """Always True."""
        if not isinstance(a, Decimal):
            raise TypeError('is_canonical requires a Decimal as an argument.')
        return True

    def compare(self, a, b):
        """Numeric comparison returning Decimal -1, 0 or 1 (NaN if a NaN is involved)."""
        return _run(self, _op_compare, _operand_or_raise(a), _operand_or_raise(b))

    def compare_signal(self, a, b):
        """Like compare(), but any NaN operand signals InvalidOperation."""
        return _run(self, _op_compare_signal, _operand_or_raise(a), _operand_or_raise(b))

    def compare_total(self, a, b):
        """Total ordering comparison on the representation (Decimal -1, 0 or 1)."""
        return _operand_or_raise(a).compare_total(_operand_or_raise(b))

    def compare_total_mag(self, a, b):
        """compare_total() applied to the absolute values."""
        return _operand_or_raise(a).compare_total_mag(_operand_or_raise(b))

    def copy_abs(self, a):
        """Copy with the sign cleared; no rounding and no signals."""
        return _operand_or_raise(a).copy_abs()

    def copy_decimal(self, a):
        """Copy of the operand; no rounding and no signals."""
        return Decimal(_operand_or_raise(a))

    def copy_negate(self, a):
        """Copy with the sign flipped; no rounding and no signals."""
        return _operand_or_raise(a).copy_negate()

    def copy_sign(self, a, b):
        """Copy of the first operand carrying the sign of the second."""
        return _operand_or_raise(a).copy_sign(_operand_or_raise(b))

    def exp(self, a):
        """Exponential function, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(self, _op_exp, _operand_or_raise(a))

    def fma(self, a, b, c):
        """Multiply exactly, then add the third operand with a single rounding."""
        return _run(self, _op_fma, _operand_or_raise(a), _operand_or_raise(b), _operand_or_raise(c))

    def is_finite(self, a):
        """True unless the operand is infinite or a NaN."""
        return _operand_or_raise(a).is_finite()

    def is_infinite(self, a):
        """True for positive or negative infinity."""
        return _operand_or_raise(a).is_infinite()

    def is_nan(self, a):
        """True for quiet and signaling NaNs."""
        return _operand_or_raise(a).is_nan()

    def is_normal(self, a):
        """True for finite nonzero operands whose adjusted exponent is at least Emin."""
        return _operand_or_raise(a).is_normal(self)

    def is_qnan(self, a):
        """True for a quiet NaN."""
        return _operand_or_raise(a).is_qnan()

    def is_signed(self, a):
        """True if the sign is negative (including -0 and -NaN)."""
        return _operand_or_raise(a).is_signed()

    def is_snan(self, a):
        """True for a signaling NaN."""
        return _operand_or_raise(a).is_snan()

    def is_subnormal(self, a):
        """True for finite nonzero operands whose adjusted exponent is below Emin."""
        return _operand_or_raise(a).is_subnormal(self)

    def is_zero(self, a):
        """True for a zero of either sign."""
        return _operand_or_raise(a).is_zero()

    def ln(self, a):
        """Natural logarithm, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(self, _op_ln, _operand_or_raise(a))

    def log10(self, a):
        """Base-10 logarithm, correctly rounded (always ROUND_HALF_EVEN)."""
        return _run(self, _op_log10, _operand_or_raise(a))

    def logb(self, a):
        """Adjusted exponent of the operand as a Decimal (-Infinity for zero)."""
        return _run(self, _op_logb, _operand_or_raise(a))

    def logical_and(self, a, b):
        """Digit-wise AND of two logical operands."""
        return _run(self, _op_and, _operand_or_raise(a), _operand_or_raise(b))

    def logical_invert(self, a):
        """Digit-wise NOT of a logical operand (digits 0/1), padded to the precision."""
        return _run(self, _op_invert, _operand_or_raise(a))

    def logical_or(self, a, b):
        """Digit-wise OR of two logical operands."""
        return _run(self, _op_or, _operand_or_raise(a), _operand_or_raise(b))

    def logical_xor(self, a, b):
        """Digit-wise XOR of two logical operands."""
        return _run(self, _op_xor, _operand_or_raise(a), _operand_or_raise(b))

    def max(self, a, b):
        """The numerically larger operand, rounded to the context."""
        return _run(self, _op_max, _operand_or_raise(a), _operand_or_raise(b))

    def max_mag(self, a, b):
        """The operand with the larger absolute value, rounded to the context."""
        return _run(self, _op_max_mag, _operand_or_raise(a), _operand_or_raise(b))

    def min(self, a, b):
        """The numerically smaller operand, rounded to the context."""
        return _run(self, _op_min, _operand_or_raise(a), _operand_or_raise(b))

    def min_mag(self, a, b):
        """The operand with the smaller absolute value, rounded to the context."""
        return _run(self, _op_min_mag, _operand_or_raise(a), _operand_or_raise(b))

    def minus(self, a):
        """Negation, rounded to the context."""
        return _run(self, _op_minus, _operand_or_raise(a))

    def next_minus(self, a):
        """Largest number representable in the context that is below the operand."""
        return _run(self, _op_next_minus, _operand_or_raise(a))

    def next_plus(self, a):
        """Smallest number representable in the context that is above the operand."""
        return _run(self, _op_next_plus, _operand_or_raise(a))

    def next_toward(self, a, b):
        """Next representable number from the first operand in the direction of the second."""
        return _run(self, _op_next_toward, _operand_or_raise(a), _operand_or_raise(b))

    def normalize(self, a):
        """Round to the context and strip trailing zeros from the coefficient."""
        return _run(self, _op_reduce, _operand_or_raise(a))

    def number_class(self, a):
        """Name of the operand's class, e.g. '+Normal', '-Zero', 'sNaN'."""
        return _number_class(_operand_or_raise(a), self._env())

    def plus(self, a):
        """The operand rounded to the context."""
        return _run(self, _op_plus, _operand_or_raise(a))

    def power(self, a, b, modulo=None):
        """First operand raised to the second; with modulo, modular exponentiation of integers."""
        a = _operand_or_raise(a)
        b = _operand_or_raise(b)
        if modulo is None:
            return _run(self, _op_power, a, b)
        return _run(self, _op_power_mod, a, b, _operand_or_raise(modulo))

    def quantize(self, a, b):
        """Round to the exponent of the second operand."""
        return _run(self, _op_quantize, _operand_or_raise(a), _operand_or_raise(b), self.rounding)

    def same_quantum(self, a, b):
        """True if both operands have the same exponent (or are both NaN / both infinite)."""
        return _operand_or_raise(a).same_quantum(_operand_or_raise(b))

    def rotate(self, a, b):
        """Rotate the coefficient digits (within the precision) by the given count."""
        return _run(self, _op_rotate, _operand_or_raise(a), _operand_or_raise(b))

    def scaleb(self, a, b):
        """Add the integral second operand to the exponent."""
        return _run(self, _op_scaleb, _operand_or_raise(a), _operand_or_raise(b))

    def shift(self, a, b):
        """Shift the coefficient digits (within the precision) by the given count."""
        return _run(self, _op_shift_, _operand_or_raise(a), _operand_or_raise(b))

    def sqrt(self, a):
        """Square root, correctly rounded to the context precision."""
        return _run(self, _op_sqrt, _operand_or_raise(a))

    def to_eng_string(self, a):
        """String form with an exponent that is a multiple of three."""
        return _to_str(_operand_or_raise(a), True, self.capitals)

    def to_sci_string(self, a):
        """String form in scientific notation (what str() gives)."""
        return _to_str(_operand_or_raise(a), False, self.capitals)

    def to_integral_exact(self, a):
        """Round to an integer, signaling Inexact and Rounded when appropriate."""
        return _run(self, _op_to_integral, _operand_or_raise(a), self.rounding, True)

    def to_integral_value(self, a):
        """Round to an integer without signaling Inexact or Rounded."""
        return _run(self, _op_to_integral, _operand_or_raise(a), self.rounding, False)

    to_integral = to_integral_value


DefaultContext = Context(prec=28, rounding=ROUND_HALF_EVEN, Emin=-999999, Emax=999999,
                         capitals=1, clamp=0, flags=[],
                         traps=[DivisionByZero, Overflow, InvalidOperation])

BasicContext = Context(prec=9, rounding=ROUND_HALF_UP,
                       traps=[DivisionByZero, Overflow, InvalidOperation, Clamped, Underflow],
                       flags=[])

ExtendedContext = Context(prec=9, rounding=ROUND_HALF_EVEN, traps=[], flags=[])


# ----------------------------------------------------------------------------
# Current context

_current = _contextvars.ContextVar('decimal_context')

def getcontext():
    """Return the context of the current thread (or task)."""
    try:
        return _current.get()
    except LookupError:
        c = Context()
        _current.set(c)
        return c

def setcontext(context):
    """Make the given context the current one (templates are copied)."""
    if not isinstance(context, Context):
        raise TypeError('argument must be a context')
    if context is DefaultContext or context is BasicContext or context is ExtendedContext:
        context = context.copy()
        context.clear_flags()
    _current.set(context)


class _ContextManager(object):
    """Installs a context for the duration of a with-block."""

    def __init__(self, new_context):
        self.new_context = new_context

    def __enter__(self):
        self.saved_context = getcontext()
        setcontext(self.new_context)
        return self.new_context

    def __exit__(self, *exc):
        setcontext(self.saved_context)


def localcontext(ctx=None, **kwargs):
    """Context manager that installs a copy of ctx (or of the current context),
    with any keyword overrides applied, and restores the previous context on exit."""
    if ctx is None:
        ctx = getcontext()
    elif not isinstance(ctx, Context):
        raise TypeError('optional argument must be a context')
    local = ctx.copy()
    for key, value in kwargs.items():
        if key not in _CTX_ATTRS:
            raise TypeError("'%s' is an invalid keyword argument for this function" % key)
        if key in ('flags', 'traps') and isinstance(value, list):
            value = _signal_dict_from(value)
        setattr(local, key, value)
    return _ContextManager(local)


def IEEEContext(bits, /):
    """Context for the IEEE 754 interchange format with the given bit width
    (a multiple of 32, at most IEEE_CONTEXT_MAX_BITS)."""
    if not isinstance(bits, int):
        raise TypeError('an integer is required')
    if bits <= 0 or bits > IEEE_CONTEXT_MAX_BITS or bits % 32:
        raise ValueError('argument must be a multiple of 32, with a maximum of %d'
                         % IEEE_CONTEXT_MAX_BITS)
    emax = 3 << (bits // 16 + 3)
    return Context(prec=9 * (bits // 32) - 2, rounding=ROUND_HALF_EVEN,
                   Emin=1 - emax, Emax=emax, capitals=1, clamp=1, flags=[], traps=[])


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings.warn("'__version__' is deprecated and slated for removal in Python 3.20",
                      DeprecationWarning, stacklevel=2)
        return SPEC_VERSION
    raise AttributeError("module 'decimal' has no attribute %r" % name)
