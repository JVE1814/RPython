"""Abstract syntax trees of Python source code.

The node classes come from the builtin ``_ast`` module.  This module adds
the conveniences around them: parsing (`parse`), safe evaluation of literals
(`literal_eval`), pretty printing (`dump`), source regeneration (`unparse`),
tree traversal (`walk`, `NodeVisitor`, `NodeTransformer`), location helpers
and structural comparison (`compare`).  ``python -m ast`` dumps a file.
"""
from _ast import *


def parse(source, filename='<unknown>', mode='exec', *,
          type_comments=False, feature_version=None, optimize=-1, module=None):
    """Parse *source* and return the root node of its AST.

    *mode* is 'exec', 'eval', 'single' or 'func_type'.  *feature_version*
    (a ``(3, minor)`` tuple or a minor version int) restricts the accepted
    grammar; *optimize* > 0 returns a constant-folded tree.
    """
    flags = PyCF_ONLY_AST
    if optimize > 0:
        flags |= PyCF_OPTIMIZED_AST
    if type_comments:
        flags |= PyCF_TYPE_COMMENTS
    if feature_version is None:
        minor = -1
    elif isinstance(feature_version, tuple):
        major, minor = feature_version
        if major != 3:
            raise ValueError(f"Unsupported major version: {major}")
    else:
        minor = feature_version
    return compile(source, filename, mode, flags, _feature_version=minor,
                   optimize=optimize, module=module)


_NUMBER_TYPES = (int, float, complex)


def literal_eval(node_or_string):
    """Evaluate a string or expression node holding only a literal display.

    Accepted: str, bytes, int, float, complex, bool and None constants,
    tuples, lists, dicts, sets (including ``set()``), unary +/- applied to
    numbers and ``real +/- imaginary`` sums.  Anything else raises
    ValueError.
    """
    root = node_or_string
    if isinstance(root, str):
        root = parse(root.lstrip(" \t"), mode='eval')
    if isinstance(root, Expression):
        root = root.body

    def reject(node):
        where = getattr(node, 'lineno', None)
        text = "malformed node or string"
        if where:
            text = f"{text} on line {where}"
        raise ValueError(f"{text}: {node!r}")

    def number(node):
        if isinstance(node, Constant) and type(node.value) in _NUMBER_TYPES:
            return node.value
        reject(node)

    def signed(node):
        if isinstance(node, UnaryOp):
            if isinstance(node.op, USub):
                return -number(node.operand)
            if isinstance(node.op, UAdd):
                return +number(node.operand)
        return number(node)

    def value(node):
        if isinstance(node, Constant):
            return node.value
        if isinstance(node, (Tuple, List, Set)):
            items = [value(e) for e in node.elts]
            if isinstance(node, Tuple):
                return tuple(items)
            return items if isinstance(node, List) else set(items)
        if isinstance(node, Dict):
            if len(node.keys) != len(node.values):
                reject(node)
            return {value(k): value(v) for k, v in zip(node.keys, node.values)}
        if (isinstance(node, Call) and isinstance(node.func, Name)
                and node.func.id == 'set' and not node.args and not node.keywords):
            return set()
        if isinstance(node, BinOp) and isinstance(node.op, (Add, Sub)):
            real = signed(node.left)
            imag = number(node.right)
            if isinstance(real, (int, float)) and isinstance(imag, complex):
                return real + imag if isinstance(node.op, Add) else real - imag
        return signed(node)

    return value(root)


