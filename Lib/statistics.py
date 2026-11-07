"""Basic statistics module.

Functions for calculating averages and measures of spread of numeric data:
mean, fmean, geometric_mean, harmonic_mean, median (and its low, high and
grouped variants), mode, multimode, quantiles, variance, pvariance, stdev,
pstdev; relations between two inputs: covariance, correlation,
linear_regression; kernel density estimation: kde, kde_random; and the
NormalDist class for normal distributions.

Calculations on ints, Fractions and Decimals are done exactly (with
fractions internally) and converted back to the input type at the end.
"""

__all__ = [
    'NormalDist',
    'StatisticsError',
    'correlation',
    'covariance',
    'fmean',
    'geometric_mean',
    'harmonic_mean',
    'kde',
    'kde_random',
    'linear_regression',
    'mean',
    'median',
    'median_grouped',
    'median_high',
    'median_low',
    'mode',
    'multimode',
    'pstdev',
    'pvariance',
    'quantiles',
    'stdev',
    'variance',
]

import math
import numbers
import random
import sys

from fractions import Fraction
from decimal import Decimal
from itertools import count, groupby, repeat
from bisect import bisect_left, bisect_right
from math import hypot, sqrt, fabs, exp, erfc, tau, log, fsum, sumprod
from math import isfinite, isinf, pi, cos, sin, tan, cosh, asin, atan, acos
from functools import reduce
from operator import itemgetter
from collections import Counter, namedtuple, defaultdict

_SQRT2 = sqrt(2.0)
_random = random


class StatisticsError(ValueError):
    pass


# ---------------------------------------------------------------- internals

def _sum(data):
    """Exact sum of the data, returned as (type, total, count).

    The total is a Fraction (or a float/Decimal NaN or infinity when such a
    value occurs); the type is the common type of the data (see _coerce).
    """
    n = 0
    types = set()
    types_add = types.add
    partials = {}
    partials_get = partials.get
    for typ, values in groupby(data, type):
        types_add(typ)
        for num, den in map(_exact_ratio, values):
            n += 1
            partials[den] = partials_get(den, 0) + num
    if None in partials:
        # a NaN or an infinity: the finite partials don't matter
        total = partials[None]
        assert not _isfinite(total)
    else:
        total = sum(Fraction(num, den) for den, num in partials.items())
    T = reduce(_coerce, types, int)
    return (T, total, n)


def _ss(data, c=None):
    """Sum of squared deviations from c (default: the exact mean).

    Returns (type, ssd, c, count), computed exactly with fractions.
    """
    if c is not None:
        T, ssd, n = _sum((d := x - c) * d for x in data)
        return (T, ssd, c, n)
    n = 0
    types = set()
    types_add = types.add
    sx_partials = defaultdict(int)
    sxx_partials = defaultdict(int)
    for typ, values in groupby(data, type):
        types_add(typ)
        for num, den in map(_exact_ratio, values):
            n += 1
            sx_partials[den] += num
            sxx_partials[den] += num * num
    if not n:
        ssd = c = Fraction(0)
    elif None in sx_partials:
        ssd = c = sx_partials[None]
        assert not _isfinite(ssd)
    else:
        sx = sum(Fraction(num, den) for den, num in sx_partials.items())
        sxx = sum(Fraction(num, den * den) for den, num in sxx_partials.items())
        # numerically poor for floats, but exact with fractions
        ssd = (n * sxx - sx * sx) / n
        c = sx / n
    T = reduce(_coerce, types, int)
    return (T, ssd, c, n)


def _isfinite(x):
    try:
        return x.is_finite()        # Decimal
    except AttributeError:
        return math.isfinite(x)


def _coerce(T, S):
    """Common type of a sum of values of types T and S (int < Fraction <
    float/Decimal; subclasses win over base classes).  T must not be bool.
    """
    if T is S:
        return T
    if S is int or S is bool:
        return T
    if T is int:
        return S
    if issubclass(S, T):
        return S
    if issubclass(T, S):
        return T
    if issubclass(T, int):
        return S
    if issubclass(S, int):
        return T
    if issubclass(T, Fraction) and issubclass(S, float):
        return S
    if issubclass(T, float) and issubclass(S, Fraction):
        return T
    msg = "don't know how to coerce %s and %s"
    raise TypeError(msg % (T.__name__, S.__name__))


