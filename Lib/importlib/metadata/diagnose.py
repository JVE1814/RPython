"""Report which distributions importlib.metadata can see on each sys.path
entry.  Run as ``python -m importlib.metadata.diagnose``."""

import sys

from . import Distribution


def inspect(path):
    print("Inspecting", path)
    dists = list(Distribution.discover(path=[path]))
    if not dists:
        return
    print("Found", len(dists), "packages:", end=' ')
    print(', '.join(dist.name for dist in dists))


def run():
    for entry in sys.path:
        inspect(entry)


if __name__ == '__main__':
    run()
