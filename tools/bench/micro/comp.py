# Building blocks of the comprehensions benchmark (perf7): one kind per run (argv[2]).
# usage: comp.py N KIND   (cost per operation = (I(N2) - I(N1)) / (N2 - N1))
import sys
from dataclasses import dataclass
from enum import Enum


class Kind(Enum):
    BIG = 1
    SMALL = 2


@dataclass
class W:
    wid: int
    kind: Kind
    knob: bool


WS = [W(i, Kind.BIG if i % 2 else Kind.SMALL, i % 3 == 0) for i in range(8)]
OPT = [None, WS[1], WS[2], None, WS[3]]


def anygen(n):
    c = 0
    for _ in range(n):
        c += any(w.knob for w in OPT if w)
    return c


def enumcmp(n):
    c = 0
    w = WS[1]
    for _ in range(n):
        c += w.kind == Kind.BIG
    return c


def listcomp(n):
    c = 0
    for _ in range(n):
        c += len([w for w in WS if w.knob])
    return c


def dictcomp(n):
    c = 0
    for _ in range(n):
        c += len({w.wid: w for w in WS})
    return c


def tuplesort(n):
    c = 0
    for _ in range(n):
        t = [(w.knob, w.wid % 3, w.wid) for w in WS]
        t.sort()
        c += t[0][2]
    return c


def dcinit(n):
    c = 0
    for i in range(n):
        c += W(i, Kind.BIG, True).wid
    return c


print(globals()[sys.argv[2]](int(sys.argv[1])))
