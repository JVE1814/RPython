"""Private stand-in for codeop (used by code.py when codeop is missing).

Decides whether a piece of interactive input is complete by compiling it
with PyCF_DONT_IMPLY_DEDENT | PyCF_ALLOW_INCOMPLETE_INPUT: the compiler
then reports "incomplete input" for input that needs more lines.
"""

import __future__

_DONT_IMPLY_DEDENT = 0x200
_ALLOW_INCOMPLETE_INPUT = 0x4000
_FEATURES = [getattr(__future__, name) for name in __future__.all_feature_names]


def _is_blank(source):
    for line in source.split("\n"):
        line = line.strip()
        if line and not line.startswith('#'):
            return False
    return True


def _compile(source, filename, symbol, flags):
    if symbol != "eval" and _is_blank(source):
        source = "pass"
    try:
        return compile(source, filename, symbol,
                       flags | _DONT_IMPLY_DEDENT | _ALLOW_INCOMPLETE_INPUT, True)
    except SyntaxError as exc:
        if "incomplete input" in str(exc):
            return None
        raise


def compile_command(source, filename="<input>", symbol="single"):
    """Compile a command; returns a code object, or None if the command is
    incomplete.  Raises SyntaxError, ValueError or OverflowError for
    invalid input."""
    return _compile(source, filename, symbol, 0)


class Compile:
    """compile() that remembers __future__ statements it has seen."""

    def __init__(self):
        self.flags = 0x400   # PyCF_SOURCE_IS_UTF8 analogue (unused here)
        self.flags = 0

    def __call__(self, source, filename, symbol, flags=0, **kwargs):
        code = compile(source, filename, symbol, self.flags | flags, True)
        for feature in _FEATURES:
            if code.co_flags & feature.compiler_flag:
                self.flags |= feature.compiler_flag
        return code


class CommandCompiler:
    """compile_command() that remembers __future__ statements."""

    def __init__(self):
        self.compiler = Compile()

    def __call__(self, source, filename="<input>", symbol="single"):
        code = _compile(source, filename, symbol, self.compiler.flags)
        if code is not None:
            for feature in _FEATURES:
                if code.co_flags & feature.compiler_flag:
                    self.compiler.flags |= feature.compiler_flag
        return code
