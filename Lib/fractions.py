"""Fraction, infinite-precision, rational numbers."""

import functools
import math
import numbers
import operator
import re
import sys

__all__ = ['Fraction']


# Constants of the numeric hash (see sys.hash_info).
_PyHASH_MODULUS = sys.hash_info.modulus
_PyHASH_INF = sys.hash_info.inf


@functools.lru_cache(maxsize=1 << 14)
def _hash_algorithm(numerator, denominator):
    # hash(n/d) == hash(n * d**-1 mod P) for the Mersenne prime P, so that
    # equal int, float, Decimal and Fraction values hash alike.  If d is a
    # multiple of P the value has no inverse and hashes like infinity.
    try:
        dinv = pow(denominator, -1, _PyHASH_MODULUS)
    except ValueError:
        h = _PyHASH_INF
    else:
        h = hash(hash(abs(numerator)) * dinv)
    result = h if numerator >= 0 else -h
    return -2 if result == -1 else result


_RATIONAL_FORMAT = re.compile(r"""
    \A\s*                                   # leading whitespace
    (?P<sign>[-+]?)                         # optional sign
    (?=\d|\.\d)                             # a digit or .digit must follow
    (?P<num>\d*|\d+(_\d+)*)                 # integer part / numerator
    (?:
        (?:\s*/\s*(?P<denom>\d+(_\d+)*))?   # a denominator
    |
        (?:\.(?P<decimal>\d*|\d+(_\d+)*))?  # or a fractional part
        (?:E(?P<exp>[-+]?\d+(_\d+)*))?      # with an optional exponent
    )
    \s*\z                                   # trailing whitespace
""", re.VERBOSE | re.IGNORECASE)


def _round_to_exponent(n, d, exponent, no_neg_zero=False):
    """Round n/d to the nearest multiple of 10**exponent (ties to even).

    Returns (negative, significand) with the value equal to
    (-1)**negative * significand * 10**exponent.  With no_neg_zero, a zero
    significand is never reported as negative.  d must be positive.
    """
    if exponent >= 0:
        d *= 10 ** exponent
    else:
        n *= 10 ** -exponent
    q, r = divmod(n + (d >> 1), d)
    if r == 0 and d & 1 == 0:
        # exact tie: round to even
        q &= -2
    negative = q < 0 if no_neg_zero else n < 0
    return negative, abs(q)


def _round_to_figures(n, d, figures):
    """Round n/d to the given number of significant decimal figures.

    Returns (negative, significand, exponent) where significand has exactly
    `figures` digits (or is zero) and the value is
    (-1)**negative * significand * 10**exponent.
    """
    if n == 0:
        return False, 0, 1 - figures
    # m with 10**(m-1) <= |n|/d <= 10**m
    sn, sd = str(abs(n)), str(d)
    m = len(sn) - len(sd) + (sd <= sn)
    exponent = m - figures
    negative, significand = _round_to_exponent(n, d, exponent)
    if len(str(significand)) == figures + 1:
        significand //= 10
        exponent += 1
    return negative, significand, exponent


# Format specification without a presentation type (or with none of
# e, f, g, %): fill/align, sign, alternate form, width, thousands separator.
_GENERAL_FORMAT_SPECIFICATION_MATCHER = re.compile(r"""
    (?:
        (?P<fill>.)?
        (?P<align>[<>=^])
    )?
    (?P<sign>[-+ ]?)
    (?P<alt>\#)?
    (?P<minimumwidth>0|[1-9][0-9]*)?
    (?P<thousands_sep>[,_])?
""", re.DOTALL | re.VERBOSE).fullmatch


# Format specification for the floating-point style presentation types.
_FLOAT_FORMAT_SPECIFICATION_MATCHER = re.compile(r"""
    (?:
        (?P<fill>.)?
        (?P<align>[<>=^])
    )?
    (?P<sign>[-+ ]?)
    (?P<no_neg_zero>z)?
    (?P<alt>\#)?
    (?P<zeropad>0(?=[0-9]))?
    (?P<minimumwidth>[0-9]+)?
    (?P<thousands_sep>[,_])?
    (?:\.
        (?=[\d,_])
        (?P<precision>\d+)?
        (?P<frac_separators>[,_])?
    )?
    (?P<presentation_type>[eEfFgG%])
""", re.DOTALL | re.VERBOSE).fullmatch


