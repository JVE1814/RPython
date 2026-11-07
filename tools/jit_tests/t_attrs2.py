# perf5: tier-1 attribute paths - attribute-adding stores (shape transitions),
# LoadMethod of plain attributes (module functions, class attributes), getset
# descriptors called directly, class attributes that instances may shadow.
import sys
import types
import os.path as osp

mod = types.ModuleType('m')
mod.f = lambda x: x + 1
mod.g = len


class P:
    def __init__(self, a, b, order):
        if order:
            self.a = a
            self.b = b
        else:
            self.b = b
            self.a = a
        self.c = a + b


class Wide:
    def __init__(self, n):
        self.x0 = n
        self.x1 = n
        self.x2 = n
        self.x3 = n
        self.x4 = n
        self.x5 = n
        self.x6 = n
        self.x7 = n


class K:
    shared = 10
    lst = [1]

    def get(self):
        return self.shared


def use_module(n):
    t = 0
    for i in range(n):
        t += mod.f(i) + mod.g('ab') + len(osp.basename('/a/b%d' % i))
        if i == n // 2:
            mod.f = lambda x: x * 2
    return t


def getsets(n):
    out = []
    for i in range(n):
        fr = sys._getframe()
        out.append((fr.f_code.co_name, fr.f_back.f_code.co_name, fr.f_lineno))
        out.append(getsets.__code__.co_filename.endswith('t_attrs2.py'))
        o = P(i, 1, True)
        d = o.__dict__
        out.append(sorted(d))
    return out[-6:], len(out)


def shadow(n):
    out = []
    ks = [K() for _ in range(4)]
    for i in range(n):
        k = ks[i % 4]
        out.append(k.shared)
        out.append(k.get())
        if i == n // 3:
            ks[1].shared = 'own'
        if i == n // 2:
            K.shared = 20
        if i == 2 * n // 3:
            ks[2].__dict__['shared'] = 'dict'
    return out


def inits(n):
    out = []
    for i in range(n):
        p = P(i, -i, i % 3 != 0)
        out.append((p.a, p.b, p.c, list(p.__dict__)))
        if i % 7 == 0:
            del p.c
            p.d = 5
            out.append(sorted(vars(p).items()))
        w = Wide(i)
        if i % 11 == 0:
            w.extra = 1
            out.append(len(vars(w)))
        out.append(w.x7)
    return out


res = [use_module(400), getsets(300), shadow(300), inits(300)]
for r in res:
    print(r)
