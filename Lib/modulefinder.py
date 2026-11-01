"""Find the modules a script imports, without running it.

ModuleFinder loads the source (or bytecode) of a script and of every module
it reaches, follows the import statements found in the compiled code, and
records which modules were found (`modules`) and which imports failed
(`badmodules`).
"""

import dis
import importlib._bootstrap_external
import importlib.machinery
import marshal
import os
import io
import sys

__all__ = ["Module", "ModuleFinder", "AddPackagePath", "ReplacePackage"]

# kinds of module files (the third element of a find_module() description)
_SEARCH_ERROR = 0
_PY_SOURCE = 1
_PY_COMPILED = 2
_C_EXTENSION = 3
_PKG_DIRECTORY = 5
_C_BUILTIN = 6
_PY_FROZEN = 7
_NS_PACKAGE = 8

# extra directories of packages that extend their __path__ at run time
packagePathMap = {}

# packages that install themselves under another name
replacePackageMap = {}


def AddPackagePath(packagename, path):
    packagePathMap.setdefault(packagename, []).append(path)


def ReplacePackage(oldname, newname):
    replacePackageMap[oldname] = newname


class Module:
    """What the finder knows about one module."""

    def __init__(self, name, file=None, path=None):
        self.__name__ = name
        self.__file__ = file
        self.__path__ = path
        self.__code__ = None
        # names bound at module level (they may satisfy "from m import x")
        self.globalnames = {}
        # star imports from modules whose names are unknown
        self.starimports = {}
        self._submodules = {}

    def __repr__(self):
        parts = [repr(self.__name__)]
        if self.__file__ is not None:
            parts.append(repr(self.__file__))
        if self.__path__ is not None:
            if self.__file__ is None:
                parts.append('None')
            parts.append(repr(self.__path__))
        return "Module(%s)" % ", ".join(parts)


