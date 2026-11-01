"""Helpers adjusting build configuration variables on macOS.

sysconfig and build tools call these to adapt compiler flags to the SDK
and architectures available on the build machine.  On other platforms the
module can be imported but its functions are not used.
"""

import os
import re
import sys

__all__ = [
    'compiler_fixup',
    'customize_config_vars',
    'customize_compiler',
    'get_platform_osx',
]

# Variables holding compiler/linker flags that may need adjusting.
_UNIVERSAL_CONFIG_VARS = ('CFLAGS', 'LDFLAGS', 'CPPFLAGS', 'BASECFLAGS',
                          'BLDSHARED', 'LDSHARED', 'CC', 'CXX',
                          'PY_CFLAGS', 'PY_LDFLAGS', 'PY_CPPFLAGS',
                          'PY_CORE_CFLAGS', 'PY_CORE_LDFLAGS')

# Variables naming the compiler executables.
_COMPILER_CONFIG_VARS = ('BLDSHARED', 'LDSHARED', 'CC', 'CXX')

# Prefix marking a saved original value in the config vars dict.
_INITPRE = '_OSX_SUPPORT_INITIAL_'


def _find_executable(executable, path=None):
    """Return the full path of *executable* on *path* (default: $PATH)."""
    if path is None:
        path = os.environ.get('PATH', os.defpath)
    base, ext = os.path.splitext(executable)
    if sys.platform == 'win32' and ext != '.exe':
        executable += '.exe'
    if os.path.isfile(executable):
        return executable
    for directory in path.split(os.pathsep):
        candidate = os.path.join(directory, executable)
        if os.path.isfile(candidate):
            return candidate
    return None


def _read_output(commandstring, capture_stderr=False):
    """Run *commandstring* in a shell and return its stripped output, or
    None if it failed."""
    import subprocess
    try:
        proc = subprocess.run(
            commandstring, shell=True, capture_output=True, text=True,
        )
    except OSError:
        return None
    if proc.returncode != 0:
        return None
    output = proc.stdout + (proc.stderr if capture_stderr else '')
    return output.strip()


def _find_build_tool(toolname):
    """Locate a developer tool, preferring xcrun on macOS."""
    return (_find_executable(toolname)
            or _read_output(f"/usr/bin/xcrun -find {toolname}")
            or '')


_SYSTEM_VERSION = None


def _get_system_version():
    """Return the macOS version as a string (e.g. '14.2'), or ''."""
    global _SYSTEM_VERSION
    if _SYSTEM_VERSION is None:
        _SYSTEM_VERSION = ''
        try:
            with open('/System/Library/CoreServices/SystemVersion.plist',
                      encoding='utf-8') as f:
                text = f.read()
        except OSError:
            pass
        else:
            match = re.search(r'<key>ProductUserVisibleVersion</key>\s*'
                              r'<string>(.*?)</string>', text)
            if match is not None:
                _SYSTEM_VERSION = '.'.join(match.group(1).split('.')[:2])
    return _SYSTEM_VERSION


_SYSTEM_VERSION_TUPLE = None


def _get_system_version_tuple():
    """Return the macOS version as a tuple of ints, or () if unknown."""
    global _SYSTEM_VERSION_TUPLE
    if _SYSTEM_VERSION_TUPLE is None:
        version = _get_system_version()
        try:
            _SYSTEM_VERSION_TUPLE = tuple(int(part) for part in version.split('.'))
        except ValueError:
            _SYSTEM_VERSION_TUPLE = ()
    return _SYSTEM_VERSION_TUPLE


def _remove_original_values(_config_vars):
    """Forget the saved original values of the config vars."""
    for key in list(_config_vars):
        if key.startswith(_INITPRE):
            del _config_vars[key]


def _save_modified_value(_config_vars, cv, newvalue):
    """Set *cv* to *newvalue*, remembering the first original value."""
    oldvalue = _config_vars.get(cv, '')
    if oldvalue != newvalue and (_INITPRE + cv) not in _config_vars:
        _config_vars[_INITPRE + cv] = oldvalue
    _config_vars[cv] = newvalue


