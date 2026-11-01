"""Site-specific configuration, imported automatically at start-up
(unless -S is given).

Adds the site-packages directories (system, user and virtual environment)
to sys.path, processes their .pth files, installs the interactive helpers
(quit, exit, copyright, credits, license, help) and imports sitecustomize
and usercustomize when they exist.
"""

import sys
import os
import builtins
import _sitebuiltins
import stat

PREFIXES = [sys.prefix, sys.exec_prefix]
ENABLE_USER_SITE = None

USER_SITE = None
USER_BASE = None


def _trace(message):
    if sys.flags.verbose:
        print(message, file=sys.stderr)


def _warn(*args, **kwargs):
    import warnings
    warnings.warn(*args, **kwargs)


def makepath(*paths):
    dir = os.path.join(*paths)
    try:
        dir = os.path.abspath(dir)
    except OSError:
        pass
    return dir, os.path.normcase(dir)


def abs_paths():
    """Make the __file__ and __cached__ of already imported modules
    absolute."""
    for m in set(sys.modules.values()):
        loader_module = None
        try:
            loader_module = m.__loader__.__module__
        except AttributeError:
            try:
                loader_module = m.__spec__.loader.__module__
            except AttributeError:
                pass
        if loader_module not in {'_frozen_importlib', '_frozen_importlib_external'}:
            continue
        try:
            m.__file__ = os.path.abspath(m.__file__)
        except (AttributeError, OSError, TypeError):
            pass
        try:
            m.__cached__ = os.path.abspath(m.__cached__)
        except (AttributeError, OSError, TypeError):
            pass


def removeduppaths():
    """Remove duplicate entries from sys.path (comparing normalised
    absolute paths) and return the set of known paths."""
    L = []
    known_paths = set()
    for dir in sys.path:
        dir, dircase = makepath(dir)
        if dircase not in known_paths:
            L.append(dir)
            known_paths.add(dircase)
    sys.path[:] = L
    return known_paths


def _init_pathinfo():
    """Return the set of normalised paths already on sys.path."""
    d = set()
    for item in sys.path:
        try:
            if os.path.exists(item):
                _, itemcase = makepath(item)
                d.add(itemcase)
        except TypeError:
            continue
    return d


def addpackage(sitedir, name, known_paths):
    """Process the .pth file *name* in *sitedir*: lines starting with
    "import" are executed, other non-comment lines are directories added
    to sys.path if they exist."""
    if known_paths is None:
        known_paths = _init_pathinfo()
        reset = True
    else:
        reset = False
    fullname = os.path.join(sitedir, name)
    try:
        st = os.lstat(fullname)
    except OSError:
        return
    if ((getattr(st, 'st_flags', 0) & stat.UF_HIDDEN) or
        (getattr(st, 'st_file_attributes', 0) & stat.FILE_ATTRIBUTE_HIDDEN)):
        _trace(f"Skipping hidden .pth file: {fullname!r}")
        return
    _trace(f"Processing .pth file: {fullname!r}")
    try:
        with io_open_code(fullname) as f:
            pth_content = f.read()
    except OSError:
        return

    try:
        pth_content = pth_content.decode("utf-8-sig")
    except UnicodeDecodeError:
        import locale
        pth_content = pth_content.decode(locale.getencoding())
        _trace(f"Cannot read {fullname!r} as UTF-8. "
               f"Using fallback encoding {locale.getencoding()!r}")

    for n, line in enumerate(pth_content.splitlines(), 1):
        if line.startswith("#"):
            continue
        if line.strip() == "":
            continue
        try:
            if line.startswith(("import ", "import\t")):
                exec(line)
                continue
            line = line.rstrip()
            dir, dircase = makepath(sitedir, line)
            if dircase not in known_paths and os.path.exists(dir):
                sys.path.append(dir)
                known_paths.add(dircase)
        except Exception as exc:
            print(f"Error processing line {n:d} of {fullname}:\n",
                  file=sys.stderr)
            import traceback
            for record in traceback.format_exception(exc):
                for line in record.splitlines():
                    print('  '+line, file=sys.stderr)
            print("\nRemainder of file ignored", file=sys.stderr)
            break
    if reset:
        known_paths = None
    return known_paths


