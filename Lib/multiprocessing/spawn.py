#
# multiprocessing.spawn -- children that are brand-new interpreters.
#
# The parent writes two pickles into a pipe: a dict of "preparation data"
# (process name, authkey, sys.path, sys.argv, working directory, how to
# re-import the main module, ...) and the Process object itself.  The child
# is started with a command line that calls spawn_main(), which reads both,
# makes the interpreter look like the parent's and runs the process.
#

import os
import sys
import types

from . import get_start_method, set_start_method
from . import process
from .context import reduction
from . import util

__all__ = ['_main', 'freeze_support', 'set_executable', 'get_executable',
           'get_preparation_data', 'get_command_line', 'import_main_path']

# Windows frozen-executable / service flags (always false here).
WINEXE = False
WINSERVICE = False

_python_exe = sys.executable

_FORK_FLAG = '--multiprocessing-fork'


def set_executable(exe):
    """Use *exe* (str, bytes or path-like) as the interpreter of spawned
    children."""
    global _python_exe
    _python_exe = None if exe is None else os.fsdecode(exe)


def get_executable():
    return _python_exe


def is_forking(argv):
    """Whether *argv* is the command line of a spawned child."""
    return len(argv) >= 2 and argv[1] == _FORK_FLAG


def freeze_support():
    """When a frozen program is started as a spawned child, run the child
    and exit; return normally otherwise."""
    if not is_forking(sys.argv):
        return
    options = {}
    for arg in sys.argv[2:]:
        key, value = arg.split('=')
        options[key] = None if value == 'None' else int(value)
    spawn_main(**options)
    sys.exit()


def get_command_line(**kwds):
    """The argv that starts a child running spawn_main(**kwds)."""
    settings = ['%s=%r' % pair for pair in kwds.items()]
    if getattr(sys, 'frozen', False):
        return [sys.executable, _FORK_FLAG] + settings
    code = ('from multiprocessing.spawn import spawn_main; spawn_main(%s)'
            % ', '.join(settings))
    return ([get_executable()] + util._args_from_interpreter_flags()
            + ['-c', code, _FORK_FLAG])


def spawn_main(pipe_handle, parent_pid=None, tracker_fd=None):
    """What a spawned child runs first."""
    assert is_forking(sys.argv), "Not forking"
    if tracker_fd is not None:
        from . import resource_tracker
        resource_tracker._resource_tracker._fd = tracker_fd
    # The pipe's read end also tells the child when the parent has gone
    # (EOF), so keep a second descriptor for that.
    sentinel = os.dup(pipe_handle)
    sys.exit(_main(pipe_handle, sentinel))


def _main(fd, parent_sentinel):
    me = process.current_process()
    with os.fdopen(fd, 'rb', closefd=True) as source:
        # While the main module is re-imported, starting processes is an
        # error (see _check_not_importing_main()).
        me._inheriting = True
        try:
            prepare(reduction.pickle.load(source))
            child = reduction.pickle.load(source)
        finally:
            del me._inheriting
    return child._bootstrap(parent_sentinel)


def _check_not_importing_main():
    if getattr(process.current_process(), '_inheriting', False):
        raise RuntimeError(
            "A child process tried to start another process while it was "
            "still re-importing its parent's main module.\n\n"
            "With the spawn and forkserver start methods every child imports "
            "the main module again, so code that starts processes must not "
            "run at import time.  Put it under a guard:\n\n"
            "    if __name__ == '__main__':\n"
            "        multiprocessing.freeze_support()   # frozen programs only\n"
            "        ...\n")


def _path_for_child(path_list):
    # '' in sys.path means "the current directory", which the child may not
    # share: replace it by the directory we started in.
    return [process.ORIGINAL_DIR if entry == '' else entry
            for entry in path_list]


