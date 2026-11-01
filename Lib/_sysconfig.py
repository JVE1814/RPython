"""Configuration values that are fixed when the interpreter is built.

``config_vars()`` returns the variables sysconfig cannot derive from a
data file (they describe the running binary itself).
"""

import sys


def config_vars():
    """Return a new dict of the interpreter's build-defining variables."""
    return {
        'Py_GIL_DISABLED': 1 if 't' in getattr(sys, 'abiflags', '') else 0,
        'Py_DEBUG': 1 if hasattr(sys, 'gettotalrefcount') else 0,
        'Py_ENABLE_SHARED': 0,
        'ABIFLAGS': sys.abiflags,
    }
