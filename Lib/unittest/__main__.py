"""Entry point for 'python -m unittest'."""

import sys

if sys.argv[0].endswith('__main__.py'):
    import os.path
    # show "python -m unittest" in usage messages instead of a path
    sys.argv[0] = os.path.basename(sys.executable) + ' -m unittest'
    del os

__unittest = True

from .main import main

main(module=None)
