"""Keywords and soft keywords of the Python 3.15 grammar.

kwlist / softkwlist list them in sorted order; iskeyword() and
issoftkeyword() test membership.
"""

__all__ = ["iskeyword", "issoftkeyword", "kwlist", "softkwlist"]

kwlist = [
    'False',
    'None',
    'True',
    'and',
    'as',
    'assert',
    'async',
    'await',
    'break',
    'class',
    'continue',
    'def',
    'del',
    'elif',
    'else',
    'except',
    'finally',
    'for',
    'from',
    'global',
    'if',
    'import',
    'in',
    'is',
    'lambda',
    'nonlocal',
    'not',
    'or',
    'pass',
    'raise',
    'return',
    'try',
    'while',
    'with',
    'yield'
]

softkwlist = [
    '_',
    'case',
    'lazy',
    'match',
    'type'
]

iskeyword = frozenset(kwlist).__contains__
issoftkeyword = frozenset(softkwlist).__contains__
