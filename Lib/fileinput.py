"""Iterate over the lines of several files (or standard input) in turn.

Typical use::

    import fileinput
    for line in fileinput.input(encoding="utf-8"):
        handle(line)

which reads the files named in sys.argv[1:], or standard input when there
are none; the name '-' also stands for standard input.  While iterating,
filename(), lineno(), filelineno(), isfirstline() and isstdin() describe
the current line, and nextfile() skips the rest of the current file.

With ``inplace=True`` each file is renamed to a backup and standard output
is redirected into a new file of the original name, so printing a line
writes it to the replacement file.  The backup is removed unless a
*backup* extension was given.

FileInput is the class behind these functions; the module-level functions
operate on one shared instance created by input().
"""

import io
import os
import sys
from types import GenericAlias

__all__ = ["input", "close", "nextfile", "filename", "lineno", "filelineno",
           "fileno", "isfirstline", "isstdin", "FileInput", "hook_compressed",
           "hook_encoded"]

# the instance used by the module-level functions
_state = None


def input(files=None, inplace=False, backup="", *, mode="r", openhook=None,
          encoding=None, errors=None):
    """Create the shared FileInput instance and return it (arguments as
    for FileInput)."""
    global _state
    if _state and _state._file:
        raise RuntimeError("input() already active")
    _state = FileInput(files, inplace, backup, mode=mode, openhook=openhook,
                       encoding=encoding, errors=errors)
    return _state


def _active():
    if not _state:
        raise RuntimeError("no active input()")
    return _state


def close():
    """Close the shared instance (if any)."""
    global _state
    state, _state = _state, None
    if state:
        state.close()


def nextfile():
    """Skip to the next file of the shared instance."""
    return _active().nextfile()


def filename():
    """Name of the file being read ('<stdin>' for standard input)."""
    return _active().filename()


def lineno():
    """Cumulative number of the line just read."""
    return _active().lineno()


def filelineno():
    """Number of the line just read within its file."""
    return _active().filelineno()


def fileno():
    """File descriptor of the current file, or -1."""
    return _active().fileno()


def isfirstline():
    """Whether the line just read is the first of its file."""
    return _active().isfirstline()


def isstdin():
    """Whether the line just read came from standard input."""
    return _active().isstdin()


