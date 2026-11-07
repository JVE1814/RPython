"""Entry point of ``python -m profiling.sampling``."""

import sys

from .cli import main
from .errors import SamplingError


def _run():
    try:
        main()
    except SamplingError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)
    except PermissionError as exc:
        print(f"Permission Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    _run()