class _Dumper:
    """Formatter behind `dump`."""

    def __init__(self, annotate_fields, include_attributes, indent,
                 show_empty, color):
        self.annotate = annotate_fields
        self.attributes = include_attributes
        if indent is not None and not isinstance(indent, str):
            indent = ' ' * indent
        self.indent = indent
        self.show_empty = show_empty
        if color:
            self.cls_fmt = '\x1b[1;34m{}\x1b[0m'
            self.key_fmt = '\x1b[36m{}\x1b[0m'
        else:
            self.cls_fmt = self.key_fmt = '{}'

    @staticmethod
    def _is_constant_field(cls, name):
        types = getattr(cls, '_field_types', None)
        return isinstance(types, dict) and types.get(name) is object

    def _hidden(self, cls, name, value):
        """Should a default-looking field value be left out?"""
        if self.show_empty or self._is_constant_field(cls, name):
            return False
        return value is None or value == [] or isinstance(value, Load)

    def format(self, node, depth):
        """Return (text, is_simple) for *node* at nesting *depth*."""
        if self.indent is None:
            opener, separator = '', ', '
        else:
            depth += 1
            pad = self.indent * depth
            opener, separator = '\n' + pad, ',\n' + pad
        if isinstance(node, list):
            if not node:
                return '[]', True
            inner = separator.join(self.format(x, depth)[0] for x in node)
            return f'[{opener}{inner}]', False
        if not isinstance(node, AST):
            return repr(node), True

        cls = type(node)
        shown = []
        pending = []   # skipped positional values, needed if a later one shows
        simple = True
        by_name = self.annotate
        for name in node._fields:
            try:
                value = getattr(node, name)
            except AttributeError:
                by_name = True
                continue
            if value is None and getattr(cls, name, ...) is None:
                by_name = True
                continue
            if self._hidden(cls, name, value):
                pending.append(self.format(value, depth)[0])
                continue
            text, sub_simple = self.format(value, depth)
            simple = simple and sub_simple
            if by_name:
                shown.append(f'{self.key_fmt.format(name)}={text}')
            else:
                shown.extend(pending)
                pending = []
                shown.append(text)
        if self.attributes:
            for name in node._attributes or ():
                try:
                    value = getattr(node, name)
                except AttributeError:
                    continue
                if value is None and getattr(cls, name, ...) is None:
                    continue
                text, sub_simple = self.format(value, depth)
                simple = simple and sub_simple
                shown.append(f'{self.key_fmt.format(name)}={text}')
        head = self.cls_fmt.format(cls.__name__)
        if simple and len(shown) <= 3:
            return f"{head}({', '.join(shown)})", not shown
        return f'{head}({opener}{separator.join(shown)})', False


def dump(node, annotate_fields=True, include_attributes=False, *,
         indent=None, show_empty=False, color=False):
    """Return a readable string representation of the tree *node*.

    Field names are shown unless *annotate_fields* is false (then they are
    only used where needed); position attributes are shown when
    *include_attributes* is true.  *indent* (spaces or a string) selects a
    multi-line layout.  Empty lists, None values and Load contexts are
    omitted unless *show_empty* is true.  *color* adds ANSI highlighting.
    """
    if not isinstance(node, AST):
        raise TypeError('expected AST, got %r' % type(node).__name__)
    dumper = _Dumper(annotate_fields, include_attributes, indent,
                     show_empty, color)
    return dumper.format(node, 0)[0]


_POSITIONS = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset')


def copy_location(new_node, old_node):
    """Give *new_node* the source position of *old_node*; returns *new_node*."""
    missing = object()
    for name in _POSITIONS:
        if name not in old_node._attributes or name not in new_node._attributes:
            continue
        value = getattr(old_node, name, missing)
        if value is missing:
            continue
        # end positions are optional, so None is a meaningful value there
        if value is not None or name.startswith('end_'):
            setattr(new_node, name, value)
    return new_node


def fix_missing_locations(node):
    """Fill in absent positions throughout the tree rooted at *node*.

    A node without a position inherits its parent's; the root defaults to
    line 1, column 0.  Returns *node*.
    """
    stack = [(node, (1, 0, 1, 0))]
    while stack:
        current, inherited = stack.pop()
        attrs = current._attributes
        pos = list(inherited)
        for i, name in enumerate(_POSITIONS):
            if name not in attrs:
                continue
            if name.startswith('end_'):
                present = getattr(current, name, None) is not None
            else:
                present = hasattr(current, name)
            if present:
                pos[i] = getattr(current, name)
            else:
                setattr(current, name, pos[i])
        children = list(iter_child_nodes(current))
        for child in reversed(children):
            stack.append((child, tuple(pos)))
    return node


def increment_lineno(node, n=1):
    """Shift every line number in the tree rooted at *node* by *n*."""
    for current in walk(node):
        if isinstance(current, TypeIgnore):
            # here lineno is a field, not a position attribute
            current.lineno = getattr(current, 'lineno', 0) + n
            continue
        attrs = current._attributes
        if 'lineno' in attrs:
            current.lineno = getattr(current, 'lineno', 0) + n
        if 'end_lineno' in attrs:
            end = getattr(current, 'end_lineno', 0)
            if end is not None:
                current.end_lineno = end + n
    return node


def iter_fields(node):
    """Yield ``(name, value)`` for each field that is set on *node*."""
    for name in node._fields:
        try:
            value = getattr(node, name)
        except AttributeError:
            continue
        yield name, value