def io_open_code(path):
    import io
    return io.open_code(path)


def addsitedir(sitedir, known_paths=None):
    """Add *sitedir* to sys.path (if missing) and process its .pth
    files."""
    _trace(f"Adding directory: {sitedir!r}")
    if known_paths is None:
        known_paths = _init_pathinfo()
        reset = True
    else:
        reset = False
    sitedir, sitedircase = makepath(sitedir)
    if not sitedircase in known_paths:
        sys.path.append(sitedir)
        known_paths.add(sitedircase)
    try:
        names = os.listdir(sitedir)
    except OSError:
        return
    names = [name for name in names
             if name.endswith(".pth") and not name.startswith(".")]
    for name in sorted(names):
        addpackage(sitedir, name, known_paths)
    if reset:
        known_paths = None
    return known_paths


def check_enableusersite():
    """Decide whether the user site directory may be used: False when
    disabled by -s, None when the process runs with changed uid/gid, True
    otherwise."""
    if sys.flags.no_user_site:
        return False

    if hasattr(os, "getuid") and hasattr(os, "geteuid"):
        if os.geteuid() != os.getuid():
            return None
    if hasattr(os, "getgid") and hasattr(os, "getegid"):
        if os.getegid() != os.getgid():
            return None

    return True


def _getuserbase():
    env_base = os.environ.get("PYTHONUSERBASE", None)
    if env_base:
        return env_base

    if sys.platform in {"emscripten", "wasi", "ios", "tvos", "watchos"}:
        return None

    def joinuser(*args):
        return os.path.expanduser(os.path.join(*args))

    if os.name == "nt":
        base = os.environ.get("APPDATA") or "~"
        return joinuser(base, "Python")

    if sys.platform == "darwin" and sys._framework:
        return joinuser("~", "Library", sys._framework,
                        "%d.%d" % sys.version_info[:2])

    return joinuser("~", ".local")


def _get_path(userbase):
    version = sys.version_info
    if hasattr(sys, 'abiflags') and 't' in sys.abiflags:
        abi_thread = 't'
    else:
        abi_thread = ''

    implementation = _get_implementation()
    implementation_lower = implementation.lower()
    if os.name == 'nt':
        ver_nodot = sys.winver.replace('.', '')
        return f'{userbase}\\{implementation}{ver_nodot}\\site-packages'

    if sys.platform == 'darwin' and sys._framework:
        return f'{userbase}/lib/{implementation_lower}/site-packages'

    return f'{userbase}/lib/python{version[0]}.{version[1]}{abi_thread}/site-packages'


def _get_implementation():
    return 'Python'


def getuserbase():
    """Return the user base directory (PYTHONUSERBASE or ~/.local),
    computing and caching USER_BASE."""
    global USER_BASE
    if USER_BASE is None:
        USER_BASE = _getuserbase()
    return USER_BASE


def getusersitepackages():
    """Return the user-specific site-packages directory, computing and
    caching USER_SITE."""
    global USER_SITE, ENABLE_USER_SITE
    userbase = getuserbase()
    if USER_SITE is None:
        if userbase is None:
            ENABLE_USER_SITE = False
        else:
            USER_SITE = _get_path(userbase)
    return USER_SITE


def addusersitepackages(known_paths):
    """Add the user site-packages directory to sys.path if it exists."""
    _trace("Processing user site-packages")
    user_site = getusersitepackages()

    if ENABLE_USER_SITE and os.path.isdir(user_site):
        addsitedir(user_site, known_paths)
    return known_paths


def getsitepackages(prefixes=None):
    """Return the global site-packages directories for *prefixes*
    (default: PREFIXES)."""
    sitepackages = []
    seen = set()

    if prefixes is None:
        prefixes = PREFIXES

    for prefix in prefixes:
        if not prefix or prefix in seen:
            continue
        seen.add(prefix)

        implementation = _get_implementation().lower()
        ver = sys.version_info
        if hasattr(sys, 'abiflags') and 't' in sys.abiflags:
            abi_thread = 't'
        else:
            abi_thread = ''
        if os.sep == '/':
            libdirs = [sys.platlibdir]
            if sys.platlibdir != "lib":
                libdirs.append("lib")

            for libdir in libdirs:
                path = os.path.join(prefix, libdir,
                                    f"{implementation}{ver[0]}.{ver[1]}{abi_thread}",
                                    "site-packages")
                sitepackages.append(path)
        else:
            sitepackages.append(prefix)
            sitepackages.append(os.path.join(prefix, "Lib", "site-packages"))
    return sitepackages


