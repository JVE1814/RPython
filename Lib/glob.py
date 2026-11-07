"""Unix shell-style pathname expansion.

glob() and iglob() expand a pattern that may contain the wildcards '*',
'?' and '[...]' in any of its path components (and, with recursive=True,
'**' for any number of nested directories) into the existing paths it
matches.  translate() turns such a pattern into a regular expression and
escape() makes a literal path safe to use as a pattern.
"""

import fnmatch
import os
import re
import stat
import sys

__all__ = ["glob", "iglob", "escape", "translate"]


def glob(pathname, *, root_dir=None, dir_fd=None, recursive=False,
         include_hidden=False):
    """Return the list of paths matching the pattern *pathname*.

    Results are unordered and may include broken symlinks.  Relative
    patterns are resolved against *root_dir* (default: the current
    directory) and, when given, the directory file descriptor *dir_fd*;
    the results are then relative too.  With *recursive*, a '**' component
    matches any number of nested directories (including none).  Names
    starting with a dot only match wildcards when *include_hidden* is true
    (or when the pattern component itself starts with a dot).
    """
    return list(iglob(pathname, root_dir=root_dir, dir_fd=dir_fd,
                      recursive=recursive, include_hidden=include_hidden))


def iglob(pathname, *, root_dir=None, dir_fd=None, recursive=False,
          include_hidden=False):
    """Like glob(), but return an iterator over the matching paths."""
    sys.audit("glob.glob", pathname, recursive)
    sys.audit("glob.glob/2", pathname, recursive, root_dir, dir_fd)
    if root_dir is not None:
        root_dir = os.fspath(root_dir)
    expander = _Expander(root_dir, dir_fd, recursive, include_hidden)
    # An empty result would only stand for "the directory we started in"
    # (for '' or a leading '**'); it is not reported.
    return filter(None, expander.expand(pathname, dirs_only=False))


def escape(pathname):
    """Return *pathname* with the wildcard characters made literal.

    A drive or UNC prefix is left alone (its '?' is not a wildcard).
    """
    drive, rest = os.path.splitdrive(pathname)
    if isinstance(pathname, bytes):
        rest = _MAGIC_BYTES.sub(rb'[\1]', rest)
    else:
        rest = _MAGIC.sub(r'[\1]', rest)
    return drive + rest


def has_magic(s):
    """True if *s* contains a wildcard character."""
    if isinstance(s, bytes):
        return _MAGIC_BYTES.search(s) is not None
    return _MAGIC.search(s) is not None


_MAGIC = re.compile('([*?[])')
_MAGIC_BYTES = re.compile(b'([*?[])')


def _is_double_star(component):
    return component in ('**', b'**')


def _is_hidden(name):
    return name[:1] in ('.', b'.')


