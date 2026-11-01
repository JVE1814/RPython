# Cycle collection (young/old generations): garbage cycles are freed, reachable
# ones survive, finalizers run once.
import gc

freed = []

class Node:
    def __init__(self, name):
        self.name = name
        self.other = None
    def __del__(self):
        freed.append(self.name)

keep = []
def churn(n):
    for i in range(n):
        a = Node(('a', i))
        b = Node(('b', i))
        a.other = b
        b.other = a
        if i % 1000 == 0:
            keep.append(a)          # reachable from an old object
        try:
            raise ValueError(i)
        except ValueError as e:
            err = e

churn(20000)
gc.collect()
gc.collect()
kept = set(k.name for k in keep)
print(len(keep), all(k.other.other is k for k in keep))
print(any(name in kept for name in freed), len(freed) >= 2 * (20000 - len(keep)) - 10)

# old object pointing at young garbage keeps it alive until it is dropped
holder = [Node('held')]
holder[0].other = holder[0]
for _ in range(5):
    churn(3000)
print('held' in freed)
holder.clear()
gc.collect()
print('held' in freed)
