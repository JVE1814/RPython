"""Compare files and directories.

cmp(f1, f2, shallow=True)          -> True if the two files look equal
cmpfiles(a, b, names, shallow=True) -> (match, mismatch, errors)
dircmp(a, b, ignore=None, hide=None, *, shallow=True)
                                   -> object describing two directory trees
clear_cache()                      -> forget remembered comparisons
"""

import os
import stat
from types import GenericAlias

__all__ = ['clear_cache', 'cmp', 'dircmp', 'cmpfiles', 'DEFAULT_IGNORES']

# Results of deep comparisons, keyed by both names and their signatures.
_cache = {}
BUFSIZE = 8 * 1024

DEFAULT_IGNORES = [
    'RCS', 'CVS', 'tags', '.git', '.hg', '.bzr', '_darcs', '__pycache__']


def clear_cache():
    """Forget all remembered file comparison results."""
    _cache.clear()


def _signature(st):
    return (stat.S_IFMT(st.st_mode), st.st_size, st.st_mtime)


def cmp(f1, f2, shallow=True):
    """Return True if the files f1 and f2 seem to be equal.

    With shallow=True (the default) files whose type, size and modification
    time agree are considered equal without reading them.  Otherwise the
    contents are compared (results are cached by file signature).
    """
    sig1 = _signature(os.stat(f1))
    sig2 = _signature(os.stat(f2))
    if sig1[0] != stat.S_IFREG or sig2[0] != stat.S_IFREG:
        return False
    if shallow and sig1 == sig2:
        return True
    if sig1[1] != sig2[1]:
        return False
    key = (f1, f2, sig1, sig2)
    result = _cache.get(key)
    if result is None:
        result = _same_contents(f1, f2)
        if len(_cache) > 100:
            clear_cache()
        _cache[key] = result
    return result


def _same_contents(f1, f2):
    with open(f1, 'rb') as a, open(f2, 'rb') as b:
        while True:
            chunk_a = a.read(BUFSIZE)
            chunk_b = b.read(BUFSIZE)
            if chunk_a != chunk_b:
                return False
            if not chunk_a:
                return True