def addsitepackages(known_paths, prefixes=None):
    """Add the existing global site-packages directories to sys.path."""
    _trace("Processing global site-packages")
    for sitedir in getsitepackages(prefixes):
        if os.path.isdir(sitedir):
            addsitedir(sitedir, known_paths)

    return known_paths


def setquit():
    """Define builtins quit() and exit() (they raise SystemExit) with a
    helpful repr."""
    if os.sep == '\\':
        eof = 'Ctrl-Z plus Return'
    else:
        eof = 'Ctrl-D (i.e. EOF)'

    builtins.quit = _sitebuiltins.Quitter('quit', eof)
    builtins.exit = _sitebuiltins.Quitter('exit', eof)


def setcopyright():
    """Set 'copyright' and 'credits' in builtins"""
    builtins.copyright = _sitebuiltins._Printer("copyright", sys.copyright)
    builtins.credits = _sitebuiltins._Printer("credits", """\
    Thanks to CWI, CNRI, BeOpen, Zope Corporation, the Python Software
    Foundation, and a cast of thousands for supporting Python
    development.  See www.python.org for more information.""")
    files, dirs = [], []
    if hasattr(os, '__file__'):
        here = os.path.dirname(os.__file__)
        files.extend(["LICENSE.txt", "LICENSE"])
        dirs.extend([os.path.join(here, os.pardir), here, os.curdir])
    builtins.license = _sitebuiltins._Printer(
        "license",
        "See https://www.python.org/psf/license/",
        files, dirs)


def sethelper():
    builtins.help = _sitebuiltins._Helper()


def gethistoryfile():
    """Path of the interactive history file (PYTHON_HISTORY or
    ~/.python_history)."""
    if not sys.flags.ignore_environment:
        history = os.environ.get("PYTHON_HISTORY")
        if history:
            return history
    return os.path.join(os.path.expanduser('~'),
        '.python_history')


def enablerlcompleter():
    """Install sys.__interactivehook__ enabling readline completion and
    history for the interactive interpreter."""
    def register_readline():
        import atexit
        try:
            import readline
            import rlcompleter
        except ImportError:
            return

        readline_doc = getattr(readline, '__doc__', '')
        if readline_doc is not None and 'libedit' in readline_doc:
            readline.parse_and_bind('bind ^I rl_complete')
        else:
            readline.parse_and_bind('tab: complete')

        try:
            readline.read_init_file()
        except OSError:
            pass

        if readline.get_current_history_length() == 0:
            history = gethistoryfile()
            try:
                readline.read_history_file(history)
            except (OSError, UnicodeError):
                pass

            def write_history():
                try:
                    readline.write_history_file(history)
                except OSError:
                    pass

            atexit.register(write_history)

    sys.__interactivehook__ = register_readline


def venv(known_paths):
    """Configure a virtual environment when a pyvenv.cfg file is found
    next to (or one level above) the executable."""
    global PREFIXES, ENABLE_USER_SITE

    env = os.environ
    if sys.platform == 'darwin' and '__PYVENV_LAUNCHER__' in env:
        executable = sys._base_executable = os.environ['__PYVENV_LAUNCHER__']
    else:
        executable = sys.executable
    exe_dir = os.path.dirname(os.path.abspath(executable))
    site_prefix = os.path.dirname(exe_dir)
    sys._home = None
    conf_basename = 'pyvenv.cfg'
    candidate_conf = next(
        (
            conffile for conffile in (
                os.path.join(exe_dir, conf_basename),
                os.path.join(site_prefix, conf_basename)
            )
            if os.path.isfile(conffile)
        ),
        None
    )

    if candidate_conf:
        virtual_conf = candidate_conf
        system_site = "true"
        with open(virtual_conf, encoding='utf-8') as f:
            for line in f:
                if '=' in line:
                    key, _, value = line.partition('=')
                    key = key.strip().lower()
                    value = value.strip()
                    if key == 'include-system-site-packages':
                        system_site = value.lower()
                    elif key == 'home':
                        sys._home = value

        if sys.prefix != site_prefix:
            _warn(f'Unexpected value in sys.prefix, expected {site_prefix}, got {sys.prefix}', RuntimeWarning)
        if sys.exec_prefix != site_prefix:
            _warn(f'Unexpected value in sys.exec_prefix, expected {site_prefix}, got {sys.exec_prefix}', RuntimeWarning)

        addsitepackages(known_paths, [sys.prefix])

        if system_site == "true":
            PREFIXES += [sys.base_prefix, sys.base_exec_prefix]
        else:
            ENABLE_USER_SITE = False

    return known_paths


