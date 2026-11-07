# Path calculation (RPython, written from the test suite's description of it).
#
# This source is not imported: it is executed with a namespace that supplies
# every input, so that the whole calculation can be driven by a mocked file
# system (test_getpath) as well as by the real one.
#
# Inputs found in the namespace
#   build-time values  PREFIX, EXEC_PREFIX, VPATH, PLATLIBDIR, PYTHONPATH,
#                      VERSION_MAJOR, VERSION_MINOR, ABI_THREAD, PYWINVER,
#                      EXE_SUFFIX, PYDEBUGEXT, WITH_NEXT_FRAMEWORK (optional)
#   process values     os_name, real_executable, executable_dir, library,
#                      py_setpath, winreg, argv0
#   environment        ENV_PATH, ENV_PYTHONHOME, ENV_PYTHONEXECUTABLE,
#                      ENV___PYVENV_LAUNCHER__
#   file system        abspath, basename, dirname, hassuffix, isabs, isdir,
#                      isfile, ismodule, isxfile, joinpath, readlines,
#                      realpath, warn
#   config             the partly filled configuration dict; whatever it
#                      already holds wins over what is computed here
#
# Outputs: config['executable'], ['base_executable'], ['prefix'],
# ['exec_prefix'], ['base_prefix'], ['base_exec_prefix'], ['stdlib_dir'],
# ['platstdlib_dir'], ['module_search_paths'], ['module_search_paths_set'],
# ['_is_python_build'], and the globals build_prefix and venv_prefix.
#
# Only builtins are used: this runs before anything can be imported.


# ---------------------------------------------------------------- platform

ON_WINDOWS = os_name == 'nt'
ON_DARWIN = os_name == 'darwin'
SEP = '\\' if ON_WINDOWS else '/'
LIST_SEP = ';' if ON_WINDOWS else ':'
EXE_EXT = EXE_SUFFIX if EXE_SUFFIX is not None else ('.exe' if ON_WINDOWS else '')
FRAMEWORK = bool(globals().get('WITH_NEXT_FRAMEWORK'))

_short_version = f'{VERSION_MAJOR}.{VERSION_MINOR}'
_abi = ABI_THREAD or ''
LIBDIR_NAME = f'python{_short_version}{_abi}'
ZIP_NAME = f'python{VERSION_MAJOR}{VERSION_MINOR}{_abi}.zip'

if ON_WINDOWS:
    STDLIB_REL = ('Lib',)
    PLATSTDLIB_REL = (PLATLIBDIR or 'DLLs',)
else:
    STDLIB_REL = (PLATLIBDIR or 'lib', LIBDIR_NAME)
    PLATSTDLIB_REL = (PLATLIBDIR or 'lib', LIBDIR_NAME, 'lib-dynload')


# ---------------------------------------------------------------- helpers

def _given(name):
    """A config value, or None when it is unset or empty."""
    value = config.get(name)
    return value if value else None


def _environ(value):
    """An environment-derived input, or None when absent or when the
    environment must be ignored (-E / -I)."""
    if not config.get('use_environment', 1):
        return None
    return value or None


def _resolved(path):
    """Follow symbolic links; a broken or circular chain keeps the path."""
    if not path:
        return path
    try:
        return realpath(path)
    except OSError:
        return path


def _split_list(text):
    return [part for part in (text or '').split(LIST_SEP) if part]


def _ancestors(start):
    """start, its parent, its grandparent, ... up to the root."""
    here = start
    while here:
        yield here
        up = dirname(here)
        if up == here:
            break
        here = up


def _holds_stdlib(directory):
    return ismodule(joinpath(directory, *STDLIB_REL, 'os.py'))


def _holds_platstdlib(directory):
    return isdir(joinpath(directory, *PLATSTDLIB_REL))


def _first_ancestor(starts, test):
    """The nearest directory, walking up from each start in turn, for which
    test() holds."""
    for start in starts:
        if not start:
            continue
        for directory in _ancestors(start):
            if test(directory):
                return directory
    return None


def _read_lines(path):
    try:
        return readlines(path)
    except OSError:
        return None


