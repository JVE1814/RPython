import sys

from . import main

status = 1
try:
    main()
    status = 0
except Exception as exc:
    print(f'Error: {exc}', file=sys.stderr)
sys.exit(status)
