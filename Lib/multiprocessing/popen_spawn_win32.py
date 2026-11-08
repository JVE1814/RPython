#
# multiprocessing.popen_spawn_win32 -- the Windows "spawn" start method.
#
# This build targets POSIX systems only.  On Windows the module would start
# the child with CreateProcess, hand it the pickled process through an
# inherited pipe handle and wait on the process handle; none of that is
# available here, so importing the module fails.
#

import sys

__all__ = ['Popen']

if sys.platform != 'win32':
    raise ImportError('multiprocessing.popen_spawn_win32 is only available '
                      'on Windows')

raise ImportError('Windows process creation is not supported by this build')
