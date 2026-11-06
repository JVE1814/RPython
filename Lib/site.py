"""Start-up configuration of the module search path.

Imported automatically at start-up (unless -S is given).  It

* removes duplicate entries from sys.path,
* adds the site-packages directories -- of a virtual environment, of the
  user (unless disabled) and of the installation -- and processes the
  configuration files found there:

    name.pth    one entry per line: a directory to append to sys.path
                (relative to the site directory), or a line starting with
                "import" that is executed; blank lines and '#' comments
                are ignored;
    name.start  one entry point per line, "package.module:callable",
                called after all paths have been added (PEP 829); a
                name.start file also disables the import lines of the
                name.pth file next to it;

* defines the interactive helpers quit, exit, copyright, credits, license
  and help, installs readline support for the interactive interpreter, and
* imports the optional modules sitecustomize and usercustomize.

`python -m site` prints the resulting configuration.
"""

import sys
import os
import builtins
import _sitebuiltins
import stat

# prefixes whose site-packages directories are searched
PREFIXES = [sys.prefix, sys.exec_prefix]
# True: user site directory enabled; False: disabled by the user (-s or
# configuration); None: disabled for safety (set-uid/set-gid process)
ENABLE_USER_SITE = None
# computed on demand by getusersitepackages() / getuserbase()
USER_SITE = None
USER_BASE = None


def _trace(message):
    """Report `message` on stderr in verbose mode (-v)."""
    if sys.flags.verbose:
        print(message, file=sys.stderr)


def _warn(*args, **kwargs):
    import warnings
    warnings.warn(*args, **kwargs)


def makepath(*paths):
    """(absolute path, normalised-case path) of the joined `paths`."""
    joined = os.path.join(*paths)
    try:
        joined = os.path.abspath(joined)
    except OSError:
        pass
    return joined, os.path.normcase(joined)


def abs_paths():
    """Turn __file__ and __cached__ of modules loaded by the standard
    import machinery into absolute paths."""
    standard = ('_frozen_importlib', '_frozen_importlib_external')
    for module in set(sys.modules.values()):
        loader = getattr(module, '__loader__', None)
        if loader is None:
            loader = getattr(getattr(module, '__spec__', None), 'loader', None)
        if getattr(loader, '__module__', None) not in standard:
            continue
        for attribute in ('__file__', '__cached__'):
            try:
                setattr(module, attribute,
                        os.path.abspath(getattr(module, attribute)))
            except (AttributeError, OSError, TypeError):
                pass


def removeduppaths():
    """Drop repeated sys.path entries (comparing normalised absolute
    paths); return the set of normalised entries."""
    seen = set()
    kept = []
    for entry in sys.path:
        absolute, key = makepath(entry)
        if key in seen:
            continue
        seen.add(key)
        kept.append(absolute)
    sys.path[:] = kept
    return seen


def _init_pathinfo():
    """Normalised forms of the existing directories on sys.path."""
    known = set()
    for entry in sys.path:
        try:
            if os.path.exists(entry):
                known.add(makepath(entry)[1])
        except TypeError:
            continue
    return known


def _is_hidden(path):
    try:
        info = os.lstat(path)
    except OSError:
        return None
    if getattr(info, 'st_flags', 0) & getattr(stat, 'UF_HIDDEN', 0):
        return True
    attributes = getattr(info, 'st_file_attributes', 0)
    return bool(attributes & getattr(stat, 'FILE_ATTRIBUTE_HIDDEN', 0))


def _print_failure(heading, exc):
    """Write `heading` and the indented traceback of `exc` to stderr."""
    import traceback
    print(heading, file=sys.stderr)
    for chunk in traceback.format_exception(exc):
        for text in chunk.splitlines():
            print('    ' + text, file=sys.stderr)


def _run_pth_line(sitedir, fullname, line):
    # Code in .pth files traditionally finds `sitedir` in the locals of its
    # caller and `fullname`/`filename` as plain names; keep both working.
    filename = fullname
    exec(line)


