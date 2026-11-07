"""Start a script for the sampling profiler and tell it when it is running.

Usage: python -m profiling.sampling._sync_coordinator PORT CWD TARGET [ARGS...]

The profiler listens on PORT; this process changes to CWD, connects, sends
b"ready" and then runs TARGET (a script path, or "-m module") as
``python TARGET ARGS`` would, so that sampling starts with the program.
"""

import os
import runpy
import socket
import sys


def _signal_readiness(port):
    with socket.create_connection(('127.0.0.1', port)) as conn:
        conn.sendall(b'ready')


def _run_script(path, args):
    full = os.path.abspath(path)
    if not os.path.exists(full):
        sys.exit('Script file not found: %s' % path)
    # like `python script.py`: the real directory of the script comes first
    sys.path[0] = os.path.dirname(os.path.realpath(full))
    sys.argv = [path] + list(args)
    runpy.run_path(full, run_name='__main__')


def _run_module(name, args):
    sys.path[0] = os.getcwd()
    sys.argv = [name] + list(args)
    runpy.run_module(name, run_name='__main__', alter_sys=True)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) < 3:
        sys.exit(__doc__.strip().splitlines()[2])
    port, cwd, target, args = int(argv[0]), argv[1], argv[2], argv[3:]
    os.chdir(cwd)
    _signal_readiness(port)
    if target == '-m':
        if not args:
            sys.exit('missing module name after -m')
        _run_module(args[0], args[1:])
    else:
        _run_script(target, args)


if __name__ == '__main__':
    main()