def _exact_ratio(x):
    """(numerator, denominator) of x exactly; (x, None) for NaN/infinity."""
    try:
        return x.as_integer_ratio()
    except AttributeError:
        pass
    except (OverflowError, ValueError):
        assert not _isfinite(x)
        return (x, None)
    try:
        # numbers.Rational without as_integer_ratio
        return (x.numerator, x.denominator)
    except AttributeError:
        msg = f"can't convert type '{type(x).__name__}' to numerator/denominator"
        raise TypeError(msg)


def _convert(value, T):
    """Convert a (Fraction) value to type T where possible."""
    if type(value) is T:
        return value
    if issubclass(T, int) and value.denominator != 1:
        T = float
    try:
        return T(value)
    except TypeError:
        if issubclass(T, Decimal):
            return T(value.numerator) / T(value.denominator)
        else:
            raise


def _fail_neg(values, errmsg='negative value'):
    """Pass values through, raising StatisticsError on a negative one."""
    for x in values:
        if x < 0:
            raise StatisticsError(errmsg)
        yield x


def _rank(data, /, *, key=None, reverse=False, ties='average', start=1):
    """Rank the data (1-based by default), averaging the ranks of ties."""
    if ties != 'average':
        raise ValueError(f'Unknown tie resolution method: {ties!r}')
    if key is not None:
        data = map(key, data)
    val_pos = sorted(zip(data, count()), reverse=reverse)
    i = start - 1
    result = [0] * len(val_pos)
    for _, g in groupby(val_pos, key=itemgetter(0)):
        group = list(g)
        size = len(group)
        rank = i + (size + 1) / 2
        for value, orig_pos in group:
            result[orig_pos] = rank
        i += size
    return result


