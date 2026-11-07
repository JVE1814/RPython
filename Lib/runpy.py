"""Locate and run Python code: modules through the import system
(run_module(), and ``python -m``) and scripts, directories or zip files by
path (run_path())."""

import contextlib
import importlib.machinery
import importlib.util
import io
import os
import sys

__all__ = ["run_module", "run_path"]


@contextlib.contextmanager
def _module_in_sys_modules(name):
    """Install a fresh module object as sys.modules[name] for the duration
    of the block, then restore whatever was there before."""
    previous = sys.modules.get(name, _MISSING)
    module = type(sys)(name)
    sys.modules[name] = module
    try:
        yield module
    finally:
        if previous is _MISSING:
            sys.modules.pop(name, None)
        else:
            sys.modules[name] = previous


@contextlib.contextmanager
def _argv0(value):
    """Temporarily set sys.argv[0]."""
    saved = sys.argv[0]
    sys.argv[0] = value
    try:
        yield
    finally:
        sys.argv[0] = saved


_MISSING = object()


def _run_code(code, run_globals, init_globals=None,
              mod_name=None, mod_spec=None,
              pkg_name=None, script_name=None):
    """Fill in the module attributes in run_globals, execute code there and
    return run_globals."""
    if init_globals is not None:
        run_globals.update(init_globals)
    if mod_spec is not None:
        loader, filename, cached = mod_spec.loader, mod_spec.origin, mod_spec.cached
        if pkg_name is None:
            pkg_name = mod_spec.parent
    else:
        loader, filename, cached = None, script_name, None
    run_globals.update(__name__=mod_name, __file__=filename,
                       __cached__=cached, __doc__=None, __loader__=loader,
                       __package__=pkg_name, __spec__=mod_spec)
    exec(code, run_globals)
    return run_globals


def _run_module_code(code, init_globals=None,
                     mod_name=None, mod_spec=None,
                     pkg_name=None, script_name=None):
    """Run code in a temporary module object registered under mod_name
    (sys.argv[0] is set to the file name meanwhile); returns a copy of the
    resulting globals."""
    filename = script_name if mod_spec is None else mod_spec.origin
    with _module_in_sys_modules(mod_name) as module, _argv0(filename):
        namespace = module.__dict__
        _run_code(code, namespace, init_globals, mod_name, mod_spec,
                  pkg_name, script_name)
        return namespace.copy()


def _err(error, message, name):
    """An `error` instance; ImportError kinds also record the module name."""
    if issubclass(error, ImportError):
        return error(message, name=name)
    return error(message)


def _get_module_details(mod_name, error=ImportError):
    """(mod_name, spec, code) for executing module mod_name; packages are
    executed through their __main__ submodule.  Problems raise `error`."""
    if mod_name.startswith("."):
        raise _err(error, "Relative module names not supported", mod_name)
    package, _, _ = mod_name.rpartition(".")
    if package:
        # importing the parent first gives the same errors as an import
        try:
            __import__(package)
        except ImportError as exc:
            missing = exc.name
            if missing is None or (missing != package and
                                   not package.startswith(missing + ".")):
                raise
        already = sys.modules.get(mod_name)
        if already is not None and not hasattr(already, "__path__"):
            from warnings import warn
            warn(RuntimeWarning(
                f"{mod_name!r} found in sys.modules after import of package "
                f"{package!r}, but prior to execution of {mod_name!r}; this "
                "may result in unpredictable behaviour"))

    try:
        spec = importlib.util.find_spec(mod_name)
    except (ImportError, AttributeError, TypeError, ValueError) as exc:
        message = (f"Error while finding module specification for {mod_name!r} "
                   f"({type(exc).__name__}: {exc})")
        if mod_name.endswith(".py"):
            message += (f". Try using '{mod_name[:-3]}' instead of "
                        f"'{mod_name}' as the module name.")
        raise _err(error, message, mod_name) from exc
    if spec is None:
        raise _err(error, f"No module named {mod_name}", mod_name)

    if spec.submodule_search_locations is not None:
        if mod_name == "__main__" or mod_name.endswith(".__main__"):
            raise _err(error, "Cannot use package as __main__ module", mod_name)
        try:
            return _get_module_details(mod_name + ".__main__", error)
        except error as exc:
            if mod_name not in sys.modules:
                raise
            raise _err(error, f"{exc}; {mod_name!r} is a package and cannot "
                       "be directly executed", mod_name)

    if spec.loader is None:
        raise _err(error, f"{mod_name!r} is a namespace package and cannot "
                   "be executed", mod_name)
    try:
        code = spec.loader.get_code(mod_name)
    except ImportError as exc:
        raise _err(error, format(exc), mod_name) from exc
    if code is None:
        raise _err(error, f"No code object available for {mod_name}", mod_name)
    return mod_name, spec, code


