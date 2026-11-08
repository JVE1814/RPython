# Root cause (pydantic-core test_validate_max_digits_and_decimal_places_pydecimal_huge_exponent):
# this implementation's `decimal` is _pydecimal with the C accelerator's input limits
# (Decimal("1e9999999999999999999") is an InvalidOperation, test_decimal's C variant).
# The limits applied to _pydecimal imported as itself too, where CPython's pure-Python
# module accepts any exponent. They now apply only through `decimal`.
import subprocess
import sys

CHILD = """
import sys, _pydecimal
sys.modules['decimal'] = _pydecimal
import decimal
d = decimal.Decimal('1e9223372036854775808')
assert d.as_tuple().exponent == 9223372036854775808
print("ok")
"""
r = subprocess.run([sys.executable, "-I", "-c", CHILD], capture_output=True, text=True)
assert r.stdout == "ok\n", r.stderr[-1000:]

import decimal  # with the limits
try:
    decimal.Decimal("1e9999999999999999999")
except decimal.InvalidOperation:
    pass
else:
    raise AssertionError("decimal accepted an exponent beyond its limits")
print("OK")
