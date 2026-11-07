"""Exact rational numbers: the Fraction type."""

import functools
import math
import numbers
import operator
import sys

__all__ = ['Fraction']


# ---------------------------------------------------------------------------
# Hashing: a Fraction hashes like any equal int, float or Decimal.  The value
# is n * d**-1 modulo the hash prime; a denominator divisible by the prime
# has no inverse and stands for "infinity".

_MODULUS = sys.hash_info.modulus
_HASH_INF = sys.hash_info.inf


@functools.lru_cache(maxsize=1 << 14)
def _hash_algorithm(numerator, denominator):
    try:
        inverse = pow(denominator, -1, _MODULUS)
    except ValueError:
        h = _HASH_INF
    else:
        h = hash(abs(numerator)) * inverse % _MODULUS
    if numerator < 0:
        h = -h
    return -2 if h == -1 else h


# ---------------------------------------------------------------------------
# Parsing of string arguments, written as a small scanner rather than a
# regular expression.

def _scan_digits(s, i):
    """Return the end of a run of decimal digits with single underscores
    between them, starting at s[i]; return i if there is none."""
    n = len(s)
    j = i
    while True:
        start = j
        while j < n and s[j].isdecimal():
            j += 1
        if j == start:
            # an underscore must be followed by a digit
            return i if start == i else start - 1
        if j < n and s[j] == '_':
            j += 1
        else:
            return j


def _parse_literal(text):
    """Return (numerator, denominator) for a fraction literal, or None."""
    s = text.strip()
    n = len(s)
    i = 0
    negative = False
    if i < n and s[i] in '+-':
        negative = s[i] == '-'
        i += 1
    j = _scan_digits(s, i)
    whole = s[i:j]
    i = j
    # whitespace is allowed around the slash
    k = i
    while k < n and s[k].isspace():
        k += 1
    if k < n and s[k] == '/':
        if not whole:
            return None
        k += 1
        while k < n and s[k].isspace():
            k += 1
        j = _scan_digits(s, k)
        if j == k or j != n:
            return None
        num = int(whole)
        den = int(s[k:j])
    else:
        frac = ''
        if i < n and s[i] == '.':
            j = _scan_digits(s, i + 1)
            frac = s[i + 1:j]
            i = j
            if not whole and not frac:
                return None
        elif not whole:
            return None
        exp = 0
        if i < n and s[i] in 'eE':
            k = i + 1
            if k < n and s[k] in '+-':
                k += 1
            j = _scan_digits(s, k)
            if j == k:
                return None
            exp = int(s[i + 1:j])
            i = j
        if i != n:
            return None
        num = int(whole) if whole else 0
        den = 1
        if frac:
            scale = 10 ** len(frac.replace('_', ''))
            num = num * scale + int(frac)
            den = scale
        if exp > 0:
            num *= 10 ** exp
        elif exp < 0:
            den *= 10 ** -exp
    if negative:
        num = -num
    return num, den


# ---------------------------------------------------------------------------
# Formatting support.

def _round_div(n, d):
    """n / d rounded to the nearest integer, ties to even (d > 0)."""
    q, r = divmod(n, d)
    twice = 2 * r
    if twice > d or (twice == d and q & 1):
        q += 1
    return q


