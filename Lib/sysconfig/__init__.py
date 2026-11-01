"""Access to Python's configuration information: installation scheme paths
and build-time configuration variables."""

import os
import sys
import threading
from os.path import realpath

__all__ = [
    'get_config_h_filename',
    'get_config_var',
    'get_config_vars',
    'get_makefile_filename',
    'get_path',
    'get_path_names',
    'get_paths',
    'get_platform',
    'get_python_version',
    'get_scheme_names',
    'parse_config_h',
]

_ALWAYS_STR = {
    'IPHONEOS_DEPLOYMENT_TARGET',
    'MACOSX_DEPLOYMENT_TARGET',
}

_INSTALL_SCHEMES = {
    'posix_prefix': {
        'stdlib': '{installed_base}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
        'platstdlib': '{platbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
        'purelib': '{base}/lib/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
        'platlib': '{platbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
        'include':
            '{installed_base}/include/{implementation_lower}{py_version_short}{abiflags}',
        'platinclude':
            '{installed_platbase}/include/{implementation_lower}{py_version_short}{abiflags}',
        'scripts': '{base}/bin',
        'data': '{base}',
        },
    'posix_home': {
        'stdlib': '{installed_base}/lib/{implementation_lower}',
        'platstdlib': '{base}/lib/{implementation_lower}',
        'purelib': '{base}/lib/{implementation_lower}',
        'platlib': '{base}/lib/{implementation_lower}',
        'include': '{installed_base}/include/{implementation_lower}',
        'platinclude': '{installed_base}/include/{implementation_lower}',
        'scripts': '{base}/bin',
        'data': '{base}',
        },
    'nt': {
        'stdlib': '{installed_base}/Lib',
        'platstdlib': '{base}/Lib',
        'purelib': '{base}/Lib/site-packages',
        'platlib': '{base}/Lib/site-packages',
        'include': '{installed_base}/Include',
        'platinclude': '{installed_base}/Include',
        'scripts': '{base}/Scripts',
        'data': '{base}',
        },
    'posix_venv': {
        'stdlib': '{installed_base}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
        'platstdlib': '{platbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
        'purelib': '{base}/lib/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
        'platlib': '{platbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
        'include':
            '{installed_base}/include/{implementation_lower}{py_version_short}{abiflags}',
        'platinclude':
            '{installed_platbase}/include/{implementation_lower}{py_version_short}{abiflags}',
        'scripts': '{base}/bin',
        'data': '{base}',
        },
    'nt_venv': {
        'stdlib': '{installed_base}/Lib',
        'platstdlib': '{base}/Lib',
        'purelib': '{base}/Lib/site-packages',
        'platlib': '{base}/Lib/site-packages',
        'include': '{installed_base}/Include',
        'platinclude': '{installed_base}/Include',
        'scripts': '{base}/Scripts',
        'data': '{base}',
        },
    }

_INSTALL_SCHEMES['venv'] = _INSTALL_SCHEMES['posix_venv']


def _getuserbase():
    env_base = os.environ.get("PYTHONUSERBASE", None)
    if env_base:
        return env_base

    if sys.platform in {"emscripten", "wasi"}:
        return None

    def joinuser(*args):
        return os.path.expanduser(os.path.join(*args))

    if os.name == "nt":
        base = os.environ.get("APPDATA") or "~"
        return joinuser(base, _get_implementation())

    if sys.platform == "darwin" and sys._framework:
        return joinuser("~", "Library", sys._framework,
                        f"{sys.version_info[0]}.{sys.version_info[1]}")

    return joinuser("~", ".local")

_HAS_USER_BASE = (_getuserbase() is not None)