# ---------------------------------------------------------------- the program

def _program_path():
    """Where the program was started from, with links left in place."""
    exe = _given('executable')
    if exe:
        return exe
    if ON_DARWIN or ON_WINDOWS:
        # A venv launcher re-executes the base interpreter and says who it is.
        if ENV___PYVENV_LAUNCHER__:
            return ENV___PYVENV_LAUNCHER__
    if ON_DARWIN and _environ(ENV_PYTHONEXECUTABLE):
        return ENV_PYTHONEXECUTABLE
    argv = config.get('orig_argv') or ()
    name = _given('program_name') or (argv[0] if argv else '')
    if not name:
        return real_executable or ''
    if SEP in name or (ON_WINDOWS and '/' in name):
        return abspath(name)
    for directory in _split_list(ENV_PATH):
        candidate = joinpath(directory, name)
        if EXE_EXT and not hassuffix(candidate, EXE_EXT):
            candidate += EXE_EXT
        if isxfile(candidate):
            return abspath(candidate)
    return real_executable or ''


program = _program_path()
program_real = _resolved(real_executable or program)


# ---------------------------------------------------------------- virtual environments

def _venv_config(exe):
    """(directory, settings) of the pyvenv.cfg next to the program or one
    level up, or (None, {})."""
    if not exe:
        return None, {}
    bin_dir = dirname(exe)
    for directory in (bin_dir, dirname(bin_dir)):
        if not directory:
            continue
        lines = _read_lines(joinpath(directory, 'pyvenv.cfg'))
        if lines is None:
            continue
        settings = {}
        for line in lines:
            key, eq, value = line.partition('=')
            if eq:
                settings[key.strip().lower()] = value.strip()
        return directory, settings
    return None, {}


def _venv_base_program(exe, home):
    """The interpreter a venv runs on: the target of the venv's program if it
    is a link, else an interpreter found in the configured home."""
    target = _resolved(exe)
    if target != exe:
        return target
    if home:
        names = (basename(exe),
                 f'python{VERSION_MAJOR}{EXE_EXT}',
                 f'python{_short_version}{EXE_EXT}')
        for name in names:
            candidate = joinpath(home, name)
            if isxfile(candidate):
                return _resolved(candidate)
    return None


venv_prefix, _venv_settings = _venv_config(program)
venv_home = None
venv_base = None
if venv_prefix:
    venv_home = _venv_settings.get('home') or None
    if venv_home and not isabs(venv_home):
        venv_home = joinpath(venv_prefix, venv_home)
    venv_base = _venv_base_program(program, venv_home)

# The installation the paths are computed for: the base interpreter's.
install_real = venv_base or program_real


# ---------------------------------------------------------------- build tree

build_prefix = None
build_platstdlib = None
_is_build = False
if install_real:
    _exe_dir = dirname(install_real)
    _marker = _read_lines(joinpath(_exe_dir, 'pybuilddir.txt'))
    if _marker is not None:
        # Running from the build directory: the marker names the directory
        # with the compiled extension modules, relative to the executable.
        _is_build = True
        build_platstdlib = joinpath(_exe_dir, _marker[0].strip() if _marker else '')
        build_prefix = joinpath(_exe_dir, VPATH or '')


# ---------------------------------------------------------------- prefixes

def _home_prefixes():
    home = _given('home') or _environ(ENV_PYTHONHOME)
    if not home:
        return None
    if ON_WINDOWS:
        return home, home
    first, _, second = home.partition(LIST_SEP)
    return first, second or first