class ModuleFinder:

    def __init__(self, path=None, debug=0, excludes=None, replace_paths=None):
        self.path = sys.path if path is None else path
        self.modules = {}
        self.badmodules = {}
        self.debug = debug
        self.indent = 0
        self.excludes = excludes if excludes is not None else []
        self.replace_paths = replace_paths if replace_paths is not None else []
        self.processed_paths = []

    # ---- diagnostics

    def msg(self, level, text, *args):
        if level <= self.debug:
            words = [text] + [repr(arg) for arg in args]
            print("  " * self.indent + " ".join(words))

    def msgin(self, *args):
        if args[0] <= self.debug:
            self.indent += 1
            self.msg(*args)

    def msgout(self, *args):
        if args[0] <= self.debug:
            self.indent -= 1
            self.msg(*args)

    # ---- entry points

    def run_script(self, pathname):
        self.msg(2, "run_script", pathname)
        with io.open_code(pathname) as fp:
            info = ("", "rb", _PY_SOURCE)
            self.load_module('__main__', fp, pathname, info)

    def load_file(self, pathname):
        name = os.path.splitext(os.path.basename(pathname))[0]
        with io.open_code(pathname) as fp:
            info = (".py", "rb", _PY_SOURCE)
            self.load_module(name, fp, pathname, info)

    def import_hook(self, name, caller=None, fromlist=None, level=-1):
        """Import `name` (relative to `caller` for level > 0) and the names
        of `fromlist`; raises ImportError when something is missing."""
        self.msg(3, "import_hook", name, caller, fromlist, level)
        fullname = self._absolute_name(name, caller, level)
        module = self._import_dotted(fullname)
        if fromlist and module.__path__ is not None:
            self._import_fromlist(module, fromlist)
        return module

    # ---- name resolution

    def _absolute_name(self, name, caller, level):
        if level is None or level <= 0:
            return name
        if caller is None:
            raise ImportError("relative import outside of a package")
        if caller.__path__ is not None:
            base = caller.__name__
        else:
            base = caller.__name__.rpartition('.')[0]
        for _ in range(level - 1):
            if '.' not in base:
                raise ImportError("relative importpath too deep")
            base = base.rpartition('.')[0]
        if not base:
            raise ImportError("relative importpath too deep")
        return base + '.' + name if name else base

    def _import_dotted(self, fullname):
        parent = None
        parts = fullname.split('.')
        for index, part in enumerate(parts):
            qualified = '.'.join(parts[:index + 1])
            module = self.import_module(part, qualified, parent)
            if module is None:
                raise ImportError("No module named " + qualified)
            parent = module
        return parent

    def _import_fromlist(self, package, fromlist, recursive=False):
        for sub in fromlist:
            if sub == '*':
                if not recursive:
                    self._import_fromlist(package,
                                          sorted(self.find_all_submodules(package)),
                                          True)
                continue
            if sub in package._submodules:
                continue
            qualified = package.__name__ + '.' + sub
            if self.import_module(sub, qualified, package) is None:
                raise ImportError("No module named " + qualified)

    def find_all_submodules(self, m):
        """Names of the modules found in the directories of package `m`."""
        if not m.__path__:
            return set()
        suffixes = sorted(importlib.machinery.all_suffixes(), key=len,
                          reverse=True)
        found = set()
        for directory in m.__path__:
            try:
                names = os.listdir(directory)
            except OSError:
                self.msg(2, "can't list directory", directory)
                continue
            for filename in names:
                for suffix in suffixes:
                    if filename.endswith(suffix):
                        stem = filename[:-len(suffix)]
                        if stem and stem != "__init__" and stem.isidentifier():
                            found.add(stem)
                        break
        return found

    def import_module(self, partname, fqname, parent):
        """The Module for `fqname` (loading it if needed), or None."""
        self.msgin(3, "import_module", partname, fqname, parent)
        module = self.modules.get(fqname)
        if module is not None:
            self.msgout(3, "import_module ->", module)
            return module
        if fqname in self.badmodules:
            self.msgout(3, "import_module -> None")
            return None
        if parent is not None and parent.__path__ is None:
            self.msgout(3, "import_module -> None")
            return None
        try:
            fp, pathname, info = self.find_module(
                partname, parent.__path__ if parent is not None else None, parent)
        except ImportError:
            self.msgout(3, "import_module ->", None)
            return None
        try:
            module = self.load_module(fqname, fp, pathname, info)
        finally:
            if fp:
                fp.close()
        if parent is not None:
            parent._submodules[partname] = module
        self.msgout(3, "import_module ->", module)
        return module

    # ---- loading

    def load_module(self, fqname, fp, pathname, file_info):
        suffix, mode, kind = file_info
        self.msgin(2, "load_module", fqname, fp and "fp", pathname)
        if kind == _PKG_DIRECTORY:
            module = self.load_package(fqname, pathname)
            self.msgout(2, "load_module ->", module)
            return module
        if kind == _NS_PACKAGE:
            module = self.add_module(fqname)
            module.__path__ = list(pathname)
            self.msgout(2, "load_module ->", module)
            return module
        code = None
        if kind == _PY_SOURCE:
            source = fp.read()
            code = compile(source + b'\n' if isinstance(source, bytes) else source + '\n',
                           pathname, 'exec')
        elif kind == _PY_COMPILED:
            code = self._read_pyc(fp, fqname, pathname)
        module = self.add_module(fqname)
        module.__file__ = pathname
        if code is not None:
            if self.replace_paths:
                code = self.replace_paths_in_code(code)
            module.__code__ = code
            self.scan_code(code, module)
        self.msgout(2, "load_module ->", module)
        return module

    @staticmethod
    def _read_pyc(fp, fqname, pathname):
        data = fp.read()
        try:
            importlib._bootstrap_external._classify_pyc(
                data, fqname, {'name': fqname, 'path': pathname})
        except ImportError as exc:
            raise ImportError("Bad magic number in %s" % pathname) from exc
        except EOFError as exc:
            raise ImportError("Truncated bytecode in %s" % pathname) from exc
        return marshal.loads(memoryview(data)[16:])

    def load_package(self, fqname, pathname):
        self.msgin(2, "load_package", fqname, pathname)
        newname = replacePackageMap.get(fqname)
        if newname:
            fqname = newname
        module = self.add_module(fqname)
        module.__file__ = pathname
        module.__path__ = [pathname] + packagePathMap.get(fqname, [])
        fp, initfile, info = self.find_module("__init__", module.__path__)
        try:
            self.load_module(fqname, fp, initfile, info)
        finally:
            if fp:
                fp.close()
        self.msgout(2, "load_package ->", module)
        return module

    def add_module(self, fqname):
        module = self.modules.get(fqname)
        if module is None:
            module = self.modules[fqname] = Module(fqname)
        return module

    def find_module(self, name, path, parent=None):
        """Locate module `name` in the directories `path` (None: top level).
        Returns (open file or None, pathname, (suffix, mode, kind))."""
        fullname = parent.__name__ + '.' + name if parent is not None else name
        if fullname in self.excludes:
            self.msgout(3, "find_module -> Excluded", fullname)
            raise ImportError(name)
        if path is None:
            if name in sys.builtin_module_names:
                return (None, None, ("", "", _C_BUILTIN))
            path = self.path
        spec = importlib.machinery.PathFinder.find_spec(name, path)
        if spec is None:
            raise ImportError("No module named {name!r}".format(name=name), name=name)
        loader = spec.loader
        origin = spec.origin
        if origin is None or loader is None:
            # a namespace package: just a list of directories
            locations = spec.submodule_search_locations
            if locations is None:
                raise ImportError("No module named {name!r}".format(name=name), name=name)
            return (None, list(locations), ("", "", _NS_PACKAGE))
        if spec.submodule_search_locations is not None and os.path.basename(origin).startswith('__init__'):
            return (None, os.path.dirname(origin), ("", "", _PKG_DIRECTORY))
        if isinstance(loader, importlib.machinery.SourceFileLoader):
            kind, mode = _PY_SOURCE, "rb"
        elif isinstance(loader, importlib.machinery.SourcelessFileLoader):
            kind, mode = _PY_COMPILED, "rb"
        elif isinstance(loader, importlib.machinery.ExtensionFileLoader):
            return (None, origin, (os.path.splitext(origin)[1], "rb", _C_EXTENSION))
        else:
            return (None, origin, ("", "", _SEARCH_ERROR))
        suffix = os.path.splitext(origin)[1]
        return (io.open_code(origin), origin, (suffix, mode, kind))

    # ---- scanning compiled code

    @staticmethod
    def _imports_in(code):
        """Yield ('store', name), ('import', name, fromlist, level) or
        ('star', name, level) for the code object (not nested ones)."""
        recent = []
        for instruction in dis.get_instructions(code):
            op = instruction.opname
            if op == 'IMPORT_NAME':
                level = recent[-2] if len(recent) >= 2 else 0
                fromlist = recent[-1] if recent else None
                name = instruction.argval
                if fromlist is not None and '*' in fromlist:
                    yield ('star', name, level)
                else:
                    yield ('import', name, fromlist, level)
            elif op in ('STORE_NAME', 'STORE_GLOBAL'):
                yield ('store', instruction.argval)
            if op in ('LOAD_CONST', 'LOAD_SMALL_INT'):
                recent.append(instruction.argval)
            elif op != 'EXTENDED_ARG':
                recent = []

    def scan_code(self, co, m):
        for item in self._imports_in(co):
            kind = item[0]
            if kind == 'store':
                m.globalnames[item[1]] = 1
            elif kind == 'import':
                _, name, fromlist, level = item
                self._safe_import_hook(name, m, list(fromlist) if fromlist else None,
                                       level=level)
            else:
                _, name, level = item
                self._star_import(name, m, level)
        for const in co.co_consts:
            if isinstance(const, type(co)):
                self.scan_code(const, m)

    def _star_import(self, name, m, level):
        self._safe_import_hook(name, m, ['*'], level=level)
        try:
            fullname = self._absolute_name(name, m, level)
        except ImportError:
            fullname = None
        source = self.modules.get(fullname) if fullname else None
        if source is None or source.__code__ is None:
            # builtin/extension module or a failed import: names unknown
            m.starimports[name] = 1
        else:
            m.globalnames.update(source.globalnames)
            m.starimports.update(source.starimports)

    def _add_badmodule(self, name, caller):
        callers = self.badmodules.setdefault(name, {})
        callers[caller.__name__ if caller is not None else "-"] = 1

    def _safe_import_hook(self, name, caller, fromlist, level=-1):
        try:
            fullname = self._absolute_name(name, caller, level)
        except ImportError as exc:
            self.msg(2, "ImportError:", str(exc))
            self._add_badmodule(name, caller)
            return
        if fullname in self.badmodules:
            self._add_badmodule(fullname, caller)
            return
        try:
            self.import_hook(name, caller, level=level)
        except (ImportError, SyntaxError) as exc:
            self.msg(2, "ImportError:", str(exc))
            self._add_badmodule(fullname, caller)
            return
        for sub in fromlist or ():
            if sub == '*':
                try:
                    self.import_hook(name, caller, ['*'], level=level)
                except (ImportError, SyntaxError) as exc:
                    self.msg(2, "ImportError:", str(exc))
                continue
            subname = fullname + '.' + sub
            if subname in self.badmodules:
                self._add_badmodule(subname, caller)
                continue
            try:
                self.import_hook(name, caller, [sub], level=level)
            except (ImportError, SyntaxError) as exc:
                self.msg(2, "ImportError:", str(exc))
                self._add_badmodule(subname, caller)

    # ---- results

    def any_missing(self):
        """Names of modules that could not be imported (and those that may
        only be missing; see any_missing_maybe())."""
        missing, maybe = self.any_missing_maybe()
        return missing + maybe

    def any_missing_maybe(self):
        """(missing, maybe): imports that certainly failed, and failed
        'from package import name' imports where `name` might be a global
        of the package that cannot be checked (star import from an
        extension module)."""
        missing = []
        maybe = []
        for name, callers in self.badmodules.items():
            if name in self.excludes:
                continue
            package_name, dot, sub = name.rpartition('.')
            package = self.modules.get(package_name) if dot else None
            if package is None or package_name in callers:
                missing.append(name)
            elif sub in package.globalnames:
                continue
            elif package.starimports:
                maybe.append(name)
            else:
                missing.append(name)
        return sorted(missing), sorted(maybe)

    def report(self):
        """Print the modules found and the missing ones."""
        print()
        print("  %-25s %s" % ("Name", "File"))
        print("  %-25s %s" % ("----", "----"))
        for name in sorted(self.modules):
            module = self.modules[name]
            tag = "P" if module.__path__ is not None else "m"
            print(tag, "%-25s" % name, module.__file__ or "")
        missing, maybe = self.any_missing_maybe()
        if missing:
            print()
            print("Missing modules:")
            for name in missing:
                print("?", name, "imported from",
                      ', '.join(sorted(self.badmodules[name])))
        if maybe:
            print()
            print("Submodules that appear to be missing, but could also be "
                  "global names in the parent package:")
            for name in maybe:
                print("?", name, "imported from",
                      ', '.join(sorted(self.badmodules[name])))

    def replace_paths_in_code(self, co):
        original = os.path.normpath(co.co_filename)
        new_filename = original
        for old, new in self.replace_paths:
            if original.startswith(old):
                new_filename = new + original[len(old):]
                break
        if self.debug and original not in self.processed_paths:
            if new_filename != original:
                self.msgout(2, "co_filename %r changed to %r" % (original, new_filename))
            else:
                self.msgout(2, "co_filename %r remains unchanged" % (original,))
            self.processed_paths.append(original)
        consts = tuple(self.replace_paths_in_code(c) if isinstance(c, type(co)) else c
                       for c in co.co_consts)
        return co.replace(co_consts=consts, co_filename=new_filename)


def test():
    import getopt
    try:
        opts, args = getopt.getopt(sys.argv[1:], "dmp:qx:")
    except getopt.error as msg:
        print(msg)
        return
    debug = 1
    domods = 0
    addpath = []
    exclude = []
    for option, value in opts:
        if option == '-d':
            debug += 1
        elif option == '-m':
            domods = 1
        elif option == '-p':
            addpath += value.split(os.pathsep)
        elif option == '-q':
            debug = 0
        elif option == '-x':
            exclude.append(value)
    script = args[0] if args else "hello.py"
    path = sys.path[:]
    path[0] = os.path.dirname(script)
    path = addpath + path
    if debug > 1:
        print("path:")
        for item in path:
            print("   ", repr(item))
    finder = ModuleFinder(path, debug, exclude)
    for arg in args[1:]:
        if arg == '-m':
            domods = 1
            continue
        if domods:
            if arg.endswith('.*'):
                finder.import_hook(arg[:-2], None, ["*"])
            else:
                finder.import_hook(arg)
        else:
            finder.load_file(arg)
    finder.run_script(script)
    finder.report()
    return finder


if __name__ == '__main__':
    try:
        test()
    except KeyboardInterrupt:
        print("\n[interrupted]")