if _HAS_USER_BASE:
    _INSTALL_SCHEMES |= {
        'nt_user': {
            'stdlib': '{userbase}/{implementation}{py_version_nodot_plat}',
            'platstdlib': '{userbase}/{implementation}{py_version_nodot_plat}',
            'purelib': '{userbase}/{implementation}{py_version_nodot_plat}/site-packages',
            'platlib': '{userbase}/{implementation}{py_version_nodot_plat}/site-packages',
            'include': '{userbase}/{implementation}{py_version_nodot_plat}/Include',
            'scripts': '{userbase}/{implementation}{py_version_nodot_plat}/Scripts',
            'data': '{userbase}',
            },
        'posix_user': {
            'stdlib': '{userbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
            'platstdlib': '{userbase}/{platlibdir}/{implementation_lower}{py_version_short}{abi_thread}',
            'purelib': '{userbase}/lib/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
            'platlib': '{userbase}/lib/{implementation_lower}{py_version_short}{abi_thread}/site-packages',
            'include': '{userbase}/include/{implementation_lower}{py_version_short}{abi_thread}',
            'scripts': '{userbase}/bin',
            'data': '{userbase}',
            },
        'osx_framework_user': {
            'stdlib': '{userbase}/lib/{implementation_lower}',
            'platstdlib': '{userbase}/lib/{implementation_lower}',
            'purelib': '{userbase}/lib/{implementation_lower}/site-packages',
            'platlib': '{userbase}/lib/{implementation_lower}/site-packages',
            'include': '{userbase}/include/{implementation_lower}{py_version_short}',
            'scripts': '{userbase}/bin',
            'data': '{userbase}',
            },
    }

_SCHEME_KEYS = ('stdlib', 'platstdlib', 'purelib', 'platlib', 'include',
                'scripts', 'data')

_PY_VERSION = sys.version.split()[0]
_PY_VERSION_SHORT = f'{sys.version_info[0]}.{sys.version_info[1]}'
_PY_VERSION_SHORT_NO_DOT = f'{sys.version_info[0]}{sys.version_info[1]}'
_BASE_PREFIX = os.path.normpath(sys.base_prefix)
_BASE_EXEC_PREFIX = os.path.normpath(sys.base_exec_prefix)
_CONFIG_VARS = None
_CONFIG_VARS_LOCK = threading.RLock()
_CONFIG_VARS_INITIALIZED = False
_USER_BASE = None


def _safe_realpath(path):
    try:
        return realpath(path)
    except OSError:
        return path

if sys.executable:
    _PROJECT_BASE = os.path.dirname(_safe_realpath(sys.executable))
else:
    _PROJECT_BASE = _safe_realpath(os.getcwd())

_sys_home = getattr(sys, '_home', None)


def is_python_build():
    """True when running from a build tree rather than an installation.
    This interpreter is never considered a CPython source build."""
    return False

_PYTHON_BUILD = is_python_build()


def _subst_vars(s, local_vars):
    try:
        return s.format(**local_vars)
    except KeyError as var:
        try:
            return s.format(**os.environ)
        except KeyError:
            raise AttributeError(f'{var}') from None

def _extend_dict(target_dict, other_dict):
    target_keys = target_dict.keys()
    for key, value in other_dict.items():
        if key in target_keys:
            continue
        target_dict[key] = value


def _expand_vars(scheme, vars):
    res = {}
    if vars is None:
        vars = {}
    _extend_dict(vars, get_config_vars())
    if os.name == 'nt':
        vars.setdefault('py_version_nodot_plat', '')

    for key, value in _INSTALL_SCHEMES[scheme].items():
        if os.name in ('posix', 'nt'):
            value = os.path.expanduser(value)
        res[key] = os.path.normpath(_subst_vars(value, vars))
    return res


def _get_preferred_schemes():
    if os.name == 'nt':
        return {
            'prefix': 'nt',
            'home': 'posix_home',
            'user': 'nt_user',
        }
    if sys.platform == 'darwin' and sys._framework:
        return {
            'prefix': 'posix_prefix',
            'home': 'posix_home',
            'user': 'osx_framework_user',
        }

    return {
        'prefix': 'posix_prefix',
        'home': 'posix_home',
        'user': 'posix_user',
    }


def get_preferred_scheme(key):
    """Return the preferred scheme name for 'prefix', 'home' or 'user'."""
    if key == 'prefix' and sys.prefix != sys.base_prefix:
        return 'venv'
    scheme = _get_preferred_schemes()[key]
    if scheme not in _INSTALL_SCHEMES:
        raise ValueError(
            f"{key!r} returned {scheme!r}, which is not a valid scheme "
            f"on this platform"
        )
    return scheme


def get_default_scheme():
    """Return the default installation scheme name."""
    return get_preferred_scheme('prefix')


