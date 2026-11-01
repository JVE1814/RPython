"""Decimal fixed- and floating-point arithmetic.

This module implements the General Decimal Arithmetic Specification (and the
decimal parts of IEEE 754-2008).  Numbers are exact: a sign, an integer
coefficient and a power-of-ten exponent, plus infinities and NaNs.  Results
of operations are rounded according to a Context (precision, rounding mode,
exponent limits); exceptional conditions set sticky flags in the context and
raise an exception when the corresponding trap is enabled.

The implementation is the _decimal extension module when available, else the
pure-Python _pydecimal module.
"""

try:
    from _decimal import *
    from _decimal import __libmpdec_version__
except ImportError:
    import _pydecimal
    import sys
    _pydecimal.__doc__ = __doc__
    sys.modules[__name__] = _pydecimal
else:
    def __getattr__(name):
        if name == "__version__":
            import warnings
            warnings.warn("'__version__' is deprecated and slated for removal in Python 3.20",
                          DeprecationWarning, stacklevel=2)
            return SPEC_VERSION
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
