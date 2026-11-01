"""Compile source typed interactively, recognising input that is unfinished.

``compile_command(source, filename="<input>", symbol="single")`` returns a
code object for complete input, None while more lines are needed, and raises
SyntaxError (or ValueError/OverflowError for bad literals) for invalid input.

``CommandCompiler`` does the same but remembers ``from __future__`` imports
seen in earlier input; ``Compile`` is the corresponding stand-in for the
builtin compile() without the incomplete-input detection.
"""

import __future__
import warnings

__all__ = ["compile_command", "Compile", "CommandCompiler"]

# compile() flag values (same numbers as the C API constants).
PyCF_DONT_IMPLY_DEDENT = 0x200
PyCF_ONLY_AST = 0x400
PyCF_ALLOW_INCOMPLETE_INPUT = 0x4000

_INCOMPLETE_FLAGS = PyCF_DONT_IMPLY_DEDENT | PyCF_ALLOW_INCOMPLETE_INPUT

_FUTURE_FLAGS = [getattr(__future__, name).compiler_flag
                 for name in __future__.all_feature_names]


def _only_blank_or_comments(source):
    for line in source.split("\n"):
        text = line.strip()
        if text and not text.startswith("#"):
            return False
    return True


def _probe(compiler, source, filename, symbol, flags):
    """Decide whether *source* is unfinished.

    Returns True when more input is expected, False when the definitive
    compilation should be attempted (which then reports any error).
    """
    with warnings.catch_warnings():
        # warnings are reported by the definitive compilation only
        warnings.simplefilter("ignore", (SyntaxWarning, DeprecationWarning))
        try:
            compiler(source, filename, symbol, flags=flags)
            return False
        except SyntaxError:
            pass
        try:
            compiler(source + "\n", filename, symbol, flags=flags)
        except _IncompleteInputError:
            return True
        except SyntaxError:
            return False
        return True


def _compile_interactive(compiler, source, filename, symbol, flags):
    if symbol != "eval" and _only_blank_or_comments(source):
        source = "pass"
    if _probe(compiler, source, filename, symbol, flags):
        return None
    return compiler(source, filename, symbol, incomplete_input=False)


def _plain_compile(source, filename, symbol, incomplete_input=True, *, flags=0):
    if incomplete_input:
        flags |= _INCOMPLETE_FLAGS
    return compile(source, filename, symbol, flags)


def compile_command(source, filename="<input>", symbol="single", flags=0):
    """Compile one interactive command.

    Returns a code object if *source* is a complete statement (or
    expression for symbol 'eval'), None if it is the beginning of one, and
    raises SyntaxError, ValueError or OverflowError if it is invalid.
    """
    return _compile_interactive(_plain_compile, source, filename, symbol, flags)


class Compile:
    """Callable with the signature of compile() that keeps track of the
    future features enabled by the code it has compiled."""

    def __init__(self):
        self.flags = _INCOMPLETE_FLAGS

    def __call__(self, source, filename, symbol, flags=0, **kwargs):
        flags |= self.flags
        if kwargs.get('incomplete_input', True) is False:
            flags &= ~_INCOMPLETE_FLAGS
        result = compile(source, filename, symbol, flags, True)
        if not flags & PyCF_ONLY_AST:
            for feature in _FUTURE_FLAGS:
                if result.co_flags & feature:
                    self.flags |= feature
        return result


class CommandCompiler:
    """Like `compile_command`, but future statements seen in earlier
    commands stay in effect for later ones."""

    def __init__(self):
        self.compiler = Compile()

    def __call__(self, source, filename="<input>", symbol="single"):
        """Compile a command; see `compile_command` for the results."""
        return _compile_interactive(self.compiler, source, filename, symbol,
                                    self.compiler.flags)