class StartupState:
    """Collects what the .pth and .start files of site directories ask for
    and applies it in one go: process() first extends sys.path, then runs
    the .pth import lines, then calls the .start entry points."""

    def __init__(self, known_paths=None):
        self._known_paths = _init_pathinfo() if known_paths is None else known_paths
        # (file that asked for it or None, directory) in request order
        self._path_entries = []
        # .pth file -> its import lines (and their line numbers)
        self._importexecs = {}
        self._import_linenos = {}
        # .start file -> its entry points
        self._entrypoints = {}
        self._sitedirs = set()

    def addsitedir(self, sitedir):
        """Queue `sitedir` itself and read its .pth and .start files."""
        _trace(f"Adding directory: {sitedir!r}")
        sitedir, key = makepath(sitedir)
        if key not in self._known_paths:
            self._known_paths.add(key)
            self._path_entries.append((None, sitedir))
        if key in self._sitedirs:
            return
        self._sitedirs.add(key)
        try:
            names = os.listdir(sitedir)
        except OSError:
            return
        visible = sorted(name for name in names if not name.startswith('.'))
        for name in visible:
            if name.endswith('.pth'):
                self._read_pth_file(sitedir, name)
        for name in visible:
            if name.endswith('.start'):
                self._read_start_file(sitedir, name)

    @staticmethod
    def _read_text(fullname, kind):
        """Contents of a configuration file, or None if it is missing,
        hidden or unreadable."""
        hidden = _is_hidden(fullname)
        if hidden is None:
            return None
        if hidden:
            _trace(f"Skipping hidden {kind} file: {fullname!r}")
            return None
        _trace(f"Processing {kind} file: {fullname!r}")
        try:
            with open(fullname, 'rb') as stream:
                return stream.read()
        except OSError:
            return None

    def _read_pth_file(self, sitedir, name):
        fullname = os.path.join(sitedir, name)
        data = self._read_text(fullname, '.pth')
        if data is None:
            return
        try:
            text = data.decode('utf-8-sig')
        except UnicodeDecodeError:
            import locale
            encoding = locale.getencoding()
            _trace(f"Cannot read {fullname!r} as UTF-8; "
                   f"using the locale encoding {encoding!r}")
            text = data.decode(encoding, 'replace')
        for number, line in enumerate(text.splitlines(), 1):
            if not line.strip() or line.startswith('#'):
                continue
            if line.startswith(('import ', 'import\t')):
                self._importexecs.setdefault(fullname, []).append(line)
                self._import_linenos.setdefault(fullname, []).append(number)
                continue
            try:
                directory, key = makepath(sitedir, line.rstrip())
                if key in self._known_paths or not os.path.exists(directory):
                    continue
            except Exception as exc:
                if sys.flags.verbose:
                    _print_failure(f"Error in {fullname}, line {number}:", exc)
                continue
            self._known_paths.add(key)
            self._path_entries.append((fullname, directory))

    def _read_start_file(self, sitedir, name):
        fullname = os.path.join(sitedir, name)
        data = self._read_text(fullname, '.start')
        if data is None:
            return
        try:
            text = data.decode('utf-8-sig')
        except UnicodeDecodeError:
            _trace(f"Skipping {fullname!r}: not valid UTF-8")
            return
        entries = self._entrypoints.setdefault(fullname, [])
        for line in text.splitlines():
            entry = line.strip()
            if entry and not entry.startswith('#'):
                entries.append(entry)

    def _extend_syspath(self):
        pending, self._path_entries = self._path_entries, []
        for source, directory in pending:
            if not os.path.exists(directory):
                origin = f" (from {source})" if source else ""
                print(f"site: directory {directory!r}{origin} does not exist",
                      file=sys.stderr)
                continue
            if directory not in sys.path:
                sys.path.append(directory)

    def _exec_imports(self):
        pending, self._importexecs = self._importexecs, {}
        numbers, self._import_linenos = self._import_linenos, {}
        for fullname, lines in pending.items():
            start_file = os.path.splitext(fullname)[0] + '.start'
            if start_file in self._entrypoints:
                _trace(f"Skipping import lines of {fullname!r}: "
                       f"{start_file!r} exists")
                continue
            sitedir = os.path.dirname(fullname)
            linenos = numbers.get(fullname) or [None] * len(lines)
            for line, number in zip(lines, linenos):
                try:
                    _run_pth_line(sitedir, fullname, line)
                except Exception as exc:
                    _print_failure(f"Error processing line {number} of {fullname}:",
                                   exc)

    def _execute_start_entrypoints(self):
        pending, self._entrypoints = self._entrypoints, {}
        if not any(pending.values()):
            return
        import pkgutil
        for fullname, entries in pending.items():
            for entry in entries:
                try:
                    target = pkgutil.resolve_name(entry, strict=True)
                except ValueError:
                    print(f"Invalid entry point syntax in {fullname}: {entry!r}",
                          file=sys.stderr)
                    continue
                except Exception as exc:
                    _print_failure(f"Error loading entry point {entry!r} "
                                   f"of {fullname}:", exc)
                    continue
                try:
                    target()
                except Exception as exc:
                    _print_failure(f"Error calling entry point {entry!r} "
                                   f"of {fullname}:", exc)

    def process(self):
        """Apply everything collected so far."""
        self._extend_syspath()
        self._exec_imports()
        self._execute_start_entrypoints()


