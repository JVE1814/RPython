class Outer:
    def meth(self, x: Nested) -> int: ...
    x: Nested
    class Nested: ...
print(Outer.meth.__annotate__(1))
print(Outer.__annotate_func__(1) if "__annotate_func__" in Outer.__dict__ else Outer.__dict__.keys())
x: int = 3
if len:
    y: str
print(__annotate__(1))
try:
    __annotate__(3)
except NotImplementedError:
    print("NIE ok")
def f(format): 
    def inner(x: format): pass
    return inner
print(f("closure").__annotate__(1))
