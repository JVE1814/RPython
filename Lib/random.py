"""Pseudo-random number generators for various distributions.

Provides random bytes and integers, selection and permutation of sequence
elements, continuous distributions (uniform, triangular, normal, lognormal,
exponential, gamma, beta, Pareto, Weibull, von Mises) and the binomial
distribution.  The core generator is the Mersenne Twister (period
2**19937-1) implemented in the _random module.
"""

from math import log as _log, exp as _exp, pi as _pi, e as _e, ceil as _ceil
from math import sqrt as _sqrt, acos as _acos, cos as _cos, sin as _sin
from math import tau as TWOPI, floor as _floor, isfinite as _isfinite
from math import lgamma as _lgamma, fabs as _fabs, log2 as _log2
from operator import index as _index
from itertools import accumulate as _accumulate, repeat as _repeat
from bisect import bisect as _bisect
import os as _os
import _random

try:
    from os import urandom as _urandom
except ImportError:
    def _urandom(n):
        raise NotImplementedError("os.urandom() is not available")

try:
    from _collections_abc import Sequence as _Sequence
except ImportError:
    _Sequence = (list, tuple, range, str, bytes, bytearray)

__all__ = [
    "Random",
    "SystemRandom",
    "betavariate",
    "binomialvariate",
    "choice",
    "choices",
    "expovariate",
    "gammavariate",
    "gauss",
    "getrandbits",
    "getstate",
    "lognormvariate",
    "normalvariate",
    "paretovariate",
    "randbytes",
    "randint",
    "random",
    "randrange",
    "sample",
    "seed",
    "setstate",
    "shuffle",
    "triangular",
    "uniform",
    "vonmisesvariate",
    "weibullvariate",
]

NV_MAGICCONST = 4 * _exp(-0.5) / _sqrt(2.0)
LOG4 = _log(4.0)
SG_MAGICCONST = 1.0 + _log(4.5)
BPF = 53
RECIP_BPF = 2 ** -BPF
_ONE = 1
_sha512 = None


