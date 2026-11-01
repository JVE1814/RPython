#
# multiprocessing.spawn: starting a fresh interpreter as a child.
#
# The parent pickles "preparation data" (sys.path, cwd, how to import the
# main module, ...) and the Process object into a pipe; the child runs
# spawn_main(), which restores that state and calls Process._bootstrap().
#

import os
import sys
import runpy
import types

from . import get_start_method, set_start_method
from . import process
from .context import reduction
from . import util

__all__ = ['_main', 'freeze_support', 'set_executable', 'get_executable',
           'get_preparation_data', 'get_command_line', 'import_main_path']

WINEXE = False
WINSERVICE = False

_python_exe = sys.executable


def set_executable(exe):
    """Interpreter used for spawned children (str, bytes or path-like)."""
    global _python_exe
    _python_exe = os.fsdecode(exe) if exe is not None else None


def get_executable():
    return _python_exe


def is_forking(argv):
    """True if *argv* is the command line of a spawned child."""
    return len(argv) >= 2 and argv[1] == '--multiprocessing-fork'


def freeze_support():
    """In a frozen executable started as a child, run the child code and
    exit; otherwise do nothing."""
    if is_forking(sys.argv):
        kwds = {}
        for arg in sys.argv[2:]:
            name, value = arg.split('=')
            kwds[name] = None if value == 'None' else int(value)
        spawn_main(**kwds)
        sys.exit()


def get_command_line(**kwds):
    """argv starting a child that runs spawn_main(**kwds)."""
    if getattr(sys, 'frozen', False):
        return ([sys.executable, '--multiprocessing-fork'] +
                ['%s=%r' % item for item in kwds.items()])
    args = ', '.join('%s=%r' % item for item in kwds.items())
    prog = 'from multiprocessing.spawn import spawn_main; spawn_main(%s)' % args
    opts = util._args_from_interpreter_flags()
    return [get_executable()] + opts + ['-c', prog, '--multiprocessing-fork']


def spawn_main(pipe_handle, parent_pid=None, tracker_fd=None):
    """Entry point of a spawned child."""
    assert is_forking(sys.argv), "Not forking"
    if tracker_fd is not None:
        from . import resource_tracker
        resource_tracker._resource_tracker._fd = tracker_fd
    # The read end of the pipe doubles as the "parent died" sentinel.
    parent_sentinel = os.dup(pipe_handle)
    exitcode = _main(pipe_handle, parent_sentinel)
    sys.exit(exitcode)


def _main(fd, parent_sentinel):
    with os.fdopen(fd, 'rb', closefd=True) as from_parent:
        process.current_process()._inheriting = True
        try:
            preparation_data = reduction.pickle.load(from_parent)
            prepare(preparation_data)
            self = reduction.pickle.load(from_parent)
        finally:
            del process.current_process()._inheriting
    return self._bootstrap(parent_sentinel)


def _check_not_importing_main():
    if getattr(process.current_process(), '_inheriting', False):
        raise RuntimeError(
            "A new process was started while the current process was still "
            "bootstrapping (importing its main module).\n\n"
            "This usually means the main module starts processes at import "
            "time.  Protect that code with\n\n"
            "    if __name__ == '__main__':\n"
            "        ...\n\n"
            "(and call freeze_support() first in frozen programs).")


def get_preparation_data(name):
    """State a spawned child needs to look like its parent."""
    _check_not_importing_main()
    d = dict(
        log_to_stderr=util._log_to_stderr,
        authkey=process.current_process().authkey,
        )

    if util._logger is not None:
        d['log_level'] = util._logger.getEffectiveLevel()

    sys_path = sys.path.copy()
    try:
        i = sys_path.index('')
    except ValueError:
        pass
    else:
        sys_path[i] = process.ORIGINAL_DIR

    d.update(
        name=name,
        sys_path=sys_path,
        sys_argv=sys.argv,
        orig_dir=process.ORIGINAL_DIR,
        dir=os.getcwd(),
        start_method=get_start_method(),
        )

    # How should the child recreate the main module?
    main_module = sys.modules['__main__']
    main_mod_name = getattr(main_module.__spec__, "name", None)
    if main_mod_name is not None:
        d['init_main_from_name'] = main_mod_name
    elif sys.platform != 'win32' or (not WINEXE and not WINSERVICE):
        main_path = getattr(main_module, '__file__', None)
        if main_path is not None:
            if (not os.path.isabs(main_path) and
                        process.ORIGINAL_DIR is not None):
                main_path = os.path.join(process.ORIGINAL_DIR, main_path)
            d['init_main_from_path'] = os.path.normpath(main_path)

    return d


# Main modules replaced by prepare() (kept alive: their globals may still
# be referenced).
old_main_modules = []


def prepare(data):
    """Apply the preparation data in the child."""
    if 'name' in data:
        process.current_process().name = data['name']

    if 'authkey' in data:
        process.current_process().authkey = data['authkey']

    if 'log_to_stderr' in data and data['log_to_stderr']:
        util.log_to_stderr()

    if 'log_level' in data:
        util.get_logger().setLevel(data['log_level'])

    if 'sys_path' in data:
        sys.path = data['sys_path']

    if 'sys_argv' in data:
        sys.argv = data['sys_argv']

    if 'dir' in data:
        os.chdir(data['dir'])

    if 'orig_dir' in data:
        process.ORIGINAL_DIR = data['orig_dir']

    if 'start_method' in data:
        set_start_method(data['start_method'], force=True)

    if 'init_main_from_name' in data:
        _fixup_main_from_name(data['init_main_from_name'])
    elif 'init_main_from_path' in data:
        _fixup_main_from_path(data['init_main_from_path'])


def _install_main(namespace):
    main_module = types.ModuleType("__mp_main__")
    main_module.__dict__.update(namespace)
    sys.modules['__main__'] = sys.modules['__mp_main__'] = main_module


def _fixup_main_from_name(mod_name):
    # Re-import the parent's main module (run with -m) as __mp_main__ so
    # that objects pickled by reference to it can be found.
    current_main = sys.modules['__main__']
    if mod_name == "__main__" or mod_name.endswith(".__main__"):
        # package __main__ modules only run when executed: skip
        return
    if getattr(current_main.__spec__, "name", None) == mod_name:
        return

    old_main_modules.append(current_main)
    namespace = runpy.run_module(mod_name, run_name="__mp_main__",
                                 alter_sys=True)
    _install_main(namespace)


def _fixup_main_from_path(main_path):
    # Run the parent's main script as __mp_main__ (its "__main__" guard
    # keeps the program itself from running again).
    current_main = sys.modules['__main__']
    main_name = os.path.splitext(os.path.basename(main_path))[0]
    if main_name == 'ipython':
        return
    if getattr(current_main, '__file__', None) == main_path:
        return

    old_main_modules.append(current_main)
    namespace = runpy.run_path(main_path, run_name="__mp_main__")
    _install_main(namespace)


def import_main_path(main_path):
    """Set sys.modules['__main__'] to the module at *main_path*."""
    _fixup_main_from_path(main_path)
