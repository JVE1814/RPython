"""Numeric codes of the token types produced by the Python tokenizer.

Every token type is a module level integer constant (``NAME``, ``OP``, ...);
``tok_name`` maps the codes back to names and ``EXACT_TOKEN_TYPES`` maps
operator strings to their specific codes.
"""

# Token type names in code order: the code of a name is its position.
_ORDERED_NAMES = (
    'ENDMARKER', 'NAME', 'NUMBER', 'STRING', 'NEWLINE', 'INDENT', 'DEDENT',
    'LPAR', 'RPAR', 'LSQB', 'RSQB', 'COLON', 'COMMA', 'SEMI', 'PLUS', 'MINUS',
    'STAR', 'SLASH', 'VBAR', 'AMPER', 'LESS', 'GREATER', 'EQUAL', 'DOT',
    'PERCENT', 'LBRACE', 'RBRACE', 'EQEQUAL', 'NOTEQUAL', 'LESSEQUAL',
    'GREATEREQUAL', 'TILDE', 'CIRCUMFLEX', 'LEFTSHIFT', 'RIGHTSHIFT',
    'DOUBLESTAR', 'PLUSEQUAL', 'MINEQUAL', 'STAREQUAL', 'SLASHEQUAL',
    'PERCENTEQUAL', 'AMPEREQUAL', 'VBAREQUAL', 'CIRCUMFLEXEQUAL',
    'LEFTSHIFTEQUAL', 'RIGHTSHIFTEQUAL', 'DOUBLESTAREQUAL', 'DOUBLESLASH',
    'DOUBLESLASHEQUAL', 'AT', 'ATEQUAL', 'RARROW', 'ELLIPSIS', 'COLONEQUAL',
    'EXCLAMATION', 'OP', 'TYPE_IGNORE', 'TYPE_COMMENT', 'SOFT_KEYWORD',
    'FSTRING_START', 'FSTRING_MIDDLE', 'FSTRING_END',
    'TSTRING_START', 'TSTRING_MIDDLE', 'TSTRING_END',
    'COMMENT', 'NL',
    # the last two are only produced by the tokenize module
    'ERRORTOKEN', 'ENCODING',
)

# Operator spelling of the types between LPAR and EXCLAMATION.
_OPERATORS = (
    '(', ')', '[', ']', ':', ',', ';', '+', '-', '*', '/', '|', '&', '<',
    '>', '=', '.', '%', '{', '}', '==', '!=', '<=', '>=', '~', '^', '<<',
    '>>', '**', '+=', '-=', '*=', '/=', '%=', '&=', '|=', '^=', '<<=',
    '>>=', '**=', '//', '//=', '@', '@=', '->', '...', ':=', '!',
)

tok_name = dict(enumerate(_ORDERED_NAMES))
globals().update({name: code for code, name in tok_name.items()})

N_TOKENS = len(_ORDERED_NAMES)
NT_OFFSET = 256   # first non-terminal (grammar symbol) code
tok_name[N_TOKENS] = 'N_TOKENS'
tok_name[NT_OFFSET] = 'NT_OFFSET'

_first_operator = _ORDERED_NAMES.index('LPAR')
EXACT_TOKEN_TYPES = {spelling: _first_operator + i
                     for i, spelling in enumerate(_OPERATORS)}
del _first_operator

__all__ = ['tok_name', 'ISTERMINAL', 'ISNONTERMINAL', 'ISEOF',
           'EXACT_TOKEN_TYPES'] + list(tok_name.values())


def ISTERMINAL(x):
    """True for token (terminal) codes."""
    return x < NT_OFFSET


def ISNONTERMINAL(x):
    """True for grammar symbol (non-terminal) codes."""
    return x >= NT_OFFSET


def ISEOF(x):
    """True for the end-of-input token."""
    return x == ENDMARKER