def get_makefile_filename():
    """Return the path of the Makefile."""
    if _PYTHON_BUILD:
        return os.path.join(_PROJECT_BASE, "Makefile")
    if hasattr(sys, 'abiflags'):
        config_dir_name = f'config-{_PY_VERSION_SHORT}{sys.abiflags}'
    else:
        config_dir_name = 'config'
    if hasattr(sys.implementation, '_multiarch'):
        config_dir_name += f'-{sys.implementation._multiarch}'
    makefile = os.path.join(get_path('stdlib'), config_dir_name, 'Makefile')
    stdlib_dir = getattr(sys, '_stdlib_dir', None)
    if not os.path.isfile(makefile) and stdlib_dir:
        # Development layout: the standard library is not below the prefix.
        candidate = os.path.join(stdlib_dir, config_dir_name, 'Makefile')
        if os.path.isfile(candidate):
            return candidate
    return makefile


def _get_sysconfigdata_name():
    multiarch = getattr(sys.implementation, '_multiarch', '')
    return os.environ.get(
        '_PYTHON_SYSCONFIGDATA_NAME',
        f'_sysconfigdata_{sys.abiflags}_{sys.platform}_{multiarch}',
    )


def _get_sysconfigdata():
    name = _get_sysconfigdata_name()
    try:
        _temp = __import__(name, globals(), locals(), ['build_time_vars'], 0)
    except ImportError:
        return {}
    return _temp.build_time_vars


def _installation_is_relocated():
    """True if the installation was moved since it was built."""
    build_prefix = _get_sysconfigdata().get('prefix')
    return build_prefix is not None and build_prefix != sys.prefix


def _init_posix(vars):
    """Initialize the module as appropriate for POSIX systems."""
    build_time_vars = _get_sysconfigdata()
    vars.update(build_time_vars)


def _init_non_posix(vars):
    """Initialize the module as appropriate for NT"""
    vars['LIBDEST'] = get_path('stdlib')
    vars['BINLIBDEST'] = get_path('platstdlib')
    vars['INCLUDEPY'] = get_path('include')
    vars['EXT_SUFFIX'] = '.pyd'
    vars['EXE'] = '.exe'
    vars['VERSION'] = _PY_VERSION_SHORT_NO_DOT
    vars['BINDIR'] = os.path.dirname(_safe_realpath(sys.executable))


def parse_config_h(fp, vars=None):
    """Parse a config.h-style file into a dict ("#define NAME VALUE" and
    "/* #undef NAME */" lines)."""
    if vars is None:
        vars = {}
    import re
    # Macro names: an upper-case ASCII letter followed by at least one more
    # name character, or the _Py_ prefix.
    name = r'(_Py_[A-Za-z0-9_]+|[A-Z][A-Za-z0-9_]+)'
    define_rx = re.compile(r'#define ' + name + r'[ \t]+(\S.*?)\s*\Z', re.ASCII)
    undef_rx = re.compile(r'/[*] #undef ' + name + r' [*]/\s*\Z', re.ASCII)

    for line in fp:
        m = define_rx.match(line)
        if m:
            n, v = m.group(1, 2)
            if len(v) >= 2 and v[0] == v[-1] == '"':
                v = v[1:-1]
            elif n not in _ALWAYS_STR:
                try:
                    v = int(v)
                except ValueError:
                    pass
            vars[n] = v
            continue
        m = undef_rx.match(line)
        if m:
            vars[m.group(1)] = 0
    return vars


def get_config_h_filename():
    """Return the path of pyconfig.h."""
    if _PYTHON_BUILD:
        inc_dir = os.path.join(_PROJECT_BASE, 'Include')
    else:
        inc_dir = get_path('platinclude')
    return os.path.join(inc_dir, 'pyconfig.h')


def get_scheme_names():
    """Return a tuple containing the schemes names."""
    return tuple(sorted(_INSTALL_SCHEMES))


def get_path_names():
    """Return a tuple containing the paths names."""
    return _SCHEME_KEYS


def get_paths(scheme=get_default_scheme(), vars=None, expand=True):
    """Return a mapping of path names to installation paths for *scheme*
    (expanded with *vars* and the configuration variables)."""
    if expand:
        return _expand_vars(scheme, vars)
    else:
        return _INSTALL_SCHEMES[scheme]