def addpackage(sitedir, name, known_paths):
    """Process the single .pth file `name` of `sitedir` right away."""
    state = StartupState(known_paths)
    state._read_pth_file(sitedir, name)
    state.process()
    return known_paths


def addsitedir(sitedir, known_paths=None):
    """Add `sitedir` to sys.path and process its .pth and .start files.

    With a `known_paths` set, a directory already in it is left alone
    (so repeated calls are harmless); the updated set is returned."""
    if known_paths is not None and makepath(sitedir)[1] in known_paths:
        return known_paths
    state = StartupState(known_paths)
    state.addsitedir(sitedir)
    state.process()
    return known_paths


def check_enableusersite():
    """False if the user site directory is disabled (-s), None in a
    set-uid/set-gid process, True otherwise."""
    if sys.flags.no_user_site:
        return False
    for real, effective in (('getuid', 'geteuid'), ('getgid', 'getegid')):
        if hasattr(os, real) and hasattr(os, effective):
            if getattr(os, real)() != getattr(os, effective)():
                return None
    return True


def _getuserbase():
    configured = os.environ.get("PYTHONUSERBASE")
    if configured:
        return configured
    if sys.platform in ("emscripten", "wasi", "ios", "tvos", "watchos"):
        return None
    if os.name == "nt":
        root = os.environ.get("APPDATA") or "~"
        return os.path.expanduser(os.path.join(root, "Python"))
    if sys.platform == "darwin" and getattr(sys, '_framework', None):
        return os.path.expanduser(os.path.join(
            "~", "Library", sys._framework, "%d.%d" % sys.version_info[:2]))
    return os.path.expanduser(os.path.join("~", ".local"))


def _version_dir():
    suffix = 't' if 't' in getattr(sys, 'abiflags', '') else ''
    return "python%d.%d%s" % (sys.version_info[0], sys.version_info[1], suffix)


def _get_path(userbase):
    if os.name == 'nt':
        return '%s\\Python%s\\site-packages' % (userbase,
                                                 sys.winver.replace('.', ''))
    if sys.platform == 'darwin' and getattr(sys, '_framework', None):
        return '%s/lib/python/site-packages' % userbase
    return '%s/lib/%s/site-packages' % (userbase, _version_dir())


def getuserbase():
    """The user base directory (PYTHONUSERBASE, default ~/.local)."""
    global USER_BASE
    if USER_BASE is None:
        USER_BASE = _getuserbase()
    return USER_BASE


def getusersitepackages():
    """The user's site-packages directory (None where there is none)."""
    global USER_SITE, ENABLE_USER_SITE
    base = getuserbase()
    if USER_SITE is None:
        if base is None:
            ENABLE_USER_SITE = False
        else:
            USER_SITE = _get_path(base)
    return USER_SITE


