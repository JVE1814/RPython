"""Information about the underlying platform: OS, hardware and the Python
interpreter (version, implementation, build)."""

__version__ = '1.1.0'

import collections
import os
import re
import sys
import functools
import itertools
try:
    import _wmi
except ImportError:
    _wmi = None


_ver_stages = {
    'dev': 10,
    'alpha': 20, 'a': 20,
    'beta': 30, 'b': 30,
    'c': 40,
    'RC': 50, 'rc': 50,
    'pl': 200, 'p': 200,
}


def _comparable_version(version):
    component_re = re.compile(r'([0-9]+|[._+-])')
    result = []
    for v in component_re.split(version):
        if v not in '._+-':
            try:
                v = int(v, 10)
                t = 100
            except ValueError:
                t = _ver_stages.get(v, 0)
            result.extend((t, v))
    return result


_libc_search = re.compile(b'(__libc_init)'
                          b'|'
                          b'(GLIBC_([0-9.]+))'
                          b'|'
                          br'(libc(_\w+)?\.so(?:\.(\d[0-9.]*))?)', re.ASCII)


def libc_ver(executable=None, lib='', version='', chunksize=16384):
    """Return (lib, version) of the C library the executable is linked
    against (best effort; empty strings when unknown)."""
    if not executable:
        try:
            ver = os.confstr('CS_GNU_LIBC_VERSION')
            parts = ver.split(maxsplit=1)
            if len(parts) == 2:
                return tuple(parts)
        except (AttributeError, ValueError, OSError):
            pass
        executable = sys.executable
        if not executable:
            return lib, version

    V = _comparable_version
    executable = os.path.realpath(executable)
    ver = None
    with open(executable, 'rb') as f:
        binary = f.read(chunksize)
        pos = 0
        while pos < len(binary):
            if b'libc' in binary or b'GLIBC' in binary:
                m = _libc_search.search(binary, pos)
            else:
                m = None
            if not m or m.end() == len(binary):
                chunk = f.read(chunksize)
                if chunk:
                    binary = binary[max(pos, len(binary) - 1000):] + chunk
                    pos = 0
                    continue
                if not m:
                    break
            libcinit, glibc, glibcversion, so, threads, soversion = [
                s.decode('latin1') if s is not None else s
                for s in m.groups()]
            if libcinit and not lib:
                lib = 'libc'
            elif glibc:
                if lib != 'glibc':
                    lib = 'glibc'
                    ver = glibcversion
                elif V(glibcversion) > V(ver):
                    ver = glibcversion
            elif so:
                if lib != 'glibc':
                    lib = 'libc'
                    if soversion and (not ver or V(soversion) > V(ver)):
                        ver = soversion
                    if threads and ver[-len(threads):] != threads:
                        ver = ver + threads
            pos = m.end()
    return lib, version if ver is None else ver


def _norm_version(version, build=''):
    l = version.split('.')
    if build:
        l.append(build)
    try:
        strings = list(map(str, map(int, l)))
    except ValueError:
        strings = l
    version = '.'.join(strings[:3])
    return version


def win32_is_iot():
    return False


def win32_edition():
    return None


def win32_ver(release='', version='', csd='', ptype=''):
    """Windows version information (empty strings on other systems)."""
    return release, version, csd, ptype


def mac_ver(release='', versioninfo=('', '', ''), machine=''):
    """macOS version information (defaults on other systems)."""
    return release, versioninfo, machine


AndroidVer = collections.namedtuple(
    "AndroidVer", "release api_level manufacturer model device is_emulator")


def android_ver(release="", api_level=0, manufacturer="", model="", device="",
                is_emulator=False):
    """Android version information (defaults on other systems)."""
    return AndroidVer(release, api_level, manufacturer, model, device, is_emulator)


IOSVersionInfo = collections.namedtuple(
    "IOSVersionInfo",
    ["system", "release", "model", "is_simulator"]
)


def ios_ver(system="", release="", model="", is_simulator=False):
    """iOS version information (defaults on other systems)."""
    return IOSVersionInfo(system, release, model, is_simulator)


def _java_getprop(name, default):
    return default


def java_ver(release='', vendor='', vminfo=('', '', ''), osinfo=('', '', '')):
    """Deprecated: Jython version information."""
    import warnings
    warnings._deprecated('java_ver', remove=(3, 15))
    return release, vendor, vminfo, osinfo