def get_path(name, scheme=get_default_scheme(), vars=None, expand=True):
    """Return the installation path *name* of *scheme*."""
    return get_paths(scheme, vars, expand)[name]


def _get_implementation():
    return 'Python'


def _init_config_vars():
    global _CONFIG_VARS
    _CONFIG_VARS = {}

    prefix = os.path.normpath(sys.prefix)
    exec_prefix = os.path.normpath(sys.exec_prefix)
    base_prefix = _BASE_PREFIX
    base_exec_prefix = _BASE_EXEC_PREFIX

    try:
        abiflags = sys.abiflags
    except AttributeError:
        abiflags = ''

    if os.name == 'posix':
        _init_posix(_CONFIG_VARS)
        _CONFIG_VARS['TZPATH'] = _CONFIG_VARS.get('TZPATH', '')
    if os.name == 'nt':
        _init_non_posix(_CONFIG_VARS)
        _CONFIG_VARS['VPATH'] = sys._vpath
    _CONFIG_VARS['prefix'] = prefix
    _CONFIG_VARS['exec_prefix'] = exec_prefix
    _CONFIG_VARS['py_version'] = _PY_VERSION
    _CONFIG_VARS['py_version_short'] = _PY_VERSION_SHORT
    _CONFIG_VARS['py_version_nodot'] = _PY_VERSION_SHORT_NO_DOT
    _CONFIG_VARS['installed_base'] = base_prefix
    _CONFIG_VARS['base'] = prefix
    _CONFIG_VARS['installed_platbase'] = base_exec_prefix
    _CONFIG_VARS['platbase'] = exec_prefix
    _CONFIG_VARS['projectbase'] = _PROJECT_BASE
    _CONFIG_VARS['platlibdir'] = sys.platlibdir
    _CONFIG_VARS['implementation'] = _get_implementation()
    _CONFIG_VARS['implementation_lower'] = _get_implementation().lower()
    _CONFIG_VARS['abiflags'] = abiflags
    try:
        _CONFIG_VARS['py_version_nodot_plat'] = sys.winver.replace('.', '')
    except AttributeError:
        _CONFIG_VARS['py_version_nodot_plat'] = ''

    # BINDIR, LIBDEST, INCLUDEPY, LIBDIR, ... keep their build-time values
    # (from _sysconfigdata), as in CPython; installation paths that follow
    # the actual location come from the install schemes (get_path()).
    if os.name != 'posix':
        _CONFIG_VARS['BINDIR'] = os.path.dirname(_safe_realpath(sys.executable)) if sys.executable else ''

    if os.name == 'nt':
        _CONFIG_VARS['VERSION'] = _PY_VERSION_SHORT_NO_DOT
    _CONFIG_VARS['VERSION'] = _CONFIG_VARS.get('VERSION', _PY_VERSION_SHORT)
    _CONFIG_VARS['abi_thread'] = 't' if _CONFIG_VARS.get('Py_GIL_DISABLED') else ''

    if _HAS_USER_BASE:
        _CONFIG_VARS['userbase'] = _getuserbase()

    srcdir = _CONFIG_VARS.get('srcdir', _PROJECT_BASE)
    if os.name == 'posix':
        if _PYTHON_BUILD:
            base = os.path.dirname(get_makefile_filename())
            srcdir = os.path.join(base, srcdir)
        else:
            srcdir = os.path.dirname(get_makefile_filename())
    _CONFIG_VARS['srcdir'] = _safe_realpath(srcdir)

    if sys.platform == 'darwin':
        import _osx_support
        _osx_support.customize_config_vars(_CONFIG_VARS)

    global _CONFIG_VARS_INITIALIZED
    _CONFIG_VARS_INITIALIZED = True


def get_config_vars(*args):
    """With no arguments, return a dict of all configuration variables;
    with arguments, return a list of their values (None for unknown
    names)."""
    if _CONFIG_VARS_INITIALIZED:
        if _CONFIG_VARS['prefix'] != os.path.normpath(sys.prefix) or _CONFIG_VARS['exec_prefix'] != os.path.normpath(sys.exec_prefix):
            with _CONFIG_VARS_LOCK:
                _init_config_vars()

    if _CONFIG_VARS is None:
        with _CONFIG_VARS_LOCK:
            if _CONFIG_VARS is None:
                _init_config_vars()

    if args:
        vals = []
        for name in args:
            vals.append(_CONFIG_VARS.get(name))
        return vals
    else:
        return _CONFIG_VARS