def iter_child_nodes(node):
    """Yield the direct child nodes of *node* in field order."""
    for _, value in iter_fields(node):
        if isinstance(value, AST):
            yield value
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, AST):
                    yield item


def _clean_docstring(text):
    """Expand tabs, drop the common indentation of all lines after the
    first, strip the first line's indentation and surrounding blank lines."""
    lines = text.expandtabs().split('\n')
    indents = [len(l) - len(l.lstrip(' ')) for l in lines[1:] if l.strip(' ')]
    cut = min(indents) if indents else 0
    lines = [lines[0].lstrip(' ')] + [l[cut:] for l in lines[1:]]
    while lines and not lines[-1]:
        del lines[-1]
    while lines and not lines[0]:
        del lines[0]
    return '\n'.join(lines)


def get_docstring(node, clean=True):
    """Docstring of a module, class or function node, or None.

    With *clean* (the default) indentation is normalized like
    ``inspect.cleandoc``.
    """
    if not isinstance(node, (AsyncFunctionDef, FunctionDef, ClassDef, Module)):
        raise TypeError("%r can't have docstrings" % type(node).__name__)
    body = node.body
    if not body or not isinstance(body[0], Expr):
        return None
    first = body[0].value
    if not (isinstance(first, Constant) and isinstance(first.value, str)):
        return None
    return _clean_docstring(first.value) if clean else first.value


def _source_lines(source, limit=None):
    """Split like the tokenizer does: only \\r\\n, \\n and \\r end lines.
    Line endings are kept; at most *limit* lines are returned."""
    lines = []
    start = 0
    i = 0
    size = len(source)
    while i < size and (limit is None or len(lines) < limit):
        ch = source[i]
        if ch == '\n' or ch == '\r':
            if ch == '\r' and source[i + 1:i + 2] == '\n':
                i += 1
            lines.append(source[start:i + 1])
            start = i + 1
        i += 1
    if limit is None or len(lines) < limit:
        lines.append(source[start:])
    return lines


def get_source_segment(source, node, *, padded=False):
    """Return the part of *source* that *node* was parsed from.

    None is returned if *node* lacks position information.  With *padded*
    the first line of a multi-line segment keeps its original column by
    being prefixed with spaces (tabs and form feeds are preserved).
    """
    try:
        if node.end_lineno is None or node.end_col_offset is None:
            return None
        first, last = node.lineno - 1, node.end_lineno - 1
        start, stop = node.col_offset, node.end_col_offset
    except AttributeError:
        return None
    lines = [l.encode() for l in _source_lines(source, last + 1)]
    if first == last:
        return lines[first][start:stop].decode()
    prefix = ''
    if padded:
        prefix = ''.join(c if c in '\t\f' else ' '
                         for c in lines[first][:start].decode())
    pieces = [prefix + lines[first][start:].decode()]
    pieces += [l.decode() for l in lines[first + 1:last]]
    pieces.append(lines[last][:stop].decode())
    return ''.join(pieces)


def walk(node):
    """Yield *node* and all its descendants (breadth first)."""
    level = [node]
    while level:
        following = []
        for current in level:
            following.extend(iter_child_nodes(current))
            yield current
        level = following


def compare(a, b, /, *, compare_attributes=False):
    """Structural equality of two trees.

    Node types and fields must match recursively; constants must have the
    same type and compare equal.  Position attributes are compared only if
    *compare_attributes* is true.
    """
    absent = object()

    def same(x, y):
        if isinstance(x, AST):
            return compare(x, y, compare_attributes=compare_attributes)
        if isinstance(x, list):
            return (isinstance(y, list) and len(x) == len(y)
                    and all(same(p, q) for p, q in zip(x, y)))
        return type(x) is type(y) and x == y

    if type(a) is not type(b) or a._fields != b._fields:
        return False
    for name in a._fields:
        x = getattr(a, name, absent)
        y = getattr(b, name, absent)
        if x is absent or y is absent:
            if x is not y:
                return False
        elif not same(x, y):
            return False
    if compare_attributes:
        if a._attributes != b._attributes:
            return False
        for name in a._attributes:
            if getattr(a, name, absent) != getattr(b, name, absent):
                return False
    return True


