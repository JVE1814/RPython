# Frame introspection of running compiled frames: locals(), f_locals reads and
# writes, frames that outlive their activation, while locals are unboxed in tier 2.
import sys

def peek(depth=1):
    f = sys._getframe(depth + 1)
    return {k: v for k, v in f.f_locals.items() if k in ('i', 't', 'x', 'acc')}

def loop_locals(n):
    t = 0
    x = 0.5
    out = []
    for i in range(n):
        t += i
        x = x * 1.5
        if i % 50 == 7:
            out.append(sorted(locals().items(), key=lambda kv: kv[0])[:0] or (i, t, x))
            out.append(peek())
    return t, x, out

for k in range(40):
    r = loop_locals(200)
print(r)

def poke():
    f = sys._getframe(1)
    f.f_locals['acc'] = 1000

def writes(n):
    acc = 0
    for i in range(n):
        acc += 1
        if i == n // 2:
            poke()          # write through f_locals while acc is unboxed
    return acc

print([writes(100) for _ in range(30)][-3:])

saved = []
def keep_frame(n):
    acc = 0
    for i in range(n):
        acc += i
    saved.append(sys._getframe())
    acc += 1
    return acc

for k in range(30):
    keep_frame(100)
print([(f.f_locals['acc'], f.f_locals['i']) for f in saved[-2:]])

def tb_locals(n):
    acc = 0.0
    for i in range(n):
        acc += 0.5
        if i == n - 1:
            try:
                1 / 0
            except ZeroDivisionError as e:
                return e.__traceback__.tb_frame.f_locals['acc']
for k in range(30):
    v = tb_locals(80)
print(v)