class _Expander:
    """Walks the file system for one glob()/iglob() call."""

    def __init__(self, root_dir, dir_fd, recursive, include_hidden):
        self.root_dir = root_dir
        self.dir_fd = dir_fd
        self.recursive = recursive
        self.include_hidden = include_hidden

    # -- file system access, relative to root_dir and dir_fd --------------

    def _real(self, path):
        if self.root_dir is None:
            return path
        return os.path.join(self.root_dir, path)

    def _lexists(self, path):
        path = self._real(path)
        if self.dir_fd is None:
            return os.path.lexists(path)
        try:
            os.lstat(path, dir_fd=self.dir_fd)
        except (OSError, ValueError):
            return False
        return True

    def _isdir(self, path):
        path = self._real(path)
        if self.dir_fd is None:
            return os.path.isdir(path)
        try:
            mode = os.stat(path, dir_fd=self.dir_fd).st_mode
        except (OSError, ValueError):
            return False
        return stat.S_ISDIR(mode)

    def _names(self, dirname, dirs_only):
        """Entries of *dirname* (all read before returning, so no directory
        stays open while results are consumed), as (name, is_dir)."""
        path = self._real(dirname)
        want_bytes = isinstance(dirname, bytes)
        if not path:
            path = os.curdir.encode() if want_bytes else os.curdir
        entries = []
        try:
            if self.dir_fd is None:
                with os.scandir(path) as it:
                    for entry in it:
                        entries.append(self._describe(entry, dirs_only))
            else:
                fd = os.open(path, _DIR_OPEN_FLAGS, dir_fd=self.dir_fd)
                try:
                    with os.scandir(fd) as it:
                        for entry in it:
                            name, is_dir = self._describe(entry, dirs_only)
                            if want_bytes:
                                name = os.fsencode(name)
                            entries.append((name, is_dir))
                finally:
                    os.close(fd)
        except OSError:
            return []
        if dirs_only:
            entries = [item for item in entries if item[1]]
        return entries

    @staticmethod
    def _describe(entry, dirs_only):
        try:
            is_dir = entry.is_dir()
        except OSError:
            is_dir = False
        return entry.name, is_dir

    # -- pattern expansion -------------------------------------------------

    def expand(self, pattern, dirs_only):
        """Yield the paths matching *pattern*; with *dirs_only*, only
        directories."""
        head, tail = os.path.split(pattern)
        if not has_magic(os.path.splitdrive(pattern)[1]):
            # No wildcards at all: a plain existence test.
            if tail:
                if self._isdir(pattern) if dirs_only else self._lexists(pattern):
                    yield pattern
            elif self._isdir(head):
                # A pattern ending in a separator names a directory.
                yield pattern
            return

        if not head:
            parents = (pattern[:0],)
        elif head != pattern and has_magic(head):
            # Wildcards in the directory part: expand it first (only
            # directories can contain the final component).
            parents = self.expand(head, dirs_only=True)
        else:
            parents = (head,)

        if not has_magic(tail):
            matches = self._literal_child
        elif self.recursive and _is_double_star(tail):
            matches = self._descendants
        else:
            matches = self._matching_children
        for parent in parents:
            for name in matches(parent, tail, dirs_only):
                yield os.path.join(parent, name)

    def _literal_child(self, parent, name, dirs_only):
        if not name:
            # trailing separator: the parent itself, if it is a directory
            if self._isdir(parent):
                yield name
        elif (self._isdir if dirs_only else self._lexists)(
                os.path.join(parent, name)):
            yield name

    def _matching_children(self, parent, pattern, dirs_only):
        entries = self._names(parent, dirs_only)
        names = [name for name, _ in entries]
        if not (self.include_hidden or _is_hidden(pattern)):
            names = [name for name in names if not _is_hidden(name)]
        return fnmatch.filter(names, pattern)

    def _descendants(self, parent, pattern, dirs_only):
        # '**': the parent itself (as an empty relative name), then every
        # path below it, depth first, following symlinked directories.
        # A parent that is not a directory matches nothing.
        if parent and not self._isdir(parent):
            return
        yield pattern[:0]
        yield from self._walk(parent, pattern[:0], dirs_only)

    def _walk(self, parent, prefix, dirs_only):
        for name, is_dir in self._names(os.path.join(parent, prefix)
                                        if prefix else parent, dirs_only):
            if not self.include_hidden and _is_hidden(name):
                continue
            relative = os.path.join(prefix, name) if prefix else name
            yield relative
            if is_dir:
                yield from self._walk(parent, relative, dirs_only)


_DIR_OPEN_FLAGS = os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0)


def translate(pat, *, recursive=False, include_hidden=False, seps=None):
    """Return a regular expression (as a string) for the path pattern *pat*.

    Wildcards never match a path separator (*seps*, by default os.sep and
    os.altsep).  With *recursive*, a '**' component matches any number of
    components; unless *include_hidden* is true, wildcards do not match a
    leading dot of a component.
    """
    if seps is None:
        seps = [os.path.sep]
        if os.path.altsep:
            seps.append(os.path.altsep)
    escaped = ''.join(map(re.escape, seps))
    sep = f'[{escaped}]' if len(seps) > 1 else escaped
    not_sep = f'[^{escaped}]'
    no_dot = '' if include_hidden else r'(?!\.)'

    if include_hidden:
        any_depth_last = '.*'
        any_depth = f'(?:.+{sep})?'
    else:
        # the dot test applies to the first character of each component
        component_re = f'{no_dot}{not_sep}*'
        any_depth_last = f'{component_re}(?:{sep}{component_re})*'
        any_depth = f'(?:{no_dot}{not_sep}+{sep})*'

    components = re.split(sep, pat) if pat else ['']
    pieces = []
    count = len(components)
    for index, component in enumerate(components):
        last = index == count - 1
        if recursive and component == '**':
            if last:
                pieces.append(any_depth_last)
            elif components[index + 1] != '**':
                pieces.append(any_depth)
            # a '**' followed by another '**' adds nothing
            continue
        if component == '*':
            # a lone '*' needs at least one character
            pieces.append(f'{not_sep}+' if include_hidden
                          else f'[^{escaped}.]{not_sep}*')
        else:
            if no_dot and component[:1] in ('*', '?'):
                pieces.append(no_dot)
            parts, _ = fnmatch._translate(component, f'{not_sep}*', not_sep)
            pieces.extend(parts)
        if not last:
            pieces.append(sep)
    return fr'(?s:{"".join(pieces)})\z'