class FileInput:
    """Line iterator over a sequence of files.

    files     file names (str or path-like); default sys.argv[1:], and
              standard input when that is empty
    inplace   replace each file by what is printed while reading it
    backup    extension for the kept backup in inplace mode
    mode      'r' (text) or 'rb' (bytes)
    openhook  callable(filename, mode[, encoding=, errors=]) returning the
              file object to read (not with inplace)
    encoding, errors  passed to open() for text files
    """

    def __init__(self, files=None, inplace=False, backup="", *, mode="r",
                 openhook=None, encoding=None, errors=None):
        if isinstance(files, str):
            files = (files,)
        elif isinstance(files, os.PathLike):
            files = (os.fspath(files),)
        else:
            if files is None:
                files = sys.argv[1:]
            files = tuple(files) if files else ('-',)
        self._files = files
        self._inplace = inplace
        self._backup = backup
        self._savestdout = None
        self._output = None
        self._filename = None
        self._lineno = 0
        self._filelineno = 0
        self._file = None
        self._isstdin = False
        self._backupfilename = None
        if "b" not in mode and encoding is None and openhook is None:
            encoding = io.text_encoding(None)
        self._encoding = encoding
        self._errors = errors
        if mode not in ('r', 'rb'):
            raise ValueError("FileInput opening mode must be 'r' or 'rb'")
        self._mode = mode
        self._write_mode = 'wb' if 'b' in mode else 'w'
        if openhook is not None:
            if inplace:
                raise ValueError("FileInput cannot use an opening hook in "
                                 "inplace mode")
            if not callable(openhook):
                raise ValueError("FileInput openhook must be callable")
        self._openhook = openhook

    def __del__(self):
        self.close()

    def close(self):
        try:
            self.nextfile()
        finally:
            self._files = ()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        self.close()

    def __iter__(self):
        return self

    def __next__(self):
        line = self.readline()
        if line:
            return line
        raise StopIteration

    def nextfile(self):
        """Stop reading the current file (finishing an in-place rewrite)."""
        saved = self._savestdout
        self._savestdout = None
        if saved:
            sys.stdout = saved

        output, self._output = self._output, None
        current, self._file = self._file, None
        backup, self._backupfilename = self._backupfilename, None
        try:
            if output:
                output.close()
        finally:
            try:
                if current and not self._isstdin:
                    current.close()
            finally:
                if backup and not self._backup:
                    try:
                        os.unlink(backup)
                    except OSError:
                        pass
                self._isstdin = False

    def readline(self):
        """Next line across all files; '' when everything has been read."""
        while True:
            if self._file is None:
                if not self._files:
                    return b'' if 'b' in self._mode else ''
                self._open_next()
            line = self._file.readline()
            if line:
                self._lineno += 1
                self._filelineno += 1
                return line
            self.nextfile()

    def _open_next(self):
        name = self._files[0]
        self._files = self._files[1:]
        self._filename = name
        self._filelineno = 0
        self._isstdin = False
        self._backupfilename = None
        binary = 'b' in self._mode
        encoding = None if binary else self._encoding

        if name == '-':
            self._filename = '<stdin>'
            stream = sys.stdin
            if binary:
                stream = getattr(sys.stdin, 'buffer', sys.stdin)
            self._file = stream
            self._isstdin = True
            return

        if self._inplace:
            self._start_inplace(name, encoding)
        elif self._openhook is not None:
            if self._encoding is None:
                self._file = self._openhook(name, self._mode)
            else:
                self._file = self._openhook(name, self._mode,
                                            encoding=self._encoding,
                                            errors=self._errors)
        else:
            self._file = open(name, self._mode, encoding=encoding,
                              errors=self._errors)

    def _start_inplace(self, name, encoding):
        backup = os.fspath(name) + (self._backup or ".bak")
        try:
            os.unlink(backup)
        except OSError:
            pass
        os.rename(name, backup)
        self._backupfilename = backup
        self._file = open(backup, self._mode, encoding=encoding,
                          errors=self._errors)
        try:
            permissions = os.fstat(self._file.fileno()).st_mode
        except OSError:
            self._output = open(name, self._write_mode, encoding=encoding,
                                errors=self._errors)
        else:
            flags = os.O_CREAT | os.O_WRONLY | os.O_TRUNC
            if hasattr(os, 'O_BINARY'):
                flags |= os.O_BINARY
            fd = os.open(name, flags, permissions)
            self._output = os.fdopen(fd, self._write_mode,
                                     encoding=encoding, errors=self._errors)
            try:
                os.chmod(name, permissions)
            except OSError:
                pass
        self._savestdout = sys.stdout
        sys.stdout = self._output

    def filename(self):
        return self._filename

    def lineno(self):
        return self._lineno

    def filelineno(self):
        return self._filelineno

    def fileno(self):
        if self._file:
            try:
                return self._file.fileno()
            except ValueError:
                return -1
        return -1

    def isfirstline(self):
        return self._filelineno == 1

    def isstdin(self):
        return self._isstdin

    __class_getitem__ = classmethod(GenericAlias)


def hook_compressed(filename, mode, *, encoding=None, errors=None):
    """Open hook that reads .gz and .bz2 files transparently (by
    extension)."""
    if encoding is None and "b" not in mode:
        encoding = "locale"
    extension = os.path.splitext(filename)[1]
    if extension == '.gz':
        import gzip
        stream = gzip.open(filename, mode)
    elif extension == '.bz2':
        import bz2
        stream = bz2.BZ2File(filename, mode)
    else:
        return open(filename, mode, encoding=encoding, errors=errors)
    if "b" not in mode:
        stream = io.TextIOWrapper(stream, encoding=encoding, errors=errors)
    return stream


def hook_encoded(encoding, errors=None):
    """Open hook that opens files with the given encoding and errors."""
    def opener(filename, mode):
        return open(filename, mode, encoding=encoding, errors=errors)
    return opener


def _main():
    import getopt
    inplace = False
    backup = False
    opts, args = getopt.getopt(sys.argv[1:], "ib:")
    for option, value in opts:
        if option == '-i':
            inplace = True
        elif option == '-b':
            backup = value
    for line in input(args, inplace=inplace, backup=backup):
        if line[-1:] == '\n':
            line = line[:-1]
        if line[-1:] == '\r':
            line = line[:-1]
        print("%d: %s[%d]%s %s" % (lineno(), filename(), filelineno(),
                                   isfirstline() and "*" or "", line))
    print("%d: %s[%d]" % (lineno(), filename(), filelineno()))


if __name__ == '__main__':
    _main()
