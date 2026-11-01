"""Bootstrap the pip installer into the current environment.

The pip wheel bundled in ``_bundled/`` (or, when the platform configured
WHEEL_PKG_DIR, the newest ``pip-*.whl`` found there) is installed by running
pip from that wheel in a child interpreter.
"""

import contextlib
import os
import re
import shutil
import subprocess
import sys
import sysconfig
import tempfile
from importlib import resources
from pathlib import Path

__all__ = ["version", "bootstrap"]

_PIP_VERSION = "26.2.2"


def _wheel_pkg_dir():
    configured = sysconfig.get_config_var('WHEEL_PKG_DIR')
    if not configured:
        return None
    return Path(configured)


# Directory holding distribution-provided wheels (None: use the bundled one).
_WHEEL_PKG_DIR = _wheel_pkg_dir()


def _version_key(text):
    """Sort key for version strings: numeric components compare as
    numbers, anything else as text after them."""
    key = []
    for piece in re.split(r'[.+-]', text):
        match = re.match(r'(\d+)(.*)', piece)
        if match:
            key.append((1, int(match.group(1)), match.group(2)))
        else:
            key.append((0, 0, piece))
    return key


def _find_wheel_pkg_dir_pip():
    """Return (version, path) of the newest pip wheel in _WHEEL_PKG_DIR, or
    None."""
    if _WHEEL_PKG_DIR is None:
        return None
    try:
        names = os.listdir(_WHEEL_PKG_DIR)
    except OSError:
        return None
    candidates = []
    for name in names:
        if not (name.startswith('pip-') and name.endswith('.whl')):
            continue
        wheel_version = name[len('pip-'):].split('-', 1)[0]
        candidates.append((_version_key(wheel_version), wheel_version, name))
    if not candidates:
        return None
    _, wheel_version, name = max(candidates)
    return wheel_version, Path(_WHEEL_PKG_DIR) / name


@contextlib.contextmanager
def _get_pip_whl_path_ctx():
    """Context manager giving the file system path of the pip wheel."""
    found = _find_wheel_pkg_dir_pip()
    if found is not None:
        yield found[1]
        return
    bundled = resources.files('ensurepip') / '_bundled' / f'pip-{_PIP_VERSION}-py3-none-any.whl'
    with resources.as_file(bundled) as path:
        yield path


def version():
    """Return the version of pip that would be installed."""
    found = _find_wheel_pkg_dir_pip()
    if found is not None:
        return found[0]
    return _PIP_VERSION


def _disable_pip_configuration_settings():
    # pip must not be influenced by the user's environment or config file.
    for name in list(os.environ):
        if name.startswith('PIP_'):
            del os.environ[name]
    os.environ['PIP_CONFIG_FILE'] = os.devnull


# Runs in the child interpreter.  Wheels are imported in place when
# zipimport is available; otherwise they are unpacked to a temporary
# directory first (without their metadata, so that the unpacked copy does
# not count as an already installed distribution).
_PIP_RUNNER = """\
import runpy, sys
paths = {paths!r}
try:
    import zipimport
    in_place = zipimport.zipimporter in sys.path_hooks
except ImportError:
    in_place = False
if not in_place:
    import atexit, os, shutil, tempfile, zipfile
    unpacked = []
    for entry in paths:
        if entry.endswith('.whl'):
            target = tempfile.mkdtemp(prefix='ensurepip-')
            atexit.register(shutil.rmtree, target, True)
            with zipfile.ZipFile(entry) as archive:
                archive.extractall(target)
            for name in os.listdir(target):
                if name.endswith('.dist-info'):
                    shutil.rmtree(os.path.join(target, name))
            entry = target
        unpacked.append(entry)
    paths = unpacked
sys.path[:0] = paths
sys.argv[1:] = {args!r}
runpy.run_module("pip", run_name="__main__", alter_sys=True)
"""


def _run_pip(args, additional_paths=None):
    """Run pip with *args* in a child interpreter, with *additional_paths*
    prepended to its sys.path; return its exit status."""
    code = _PIP_RUNNER.format(paths=list(additional_paths or ()), args=list(args))
    command = [
        sys.executable,
        '-W', 'ignore::DeprecationWarning',
        '-c', code,
    ]
    if sys.flags.isolated:
        command.insert(1, '-I')
    return subprocess.run(command, check=True).returncode


