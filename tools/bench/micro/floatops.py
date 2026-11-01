import sys
def f(n):
    x = 0.0
    y = 1.5
    for i in range(n):
        x = x * 0.5 + y * 1.25 - 0.125
        y = y + x / 3.0
        if y > 1000.0:
            y = 1.0
    return x + y
print(f(int(sys.argv[1]) if len(sys.argv) > 1 else 1000000))