def get_config_var(name):
    """Return the value of a single configuration variable (None if
    unknown)."""
    return get_config_vars().get(name)


def get_platform():
    """Return a string identifying the platform the interpreter was built
    for, e.g. 'linux-x86_64'."""
    if os.name == 'nt':
        import _sysconfig
        platform = getattr(_sysconfig, 'get_platform', lambda: None)()
        return platform or sys.platform

    if os.name != "posix" or not hasattr(os, 'uname'):
        return sys.platform

    if "_PYTHON_HOST_PLATFORM" in os.environ:
        return os.environ["_PYTHON_HOST_PLATFORM"]

    osname, host, release, version, machine = os.uname()

    osname = osname.lower().replace('/', '')
    machine = machine.replace(' ', '_')
    machine = machine.replace('/', '-')

    if osname[:5] == "linux":
        if sys.platform == "android":
            osname = "android"
            release = get_config_var("ANDROID_API_LEVEL")
            machine = {
                "x86_64": "x86_64", "i686": "x86", "aarch64": "arm64_v8a",
                "armv7l": "armeabi_v7a", "armv8l": "armeabi_v7a", "arm": "armeabi_v7a",
            }[machine]
        else:
            return  f"{osname}-{machine}"
    elif osname[:5] == "sunos":
        if release[0] >= "5":
            osname = "solaris"
            release = f"{int(release[0]) - 3}.{release[2:]}"
            bitness = {2147483647:"32bit", 9223372036854775807:"64bit"}
            machine += f".{bitness[sys.maxsize]}"
    elif osname[:3] == "aix":
        from _aix_support import aix_platform
        return aix_platform()
    elif osname[:6] == "cygwin":
        osname = "cygwin"
        import re
        rel_re = re.compile(r'[\d.]+')
        m = rel_re.match(release)
        if m:
            release = m.group()
    elif osname[:6] == "darwin":
        if sys.platform == "ios":
            release = get_config_vars().get("IPHONEOS_DEPLOYMENT_TARGET", "13.0")
            osname = sys.platform
            machine = sys.implementation._multiarch
        else:
            import _osx_support
            osname, release, machine = _osx_support.get_platform_osx(
                                                get_config_vars(),
                                                osname, release, machine)

    return '-'.join(map(str, filter(None, (osname, release, machine))))


def get_python_version():
    return _PY_VERSION_SHORT


def _get_python_version_abi():
    return _PY_VERSION_SHORT + get_config_var("abi_thread")


def expand_makefile_vars(s, vars):
    """Expand Makefile-style variables ("${foo}" / "$(foo)") in *s* using
    *vars* (unknown names become empty strings)."""
    import re
    import warnings
    warnings.warn(
        'sysconfig.expand_makefile_vars is deprecated and will be removed in '
        'Python 3.16. Use sysconfig.get_paths(vars=...) instead.',
        DeprecationWarning,
        stacklevel=2,
    )

    _findvar1_rx = r"\$\(([A-Za-z][A-Za-z0-9_]*)\)"
    _findvar2_rx = r"\${([A-Za-z][A-Za-z0-9_]*)}"

    while True:
        m = re.search(_findvar1_rx, s) or re.search(_findvar2_rx, s)
        if m:
            (beg, end) = m.span()
            s = s[0:beg] + vars.get(m.group(1)) + s[end:]
        else:
            break
    return s


def _print_dict(title, data):
    for index, (key, value) in enumerate(sorted(data.items())):
        if index == 0:
            print(f'{title}: ')
        print(f'\t{key} = "{value}"')


def _main():
    """Display all information sysconfig contains."""
    print(f'Platform: "{get_platform()}"')
    print(f'Python version: "{get_python_version()}"')
    print(f'Current installation scheme: "{get_default_scheme()}"')
    print()
    _print_dict('Paths', get_paths())
    print()
    _print_dict('Variables', get_config_vars())
