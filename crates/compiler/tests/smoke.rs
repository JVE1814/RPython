mod common;
use common::*;

#[test]
fn smoke_many() {
    let srcs = [
        "x = 1\nprint(x + 2)\n",
        "def f(a, b=2, *c, d, e=5, **g):\n    return a + b\n",
        "class C(B, metaclass=M):\n    '''doc'''\n    x = 1\n    def m(self):\n        super().m()\n        self.y = 2\n        return __class__\n",
        "for i in range(10):\n    if i: break\n    else: continue\nelse:\n    pass\n",
        "while x:\n    x -= 1\n",
        "try:\n    f()\nexcept E as e:\n    g(e)\nexcept:\n    pass\nelse:\n    h()\nfinally:\n    k()\n",
        "with a as b, c:\n    d()\n",
        "def g():\n    yield 1\n    x = yield from h()\n    return x\n",
        "async def h():\n    async with a as b:\n        await c\n    async for x in y:\n        pass\n    return [z async for z in w]\n",
        "import a.b.c as d\nfrom .x import y, z as w\nfrom m import *\n",
        "x = f'{a!r:>{w}} {b}'\n",
        "l = [x*y for x in a for y in b if x]\nd = {k: v for k, v in it}\ns = {i for i in r}\ng = (i for i in r)\n",
        "def f():\n    x = 1\n    return [lambda: x for x in range(3)]\n",
        "match p:\n    case [1, *rest]: pass\n    case {'a': x, **kw}: pass\n    case Point(x=0, y=y) | Point(x=y, y=0): pass\n    case str() as s if s: pass\n    case _: pass\n",
        "def f[T: int, *Ts, **P](x: T) -> T: return x\nclass G[T](Base): pass\ntype A[X] = list[X]\n",
        "x: int = 1\nclass K:\n    y: str\n    if c:\n        z: int\n",
        "try:\n    pass\nexcept* ValueError as eg:\n    pass\n",
        "del a, b[0], c.d\na, *b, c = x\na[1:2] += 3\n",
        "assert x, 'msg'\nraise E from F\nglobal q\n",
        "t = t'{x} and {y!r}'\n",
        "def outer():\n    v = 1\n    def inner():\n        nonlocal v\n        v += 1\n    return inner\n",
        "x = 1 if a else 2\ny = a < b < c\nz = not (a and b or c)\n",
        "f(*a, **k)\nf(1, *a, b=2, **k)\no.m(1, k=2)\n",
    ];
    for s in srcs {
        let c = comp(s);
        let d = rpy_compiler::dis::disassemble(&c);
        assert!(!d.is_empty());
    }
}
