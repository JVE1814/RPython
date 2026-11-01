"""Run Python code located through the import system (``python -m``) or
by file system path (run_module() and run_path())."""

import sys
import importlib.machinery
import importlib.util
import io
import os

__all__ = [
    "run_module", "run_path",
]

_ModuleType = type(sys)


class _TempModule(object):
    """Temporarily replace a module in sys.modules with an empty one."""
    def __init__(self, mod_name):
        self.mod_name = mod_name
        self.module = _ModuleType(mod_name)
        self._saved_module = []

    def __enter__(self):
        mod_name = self.mod_name
        try:
            self._saved_module.append(sys.modules[mod_name])
        except KeyError:
            pass
        sys.modules[mod_name] = self.module
        return self

    def __exit__(self, *args):
        if self._saved_module:
            sys.modules[self.mod_name] = self._saved_module[0]
        else:
            del sys.modules[self.mod_name]
        self._saved_module = []


class _ModifiedArgv0(object):
    """Temporarily set sys.argv[0]."""
    def __init__(self, value):
        self.value = value
        self._saved_value = self._sentinel = object()

    def __enter__(self):
        if self._saved_value is not self._sentinel:
            raise RuntimeError("Already preserving saved value")
        self._saved_value = sys.argv[0]
        sys.argv[0] = self.value

    def __exit__(self, *args):
        self.value = self._sentinel
        sys.argv[0] = self._saved_value


def _run_code(code, run_globals, init_globals=None,
              mod_name=None, mod_spec=None,
              pkg_name=None, script_name=None):
    """Execute *code* in *run_globals* after filling in the module
    attributes; returns the globals dict."""
    if init_globals is not None:
        run_globals.update(init_globals)
    if mod_spec is None:
        loader = None
        fname = script_name
        cached = None
    else:
        loader = mod_spec.loader
        fname = mod_spec.origin
        cached = mod_spec.cached
        if pkg_name is None:
            pkg_name = mod_spec.parent
    run_globals.update(__name__ = mod_name,
                       __file__ = fname,
                       __cached__ = cached,
                       __doc__ = None,
                       __loader__ = loader,
                       __package__ = pkg_name,
                       __spec__ = mod_spec)
    exec(code, run_globals)
    return run_globals


def _run_module_code(code, init_globals=None,
                    mod_name=None, mod_spec=None,
                    pkg_name=None, script_name=None):
    """Execute code in a fresh temporary module (with sys.argv[0] set to
    the file name) and return a copy of its globals."""
    fname = script_name if mod_spec is None else mod_spec.origin
    with _TempModule(mod_name) as temp_module, _ModifiedArgv0(fname):
        mod_globals = temp_module.module.__dict__
        _run_code(code, mod_globals, init_globals,
                  mod_name, mod_spec, pkg_name, script_name)
    return mod_globals.copy()


def _get_module_details(mod_name, error=ImportError):
    """Return (mod_name, spec, code) for running module *mod_name*,
    following packages to their __main__ submodule."""
    if mod_name.startswith("."):
        raise error("Relative module names not supported")
    pkg_name, _, _ = mod_name.rpartition(".")
    if pkg_name:
        try:
            __import__(pkg_name)
        except ImportError as e:
            if e.name is None or (e.name != pkg_name and
                    not pkg_name.startswith(e.name + ".")):
                raise
        existing = sys.modules.get(mod_name)
        if existing is not None and not hasattr(existing, "__path__"):
            from warnings import warn
            msg = "{mod_name!r} found in sys.modules after import of " \
                "package {pkg_name!r}, but prior to execution of " \
                "{mod_name!r}; this may result in unpredictable " \
                "behaviour".format(mod_name=mod_name, pkg_name=pkg_name)
            warn(RuntimeWarning(msg))

    try:
        spec = importlib.util.find_spec(mod_name)
    except (ImportError, AttributeError, TypeError, ValueError) as ex:
        msg = "Error while finding module specification for {!r} ({}: {})"
        if mod_name.endswith(".py"):
            msg += (f". Try using '{mod_name[:-3]}' instead of "
                    f"'{mod_name}' as the module name.")
        raise error(msg.format(mod_name, type(ex).__name__, ex)) from ex
    if spec is None:
        raise error("No module named %s" % mod_name)
    if spec.submodule_search_locations is not None:
        if mod_name == "__main__" or mod_name.endswith(".__main__"):
            raise error("Cannot use package as __main__ module")
        try:
            pkg_main_name = mod_name + ".__main__"
            return _get_module_details(pkg_main_name, error)
        except error as e:
            if mod_name not in sys.modules:
                raise
            raise error(("%s; %r is a package and cannot " +
                               "be directly executed") %(e, mod_name))
    loader = spec.loader
    if loader is None:
        raise error("%r is a namespace package and cannot be executed"
                                                                 % mod_name)
    try:
        code = loader.get_code(mod_name)
    except ImportError as e:
        raise error(format(e)) from e
    if code is None:
        raise error("No code object available for %s" % mod_name)
    return mod_name, spec, code