def bootstrap(*, root=None, upgrade=False, user=False,
              altinstall=False, default_pip=False,
              verbosity=0):
    """Install pip into the current environment (or below *root*).

    Raise ValueError if both *altinstall* and *default_pip* are true.
    """
    _bootstrap(root=root, upgrade=upgrade, user=user,
               altinstall=altinstall, default_pip=default_pip,
               verbosity=verbosity)


def _bootstrap(*, root=None, upgrade=False, user=False,
               altinstall=False, default_pip=False,
               verbosity=0):
    """Implementation of bootstrap(); returns pip's exit status."""
    if altinstall and default_pip:
        raise ValueError("Cannot use altinstall and default_pip together")

    sys.audit("ensurepip.bootstrap", root)

    try:
        import zlib  # noqa: F401  (pip needs it to read wheels)
    except ImportError:
        raise ModuleNotFoundError(
            "ensurepip requires the standard library module 'zlib' "
            "to install pip."
        ) from None

    _disable_pip_configuration_settings()

    # ENSUREPIP_OPTIONS tells pip which scripts to create:
    #   install    -> pip, pipX and pipX.Y
    #   altinstall -> only pipX.Y
    if altinstall:
        os.environ["ENSUREPIP_OPTIONS"] = "altinstall"
    elif not default_pip:
        os.environ["ENSUREPIP_OPTIONS"] = "install"

    with tempfile.TemporaryDirectory() as tmpdir:
        with _get_pip_whl_path_ctx() as wheel_path:
            local_wheel = Path(tmpdir) / Path(wheel_path).name
            shutil.copyfile(wheel_path, local_wheel)

        args = ["install", "--no-cache-dir", "--no-index", "--find-links", tmpdir]
        if root:
            args += ["--root", root]
        if upgrade:
            args.append("--upgrade")
        if user:
            args.append("--user")
        if verbosity:
            args.append("-" + "v" * verbosity)
        if sys.implementation.cache_tag is None:
            args.append("--no-compile")

        return _run_pip([*args, "pip"], [os.fsdecode(local_wheel)])


def _uninstall_helper(*, verbosity=0):
    """Uninstall pip if it is the version ensurepip would install."""
    try:
        import pip
    except ImportError:
        return

    available = version()
    if pip.__version__ != available:
        print(
            f"ensurepip will only uninstall a matching version "
            f"({pip.__version__!r} installed, {available!r} available)",
            file=sys.stderr,
        )
        return

    _disable_pip_configuration_settings()

    args = ["uninstall", "-y", "--disable-pip-version-check"]
    if verbosity:
        args.append("-" + "v" * verbosity)
    return _run_pip([*args, "pip"])


def _main(argv=None):
    import argparse

    parser = argparse.ArgumentParser(prog="python -m ensurepip")
    parser.add_argument(
        "--version",
        action="version",
        version=f"pip {version()}",
        help="Show the version of pip that is bundled with this Python.",
    )
    parser.add_argument(
        "-v", "--verbose",
        action="count",
        default=0,
        dest="verbosity",
        help="Give more output. Option is additive, and can be used up to 3 times.",
    )
    parser.add_argument(
        "-U", "--upgrade",
        action="store_true",
        default=False,
        help="Upgrade pip and dependencies, even if already installed.",
    )
    parser.add_argument(
        "--user",
        action="store_true",
        default=False,
        help="Install using the user scheme.",
    )
    parser.add_argument(
        "--root",
        default=None,
        help="Install everything relative to this alternate root directory.",
    )
    parser.add_argument(
        "--altinstall",
        action="store_true",
        default=False,
        help="Make an alternate install, installing only the X.Y versioned scripts "
             "(Default: pipX, pipX.Y).",
    )
    parser.add_argument(
        "--default-pip",
        action="store_true",
        default=False,
        help="Make a default pip install, installing the unqualified pip "
             "in addition to the versioned scripts.",
    )

    args = parser.parse_args(argv)

    return _bootstrap(
        root=args.root,
        upgrade=args.upgrade,
        user=args.user,
        verbosity=args.verbosity,
        altinstall=args.altinstall,
        default_pip=args.default_pip,
    )
