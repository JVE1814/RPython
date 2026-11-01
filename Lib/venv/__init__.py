"""Create lightweight "virtual environments" with their own site-packages.

A virtual environment is a directory holding a ``pyvenv.cfg`` file, a copy
of (or a symlink to) the interpreter in ``bin/``, activation scripts and an
empty ``lib/pythonX.Yt/site-packages``.  The interpreter notices the
``pyvenv.cfg`` next to it at start-up and uses the environment's directory
as ``sys.prefix``.

    python -m venv [options] ENV_DIR [ENV_DIR ...]
"""

import logging
import os
import shlex
import shutil
import subprocess
import sys
import sysconfig
import types

__all__ = ['EnvBuilder', 'create', 'main']

# Packages upgraded by --upgrade-deps.
CORE_VENV_DEPS = ('pip',)

logger = logging.getLogger(__name__)

_GITIGNORE_TEXT = (
    '# Created by venv; see https://docs.python.org/3/library/venv.html\n'
    '*\n'
)


class EnvBuilder:
    """Builds virtual environments.

    The building steps are separate methods so that subclasses can adapt
    them; ``post_setup(context)`` is a hook called after everything else.

    :param system_site_packages: let the environment see the base
        installation's site-packages.
    :param clear: delete the contents of an existing environment directory.
    :param symlinks: symlink the interpreter rather than copying it.
    :param upgrade: upgrade an existing environment to this interpreter
        (re-creates the interpreter files only).
    :param with_pip: install pip with ensurepip.
    :param prompt: the prompt prefix shown when activated ('.' means the
        name of the current directory).
    :param upgrade_deps: upgrade pip to the latest release from PyPI.
    :param scm_ignore_files: names of SCMs (e.g. {'git'}) for which an
        ignore file is created; ``create_<name>_ignore_file`` is called.
    """

    def __init__(self, system_site_packages=False, clear=False,
                 symlinks=False, upgrade=False, with_pip=False, prompt=None,
                 upgrade_deps=False, *, scm_ignore_files=frozenset()):
        self.system_site_packages = system_site_packages
        self.clear = clear
        self.symlinks = symlinks
        self.upgrade = upgrade
        self.with_pip = with_pip
        self.orig_prompt = prompt
        if prompt == '.':
            prompt = os.path.basename(os.getcwd())
        self.prompt = prompt
        self.upgrade_deps = upgrade_deps
        self.scm_ignore_files = frozenset(name.lower() for name in scm_ignore_files)

    # -- the overall procedure ---------------------------------------------

    def create(self, env_dir):
        """Create (or upgrade) a virtual environment in *env_dir*."""
        env_dir = os.path.abspath(env_dir)
        context = self.ensure_directories(env_dir)
        for scm in sorted(self.scm_ignore_files):
            getattr(self, f'create_{scm}_ignore_file')(context)

        # While pip is being set up the environment must not see the base
        # installation's packages; the final configuration is written after.
        wants_system_site = self.system_site_packages
        self.system_site_packages = False
        try:
            self.create_configuration(context)
            self.setup_python(context)
            if self.with_pip:
                self._setup_pip(context)
            if not self.upgrade:
                self.setup_scripts(context)
                self.post_setup(context)
        finally:
            self.system_site_packages = wants_system_site
        if wants_system_site:
            self.create_configuration(context)
        if self.upgrade_deps:
            self.upgrade_dependencies(context)

    # -- directories -------------------------------------------------------

    def clear_directory(self, path):
        """Remove everything inside *path* (but not *path* itself)."""
        for name in os.listdir(path):
            full = os.path.join(path, name)
            if os.path.islink(full) or os.path.isfile(full):
                os.remove(full)
            elif os.path.isdir(full):
                shutil.rmtree(full)

    def _venv_path(self, env_dir, name):
        base = {
            'base': env_dir,
            'platbase': env_dir,
            'installed_base': env_dir,
            'installed_platbase': env_dir,
        }
        return sysconfig.get_path(name, scheme='venv', vars=base)

    @classmethod
    def _same_path(cls, path1, path2):
        """True if two paths name the same location (on POSIX: are equal
        strings; Windows also ignores case and 8.3 short names)."""
        if sys.platform != 'win32':
            return path1 == path2
        if os.path.normcase(path1) == os.path.normcase(path2):
            return True
        try:
            import _winapi
            long1 = _winapi.GetLongPathName(path1)
            long2 = _winapi.GetLongPathName(path2)
        except (ImportError, OSError):
            return False
        return os.path.normcase(long1) == os.path.normcase(long2)

    def ensure_directories(self, env_dir):
        """Create the environment's directories (as needed) and return a
        context object describing the environment."""

        def make_dir(path):
            if not os.path.exists(path):
                os.makedirs(path)
            elif os.path.islink(path) or os.path.isfile(path):
                raise ValueError(f'Unable to create directory {path!r}')

        env_dir = os.fspath(env_dir)
        if os.pathsep in env_dir:
            raise ValueError(
                f'Refusing to create a venv in {env_dir} because it contains '
                f'the PATH separator {os.pathsep}.'
            )
        if os.path.exists(env_dir) and self.clear:
            self.clear_directory(env_dir)

        context = types.SimpleNamespace()
        context.env_dir = env_dir
        context.env_name = os.path.split(env_dir)[1]
        context.prompt = self.prompt if self.prompt is not None else context.env_name
        make_dir(env_dir)

        executable = sys._base_executable
        if not executable:
            raise ValueError(
                'Unable to determine path to the running Python interpreter. '
                'Provide an explicit path or check that your PATH environment '
                'variable is correctly set.'
            )
        python_dir, python_exe = os.path.split(os.path.abspath(executable))
        context.executable = executable
        context.python_dir = python_dir
        context.python_exe = python_exe

        bin_path = self._venv_path(env_dir, 'scripts')
        inc_path = self._venv_path(env_dir, 'include')
        lib_path = self._venv_path(env_dir, 'purelib')
        context.inc_path = inc_path
        context.lib_path = lib_path
        make_dir(inc_path)
        make_dir(lib_path)

        context.bin_path = bin_path
        context.bin_name = os.path.relpath(bin_path, env_dir)
        context.env_exe = os.path.join(bin_path, python_exe)
        make_dir(bin_path)
        # The command to run the environment's interpreter.
        context.env_exec_cmd = context.env_exe
        if sys.platform == 'win32':
            real_env_exe = os.path.realpath(context.env_exe)
            if not self._same_path(real_env_exe, context.env_exe):
                logger.warning(
                    'Actual environment location may have moved due to '
                    'redirects, links or junctions.\n'
                    '  Requested location: "%s"\n'
                    '  Actual location:    "%s"',
                    context.env_exe, real_env_exe,
                )
                context.env_exec_cmd = real_env_exe
        context.cfg_path = os.path.join(env_dir, 'pyvenv.cfg')
        return context

    # -- configuration -----------------------------------------------------

    def _creation_command(self, context):
        options = []
        if os.name == 'nt':
            if self.symlinks:
                options.append('--symlinks')
        elif not self.symlinks:
            options.append('--copies')
        if not self.with_pip:
            options.append('--without-pip')
        if self.system_site_packages:
            options.append('--system-site-packages')
        if self.clear:
            options.append('--clear')
        if self.upgrade:
            options.append('--upgrade')
        if self.upgrade_deps:
            options.append('--upgrade-deps')
        if self.orig_prompt is not None:
            options.append(f'--prompt="{self.orig_prompt}"')
        if not self.scm_ignore_files:
            options.append('--without-scm-ignore-files')
        options.append(context.env_dir)
        return f'{sys.executable} -m venv {" ".join(options)}'

    def create_configuration(self, context):
        """Write the environment's pyvenv.cfg."""
        include_system = 'true' if self.system_site_packages else 'false'
        version = '%d.%d.%d' % sys.version_info[:3]
        lines = [
            f'home = {context.python_dir}',
            f'include-system-site-packages = {include_system}',
            f'version = {version}',
        ]
        if self.prompt is not None:
            lines.append(f'prompt = {self.prompt!r}')
        lines.append(f'executable = {os.path.realpath(sys.executable)}')
        lines.append(f'command = {self._creation_command(context)}')
        with open(context.cfg_path, 'w', encoding='utf-8') as cfg:
            cfg.write(''.join(line + '\n' for line in lines))

    def create_git_ignore_file(self, context):
        """Make git ignore the whole environment directory."""
        path = os.path.join(context.env_dir, '.gitignore')
        with open(path, 'w', encoding='utf-8') as f:
            f.write(_GITIGNORE_TEXT)

    # -- the interpreter ---------------------------------------------------

    def symlink_or_copy(self, src, dst, relative_symlinks_ok=False):
        """Symlink *src* to *dst* if symlinks were requested (or for
        same-directory aliases when *relative_symlinks_ok*), otherwise copy;
        fall back to copying if symlinking fails."""
        if self.symlinks or relative_symlinks_ok:
            if relative_symlinks_ok and os.path.dirname(src) == os.path.dirname(dst):
                target = os.path.basename(src)
            else:
                target = src
            try:
                os.symlink(target, dst)
                return
            except Exception:
                logger.warning('Unable to symlink %r to %r', src, dst)
        shutil.copyfile(src, dst)

    def _executable_names(self, context):
        major, minor = sys.version_info[:2]
        names = ['python', f'python{major}', f'python{major}.{minor}']
        if 't' in getattr(sys, 'abiflags', ''):
            names.append(f'python{major}.{minor}t')
        return [name for name in names if name != context.python_exe]

    def setup_python(self, context):
        """Put the interpreter (and its usual aliases) into bin/."""
        if sys.platform == 'win32':
            return self._setup_python_windows(context)
        env_exe = context.env_exe
        if os.path.lexists(env_exe):
            os.unlink(env_exe)
        self.symlink_or_copy(context.executable, env_exe)
        if not os.path.islink(env_exe):
            os.chmod(env_exe, 0o755)
        for name in self._executable_names(context):
            alias = os.path.join(context.bin_path, name)
            if not os.path.lexists(alias):
                self.symlink_or_copy(env_exe, alias, relative_symlinks_ok=True)
                if not os.path.islink(alias):
                    os.chmod(alias, 0o755)

    def _setup_python_windows(self, context):
        # Only reached on Windows; copy the interpreter and its DLLs.
        env_exe = context.env_exe
        self.symlink_or_copy(context.executable, env_exe)
        for name in os.listdir(context.python_dir):
            if name.lower().endswith(('.dll', '.pyd')):
                src = os.path.join(context.python_dir, name)
                dst = os.path.join(context.bin_path, name)
                if not os.path.exists(dst):
                    self.symlink_or_copy(src, dst)

    def _call_new_python(self, context, *py_args, **kwargs):
        """Run the environment's interpreter with *py_args*, isolated from
        PYTHONHOME/PYTHONPATH and with VIRTUAL_ENV set."""
        args = [context.env_exec_cmd, *py_args]
        env = os.environ.copy()
        env['VIRTUAL_ENV'] = context.env_dir
        env.pop('PYTHONHOME', None)
        env.pop('PYTHONPATH', None)
        kwargs['env'] = env
        kwargs['cwd'] = context.env_dir
        kwargs['executable'] = context.env_exec_cmd
        subprocess.check_output(args, **kwargs)

    def _setup_pip(self, context):
        """Install pip into the environment with ensurepip."""
        self._call_new_python(
            context, '-m', 'ensurepip', '--upgrade', '--default-pip',
            stderr=subprocess.STDOUT,
        )

    # -- activation scripts ------------------------------------------------

    def setup_scripts(self, context):
        """Install the activation scripts shipped in venv/scripts."""
        here = os.path.dirname(os.path.abspath(__file__))
        self.install_scripts(context, os.path.join(here, 'scripts'))

    def post_setup(self, context):
        """Hook for subclasses; called after the environment is ready."""

    @staticmethod
    def _quoter_for(script_name):
        """Return the function quoting substituted values for the script
        language of *script_name*."""
        lowered = script_name.lower()
        if lowered.endswith('.ps1'):
            return lambda s: "'" + s.replace("'", "''") + "'"
        if lowered.endswith('.bat'):
            return lambda s: s
        if lowered.endswith('.fish'):
            return lambda s: "'" + s.replace('\\', '\\\\').replace("'", "\\'") + "'"
        if lowered.endswith('.csh'):
            # tcsh: single quotes, with '!' and newlines escaped.
            return lambda s: "'" + s.replace("'", "'\\''").replace('!', '\\!') + "'"
        return shlex.quote

    def replace_variables(self, text, context, quote=None):
        """Substitute the __VENV_*__ placeholders in a script template.

        Values are quoted with *quote* (default: suitable for a POSIX
        shell) so that unusual characters in paths are safe.
        """
        if quote is None:
            quote = shlex.quote
        replacements = {
            '__VENV_DIR__': context.env_dir,
            '__VENV_NAME__': context.env_name,
            '__VENV_PROMPT__': context.prompt,
            '__VENV_BIN_NAME__': context.bin_name,
            '__VENV_PYTHON__': context.env_exe,
        }
        for placeholder, value in replacements.items():
            text = text.replace(placeholder, quote(value))
        return text

    def install_scripts(self, context, path):
        """Copy the script templates in *path*/common and *path*/<os.name>
        into the environment's bin directory, substituting placeholders.

        Binary files are copied unchanged.  File metadata (including
        extended attributes) is not copied.
        """
        bin_path = context.bin_path
        for root, dirs, files in os.walk(path):
            dirs.sort()
            if root == path:
                # Only descend into 'common' and the current platform's dir.
                dirs[:] = [d for d in dirs if d in ('common', os.name)]
                continue
            relative = os.path.relpath(root, path)
            parts = relative.split(os.sep)
            target_dir = os.path.join(bin_path, *parts[1:])
            if not os.path.exists(target_dir):
                os.makedirs(target_dir)
            for name in sorted(files):
                if name.endswith(('.pyc', '.pyo')) or name == '__init__.py':
                    continue
                source = os.path.join(root, name)
                target = os.path.join(target_dir, name)
                with open(source, 'rb') as f:
                    data = f.read()
                if not name.endswith(('.exe', '.pdb')):
                    try:
                        text = data.decode('utf-8')
                    except UnicodeDecodeError:
                        logger.warning('unable to copy script %r, may be binary', source)
                        continue
                    text = self.replace_variables(text, context, self._quoter_for(name))
                    if name.endswith('.bat'):
                        text = text.replace('\r\n', '\n').replace('\n', '\r\n')
                    data = text.encode('utf-8')
                with open(target, 'wb') as f:
                    f.write(data)

    # -- dependency upgrade ------------------------------------------------

    def upgrade_dependencies(self, context):
        """Upgrade the core packages (pip) to their latest versions."""
        logger.debug('Upgrading %s packages in %s', CORE_VENV_DEPS, context.bin_path)
        self._call_new_python(context, '-m', 'pip', 'install', '--upgrade', *CORE_VENV_DEPS)