def getsitepackages(prefixes=None):
    """The global site-packages directories of `prefixes` (default
    PREFIXES)."""
    found = []
    done = set()
    for prefix in PREFIXES if prefixes is None else prefixes:
        if not prefix or prefix in done:
            continue
        done.add(prefix)
        if os.sep == '/':
            libdirs = [sys.platlibdir]
            if sys.platlibdir != "lib":
                libdirs.append("lib")
            for libdir in libdirs:
                found.append(os.path.join(prefix, libdir, _version_dir(),
                                          "site-packages"))
        else:
            found.append(prefix)
            found.append(os.path.join(prefix, "Lib", "site-packages"))
    return found


def _queue_user_site(state):
    _trace("Processing user site-packages")
    user_site = getusersitepackages()
    if ENABLE_USER_SITE and user_site and os.path.isdir(user_site):
        state.addsitedir(user_site)


def _queue_global_sites(state, prefixes=None):
    _trace("Processing global site-packages")
    for directory in getsitepackages(prefixes):
        if os.path.isdir(directory):
            state.addsitedir(directory)


def addusersitepackages(known_paths):
    """Add (and process) the user site-packages directory if it exists."""
    state = StartupState(known_paths)
    _queue_user_site(state)
    state.process()
    return known_paths


def addsitepackages(known_paths, prefixes=None):
    """Add (and process) the existing global site-packages directories."""
    state = StartupState(known_paths)
    _queue_global_sites(state, prefixes)
    state.process()
    return known_paths


def _read_venv_config(path):
    settings = {}
    with open(path, encoding='utf-8') as stream:
        for line in stream:
            key, separator, value = line.partition('=')
            if separator:
                settings[key.strip().lower()] = value.strip()
    return settings


def _venv_config_file():
    if sys.platform == 'darwin' and '__PYVENV_LAUNCHER__' in os.environ:
        executable = sys._base_executable = os.environ['__PYVENV_LAUNCHER__']
    else:
        executable = sys.executable
    exe_dir = os.path.dirname(os.path.abspath(executable))
    prefix = os.path.dirname(exe_dir)
    for candidate in (os.path.join(exe_dir, 'pyvenv.cfg'),
                      os.path.join(prefix, 'pyvenv.cfg')):
        if os.path.isfile(candidate):
            return candidate, prefix
    return None, prefix


def _configure_venv(state):
    global PREFIXES, ENABLE_USER_SITE
    sys._home = None
    config, prefix = _venv_config_file()
    if config is None:
        return
    settings = _read_venv_config(config)
    if 'home' in settings:
        sys._home = settings['home']
    for name, value in (('sys.prefix', sys.prefix),
                        ('sys.exec_prefix', sys.exec_prefix)):
        if value != prefix:
            _warn(f'Unexpected value in {name}, expected {prefix}, got {value}',
                  RuntimeWarning)
    _queue_global_sites(state, [sys.prefix])
    if settings.get('include-system-site-packages', 'true').lower() == 'true':
        PREFIXES += [sys.base_prefix, sys.base_exec_prefix]
    else:
        ENABLE_USER_SITE = False


def venv(known_paths):
    """Set up a virtual environment if a pyvenv.cfg is found next to (or
    one directory above) the executable."""
    state = StartupState(known_paths)
    _configure_venv(state)
    state.process()
    return known_paths


def setquit():
    """Install quit() and exit() in builtins."""
    hint = 'Ctrl-Z plus Return' if os.sep == '\\' else 'Ctrl-D (i.e. EOF)'
    builtins.quit = _sitebuiltins.Quitter('quit', hint)
    builtins.exit = _sitebuiltins.Quitter('exit', hint)


def setcopyright():
    """Install copyright, credits and license in builtins."""
    builtins.copyright = _sitebuiltins._Printer("copyright", sys.copyright)
    builtins.credits = _sitebuiltins._Printer(
        "credits",
        "Python is developed by its core team and many volunteers, with the\n"
        "support of the Python Software Foundation; see www.python.org.\n"
        "This implementation is an independent reimplementation in Rust.")
    files, dirs = [], []
    if hasattr(os, '__file__'):
        library = os.path.dirname(os.__file__)
        files = ["LICENSE.txt", "LICENSE"]
        dirs = [os.path.join(library, os.pardir), library, os.curdir]
    builtins.license = _sitebuiltins._Printer(
        "license", "See https://www.python.org/psf/license/", files, dirs)


