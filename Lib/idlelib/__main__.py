"""python -m idlelib: start IDLE."""
import os
import sys

# With -m, the first sys.path entry is the current directory: modules
# there must not replace the standard modules IDLE imports (user code
# gets the directory back).
if sys.path and (not sys.path[0] or
                 os.path.abspath(sys.path[0]) == os.getcwd()):
    del sys.path[0]

import idlelib.pyshell  # noqa: E402
idlelib.pyshell.main()