class Random(_random.Random):
    """Random number generator base class; subclass and override random(), seed(), getstate(), setstate() (and optionally getrandbits()) to use a different core generator."""

    VERSION = 3

    def __init__(self, x=None):
        """Initialize an instance.

        Optional argument x controls seeding, as for Random.seed().
        """

        self.seed(x)
        self.gauss_next = None

    def seed(self, a=None, version=2):
        """Initialize the internal state from a seed: None (OS entropy or time), int, float, str, bytes or bytearray."""

        if version == 1 and isinstance(a, (str, bytes)):
            if isinstance(a, bytes):
                a = a.decode('latin-1')
            x = ord(a[0]) << 7 if a else 0
            for c in map(ord, a):
                x = ((1000003 * x) ^ c) & 0xFFFFFFFFFFFFFFFF
            x ^= len(a)
            a = -2 if x == -1 else x

        elif version == 2 and isinstance(a, (str, bytes, bytearray)):
            global _sha512
            if _sha512 is None:
                try:
                    from _sha2 import sha512 as _sha512
                except ImportError:
                    from hashlib import sha512 as _sha512
            if isinstance(a, str):
                a = a.encode()
            a = int.from_bytes(a + _sha512(a).digest())

        elif not isinstance(a, (type(None), int, float, str, bytes, bytearray)):
            raise TypeError('The only supported seed types are:\n'
                            'None, int, float, str, bytes, and bytearray.')

        super().seed(a)
        self.gauss_next = None

    def getstate(self):
        """Return internal state; can be passed to setstate() later."""
        return self.VERSION, super().getstate(), self.gauss_next

    def setstate(self, state):
        """Restore internal state from object returned by getstate()."""
        version = state[0]
        if version == 3:
            version, internalstate, self.gauss_next = state
            super().setstate(internalstate)
        elif version == 2:
            version, internalstate, self.gauss_next = state
            try:
                internalstate = tuple(x % (2 ** 32) for x in internalstate)
            except ValueError as e:
                raise TypeError from e
            super().setstate(internalstate)
        else:
            raise ValueError("state with version %s passed to "
                             "Random.setstate() of version %s" %
                             (version, self.VERSION))



    def __getstate__(self):
        return self.getstate()

    def __setstate__(self, state):
        self.setstate(state)

    def __reduce__(self):
        return self.__class__, (), self.getstate()


    def __init_subclass__(cls, /, **kwargs):
        """Control how subclasses generate random integers.

        The algorithm a subclass can use depends on the random() and/or
        getrandbits() implementation available to it and determines
        whether it can generate random integers from arbitrarily large
        ranges.
        """

        for c in cls.__mro__:
            if '_randbelow' in c.__dict__:
                break
            if 'getrandbits' in c.__dict__:
                cls._randbelow = cls._randbelow_with_getrandbits
                break
            if 'random' in c.__dict__:
                cls._randbelow = cls._randbelow_without_getrandbits
                break

    def _randbelow_with_getrandbits(self, n):
        "Return a random int in the range [0,n).  Defined for n > 0."

        getrandbits = self.getrandbits
        k = n.bit_length()
        r = getrandbits(k)
        while r >= n:
            r = getrandbits(k)
        return r

    def _randbelow_without_getrandbits(self, n, maxsize=1<<BPF):
        """Return a random int in the range [0,n).  Defined for n > 0.

        The implementation does not use getrandbits, but only random.
        """

        random = self.random
        if n >= maxsize:
            from warnings import warn
            warn("Underlying random() generator does not supply \n"
                 "enough bits to choose from a population range this large.\n"
                 "To remove the range limitation, add a getrandbits() method.")
            return _floor(random() * n)
        rem = maxsize % n
        limit = (maxsize - rem) / maxsize
        r = random()
        while r >= limit:
            r = random()
        return _floor(r * maxsize) % n

    _randbelow = _randbelow_with_getrandbits



    def randbytes(self, n):
        """Generate n random bytes."""
        return self.getrandbits(n * 8).to_bytes(n, 'little')


    def randrange(self, start, stop=None, step=_ONE):
        """Choose a random item from range(stop) or range(start, stop[, step]).

        Roughly equivalent to ``choice(range(start, stop, step))`` but
        supports arbitrarily large ranges and is optimized for common cases.

        """

        istart = _index(start)
        if stop is None:
            if step != 1:
                raise TypeError("Missing a non-None stop argument")
            if istart > 0:
                return self._randbelow(istart)
            raise ValueError("empty range for randrange()")

        istop = _index(stop)
        width = istop - istart
        istep = _index(step)
        if istep == 1:
            if width > 0:
                return istart + self._randbelow(width)
            raise ValueError(f"empty range in randrange({start}, {stop})")

        if istep > 0:
            n = (width + istep - 1) // istep
        elif istep < 0:
            n = (width + istep + 1) // istep
        else:
            raise ValueError("zero step for randrange()")
        if n <= 0:
            raise ValueError(f"empty range in randrange({start}, {stop}, {step})")
        return istart + istep * self._randbelow(n)

    def randint(self, a, b):
        """Return random integer in range [a, b], including both end points.
        """
        return self.randrange(a, _index(b) + 1)


    def choice(self, seq):
        """Choose a random element from a non-empty sequence."""

        if not len(seq):
            raise IndexError('Cannot choose from an empty sequence')
        return seq[self._randbelow(len(seq))]

    def shuffle(self, x):
        """Shuffle list x in place, and return None."""

        randbelow = self._randbelow
        for i in reversed(range(1, len(x))):
            j = randbelow(i + 1)
            x[i], x[j] = x[j], x[i]

    def sample(self, population, k, *, counts=None):
        """Return k unique elements chosen from the population sequence (selection order preserved); counts may give element multiplicities."""






        if not isinstance(population, _Sequence):
            raise TypeError("Population must be a sequence.  "
                            "For dicts or sets, use sorted(d).")
        n = len(population)
        if counts is not None:
            cum_counts = list(_accumulate(counts))
            if len(cum_counts) != n:
                raise ValueError('The number of counts does not match the population')
            total = cum_counts.pop() if cum_counts else 0
            if not isinstance(total, int):
                raise TypeError('Counts must be integers')
            if total < 0:
                raise ValueError('Counts must be non-negative')
            selections = self.sample(range(total), k=k)
            bisect = _bisect
            return [population[bisect(cum_counts, s)] for s in selections]
        randbelow = self._randbelow
        if not 0 <= k <= n:
            raise ValueError("Sample larger than population or is negative")
        result = [None] * k
        setsize = 21
        if k > 5:
            setsize += 4 ** _ceil(_log(k * 3, 4))
        if n <= setsize:
            pool = list(population)
            for i in range(k):
                j = randbelow(n - i)
                result[i] = pool[j]
                pool[j] = pool[n - i - 1]
        else:
            selected = set()
            selected_add = selected.add
            for i in range(k):
                j = randbelow(n)
                while j in selected:
                    j = randbelow(n)
                selected_add(j)
                result[i] = population[j]
        return result

    def choices(self, population, weights=None, *, cum_weights=None, k=1):
        """Return a list of k elements chosen with replacement, optionally weighted by weights or cum_weights."""
        random = self.random
        n = len(population)
        if cum_weights is None:
            if weights is None:
                floor = _floor
                n += 0.0
                return [population[floor(random() * n)] for i in _repeat(None, k)]
            try:
                cum_weights = list(_accumulate(weights))
            except TypeError:
                if not isinstance(weights, int):
                    raise
                k = weights
                raise TypeError(
                    f'The number of choices must be a keyword argument: {k=}'
                ) from None
        elif weights is not None:
            raise TypeError('Cannot specify both weights and cumulative weights')
        if len(cum_weights) != n:
            raise ValueError('The number of weights does not match the population')
        total = cum_weights[-1] + 0.0
        if total <= 0.0:
            raise ValueError('Total of weights must be greater than zero')
        if not _isfinite(total):
            raise ValueError('Total of weights must be finite')
        bisect = _bisect
        hi = n - 1
        return [population[bisect(cum_weights, random() * total, 0, hi)]
                for i in _repeat(None, k)]


    def uniform(self, a, b):
        """Get a random number in the range [a, b) or [a, b] depending on rounding.

        The mean (expected value) and variance of the random variable are:

            E[X] = (a + b) / 2
            Var[X] = (b - a) ** 2 / 12

        """
        return a + (b - a) * self.random()

    def triangular(self, low=0.0, high=1.0, mode=None):
        """Triangular distribution between low and high with the given mode (default: midpoint)."""
        u = self.random()
        try:
            c = 0.5 if mode is None else (mode - low) / (high - low)
        except ZeroDivisionError:
            return low
        if u > c:
            u = 1.0 - u
            c = 1.0 - c
            low, high = high, low
        return low + (high - low) * _sqrt(u * c)

    def normalvariate(self, mu=0.0, sigma=1.0):
        """Normal distribution.

        mu is the mean, and sigma is the standard deviation.

        """

        random = self.random
        while True:
            u1 = random()
            u2 = 1.0 - random()
            z = NV_MAGICCONST * (u1 - 0.5) / u2
            zz = z * z / 4.0
            if zz <= -_log(u2):
                break
        return mu + z * sigma

    def gauss(self, mu=0.0, sigma=1.0):
        """Gaussian distribution with mean mu and standard deviation sigma (faster than normalvariate, not thread-safe)."""


        random = self.random
        z = self.gauss_next
        self.gauss_next = None
        if z is None:
            x2pi = random() * TWOPI
            g2rad = _sqrt(-2.0 * _log(1.0 - random()))
            z = _cos(x2pi) * g2rad
            self.gauss_next = _sin(x2pi) * g2rad

        return mu + z * sigma

    def lognormvariate(self, mu, sigma):
        """Log normal distribution.

        If you take the natural logarithm of this distribution, you'll get a
        normal distribution with mean mu and standard deviation sigma.
        mu can have any value, and sigma must be greater than zero.

        """
        return _exp(self.normalvariate(mu, sigma))

    def expovariate(self, lambd=1.0):
        """Exponential distribution with rate lambd (1/mean); negative lambd gives values in (-inf, 0]."""

        return -_log(1.0 - self.random()) / lambd

    def vonmisesvariate(self, mu, kappa):
        """Von Mises distribution on the circle: mean angle mu (radians), concentration kappa >= 0 (0 gives a uniform angle)."""


        random = self.random
        if kappa <= 1e-6:
            return TWOPI * random()

        s = 0.5 / kappa
        r = s + _sqrt(1.0 + s * s)

        while True:
            u1 = random()
            z = _cos(_pi * u1)

            d = z / (r + z)
            u2 = random()
            if u2 < 1.0 - d * d or u2 <= (1.0 - d) * _exp(d):
                break

        q = 1.0 / r
        f = (q + z) / (1.0 + q * z)
        u3 = random()
        if u3 > 0.5:
            theta = (mu + _acos(f)) % TWOPI
        else:
            theta = (mu - _acos(f)) % TWOPI

        return theta

    def gammavariate(self, alpha, beta):
        """Gamma distribution (not the gamma function) with shape alpha > 0 and scale beta > 0; mean alpha*beta."""

        if alpha <= 0.0 or beta <= 0.0:
            raise ValueError('gammavariate: alpha and beta must be > 0.0')

        random = self.random
        if alpha > 1.0:


            ainv = _sqrt(2.0 * alpha - 1.0)
            bbb = alpha - LOG4
            ccc = alpha + ainv

            while True:
                u1 = random()
                if not 1e-7 < u1 < 0.9999999:
                    continue
                u2 = 1.0 - random()
                v = _log(u1 / (1.0 - u1)) / ainv
                x = alpha * _exp(v)
                z = u1 * u1 * u2
                r = bbb + ccc * v - x
                if r + SG_MAGICCONST - 4.5 * z >= 0.0 or r >= _log(z):
                    return x * beta

        elif alpha == 1.0:
            return -_log(1.0 - random()) * beta

        else:
            while True:
                u = random()
                b = (_e + alpha) / _e
                p = b * u
                if p <= 1.0:
                    x = p ** (1.0 / alpha)
                else:
                    x = -_log((b - p) / alpha)
                u1 = random()
                if p > 1.0:
                    if u1 <= x ** (alpha - 1.0):
                        break
                elif u1 <= _exp(-x):
                    break
            return x * beta

    def betavariate(self, alpha, beta):
        """Beta distribution with parameters alpha > 0 and beta > 0; results lie in [0, 1]."""

        y = self.gammavariate(alpha, 1.0)
        if y:
            return y / (y + self.gammavariate(beta, 1.0))
        return 0.0

    def paretovariate(self, alpha):
        """Pareto distribution.  alpha is the shape parameter."""

        u = 1.0 - self.random()
        return u ** (-1.0 / alpha)

    def weibullvariate(self, alpha, beta):
        """Weibull distribution.

        alpha is the scale parameter and beta is the shape parameter.

        """

        u = 1.0 - self.random()
        return alpha * (-_log(u)) ** (1.0 / beta)



    def binomialvariate(self, n=1, p=0.5):
        """Number of successes in n independent trials with success probability p (0 <= result <= n)."""
        if n < 0:
            raise ValueError("n must be non-negative")
        if p <= 0.0 or p >= 1.0:
            if p == 0.0:
                return 0
            if p == 1.0:
                return n
            raise ValueError("p must be in the range 0.0 <= p <= 1.0")

        random = self.random

        if n == 1:
            return _index(random() < p)

        if p > 0.5:
            return n - self.binomialvariate(n, 1.0 - p)

        if n * p < 10.0:
            x = y = 0
            c = _log2(1.0 - p)
            if not c:
                return x
            while True:
                u = random()
                if not u:
                    continue
                y += _floor(_log2(u) / c) + 1
                if y > n:
                    return x
                x += 1

        assert n*p >= 10.0 and p <= 0.5
        setup_complete = False

        spq = _sqrt(n * p * (1.0 - p))
        b = 1.15 + 2.53 * spq
        a = -0.0873 + 0.0248 * b + 0.01 * p
        c = n * p + 0.5
        vr = 0.92 - 4.2 / b

        while True:

            u = random()
            u -= 0.5
            us = 0.5 - _fabs(u)
            if not us:
                continue
            k = _floor((2.0 * a / us + b) * u + c)
            if k < 0 or k > n:
                continue

            v = random()
            if us >= 0.07 and v <= vr:
                return k

            if not setup_complete:
                alpha = (2.83 + 5.1 / b) * spq
                lpq = _log(p / (1.0 - p))
                m = _floor((n + 1) * p)
                h = _lgamma(m + 1) + _lgamma(n - m + 1)
                setup_complete = True
            v *= alpha / (a / (us * us) + b)
            if _log(v) <= h - _lgamma(k + 1) - _lgamma(n - k + 1) + (k - m) * lpq:
                return k