class dircmp:
    """Compare two directories (lazily; attributes are computed on first use).

    dircmp(a, b, ignore=None, hide=None, *, shallow=True)
      a, b:    the directories
      ignore:  names to ignore (default: filecmp.DEFAULT_IGNORES)
      hide:    names to hide (default: [os.curdir, os.pardir])
      shallow: passed on to cmp() for the common files

    Attributes:
      left_list, right_list: the (filtered) directory listings
      common: names in both directories
      left_only, right_only: names in only one of them
      common_dirs, common_files, common_funny: common names by kind
      same_files, diff_files, funny_files: outcome of comparing common_files
      subdirs: dict of name -> dircmp for common_dirs

    Methods report(), report_partial_closure() and report_full_closure()
    print a summary.
    """

    def __init__(self, a, b, ignore=None, hide=None, *, shallow=True):
        self.left = a
        self.right = b
        self.hide = [os.curdir, os.pardir] if hide is None else hide
        self.ignore = DEFAULT_IGNORES if ignore is None else ignore
        self.shallow = shallow

    # ---- lazily computed attributes

    def _listing(self, path):
        skip = set(self.hide) | set(self.ignore)
        return sorted(name for name in os.listdir(path) if name not in skip)

    def _compute_lists(self):
        self.left_list = self._listing(self.left)
        self.right_list = self._listing(self.right)

    def _compute_common(self):
        # names are matched case-insensitively where the OS does so
        left = {os.path.normcase(n): n for n in self.left_list}
        right = {os.path.normcase(n): n for n in self.right_list}
        self.common = [left[k] for k in left if k in right]
        self.left_only = [left[k] for k in left if k not in right]
        self.right_only = [right[k] for k in right if k not in left]

    def _compute_common_kinds(self):
        self.common_dirs = []
        self.common_files = []
        self.common_funny = []
        for name in self.common:
            try:
                st_a = os.stat(os.path.join(self.left, name))
                st_b = os.stat(os.path.join(self.right, name))
            except (OSError, ValueError):
                self.common_funny.append(name)
                continue
            kind_a = stat.S_IFMT(st_a.st_mode)
            kind_b = stat.S_IFMT(st_b.st_mode)
            if kind_a != kind_b:
                self.common_funny.append(name)
            elif stat.S_ISDIR(kind_a):
                self.common_dirs.append(name)
            elif stat.S_ISREG(kind_a):
                self.common_files.append(name)
            else:
                self.common_funny.append(name)

    def _compute_file_results(self):
        self.same_files, self.diff_files, self.funny_files = cmpfiles(
            self.left, self.right, self.common_files, self.shallow)

    def _compute_subdirs(self):
        self.subdirs = {}
        for name in self.common_dirs:
            self.subdirs[name] = self.__class__(
                os.path.join(self.left, name), os.path.join(self.right, name),
                self.ignore, self.hide, shallow=self.shallow)

    _lazy = {
        'left_list': _compute_lists, 'right_list': _compute_lists,
        'common': _compute_common, 'left_only': _compute_common,
        'right_only': _compute_common,
        'common_dirs': _compute_common_kinds,
        'common_files': _compute_common_kinds,
        'common_funny': _compute_common_kinds,
        'same_files': _compute_file_results,
        'diff_files': _compute_file_results,
        'funny_files': _compute_file_results,
        'subdirs': _compute_subdirs,
    }

    def __getattr__(self, attr):
        compute = self._lazy.get(attr)
        if compute is None:
            raise AttributeError(attr)
        compute(self)
        return self.__dict__[attr]

    # Old method names (kept for compatibility).
    def phase0(self):
        self._compute_lists()

    def phase1(self):
        self._compute_common()

    def phase2(self):
        self._compute_common_kinds()

    def phase3(self):
        self._compute_file_results()

    def phase4(self):
        self._compute_subdirs()

    def phase4_closure(self):
        for sub in self.subdirs.values():
            sub.phase4_closure()

    # ---- reports

    def report(self):
        """Print a summary of the differences between a and b."""
        print('diff', self.left, self.right)
        sections = [
            (f'Only in {self.left}', self.left_only),
            (f'Only in {self.right}', self.right_only),
            ('Identical files', self.same_files),
            ('Differing files', self.diff_files),
            ('Trouble with common files', self.funny_files),
            ('Common subdirectories', self.common_dirs),
            ('Common funny cases', self.common_funny),
        ]
        for title, names in sections:
            if names:
                print(title, ':', sorted(names))

    def report_partial_closure(self):
        """Print the report for a and b and their immediate subdirectories."""
        self.report()
        for sub in self.subdirs.values():
            print()
            sub.report()

    def report_full_closure(self):
        """Print the report for a and b and all their subdirectories."""
        self.report()
        for sub in self.subdirs.values():
            print()
            sub.report_full_closure()

    __class_getitem__ = classmethod(GenericAlias)


def cmpfiles(a, b, common, shallow=True):
    """Compare the files named in `common` in directories a and b.

    Returns three lists: names of the files that compare equal, those that
    differ, and those that could not be compared."""
    result = ([], [], [])
    for name in common:
        outcome = _try_cmp(os.path.join(a, name), os.path.join(b, name), shallow)
        result[outcome].append(name)
    return result


def _try_cmp(left, right, shallow):
    # 0: equal, 1: different, 2: error
    try:
        return 0 if cmp(left, right, shallow) else 1
    except (OSError, ValueError):
        return 2


def demo():
    import sys
    import getopt
    options, args = getopt.getopt(sys.argv[1:], 'r')
    if len(args) != 2:
        raise getopt.GetoptError('need exactly two args', None)
    comparison = dircmp(args[0], args[1])
    if ('-r', '') in options:
        comparison.report_full_closure()
    else:
        comparison.report()


if __name__ == '__main__':
    demo()