class _Error(Exception):
    """Error that _run_module_as_main() should report without a
    traceback."""


def _run_module_as_main(mod_name, alter_argv=True):
    """Run a module as __main__ (the implementation of ``-m``): its code
    runs in the existing __main__ namespace, so this cannot be used for
    anything else than the program's main module."""
    try:
        if alter_argv or mod_name != "__main__":
            mod_name, mod_spec, code = _get_module_details(mod_name, _Error)
        else:
            mod_name, mod_spec, code = _get_main_module_details(_Error)
    except _Error as exc:
        msg = "%s: %s" % (sys.executable, exc)
        sys.exit(msg)
    main_globals = sys.modules["__main__"].__dict__
    if alter_argv:
        sys.argv[0] = mod_spec.origin
    return _run_code(code, main_globals, None,
                     "__main__", mod_spec)


def run_module(mod_name, init_globals=None,
               run_name=None, alter_sys=False):
    """Execute a module's code without importing it and return the
    resulting globals dict.  *run_name* defaults to the module name;
    with *alter_sys*, sys.argv[0] and sys.modules[run_name] are updated
    while the code runs."""
    mod_name, mod_spec, code = _get_module_details(mod_name)
    if run_name is None:
        run_name = mod_name
    if alter_sys:
        return _run_module_code(code, init_globals, run_name, mod_spec)
    else:
        return _run_code(code, {}, init_globals, run_name, mod_spec)


def _get_main_module_details(error=ImportError):
    """Locate __main__ along sys.path (used for directory and zipfile
    execution); __main__ itself is hidden from the import system
    meanwhile."""
    main_name = "__main__"
    saved_main = sys.modules[main_name]
    del sys.modules[main_name]
    try:
        return _get_module_details(main_name)
    except ImportError as exc:
        if main_name in str(exc):
            raise error("can't find %r module in %r" %
                              (main_name, sys.path[0])) from exc
        raise
    finally:
        sys.modules[main_name] = saved_main


def _get_code_from_file(fname):
    """Return the code object for a .py or .pyc file."""
    from pkgutil import read_code
    code_path = os.path.abspath(fname)
    with io.open_code(code_path) as f:
        code = read_code(f)
    if code is None:
        with io.open_code(code_path) as f:
            code = compile(f.read(), fname, 'exec')
    return code


def run_path(path_name, init_globals=None, run_name=None):
    """Execute the code at *path_name* and return the resulting globals.

    *path_name* may be a Python source or bytecode file, or a directory or
    zipfile containing a __main__ module.  *run_name* defaults to
    '<run_path>'.
    """
    if run_name is None:
        run_name = "<run_path>"
    pkg_name = run_name.rpartition(".")[0]
    from pkgutil import get_importer
    importer = get_importer(path_name)
    path_name = os.fsdecode(path_name)
    if isinstance(importer, type(None)):
        code = _get_code_from_file(path_name)
        return _run_module_code(code, init_globals, run_name,
                                pkg_name=pkg_name, script_name=path_name)
    else:
        sys.path.insert(0, path_name)
        try:
            mod_name, mod_spec, code = _get_main_module_details()
            with _TempModule(run_name) as temp_module, \
                 _ModifiedArgv0(path_name):
                mod_globals = temp_module.module.__dict__
                return _run_code(code, mod_globals, init_globals,
                                    run_name, mod_spec, pkg_name).copy()
        finally:
            try:
                sys.path.remove(path_name)
            except ValueError:
                pass


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("No module specified for execution", file=sys.stderr)
    else:
        del sys.argv[0]
        _run_module_as_main(sys.argv[0])