_cache_default_sysroot = None


def _default_sysroot(cc):
    """Return the SDK root the compiler *cc* uses by default ('/' if none)."""
    global _cache_default_sysroot
    if _cache_default_sysroot is not None:
        return _cache_default_sysroot
    contents = _read_output(f'{cc} -c -E -v - </dev/null', True) or ''
    in_includes = False
    sysroot = '/'
    for line in contents.splitlines():
        if line.startswith('#include <...>'):
            in_includes = True
        elif line.startswith('End of search list'):
            in_includes = False
        elif in_includes:
            line = line.strip()
            if line.endswith('.sdk/usr/include'):
                sysroot = line[:-len('/usr/include')]
                break
    _cache_default_sysroot = sysroot
    return sysroot


def _supports_universal_builds():
    """True if this macOS release can build universal binaries."""
    version = _get_system_version_tuple()
    return bool(version) and version >= (10, 4)


def _supports_arm64_builds():
    """True if this macOS release can build arm64 binaries."""
    version = _get_system_version_tuple()
    return bool(version) and version >= (11, 0)


def _find_appropriate_compiler(_config_vars):
    """Replace a missing or gcc-compatibility compiler with clang."""
    if 'CC' in os.environ:
        return _config_vars
    cc = oldcc = _config_vars['CC'].split()[0]
    if not _find_executable(cc):
        cc = _find_build_tool('clang')
    elif os.path.basename(cc).startswith('gcc'):
        import shlex
        data = _read_output(f"{shlex.quote(cc)} --version")
        if data and 'llvm-gcc' in data:
            cc = _find_build_tool('clang')
    if not cc:
        raise SystemError("Cannot locate working compiler")
    if cc != oldcc:
        for cv in _COMPILER_CONFIG_VARS:
            if cv in _config_vars and cv not in os.environ:
                parts = _config_vars[cv].split()
                parts[0] = cc if cv != 'CXX' else cc + '++'
                _save_modified_value(_config_vars, cv, ' '.join(parts))
    return _config_vars


def _remove_universal_flags(_config_vars):
    """Drop -arch and -isysroot options from the flag variables."""
    for cv in _UNIVERSAL_CONFIG_VARS:
        if cv in _config_vars and cv not in os.environ:
            flags = _config_vars[cv]
            flags = re.sub(r'-arch\s+\w+\s', ' ', flags, flags=re.ASCII)
            flags = re.sub(r'-isysroot\s*\S+', ' ', flags)
            _save_modified_value(_config_vars, cv, flags)
    return _config_vars


def _remove_unsupported_archs(_config_vars):
    """Drop '-arch ppc' if the compiler cannot build for it."""
    if 'CC' in os.environ:
        return _config_vars
    if re.search(r'-arch\s+ppc', _config_vars.get('CFLAGS', '')) is not None:
        import subprocess
        try:
            status = subprocess.run(
                [*_config_vars['CC'].split(), '-c', '-arch', 'ppc', '-x', 'c',
                 '-o', '/dev/null', '-'],
                input=b'int main{};', capture_output=True,
            ).returncode
        except OSError:
            status = 1
        if status:
            for cv in _UNIVERSAL_CONFIG_VARS:
                if cv in _config_vars and cv not in os.environ:
                    flags = re.sub(r'-arch\s+ppc\w*\s', ' ', _config_vars[cv])
                    _save_modified_value(_config_vars, cv, flags)
    return _config_vars


def _override_all_archs(_config_vars):
    """Apply ARCHFLAGS from the environment to all flag variables."""
    if 'ARCHFLAGS' in os.environ:
        arch = os.environ['ARCHFLAGS']
        for cv in _UNIVERSAL_CONFIG_VARS:
            if cv in _config_vars and '-arch' in _config_vars[cv]:
                flags = re.sub(r'-arch\s+\w+\s', ' ', _config_vars[cv])
                _save_modified_value(_config_vars, cv, flags + ' ' + arch)
    return _config_vars


