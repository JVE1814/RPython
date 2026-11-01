def indent(code):
    out = "    "
    for ch in code:
        out = out + ch
        if ch == "\n":
            out = out + "    "
    return out

def check(name, code, outputs=None, ns=None, scopes=("module", "class", "function"), raises=None):
    for scope in scopes:
        if scope == "class":
            newcode = "class _C:\n" + indent(code)
            get = lambda d, k: getattr(d["_C"], k)
        elif scope == "function":
            newcode = "def _f():\n" + indent(code) + "\n    return locals()\n_out = _f()\n"
            get = lambda d, k: d["_out"][k]
        else:
            newcode = code
            get = lambda d, k: d[k]
        d = dict(ns) if ns else {}
        try:
            exec(newcode, d)
        except Exception as e:
            if raises is not None and type(e) is raises:
                continue
            print("FAIL", name, scope, "raised", type(e).__name__, e)
            continue
        if raises is not None:
            print("FAIL", name, scope, "expected", raises.__name__)
            continue
        for k, v in (outputs or {}).items():
            got = get(d, k)
            if got != v:
                print("FAIL", name, scope, k, got, "!=", v)
    print("done", name)

check("defaults", "items = [(lambda i=i: i) for i in range(5)]\ny = [x() for x in items]", {"y": [0, 1, 2, 3, 4]})
check("free_var", "items = [(lambda: i) for i in range(5)]\ny = [x() for x in items]", {"y": [4, 4, 4, 4, 4]})
check("super", "res = [super for x in [1]]", {"res": [super]})
check("__class__", "res = [__class__ for x in [1]]", raises=NameError)
check("__class__nested", "res = [(lambda: __class__)() for _ in [1]]", raises=NameError)
check("__class__defined", "__class__ = 2\nres = [__class__ for x in [1]]", {"res": [2]}, scopes=["module", "function"])
check("__class__defined_cls", "__class__ = 2\nres = [__class__ for x in [1]]", scopes=["class"], raises=NameError)
check("inner_cell_shadows", "items = [(lambda: i) for i in range(5)]\ni = 20\ny = [x() for x in items]", {"y": [4, 4, 4, 4, 4], "i": 20})
check("inner_cell_no_store", "def f(x):\n    return [lambda: x for x in range(x)], x\nfns, x = f(2)\ny = [fn() for fn in fns]", {"y": [1, 1], "x": 2})
check("jump_over", "items = [(lambda: y) for i in range(5)]\ny = 2\nz = [x() for x in items]", {"z": [2, 2, 2, 2, 2]}, scopes=["module", "function"])
check("cell_inner_free_outer", "def f():\n    return [lambda: x for x in (x, [1])[1]]\nx = ...\ny = [fn() for fn in f()]", {"y": [1]}, scopes=["module", "function"])
check("free_inner_cell_outer", "g = 2\ndef f():\n    return g\ny = [g for x in [1]]", {"y": [2]}, scopes=["module", "function"])
check("free_inner_cell_outer_cls", "g = 2\ndef f():\n    return g\ny = [g for x in [1]]", scopes=["class"], raises=NameError)
check("redefined", "y = 10\nitems = [(lambda: y) for y in range(5)]\nx = y\ny = 20\nout = [z() for z in items]", {"x": 10, "out": [4, 4, 4, 4, 4]})
check("shadows_outer_cell", "def inner():\n    return g\n[g for g in range(5)]\nx = inner()", {"x": -1}, ns={"g": -1})
check("explicit_global", "global g\nx = g\ng = 2\nitems = [g for g in [1]]\ny = g", {"x": 1, "y": 2, "items": [1]}, ns={"g": 1})
check("explicit_global_2", "global g\nx = g\ng = 2\nitems = [g for x in [1]]\ny = g", {"x": 1, "y": 2, "items": [2]}, ns={"g": 1})
check("explicit_global_3", "global g\nfns = [lambda: g for g in [2]]\nitems = [fn() for fn in fns]", {"items": [2]}, ns={"g": 1})
check("walrus", "x = -1\nitems = [(x:=y) for y in range(3)]", {"x": 2}, scopes=["module", "function"])
check("free_in_comp_child", "lst = range(3)\nfuncs = [lambda: x for x in lst]\ninc = [x + 1 for x in lst]\n[x for x in inc]\nx = funcs[0]()", {"x": 2})
check("shadow_free_local", "lst = range(3)\nx = -1\nfuncs = [lambda: x for x in lst]\nitems = [x + 1 for x in lst]", {"x": -1})
check("iterable_name", "x = [1]\ny = [x for x in x]", {"x": [1]})
check("nested_free", "x = 1\ndef g():\n    [x for x in range(3)]\n    return x\ng()", {"x": 1}, scopes=["module", "function"])
check("nested", "l = [2, 3]\ny = [[x ** 2 for x in range(x)] for x in l]", {"y": [[0, 1], [0, 1, 4]]})
check("nested_2", "l = [1, 2, 3]\nx = 3\ny = [x for [x ** x for x in range(x)][x - 1] in l]", {"y": [3, 3, 3]}, scopes=["module", "function"])
check("nested_2_cls", "l = [1, 2, 3]\nx = 3\ny = [x for [x ** x for x in range(x)][x - 1] in l]", scopes=["class"], raises=NameError)
check("exception_restores", "x = 1\ntry:\n    [x for x in [1, 0] if 1 / x]\nexcept ZeroDivisionError:\n    pass\ny = x", {"y": 1})
check("unbound_after", "[i for i in [1]]\ntry:\n    i\n    r = 'bound'\nexcept NameError:\n    r = 'unbound'", {"r": "unbound"}, scopes=["module", "class"])
check("dict_set", "s = {x for x in 'ab'}\nd = {k: v for k, v in [(1, 2)]}", {"s": {"a", "b"}, "d": {1: 2}})
check("unpack_798", "a = [*x for x in [[1, 2], [3]]]\nb = {**d for d in [{1: 2}, {3: 4}]}", {"a": [1, 2, 3], "b": {1: 2, 3: 4}})