def _parse_format_spec(spec):
    """Split a format specification into a dict, or return None if the
    specification is not one Fraction understands."""
    n = len(spec)
    i = 0
    fill = align = None
    if n >= 2 and spec[1] in '<>=^':
        fill, align = spec[0], spec[1]
        i = 2
    elif n >= 1 and spec[0] in '<>=^':
        align = spec[0]
        i = 1
    sign = '-'
    if i < n and spec[i] in '+- ':
        sign = spec[i]
        i += 1
    no_neg_zero = i < n and spec[i] == 'z'
    if no_neg_zero:
        i += 1
    alternate = i < n and spec[i] == '#'
    if alternate:
        i += 1
    zero_pad = False
    # A '0' directly followed by more digits is the zero-padding flag; on
    # its own it is just a minimum width of zero.
    if i + 1 < n and spec[i] == '0' and '0' <= spec[i + 1] <= '9':
        zero_pad = True
        i += 1
    j = i
    while j < n and '0' <= spec[j] <= '9':
        j += 1
    width = int(spec[i:j]) if j > i else 0
    i = j
    group = ''
    if i < n and spec[i] in ',_':
        group = spec[i]
        i += 1
    precision = None
    frac_group = ''
    if i < n and spec[i] == '.':
        j = i + 1
        while j < n and '0' <= spec[j] <= '9':
            j += 1
        if j > i + 1:
            precision = int(spec[i + 1:j])
        i = j
        if i < n and spec[i] in ',_':
            frac_group = spec[i]
            i += 1
        elif precision is None:
            # '.' needs digits, a separator for the fraction, or both
            return None
    kind = ''
    if i < n and spec[i] in 'eEfFgG%':
        kind = spec[i]
        i += 1
    if i != n:
        return None
    if zero_pad and align is not None:
        return None
    if not kind and (no_neg_zero or zero_pad or precision is not None):
        return None
    return dict(fill=fill, align=align, sign=sign, no_neg_zero=no_neg_zero,
                alternate=alternate, zero_pad=zero_pad, width=width,
                group=group, precision=precision, frac_group=frac_group,
                kind=kind)


def _group_left(digits, sep):
    """Insert sep every three digits counting from the right."""
    if not sep or len(digits) <= 3:
        return digits
    head = len(digits) % 3 or 3
    parts = [digits[:head]]
    parts.extend(digits[k:k + 3] for k in range(head, len(digits), 3))
    return sep.join(parts)


def _group_right(digits, sep):
    """Insert sep every three digits counting from the left."""
    if not sep or len(digits) <= 3:
        return digits
    return sep.join(digits[k:k + 3] for k in range(0, len(digits), 3))


