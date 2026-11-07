"""Basic statistics for numeric data.

Averages and measures of central location (mean, fmean, geometric_mean,
harmonic_mean, median and its variants, mode, multimode), measures of
spread (variance, stdev, pvariance, pstdev), quantiles, bivariate
statistics (covariance, correlation, linear_regression), kernel density
estimation (kde, kde_random) and the NormalDist class.

Unless noted otherwise the functions accept ints, floats, Fractions and
Decimals (not mixed with each other, except that ints mix with anything)
and return a value of the same type as the data:

>>> mean([1, 2, 3, 4, 4])
2.8
>>> median([Fraction(1, 3), Fraction(1, 2), Fraction(5, 6)])
Fraction(1, 2)
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

from bisect import bisect_left, bisect_right
from collections import namedtuple
from decimal import Decimal
from fractions import Fraction
from itertools import groupby, repeat


class StatisticsError(ValueError):
    pass


# ---------------------------------------------------------------------------
# Exact arithmetic helpers.  Data values are summed exactly as fractions and
# only the final result is converted back to the type of the data.

def _exact_ratio(x):
    """Return (numerator, denominator) for x, or (x, None) for an infinity
    or a NaN."""
    try:
        return x.as_integer_ratio()
    except AttributeError:
        pass
    except (OverflowError, ValueError):
        return (x, None)
    try:
        return (x.numerator, x.denominator)
    except AttributeError:
        raise TypeError("can't convert type '%s' to numerator/denominator"
                        % type(x).__name__) from None


def _coerce(T, S):
    """The type a mixture of values of types T and S is reported as.

    ints (and bools) mix with anything; a type mixes with its subclasses
    (giving the subclass); Fractions mix with floats (giving the float
    type).  Anything else is a TypeError.
    """
    assert T is not bool, "the first argument may not be bool"
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
    raise TypeError("can't mix %s and %s" % (T.__name__, S.__name__))


def _sum(data):
    """Sum data exactly; return (type, total, count).

    The total is a Fraction, except when the data holds infinities or NaNs:
    then it is the sum of those special values.
    """
    count = 0
    T = int
    by_den = {}         # denominator -> sum of numerators
    for typ, values in groupby(data, type):
        T = _coerce(T, typ)
        for n, d in map(_exact_ratio, values):
            count += 1
            by_den[d] = by_den.get(d, 0) + n
    if None in by_den:
        total = by_den[None]
    else:
        total = Fraction(0)
        for d, n in by_den.items():
            total += Fraction(n, d)
    return (T, total, count)


def _ss(data, c=None):
    """Sum of squared deviations of data; return (type, total, count).

    Deviations are taken from c when given, else from the exact mean.  The
    total is exact (a Fraction) unless the data holds infinities or NaNs.
    """
    if c is not None:
        return _sum((x - c) * (x - c) for x in data)
    count = 0
    T = int
    sums = {}           # denominator -> (sum of numerators, of squares)
    special = False
    for typ, values in groupby(data, type):
        T = _coerce(T, typ)
        for n, d in map(_exact_ratio, values):
            count += 1
            if d is None:
                special = True
                continue
            s, s2 = sums.get(d, (0, 0))
            sums[d] = (s + n, s2 + n * n)
    if special:
        return (T, Decimal('NaN') if issubclass(T, Decimal) else math.nan,
                count)
    if not count:
        return (T, Fraction(0), 0)
    sx = Fraction(0)
    sxx = Fraction(0)
    for d, (s, s2) in sums.items():
        sx += Fraction(s, d)
        sxx += Fraction(s2, d * d)
    return (T, sxx - sx * sx / count, count)


def _isfinite(x):
    try:
        return x.is_finite()        # Decimal
    except AttributeError:
        return math.isfinite(x)


def _convert(value, T):
    """Convert value (normally a Fraction) to type T."""
    if type(value) is T:
        return value
    if issubclass(T, int) and value.denominator != 1:
        T = float
    try:
        return T(value)
    except TypeError:
        if issubclass(T, Decimal):
            # Decimal does not accept Fractions directly
            return T(value.numerator) / value.denominator
        raise


def _fail_neg(values, errmsg='negative value'):
    """Pass values through, raising StatisticsError at the first negative."""
    for x in values:
        if x < 0:
            raise StatisticsError(errmsg)
        yield x


# ---------------------------------------------------------------------------
# Correctly rounded square roots of fractions.

def _integer_sqrt_of_frac_rto(n, m):
    """floor(sqrt(n/m)), with the lowest bit set when the root is inexact
    ("round to odd", which makes a later rounding step exact)."""
    a = math.isqrt(n // m)
    return a | (a * a * m != n)


# number of bits kept in the intermediate integer root; comfortably more
# than the 53 bits of a float plus the two needed for correct rounding
_ROOT_BITS = sys.float_info.mant_dig + 8


def _float_sqrt_of_frac(n, m):
    """sqrt(n/m) correctly rounded to a float."""
    q = (n.bit_length() - m.bit_length() - 2 * _ROOT_BITS) // 2
    if q >= 0:
        return float(_integer_sqrt_of_frac_rto(n, m << 2 * q) << q)
    return _integer_sqrt_of_frac_rto(n << -2 * q, m) / (1 << -q)


def _decimal_sqrt_of_frac(n, m):
    """sqrt(n/m) correctly rounded to a Decimal in the current context."""
    if n <= 0:
        if not n:
            return Decimal('0.0')
        n, m = -n, -m
    root = (Decimal(n) / Decimal(m)).sqrt()
    # The division above rounded once already, so the root may be one unit
    # off; compare the squares of the midpoints to the neighbours exactly.
    rn, rd = root.as_integer_ratio()
    above = root.next_plus()
    an, ad = above.as_integer_ratio()
    if (rn * ad + an * rd) ** 2 * m < n * (2 * rd * ad) ** 2:
        return above
    below = root.next_minus()
    bn, bd = below.as_integer_ratio()
    if (rn * bd + bn * rd) ** 2 * m > n * (2 * rd * bd) ** 2:
        return below
    return root


def _sqrtprod(x, y):
    """sqrt(x * y) correctly rounded, without overflow or underflow in the
    product."""
    if not (math.isfinite(x) and math.isfinite(y)) or not x or not y:
        return math.sqrt(x * y)
    if (x < 0) != (y < 0):
        raise ValueError('math domain error')
    xn, xd = abs(x).as_integer_ratio()
    yn, yd = abs(y).as_integer_ratio()
    return _float_sqrt_of_frac(xn * yn, xd * yd)


# ---------------------------------------------------------------------------
# Averages and measures of central location

def mean(data):
    """Arithmetic mean of data, computed exactly and returned in the data's
    type.

    >>> mean([Decimal('0.5'), Decimal('0.75'), Decimal('0.625')])
    Decimal('0.625')
    """
    T, total, n = _sum(data)
    if n < 1:
        raise StatisticsError('mean requires at least one data point')
    return _convert(total / n, T)


def fmean(data, weights=None):
    """Fast floating-point arithmetic mean, optionally weighted.

    >>> fmean([3.5, 4.0, 5.25])
    4.25
    """
    if weights is None:
        if not isinstance(data, (list, tuple)):
            data = list(data)
        n = len(data)
        if not n:
            raise StatisticsError('fmean requires at least one data point')
        return math.fsum(data) / n
    if not isinstance(data, (list, tuple)):
        data = list(data)
    if not isinstance(weights, (list, tuple)):
        weights = list(weights)
    if len(data) != len(weights):
        raise StatisticsError('data and weights must be the same length')
    total_weight = math.fsum(weights)
    if not total_weight:
        raise StatisticsError('sum of weights must be non-zero')
    return math.sumprod(data, weights) / total_weight


def geometric_mean(data):
    """Geometric mean of positive data, as a float (via logarithms).

    >>> round(geometric_mean([54, 24, 36]), 9)
    36.0
    """
    count = 0
    zero_seen = False

    def logs(values):
        nonlocal count, zero_seen
        for x in values:
            count += 1
            if x > 0.0 or math.isnan(x):
                yield math.log(x)
            elif x == 0.0:
                zero_seen = True
            else:
                raise StatisticsError('No negative inputs allowed', x)

    total = math.fsum(logs(data))
    if not count:
        raise StatisticsError('geometric mean requires a non-empty dataset '
                              'containing positive numbers')
    if zero_seen:
        # a zero wins, unless an infinity or NaN makes the product undefined
        return 0.0 if math.isfinite(total) else math.nan
    return math.exp(total / count)


def harmonic_mean(data, weights=None):
    """Harmonic mean of non-negative data, optionally weighted.

    >>> harmonic_mean([40, 60], weights=[5, 30])
    56.0
    """
    if not isinstance(data, list):
        data = list(data)
    n = len(data)
    errmsg = 'harmonic mean does not support negative values'
    if n < 1:
        raise StatisticsError('harmonic_mean requires at least one data point')
    if n == 1 and weights is None:
        x = data[0]
        if not isinstance(x, (numbers.Real, Decimal)):
            raise TypeError('unsupported type')
        if x < 0:
            raise StatisticsError(errmsg)
        return x
    if weights is None:
        weights = repeat(1, n)
        weight_total = n
    else:
        if not isinstance(weights, list):
            weights = list(weights)
        if len(weights) != n:
            raise StatisticsError('Number of weights does not match data size')
        _, weight_total, _ = _sum(_fail_neg(weights, errmsg))
    try:
        T, total, _ = _sum(w / x if w else 0
                           for w, x in zip(weights, _fail_neg(data, errmsg)))
    except ZeroDivisionError:
        return 0
    if total <= 0:
        raise StatisticsError('Weighted sum must be positive')
    return _convert(weight_total / total, T)


def median(data):
    """Median of data; the mean of the two middle values for an even count.

    >>> median([1, 3, 5, 7])
    4.0
    """
    data = sorted(data)
    n = len(data)
    if not n:
        raise StatisticsError('no median for empty data')
    half = n // 2
    if n % 2:
        return data[half]
    return (data[half - 1] + data[half]) / 2


def median_low(data):
    """Low median: the smaller of the two middle values for an even count."""
    data = sorted(data)
    n = len(data)
    if not n:
        raise StatisticsError('no median for empty data')
    return data[(n - 1) // 2]


def median_high(data):
    """High median: the larger of the two middle values for an even count."""
    data = sorted(data)
    n = len(data)
    if not n:
        raise StatisticsError('no median for empty data')
    return data[n // 2]


def median_grouped(data, interval=1.0):
    """Median of grouped continuous data, as the 50th percentile.

    Each value x stands for the class interval x - interval/2 to
    x + interval/2; the median is interpolated inside the class that holds
    the middle value.

    >>> median_grouped([1, 2, 2, 3, 4, 4, 4, 4, 4, 5])
    3.7
    """
    data = sorted(data)
    n = len(data)
    if not n:
        raise StatisticsError('no median for empty data')
    middle = data[n // 2]
    below = bisect_left(data, middle)               # values in lower classes
    width = bisect_right(data, middle, lo=below) - below
    try:
        interval = float(interval)
        middle = float(middle)
    except ValueError:
        raise TypeError('Value cannot be converted to a float') from None
    lower_limit = middle - interval / 2.0
    return lower_limit + interval * (n / 2 - below) / width


def mode(data):
    """Most common value of discrete or nominal data (the first one seen
    when there is a tie).

    >>> mode(['red', 'blue', 'blue', 'red', 'green', 'red'])
    'red'
    """
    counts = _tally(data)
    if not counts:
        raise StatisticsError('no mode for empty data')
    return max(counts.items(), key=_second)[0]


def multimode(data):
    """List of the most common values, in order of first appearance.

    >>> multimode('aabbbbccddddeeffffgg')
    ['b', 'd', 'f']
    """
    counts = _tally(data)
    if not counts:
        return []
    top = max(counts.values())
    return [value for value, k in counts.items() if k == top]


def _tally(data):
    counts = {}
    get = counts.get
    for value in iter(data):
        counts[value] = get(value, 0) + 1
    return counts


def _second(pair):
    return pair[1]


# ---------------------------------------------------------------------------
# Measures of spread

def variance(data, xbar=None):
    """Sample variance of data (n - 1 in the denominator).

    xbar, if given, is used as the centre instead of the mean.

    >>> variance([1, 2, 3, 4])
    1.6666666666666667
    """
    T, ss, n = _ss(data, xbar)
    if n < 2:
        raise StatisticsError('variance requires at least two data points')
    return _convert(ss / (n - 1), T)


def pvariance(data, mu=None):
    """Population variance of data (n in the denominator).

    mu, if given, is used as the centre instead of the mean.

    >>> pvariance([1, 2, 3, 4])
    1.25
    """
    T, ss, n = _ss(data, mu)
    if n < 1:
        raise StatisticsError('pvariance requires at least one data point')
    return _convert(ss / n, T)


def _sqrt_of(value, T, name):
    if not isinstance(value, Fraction):
        raise ValueError('%s is undefined for data containing '
                         'infinities or NaNs' % name)
    if issubclass(T, Decimal):
        return _decimal_sqrt_of_frac(value.numerator, value.denominator)
    return _float_sqrt_of_frac(value.numerator, value.denominator)


def stdev(data, xbar=None):
    """Sample standard deviation: the square root of the sample variance.

    >>> stdev([1, 3])
    1.4142135623730951
    """
    T, ss, n = _ss(data, xbar)
    if n < 2:
        raise StatisticsError('stdev requires at least two data points')
    return _sqrt_of(ss / (n - 1), T, 'stdev')


def pstdev(data, mu=None):
    """Population standard deviation: the square root of pvariance.

    >>> pstdev([2, 4, 4, 4, 5, 5, 7, 9])
    2.0
    """
    T, ss, n = _ss(data, mu)
    if n < 1:
        raise StatisticsError('pstdev requires at least one data point')
    return _sqrt_of(ss / n, T, 'pstdev')


# ---------------------------------------------------------------------------
# Quantiles

def quantiles(data, *, n=4, method='exclusive'):
    """Divide data into n intervals of equal probability; return the n - 1
    cut points.

    method 'exclusive' treats data as a sample from a larger population
    (the extremes may lie beyond the data); 'inclusive' treats it as the
    whole population (or as including the extremes).

    >>> quantiles([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], n=4)
    [2.75, 5.5, 8.25]
    """
    if n < 1:
        raise StatisticsError('n must be at least 1')
    data = sorted(data)
    size = len(data)
    if size < 2:
        if size == 1:
            return data * (n - 1)
        raise StatisticsError('must have at least one data point')
    cuts = []
    if method == 'inclusive':
        # cut point i lies at 0-based position i*(size-1)/n
        span = size - 1
        for i in range(1, n):
            k, rem = divmod(i * span, n)
            cuts.append((data[k] * (n - rem) + data[k + 1] * rem) / n)
        return cuts
    if method == 'exclusive':
        # cut point i lies at 1-based position i*(size+1)/n, clamped to the
        # first and last pair of points
        span = size + 1
        for i in range(1, n):
            k = min(max(i * span // n, 1), size - 1)
            rem = i * span - k * n
            cuts.append((data[k - 1] * (n - rem) + data[k] * rem) / n)
        return cuts
    raise ValueError('Unknown method: %r' % (method,))


# ---------------------------------------------------------------------------
# Bivariate statistics

def _check_pair(x, y, what):
    n = len(x)
    if len(y) != n:
        raise StatisticsError('%s requires that both inputs have same number '
                              'of data points' % what)
    if n < 2:
        raise StatisticsError('%s requires at least two data points' % what)
    return n


def covariance(x, y, /):
    """Sample covariance of two equally long inputs.

    >>> covariance([1, 2, 3, 4, 5], [5, 4, 3, 2, 1])
    -2.5
    """
    n = _check_pair(x, y, 'covariance')
    xbar = math.fsum(x) / n
    ybar = math.fsum(y) / n
    sxy = math.sumprod([xi - xbar for xi in x], [yi - ybar for yi in y])
    return sxy / (n - 1)


def _ranks(values):
    """Ranks 1..n of values; tied values share the mean of their ranks."""
    order = sorted(range(len(values)), key=values.__getitem__)
    result = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        v = values[order[i]]
        while j + 1 < len(order) and values[order[j + 1]] == v:
            j += 1
        shared = (i + j) / 2 + 1
        for k in range(i, j + 1):
            result[order[k]] = shared
        i = j + 1
    return result


def correlation(x, y, /, *, method='linear'):
    """Correlation coefficient of two inputs, between -1 and +1.

    method 'linear' gives Pearson's coefficient, 'ranked' Spearman's (the
    Pearson coefficient of the ranks).

    >>> correlation([1, 2, 3, 4, 5], [2, 4, 6, 8, 10])
    1.0
    """
    n = _check_pair(x, y, 'correlation')
    if method not in ('linear', 'ranked'):
        raise ValueError('Unknown method: %r' % (method,))
    if method == 'ranked':
        x = _ranks(list(x))
        y = _ranks(list(y))
    xbar = math.fsum(x) / n
    ybar = math.fsum(y) / n
    dx = [xi - xbar for xi in x]
    dy = [yi - ybar for yi in y]
    sxy = math.sumprod(dx, dy)
    sxx = math.sumprod(dx, dx)
    syy = math.sumprod(dy, dy)
    try:
        return sxy / _sqrtprod(sxx, syy)
    except ZeroDivisionError:
        raise StatisticsError('at least one of the inputs is constant') \
            from None


LinearRegression = namedtuple('LinearRegression', ('slope', 'intercept'))


def linear_regression(x, y, /, *, proportional=False):
    """Ordinary least squares fit y = slope * x + intercept.

    With proportional=True the line is forced through the origin
    (intercept 0.0).

    >>> linear_regression([1, 2, 3, 4, 5], [3, 5, 7, 9, 11])
    LinearRegression(slope=2.0, intercept=1.0)
    """
    n = _check_pair(x, y, 'linear regression')
    if proportional:
        dx, dy = x, y
    else:
        xbar = math.fsum(x) / n
        ybar = math.fsum(y) / n
        dx = [xi - xbar for xi in x]
        dy = [yi - ybar for yi in y]
    sxy = math.sumprod(dx, dy) + 0.0      # always a float
    sxx = math.sumprod(dx, dx)
    try:
        slope = sxy / sxx
    except ZeroDivisionError:
        raise StatisticsError('x is constant') from None
    intercept = 0.0 if proportional else ybar - slope * xbar
    return LinearRegression(slope=slope, intercept=intercept)


# ---------------------------------------------------------------------------
# Kernel density estimation.  Each kernel is described by its density, its
# cumulative distribution, the inverse of that, and the half width of its
# support (None for kernels with unbounded support).

_SQRT2 = math.sqrt(2.0)
_INV_SQRT_TAU = 1.0 / math.sqrt(math.tau)


def _gauss_pdf(t):
    return math.exp(-0.5 * t * t) * _INV_SQRT_TAU


def _gauss_cdf(t):
    return 0.5 * math.erfc(-t / _SQRT2)


def _gauss_invcdf(p):
    return _normal_dist_inv_cdf(p, 0.0, 1.0)


def _logistic_pdf(t):
    e = math.exp(-abs(t))
    return e / ((1.0 + e) * (1.0 + e))


def _logistic_cdf(t):
    if t >= 0:
        return 1.0 / (1.0 + math.exp(-t))
    e = math.exp(t)
    return e / (1.0 + e)


def _logistic_invcdf(p):
    return math.log(p / (1.0 - p))


# 'sigmoid' is the kernel 1/(pi cosh t), written to avoid overflow

def _sigmoid_pdf(t):
    e = math.exp(-abs(t))
    return 2.0 / math.pi * e / (1.0 + e * e)


def _sigmoid_cdf(t):
    if t > 0:
        return 1.0 - 2.0 / math.pi * math.atan(math.exp(-t))
    return 2.0 / math.pi * math.atan(math.exp(t))


def _sigmoid_invcdf(p):
    return math.log(math.tan(p * math.pi / 2.0))


def _uniform_pdf(t):
    return 0.5


def _uniform_cdf(t):
    return 0.5 * t + 0.5


def _uniform_invcdf(p):
    return 2.0 * p - 1.0


def _triangle_pdf(t):
    return 1.0 - abs(t)


def _triangle_cdf(t):
    if t >= 0:
        return 1.0 - (1.0 - t) * (1.0 - t) / 2.0
    return (1.0 + t) * (1.0 + t) / 2.0


def _triangle_invcdf(p):
    if p < 0.5:
        return math.sqrt(2.0 * p) - 1.0
    return 1.0 - math.sqrt(2.0 - 2.0 * p)


# The polynomial kernels' distribution functions are written with integer
# coefficients so that they are exactly 0 and 1 at the ends of the support.

def _parabolic_pdf(t):
    return 0.75 * (1.0 - t * t)


def _parabolic_cdf(t):
    return 0.5 + t * (3.0 - t * t) / 4.0


def _parabolic_invcdf(p):
    # with t = 2 sin(a): t(3 - t^2)/2 = sin(3a)
    return 2.0 * math.sin(math.asin(2.0 * p - 1.0) / 3.0)


def _quartic_pdf(t):
    u = 1.0 - t * t
    return 0.9375 * u * u


def _quartic_cdf(t):
    s = t * t
    return 0.5 + t * (15.0 + s * (-10.0 + 3.0 * s)) / 16.0


def _quartic_invcdf(p):
    return _solve_cdf(_quartic_cdf, _quartic_pdf, p)


def _triweight_pdf(t):
    u = 1.0 - t * t
    return 1.09375 * u * u * u


def _triweight_cdf(t):
    s = t * t
    return 0.5 + t * (35.0 + s * (-35.0 + s * (21.0 - 5.0 * s))) / 32.0


def _triweight_invcdf(p):
    return _solve_cdf(_triweight_cdf, _triweight_pdf, p)


def _cosine_pdf(t):
    return math.pi / 4.0 * math.cos(math.pi / 2.0 * t)


def _cosine_cdf(t):
    return 0.5 + 0.5 * math.sin(math.pi / 2.0 * t)


def _cosine_invcdf(p):
    return 2.0 * math.asin(2.0 * p - 1.0) / math.pi


def _solve_cdf(cdf, pdf, p):
    """Invert a kernel cdf on [-1, 1]: Newton steps kept inside a shrinking
    bracket, falling back to bisection."""
    if p <= 0.0:
        return -1.0
    if p >= 1.0:
        return 1.0
    lo, hi = -1.0, 1.0
    x = _parabolic_invcdf(p)        # a similar shape: a good first guess
    for _ in range(100):
        err = cdf(x) - p
        if err == 0.0:
            return x
        if err > 0.0:
            hi = x
        else:
            lo = x
        slope = pdf(x)
        nxt = x - err / slope if slope > 0.0 else lo - 1.0
        if not lo < nxt < hi:
            nxt = 0.5 * (lo + hi)
        if abs(nxt - x) <= 1e-16:
            return nxt
        x = nxt
    return x


def _make_kernel_specs():
    table = {}
    for names, pdf, cdf, invcdf, support in [
        (('normal', 'gauss'), _gauss_pdf, _gauss_cdf, _gauss_invcdf, None),
        (('logistic',), _logistic_pdf, _logistic_cdf, _logistic_invcdf, None),
        (('sigmoid',), _sigmoid_pdf, _sigmoid_cdf, _sigmoid_invcdf, None),
        (('rectangular', 'uniform'), _uniform_pdf, _uniform_cdf,
         _uniform_invcdf, 1.0),
        (('triangular',), _triangle_pdf, _triangle_cdf, _triangle_invcdf, 1.0),
        (('parabolic', 'epanechnikov'), _parabolic_pdf, _parabolic_cdf,
         _parabolic_invcdf, 1.0),
        (('quartic', 'biweight'), _quartic_pdf, _quartic_cdf,
         _quartic_invcdf, 1.0),
        (('triweight',), _triweight_pdf, _triweight_cdf, _triweight_invcdf,
         1.0),
        (('cosine',), _cosine_pdf, _cosine_cdf, _cosine_invcdf, 1.0),
    ]:
        spec = dict(pdf=pdf, cdf=cdf, invcdf=invcdf, support=support)
        for name in names:
            table[name] = spec
    return table


_kernel_specs = _make_kernel_specs()


def _kernel(kernel):
    try:
        return _kernel_specs[kernel]
    except (KeyError, TypeError):
        raise StatisticsError(f'Unknown kernel name: {kernel!r}') from None


def _check_kde_args(data, h):
    n = len(data)
    if not n:
        raise StatisticsError('Empty data sequence')
    if not isinstance(data[0], numbers.Real):
        raise TypeError('Data sequence must contain ints or floats')
    if h <= 0.0:
        raise StatisticsError(f'Bandwidth h must be positive, not {h!r}')


def kde(data, h, kernel='normal', *, cumulative=False):
    """Kernel density estimate of the distribution of a sample.

    Returns a function estimating the probability density (or, with
    cumulative=True, the cumulative distribution) at a point.  h is the
    bandwidth; kernel names the smoothing kernel: 'normal' ('gauss'),
    'logistic', 'sigmoid', 'rectangular' ('uniform'), 'triangular',
    'parabolic' ('epanechnikov'), 'quartic' ('biweight'), 'triweight' or
    'cosine'.  Later changes to the data sequence are taken into account.

    >>> f = kde([0.0], h=1.0)
    >>> round(f(0.0), 4)
    0.3989
    """
    _check_kde_args(data, h)
    spec = _kernel(kernel)
    K = spec['cdf'] if cumulative else spec['pdf']
    support = spec['support']

    if support is None:
        if cumulative:
            def cdf(x):
                return sum(K((x - xi) / h) for xi in data) / len(data)
        else:
            def pdf(x):
                return sum(K((x - xi) / h) for xi in data) / (len(data) * h)
    else:
        reach = h * support
        cache = [None, None]        # [length seen, sorted copy]

        def window(x):
            if cache[0] != len(data):
                cache[1] = sorted(data)
                cache[0] = len(data)
            ordered = cache[1]
            return (ordered, bisect_left(ordered, x - reach),
                    bisect_right(ordered, x + reach))

        if cumulative:
            def cdf(x):
                ordered, i, j = window(x)
                inside = sum(K((x - xi) / h) for xi in ordered[i:j])
                return (i + inside) / len(ordered)
        else:
            def pdf(x):
                ordered, i, j = window(x)
                inside = sum(K((x - xi) / h) for xi in ordered[i:j])
                return inside / (len(ordered) * h)

    if cumulative:
        cdf.__doc__ = f'CDF estimate with h={h!r} and kernel={kernel!r}'
        return cdf
    pdf.__doc__ = f'PDF estimate with h={h!r} and kernel={kernel!r}'
    return pdf


def kde_random(data, h, kernel='normal', *, seed=None):
    """Return a function drawing random values from the kernel density
    estimate of data (see kde()).  seed makes the draws reproducible.

    >>> draw = kde_random([-2.1, -1.3, -0.4, 1.9, 5.1, 6.2], 1.5, seed=8675309)
    >>> isinstance(draw(), float)
    True
    """
    _check_kde_args(data, h)
    invcdf = _kernel(kernel)['invcdf']
    prng = random.Random(seed)
    uniform = prng.random
    choose = prng.choice

    def rand():
        p = uniform()
        while p == 0.0:             # keep the inverse cdf finite
            p = uniform()
        return choose(data) + h * invcdf(p)

    rand.__doc__ = f'Random KDE selection with h={h!r} and kernel={kernel!r}'
    return rand


# ---------------------------------------------------------------------------
# Normal distribution

# Wichura's algorithm AS 241 (Applied Statistics 37, 1988) for the inverse
# of the standard normal distribution, accurate to about 1e-16.  The
# coefficients are listed from the highest power down.
_AS241_CENTRAL = (
    (2.5090809287301226727e+3, 3.3430575583588128105e+4,
     6.7265770927008700853e+4, 4.5921953931549871457e+4,
     1.3731693765509461125e+4, 1.9715909503065514427e+3,
     1.3314166789178437745e+2, 3.3871328727963666080e+0),
    (5.2264952788528545610e+3, 2.8729085735721942674e+4,
     3.9307895800092710610e+4, 2.1213794301586595867e+4,
     5.3941960214247511077e+3, 6.8718700749205790830e+2,
     4.2313330701600911252e+1, 1.0),
)
_AS241_NEAR = (
    (7.74545014278341407640e-4, 2.27238449892691845833e-2,
     2.41780725177450611770e-1, 1.27045825245236838258e+0,
     3.64784832476320460504e+0, 5.76949722146069140550e+0,
     4.63033784615654529590e+0, 1.42343711074968357734e+0),
    (1.05075007164441684324e-9, 5.47593808499534494600e-4,
     1.51986665636164571966e-2, 1.48103976427480074590e-1,
     6.89767334985100004550e-1, 1.67638483018380384940e+0,
     2.05319162663775882187e+0, 1.0),
)
_AS241_FAR = (
    (2.01033439929228813265e-7, 2.71155556874348757815e-5,
     1.24266094738807843860e-3, 2.65321895265761230930e-2,
     2.96560571828504891230e-1, 1.78482653991729133580e+0,
     5.46378491116411436990e+0, 6.65790464350110377720e+0),
    (2.04426310338993978564e-15, 1.42151175831644588870e-7,
     1.84631831751005468180e-5, 7.86869131145613259100e-4,
     1.48753612908506148525e-2, 1.36929880922735805310e-1,
     5.99832206555887937690e-1, 1.0),
)


def _poly_ratio(coefficients, r):
    top, bottom = coefficients
    num = 0.0
    for c in top:
        num = num * r + c
    den = 0.0
    for c in bottom:
        den = den * r + c
    return num / den


def _normal_dist_inv_cdf(p, mu, sigma):
    q = p - 0.5
    if abs(q) <= 0.425:
        z = q * _poly_ratio(_AS241_CENTRAL, 0.180625 - q * q)
        return mu + z * sigma
    tail = p if q <= 0.0 else 1.0 - p
    r = math.sqrt(-math.log(tail))
    if r <= 5.0:
        z = _poly_ratio(_AS241_NEAR, r - 1.6)
    else:
        z = _poly_ratio(_AS241_FAR, r - 5.0)
    if q < 0.0:
        z = -z
    return mu + z * sigma


try:
    from _statistics import _normal_dist_inv_cdf
except ImportError:
    pass


class NormalDist:
    """Normal distribution with mean mu and standard deviation sigma."""

    __slots__ = ('_mu', '_sigma')

    def __init__(self, mu=0.0, sigma=1.0):
        if sigma < 0.0:
            raise StatisticsError('sigma must be non-negative')
        self._mu = float(mu)
        self._sigma = float(sigma)

    @classmethod
    def from_samples(cls, data):
        """Estimate mu and sigma from a sample."""
        if not isinstance(data, (list, tuple)):
            data = list(data)
        return cls(fmean(data), stdev(data))

    def samples(self, n, *, seed=None):
        """Return n random values from the distribution."""
        gauss = random.gauss if seed is None else random.Random(seed).gauss
        mu, sigma = self._mu, self._sigma
        return [gauss(mu, sigma) for _ in repeat(None, n)]

    def pdf(self, x):
        """Probability density at x."""
        var = self._sigma * self._sigma
        if not var:
            raise StatisticsError('pdf() not defined when sigma is zero')
        d = x - self._mu
        return math.exp(d * d / (-2.0 * var)) / math.sqrt(math.tau * var)

    def cdf(self, x):
        """Probability that a random value is at most x."""
        if not self._sigma:
            raise StatisticsError('cdf() not defined when sigma is zero')
        return 0.5 * math.erfc((self._mu - x) / (self._sigma * _SQRT2))

    def inv_cdf(self, p):
        """The value x with cdf(x) == p (the quantile function)."""
        if p <= 0.0 or p >= 1.0:
            raise StatisticsError('p must be in the range 0.0 < p < 1.0')
        return _normal_dist_inv_cdf(p, self._mu, self._sigma)

    def quantiles(self, n=4):
        """The n - 1 cut points dividing the distribution into n intervals
        of equal probability."""
        return [self.inv_cdf(i / n) for i in range(1, n)]

    def overlap(self, other):
        """Overlapping coefficient: the area shared by the two densities,
        between 0.0 and 1.0."""
        if not isinstance(other, NormalDist):
            raise TypeError('Expected another NormalDist instance')
        # a is the narrower distribution (ties broken by the mean)
        a, b = self, other
        if (b._sigma, b._mu) < (a._sigma, a._mu):
            a, b = b, a
        var_a, var_b = a.variance, b.variance
        if not var_a or not var_b:
            raise StatisticsError('overlap() not defined when sigma is zero')
        dvar = var_b - var_a
        dmu = abs(b._mu - a._mu)
        if not dvar:
            # one crossing point, half way between the means
            return math.erfc(dmu / (2.0 * a._sigma * _SQRT2))
        # the densities cross where a quadratic in x vanishes
        centre = a._mu * var_b - b._mu * var_a
        spread = a._sigma * b._sigma * math.sqrt(
            dmu * dmu + dvar * math.log(var_b / var_a))
        x1 = (centre + spread) / dvar
        x2 = (centre - spread) / dvar
        return 1.0 - (abs(b.cdf(x1) - a.cdf(x1)) + abs(b.cdf(x2) - a.cdf(x2)))

    def zscore(self, x):
        """Number of standard deviations x lies above the mean."""
        if not self._sigma:
            raise StatisticsError('zscore() not defined when sigma is zero')
        return (x - self._mu) / self._sigma

    @property
    def mean(self):
        """Arithmetic mean."""
        return self._mu

    @property
    def median(self):
        """Median (equal to the mean)."""
        return self._mu

    @property
    def mode(self):
        """Mode (equal to the mean)."""
        return self._mu

    @property
    def stdev(self):
        """Standard deviation."""
        return self._sigma

    @property
    def variance(self):
        """Variance (the square of the standard deviation)."""
        return self._sigma * self._sigma

    # Arithmetic: adding or subtracting another NormalDist combines
    # independent variables; a plain number shifts or scales.

    def __add__(x1, x2):
        if isinstance(x2, NormalDist):
            return NormalDist(x1._mu + x2._mu, math.hypot(x1._sigma, x2._sigma))
        return NormalDist(x1._mu + x2, x1._sigma)

    def __sub__(x1, x2):
        if isinstance(x2, NormalDist):
            return NormalDist(x1._mu - x2._mu, math.hypot(x1._sigma, x2._sigma))
        return NormalDist(x1._mu - x2, x1._sigma)

    def __mul__(x1, x2):
        return NormalDist(x1._mu * x2, x1._sigma * abs(x2))

    def __truediv__(x1, x2):
        return NormalDist(x1._mu / x2, x1._sigma / abs(x2))

    def __pos__(x1):
        return NormalDist(x1._mu, x1._sigma)

    def __neg__(x1):
        return NormalDist(-x1._mu, x1._sigma)

    __radd__ = __add__

    def __rsub__(x1, x2):
        return -(x1 - x2)

    __rmul__ = __mul__

    def __eq__(x1, x2):
        if not isinstance(x2, NormalDist):
            return NotImplemented
        return x1._mu == x2._mu and x1._sigma == x2._sigma

    def __hash__(self):
        return hash((self._mu, self._sigma))

    def __repr__(self):
        return f'{type(self).__name__}(mu={self._mu!r}, sigma={self._sigma!r})'

    def __getstate__(self):
        return self._mu, self._sigma

    def __setstate__(self, state):
        self._mu, self._sigma = state