class NodeVisitor(object):
    """Base class for tree walkers.

    ``visit(node)`` calls the method ``visit_<ClassName>`` if the subclass
    defines one and ``generic_visit`` otherwise; the latter visits all child
    nodes.  Use `NodeTransformer` to modify the tree while walking it.
    """

    def visit(self, node):
        """Dispatch *node* to its visitor method."""
        handler = getattr(self, 'visit_' + type(node).__name__,
                          self.generic_visit)
        return handler(node)

    def generic_visit(self, node):
        """Visit all child nodes of *node*."""
        for child in iter_child_nodes(node):
            self.visit(child)


class NodeTransformer(NodeVisitor):
    """A `NodeVisitor` whose visitor results replace the visited nodes.

    Returning None removes the node; inside a list field a visitor may also
    return a sequence of nodes, which is spliced in.
    """

    def generic_visit(self, node):
        for name, old in iter_fields(node):
            if isinstance(old, list):
                replacement = []
                for item in old:
                    if not isinstance(item, AST):
                        replacement.append(item)
                        continue
                    result = self.visit(item)
                    if result is None:
                        continue
                    if isinstance(result, AST):
                        replacement.append(result)
                    else:
                        replacement.extend(result)
                old[:] = replacement
            elif isinstance(old, AST):
                result = self.visit(old)
                if result is None:
                    delattr(node, name)
                else:
                    setattr(node, name, result)
        return node


# Classes kept only so that old code importing them keeps working.

class slice(AST):
    """Unused since Python 3.9; kept for compatibility."""


class Index(slice):
    """Unused since Python 3.9: ``Index(x)`` simply returns *x*."""

    def __new__(cls, value, **kwargs):
        return value


class ExtSlice(slice):
    """Unused since Python 3.9: builds the equivalent Tuple node."""

    def __new__(cls, dims=(), **kwargs):
        return Tuple(list(dims), Load(), **kwargs)


if not hasattr(Tuple, 'dims'):
    def _get_elts(self):
        return self.elts

    def _set_elts(self, value):
        self.elts = value

    Tuple.dims = property(_get_elts, _set_elts,
                          doc="Old name of the elts field.")
    del _get_elts, _set_elts


class Suite(mod):
    """Unused; kept for compatibility."""


class AugLoad(expr_context):
    """Unused; kept for compatibility."""


class AugStore(expr_context):
    """Unused; kept for compatibility."""


class Param(expr_context):
    """Unused; kept for compatibility."""


def unparse(ast_obj):
    """Generate Python source code equivalent to the tree *ast_obj*."""
    from _ast_unparse import unparse as _generate
    return _generate(ast_obj)


def main(args=None):
    """Command line interface: dump the AST of a file (or stdin)."""
    import argparse
    import sys

    cli = argparse.ArgumentParser(prog='python -m ast')
    cli.add_argument('infile', nargs='?', default='-',
                     help='file to parse (default: standard input)')
    cli.add_argument('-m', '--mode', default='exec',
                     choices=('exec', 'single', 'eval', 'func_type'),
                     help='compilation mode used for parsing')
    cli.add_argument('--no-type-comments', dest='type_comments',
                     default=True, action='store_false',
                     help='do not record type comments')
    cli.add_argument('-a', '--include-attributes', action='store_true',
                     help='also show line and column positions')
    cli.add_argument('-i', '--indent', type=int, default=3,
                     help='number of spaces per indentation level')
    cli.add_argument('--feature-version', default=None, metavar='VERSION',
                     help='grammar version to accept, e.g. 3.10')
    cli.add_argument('-O', '--optimize', type=int, default=-1, metavar='LEVEL',
                     help='AST optimization level (default -1)')
    cli.add_argument('--show-empty', action='store_true', default=False,
                     help='also show empty lists and None fields')
    opts = cli.parse_args(args)

    if opts.infile == '-':
        filename = '<stdin>'
        data = sys.stdin.buffer.read()
    else:
        filename = opts.infile
        with open(opts.infile, 'rb') as f:
            data = f.read()

    version = None
    if opts.feature_version:
        try:
            major, minor = (int(p) for p in opts.feature_version.split('.', 1))
        except ValueError:
            cli.error('Invalid format for --feature-version; '
                      'expected format 3.x (for example, 3.10)')
        version = (major, minor)

    tree = parse(data, filename, opts.mode, type_comments=opts.type_comments,
                 feature_version=version, optimize=opts.optimize)
    print(dump(tree, include_attributes=opts.include_attributes,
               indent=opts.indent, show_empty=opts.show_empty))


if __name__ == '__main__':
    main()