def system_alias(system, release, version):
    """Return (system, release, version) aliased to common marketing names
    where applicable."""
    if system == 'SunOS':
        if release < '5':
            return system, release, version
        l = release.split('.')
        if l:
            try:
                major = int(l[0])
            except ValueError:
                pass
            else:
                major = major - 3
                l[0] = str(major)
                release = '.'.join(l)
        if release < '6':
            system = 'Solaris'
        else:
            system = 'Solaris'
    elif system in ('win32', 'win16'):
        system = 'Windows'
    return system, release, version


def _platform(*args):
    """Join the non-empty arguments with '-' into a filename-safe string."""
    platform = '-'.join(x.strip() for x in filter(len, args))
    platform = platform.replace(' ', '_')
    platform = platform.replace('/', '-')
    platform = platform.replace('\\', '-')
    platform = platform.replace(':', '-')
    platform = platform.replace(';', '-')
    platform = platform.replace('"', '-')
    platform = platform.replace('(', '-')
    platform = platform.replace(')', '-')
    platform = platform.replace('unknown', '')
    while True:
        cleaned = platform.replace('--', '-')
        if cleaned == platform:
            break
        platform = cleaned
    while platform and platform[-1] == '-':
        platform = platform[:-1]
    return platform


def _node(default=''):
    """Return the host name."""
    try:
        import socket
    except ImportError:
        return default
    try:
        return socket.gethostname()
    except OSError:
        return default


def _follow_symlinks(filepath):
    """Resolve symlinks of *filepath* (absolute result)."""
    filepath = os.path.abspath(filepath)
    while os.path.islink(filepath):
        filepath = os.path.normpath(
            os.path.join(os.path.dirname(filepath), os.readlink(filepath)))
    return filepath


def _syscmd_file(target, default=''):
    """Output of ``file -b <target>`` (or *default* on failure)."""
    if sys.platform in {'dos', 'win32', 'win16', 'ios', 'tvos', 'watchos'}:
        return default
    try:
        import subprocess
    except ImportError:
        return default
    target = _follow_symlinks(target)
    env = dict(os.environ, LC_ALL='C')
    try:
        output = subprocess.check_output(['file', '-b', target],
                                         stderr=subprocess.DEVNULL,
                                         env=env)
    except (OSError, subprocess.CalledProcessError):
        return default
    if not output:
        return default
    return os.fsdecode(output)


_default_architecture = {
    'win32': ('', 'WindowsPE'),
    'win16': ('', 'Windows'),
    'dos': ('', 'MSDOS'),
}


def architecture(executable=sys.executable, bits='', linkage=''):
    """Return (bits, linkage) of the given executable (default: the
    interpreter), e.g. ('64bit', 'ELF')."""
    if not bits:
        import struct
        size = struct.calcsize('P')
        bits = str(size * 8) + 'bit'

    if executable:
        fileout = _syscmd_file(executable, '')
    else:
        fileout = ''

    if not fileout and executable == sys.executable:
        if sys.platform in _default_architecture:
            b, l = _default_architecture[sys.platform]
            if b:
                bits = b
            if l:
                linkage = l
        return bits, linkage

    if 'executable' not in fileout and 'shared object' not in fileout:
        return bits, linkage

    if '32-bit' in fileout:
        bits = '32bit'
    elif '64-bit' in fileout:
        bits = '64bit'

    if 'ELF' in fileout:
        linkage = 'ELF'
    elif 'Mach-O' in fileout:
        linkage = "Mach-O"
    elif 'PE' in fileout:
        if 'Windows' in fileout:
            linkage = 'WindowsPE'
        else:
            linkage = 'PE'
    elif 'COFF' in fileout:
        linkage = 'COFF'
    elif 'MS-DOS' in fileout:
        linkage = 'MSDOS'
    return bits, linkage


def _get_machine_win32():
    return os.environ.get('PROCESSOR_ARCHITEW6432', '') or os.environ.get('PROCESSOR_ARCHITECTURE', '')


class _Processor:
    @classmethod
    def get(cls):
        func = getattr(cls, f'get_{sys.platform}', cls.from_subprocess)
        return func() or ''

    def get_win32():
        return os.environ.get('PROCESSOR_IDENTIFIER', _get_machine_win32())

    def get_OpenVMS():
        return ''

    def from_subprocess():
        """Ask ``uname -p`` (empty when it cannot tell)."""
        try:
            import subprocess
        except ImportError:
            return None
        try:
            return subprocess.check_output(
                ['uname', '-p'],
                stderr=subprocess.DEVNULL,
                text=True,
                encoding="utf8",
            ).strip()
        except (OSError, subprocess.CalledProcessError):
            pass


def _unknown_as_blank(val):
    return '' if val == 'unknown' else val


