"""Command line option parsing in the style of Unix getopt().

getopt(args, shortopts, longopts=[])     options must precede operands
gnu_getopt(args, shortopts, longopts=[]) options and operands may mix
GetoptError (alias: error)               raised for invalid command lines

Short option specs are letters; a letter followed by ':' takes a required
argument and by '::' an optional one.  Long option specs are names; a
trailing '=' means a required argument and '=?' an optional one.  Long
options may be abbreviated to any unique prefix.
"""

import os

__all__ = ["GetoptError", "error", "getopt", "gnu_getopt"]


def _(text):
    # Messages are translatable when gettext is available.
    try:
        from gettext import gettext
    except ImportError:
        return text
    return gettext(text)


class GetoptError(Exception):
    """Invalid command line; .msg is the message, .opt the option name."""
    opt = ''
    msg = ''

    def __init__(self, msg, opt=''):
        self.msg = msg
        self.opt = opt
        Exception.__init__(self, msg, opt)

    def __str__(self):
        return self.msg


error = GetoptError

# Argument requirements as reported by short_has_arg()/long_has_args().
_REQUIRED = True
_NONE = False
_OPTIONAL = '?'


def _as_list(longopts):
    if isinstance(longopts, str):
        return [longopts]
    return list(longopts)


def getopt(args, shortopts, longopts=[]):
    """Parse the option list `args` (normally sys.argv[1:]).

    Returns (options, operands): options is a list of (name, value) pairs
    with names such as '-x' or '--name' and value '' when there is no
    argument; operands are the remaining arguments.  Parsing stops at the
    first operand or after '--'.
    """
    longopts = _as_list(longopts)
    found = []
    rest = list(args)
    while rest:
        arg = rest[0]
        if arg == '--':
            rest = rest[1:]
            break
        if not arg.startswith('-') or arg == '-':
            break
        if arg.startswith('--'):
            found, rest = do_longs(found, arg[2:], longopts, rest[1:])
        else:
            found, rest = do_shorts(found, arg[1:], shortopts, rest[1:])
    return found, rest


def gnu_getopt(args, shortopts, longopts=[]):
    """Like getopt(), but operands and options may be interleaved.

    A leading '+' in shortopts (or a set POSIXLY_CORRECT environment
    variable) restores the getopt() behaviour of stopping at the first
    operand.  A leading '-' keeps operands in place: each appears in the
    option list as (None, [operand]).
    """
    longopts = _as_list(longopts)
    in_order = stop_at_operand = False
    if shortopts[:1] == '-':
        in_order = True
        shortopts = shortopts[1:]
    elif shortopts[:1] == '+':
        stop_at_operand = True
        shortopts = shortopts[1:]
    elif os.environ.get("POSIXLY_CORRECT"):
        stop_at_operand = True

    found = []
    operands = []
    rest = list(args)
    while rest:
        arg = rest[0]
        if arg == '--':
            operands.extend(rest[1:])
            break
        if in_order and operands and arg.startswith('-') and arg != '-':
            # operands seen so far are reported in place, before this option
            found.append((None, operands))
            operands = []
        if arg.startswith('--'):
            found, rest = do_longs(found, arg[2:], longopts, rest[1:])
            continue
        if arg.startswith('-') and arg != '-':
            found, rest = do_shorts(found, arg[1:], shortopts, rest[1:])
            continue
        # an operand
        if stop_at_operand:
            operands.extend(rest)
            break
        operands.append(arg)
        rest = rest[1:]
    return found, operands


def do_longs(opts, opt, longopts, args):
    """Handle one '--name[=value]' argument (without the dashes)."""
    name, sep, value = opt.partition('=')
    if not sep:
        value = None
    needs, name = long_has_args(name, longopts)
    if needs is _REQUIRED:
        if value is None:
            if not args:
                raise GetoptError(_('option --%s requires argument') % name, name)
            value, args = args[0], args[1:]
    elif needs == _OPTIONAL:
        if value is None:
            value = ''
    elif value is not None:
        raise GetoptError(_('option --%s must not have an argument') % name, name)
    opts.append(('--' + name, value or ''))
    return opts, args


def long_has_args(opt, longopts):
    """Look up a (possibly abbreviated) long option.

    Returns (requirement, full_name) with requirement True (required
    argument), False (no argument) or '?' (optional argument).
    """
    # An exact spelling wins over abbreviations.
    for suffix, requirement in (('', _NONE), ('=', _REQUIRED), ('=?', _OPTIONAL)):
        if opt + suffix in longopts:
            return requirement, opt
    candidates = [spec for spec in longopts if spec.startswith(opt)]
    if not candidates:
        raise GetoptError(_('option --%s not recognized') % opt, opt)
    if len(candidates) > 1:
        raise GetoptError(_('option --%s not a unique prefix') % opt, opt)
    spec = candidates[0]
    if spec.endswith('=?'):
        return _OPTIONAL, spec[:-2]
    if spec.endswith('='):
        return _REQUIRED, spec[:-1]
    return _NONE, spec


def do_shorts(opts, optstring, shortopts, args):
    """Handle one cluster of short options such as '-abc' (without '-')."""
    pos = 0
    while pos < len(optstring):
        letter = optstring[pos]
        pos += 1
        needs = short_has_arg(letter, shortopts)
        if needs is _REQUIRED:
            if pos == len(optstring):
                if not args:
                    raise GetoptError(_('option -%s requires argument') % letter,
                                      letter)
                value, args = args[0], args[1:]
            else:
                value = optstring[pos:]
            pos = len(optstring)
        elif needs == _OPTIONAL:
            value = optstring[pos:]
            pos = len(optstring)
        else:
            value = ''
        opts.append(('-' + letter, value))
    return opts, args


def short_has_arg(opt, shortopts):
    """Return True, False or '?' for the argument requirement of letter
    `opt` according to `shortopts`."""
    index = 0
    while index < len(shortopts):
        letter = shortopts[index]
        colons = 0
        while index + 1 + colons < len(shortopts) and shortopts[index + 1 + colons] == ':':
            colons += 1
        if letter == opt and letter != ':':
            if colons >= 2:
                return _OPTIONAL
            return colons == 1
        index += 1 + colons if letter != ':' else 1
    raise GetoptError(_('option -%s not recognized') % opt, opt)


if __name__ == '__main__':
    import sys
    print(getopt(sys.argv[1:], "a:b", ["alpha=", "beta"]))
