"""Detect ambiguous indentation in Python source files.

A file is ambiguous when two lines compare differently (more, less or
equally indented) depending on the size of a tab.  Usage:

    python -m tabnanny [-v] [-q] file_or_directory ...

-v reports more (repeat for even more), -q only prints the names of
offending files.
"""

import os
import sys
import tokenize

__all__ = ["check", "NannyNag", "process_tokens"]

verbose = 0
filename_only = 0


def __getattr__(name):
    if name == "__version__":
        from warnings import _deprecated
        _deprecated("__version__", remove=(3, 20))
        return "6"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def errprint(*args):
    """Print the arguments to stderr and exit with status 1."""
    sys.stderr.write(" ".join(str(arg) for arg in args))
    sys.stderr.write("\n")
    sys.exit(1)


def main():
    import getopt
    global verbose, filename_only
    try:
        opts, args = getopt.getopt(sys.argv[1:], "qv")
    except getopt.error as msg:
        errprint(msg)
    for option, _ in opts:
        if option == '-q':
            filename_only += 1
        elif option == '-v':
            verbose += 1
    if not args:
        errprint("Usage: %s -m tabnanny [-v] file_or_directory ..."
                 % sys.executable)
    for arg in args:
        check(arg)


class NannyNag(Exception):
    """Raised by process_tokens() for an ambiguously indented line."""

    def __init__(self, lineno, msg, line):
        self.lineno, self.msg, self.line = lineno, msg, line

    def get_lineno(self):
        return self.lineno

    def get_msg(self):
        return self.msg

    def get_line(self):
        return self.line


def check(file):
    """Check a file, or recursively all .py files below a directory,
    reporting problems on stdout/stderr."""
    if os.path.isdir(file) and not os.path.islink(file):
        if verbose:
            print("%r: listing directory" % (file,))
        for name in os.listdir(file):
            path = os.path.join(file, name)
            if ((os.path.isdir(path) and not os.path.islink(path))
                    or os.path.normcase(name[-3:]) == ".py"):
                check(path)
        return

    try:
        stream = tokenize.open(file)
    except OSError as err:
        errprint("%r: I/O Error: %s" % (file, err))
        return

    if verbose > 1:
        print("checking %r ..." % file)

    try:
        process_tokens(tokenize.generate_tokens(stream.readline))
    except tokenize.TokenError as err:
        errprint("%r: Token Error: %s" % (file, err))
        return
    except IndentationError as err:
        errprint("%r: Indentation Error: %s" % (file, err))
        return
    except SyntaxError as err:
        errprint("%r: Syntax Error: %s" % (file, err))
        return
    except NannyNag as nag:
        lineno, line = nag.get_lineno(), nag.get_line()
        if verbose:
            print("%r: *** Line %d: trouble in tab city! ***" % (file, lineno))
            print("offending line: %r" % (line,))
            print(nag.get_msg())
        else:
            shown = '"%s"' % file if ' ' in file else file
            if filename_only:
                print(shown)
            else:
                print(shown, lineno, repr(line))
        return
    finally:
        stream.close()

    if verbose:
        print("%r: Clean bill of health." % (file,))


class Whitespace:
    """The leading whitespace of a line, compared under every tab size."""

    def __init__(self, text):
        self.raw = text
        self.spaces = text.count(' ')
        # characters past the indentation (if any) are ignored
        self.prefix = text[:len(text) - len(text.lstrip(' \t'))]

    def width(self, tabsize):
        column = 0
        for char in self.prefix:
            if char == '\t':
                column = (column // tabsize + 1) * tabsize
            else:
                column += 1
        return column

    def _sizes(self, other):
        # Beyond this many columns both widths grow linearly in the tab
        # size, so the comparison cannot change any more.
        return range(1, max(len(self.prefix), len(other.prefix)) + 2)

    def indent_level(self, tabsize):
        return self.width(tabsize)

    def equal(self, other):
        return not self.not_equal_witness(other)

    def not_equal_witness(self, other):
        """Tab sizes for which the two widths differ."""
        return [size for size in self._sizes(other)
                if self.width(size) != other.width(size)]

    def less(self, other):
        return not self.not_less_witness(other)

    def not_less_witness(self, other):
        """Tab sizes for which self is not narrower than other."""
        return [size for size in self._sizes(other)
                if self.width(size) >= other.width(size)]


def format_witnesses(w):
    sizes = [str(item) for item in w]
    label = "at tab size" + ("s" if len(sizes) > 1 else "")
    return label + " " + ", ".join(sizes)


def process_tokens(tokens):
    """Raise NannyNag at the first line whose indentation is ambiguous."""
    try:
        _check_tokens(tokens)
    except TabError as err:
        # the tokenizer itself noticed the inconsistency
        raise NannyNag(err.lineno, err.msg, (err.text or '').rstrip('\r\n'))


def _check_tokens(tokens):
    ignorable = (tokenize.COMMENT, tokenize.NL)
    stack = [Whitespace("")]
    pending_check = False

    for kind, text, start, _, line in tokens:
        if kind == tokenize.NEWLINE:
            # the next significant line must match the current level
            pending_check = True
        elif kind == tokenize.INDENT:
            pending_check = False
            current = Whitespace(text)
            if not stack[-1].less(current):
                witness = stack[-1].not_less_witness(current)
                raise NannyNag(start[0],
                               "indent not greater e.g. " + format_witnesses(witness),
                               line)
            stack.append(current)
        elif kind == tokenize.DEDENT:
            pending_check = True
            del stack[-1]
        elif pending_check and kind not in ignorable:
            pending_check = False
            current = Whitespace(line)
            if not stack[-1].equal(current):
                witness = stack[-1].not_equal_witness(current)
                raise NannyNag(start[0],
                               "indent not equal e.g. " + format_witnesses(witness),
                               line)


if __name__ == '__main__':
    main()
