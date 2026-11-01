"""Deterministic profiler (the classic name of profiling.tracing).

See profiling.tracing for the documentation; `python -m cProfile` takes
the same command line.
"""

from profiling.tracing import Profile, label, run, runctx
from profiling.tracing import _runctx_cli

__all__ = ["run", "runctx", "Profile"]


def main():
    from profiling.tracing import _cli
    _cli.main(_runctx_cli, "cProfile.py")


if __name__ == '__main__':
    main()
