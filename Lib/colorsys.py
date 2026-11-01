"""Conversions between RGB and the YIQ, HLS and HSV color models.

Each model ABC has a pair of functions

    rgb_to_abc(r, g, b) -> (a, b, c)
    abc_to_rgb(a, b, c) -> (r, g, b)

Coordinates are floats; RGB, H, L, S and V lie in [0, 1], while the I and
Q chroma components of YIQ can be slightly negative or above 1.
"""

__all__ = ["rgb_to_yiq", "yiq_to_rgb", "rgb_to_hls", "hls_to_rgb",
           "rgb_to_hsv", "hsv_to_rgb"]

ONE_THIRD = 1.0 / 3.0
ONE_SIXTH = 1.0 / 6.0
TWO_THIRD = 2.0 / 3.0


def _clamp(x):
    return 0.0 if x < 0.0 else 1.0 if x > 1.0 else x


# ---------------------------------------------------------------- YIQ
# NTSC luma/chroma with the FCC coefficients.  The inverse matrix is
# written out numerically.

def rgb_to_yiq(r, g, b):
    y = 0.30 * r + 0.59 * g + 0.11 * b
    i = 0.74 * (r - y) - 0.27 * (b - y)
    q = 0.48 * (r - y) + 0.41 * (b - y)
    return (y, i, q)


def yiq_to_rgb(y, i, q):
    r = y + 0.9468822170900693 * i + 0.6235565819861433 * q
    g = y - 0.27478764629897834 * i - 0.6356910791873801 * q
    b = y - 1.1085450346420322 * i + 1.7090069284064666 * q
    return (_clamp(r), _clamp(g), _clamp(b))


# ---------------------------------------------------------------- hue helper

def _hue(r, g, b, top, spread):
    """Hue in [0, 1) of a non-grey color with maximum component `top` and
    max-min difference `spread`."""
    if r == top:
        sector = (g - b) / spread
    elif g == top:
        sector = 2.0 + (b - r) / spread
    else:
        sector = 4.0 + (r - g) / spread
    return (sector / 6.0) % 1.0


# ---------------------------------------------------------------- HLS

def rgb_to_hls(r, g, b):
    top = max(r, g, b)
    bottom = min(r, g, b)
    total = top + bottom
    spread = top - bottom
    lightness = total / 2.0
    if spread == 0:
        return 0.0, lightness, 0.0
    if lightness <= 0.5:
        saturation = spread / total
    else:
        # computed from both extremes for accuracy near white
        saturation = spread / (2.0 - top - bottom)
    return _hue(r, g, b, top, spread), lightness, saturation


def _hls_channel(low, high, h):
    h = h % 1.0
    if h < ONE_SIXTH:
        return low + (high - low) * 6.0 * h
    if h < 0.5:
        return high
    if h < TWO_THIRD:
        return low + (high - low) * 6.0 * (TWO_THIRD - h)
    return low


def hls_to_rgb(h, l, s):
    if s == 0.0:
        return l, l, l
    high = l * (1.0 + s) if l <= 0.5 else l + s - l * s
    low = 2.0 * l - high
    return (_hls_channel(low, high, h + ONE_THIRD),
            _hls_channel(low, high, h),
            _hls_channel(low, high, h - ONE_THIRD))


# ---------------------------------------------------------------- HSV

def rgb_to_hsv(r, g, b):
    top = max(r, g, b)
    bottom = min(r, g, b)
    spread = top - bottom
    if spread == 0:
        return 0.0, 0.0, top
    return _hue(r, g, b, top, spread), spread / top, top


def hsv_to_rgb(h, s, v):
    if s == 0.0:
        return v, v, v
    sector = int(h * 6.0)
    frac = h * 6.0 - sector
    p = v * (1.0 - s)
    q = v * (1.0 - s * frac)
    t = v * (1.0 - s * (1.0 - frac))
    return [(v, t, p), (q, v, p), (p, v, t),
            (p, q, v), (t, p, v), (v, p, q)][sector % 6]
