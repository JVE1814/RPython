# Instructions tier 1 runs through eval::exec_misc: exception handlers, with,
# async/await, async for/with, imports, pattern matching, deref/class helpers.
import sys


class CM:
    def __init__(self, log, swallow=False):
        self.log = log
        self.swallow = swallow

    def __enter__(self):
        self.log.append("enter")
        return self

    def __exit__(self, t, v, tb):
        self.log.append(("exit", t.__name__ if t else None))
        return self.swallow


class BadCM:
    def __enter__(self):
        raise KeyError("enter failed")

    def __exit__(self, *a):
        return False


def handlers(n):
    out = []
    for i in range(n):
        try:
            if i % 3 == 0:
                raise ValueError(i)
            elif i % 3 == 1:
                {}[i]
            out.append(i)
        except ValueError as e:
            out.append(("V", e.args[0]))
        except (KeyError, IndexError) as e:
            out.append(("K", type(e).__name__))
        finally:
            out.append("f")
        try:
            try:
                1 / (i % 2)
            except ZeroDivisionError:
                out.append("z")
                raise
        except ArithmeticError as e:
            out.append(type(e).__name__)
    return out


def withs(n):
    log = []
    for i in range(n):
        with CM(log):
            log.append(i)
        with CM(log, swallow=True):
            if i % 2:
                raise RuntimeError("x")
        try:
            with CM(log):
                raise TypeError("t")
        except TypeError:
            log.append("caught")
        try:
            with BadCM():
                log.append("never")
        except KeyError:
            log.append("bad")
        try:
            with 5:
                pass
        except TypeError as e:
            log.append(str(e)[:20])
    return log


def groups(n):
    out = []
    for i in range(n):
        try:
            raise ExceptionGroup("g", [ValueError(i), TypeError(i)])
        except* ValueError as eg:
            out.append(("v", len(eg.exceptions)))
        except* TypeError as eg:
            out.append(("t", len(eg.exceptions)))
    return out


def matching(vals):
    out = []
    for v in vals:
        match v:
            case {"k": x, **rest}:
                out.append(("map", x, sorted(rest)))
            case [a, b, *c]:
                out.append(("seq", a, b, c))
            case CM(log=l):
                out.append(("cls", l))
            case int(x) if x > 2:
                out.append(("int", x))
            case str() | bytes():
                out.append("strish")
            case _:
                out.append(("other", v))
    return out


def imports(n):
    out = []
    for i in range(n):
        import math
        from os import path as p
        from collections import OrderedDict
        out.append((math.floor(i / 2), p.basename("/a/b%d" % i), len(OrderedDict(a=i))))
    try:
        from math import no_such_name
    except ImportError as e:
        out.append(type(e).__name__)
    return out


def closures(n):
    x = 1
    def get():
        return x
    out = []
    for i in range(n):
        x = i
        out.append(get())
    del x
    try:
        get()
    except NameError as e:
        out.append(type(e).__name__)
    return out


def classes(n):
    out = []
    for i in range(n):
        class K:
            v = i
            def m(self):
                return self.v * 2
        out.append(K().m())
    return out


async def leaf(n):
    if n <= 1:
        return n
    return await leaf(n - 1) + await leaf(n - 2)


class Awaitable:
    def __init__(self, v):
        self.v = v

    def __await__(self):
        yield ("suspend", self.v)
        return self.v * 10


class AIter:
    def __init__(self, n):
        self.i = 0
        self.n = n

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self.i >= self.n:
            raise StopAsyncIteration
        self.i += 1
        return await Awaitable(self.i)


class ACM:
    async def __aenter__(self):
        return await Awaitable(7)

    async def __aexit__(self, *a):
        await Awaitable(8)
        return False


async def agen(n):
    for i in range(n):
        yield i * 3


async def amain(n):
    out = []
    for i in range(n):
        out.append(await leaf(i % 8))
        out.append(await Awaitable(i))
        async for x in AIter(3):
            out.append(x)
        async with ACM() as v:
            out.append(v)
        async for y in agen(3):
            out.append(y)
        out.append([z async for z in agen(2)])
    return out


def run(coro):
    sent = []
    try:
        while True:
            sent.append(coro.send(None))
    except StopIteration as e:
        return e.value, len(sent)


def throwing(n):
    def g():
        for i in range(n):
            try:
                yield i
            except KeyError as e:
                yield ("caught", e.args[0])
    def outer():
        r = yield from g()
        return r
    out = []
    it = outer()
    out.append(next(it))
    for i in range(n):
        out.append(it.throw(KeyError(i)))
        out.append(next(it, "end"))
    return out


for k in range(3):
    print(handlers(12))
    print(withs(6))
    print(groups(4))
    print(matching([{"k": 1, "z": 2}, [1, 2, 3, 4], CM([1]), 5, 1, "s", b"b", 2.5, (9, 8)] * 2))
    print(imports(5))
    print(closures(5))
    print(classes(4))
    print(run(amain(10)))
    print(throwing(6))
print(sys.getrecursionlimit() > 0)