class SystemRandom(Random):
    """Random generator drawing from the operating system's entropy source (os.urandom); it has no state and cannot be seeded or reproduced."""

    def random(self):
        """Get the next random number in the range 0.0 <= X < 1.0."""
        return (int.from_bytes(_urandom(7)) >> 3) * RECIP_BPF

    def getrandbits(self, k):
        """getrandbits(k) -> x.  Generates an int with k random bits."""
        if k < 0:
            raise ValueError('number of bits must be non-negative')
        numbytes = (k + 7) // 8
        x = int.from_bytes(_urandom(numbytes))
        return x >> (numbytes * 8 - k)

    def randbytes(self, n):
        """Generate n random bytes."""
        return _urandom(n)

    def seed(self, *args, **kwds):
        "Stub method.  Not used for a system random number generator."
        return None

    def _notimplemented(self, *args, **kwds):
        "Method should not be called for a system random number generator."
        raise NotImplementedError('System entropy source does not have state.')
    getstate = setstate = _notimplemented



_inst = Random()
seed = _inst.seed
random = _inst.random
uniform = _inst.uniform
triangular = _inst.triangular
randint = _inst.randint
choice = _inst.choice
randrange = _inst.randrange
sample = _inst.sample
shuffle = _inst.shuffle
choices = _inst.choices
normalvariate = _inst.normalvariate
lognormvariate = _inst.lognormvariate
expovariate = _inst.expovariate
vonmisesvariate = _inst.vonmisesvariate
gammavariate = _inst.gammavariate
gauss = _inst.gauss
betavariate = _inst.betavariate
binomialvariate = _inst.binomialvariate
paretovariate = _inst.paretovariate
weibullvariate = _inst.weibullvariate
getstate = _inst.getstate
setstate = _inst.setstate
getrandbits = _inst.getrandbits
randbytes = _inst.randbytes



