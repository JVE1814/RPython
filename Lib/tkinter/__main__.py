"""Entry point for `python -m tkinter`: show a small demo window."""

import sys

if sys.argv[0].endswith("__main__.py"):
    sys.argv[0] = "python -m tkinter"

from . import _test as main
main()
