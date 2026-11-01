"""Abstract Base Classes (ABCs) for numbers, according to PEP 3141."""

# The numeric tower: Number > Complex > Real > Rational > Integral.
# Abstract methods define the required protocol; concrete methods give
# default implementations in terms of the abstract ones.

from abc import ABCMeta, abstractmethod

__all__ = ["Number", "Complex", "Real", "Rational", "Integral"]


class Number(metaclass=ABCMeta):
    """Root of the numeric hierarchy.

    Use isinstance(x, Number) to test whether x is any kind of number.
    """
    __slots__ = ()

    # Concrete numeric types define their own hash.
    __hash__ = None


class Complex(Number):
    """Numbers supporting the operations of the builtin complex type:
    complex(), .real, .imag, +, -, *, /, **, abs(), .conjugate(), == and !=.
    """

    __slots__ = ()

    @abstractmethod
    def __complex__(self):
        """Return an equivalent builtin complex."""

    def __bool__(self):
        """True unless the number equals zero."""
        return self != 0

    @property
    @abstractmethod
    def real(self):
        """The real part (a Real)."""
        raise NotImplementedError

    @property
    @abstractmethod
    def imag(self):
        """The imaginary part (a Real)."""
        raise NotImplementedError

    @abstractmethod
    def __add__(self, other):
        """self + other"""
        raise NotImplementedError

    @abstractmethod
    def __radd__(self, other):
        """other + self"""
        raise NotImplementedError

    @abstractmethod
    def __neg__(self):
        """-self"""
        raise NotImplementedError

    @abstractmethod
    def __pos__(self):
        """+self"""
        raise NotImplementedError

    def __sub__(self, other):
        """self - other"""
        return self + -other

    def __rsub__(self, other):
        """other - self"""
        return -self + other

    @abstractmethod
    def __mul__(self, other):
        """self * other"""
        raise NotImplementedError

    @abstractmethod
    def __rmul__(self, other):
        """other * self"""
        raise NotImplementedError

    @abstractmethod
    def __truediv__(self, other):
        """self / other"""
        raise NotImplementedError

    @abstractmethod
    def __rtruediv__(self, other):
        """other / self"""
        raise NotImplementedError

    @abstractmethod
    def __pow__(self, exponent):
        """self ** exponent"""
        raise NotImplementedError

    @abstractmethod
    def __rpow__(self, base):
        """base ** self"""
        raise NotImplementedError

    @abstractmethod
    def __abs__(self):
        """The (Real) distance from zero."""
        raise NotImplementedError

    @abstractmethod
    def conjugate(self):
        """The complex conjugate."""
        raise NotImplementedError

    @abstractmethod
    def __eq__(self, other):
        """self == other"""
        raise NotImplementedError

Complex.register(complex)


class Real(Complex):
    """Complex numbers that are also totally ordered: adds float(),
    trunc(), floor(), ceil(), round(), divmod(), //, %, <, <=, > and >=.
    """

    __slots__ = ()

    @abstractmethod
    def __float__(self):
        """Convert to a builtin float."""
        raise NotImplementedError

    @abstractmethod
    def __trunc__(self):
        """Round towards zero to an Integral."""
        raise NotImplementedError

    @abstractmethod
    def __floor__(self):
        """The greatest Integral <= self."""
        raise NotImplementedError

    @abstractmethod
    def __ceil__(self):
        """The least Integral >= self."""
        raise NotImplementedError

    @abstractmethod
    def __round__(self, ndigits=None):
        """Round half to even; an Integral without ndigits, else a Real."""
        raise NotImplementedError

    def __divmod__(self, other):
        """(self // other, self % other)"""
        return (self // other, self % other)

    def __rdivmod__(self, other):
        """(other // self, other % self)"""
        return (other // self, other % self)

    @abstractmethod
    def __floordiv__(self, other):
        """self // other"""
        raise NotImplementedError

    @abstractmethod
    def __rfloordiv__(self, other):
        """other // self"""
        raise NotImplementedError

    @abstractmethod
    def __mod__(self, other):
        """self % other"""
        raise NotImplementedError

    @abstractmethod
    def __rmod__(self, other):
        """other % self"""
        raise NotImplementedError

    @abstractmethod
    def __lt__(self, other):
        """self < other"""
        raise NotImplementedError

    @abstractmethod
    def __le__(self, other):
        """self <= other"""
        raise NotImplementedError

    def __complex__(self):
        """complex(float(self))"""
        return complex(float(self))

    @property
    def real(self):
        """A real number is its own real part."""
        return +self

    @property
    def imag(self):
        """Real numbers have a zero imaginary part."""
        return 0

    def conjugate(self):
        """Conjugation does nothing to a real number."""
        return +self

Real.register(float)


class Rational(Real):
    """Real numbers with a numerator and a (positive) denominator in
    lowest terms."""

    __slots__ = ()

    @property
    @abstractmethod
    def numerator(self):
        """Numerator in lowest terms."""
        raise NotImplementedError

    @property
    @abstractmethod
    def denominator(self):
        """Positive denominator in lowest terms."""
        raise NotImplementedError

    def __float__(self):
        """Divide numerator by denominator with true integer division,
        which avoids overflow for huge ratios."""
        return int(self.numerator) / int(self.denominator)


class Integral(Rational):
    """Rational numbers with denominator 1: adds int(), __index__,
    three-argument pow() and the bitwise operators."""

    __slots__ = ()

    @abstractmethod
    def __int__(self):
        """int(self)"""
        raise NotImplementedError

    def __index__(self):
        """Lossless conversion to int (used for slicing etc.)."""
        return int(self)

    @abstractmethod
    def __pow__(self, exponent, modulus=None):
        """self ** exponent, or pow(self, exponent, modulus)."""
        raise NotImplementedError

    @abstractmethod
    def __lshift__(self, other):
        """self << other"""
        raise NotImplementedError

    @abstractmethod
    def __rlshift__(self, other):
        """other << self"""
        raise NotImplementedError

    @abstractmethod
    def __rshift__(self, other):
        """self >> other"""
        raise NotImplementedError

    @abstractmethod
    def __rrshift__(self, other):
        """other >> self"""
        raise NotImplementedError

    @abstractmethod
    def __and__(self, other):
        """self & other"""
        raise NotImplementedError

    @abstractmethod
    def __rand__(self, other):
        """other & self"""
        raise NotImplementedError

    @abstractmethod
    def __xor__(self, other):
        """self ^ other"""
        raise NotImplementedError

    @abstractmethod
    def __rxor__(self, other):
        """other ^ self"""
        raise NotImplementedError

    @abstractmethod
    def __or__(self, other):
        """self | other"""
        raise NotImplementedError

    @abstractmethod
    def __ror__(self, other):
        """other | self"""
        raise NotImplementedError

    @abstractmethod
    def __invert__(self):
        """~self"""
        raise NotImplementedError

    def __float__(self):
        """float(int(self))"""
        return float(int(self))

    @property
    def numerator(self):
        """An integer is its own numerator."""
        return +self

    @property
    def denominator(self):
        """Integers have denominator 1."""
        return 1

Integral.register(int)