def _installation_prefixes():
    """(prefix, exec_prefix) of the base installation."""
    home = _home_prefixes()
    if home:
        return home
    if _is_build:
        if ON_WINDOWS:
            return build_prefix, build_prefix
        return PREFIX, EXEC_PREFIX or PREFIX

    if venv_prefix:
        starts = [dirname(venv_base) if venv_base else None, venv_home]
        if not venv_base and not venv_home:
            starts.append(dirname(program_real))
    elif FRAMEWORK and library:
        starts = [dirname(library)]
    else:
        starts = [dirname(program_real) if program_real else None]

    prefix = _first_ancestor(starts, _holds_stdlib)
    if prefix is None:
        fallback = PREFIX or (dirname(program_real) if ON_WINDOWS else '')
        warn(f'Could not find platform independent libraries <prefix>')
        prefix = fallback
    if ON_WINDOWS:
        return prefix, prefix
    exec_prefix = _first_ancestor(starts, _holds_platstdlib)
    if exec_prefix is None:
        warn(f'Could not find platform dependent libraries <exec_prefix>')
        exec_prefix = EXEC_PREFIX or prefix
    return prefix, exec_prefix


_base_prefix, _base_exec_prefix = _installation_prefixes()

if venv_prefix:
    _prefix = _exec_prefix = venv_prefix
else:
    _prefix, _exec_prefix = _base_prefix, _base_exec_prefix

prefix = _given('prefix') or _prefix
exec_prefix = _given('exec_prefix') or _exec_prefix
base_prefix = _given('base_prefix') or (_base_prefix if venv_prefix else prefix)
base_exec_prefix = _given('base_exec_prefix') or (_base_exec_prefix if venv_prefix else exec_prefix)


# ---------------------------------------------------------------- library directories

if _home_prefixes() is None and _is_build:
    stdlib_dir = joinpath(build_prefix, 'Lib')
else:
    stdlib_dir = joinpath(base_prefix, *STDLIB_REL)

if _is_build:
    platstdlib_dir = build_platstdlib
else:
    platstdlib_dir = joinpath(base_exec_prefix, *PLATSTDLIB_REL)

if ON_WINDOWS:
    _zip_dir = dirname(library) if library else dirname(install_real or program_real or '')
    zip_path = joinpath(_zip_dir, ZIP_NAME) if _zip_dir else ZIP_NAME
else:
    zip_path = joinpath(base_prefix, PLATLIBDIR or 'lib', ZIP_NAME)


def _registry_paths():
    """Directories that other software registered for this version
    (HKCU first, then HKLM; one value per subkey of PythonPath)."""
    if not (ON_WINDOWS and winreg and PYWINVER):
        return []
    key_name = f'Software\\Python\\PythonCore\\{PYWINVER}\\PythonPath'
    found = []
    for root in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        try:
            key = winreg.OpenKeyEx(root, key_name)
        except OSError:
            continue
        try:
            index = 0
            while True:
                try:
                    sub = winreg.EnumKey(key, index)
                except OSError:
                    break
                index += 1
                try:
                    value = winreg.QueryValue(key, sub)
                except OSError:
                    continue
                if value:
                    found.extend(_split_list(value))
        finally:
            winreg.CloseKey(key)
    return found


def _default_search_path():
    if py_setpath:
        return _split_list(py_setpath)
    paths = []
    paths.extend(_split_list(_given('pythonpath_env') if config.get('use_environment', 1) else None))
    paths.append(zip_path)
    if ON_WINDOWS:
        if config.get('use_environment', 1):
            paths.extend(_registry_paths())
        paths.append(platstdlib_dir if isdir(platstdlib_dir) else base_exec_prefix)
        paths.append(stdlib_dir)
        if not _is_build and base_prefix not in paths:
            paths.append(base_prefix)
    else:
        paths.append(stdlib_dir)
        paths.append(platstdlib_dir)
    return paths


# ---------------------------------------------------------------- results

if not config.get('module_search_paths_set'):
    config['module_search_paths'] = _default_search_path()
    config['module_search_paths_set'] = 1

if not _given('executable'):
    config['executable'] = program
if not _given('base_executable'):
    config['base_executable'] = (venv_base or program) if venv_prefix else program
config['prefix'] = prefix
config['exec_prefix'] = exec_prefix
config['base_prefix'] = base_prefix
config['base_exec_prefix'] = base_exec_prefix
if not _given('stdlib_dir'):
    config['stdlib_dir'] = stdlib_dir
if not _given('platstdlib_dir'):
    config['platstdlib_dir'] = platstdlib_dir
if _is_build:
    config['_is_python_build'] = 1