class _Error(Exception):
    """A problem _run_module_as_main() reports without a traceback."""


def _run_module_as_main(mod_name, alter_argv=True):
    """Implementation of ``python -m mod_name``: run the module's code in
    the namespace of the existing __main__ module."""
    try:
        if alter_argv or mod_name != "__main__":
            mod_name, spec, code = _get_module_details(mod_name, _Error)
        else:
            mod_name, spec, code = _get_main_module_details(_Error)
    except _Error as exc:
        sys.exit(f"{sys.executable}: {exc}")
    namespace = sys.modules["__main__"].__dict__
    if alter_argv:
        sys.argv[0] = spec.origin
    return _run_code(code, namespace, None, "__main__", spec)


def run_module(mod_name, init_globals=None, run_name=None, alter_sys=False):
    """Execute module mod_name without importing it; returns its globals.

    run_name (default mod_name) becomes __name__.  With alter_sys=True,
    sys.argv[0] and sys.modules[run_name] are updated while it runs."""
    mod_name, spec, code = _get_module_details(mod_name)
    if run_name is None:
        run_name = mod_name
    if alter_sys:
        return _run_module_code(code, init_globals, run_name, spec)
    return _run_code(code, {}, init_globals, run_name, spec)


def _get_main_module_details(error=ImportError):
    """Find the __main__ module on sys.path (directory or zip file
    execution) while the running __main__ is hidden from the import
    system."""
    current = sys.modules.pop("__main__")
    try:
        return _get_module_details("__main__")
    except ImportError as exc:
        if "__main__" in str(exc):
            raise _err(error, f"can't find '__main__' module in "
                       f"{sys.path[0]!r}", "__main__") from exc
        raise
    finally:
        sys.modules["__main__"] = current


def _read_cached_code(stream):
    """The code object of a .pyc stream, or None if it is not one of ours."""
    import marshal
    header = stream.read(16)
    if header[:4] != importlib.util.MAGIC_NUMBER:
        return None
    return marshal.load(stream)


def _get_code_from_file(fname, module=None):
    """Code object for a source or cached bytecode file."""
    path = os.path.abspath(fname)
    with io.open_code(path) as stream:
        code = _read_cached_code(stream)
    if code is None:
        with io.open_code(path) as stream:
            # warnings raised while compiling are attributed to `module`
            code = compile(stream.read(), fname, 'exec', module=module)
    return code


def run_path(path_name, init_globals=None, run_name=None):
    """Execute the script, cached bytecode file, directory or zip file at
    path_name and return the resulting globals.

    Directories and zip files must contain a __main__ module.  run_name
    (default '<run_path>') becomes __name__."""
    if run_name is None:
        run_name = "<run_path>"
    package = run_name.rpartition(".")[0]
    from pkgutil import get_importer
    finder = get_importer(path_name)
    path_name = os.fsdecode(path_name)
    if finder is None:
        code = _get_code_from_file(path_name, run_name)
        return _run_module_code(code, init_globals, run_name,
                                pkg_name=package, script_name=path_name)
    sys.path.insert(0, path_name)
    try:
        _, spec, code = _get_main_module_details()
        with _module_in_sys_modules(run_name) as module, _argv0(path_name):
            namespace = module.__dict__
            _run_code(code, namespace, init_globals, run_name, spec, package)
            return namespace.copy()
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