def sethelper():
    builtins.help = _sitebuiltins._Helper()


def gethistoryfile():
    """The interactive history file: PYTHON_HISTORY (unless -E/-I) or
    ~/.python_history."""
    if not sys.flags.ignore_environment:
        configured = os.environ.get("PYTHON_HISTORY")
        if configured:
            return configured
    return os.path.join(os.path.expanduser('~'), '.python_history')


def register_readline():
    """The interactive hook: set up readline completion and history."""
    import atexit
    try:
        import readline
        import rlcompleter  # installs the completer
    except ImportError:
        return
    if 'libedit' in (getattr(readline, '__doc__', '') or ''):
        readline.parse_and_bind('bind ^I rl_complete')
    else:
        readline.parse_and_bind('tab: complete')
    try:
        readline.read_init_file()
    except OSError:
        pass
    if readline.get_current_history_length() != 0:
        # a startup file already set up the history
        return
    history = gethistoryfile()
    try:
        readline.read_history_file(history)
    except (OSError, UnicodeError):
        pass

    def save_history():
        try:
            readline.write_history_file(history)
        except OSError:
            pass

    atexit.register(save_history)


def enablerlcompleter():
    """Arrange for the interactive interpreter to get tab completion and
    a persistent history through readline."""
    sys.__interactivehook__ = register_readline


def _import_customization(name):
    try:
        __import__(name)
    except ImportError as exc:
        if exc.name != name:
            _report_customization_error(name, exc)
    except Exception as exc:
        _report_customization_error(name, exc)


def _report_customization_error(name, exc):
    if sys.flags.verbose:
        sys.excepthook(type(exc), exc, exc.__traceback__)
    else:
        sys.stderr.write("Error in %s; set PYTHONVERBOSE for traceback:\n"
                         "%s: %s\n" % (name, type(exc).__name__, exc))


def execsitecustomize():
    """Import sitecustomize, if there is one."""
    _import_customization('sitecustomize')


def execusercustomize():
    """Import usercustomize, if there is one."""
    _import_customization('usercustomize')


def main():
    """Configure sys.path and the interactive helpers (run at start-up
    unless -S was given)."""
    global ENABLE_USER_SITE
    before = sys.path[:]
    known_paths = removeduppaths()
    if before != sys.path:
        abs_paths()
    state = StartupState(known_paths)
    _configure_venv(state)
    if ENABLE_USER_SITE is None:
        ENABLE_USER_SITE = check_enableusersite()
    _queue_user_site(state)
    _queue_global_sites(state)
    state.process()
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


_USAGE = """\
%s [--user-base] [--user-site]

Without arguments, show sys.path and the user site configuration.
With arguments, print USER_BASE and/or USER_SITE (separated by %r) and
exit with status
  0 - the user site directory is enabled
  1 - it was disabled by the user
  2 - it was disabled by the administrator or for security reasons
  3 - unknown state
"""


def _script():
    args = sys.argv[1:]
    if not args:
        user_base = getuserbase()
        user_site = getusersitepackages()

        def state(path):
            if path is not None and os.path.isdir(path):
                return "exists"
            return "doesn't exist"

        print("sys.path = [")
        for entry in sys.path:
            print("    %r," % (entry,))
        print("]")
        print(f"USER_BASE: {user_base!r} ({state(user_base)})")
        print(f"USER_SITE: {user_site!r} ({state(user_site)})")
        print(f"ENABLE_USER_SITE: {ENABLE_USER_SITE!r}")
        sys.exit(0)

    values = []
    if '--user-base' in args:
        values.append(getuserbase())
    if '--user-site' in args:
        values.append(getusersitepackages())
    if not values:
        print(_USAGE % (sys.argv[0], os.pathsep))
        sys.exit(10)
    print(os.pathsep.join(values))
    if ENABLE_USER_SITE:
        sys.exit(0)
    if ENABLE_USER_SITE is False:
        sys.exit(1)
    if ENABLE_USER_SITE is None:
        sys.exit(2)
    sys.exit(3)


if __name__ == '__main__':
    _script()