def _test_generator(n, func, args):
    from statistics import stdev, fmean as mean
    from time import perf_counter

    t0 = perf_counter()
    data = [func(*args) for i in _repeat(None, n)]
    t1 = perf_counter()

    xbar = mean(data)
    sigma = stdev(data, xbar)
    low = min(data)
    high = max(data)

    print(f'{t1 - t0:.3f} sec, {n} times {func.__name__}{args!r}')
    print('avg %g, stddev %g, min %g, max %g\n' % (xbar, sigma, low, high))


def _test(N=10_000):
    _test_generator(N, random, ())
    _test_generator(N, normalvariate, (0.0, 1.0))
    _test_generator(N, lognormvariate, (0.0, 1.0))
    _test_generator(N, vonmisesvariate, (0.0, 1.0))
    _test_generator(N, binomialvariate, (15, 0.60))
    _test_generator(N, binomialvariate, (100, 0.75))
    _test_generator(N, gammavariate, (0.01, 1.0))
    _test_generator(N, gammavariate, (0.1, 1.0))
    _test_generator(N, gammavariate, (0.1, 2.0))
    _test_generator(N, gammavariate, (0.5, 1.0))
    _test_generator(N, gammavariate, (0.9, 1.0))
    _test_generator(N, gammavariate, (1.0, 1.0))
    _test_generator(N, gammavariate, (2.0, 1.0))
    _test_generator(N, gammavariate, (20.0, 1.0))
    _test_generator(N, gammavariate, (200.0, 1.0))
    _test_generator(N, gauss, (0.0, 1.0))
    _test_generator(N, betavariate, (3.0, 3.0))
    _test_generator(N, triangular, (0.0, 1.0, 1.0 / 3.0))