def _assemble(sign, whole, rest, opts):
    """Pad and align sign + grouped whole part + rest per the options."""
    width = opts['width']
    group = opts['group']
    if opts['zero_pad'] or (opts['fill'] == '0' and opts['align'] == '='):
        # zeros are inserted into the digits, so they get separators too
        room = width - len(sign) - len(rest)
        if not group:
            return sign + whole.rjust(room, '0') + rest
        # fewest digits k whose grouped form (k + (k-1)//3 chars) fills room
        k = max(len(whole), room - room // 4 - 1, 1)
        while k + (k - 1) // 3 < room:
            k += 1
        while k > len(whole) and (k - 1) + (k - 2) // 3 >= room:
            k -= 1
        return sign + _group_left(whole.rjust(k, '0'), group) + rest
    body = _group_left(whole, group) + rest
    pad = width - len(sign) - len(body)
    if pad <= 0:
        return sign + body
    fill = opts['fill'] or ' '
    align = opts['align'] or '>'
    if align == '<':
        return sign + body + fill * pad
    if align == '=':
        return sign + fill * pad + body
    if align == '^':
        left = pad // 2
        return fill * left + sign + body + fill * (pad - left)
    return fill * pad + sign + body


def _sign_text(negative, opts):
    if negative:
        return '-'
    s = opts['sign']
    return '' if s == '-' else s


# ---------------------------------------------------------------------------

class Fraction(numbers.Rational):
    """An exact rational number numerator/denominator.

    Fraction(n=0, d=1) takes two Rational values (ints, Fractions, ...);
    a single argument may also be a float, a Decimal, any object with an
    as_integer_ratio() method, or a string such as '3/7', '-1.25' or
    '6.02e23'.
    """

    __slots__ = ('_numerator', '_denominator')

    def __new__(cls, numerator=0, denominator=None):
        self = super().__new__(cls)
        if denominator is None:
            if type(numerator) is int:
                self._numerator = numerator
                self._denominator = 1
                return self
            if isinstance(numerator, numbers.Rational):
                # assumed to be in lowest terms already
                self._numerator = numerator.numerator
                self._denominator = numerator.denominator
                return self
            if isinstance(numerator, str):
                parsed = _parse_literal(numerator)
                if parsed is None:
                    raise ValueError('Invalid literal for Fraction: %r'
                                     % numerator)
                n, d = parsed
            elif type(numerator) is float:
                self._numerator, self._denominator = \
                    numerator.as_integer_ratio()
                return self
            else:
                ratio = _ratio_method(numerator)
                if ratio is None:
                    raise TypeError("argument should be a string or a "
                                    "Rational instance or have the "
                                    "as_integer_ratio() method")
                n, d = ratio()
        elif type(numerator) is int is type(denominator):
            n, d = numerator, denominator
        elif (isinstance(numerator, numbers.Rational)
              and isinstance(denominator, numbers.Rational)):
            n = numerator.numerator * denominator.denominator
            d = denominator.numerator * numerator.denominator
        else:
            raise TypeError("both arguments should be Rational instances")
        if d == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % n)
        g = math.gcd(n, d)
        if d < 0:
            g = -g
        self._numerator = n // g
        self._denominator = d // g
        return self

    @classmethod
    def _from_coprime_ints(cls, n, d):
        # n/d is already in lowest terms with d > 0
        self = object.__new__(cls)
        self._numerator = n
        self._denominator = d
        return self

    # -- alternative constructors ----------------------------------------

    @classmethod
    def from_float(cls, f):
        """Convert a float (or an int) to an equal Fraction."""
        if isinstance(f, numbers.Integral):
            return cls(f)
        if not isinstance(f, float):
            raise TypeError('%s.from_float() expects a float, not %s'
                            % (cls.__name__, type(f).__name__))
        return cls._from_coprime_ints(*f.as_integer_ratio())

    @classmethod
    def from_decimal(cls, dec):
        """Convert a Decimal (or an int) to an equal Fraction."""
        from decimal import Decimal
        if isinstance(dec, numbers.Integral):
            dec = Decimal(int(dec))
        elif not isinstance(dec, Decimal):
            raise TypeError('%s.from_decimal() expects a Decimal, not %s'
                            % (cls.__name__, type(dec).__name__))
        return cls._from_coprime_ints(*dec.as_integer_ratio())

    @classmethod
    def from_number(cls, number):
        """Convert a number (not a string) to an equal Fraction."""
        if isinstance(number, numbers.Rational):
            return cls(number)
        ratio = None if isinstance(number, str) else _ratio_method(number)
        if ratio is None:
            raise TypeError("argument should be a Rational instance or "
                            "have the as_integer_ratio() method")
        return cls(*ratio())

    # -- basic accessors -----------------------------------------------------

    @property
    def numerator(a):
        return a._numerator

    @property
    def denominator(a):
        return a._denominator

    def is_integer(self):
        """True if the value is a whole number."""
        return self._denominator == 1

    def as_integer_ratio(self):
        """Return (numerator, denominator) in lowest terms."""
        return (self._numerator, self._denominator)

    def limit_denominator(self, max_denominator=1000000):
        """Return the Fraction closest to self whose denominator is at
        most max_denominator."""
        if max_denominator < 1:
            raise ValueError("max_denominator should be at least 1")
        if self._denominator <= max_denominator:
            return Fraction(self)
        # Walk the continued fraction expansion, keeping the last two
        # convergents p_prev/q_prev and p_cur/q_cur, until the next one
        # would have too large a denominator.
        p_prev = q_cur = 0
        q_prev = p_cur = 1
        x, y = self._numerator, self._denominator
        while True:
            a = x // y
            q_next = q_prev + a * q_cur
            if q_next > max_denominator:
                break
            p_prev, p_cur = p_cur, p_prev + a * p_cur
            q_prev, q_cur = q_cur, q_next
            x, y = y, x - a * y
        # the best approximation is either the last convergent or the
        # largest admissible semiconvergent
        k = (max_denominator - q_prev) // q_cur
        p_semi, q_semi = p_prev + k * p_cur, q_prev + k * q_cur
        # compare |p/q - n/d| for both candidates by cross-multiplying
        n, d = self._numerator, self._denominator
        if abs(p_cur * d - n * q_cur) * q_semi <= abs(p_semi * d - n * q_semi) * q_cur:
            return _new(p_cur, q_cur)
        return _new(p_semi, q_semi)

    # -- text --------------------------------------------------------------

    def __repr__(self):
        return '%s(%s, %s)' % (self.__class__.__name__,
                               self._numerator, self._denominator)

    def __str__(self):
        if self._denominator == 1:
            return str(self._numerator)
        return '%s/%s' % (self._numerator, self._denominator)

    def __format__(self, format_spec, /):
        if not format_spec:
            return str(self)
        opts = _parse_format_spec(format_spec)
        if opts is None:
            raise ValueError("Invalid format specifier %r for object of "
                             "type %r" % (format_spec, type(self).__name__))
        if opts['kind']:
            return self._format_decimal(opts)
        return self._format_ratio(opts)

    def _format_ratio(self, opts):
        n, d = self._numerator, self._denominator
        group = opts['group']
        rest = ''
        if d != 1 or opts['alternate']:
            rest = '/' + _group_left(str(d), group)
        return _assemble(_sign_text(n < 0, opts), str(abs(n)), rest, opts)

    def _format_decimal(self, opts):
        kind = opts['kind']
        prec = opts['precision']
        if prec is None:
            prec = 6
        alternate = opts['alternate']
        n, d = self._numerator, self._denominator
        negative = n < 0
        n = abs(n)
        suffix = ''
        exponent = None
        if kind in 'fF%':
            if kind == '%':
                n *= 100
                suffix = '%'
            digits = str(_round_div(n * 10 ** prec, d))
            if prec:
                digits = digits.rjust(prec + 1, '0')
                whole, frac = digits[:-prec], digits[-prec:]
            else:
                whole, frac = digits, ''
            show_point = bool(prec) or alternate
            if negative and opts['no_neg_zero'] and not digits.strip('0'):
                negative = False
        else:
            sig = prec + 1 if kind in 'eE' else max(prec, 1)
            digits, e = _significant(n, d, sig)
            if kind in 'eE' or not -4 <= e < sig:
                whole, frac = digits[0], digits[1:]
                exponent = e
            elif e >= 0:
                whole, frac = digits[:e + 1], digits[e + 1:]
            else:
                whole, frac = '0', '0' * (-e - 1) + digits
            if kind in 'gG' and not alternate:
                frac = frac.rstrip('0')
            show_point = bool(frac) or alternate
            if exponent is not None:
                mark = 'E' if kind in 'EG' else 'e'
                suffix = '%s%+03d' % (mark, exponent)
        rest = ''
        if show_point:
            rest = '.' + _group_right(frac, opts['frac_group'])
        return _assemble(_sign_text(negative, opts), whole, rest + suffix,
                         opts)

    # -- arithmetic --------------------------------------------------------
    # Each operator is built by _binary_op from an exact implementation for
    # (Fraction, Fraction) and the operator applied to float/complex values.

    def _add(a, b):
        return _sum(a._numerator, a._denominator, b._numerator, b._denominator)

    def _sub(a, b):
        return _sum(a._numerator, a._denominator, -b._numerator, b._denominator)

    def _mul(a, b):
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g = math.gcd(na, db)
        if g > 1:
            na //= g
            db //= g
        g = math.gcd(nb, da)
        if g > 1:
            nb //= g
            da //= g
        return _new(na * nb, db * da)

    def _div(a, b):
        nb, db = b._numerator, b._denominator
        if nb == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % a._numerator)
        if nb < 0:
            nb, db = -nb, -db
        return Fraction._mul(a, _new(db, nb))

    def _floordiv(a, b):
        return (a._numerator * b._denominator) // (a._denominator * b._numerator)

    def _divmod(a, b):
        da, db = a._denominator, b._denominator
        q, r = divmod(a._numerator * db, da * b._numerator)
        return q, Fraction(r, da * db)

    def _mod(a, b):
        da, db = a._denominator, b._denominator
        return Fraction((a._numerator * db) % (b._numerator * da), da * db)

    def _binary_op(exact, inexact, allow_complex=True):
        name = inexact.__name__

        def forward(a, b):
            if isinstance(b, Fraction):
                return exact(a, b)
            if isinstance(b, int):
                return exact(a, _new(b, 1))
            if isinstance(b, float):
                return inexact(float(a), b)
            if allow_complex and isinstance(b, complex):
                # float(a) first, so complex subclasses get their turn
                return inexact(float(a), b)
            return NotImplemented

        def reverse(b, a):
            if isinstance(a, numbers.Rational):
                return exact(Fraction(a), b)
            if isinstance(a, numbers.Real):
                return inexact(float(a), float(b))
            if allow_complex and isinstance(a, numbers.Complex):
                return inexact(complex(a), complex(b))
            return NotImplemented

        forward.__name__ = '__%s__' % name
        forward.__qualname__ = 'Fraction.' + forward.__name__
        reverse.__name__ = '__r%s__' % name
        reverse.__qualname__ = 'Fraction.' + reverse.__name__
        return forward, reverse

    __add__, __radd__ = _binary_op(_add, operator.add)
    __sub__, __rsub__ = _binary_op(_sub, operator.sub)
    __mul__, __rmul__ = _binary_op(_mul, operator.mul)
    __truediv__, __rtruediv__ = _binary_op(_div, operator.truediv)
    __floordiv__, __rfloordiv__ = _binary_op(_floordiv, operator.floordiv,
                                             False)
    __divmod__, __rdivmod__ = _binary_op(_divmod, divmod, False)
    __mod__, __rmod__ = _binary_op(_mod, operator.mod, False)
    del _binary_op

    def __pow__(a, b, modulo=None):
        """a ** b; exact when b is a whole number."""
        if modulo is not None:
            return NotImplemented
        if isinstance(b, numbers.Rational):
            if b.denominator == 1:
                return a._int_power(b.numerator)
            return float(a) ** float(b)
        if isinstance(b, (float, complex)):
            return float(a) ** b
        return NotImplemented

    def _int_power(a, power):
        n, d = a._numerator, a._denominator
        if power >= 0:
            return _new(n ** power, d ** power)
        if n == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % d ** -power)
        if n < 0:
            n, d = -n, -d
        return _new(d ** -power, n ** -power)

    def __rpow__(b, a, modulo=None):
        """a ** b"""
        if modulo is not None:
            return NotImplemented
        whole = b._denominator == 1
        if isinstance(a, numbers.Rational):
            if whole and b._numerator >= 0:
                return a ** b._numerator
            return Fraction(a) ** b
        if whole:
            return a ** b._numerator
        return a ** float(b)

    # -- unary operations and conversions ----------------------------------

    def __pos__(a):
        """+a, as an exact Fraction"""
        return _new(a._numerator, a._denominator)

    def __neg__(a):
        """-a"""
        return _new(-a._numerator, a._denominator)

    def __abs__(a):
        """abs(a)"""
        return _new(abs(a._numerator), a._denominator)

    def __int__(a):
        """int(a), rounding towards zero"""
        n, d = a._numerator, a._denominator
        if n < 0:
            return int(-(-n // d))
        return int(n // d)

    def __trunc__(a):
        """math.trunc(a)"""
        n, d = a._numerator, a._denominator
        if n < 0:
            return -(-n // d)
        return n // d

    def __floor__(a):
        """math.floor(a)"""
        return a._numerator // a._denominator

    def __ceil__(a):
        """math.ceil(a)"""
        return -(-a._numerator // a._denominator)

    def __round__(self, ndigits=None):
        """round(self[, ndigits]); halfway cases go to the even neighbour."""
        if ndigits is None:
            return _round_div(self._numerator, self._denominator)
        scale = 10 ** abs(ndigits)
        if ndigits > 0:
            return Fraction(round(self * scale), scale)
        return Fraction(round(self / scale) * scale)

    def __float__(a):
        """float(a), correctly rounded"""
        n, d = a._numerator, a._denominator
        if type(n) is int and type(d) is int:
            return n / d
        # other Integral types need not divide into a float themselves
        return int(n) / int(d)

    def __bool__(a):
        """a != 0"""
        # use the numerator's own truth value (it may not be an int)
        return bool(a._numerator)

    def __hash__(self):
        return _hash_algorithm(self._numerator, self._denominator)

    # -- comparisons -------------------------------------------------------

    def __eq__(a, b):
        """a == b"""
        if type(b) is int:
            return a._numerator == b and a._denominator == 1
        if isinstance(b, numbers.Rational):
            return (a._numerator == b.numerator and
                    a._denominator == b.denominator)
        if isinstance(b, numbers.Complex) and b.imag == 0:
            b = b.real
        if isinstance(b, float):
            if math.isfinite(b):
                return a == a.from_float(b)
            # infinities and NaN are equal to no finite value
            return False
        return NotImplemented

    def _compare(a, b, op):
        if isinstance(b, numbers.Rational):
            return op(a._numerator * b.denominator,
                      a._denominator * b.numerator)
        if isinstance(b, float):
            if math.isfinite(b):
                return op(a, a.from_float(b))
            # any finite value behaves like 0.0 against inf/nan
            return op(0.0, b)
        return NotImplemented

    def __lt__(a, b):
        """a < b"""
        return a._compare(b, operator.lt)

    def __gt__(a, b):
        """a > b"""
        return a._compare(b, operator.gt)

    def __le__(a, b):
        """a <= b"""
        return a._compare(b, operator.le)

    def __ge__(a, b):
        """a >= b"""
        return a._compare(b, operator.ge)

    # -- copying and pickling ----------------------------------------------

    def __reduce__(self):
        return (self.__class__, (self._numerator, self._denominator))

    def __copy__(self):
        if type(self) is Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)

    def __deepcopy__(self, memo):
        if type(self) is Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)


def _ratio_method(obj):
    """The bound as_integer_ratio of an instance (not of a class), if any."""
    if isinstance(obj, type):
        return None
    return getattr(obj, 'as_integer_ratio', None)


def _significant(n, d, count):
    """Round n/d (n >= 0, d > 0) to count significant digits.

    Return (digits, exponent) such that n/d ~= 0.digits * 10**(exponent+1),
    i.e. digits[0] is the 10**exponent place.  Zero gives exponent 0.
    """
    if n == 0:
        return '0' * count, 0
    # estimate the decimal exponent of n/d, then correct it
    # (from the bit lengths, without converting huge ints to text)
    e = math.floor((n.bit_length() - d.bit_length()) * 0.3010299956639812)
    while _at_least_pow10(n, d, e + 1):
        e += 1
    while not _at_least_pow10(n, d, e):
        e -= 1
    shift = count - 1 - e
    if shift >= 0:
        q = _round_div(n * 10 ** shift, d)
    else:
        q = _round_div(n, d * 10 ** -shift)
    if q == 10 ** count:
        # rounding carried into a new digit
        q //= 10
        e += 1
    return str(q), e


def _at_least_pow10(n, d, k):
    """n/d >= 10**k"""
    if k >= 0:
        return n >= d * 10 ** k
    return n * 10 ** -k >= d


def _new(n, d):
    """A Fraction from n/d already in lowest terms with d > 0."""
    f = _object_new(Fraction)
    f._numerator = n
    f._denominator = d
    return f

_object_new = object.__new__


def _sum(na, da, nb, db):
    """na/da + nb/db (both in lowest terms) as a Fraction."""
    g = math.gcd(da, db)
    if g == 1:
        return _new(na * db + da * nb, da * db)
    # the denominators share factors: work with da/g and db/g instead
    s = da // g
    t = na * (db // g) + nb * s
    g2 = math.gcd(t, g)
    if g2 == 1:
        return _new(t, s * db)
    return _new(t // g2, s * (db // g2))
