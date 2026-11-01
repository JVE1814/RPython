def f(x):
    class C:
        y = x
        def m(self):
            return x
        z = 12
    return C
c = f(1)
print(c.y, c().m(), c.z)
x = 12
class Global:
    global x
    x = 13
    def set(self, val):
        x = val
    def get(self):
        return x
g = Global()
print(g.get(), x)
def test():
    method_and_var = "var"
    class Test:
        def method_and_var(self):
            return "method"
        def test(self):
            return method_and_var
        def actual_global(self):
            return str("global")
        def str(self):
            return str(self)
    return Test()
t = test()
print(t.test(), t.method_and_var(), t.actual_global())
def adaptgetter(name, klass, getter):
    kind, des = getter
    if kind == 1:
        if des == "":
            des = "_" + klass.__name__ + "__" + name
        return lambda obj: getattr(obj, des)
class TestClass:
    pass
print(adaptgetter("foo", TestClass, (1, "")).__name__)
def make_adder(x):
    def adder(y):
        return x + y
    return adder
print(make_adder(1)(2))
def outer2():
    x = 1
    class Inner:
        x = 2
        def get(self):
            return x
        y = [x for _ in [1]]
    return Inner().get(), Inner.y
print(outer2())
def overrides():
    x = 1
    class C:
        locals()["x"] = 5 if False else None
        x = 2
        z = x
    return C.z
print(overrides())
def classnamespace():
    x = 1
    class C:
        nonlocal x
        x = 3
    return x
print(classnamespace())
def dels():
    x = 1
    def g():
        return x
    del x
    try:
        g()
    except NameError as e:
        print("NameError", type(e).__name__)
dels()
def ns_override():
    x = 42
    class X:
        locals()["x"] = 43
        y = x
    return X.y
print(ns_override())