def _integer_sqrt_of_frac_rto(n: int, m: int) -> int:
    """sqrt(n/m) rounded to an integer with round-to-odd."""
    a = math.isqrt(n // m)
    return a | (a * a * m != n)


# Enough extra bits that round-to-odd followed by the float conversion
# rounds correctly.
_sqrt_bit_width: int = 2 * sys.float_info.mant_dig + 3


def _float_sqrt_of_frac(n: int, m: int) -> float:
    """Correctly rounded float square root of the fraction n/m."""
    q = (n.bit_length() - m.bit_length() - _sqrt_bit_width) // 2
    if q >= 0:
        numerator = _integer_sqrt_of_frac_rto(n, m << 2 * q) << q
        denominator = 1
    else:
        numerator = _integer_sqrt_of_frac_rto(n << -2 * q, m)
        denominator = 1 << -q
    return numerator / denominator


def _decimal_sqrt_of_frac(n: int, m: int) -> Decimal:
    """Correctly rounded Decimal square root of the fraction n/m."""
    if n <= 0:
        if not n:
            return Decimal('0.0')
        n, m = -n, -m
    root = (Decimal(n) / Decimal(m)).sqrt()
    nr, dr = root.as_integer_ratio()

    plus = root.next_plus()
    np, dp = plus.as_integer_ratio()
    # is n/m above the midpoint of root and plus?
    if 4 * n * (dr * dp) ** 2 > m * (dr * np + dp * nr) ** 2:
        return plus

    minus = root.next_minus()
    nm, dm = minus.as_integer_ratio()
    # is n/m below the midpoint of root and minus?
    if 4 * n * (dr * dm) ** 2 < m * (dr * nm + dm * nr) ** 2:
        return minus

    return root


def _mean_stdev(data):
    """(mean, sample standard deviation) as floats."""
    T, ss, xbar, n = _ss(data)
    if n < 2:
        raise StatisticsError('stdev requires at least two data points')
    mss = ss / (n - 1)
    try:
        return float(xbar), _float_sqrt_of_frac(mss.numerator, mss.denominator)
    except AttributeError:
        # NaN or infinity
        return float(xbar), float(xbar) / float(ss)


def _sqrtprod(x: float, y: float) -> float:
    """sqrt(x * y), avoiding overflow/underflow, with a correction step."""
    h = sqrt(x * y)
    if not isfinite(h):
        if isinf(h) and not isinf(x) and not isinf(y):
            # the product overflowed: rescale
            scale = 2.0 ** -512
            return _sqrtprod(scale * x, scale * y) / scale
        return h
    if not h:
        if x and y:
            # the product underflowed: rescale
            scale = 2.0 ** 537
            return _sqrtprod(scale * x, scale * y) / scale
        return h
    # one Newton-style differential correction
    d = sumprod((x, h), (y, -h))
    return h + d / (2.0 * h)


def _sqrt_of_mss(mss, T):
    if not isinstance(mss, Fraction):
        # the data held a NaN or an infinity: no meaningful deviation
        raise ValueError('standard deviation of data containing '
                         'infinities or NaNs is undefined')
    if issubclass(T, Decimal):
        return _decimal_sqrt_of_frac(mss.numerator, mss.denominator)
    return _float_sqrt_of_frac(mss.numerator, mss.denominator)


# ---------------------------------------------------------------- averages

def mean(data):
    """Arithmetic mean of the data, computed exactly."""
    T, total, n = _sum(data)
    if n < 1:
        raise StatisticsError('mean requires at least one data point')
    return _convert(total / n, T)


def fmean(data, weights=None):
    """Fast floating-point (optionally weighted) arithmetic mean."""
    if weights is None:
        try:
            n = len(data)
        except TypeError:
            n = 0

            def counted(iterable):
                nonlocal n
                for n, x in enumerate(iterable, start=1):
                    yield x
            data = counted(data)
        total = fsum(data)
        if not n:
            raise StatisticsError('fmean requires at least one data point')
        return total / n
    if not isinstance(weights, (list, tuple)):
        weights = list(weights)
    try:
        num = sumprod(data, weights)
    except ValueError:
        raise StatisticsError('data and weights must be the same length')
    den = fsum(weights)
    if not den:
        raise StatisticsError('sum of weights must be non-zero')
    return num / den


def geometric_mean(data):
    """Geometric mean of positive data, computed with floats."""
    n = 0
    found_zero = False

    def positive(iterable):
        nonlocal n, found_zero
        for n, x in enumerate(iterable, start=1):
            if x > 0.0 or math.isnan(x):
                yield x
            elif x == 0.0:
                found_zero = True
            else:
                raise StatisticsError('No negative inputs allowed', x)

    total = fsum(map(log, positive(data)))
    if not n:
        raise StatisticsError('Must have a non-empty dataset')
    if math.isnan(total):
        return math.nan
    if found_zero:
        return math.nan if total == math.inf else 0.0
    return exp(total / n)


def harmonic_mean(data, weights=None):
    """(Weighted) harmonic mean of non-negative data."""
    if iter(data) is data:
        data = list(data)
    errmsg = 'harmonic mean does not support negative values'
    n = len(data)
    if n < 1:
        raise StatisticsError('harmonic_mean requires at least one data point')
    elif n == 1 and weights is None:
        x = data[0]
        if isinstance(x, (numbers.Real, Decimal)):
            if x < 0:
                raise StatisticsError(errmsg)
            return x
        else:
            raise TypeError('unsupported type')
    if weights is None:
        weights = repeat(1, n)
        sum_weights = n
    else:
        if iter(weights) is weights:
            weights = list(weights)
        if len(weights) != n:
            raise StatisticsError('Number of weights does not match data size')
        _, sum_weights, _ = _sum(w for w in _fail_neg(weights, errmsg))
    try:
        data = _fail_neg(data, errmsg)
        T, total, count_ = _sum(w / x if w else 0 for w, x in zip(weights, data))
    except ZeroDivisionError:
        return 0
    if total <= 0:
        raise StatisticsError('Weighted sum must be positive')
    return _convert(sum_weights / total, T)


def median(data):
    """Median (middle value; mean of the two middle values for even sizes)."""
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    if n % 2 == 1:
        return data[n // 2]
    i = n // 2
    return (data[i - 1] + data[i]) / 2


def median_low(data):
    """Low median: the smaller of the two middle values for even sizes."""
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    if n % 2 == 1:
        return data[n // 2]
    return data[n // 2 - 1]


def median_high(data):
    """High median: the larger of the two middle values for even sizes."""
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    return data[n // 2]


def median_grouped(data, interval=1.0):
    """Median of grouped continuous data (50th percentile by interpolation),
    where each value is the midpoint of a class of width interval."""
    data = sorted(data)
    n = len(data)
    if not n:
        raise StatisticsError("no median for empty data")
    x = data[n // 2]
    # all copies of x are in data[i:j]
    i = bisect_left(data, x)
    j = bisect_right(data, x, lo=i)
    try:
        interval = float(interval)
        x = float(x)
    except ValueError:
        raise TypeError(f'Value cannot be converted to a float')
    L = x - interval / 2.0      # lower limit of the median class
    cf = i                      # cumulative frequency below it
    f = j - i                   # frequency of the median class
    return L + interval * (n / 2 - cf) / f


def mode(data):
    """Most common value (the first one encountered on ties)."""
    pairs = Counter(iter(data)).most_common(1)
    try:
        return pairs[0][0]
    except IndexError:
        raise StatisticsError('no mode for empty data') from None


def multimode(data):
    """List of the most frequent values, in order of first occurrence."""
    counts = Counter(iter(data))
    if not counts:
        return []
    maxcount = max(counts.values())
    return [value for value, c in counts.items() if c == maxcount]


# ---------------------------------------------------------------- kernels

_kernel_specs = {}


def _register(*kernels):
    def deco(builder):
        spec = dict(zip(('pdf', 'cdf', 'invcdf', 'support'), builder()))
        for kernel in kernels:
            _kernel_specs[kernel] = spec
        return builder
    return deco


@_register('normal', 'gauss')
def _normal_kernel():
    sqrt2pi = sqrt(2 * pi)
    neg_sqrt2 = -sqrt(2)
    pdf = lambda t: exp(-1/2 * t * t) / sqrt2pi
    cdf = lambda t: 1/2 * erfc(t / neg_sqrt2)
    invcdf = lambda t: _normal_dist_inv_cdf(t, 0.0, 1.0)
    support = None
    return pdf, cdf, invcdf, support


@_register('logistic')
def _logistic_kernel():
    pdf = lambda t: 1/2 / (1.0 + cosh(t))
    cdf = lambda t: 1.0 - 1.0 / (exp(t) + 1.0)
    invcdf = lambda p: log(p / (1.0 - p))
    support = None
    return pdf, cdf, invcdf, support


@_register('sigmoid')
def _sigmoid_kernel():
    c1 = 1 / pi
    c2 = 2 / pi
    c3 = pi / 2
    pdf = lambda t: c1 / cosh(t)
    cdf = lambda t: c2 * atan(exp(t))
    invcdf = lambda p: log(tan(p * c3))
    support = None
    return pdf, cdf, invcdf, support


@_register('rectangular', 'uniform')
def _rectangular_kernel():
    pdf = lambda t: 1/2
    cdf = lambda t: 1/2 * t + 1/2
    invcdf = lambda p: 2.0 * p - 1.0
    support = 1.0
    return pdf, cdf, invcdf, support


@_register('triangular')
def _triangular_kernel():
    pdf = lambda t: 1.0 - abs(t)
    cdf = lambda t: t * t * (1/2 if t < 0.0 else -1/2) + t + 1/2
    invcdf = lambda p: (sqrt(2.0 * p) - 1.0 if p < 1/2
                        else 1.0 - sqrt(2.0 - 2.0 * p))
    support = 1.0
    return pdf, cdf, invcdf, support


@_register('parabolic', 'epanechnikov')
def _parabolic_kernel():
    pdf = lambda t: 3/4 * (1.0 - t * t)
    cdf = lambda t: sumprod((-1/4, 3/4, 1/2), (t**3, t, 1.0))
    invcdf = lambda p: 2.0 * cos((acos(2.0 * p - 1.0) + pi) / 3.0)
    support = 1.0
    return pdf, cdf, invcdf, support


def _newton_raphson(f_inv_estimate, f, f_prime, tolerance=1e-12):
    def f_inv(y):
        "Return x such that f(x) is within tolerance of y."
        x = f_inv_estimate(y)
        while abs(diff := f(x) - y) > tolerance:
            x -= diff / f_prime(x)
        return x
    return f_inv


def _quartic_invcdf_estimate(p):
    # piecewise starting point for Newton-Raphson
    sign, p = (1.0, p) if p <= 1/2 else (-1.0, 1.0 - p)
    if p < 0.0106:
        return ((2.0 * p) ** 0.3838 - 1.0) * sign
    x = (2.0 * p) ** 0.4258865685331 - 1.0
    if p < 0.499:
        x += 0.026818732 * sin(7.101753784 * p + 2.73230839482953)
    return x * sign


@_register('quartic', 'biweight')
def _quartic_kernel():
    pdf = lambda t: 15/16 * (1.0 - t * t) ** 2
    cdf = lambda t: sumprod((3/16, -5/8, 15/16, 1/2), (t**5, t**3, t, 1.0))
    invcdf = _newton_raphson(_quartic_invcdf_estimate, f=cdf, f_prime=pdf)
    support = 1.0
    return pdf, cdf, invcdf, support


def _triweight_invcdf_estimate(p):
    # piecewise starting point for Newton-Raphson
    sign, p = (1.0, p) if p <= 1/2 else (-1.0, 1.0 - p)
    x = (2.0 * p) ** 0.3400218741872791 - 1.0
    if 0.00001 < p < 0.499:
        x -= 0.033 * sin(1.07 * tau * (p - 0.035))
    return x * sign


@_register('triweight')
def _triweight_kernel():
    pdf = lambda t: 35/32 * (1.0 - t * t) ** 3
    cdf = lambda t: sumprod((-5/32, 21/32, -35/32, 35/32, 1/2),
                            (t**7, t**5, t**3, t, 1.0))
    invcdf = _newton_raphson(_triweight_invcdf_estimate, f=cdf, f_prime=pdf)
    support = 1.0
    return pdf, cdf, invcdf, support


@_register('cosine')
def _cosine_kernel():
    c1 = pi / 4
    c2 = pi / 2
    pdf = lambda t: c1 * cos(c2 * t)
    cdf = lambda t: 1/2 * sin(c2 * t) + 1/2
    invcdf = lambda p: 2.0 * asin(2.0 * p - 1.0) / pi
    support = 1.0
    return pdf, cdf, invcdf, support


del _register, _normal_kernel, _logistic_kernel, _sigmoid_kernel
del _rectangular_kernel, _triangular_kernel, _parabolic_kernel
del _quartic_kernel, _triweight_kernel, _cosine_kernel


def _check_kde_args(data, h, kernel):
    n = len(data)
    if not n:
        raise StatisticsError('Empty data sequence')
    if not isinstance(data[0], (int, float)):
        raise TypeError('Data sequence must contain ints or floats')
    if h <= 0.0:
        raise StatisticsError(f'Bandwidth h must be positive, not {h=!r}')
    kernel_spec = _kernel_specs.get(kernel)
    if kernel_spec is None:
        raise StatisticsError(f'Unknown kernel name: {kernel!r}')
    return n, kernel_spec


def kde(data, h, kernel='normal', *, cumulative=False):
    """Kernel density estimate of the data with bandwidth h.

    Returns a function estimating the probability density (or, with
    cumulative=True, the cumulative distribution) at a point x.
    """
    n, kernel_spec = _check_kde_args(data, h, kernel)
    K = kernel_spec['pdf']
    W = kernel_spec['cdf']
    support = kernel_spec['support']

    if support is None:

        def pdf(x):
            return sum(K((x - x_i) / h) for x_i in data) / (len(data) * h)

        def cdf(x):
            return sum(W((x - x_i) / h) for x_i in data) / len(data)

    else:

        sample = sorted(data)
        bandwidth = h * support

        def pdf(x):
            nonlocal n, sample
            if len(data) != n:
                sample = sorted(data)
                n = len(data)
            i = bisect_left(sample, x - bandwidth)
            j = bisect_right(sample, x + bandwidth)
            supported = sample[i:j]
            return sum(K((x - x_i) / h) for x_i in supported) / (n * h)

        def cdf(x):
            nonlocal n, sample
            if len(data) != n:
                sample = sorted(data)
                n = len(data)
            i = bisect_left(sample, x - bandwidth)
            j = bisect_right(sample, x + bandwidth)
            supported = sample[i:j]
            return sum((W((x - x_i) / h) for x_i in supported), i) / n

    if cumulative:
        cdf.__doc__ = f'CDF estimate with {h=!r} and {kernel=!r}'
        return cdf
    else:
        pdf.__doc__ = f'PDF estimate with {h=!r} and {kernel=!r}'
        return pdf


def kde_random(data, h, kernel='normal', *, seed=None):
    """Return a function drawing random samples from the kernel density
    estimate of the data with bandwidth h."""
    n, kernel_spec = _check_kde_args(data, h, kernel)
    invcdf = kernel_spec['invcdf']
    prng = _random.Random(seed)
    random_ = prng.random
    choice = prng.choice

    def rand():
        return choice(data) + h * invcdf(random_())

    rand.__doc__ = f'Random KDE selection with {h=!r} and {kernel=!r}'
    return rand


# ---------------------------------------------------------------- spread

def quantiles(data, *, n=4, method='exclusive'):
    """Divide the data into n intervals of equal probability, returning the
    n - 1 cut points ('exclusive' or 'inclusive' interpolation)."""
    if n < 1:
        raise StatisticsError('n must be at least 1')
    data = sorted(data)
    ld = len(data)
    if ld < 2:
        if ld == 1:
            return data * (n - 1)
        raise StatisticsError('must have at least one data point')

    if method == 'inclusive':
        m = ld - 1
        result = []
        for i in range(1, n):
            j, delta = divmod(i * m, n)
            interpolated = (data[j] * (n - delta) + data[j + 1] * delta) / n
            result.append(interpolated)
        return result

    if method == 'exclusive':
        m = ld + 1
        result = []
        for i in range(1, n):
            j = i * m // n
            j = 1 if j < 1 else ld - 1 if j > ld - 1 else j
            delta = i * m - j * n
            interpolated = (data[j - 1] * (n - delta) + data[j] * delta) / n
            result.append(interpolated)
        return result

    raise ValueError(f'Unknown method: {method!r}')


def variance(data, xbar=None):
    """Sample variance (unbiased, n - 1 in the denominator)."""
    T, ss, c, n = _ss(data, xbar)
    if n < 2:
        raise StatisticsError('variance requires at least two data points')
    return _convert(ss / (n - 1), T)


def pvariance(data, mu=None):
    """Population variance (n in the denominator)."""
    T, ss, c, n = _ss(data, mu)
    if n < 1:
        raise StatisticsError('pvariance requires at least one data point')
    return _convert(ss / n, T)


def stdev(data, xbar=None):
    """Sample standard deviation (square root of the sample variance)."""
    T, ss, c, n = _ss(data, xbar)
    if n < 2:
        raise StatisticsError('stdev requires at least two data points')
    return _sqrt_of_mss(ss / (n - 1), T)


def pstdev(data, mu=None):
    """Population standard deviation."""
    T, ss, c, n = _ss(data, mu)
    if n < 1:
        raise StatisticsError('pstdev requires at least one data point')
    return _sqrt_of_mss(ss / n, T)


# ---------------------------------------------------------------- two inputs

def covariance(x, y, /):
    """Sample covariance of two inputs of the same length."""
    n = len(x)
    if len(y) != n:
        raise StatisticsError('covariance requires that both inputs have same number of data points')
    if n < 2:
        raise StatisticsError('covariance requires at least two data points')
    xbar = fsum(x) / n
    ybar = fsum(y) / n
    sxy = sumprod((xi - xbar for xi in x), (yi - ybar for yi in y))
    return sxy / (n - 1)


def correlation(x, y, /, *, method='linear'):
    """Pearson's correlation coefficient (method='linear') or Spearman's
    rank correlation (method='ranked') of two inputs."""
    n = len(x)
    if len(y) != n:
        raise StatisticsError('correlation requires that both inputs have same number of data points')
    if n < 2:
        raise StatisticsError('correlation requires at least two data points')
    if method not in {'linear', 'ranked'}:
        raise ValueError(f'Unknown method: {method!r}')

    if method == 'ranked':
        start = (n - 1) / -2        # center the ranks around zero
        x = _rank(x, start=start)
        y = _rank(y, start=start)
    else:
        xbar = fsum(x) / n
        ybar = fsum(y) / n
        x = [xi - xbar for xi in x]
        y = [yi - ybar for yi in y]

    sxy = sumprod(x, y)
    sxx = sumprod(x, x)
    syy = sumprod(y, y)

    try:
        return sxy / _sqrtprod(sxx, syy)
    except ZeroDivisionError:
        raise StatisticsError('at least one of the inputs is constant')


LinearRegression = namedtuple('LinearRegression', ('slope', 'intercept'))


def linear_regression(x, y, /, *, proportional=False):
    """Slope and intercept of the ordinary least squares fit y = slope * x +
    intercept (intercept fixed at 0 when proportional=True)."""
    n = len(x)
    if len(y) != n:
        raise StatisticsError('linear regression requires that both inputs have same number of data points')
    if n < 2:
        raise StatisticsError('linear regression requires at least two data points')

    if not proportional:
        xbar = fsum(x) / n
        ybar = fsum(y) / n
        x = [xi - xbar for xi in x]
        y = (yi - ybar for yi in y)

    sxy = sumprod(x, y) + 0.0
    sxx = sumprod(x, x)

    try:
        slope = sxy / sxx
    except ZeroDivisionError:
        raise StatisticsError('x is constant')

    intercept = 0.0 if proportional else ybar - slope * xbar
    return LinearRegression(slope=slope, intercept=intercept)


# ---------------------------------------------------------------- NormalDist

def _normal_dist_inv_cdf(p, mu, sigma):
    # Wichura's algorithm AS241 (PPND16), accurate to about 1e-16.
    q = p - 0.5
    if fabs(q) <= 0.425:
        r = 0.180625 - q * q
        num = (((((((2.5090809287301226727e+3 * r +
                     3.3430575583588128105e+4) * r +
                     6.7265770927008700853e+4) * r +
                     4.5921953931549871457e+4) * r +
                     1.3731693765509461125e+4) * r +
                     1.9715909503065514427e+3) * r +
                     1.3314166789178437745e+2) * r +
                     3.3871328727963666080e+0) * q
        den = (((((((5.226495278852545925e+3 * r +
                     2.8729085735721942674e+4) * r +
                     3.9307895800092710610e+4) * r +
                     2.1213794301586595867e+4) * r +
                     5.3941960214247511077e+3) * r +
                     6.8718700749205790830e+2) * r +
                     4.2313330701600911252e+1) * r +
                     1.0)
        x = num / den
        return mu + (x * sigma)
    r = p if q <= 0.0 else 1.0 - p
    r = sqrt(-log(r))
    if r <= 5.0:
        r = r - 1.6
        num = (((((((7.7454501427834140764e-4 * r +
                     2.2723844989269184583e-2) * r +
                     2.4178072517745061177e-1) * r +
                     1.27045825245236838258e+0) * r +
                     3.64784832476320460504e+0) * r +
                     5.7694972214606914055e+0) * r +
                     4.6303378461565452959e+0) * r +
                     1.4234371107496835773e+0)
        den = (((((((1.0507500716444168432e-9 * r +
                     5.4759380849953449460e-4) * r +
                     1.5198666563616457197e-2) * r +
                     1.4810397642748007459e-1) * r +
                     6.8976733498510000455e-1) * r +
                     1.6763848301838038494e+0) * r +
                     2.0531916266377588219e+0) * r +
                     1.0)
    else:
        r = r - 5.0
        num = (((((((2.0103343992922881327e-7 * r +
                     2.7115555687434875782e-5) * r +
                     1.2426609473880784386e-3) * r +
                     2.6532189526576123093e-2) * r +
                     2.9656057182850489123e-1) * r +
                     1.7848265399172913358e+0) * r +
                     5.4637849111641143699e+0) * r +
                     6.6579046435011037772e+0)
        den = (((((((2.0442631033899397856e-15 * r +
                     1.4215117583164458887e-7) * r +
                     1.8463183175100546818e-5) * r +
                     7.8686913114561329059e-4) * r +
                     1.4875361290850615025e-2) * r +
                     1.3692988092273580531e-1) * r +
                     5.9983220655588793769e-1) * r +
                     1.0)
    x = num / den
    if q < 0.0:
        x = -x
    return mu + (x * sigma)


class NormalDist:
    "Normal distribution of a random variable"

    __slots__ = {
        '_mu': 'Arithmetic mean of a normal distribution',
        '_sigma': 'Standard deviation of a normal distribution',
    }

    def __init__(self, mu=0.0, sigma=1.0):
        "NormalDist where mu is the mean and sigma is the standard deviation."
        if sigma < 0.0:
            raise StatisticsError('sigma must be non-negative')
        self._mu = float(mu)
        self._sigma = float(sigma)

    @classmethod
    def from_samples(cls, data):
        "Make a normal distribution instance from sample data."
        return cls(*_mean_stdev(data))

    def samples(self, n, *, seed=None):
        "Generate *n* samples for a given mean and standard deviation."
        rnd = random.random if seed is None else random.Random(seed).random
        inv_cdf = _normal_dist_inv_cdf
        mu = self._mu
        sigma = self._sigma
        return [inv_cdf(rnd(), mu, sigma) for _ in repeat(None, n)]

    def pdf(self, x):
        "Probability density function.  P(x <= X < x+dx) / dx"
        variance = self._sigma * self._sigma
        if not variance:
            raise StatisticsError('pdf() not defined when sigma is zero')
        diff = x - self._mu
        return exp(diff * diff / (-2.0 * variance)) / sqrt(tau * variance)

    def cdf(self, x):
        "Cumulative distribution function.  P(X <= x)"
        if not self._sigma:
            raise StatisticsError('cdf() not defined when sigma is zero')
        return 0.5 * erfc((self._mu - x) / (self._sigma * _SQRT2))

    def inv_cdf(self, p):
        """Inverse cumulative distribution function: the x with P(X <= x) = p
        (also called the percent point function or quantile function)."""
        if p <= 0.0 or p >= 1.0:
            raise StatisticsError('p must be in the range 0.0 < p < 1.0')
        return _normal_dist_inv_cdf(p, self._mu, self._sigma)

    def quantiles(self, n=4):
        """Divide into *n* continuous intervals with equal probability; return
        the n - 1 cut points."""
        return [self.inv_cdf(i / n) for i in range(1, n)]

    def overlap(self, other):
        """Overlapping coefficient (OVL) between two normal distributions:
        the area shared by both probability density functions."""
        if not isinstance(other, NormalDist):
            raise TypeError('Expected another NormalDist instance')
        X, Y = self, other
        if (Y._sigma, Y._mu) < (X._sigma, X._mu):   # commutativity
            X, Y = Y, X
        X_var, Y_var = X.variance, Y.variance
        if not X_var or not Y_var:
            raise StatisticsError('overlap() not defined when sigma is zero')
        dv = Y_var - X_var
        dm = fabs(Y._mu - X._mu)
        if not dv:
            return 2.0 * NormalDist(dm, 2.0 * X._sigma).cdf(0)
        a = X._mu * Y_var - Y._mu * X_var
        b = X._sigma * Y._sigma * sqrt(dm * dm + dv * log(Y_var / X_var))
        x1 = (a + b) / dv
        x2 = (a - b) / dv
        return 1.0 - (fabs(Y.cdf(x1) - X.cdf(x1)) + fabs(Y.cdf(x2) - X.cdf(x2)))

    def zscore(self, x):
        """Number of standard deviations x lies above the mean."""
        if not self._sigma:
            raise StatisticsError('zscore() not defined when sigma is zero')
        return (x - self._mu) / self._sigma

    @property
    def mean(self):
        "Arithmetic mean of the normal distribution."
        return self._mu

    @property
    def median(self):
        "Return the median of the normal distribution"
        return self._mu

    @property
    def mode(self):
        "Return the mode of the normal distribution"
        return self._mu

    @property
    def stdev(self):
        "Standard deviation of the normal distribution."
        return self._sigma

    @property
    def variance(self):
        "Square of the standard deviation."
        return self._sigma * self._sigma

    def __add__(x1, x2):
        """Add a constant or another (independent) NormalDist."""
        if isinstance(x2, NormalDist):
            return NormalDist(x1._mu + x2._mu, hypot(x1._sigma, x2._sigma))
        return NormalDist(x1._mu + x2, x1._sigma)

    def __sub__(x1, x2):
        """Subtract a constant or another (independent) NormalDist."""
        if isinstance(x2, NormalDist):
            return NormalDist(x1._mu - x2._mu, hypot(x1._sigma, x2._sigma))
        return NormalDist(x1._mu - x2, x1._sigma)

    def __mul__(x1, x2):
        """Multiply by a constant."""
        return NormalDist(x1._mu * x2, x1._sigma * fabs(x2))

    def __truediv__(x1, x2):
        """Divide by a constant."""
        return NormalDist(x1._mu / x2, x1._sigma / fabs(x2))

    def __pos__(x1):
        "Return a copy of the instance."
        return NormalDist(x1._mu, x1._sigma)

    def __neg__(x1):
        "Negates mu while keeping sigma the same."
        return NormalDist(-x1._mu, x1._sigma)

    __radd__ = __add__

    def __rsub__(x1, x2):
        "Subtract a NormalDist from a constant or another NormalDist."
        return -(x1 - x2)

    __rmul__ = __mul__

    def __eq__(x1, x2):
        "Two NormalDist objects are equal if their mu and sigma are both equal."
        if not isinstance(x2, NormalDist):
            return NotImplemented
        return x1._mu == x2._mu and x1._sigma == x2._sigma

    def __hash__(self):
        "NormalDist objects hash equal if their mu and sigma are both equal."
        return hash((self._mu, self._sigma))

    def __repr__(self):
        return f'{type(self).__name__}(mu={self._mu!r}, sigma={self._sigma!r})'

    def __getstate__(self):
        return self._mu, self._sigma

    def __setstate__(self, state):
        self._mu, self._sigma = state