def _pad(fill, align, minimumwidth, sign, body):
    padding = fill * (minimumwidth - len(sign) - len(body))
    if align == ">":
        return padding + sign + body
    if align == "<":
        return sign + body + padding
    if align == "^":
        half = len(padding) // 2
        return padding[:half] + sign + body + padding[half:]
    return sign + padding + body


class Fraction(numbers.Rational):
    """This class implements rational numbers.

    In the two-argument form of the constructor, Fraction(8, 6) will
    produce a rational number equivalent to 4/3. Both arguments must
    be Rational. The numerator defaults to 0 and the denominator
    defaults to 1 so that Fraction(3) == 3 and Fraction() == 0.

    Fractions can also be constructed from:

      - numeric strings similar to those accepted by the
        float constructor (for example, '-2.3' or '1e10')

      - strings of the form '123/456'

      - float and Decimal instances

      - other Rational instances (including integers)

    """

    __slots__ = ('_numerator', '_denominator')

    def __new__(cls, numerator=0, denominator=None):
        """Constructs a Rational.

        Takes a string like '3/2' or '1.5', another Rational instance, a
        numerator/denominator pair, or a float.
        """
        self = super(Fraction, cls).__new__(cls)

        if denominator is None:
            if type(numerator) is int:
                self._numerator = numerator
                self._denominator = 1
                return self

            elif isinstance(numerator, numbers.Rational):
                self._numerator = numerator.numerator
                self._denominator = numerator.denominator
                return self

            elif (isinstance(numerator, float) or
                  (not isinstance(numerator, type) and
                   hasattr(numerator, 'as_integer_ratio'))):
                # exact conversion
                self._numerator, self._denominator = numerator.as_integer_ratio()
                return self

            elif isinstance(numerator, str):
                m = _RATIONAL_FORMAT.match(numerator)
                if m is None:
                    raise ValueError('Invalid literal for Fraction: %r' %
                                     numerator)
                numerator = int(m.group('num') or '0')
                denom = m.group('denom')
                if denom:
                    denominator = int(denom)
                else:
                    denominator = 1
                    decimal = m.group('decimal')
                    if decimal:
                        decimal = decimal.replace('_', '')
                        scale = 10 ** len(decimal)
                        numerator = numerator * scale + int(decimal)
                        denominator *= scale
                    exp = m.group('exp')
                    if exp:
                        exp = int(exp)
                        if exp >= 0:
                            numerator *= 10 ** exp
                        else:
                            denominator *= 10 ** -exp
                if m.group('sign') == '-':
                    numerator = -numerator

            else:
                raise TypeError("argument should be a string or a Rational "
                                "instance or have the as_integer_ratio() "
                                "method")

        elif type(numerator) is int is type(denominator):
            pass

        elif (isinstance(numerator, numbers.Rational) and
              isinstance(denominator, numbers.Rational)):
            numerator, denominator = (
                numerator.numerator * denominator.denominator,
                denominator.numerator * numerator.denominator
            )
        else:
            raise TypeError("both arguments should be "
                            "Rational instances")

        if denominator == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % numerator)
        g = math.gcd(numerator, denominator)
        if denominator < 0:
            g = -g
        numerator //= g
        denominator //= g
        self._numerator = numerator
        self._denominator = denominator
        return self

    @classmethod
    def from_number(cls, number):
        """Converts a finite real number to a rational number, exactly.

        Beware that Fraction.from_number(0.3) != Fraction(3, 10).

        """
        if type(number) is int:
            return cls._from_coprime_ints(number, 1)

        elif isinstance(number, numbers.Rational):
            return cls._from_coprime_ints(number.numerator, number.denominator)

        elif (isinstance(number, float) or
              (not isinstance(number, type) and
               hasattr(number, 'as_integer_ratio'))):
            return cls._from_coprime_ints(*number.as_integer_ratio())

        else:
            raise TypeError(f"{cls.__name__}.from_number() only takes "
                            f"numbers, not {number!r} "
                            f"({type(number).__name__})")

    @classmethod
    def from_float(cls, f):
        """Converts a finite float to a rational number, exactly.

        Beware that Fraction.from_float(0.3) != Fraction(3, 10).

        """
        if isinstance(f, numbers.Integral):
            return cls(f)
        elif not isinstance(f, float):
            raise TypeError("%s.from_float() only takes floats, not %r (%s)" %
                            (cls.__name__, f, type(f).__name__))
        return cls._from_coprime_ints(*f.as_integer_ratio())

    @classmethod
    def from_decimal(cls, dec):
        """Converts a finite Decimal instance to a rational number, exactly."""
        from decimal import Decimal
        if isinstance(dec, numbers.Integral):
            dec = Decimal(int(dec))
        elif not isinstance(dec, Decimal):
            raise TypeError(
                "%s.from_decimal() only takes Decimals, not %r (%s)" %
                (cls.__name__, dec, type(dec).__name__))
        return cls._from_coprime_ints(*dec.as_integer_ratio())

    @classmethod
    def _from_coprime_ints(cls, numerator, denominator, /):
        """Build a fraction from already normalized (coprime, d > 0) ints."""
        obj = super(Fraction, cls).__new__(cls)
        obj._numerator = numerator
        obj._denominator = denominator
        return obj

    def is_integer(self):
        """Return True if the Fraction is an integer."""
        return self._denominator == 1

    def as_integer_ratio(self):
        """Return a pair of integers, whose ratio is equal to the original Fraction.

        The ratio is in lowest terms and has a positive denominator.
        """
        return (self._numerator, self._denominator)

    def limit_denominator(self, max_denominator=1000000):
        """Closest Fraction to self with denominator at most max_denominator.

        >>> Fraction('3.141592653589793').limit_denominator(10)
        Fraction(22, 7)
        >>> Fraction('3.141592653589793').limit_denominator(100)
        Fraction(311, 99)
        >>> Fraction(4321, 8765).limit_denominator(10000)
        Fraction(4321, 8765)
        """
        # Walk the continued fraction expansion of self, computing
        # convergents p1/q1 until the next denominator would exceed the
        # bound; the best approximation is then either the last convergent
        # or the best semiconvergent (p0 + k*p1)/(q0 + k*q1).
        if max_denominator < 1:
            raise ValueError("max_denominator should be at least 1")
        if self._denominator <= max_denominator:
            return Fraction(self)

        p0, q0, p1, q1 = 0, 1, 1, 0
        n, d = self._numerator, self._denominator
        while True:
            a = n // d
            q2 = q0 + a * q1
            if q2 > max_denominator:
                break
            p0, q0, p1, q1 = p1, q1, p0 + a * p1, q2
            n, d = d, n - a * d
        k = (max_denominator - q0) // q1

        # |self - p1/q1| = d/(q1*den) and the semiconvergent's error is
        # 1/((q0+k*q1)*q1) - d/(q1*den); pick the smaller one.
        if 2 * d * (q0 + k * q1) <= self._denominator:
            return Fraction._from_coprime_ints(p1, q1)
        else:
            return Fraction._from_coprime_ints(p0 + k * p1, q0 + k * q1)

    @property
    def numerator(a):
        return a._numerator

    @property
    def denominator(a):
        return a._denominator

    def __repr__(self):
        """repr(self)"""
        return '%s(%s, %s)' % (self.__class__.__name__,
                               self._numerator, self._denominator)

    def __str__(self):
        """str(self)"""
        if self._denominator == 1:
            return str(self._numerator)
        else:
            return '%s/%s' % (self._numerator, self._denominator)

    def _format_general(self, match):
        """Format without a presentation type: like str(), with padding."""
        fill = match["fill"] or " "
        align = match["align"] or ">"
        pos_sign = "" if match["sign"] == "-" else match["sign"]
        alternate_form = bool(match["alt"])
        minimumwidth = int(match["minimumwidth"] or "0")
        thousands_sep = match["thousands_sep"] or ''

        n, d = self._numerator, self._denominator
        if d > 1 or alternate_form:
            body = f"{abs(n):{thousands_sep}}/{d:{thousands_sep}}"
        else:
            body = f"{abs(n):{thousands_sep}}"
        sign = '-' if n < 0 else pos_sign
        return _pad(fill, align, minimumwidth, sign, body)

    def _format_float_style(self, match):
        """Format with one of the presentation types e, E, f, F, g, G, %."""
        fill = match["fill"] or " "
        align = match["align"] or ">"
        pos_sign = "" if match["sign"] == "-" else match["sign"]
        no_neg_zero = bool(match["no_neg_zero"])
        alternate_form = bool(match["alt"])
        zeropad = bool(match["zeropad"])
        minimumwidth = int(match["minimumwidth"] or "0")
        thousands_sep = match["thousands_sep"]
        precision = int(match["precision"] or "6")
        frac_sep = match["frac_separators"] or ""
        presentation_type = match["presentation_type"]
        trim_zeros = presentation_type in "gG" and not alternate_form
        trim_point = not alternate_form
        exponent_indicator = "E" if presentation_type in "EFG" else "e"

        if presentation_type in "fF%":
            exponent = -precision
            if presentation_type == "%":
                exponent -= 2
            negative, significand = _round_to_exponent(
                self._numerator, self._denominator, exponent, no_neg_zero)
            scientific = False
            point_pos = precision
        else:
            figures = (max(precision, 1) if presentation_type in "gG"
                       else precision + 1)
            negative, significand, exponent = _round_to_figures(
                self._numerator, self._denominator, figures)
            scientific = (presentation_type in "eE"
                          or exponent > 0
                          or exponent + figures <= -4)
            point_pos = figures - 1 if scientific else -exponent

        if presentation_type == "%":
            suffix = "%"
        elif scientific:
            suffix = f"{exponent_indicator}{exponent + point_pos:+03d}"
        else:
            suffix = ""

        # Digits, zero-padded so that there is at least one digit before
        # the decimal point.
        digits = f"{significand:0{point_pos + 1}d}"

        sign = "-" if negative else pos_sign
        leading = digits[: len(digits) - point_pos]
        frac_part = digits[len(digits) - point_pos:]
        if trim_zeros:
            frac_part = frac_part.rstrip("0")
        separator = "" if trim_point and not frac_part else "."
        if frac_sep:
            frac_part = frac_sep.join(frac_part[pos:pos + 3]
                                      for pos in range(0, len(frac_part), 3))
        trailing = separator + frac_part + suffix

        if zeropad:
            min_leading = minimumwidth - len(sign) - len(trailing)
            # thousands separators will also be inserted into the zeros
            leading = leading.zfill(
                3 * min_leading // 4 + 1 if thousands_sep else min_leading)

        if thousands_sep:
            first_pos = 1 + (len(leading) - 1) % 3
            leading = leading[:first_pos] + "".join(
                thousands_sep + leading[pos:pos + 3]
                for pos in range(first_pos, len(leading), 3))

        return _pad(fill, align, minimumwidth, sign, leading + trailing)

    def __format__(self, format_spec, /):
        """Format this fraction according to the given format specification."""

        if not format_spec:
            return str(self)

        if match := _GENERAL_FORMAT_SPECIFICATION_MATCHER(format_spec):
            return self._format_general(match)

        if match := _FLOAT_FORMAT_SPECIFICATION_MATCHER(format_spec):
            if match["align"] is None or match["zeropad"] is None:
                return self._format_float_style(match)
            raise ValueError(
                f"Invalid format specifier {format_spec!r} for object of "
                f"type {type(self).__name__!r}; can't use explicit "
                f"alignment when zero-padding")

        raise ValueError(
            f"Invalid format specifier {format_spec!r} for object of "
            f"type {type(self).__name__!r}"
        )

    def _operator_fallbacks(monomorphic_operator, fallback_operator,
                            handle_complex=True):
        """Generate forward and reverse operator methods.

        Fractions and ints are handled exactly by monomorphic_operator;
        floats (and complex numbers, when handle_complex) fall back to
        fallback_operator on float/complex values; anything else returns
        NotImplemented so that the other operand gets a chance.
        """

        def forward(a, b):
            if isinstance(b, Fraction):
                return monomorphic_operator(a, b)
            elif isinstance(b, int):
                return monomorphic_operator(a, Fraction._from_coprime_ints(b, 1))
            elif isinstance(b, float):
                return fallback_operator(float(a), b)
            elif handle_complex and isinstance(b, complex):
                return fallback_operator(float(a), b)
            else:
                return NotImplemented
        forward.__name__ = '__' + fallback_operator.__name__ + '__'
        forward.__doc__ = monomorphic_operator.__doc__

        def reverse(b, a):
            if isinstance(a, numbers.Rational):
                # includes ints
                return monomorphic_operator(Fraction(a), b)
            elif isinstance(a, numbers.Real):
                return fallback_operator(float(a), float(b))
            elif handle_complex and isinstance(a, numbers.Complex):
                return fallback_operator(complex(a), float(b))
            else:
                return NotImplemented
        reverse.__name__ = '__r' + fallback_operator.__name__ + '__'
        reverse.__doc__ = monomorphic_operator.__doc__

        return forward, reverse

    def _add(a, b):
        """a + b"""
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g = math.gcd(da, db)
        if g == 1:
            return Fraction._from_coprime_ints(na * db + da * nb, da * db)
        s = da // g
        t = na * (db // g) + nb * s
        g2 = math.gcd(t, g)
        if g2 == 1:
            return Fraction._from_coprime_ints(t, s * db)
        return Fraction._from_coprime_ints(t // g2, s * (db // g2))

    __add__, __radd__ = _operator_fallbacks(_add, operator.add)

    def _sub(a, b):
        """a - b"""
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g = math.gcd(da, db)
        if g == 1:
            return Fraction._from_coprime_ints(na * db - da * nb, da * db)
        s = da // g
        t = na * (db // g) - nb * s
        g2 = math.gcd(t, g)
        if g2 == 1:
            return Fraction._from_coprime_ints(t, s * db)
        return Fraction._from_coprime_ints(t // g2, s * (db // g2))

    __sub__, __rsub__ = _operator_fallbacks(_sub, operator.sub)

    def _mul(a, b):
        """a * b"""
        na, da = a._numerator, a._denominator
        nb, db = b._numerator, b._denominator
        g1 = math.gcd(na, db)
        if g1 > 1:
            na //= g1
            db //= g1
        g2 = math.gcd(nb, da)
        if g2 > 1:
            nb //= g2
            da //= g2
        return Fraction._from_coprime_ints(na * nb, db * da)

    __mul__, __rmul__ = _operator_fallbacks(_mul, operator.mul)

    def _div(a, b):
        """a / b"""
        nb, db = b._numerator, b._denominator
        if nb == 0:
            raise ZeroDivisionError('Fraction(%s, 0)' % db)
        na, da = a._numerator, a._denominator
        g1 = math.gcd(na, nb)
        if g1 > 1:
            na //= g1
            nb //= g1
        g2 = math.gcd(db, da)
        if g2 > 1:
            da //= g2
            db //= g2
        n, d = na * db, nb * da
        if d < 0:
            n, d = -n, -d
        return Fraction._from_coprime_ints(n, d)

    __truediv__, __rtruediv__ = _operator_fallbacks(_div, operator.truediv)

    def _floordiv(a, b):
        """a // b"""
        return (a.numerator * b.denominator) // (a.denominator * b.numerator)

    __floordiv__, __rfloordiv__ = _operator_fallbacks(_floordiv, operator.floordiv, False)

    def _divmod(a, b):
        """(a // b, a % b)"""
        da, db = a.denominator, b.denominator
        div, n_mod = divmod(a.numerator * db, da * b.numerator)
        return div, Fraction(n_mod, da * db)

    __divmod__, __rdivmod__ = _operator_fallbacks(_divmod, divmod, False)

    def _mod(a, b):
        """a % b"""
        da, db = a.denominator, b.denominator
        return Fraction((a.numerator * db) % (b.numerator * da), da * db)

    __mod__, __rmod__ = _operator_fallbacks(_mod, operator.mod, False)

    def __pow__(a, b, modulo=None):
        """a ** b

        If b is not an integer, the result will be a float or complex
        since roots are generally irrational. If b is an integer, the
        result will be rational.

        """
        if modulo is not None:
            return NotImplemented
        if isinstance(b, numbers.Rational):
            if b.denominator == 1:
                power = b.numerator
                if power >= 0:
                    return Fraction._from_coprime_ints(a._numerator ** power,
                                                       a._denominator ** power)
                elif a._numerator > 0:
                    return Fraction._from_coprime_ints(a._denominator ** -power,
                                                       a._numerator ** -power)
                elif a._numerator == 0:
                    raise ZeroDivisionError('Fraction(%s, 0)' %
                                            a._denominator ** -power)
                else:
                    return Fraction._from_coprime_ints((-a._denominator) ** -power,
                                                       (-a._numerator) ** -power)
            else:
                # fractional powers are generally irrational
                return float(a) ** float(b)
        elif isinstance(b, (float, complex)):
            return float(a) ** b
        else:
            return NotImplemented

    def __rpow__(b, a, modulo=None):
        """a ** b"""
        if modulo is not None:
            return NotImplemented
        if b._denominator == 1 and b._numerator >= 0:
            # keep an int base an int if possible
            return a ** b._numerator

        if isinstance(a, numbers.Rational):
            return Fraction(a.numerator, a.denominator) ** b

        if b._denominator == 1:
            return a ** b._numerator

        return a ** float(b)

    def __pos__(a):
        """+a: Coerces a subclass instance to Fraction"""
        return Fraction._from_coprime_ints(a._numerator, a._denominator)

    def __neg__(a):
        """-a"""
        return Fraction._from_coprime_ints(-a._numerator, a._denominator)

    def __abs__(a):
        """abs(a)"""
        return Fraction._from_coprime_ints(abs(a._numerator), a._denominator)

    def __int__(a):
        """int(a)"""
        if a._numerator < 0:
            return int(-(-a._numerator // a._denominator))
        else:
            return int(a._numerator // a._denominator)

    def __trunc__(a):
        """math.trunc(a)"""
        if a._numerator < 0:
            return -(-a._numerator // a._denominator)
        else:
            return a._numerator // a._denominator

    def __floor__(a):
        """math.floor(a)"""
        return a._numerator // a._denominator

    def __ceil__(a):
        """math.ceil(a)"""
        return -(-a._numerator // a._denominator)

    def __round__(self, ndigits=None):
        """round(self, ndigits)

        Rounds half toward even.
        """
        if ndigits is None:
            d = self._denominator
            floor, remainder = divmod(self._numerator, d)
            if remainder * 2 < d:
                return floor
            elif remainder * 2 > d:
                return floor + 1
            elif floor % 2 == 0:
                return floor
            else:
                return floor + 1
        shift = 10 ** abs(ndigits)
        if ndigits > 0:
            return Fraction(round(self * shift), shift)
        else:
            return Fraction(round(self / shift) * shift)

    def __hash__(self):
        """hash(self)"""
        return _hash_algorithm(self._numerator, self._denominator)

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
            if math.isnan(b) or math.isinf(b):
                # comparing with any finite value gives the right answer
                return 0.0 == b
            else:
                return a == a.from_float(b)
        else:
            return NotImplemented

    def _richcmp(self, other, op):
        """Exact comparison with Rationals and floats."""
        if isinstance(other, numbers.Rational):
            return op(self._numerator * other.denominator,
                      self._denominator * other.numerator)
        if isinstance(other, float):
            if math.isnan(other) or math.isinf(other):
                return op(0.0, other)
            else:
                return op(self, self.from_float(other))
        else:
            return NotImplemented

    def __lt__(a, b):
        """a < b"""
        return a._richcmp(b, operator.lt)

    def __gt__(a, b):
        """a > b"""
        return a._richcmp(b, operator.gt)

    def __le__(a, b):
        """a <= b"""
        return a._richcmp(b, operator.le)

    def __ge__(a, b):
        """a >= b"""
        return a._richcmp(b, operator.ge)

    def __bool__(a):
        """a != 0"""
        # use bool() of the numerator so that __bool__ always yields a bool
        return bool(a._numerator)

    # support for pickling, copy, and deepcopy

    def __reduce__(self):
        return (self.__class__, (self._numerator, self._denominator))

    def __copy__(self):
        if type(self) == Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)

    def __deepcopy__(self, memo):
        if type(self) == Fraction:
            return self
        return self.__class__(self._numerator, self._denominator)
