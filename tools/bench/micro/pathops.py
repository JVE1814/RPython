# pathlib / os.stat micro benchmark (perf5): one operation kind per run (argv[2]).
# usage: pathops.py N KIND   (cost per op via tools/bench/micro/percall.sh)
import os
import sys
import pathlib
import tempfile

d = tempfile.mkdtemp()
names = []
for i in range(50):
    fn = os.path.join(d, '%d.py' % i)
    with open(fn, 'wb') as f:
        f.write(b'x')
    names.append(fn)
base = pathlib.Path(d)


class S:
    __slots__ = ('a', 'b')


def run(n, kind):
    k = len(names)
    if kind == 'stat':          # os.stat(str) + one field
        for i in range(n):
            os.stat(names[i % k]).st_mode
    elif kind == 'newpath':     # Path(text) as glob() builds it
        for i in range(n):
            base.with_segments(names[i % k])
    elif kind == 'newstr':      # Path(text) then str(): lazy parse (two AttributeErrors)
        for i in range(n):
            str(base.with_segments(names[i % k]))
    elif kind == 'pstat':       # Path.stat() of an iterdir() child (string cached)
        kids = list(base.iterdir())
        m = len(kids)
        for i in range(n):
            kids[i % m].stat()
    elif kind == 'iterdir':     # per entry of iterdir()
        for i in range(n // k):
            for p in base.iterdir():
                pass
    elif kind == 'glob':        # per entry of glob('*.py')
        for i in range(n // k):
            for p in base.glob('*.py'):
                pass
    elif kind == 'attrerr':     # unset slot read caught in the same frame
        o = S()
        for i in range(n):
            try:
                o.a
            except AttributeError:
                pass
    elif kind == 'attrerr_call':  # unset slot read caught in a called function
        o = S()
        f = _attr
        for i in range(n):
            f(o)
    elif kind == 'newinit':     # class with __new__(cls, *a, **k) and __init__(self, *a)
        for i in range(n):
            NI('x')
    elif kind == 'withseg':     # type(self)(*segments) from a method with *segments
        o = NI('a')
        for i in range(n):
            o.with_segments('x')
    elif kind == 'superinit':   # super().__init__(x) into a base __init__
        for i in range(n):
            SubInit('p', i)
    elif kind == 'fspath':      # os.fspath(path) (cached string)
        p = base / 'x'
        str(p)
        for i in range(n):
            os.fspath(p)
    else:
        raise SystemExit('unknown kind ' + kind)


class NI:
    __slots__ = ('_raw',)

    def __new__(cls, *args, **kwargs):
        return object.__new__(cls)

    def __init__(self, *args):
        raw = []
        for a in args:
            raw.append(a)
        self._raw = raw

    def with_segments(self, *segs):
        return type(self)(*segs)


class BaseInit:
    __slots__ = ('_p',)

    def __init__(self, p):
        self._p = p


class SubInit(BaseInit):
    __slots__ = ('_e',)

    def __init__(self, p, e):
        super().__init__(p)
        self._e = e


def _attr(o):
    try:
        return o.a
    except AttributeError:
        return None


run(int(sys.argv[1]), sys.argv[2])
