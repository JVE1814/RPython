# perf4: exceptions propagating out of compiled frames that have no handler
# (unwound without the interpreter) - tracebacks, context chaining, re-raise, finally.
import traceback

def leaf(x):
    if x % 3 == 0:
        raise KeyError(x)
    if x % 3 == 1:
        return {}[x]
    raise AttributeError('a%d' % x)

def mid(x):
    y = x + 1
    return leaf(y - 1) + y

def mid_finally(x, log):
    try:
        return mid(x)
    finally:
        log.append(x)

def ctx(x):
    try:
        leaf(x)
    except LookupError:
        mid(x + 1)

def rer(x):
    try:
        mid(x)
    except Exception:
        raise

def frm(x):
    try:
        mid(x)
    except Exception as e:
        raise ValueError(x) from e

def describe(e):
    tb = traceback.extract_tb(e.__traceback__)
    c = e.__context__
    return (type(e).__name__, str(e), [(f.name, f.lineno) for f in tb],
            type(c).__name__ if c is not None else None,
            type(e.__cause__).__name__ if e.__cause__ is not None else None)

def run():
    out = []
    log = []
    for i in range(400):
        for f in (mid, ctx, rer, frm):
            try:
                f(i)
            except Exception as e:
                if i % 37 == 0:
                    out.append(describe(e))
        try:
            mid_finally(i, log)
        except Exception as e:
            if i % 41 == 0:
                out.append(describe(e))
    out.append(len(log))
    return out

for x in run():
    print(x)