class uname_result(
    collections.namedtuple(
        "uname_result_base",
        "system node release version machine")
        ):
    """A uname() result: system, node, release, version, machine and a
    lazily computed processor."""

    _fields = ('system', 'node', 'release', 'version', 'machine', 'processor')

    @functools.cached_property
    def processor(self):
        return _unknown_as_blank(_Processor.get())

    def __iter__(self):
        return itertools.chain(
            super().__iter__(),
            (self.processor,)
        )

    @classmethod
    def _make(cls, iterable):
        num_fields = len(cls._fields) - 1
        result = cls.__new__(cls, *iterable)
        if len(result) != num_fields + 1:
            msg = f'Expected {num_fields} arguments, got {len(result)}'
            raise TypeError(msg)
        return result

    def __getitem__(self, key):
        return tuple(self)[key]

    def __len__(self):
        return len(tuple(iter(self)))

    def __reduce__(self):
        return uname_result, tuple(self)[:len(self._fields) - 1]


_uname_cache = None


def uname():
    """Return (system, node, release, version, machine, processor) as a
    uname_result; unknown values are empty strings."""
    global _uname_cache

    if _uname_cache is not None:
        return _uname_cache

    try:
        system, node, release, version, machine = infos = os.uname()
    except AttributeError:
        system = sys.platform
        node = _node()
        release = version = machine = ''
        infos = ()

    if not any(infos):
        if system == 'win32':
            release, version, csd, ptype = win32_ver()
            machine = machine or _get_machine_win32()
        if not (release and version):
            system, release, version = system, release, version
        if system == 'Microsoft' and release == 'Windows':
            system = 'Windows'
            release = 'Vista'

    if system == 'OpenVMS':
        if not release or release == '0':
            release = version
            version = ''

    if sys.platform == 'android':
        system = 'Android'
        release = android_ver().release

    vals = system, node, release, version, machine
    _uname_cache = uname_result(*map(_unknown_as_blank, vals))
    return _uname_cache


def system():
    """The system/OS name, e.g. 'Linux' ('' if unknown)."""
    return uname().system


def node():
    """The computer's network name ('' if unknown)."""
    return uname().node


def release():
    """The system's release, e.g. '6.1.0' ('' if unknown)."""
    return uname().release


def version():
    """The system's release version ('' if unknown)."""
    return uname().version


def machine():
    """The machine type, e.g. 'x86_64' ('' if unknown)."""
    return uname().machine


def processor():
    """The (real) processor name ('' if unknown)."""
    return uname().processor


_sys_version_cache = {}


def _sys_version(sys_version=None):
    """Parse a sys.version-like string into (name, version, branch,
    revision, buildno, builddate, compiler)."""
    if sys_version is None:
        sys_version = sys.version

    result = _sys_version_cache.get(sys_version, None)
    if result is not None:
        return result

    if sys.platform.startswith('java'):
        name = 'Jython'
        match = re.match(
            r'([\w.+]+)'
            r'\s*'
            r'\(#?([^,]+)'
            r'(?:,\s*([\w ]*)'
            r'(?:,\s*([\w :]*))?)?\)\s*'
            r'\[([^\]]+)\]?', sys_version)
        if match is None:
            raise ValueError('failed to parse Jython sys.version: %s' %
                             repr(sys_version))
        version, buildno, builddate, buildtime, _ = match.groups()
        if builddate is None:
            builddate = ''
        compiler = sys.platform

    elif "PyPy" in sys_version:
        name = "PyPy"
        match = re.match(
            r'([\w.+]+)\s*'
            r'\(#?([^,]+),\s*([\w ]+),\s*([\w :]+)\)\s*'
            r'\[PyPy [^\]]+\]?', sys_version)
        if match is None:
            raise ValueError("failed to parse PyPy sys.version: %s" %
                             repr(sys_version))
        version, buildno, builddate, buildtime = match.groups()
        compiler = ""

    else:
        match = re.match(
            r'([\w.+]+)\s*'
            r'(?:experimental free-threading build\s+|free-threading build\s+)?'
            r'(?:\|[^|]*\|)?\s*'
            r'\(#?([^,]+)'
            r'(?:,\s*([\w ]*)'
            r'(?:,\s*([\w :]*))?)?\)\s*'
            r'\[([^\]]+)\]?', sys_version)
        if match is None:
            raise ValueError('failed to parse CPython sys.version: %s' %
                             repr(sys_version))
        version, buildno, builddate, buildtime, compiler = \
              match.groups()
        name = 'CPython'
        if builddate is None:
            builddate = ''
        elif buildtime:
            builddate = builddate + ' ' + buildtime

    if hasattr(sys, '_git'):
        _, branch, revision = sys._git
    elif hasattr(sys, '_mercurial'):
        _, branch, revision = sys._mercurial
    else:
        branch = ''
        revision = ''

    l = version.split('.')
    if len(l) == 2:
        l.append('0')
        version = '.'.join(l)

    result = (name, version, branch, revision, buildno, builddate, compiler)
    _sys_version_cache[sys_version] = result
    return result