def _check_for_unavailable_sdk(_config_vars):
    """Remove -isysroot options naming an SDK that is not installed."""
    cflags = _config_vars.get('CFLAGS', '')
    match = re.search(r'-isysroot\s*(\S+)', cflags)
    if match is not None and not os.path.exists(match.group(1)):
        for cv in _UNIVERSAL_CONFIG_VARS:
            if cv in _config_vars and cv not in os.environ:
                flags = re.sub(r'-isysroot\s*\S+(?:\s|$)', ' ', _config_vars[cv])
                _save_modified_value(_config_vars, cv, flags)
    return _config_vars


def compiler_fixup(compiler_so, cc_args):
    """Adjust a compiler command list for the architectures and SDK the
    build machine supports; return the new list."""
    strip_arch = strip_sysroot = False
    compiler_so = list(compiler_so)
    if not _supports_universal_builds():
        strip_arch = strip_sysroot = True
    else:
        strip_arch = '-arch' in cc_args
        strip_sysroot = any(arg.startswith('-isysroot') for arg in cc_args)
    if strip_arch or 'ARCHFLAGS' in os.environ:
        while '-arch' in compiler_so:
            index = compiler_so.index('-arch')
            del compiler_so[index:index + 2]
    if 'ARCHFLAGS' in os.environ and not strip_arch:
        compiler_so = compiler_so + os.environ['ARCHFLAGS'].split()
    if strip_sysroot:
        while True:
            indices = [i for i, arg in enumerate(compiler_so) if arg.startswith('-isysroot')]
            if not indices:
                break
            index = indices[0]
            if compiler_so[index] == '-isysroot':
                del compiler_so[index:index + 2]
            else:
                del compiler_so[index:index + 1]
    return compiler_so


def customize_config_vars(_config_vars):
    """Adapt the config vars to the running system (called by sysconfig
    on macOS)."""
    if not _supports_universal_builds():
        _remove_universal_flags(_config_vars)
    _override_all_archs(_config_vars)
    _check_for_unavailable_sdk(_config_vars)
    return _config_vars


def customize_compiler(_config_vars):
    """Adapt the config vars before compiling extensions."""
    _find_appropriate_compiler(_config_vars)
    _remove_unsupported_archs(_config_vars)
    _override_all_archs(_config_vars)
    return _config_vars


def get_platform_osx(_config_vars, osname, release, machine):
    """Return (osname, release, machine) for sysconfig.get_platform()."""
    macver = _config_vars.get('MACOSX_DEPLOYMENT_TARGET', '')
    if macver and '.' not in str(macver):
        macver = f'{macver}.0'
    macrelease = _get_system_version() or macver
    macver = macver or macrelease
    if macver:
        release = str(macver)
        osname = 'macosx'
        cflags = _config_vars.get(_INITPRE + 'CFLAGS', _config_vars.get('CFLAGS', ''))
        if macrelease:
            try:
                macrelease = tuple(int(i) for i in macrelease.split('.')[:2])
            except ValueError:
                macrelease = (10, 3)
        else:
            macrelease = (10, 3)
        if macrelease >= (10, 4) and '-arch' in cflags.strip():
            machine = 'fat'
            archs = tuple(sorted(set(re.findall(r'-arch\s+(\S+)', cflags))))
            known = {
                ('arm64', 'x86_64'): 'universal2',
                ('i386', 'ppc'): 'fat',
                ('i386', 'x86_64'): 'intel',
                ('i386', 'ppc', 'x86_64'): 'fat3',
                ('ppc64', 'x86_64'): 'fat64',
                ('i386', 'ppc', 'ppc64', 'x86_64'): 'universal',
            }
            if len(archs) == 1:
                machine = archs[0]
            elif archs in known:
                machine = known[archs]
            else:
                raise ValueError(f"Don't know machine value for archs={archs!r}")
        elif machine == 'i386':
            if sys.maxsize >= 2**32:
                machine = 'x86_64'
        elif machine in ('PowerPC', 'Power_Macintosh'):
            machine = 'ppc64' if sys.maxsize >= 2**32 else 'ppc'
    return (osname, release, machine)
