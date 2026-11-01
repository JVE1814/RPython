"""``python -m json``: validate and pretty-print JSON (see json.tool)."""
import os
import sys

import json.tool

if __name__ == '__main__':
    try:
        json.tool.main()
    except BrokenPipeError as exc:
        # the reader went away; keep the interpreter's final flush quiet
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        sys.exit(exc.errno)
