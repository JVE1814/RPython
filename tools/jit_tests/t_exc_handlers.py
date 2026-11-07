# perf5: raise / re-raise compiled in tier 1, and exceptions caught by a handler of
# the same compiled frame (unwound in Rust, then continued at the handler in compiled
# code): tracebacks, context/cause, sys.exception(), finally/with, loops, nesting.
import sys
import traceback


def tb_lines(e):
    return [(f.name, f.lineno) for f in traceback.extract_tb(e.__traceback__)]


def leaf(x):
    if x % 4 == 0:
        raise KeyError(x)
    if x % 4 == 1:
        return [][x]
    if x % 4 == 2:
        raise ValueError('v%d' % x) from None
    return x


def same_frame(x):
    try:
        if x & 1:
            raise TypeError(x)
        return 'no'
    except TypeError as e:
        return ('caught', e.args, sys.exception() is e, tb_lines(e))


def callee_raises(x):
    try:
        leaf(x)
        return 'ok'
    except LookupError as e:
        return ('lookup', type(e).__name__, tb_lines(e))
    except ValueError as e:
        return ('value', str(e), e.__suppress_context__, tb_lines(e))


def nested(x):
    log = []
    try:
        try:
            leaf(x)
        except KeyError:
            log.append('inner')
            raise RuntimeError('wrapped')
        finally:
            log.append('fin')
    except RuntimeError as e:
        log.append(type(e.__context__).__name__)
        log.append(tb_lines(e))
    except Exception as e:
        log.append(('other', type(e).__name__))
    return log


def reraise(x):
    try:
        try:
            leaf(x)
        except Exception:
            raise
    except Exception as e:
        return (type(e).__name__, tb_lines(e))
    return None


def finally_return(x):
    try:
        leaf(x)
    finally:
        if x % 8 == 0:
            return 'swallowed'
    return 'normal'


class CM:
    def __init__(self, suppress, log):
        self.suppress = suppress
        self.log = log

    def __enter__(self):
        return self

    def __exit__(self, t, v, tb):
        self.log.append(t.__name__ if t else None)
        return self.suppress


def with_stmt(x):
    log = []
    try:
        with CM(x % 3 == 0, log):
            leaf(x)
        log.append('after')
    except Exception as e:
        log.append(('out', type(e).__name__, tb_lines(e)))
    return log


def loop_continue(n):
    total = 0
    errs = 0
    for i in range(n):
        try:
            total += leaf(i)
        except (KeyError, IndexError):
            errs += 1
            continue
        except ValueError:
            errs += 100
            if i > 40:
                break
    return total, errs


def handler_raises(x):
    try:
        try:
            raise KeyError(x)
        except KeyError:
            {}[x + 1]
    except KeyError as e:
        return (e.args, e.__context__.args, tb_lines(e), tb_lines(e.__context__))


def exc_state(x):
    out = []
    try:
        raise ValueError(x)
    except ValueError:
        out.append(type(sys.exception()).__name__)
        try:
            raise KeyError(x)
        except KeyError:
            out.append(type(sys.exception()).__name__)
        out.append(type(sys.exception()).__name__)
    out.append(sys.exception())
    return out


def deep(n):
    if n == 0:
        raise IndexError('bottom')
    try:
        return deep(n - 1)
    except KeyError:
        return 'never'


def catch_deep(n):
    try:
        deep(n)
    except IndexError as e:
        return len(tb_lines(e)), tb_lines(e)[-1]


def raise_class(x):
    try:
        raise StopIteration
    except StopIteration as e:
        return type(e).__name__, e.args


def raise_bad(x):
    try:
        raise 42
    except TypeError as e:
        return str(e)


def raise_from_bad(x):
    try:
        raise ValueError from 7
    except TypeError as e:
        return str(e)


def bare_raise_nothing(x):
    try:
        raise
    except RuntimeError as e:
        return str(e)


def gen_handler(n):
    for i in range(n):
        try:
            yield leaf(i)
        except LookupError:
            yield 'L'
        except ValueError as e:
            yield str(e)


def run():
    out = []
    for i in range(300):
        out.append(same_frame(i))
        out.append(callee_raises(i))
        out.append(nested(i))
        out.append(reraise(i))
        out.append(finally_return(i) if i % 8 in (0, 3, 7) else None)
        out.append(with_stmt(i))
        out.append(handler_raises(i))
        out.append(exc_state(i))
        out.append(raise_class(i))
        out.append(raise_bad(i))
        out.append(raise_from_bad(i))
        out.append(bare_raise_nothing(i))
        if i % 10 == 0:
            out.append(catch_deep(i % 30))
        if i % 25 == 0:
            out.append(loop_continue(60))
            out.append(list(gen_handler(9)))
    try:
        finally_return(1)
    except IndexError as e:
        out.append(('escaped', tb_lines(e)))
    return out


res = run()
print(len(res))
for r in res:
    print(r)