if hasattr(_os, "fork") and hasattr(_os, "register_at_fork"):
    _os.register_at_fork(after_in_child=_inst.seed)




def _parse_args(arg_list: list[str] | None):
    import argparse
    parser = argparse.ArgumentParser(
        formatter_class=argparse.RawTextHelpFormatter, color=True)
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "-c", "--choice", nargs="+",
        help="print a random choice")
    group.add_argument(
        "-i", "--integer", type=int, metavar="N",
        help="print a random integer between 1 and N inclusive")
    group.add_argument(
        "-f", "--float", type=float, metavar="N",
        help="print a random floating-point number between 0 and N inclusive")
    group.add_argument(
        "--test", type=int, const=10_000, nargs="?",
        help=argparse.SUPPRESS)
    parser.add_argument("input", nargs="*",
                        help="""\
if no options given, output depends on the input
    string or multiple: same as --choice
    integer: same as --integer
    float: same as --float""")
    args = parser.parse_args(arg_list)
    return args, parser.format_help()


def main(arg_list: list[str] | None = None) -> int | str:
    args, help_text = _parse_args(arg_list)

    if args.choice:
        return choice(args.choice)

    if args.integer is not None:
        return randint(1, args.integer)

    if args.float is not None:
        return uniform(0, args.float)

    if args.test:
        _test(args.test)
        return ""

    if len(args.input) == 1:
        val = args.input[0]
        try:
            val = int(val)
            return randint(1, val)
        except ValueError:
            try:
                val = float(val)
                return uniform(0, val)
            except ValueError:
                return choice(val.split())

    if len(args.input) >= 2:
        return choice(args.input)

    return help_text


if __name__ == '__main__':
    print(main())
