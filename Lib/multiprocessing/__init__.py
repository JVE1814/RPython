#
# multiprocessing: process-based parallelism with an API modelled on the
# threading module.
#
# The package's public functions are the methods of the default context,
# exported here as module-level functions.
#

import sys
from . import context

__all__ = [name for name in dir(context._default_context)
           if not name.startswith('_')]
globals().update((name, getattr(context._default_context, name))
                 for name in __all__)

SUBDEBUG = 5
SUBWARNING = 25

# Spawned children import the parent's main module under the name
# '__mp_main__'; in the parent it is simply __main__.
if '__main__' in sys.modules:
    sys.modules['__mp_main__'] = sys.modules['__main__']