def _how_to_load_main():
    # -> ('init_main_from_name', module name) for "python -m ...",
    #    ('init_main_from_path', path) for a script, or None.
    main = sys.modules['__main__']
    spec_name = getattr(main.__spec__, 'name', None)
    if spec_name is not None:
        return 'init_main_from_name', spec_name
    if sys.platform == 'win32' and (WINEXE or WINSERVICE):
        return None
    path = getattr(main, '__file__', None)
    if path is None:
        return None
    if not os.path.isabs(path) and process.ORIGINAL_DIR is not None:
        path = os.path.join(process.ORIGINAL_DIR, path)
    return 'init_main_from_path', os.path.normpath(path)


def get_preparation_data(name):
    """Collect what a spawned child needs in order to resemble us."""
    _check_not_importing_main()
    data = {
        'log_to_stderr': util._log_to_stderr,
        'authkey': process.current_process().authkey,
        'name': name,
        'sys_path': _path_for_child(sys.path),
        'sys_argv': sys.argv,
        'orig_dir': process.ORIGINAL_DIR,
        'dir': os.getcwd(),
        'start_method': get_start_method(allow_none=True),
    }
    if util._logger is not None:
        data['log_level'] = util._logger.getEffectiveLevel()
    main = _how_to_load_main()
    if main is not None:
        data[main[0]] = main[1]
    return data


# --- applying preparation data in the child ---------------------------------

# Main modules replaced by prepare(); kept so that their globals stay alive
# for code that still refers to them.
old_main_modules = []


def _set_name(value):
    process.current_process().name = value


def _set_authkey(value):
    process.current_process().authkey = value


def _set_log_to_stderr(value):
    if value:
        util.log_to_stderr()


def _set_log_level(value):
    util.get_logger().setLevel(value)


def _set_sys_path(value):
    sys.path = value


def _set_sys_argv(value):
    sys.argv = value


def _set_orig_dir(value):
    process.ORIGINAL_DIR = value


def _set_start_method(value):
    set_start_method(value, force=True)


# Applied in this order; the main module is handled last, once sys.path and
# the working directory are right.
_PREPARE_STEPS = (
    ('name', _set_name),
    ('authkey', _set_authkey),
    ('log_to_stderr', _set_log_to_stderr),
    ('log_level', _set_log_level),
    ('sys_path', _set_sys_path),
    ('sys_argv', _set_sys_argv),
    ('dir', os.chdir),
    ('orig_dir', _set_orig_dir),
    ('start_method', _set_start_method),
)


def prepare(data):
    """Make this (child) interpreter match the parent's preparation data."""
    for key, apply in _PREPARE_STEPS:
        if key in data:
            apply(data[key])
    if 'init_main_from_name' in data:
        _fixup_main_from_name(data['init_main_from_name'])
    elif 'init_main_from_path' in data:
        _fixup_main_from_path(data['init_main_from_path'])


def _install_as_main(namespace):
    # Objects pickled by the parent refer to "__main__"; make both that and
    # "__mp_main__" resolve to the re-imported module.
    module = types.ModuleType("__mp_main__")
    module.__dict__.update(namespace)
    sys.modules['__main__'] = sys.modules['__mp_main__'] = module


def _fixup_main_from_name(mod_name):
    # The parent was started with "-m mod_name".  A package's __main__ only
    # makes sense when run as a program, so it is not re-imported.
    if mod_name == '__main__' or mod_name.endswith('.__main__'):
        return
    current = sys.modules['__main__']
    if getattr(current.__spec__, 'name', None) == mod_name:
        return
    import runpy
    old_main_modules.append(current)
    _install_as_main(runpy.run_module(mod_name, run_name='__mp_main__',
                                      alter_sys=True))


def _fixup_main_from_path(main_path):
    # The parent ran a script.  Running it again as "__mp_main__" defines its
    # functions and classes while the "if __name__ == '__main__'" block
    # stays inactive.
    current = sys.modules['__main__']
    stem = os.path.splitext(os.path.basename(main_path))[0]
    if stem == 'ipython':
        return
    if getattr(current, '__file__', None) == main_path:
        return
    import runpy
    old_main_modules.append(current)
    _install_as_main(runpy.run_path(main_path, run_name='__mp_main__'))


def import_main_path(main_path):
    """Make the module at *main_path* this process's __main__."""
    _fixup_main_from_path(main_path)
