# (perf8) int / float and int * float with ints of one, two and more 30-bit digits
# (tier 2 converts one- and two-digit ints inline; larger ones deoptimise), signs, and
# the rounding of 2^53+ values; a nanosecond difference crosses 2^30 after a second.
def rel(stamp, start):
    return (stamp - start) / 1e6

def mixed(a, f):
    return a * f + a / f - (a - f)

vals = [0, 1, -1, 7, 2**29, 2**30 - 1, 2**30, 2**30 + 1, -(2**30), 2**31 + 12345,
        2**53 + 1, -(2**53 + 3), 2**59 + 2**31 + 7, 2**60 - 1, -(2**60 - 1), 2**60,
        2**61 + 5, 1_700_000_000_123_456_789, -(2**75), 3**50]
out = []
for rnd in range(30):
    for v in vals:
        out.append(rel(v, 3))
        out.append(mixed(v, 0.5))
        out.append(mixed(v, -3.25))
print(out[:60])
print(out[-60:])
print(sum(x for x in out if x == x) == sum(x for x in out if x == x))
print(len(out), repr(rel(2**30 + 999, 0)), repr(rel(2**60 - 1, 0)))
