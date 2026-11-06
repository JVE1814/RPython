"""Start IDLE from this file (python idle.py, or python -m idlelib.idle)."""
import os
import sys

# The first sys.path entry is the current directory (-m) or idlelib's own
# directory (as a script); neither may shadow the standard modules IDLE
# imports.  idlelib's parent must be importable instead.
_here = os.path.dirname(os.path.abspath(__file__))
if sys.path and (not sys.path[0] or
                 os.path.abspath(sys.path[0]) in (os.getcwd(), _here)):
    del sys.path[0]
_parent = os.path.dirname(_here)
if _parent not in sys.path:
    sys.path.insert(0, _parent)
del _here, _parent

from idlelib.pyshell import main  # noqa: E402

main()
