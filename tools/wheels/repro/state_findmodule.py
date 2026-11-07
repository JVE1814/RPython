# Root cause: a single-phase extension module (PyModule_Create in PyInit_*) was not
# registered for PyState_FindModule after its init function returned.  pandas' ujson
# keeps the Decimal/Series/DataFrame/Index types in its module state and finds that
# state with PyState_FindModule(&moduledef); with NULL every type check failed and the
# encoder fell back to walking dir(obj) until "Maximum recursion level reached".
# (pandas io/json: ~700 failures.)  Run with the venv python that has pandas.
import decimal
import sys

try:
    from pandas.io.json import ujson_dumps
    import pandas as pd
except ImportError:
    print("SKIP: pandas not installed")
    sys.exit(0)

assert ujson_dumps(decimal.Decimal("1.5")) == '"1.5"', ujson_dumps(decimal.Decimal("1.5"))
assert ujson_dumps(pd.Series([1, 2])) == '{"0":1,"1":2}'
assert ujson_dumps(pd.DataFrame({"a": [1]})) == '{"a":{"0":1}}'
print("OK")