def python_implementation():
    """The Python implementation name ('CPython' for this interpreter)."""
    return _sys_version()[0]


def python_version():
    """The Python version as 'major.minor.patchlevel'."""
    return _sys_version()[1]


def python_version_tuple():
    """The Python version as a tuple of strings (major, minor, patchlevel)."""
    return tuple(_sys_version()[1].split('.'))


def python_branch():
    """The branch of the build ('' if unknown)."""
    return _sys_version()[2]


def python_revision():
    """The revision of the build ('' if unknown)."""
    return _sys_version()[3]


def python_build():
    """(buildno, builddate) of the interpreter."""
    return _sys_version()[4:6]


def python_compiler():
    """The compiler used to build the interpreter."""
    return _sys_version()[6]


_platform_cache = {}


def platform(aliased=False, terse=False):
    """A single string identifying the platform, e.g.
    'Linux-6.1.0-x86_64-with-glibc2.39'."""
    result = _platform_cache.get((aliased, terse), None)
    if result is not None:
        return result

    system, node, release, version, machine, processor = uname()
    if machine == processor:
        processor = ''
    if aliased:
        system, release, version = system_alias(system, release, version)

    if system == 'Darwin':
        macos_release = mac_ver()[0]
        if macos_release:
            system = 'macOS'
            release = macos_release

    if system == 'Windows':
        platform = _platform(system, release, version, machine)
    elif system in ('Linux',):
        libcname, libcversion = libc_ver()
        platform = _platform(system, release, machine, processor,
                             'with',
                             libcname+libcversion)
    elif system == 'Java':
        platform = _platform(system, release, version)
    else:
        if terse:
            platform = _platform(system, release)
        else:
            bits, linkage = architecture(sys.executable)
            platform = _platform(system, release, machine,
                                 processor, bits, linkage)

    if terse and system == 'Linux':
        platform = _platform(system, release)

    _platform_cache[(aliased, terse)] = platform
    return platform


_os_release_line = re.compile(
    "^(?P<name>[a-zA-Z0-9_]+)=(?P<quote>[\"\']?)(?P<value>.*)(?P=quote)$"
)
_os_release_unescape = re.compile(r"\\([\\\$\"\'`])")
_os_release_candidates = ("/etc/os-release", "/usr/lib/os-release")
_os_release_cache = None


def _parse_os_release(lines):
    info = {
        "NAME": "Linux",
        "ID": "linux",
        "PRETTY_NAME": "Linux",
    }

    for line in lines:
        mo = _os_release_line.match(line)
        if mo is not None:
            info[mo.group('name')] = _os_release_unescape.sub(
                r"\1", mo.group('value')
            )

    return info


def freedesktop_os_release():
    """Return the operating system identification from os-release(5)."""
    global _os_release_cache

    if _os_release_cache is None:
        errno = None
        for candidate in _os_release_candidates:
            try:
                with open(candidate, encoding="utf-8") as f:
                    _os_release_cache = _parse_os_release(f)
                break
            except OSError as e:
                errno = e.errno
        else:
            raise OSError(
                errno,
                f"Unable to read files {', '.join(_os_release_candidates)}"
            )

    return _os_release_cache.copy()


def invalidate_caches():
    """Forget all cached platform information."""
    global _uname_cache, _os_release_cache
    _platform_cache.clear()
    _sys_version_cache.clear()
    _uname_cache = None
    _os_release_cache = None


def _parse_args(args):
    import argparse

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument("args", nargs="*", choices=["nonaliased", "terse"])
    parser.add_argument(
        "--terse",
        action="store_true",
        help=(
            "return only the absolute minimum information needed to identify the "
            "platform"
        ),
    )
    parser.add_argument(
        "--nonaliased",
        dest="aliased",
        action="store_false",
        help=(
            "disable system/OS name aliasing. If aliasing is enabled, some platforms "
            "report system names different from their common names, e.g. SunOS is "
            "reported as Solaris"
        ),
    )

    return parser.parse_args(args)


def _main(args=None):
    args = _parse_args(args)

    terse = args.terse or ("terse" in args.args)
    aliased = args.aliased and ('nonaliased' not in args.args)

    print(platform(aliased, terse))


if __name__ == "__main__":
    _main()
