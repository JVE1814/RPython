# Stress: test_enum's test_unique_composite, repeated (8 threads creating Flag
# pseudo-members concurrently: set.add with a Python __eq__, enum class dict
# updates).
import sys
import threading
from enum import Flag, IntFlag, auto

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 20


def one_round(base):
    class TestFlag(base):
        one = auto()
        two = auto()
        three = auto()
        four = auto()
        five = auto()
        six = auto()
        seven = auto()
        eight = auto()

        def __eq__(self, other):
            return self is other

        def __hash__(self):
            return hash(self._value_)

    seen = set()
    failed = []

    def cycle_enum():
        try:
            for i in range(256):
                seen.add(TestFlag(i))
        except Exception as e:
            failed.append(e)

    threads = [threading.Thread(target=cycle_enum) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    if failed:
        raise failed[0]
    assert len(seen) == 256, len(seen)


for r in range(ROUNDS):
    one_round(Flag if r % 2 == 0 else IntFlag)
print("ok")
