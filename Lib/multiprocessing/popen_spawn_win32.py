#
# multiprocessing.popen_spawn_win32: the spawn start method on Windows.
#
# This implementation targets POSIX systems only; on Windows the module
# would start children with CreateProcess and pass handles with
# DuplicateHandle.
#

import sys

__all__ = ['Popen']

if sys.platform != 'win32':
    raise ImportError('multiprocessing.popen_spawn_win32 is only available '
                      'on Windows')

raise ImportError('Windows process creation is not supported by this build')