def create(env_dir, system_site_packages=False, clear=False,
           symlinks=False, with_pip=False, prompt=None, upgrade_deps=False,
           *, scm_ignore_files=frozenset()):
    """Create a virtual environment in *env_dir* (see EnvBuilder)."""
    builder = EnvBuilder(system_site_packages=system_site_packages,
                         clear=clear, symlinks=symlinks, with_pip=with_pip,
                         prompt=prompt, upgrade_deps=upgrade_deps,
                         scm_ignore_files=scm_ignore_files)
    builder.create(env_dir)


def main(args=None):
    """Command line interface: python -m venv [options] ENV_DIR ..."""
    import argparse

    parser = argparse.ArgumentParser(
        prog=__name__,
        description='Creates virtual Python environments in one or more '
                    'target directories.',
        epilog='Once an environment has been created, you may wish to '
               'activate it, e.g. by sourcing an activate script in its '
               'bin directory.',
    )
    parser.add_argument('dirs', metavar='ENV_DIR', nargs='+',
                        help='A directory to create the environment in.')
    parser.add_argument('--system-site-packages', default=False,
                        action='store_true', dest='system_site',
                        help='Give the virtual environment access to the '
                             'system site-packages dir.')
    use_symlinks = os.name != 'nt'
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--symlinks', default=use_symlinks,
                       action='store_true', dest='symlinks',
                       help='Try to use symlinks rather than copies, '
                            'when symlinks are not the default for '
                            'the platform.')
    group.add_argument('--copies', default=not use_symlinks,
                       action='store_false', dest='symlinks',
                       help='Try to use copies rather than symlinks, '
                            'even when symlinks are the default for '
                            'the platform.')
    parser.add_argument('--clear', default=False, action='store_true',
                        dest='clear',
                        help='Delete the contents of the environment '
                             'directory if it already exists, before '
                             'environment creation.')
    parser.add_argument('--upgrade', default=False, action='store_true',
                        dest='upgrade',
                        help='Upgrade the environment directory to use this '
                             'version of Python, assuming Python has been '
                             'upgraded in-place.')
    parser.add_argument('--without-pip', dest='with_pip',
                        default=True, action='store_false',
                        help='Skips installing or upgrading pip in the '
                             'virtual environment (pip is bootstrapped '
                             'by default)')
    parser.add_argument('--prompt',
                        help='Provides an alternative prompt prefix for '
                             'this environment.')
    parser.add_argument('--upgrade-deps', default=False, action='store_true',
                        dest='upgrade_deps',
                        help=f'Upgrade core dependencies ({", ".join(CORE_VENV_DEPS)}) '
                             'to the latest version in PyPI')
    parser.add_argument('--without-scm-ignore-files', dest='scm_ignore_files',
                        action='store_const', const=frozenset(),
                        default=frozenset(['git']),
                        help='Skips adding SCM ignore files to the environment '
                             'directory (Git is supported by default).')
    options = parser.parse_args(args)
    if options.upgrade and options.clear:
        raise ValueError('you cannot supply --upgrade and --clear together.')
    builder = EnvBuilder(system_site_packages=options.system_site,
                         clear=options.clear,
                         symlinks=options.symlinks,
                         upgrade=options.upgrade,
                         with_pip=options.with_pip,
                         prompt=options.prompt,
                         upgrade_deps=options.upgrade_deps,
                         scm_ignore_files=options.scm_ignore_files)
    for d in options.dirs:
        builder.create(d)