def execsitecustomize():
    """Import sitecustomize if it exists."""
    try:
        try:
            import sitecustomize
        except ImportError as exc:
            if exc.name == 'sitecustomize':
                pass
            else:
                raise
    except Exception as err:
        if sys.flags.verbose:
            sys.excepthook(*sys.exc_info())
        else:
            sys.stderr.write(
                "Error in sitecustomize; set PYTHONVERBOSE for traceback:\n"
                "%s: %s\n" %
                (err.__class__.__name__, err))


def execusercustomize():
    """Import usercustomize if it exists."""
    try:
        try:
            import usercustomize
        except ImportError as exc:
            if exc.name == 'usercustomize':
                pass
            else:
                raise
    except Exception as err:
        if sys.flags.verbose:
            sys.excepthook(*sys.exc_info())
        else:
            sys.stderr.write(
                "Error in usercustomize; set PYTHONVERBOSE for traceback:\n"
                "%s: %s\n" %
                (err.__class__.__name__, err))


def main():
    """Add standard site-specific directories to the module search path.

    Called automatically at start-up unless -S was given.
    """
    global ENABLE_USER_SITE

    orig_path = sys.path[:]
    known_paths = removeduppaths()
    if orig_path != sys.path:
        abs_paths()

    known_paths = venv(known_paths)
    if ENABLE_USER_SITE is None:
        ENABLE_USER_SITE = check_enableusersite()
    known_paths = addusersitepackages(known_paths)
    known_paths = addsitepackages(known_paths)
    setquit()
    setcopyright()
    sethelper()
    if not sys.flags.isolated:
        enablerlcompleter()
    execsitecustomize()
    if ENABLE_USER_SITE:
        execusercustomize()

if not sys.flags.no_site:
    main()


def _script():
    help = """\
    %s [--user-base] [--user-site]

    Without arguments print some useful information
    With arguments print the value of USER_BASE and/or USER_SITE separated
    by '%s'.

    Exit codes with --user-base or --user-site:
      0 - user site directory is enabled
      1 - user site directory is disabled by user
      2 - user site directory is disabled by super user
          or for security reasons
     >2 - unknown error
    """
    args = sys.argv[1:]
    if not args:
        user_base = getuserbase()
        user_site = getusersitepackages()
        print("sys.path = [")
        for dir in sys.path:
            print("    %r," % (dir,))
        print("]")
        def exists(path):
            if path is not None and os.path.isdir(path):
                return "exists"
            else:
                return "doesn't exist"
        print(f"USER_BASE: {user_base!r} ({exists(user_base)})")
        print(f"USER_SITE: {user_site!r} ({exists(user_site)})")
        print(f"ENABLE_USER_SITE: {ENABLE_USER_SITE!r}")
        sys.exit(0)

    buffer = []
    if '--user-base' in args:
        buffer.append(USER_BASE)
    if '--user-site' in args:
        buffer.append(USER_SITE)

    if buffer:
        print(os.pathsep.join(buffer))
        if ENABLE_USER_SITE:
            sys.exit(0)
        elif ENABLE_USER_SITE is False:
            sys.exit(1)
        elif ENABLE_USER_SITE is None:
            sys.exit(2)
        else:
            sys.exit(3)
    else:
        import textwrap
        print(textwrap.dedent(help % (sys.argv[0], os.pathsep)))
        sys.exit(10)

if __name__ == '__main__':
    _script()
