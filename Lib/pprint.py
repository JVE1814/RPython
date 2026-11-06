"""Pretty-printing of Python data structures.

PrettyPrinter formats objects so that containers too wide for one line are
split over several lines.  Module functions: pprint, pp, pformat,
saferepr, isreadable, isrecursive.
"""

import sys as _sys
import types as _types
from io import StringIO as _StringIO

__all__ = ["pprint", "pformat", "isreadable", "isrecursive", "saferepr",
           "PrettyPrinter", "pp"]


def pprint(object, stream=None, indent=1, width=80, depth=None, *,
           compact=False, sort_dicts=True, underscore_numbers=False,
           expand=False):
    """Pretty-print a Python object to a stream [default is sys.stdout]."""
    printer = PrettyPrinter(
        stream=stream, indent=indent, width=width, depth=depth,
        compact=compact, sort_dicts=sort_dicts,
        underscore_numbers=underscore_numbers, expand=expand)
    printer.pprint(object)


def pformat(object, indent=1, width=80, depth=None, *,
            compact=False, sort_dicts=True, underscore_numbers=False,
            expand=False):
    """Format a Python object into a pretty-printed representation."""
    return PrettyPrinter(indent=indent, width=width, depth=depth,
                         compact=compact, sort_dicts=sort_dicts,
                         underscore_numbers=underscore_numbers,
                         expand=expand).pformat(object)


def pp(object, *args, sort_dicts=False, **kwargs):
    """Pretty-print a Python object (dicts keep their insertion order)."""
    pprint(object, *args, sort_dicts=sort_dicts, **kwargs)


def saferepr(object):
    """Version of repr() which can handle recursive data structures."""
    return PrettyPrinter()._safe_repr(object, {}, None, 0)[0]


def isreadable(object):
    """Determine if saferepr(object) is readable by eval()."""
    return PrettyPrinter()._safe_repr(object, {}, None, 0)[1]


def isrecursive(object):
    """Determine if object requires a recursive representation."""
    return PrettyPrinter()._safe_repr(object, {}, None, 0)[2]


class _safe_key:
    """Sort key that tolerates unorderable objects (falls back to the type
    name and id)."""

    __slots__ = ['obj']

    def __init__(self, obj):
        self.obj = obj

    def __lt__(self, other):
        try:
            return self.obj < other.obj
        except TypeError:
            return ((str(type(self.obj)), id(self.obj)) <
                    (str(type(other.obj)), id(other.obj)))


def _safe_tuple(t):
    "Helper function for comparing 2-tuples"
    return _safe_key(t[0]), _safe_key(t[1])


class PrettyPrinter:
    def __init__(self, indent=1, width=80, depth=None, stream=None, *,
                 compact=False, sort_dicts=True, underscore_numbers=False,
                 expand=False):
        """Handle pretty printing operations onto a stream.

        indent: number of spaces added per nesting level
        width: desired maximum line width
        depth: maximum nesting depth printed (deeper levels become '...')
        stream: output stream (default sys.stdout)
        compact: pack several short sequence items per line
        sort_dicts: sort dict items by key
        underscore_numbers: use '_' thousands separators for ints
        expand: put each item of a split container on its own line, with
                the closing bracket on a line of its own
        """
        indent = int(indent)
        width = int(width)
        if indent < 0:
            raise ValueError('indent must be >= 0')
        if depth is not None and depth <= 0:
            raise ValueError('depth must be > 0')
        if not width:
            raise ValueError('width must be != 0')
        if compact and expand:
            raise ValueError('compact and expand are mutually exclusive')
        self._depth = depth
        self._indent_per_level = indent
        self._width = width
        if stream is not None:
            self._stream = stream
        else:
            self._stream = _sys.stdout
        self._compact = bool(compact)
        self._sort_dicts = sort_dicts
        self._underscore_numbers = underscore_numbers
        self._expand = bool(expand)

    def pprint(self, object):
        if self._stream is not None:
            self._format(object, self._stream, 0, 0, {}, 0)
            self._stream.write("\n")

    def pformat(self, object):
        sio = _StringIO()
        self._format(object, sio, 0, 0, {}, 0)
        return sio.getvalue()

    def isrecursive(self, object):
        return self.format(object, {}, 0, 0)[2]

    def isreadable(self, object):
        s, readable, recursive = self.format(object, {}, 0, 0)
        return readable and not recursive

    def _format(self, object, stream, indent, allowance, context, level):
        objid = id(object)
        if objid in context:
            stream.write(_recursion(object))
            self._recursive = True
            self._readable = False
            return
        rep = self._repr(object, context, level)
        max_width = self._width - indent - allowance
        if len(rep) > max_width:
            if self._pretty(object, stream, indent, allowance, context, level):
                return
        stream.write(rep)

    def _pretty(self, object, stream, indent, allowance, context, level):
        """Write object split over several lines if a specialised printer
        exists for its type; return False (writing nothing) otherwise."""
        objid = id(object)
        p = self._dispatch.get(type(object).__repr__, None)
        if p is None:
            # Dataclasses are recognised by their generated __repr__.
            from dataclasses import is_dataclass
            if (is_dataclass(object) and
                    not isinstance(object, type) and
                    object.__dataclass_params__.repr and
                    hasattr(object.__repr__, "__wrapped__") and
                    "__create_fn__" in object.__repr__.__wrapped__.__qualname__):
                p = PrettyPrinter._pprint_dataclass
            else:
                return False
        context[objid] = 1
        try:
            p(self, object, stream, indent, allowance, context, level + 1)
        finally:
            del context[objid]
        return True

    # -- helpers for expand mode -----------------------------------------

    def _child_indent(self, indent):
        return indent + self._indent_per_level

    def _write_expanded_items(self, items, stream, indent, context, level,
                              fmt_item):
        """Write each item on its own line at indent + indent_per_level,
        each followed by ',' and a newline."""
        child = self._child_indent(indent)
        for item in items:
            stream.write(' ' * child)
            fmt_item(item, child)
            stream.write(',\n')

    # -- dataclasses / namespaces ------------------------------------------

    def _pprint_dataclass(self, object, stream, indent, allowance, context, level):
        from dataclasses import fields as dataclass_fields
        cls_name = object.__class__.__name__
        items = [(f.name, getattr(object, f.name))
                 for f in dataclass_fields(object) if f.repr]
        if self._expand:
            stream.write(cls_name + '(\n')
            self._format_namespace_items_expanded(items, stream, indent,
                                                  context, level)
            stream.write(' ' * indent + ')')
            return
        indent += len(cls_name) + 1
        stream.write(cls_name + '(')
        self._format_namespace_items(items, stream, indent, allowance, context, level)
        stream.write(')')

    _dispatch = {}

    def _pprint_dict(self, object, stream, indent, allowance, context, level):
        write = stream.write
        write('{')
        if self._sort_dicts:
            items = sorted(object.items(), key=_safe_tuple)
        else:
            items = object.items()
        if self._expand:
            self._format_dict_items_expanded(items, stream, indent, context,
                                             level)
            write('}')
            return
        if self._indent_per_level > 1:
            write((self._indent_per_level - 1) * ' ')
        length = len(object)
        if length:
            self._format_dict_items(items, stream, indent, allowance + 1,
                                    context, level)
        write('}')

    _dispatch[dict.__repr__] = _pprint_dict

    def _pprint_ordered_dict(self, object, stream, indent, allowance, context, level):
        if not len(object):
            stream.write(repr(object))
            return
        cls = object.__class__
        if self._expand:
            # Same shape as the non-expanded form: a list of pairs.
            stream.write(cls.__name__ + '([')
            self._format_items(list(object.items()), stream, indent, 2,
                               context, level)
            stream.write('])')
            return
        stream.write(cls.__name__ + '(')
        self._format(list(object.items()), stream,
                     indent + len(cls.__name__) + 1, allowance + 1,
                     context, level)
        stream.write(')')

    def _pprint_list(self, object, stream, indent, allowance, context, level):
        stream.write('[')
        self._format_items(object, stream, indent, allowance + 1,
                           context, level)
        stream.write(']')

    _dispatch[list.__repr__] = _pprint_list

    def _pprint_tuple(self, object, stream, indent, allowance, context, level):
        stream.write('(')
        endchar = ',)' if len(object) == 1 and not self._expand else ')'
        self._format_items(object, stream, indent, allowance + len(endchar),
                           context, level)
        stream.write(endchar)

    _dispatch[tuple.__repr__] = _pprint_tuple

    def _pprint_set(self, object, stream, indent, allowance, context, level):
        if not len(object):
            stream.write(repr(object))
            return
        typ = object.__class__
        if typ is set:
            stream.write('{')
            endchar = '}'
        else:
            stream.write(typ.__name__ + '({')
            endchar = '})'
            if not self._expand:
                indent += len(typ.__name__) + 1
        object = sorted(object, key=_safe_key)
        self._format_items(object, stream, indent, allowance + len(endchar),
                           context, level)
        stream.write(endchar)

    _dispatch[set.__repr__] = _pprint_set
    _dispatch[frozenset.__repr__] = _pprint_set

    def _pprint_str(self, object, stream, indent, allowance, context, level):
        write = stream.write
        if not len(object):
            write(repr(object))
            return
        chunks = []
        lines = object.splitlines(True)
        if level == 1:
            indent += 1
            allowance += 1
        # Only a top-level string gets its own parenthesised block in expand
        # mode; nested ones continue at the current indentation.
        expand_block = self._expand and level == 1
        if expand_block:
            max_width1 = max_width = self._width - self._child_indent(indent - 1)
        else:
            max_width1 = max_width = self._width - indent
        for i, line in enumerate(lines):
            rep = repr(line)
            if i == len(lines) - 1:
                max_width1 -= allowance
            if len(rep) <= max_width1:
                chunks.append(rep)
            else:
                # Split the line at whitespace boundaries.
                import re
                parts = re.findall(r'\S*\s*', line)
                assert parts
                assert not parts[-1]
                parts.pop()  # drop the empty last part
                max_width2 = max_width
                current = ''
                for j, part in enumerate(parts):
                    candidate = current + part
                    if j == len(parts) - 1 and i == len(lines) - 1:
                        max_width2 -= allowance
                    if len(repr(candidate)) > max_width2:
                        if current:
                            chunks.append(repr(current))
                        current = part
                    else:
                        current = candidate
                if current:
                    chunks.append(repr(current))
        if len(chunks) == 1:
            write(rep)
            return
        if expand_block:
            base = indent - 1
            write('(\n')
            child = self._child_indent(base)
            for rep in chunks:
                write(' ' * child + rep + '\n')
            write(' ' * base + ')')
            return
        if level == 1:
            write('(')
        for i, rep in enumerate(chunks):
            if i > 0:
                write('\n' + ' ' * indent)
            write(rep)
        if level == 1:
            write(')')

    _dispatch[str.__repr__] = _pprint_str

    def _pprint_bytes(self, object, stream, indent, allowance, context, level):
        write = stream.write
        if len(object) <= 4:
            write(repr(object))
            return
        parens = level == 1
        if parens:
            indent += 1
            allowance += 1
            if self._expand:
                write('(\n')
                child = self._child_indent(indent - 1)
                for rep in _wrap_bytes_repr(object, self._width - child, 0):
                    write(' ' * child + rep + '\n')
                write(' ' * (indent - 1) + ')')
                return
            write('(')
        delim = ''
        for rep in _wrap_bytes_repr(object, self._width - indent, allowance):
            write(delim)
            write(rep)
            if not delim:
                delim = '\n' + ' ' * indent
        if parens:
            write(')')

    _dispatch[bytes.__repr__] = _pprint_bytes

    def _pprint_bytearray(self, object, stream, indent, allowance, context, level):
        write = stream.write
        if self._expand:
            write('bytearray(\n')
            child = self._child_indent(indent)
            for rep in _wrap_bytes_repr(bytes(object), self._width - child, 0):
                write(' ' * child + rep + '\n')
            write(' ' * indent + ')')
            return
        write('bytearray(')
        self._pprint_bytes(bytes(object), stream, indent + 10,
                           allowance + 1, context, level + 1)
        write(')')

    _dispatch[bytearray.__repr__] = _pprint_bytearray

    def _pprint_mappingproxy(self, object, stream, indent, allowance, context, level):
        if self._expand:
            stream.write('mappingproxy({')
            items = object.items()
            if self._sort_dicts:
                items = sorted(items, key=_safe_tuple)
            self._format_dict_items_expanded(items, stream, indent, context,
                                             level)
            stream.write('})')
            return
        stream.write('mappingproxy(')
        self._format(object.copy(), stream, indent + 13, allowance + 1,
                     context, level)
        stream.write(')')

    _dispatch[_types.MappingProxyType.__repr__] = _pprint_mappingproxy

    def _pprint_simplenamespace(self, object, stream, indent, allowance, context, level):
        if type(object) is _types.SimpleNamespace:
            # The SimpleNamespace repr is "namespace" instead of the class
            # name, so we do the same here.
            cls_name = 'namespace'
        else:
            cls_name = object.__class__.__name__
        items = object.__dict__.items()
        if self._expand:
            stream.write(cls_name + '(\n')
            self._format_namespace_items_expanded(items, stream, indent,
                                                  context, level)
            stream.write(' ' * indent + ')')
            return
        indent += len(cls_name) + 1
        stream.write(cls_name + '(')
        self._format_namespace_items(items, stream, indent, allowance, context, level)
        stream.write(')')

    _dispatch[_types.SimpleNamespace.__repr__] = _pprint_simplenamespace

    def _format_dict_items(self, items, stream, indent, allowance, context,
                           level):
        write = stream.write
        indent += self._indent_per_level
        delimnl = ',\n' + ' ' * indent
        last_index = len(items) - 1
        for i, (key, ent) in enumerate(items):
            last = i == last_index
            rep = self._repr(key, context, level)
            write(rep)
            write(': ')
            self._format(ent, stream, indent + len(rep) + 2,
                         allowance if last else 1,
                         context, level)
            if not last:
                write(delimnl)

    def _format_dict_items_expanded(self, items, stream, indent, context,
                                    level):
        items = list(items)
        if not items:
            return
        write = stream.write
        write('\n')
        child = self._child_indent(indent)
        for key, ent in items:
            rep = self._repr(key, context, level)
            write(' ' * child + rep + ': ')
            self._format_expanded_value(ent, stream, child, len(rep) + 2,
                                        context, level)
            write(',\n')
        write(' ' * indent)

    def _format_expanded_value(self, ent, stream, indent, prefix_len,
                               context, level):
        """Format a value that follows a prefix of prefix_len characters on
        a line indented by indent; nested containers are expanded relative
        to indent."""
        if id(ent) in context:
            self._format(ent, stream, indent, 1, context, level)
            return
        rep = self._repr(ent, context, level)
        # The value shares its line with the prefix and is followed by ','.
        if len(rep) + prefix_len + 1 <= self._width - indent:
            stream.write(rep)
            return
        if not self._pretty(ent, stream, indent, 1, context, level):
            stream.write(rep)

    def _format_namespace_items(self, items, stream, indent, allowance, context, level):
        write = stream.write
        delimnl = ',\n' + ' ' * indent
        last_index = len(items) - 1
        for i, (key, ent) in enumerate(items):
            last = i == last_index
            write(key)
            write('=')
            if id(ent) in context:
                # Special-case representation of recursion to match standard
                # recursive dataclass repr.
                write("...")
            else:
                self._format(ent, stream, indent + len(key) + 1,
                             allowance if last else 1,
                             context, level)
            if not last:
                write(delimnl)

    def _format_namespace_items_expanded(self, items, stream, indent,
                                         context, level):
        write = stream.write
        child = self._child_indent(indent)
        for key, ent in items:
            write(' ' * child + key + '=')
            if id(ent) in context:
                write("...")
            else:
                self._format_expanded_value(ent, stream, child, len(key) + 1,
                                            context, level)
            write(',\n')

    def _format_items(self, items, stream, indent, allowance, context, level):
        write = stream.write
        if self._expand:
            items = list(items)
            if not items:
                return
            write('\n')
            child = self._child_indent(indent)
            for ent in items:
                write(' ' * child)
                self._format(ent, stream, child, 1, context, level)
                write(',\n')
            write(' ' * indent)
            return
        indent += self._indent_per_level
        if self._indent_per_level > 1:
            write((self._indent_per_level - 1) * ' ')
        delimnl = ',\n' + ' ' * indent
        delim = ''
        width = max_width = self._width - indent + 1
        it = iter(items)
        try:
            next_ent = next(it)
        except StopIteration:
            return
        last = False
        while not last:
            ent = next_ent
            try:
                next_ent = next(it)
            except StopIteration:
                last = True
                max_width -= allowance
                width -= allowance
            if self._compact:
                rep = self._repr(ent, context, level)
                w = len(rep) + 2
                if width < w:
                    width = max_width
                    if delim:
                        delim = delimnl
                if width >= w:
                    width -= w
                    write(delim)
                    delim = ', '
                    write(rep)
                    continue
            write(delim)
            delim = delimnl
            self._format(ent, stream, indent,
                         allowance if last else 1,
                         context, level)

    def _repr(self, object, context, level):
        repr, readable, recursive = self.format(object, context.copy(),
                                                self._depth, level)
        if not readable:
            self._readable = False
        if recursive:
            self._recursive = True
        return repr

    def format(self, object, context, maxlevels, level):
        """Format object for a specific context, returning a string
        and flags indicating whether the representation is 'readable'
        and whether the object represents a recursive construct.
        """
        return self._safe_repr(object, context, maxlevels, level)

    def _pprint_default_dict(self, object, stream, indent, allowance, context, level):
        if not len(object):
            stream.write(repr(object))
            return
        rdf = self._repr(object.default_factory, context, level)
        cls = object.__class__
        if self._expand:
            stream.write('%s(%s, {' % (cls.__name__, rdf))
            items = object.items()
            if self._sort_dicts:
                items = sorted(items, key=_safe_tuple)
            self._format_dict_items_expanded(items, stream, indent, context,
                                             level)
            stream.write('})')
            return
        indent += len(cls.__name__) + 1
        stream.write('%s(%s,\n%s' % (cls.__name__, rdf, ' ' * indent))
        self._pprint_dict(object, stream, indent, allowance + 1, context, level)
        stream.write(')')

    def _pprint_counter(self, object, stream, indent, allowance, context, level):
        if not len(object):
            stream.write(repr(object))
            return
        cls = object.__class__
        items = object.most_common()
        if self._expand:
            stream.write(cls.__name__ + '({')
            self._format_dict_items_expanded(items, stream, indent, context,
                                             level)
            stream.write('})')
            return
        stream.write(cls.__name__ + '({')
        if self._indent_per_level > 1:
            stream.write((self._indent_per_level - 1) * ' ')
        self._format_dict_items(items, stream,
                                indent + len(cls.__name__) + 1, allowance + 2,
                                context, level)
        stream.write('})')

    def _pprint_chain_map(self, object, stream, indent, allowance, context, level):
        if not len(object.maps) or (len(object.maps) == 1 and not len(object.maps[0])):
            stream.write(repr(object))
            return
        cls = object.__class__
        if self._expand:
            stream.write(cls.__name__ + '(\n')
            child = self._child_indent(indent)
            for m in object.maps:
                stream.write(' ' * child)
                self._format(m, stream, child, 1, context, level)
                stream.write(',\n')
            stream.write(' ' * indent + ')')
            return
        stream.write(cls.__name__ + '(')
        indent += len(cls.__name__) + 1
        for i, m in enumerate(object.maps):
            if i == len(object.maps) - 1:
                self._format(m, stream, indent, allowance + 1, context, level)
                stream.write(')')
            else:
                self._format(m, stream, indent, 1, context, level)
                stream.write(',\n' + ' ' * indent)

    def _pprint_deque(self, object, stream, indent, allowance, context, level):
        if not len(object):
            stream.write(repr(object))
            return
        cls = object.__class__
        stream.write(cls.__name__ + '(')
        if not self._expand:
            indent += len(cls.__name__) + 1
        stream.write('[')
        if object.maxlen is None:
            self._format_items(object, stream, indent, allowance + 2,
                               context, level)
            stream.write('])')
        else:
            self._format_items(object, stream, indent, 2,
                               context, level)
            rml = self._repr(object.maxlen, context, level)
            if self._expand:
                stream.write('], maxlen=%s)' % rml)
            else:
                stream.write('],\n%smaxlen=%s)' % (' ' * indent, rml))

    def _pprint_user_dict(self, object, stream, indent, allowance, context, level):
        self._format(object.data, stream, indent, allowance, context, level - 1)

    def _pprint_user_list(self, object, stream, indent, allowance, context, level):
        self._format(object.data, stream, indent, allowance, context, level - 1)

    def _pprint_user_string(self, object, stream, indent, allowance, context, level):
        self._format(object.data, stream, indent, allowance, context, level - 1)

    def _pprint_template(self, object, stream, indent, allowance, context, level):
        cls_name = object.__class__.__name__
        if self._expand:
            stream.write(cls_name + '(\n')
            self._format_namespace_items_expanded(
                [('strings', object.strings),
                 ('interpolations', object.interpolations)],
                stream, indent, context, level)
            stream.write(' ' * indent + ')')
            return
        indent += len(cls_name) + 1
        stream.write(cls_name + '(')
        self._format_namespace_items(
            [('strings', object.strings),
             ('interpolations', object.interpolations)],
            stream, indent, allowance, context, level)
        stream.write(')')

    def _pprint_interpolation(self, object, stream, indent, allowance, context, level):
        cls_name = object.__class__.__name__
        if self._expand:
            stream.write(cls_name + '(\n')
            self._format_namespace_items_expanded(
                [('value', object.value),
                 ('expression', object.expression),
                 ('conversion', object.conversion),
                 ('format_spec', object.format_spec)],
                stream, indent, context, level)
            stream.write(' ' * indent + ')')
            return
        # Positional fields, aligned just after the opening parenthesis.
        indent += len(cls_name) + 1
        items = (object.value, object.expression,
                 object.conversion, object.format_spec)
        stream.write(cls_name + '(')
        for i, item in enumerate(items):
            if i:
                stream.write(',\n' + ' ' * indent)
            last = i == len(items) - 1
            self._format(item, stream, indent, allowance + 1 if last else 1,
                         context, level)
        stream.write(')')

    def _pprint_dict_view(self, object, stream, indent, allowance, context, level):
        """Pretty print dict views (keys, values, items)."""
        if isinstance(object, self._dict_items_view):
            key = _safe_tuple
        else:
            key = _safe_key
        write = stream.write
        write(object.__class__.__name__ + '([')
        if len(object):
            if self._sort_dicts:
                entries = sorted(object, key=key)
            else:
                entries = list(object)
            # Continuation lines are indented relative to the view's start,
            # not to its opening bracket.
            self._format_items(entries, stream, indent, allowance + 2,
                               context, level)
        write('])')

    def _pprint_mapping_abc_view(self, object, stream, indent, allowance, context, level):
        """Pretty print mapping views from collections.abc."""
        write = stream.write
        write(object.__class__.__name__ + '(')
        # Dispatch formatting to the view's _mapping
        self._format(object._mapping, stream, indent, allowance, context, level)
        write(')')

    _dict_keys_view = type({}.keys())
    _dict_values_view = type({}.values())
    _dict_items_view = type({}.items())

    def _safe_repr(self, object, context, maxlevels, level):
        # Return triple (repr_string, isreadable, isrecursive).
        typ = type(object)
        if typ in _builtin_scalars:
            return repr(object), True, False

        r = getattr(typ, "__repr__", None)

        if issubclass(typ, int) and r is int.__repr__:
            if self._underscore_numbers:
                return f"{object:_d}", True, False
            else:
                return repr(object), True, False

        if issubclass(typ, dict) and r is dict.__repr__:
            if not object:
                return "{}", True, False
            objid = id(object)
            if maxlevels and level >= maxlevels:
                return "{...}", False, objid in context
            if objid in context:
                return _recursion(object), False, True
            context[objid] = 1
            readable = True
            recursive = False
            components = []
            append = components.append
            level += 1
            if self._sort_dicts:
                items = sorted(object.items(), key=_safe_tuple)
            else:
                items = object.items()
            for k, v in items:
                krepr, kreadable, krecur = self.format(
                    k, context, maxlevels, level)
                vrepr, vreadable, vrecur = self.format(
                    v, context, maxlevels, level)
                append("%s: %s" % (krepr, vrepr))
                readable = readable and kreadable and vreadable
                if krecur or vrecur:
                    recursive = True
            del context[objid]
            return "{%s}" % ", ".join(components), readable, recursive

        if (issubclass(typ, list) and r is list.__repr__) or \
           (issubclass(typ, tuple) and r is tuple.__repr__):
            if issubclass(typ, list):
                if not object:
                    return "[]", True, False
                format = "[%s]"
            elif len(object) == 1:
                format = "(%s,)"
            else:
                if not object:
                    return "()", True, False
                format = "(%s)"
            objid = id(object)
            if maxlevels and level >= maxlevels:
                return format % "...", False, objid in context
            if objid in context:
                return _recursion(object), False, True
            context[objid] = 1
            readable = True
            recursive = False
            components = []
            append = components.append
            level += 1
            for o in object:
                orepr, oreadable, orecur = self.format(
                    o, context, maxlevels, level)
                append(orepr)
                if not oreadable:
                    readable = False
                if orecur:
                    recursive = True
            del context[objid]
            return format % ", ".join(components), readable, recursive

        if issubclass(typ, _dict_view_types) and r in _dict_view_reprs:
            return self._safe_view_repr(object, typ, context, maxlevels, level)

        if _MappingView is not None and r is _MappingView.__repr__ \
                and issubclass(typ, _MappingView):
            if maxlevels and level >= maxlevels:
                return "{...}", False, id(object) in context
            mrepr, readable, recursive = self.format(
                object._mapping, context, maxlevels, level)
            return "%s(%s)" % (typ.__name__, mrepr), readable, recursive

        rep = repr(object)
        return rep, (rep and not rep.startswith('<')), False

    def _safe_view_repr(self, object, typ, context, maxlevels, level):
        # keys()/values()/items() views of dicts: "name([item, ...])".
        if not object:
            return "%s([])" % typ.__name__, True, False
        objid = id(object)
        if maxlevels and level >= maxlevels:
            return "{...}", False, objid in context
        if objid in context:
            return _recursion(object), False, True
        context[objid] = 1
        entries = list(object)
        if self._sort_dicts:
            if issubclass(typ, PrettyPrinter._dict_items_view):
                entries.sort(key=_safe_tuple)
            else:
                entries.sort(key=_safe_key)
        readable = True
        recursive = False
        components = []
        level += 1
        for o in entries:
            orepr, oreadable, orecur = self.format(o, context, maxlevels, level)
            components.append(orepr)
            readable = readable and oreadable
            recursive = recursive or orecur
        del context[objid]
        return ("%s([%s])" % (typ.__name__, ", ".join(components)),
                readable, recursive)


_dict_view_types = (PrettyPrinter._dict_keys_view,
                    PrettyPrinter._dict_values_view,
                    PrettyPrinter._dict_items_view)
_dict_view_reprs = tuple(t.__repr__ for t in _dict_view_types)

try:
    from _collections_abc import MappingView as _MappingView
except ImportError:
    _MappingView = None


_builtin_scalars = frozenset({str, bytes, bytearray, float, complex,
                              bool, type(None)})


def _recursion(object):
    return ("<Recursion on %s with id=%s>"
            % (type(object).__name__, id(object)))


def _wrap_bytes_repr(object, width, allowance):
    current = b''
    last = len(object) // 4 * 4
    for i in range(0, len(object), 4):
        part = object[i: i+4]
        candidate = current + part
        if i == last:
            width -= allowance
        if len(repr(candidate)) > width:
            if current:
                yield repr(current)
            current = part
        else:
            current = candidate
    if current:
        yield repr(current)


def _register_lazy_types():
    # Types from modules that pprint does not import at load time.
    d = PrettyPrinter._dispatch
    import collections
    d[collections.OrderedDict.__repr__] = PrettyPrinter._pprint_ordered_dict
    d[collections.defaultdict.__repr__] = PrettyPrinter._pprint_default_dict
    d[collections.Counter.__repr__] = PrettyPrinter._pprint_counter
    d[collections.ChainMap.__repr__] = PrettyPrinter._pprint_chain_map
    d[collections.deque.__repr__] = PrettyPrinter._pprint_deque
    d[collections.UserDict.__repr__] = PrettyPrinter._pprint_user_dict
    d[collections.UserList.__repr__] = PrettyPrinter._pprint_user_list
    d[collections.UserString.__repr__] = PrettyPrinter._pprint_user_string
    d[PrettyPrinter._dict_keys_view.__repr__] = PrettyPrinter._pprint_dict_view
    d[PrettyPrinter._dict_values_view.__repr__] = PrettyPrinter._pprint_dict_view
    d[PrettyPrinter._dict_items_view.__repr__] = PrettyPrinter._pprint_dict_view
    import collections.abc as _abc
    d[_abc.MappingView.__repr__] = PrettyPrinter._pprint_mapping_abc_view
    try:
        from string.templatelib import Template, Interpolation
    except ImportError:
        pass
    else:
        d[Template.__repr__] = PrettyPrinter._pprint_template
        d[Interpolation.__repr__] = PrettyPrinter._pprint_interpolation
    fd = getattr(__import__('builtins'), 'frozendict', None)
    if fd is not None:
        d[fd.__repr__] = PrettyPrinter._pprint_frozendict


def _pprint_frozendict(self, object, stream, indent, allowance, context, level):
    cls = object.__class__
    if not len(object):
        stream.write(repr(object))
        return
    items = object.items()
    if self._sort_dicts:
        items = sorted(items, key=_safe_tuple)
    if self._expand:
        stream.write(cls.__name__ + '({')
        self._format_dict_items_expanded(items, stream, indent, context, level)
        stream.write('})')
        return
    stream.write(cls.__name__ + '({')
    self._format_dict_items(list(items), stream,
                            indent + len(cls.__name__) + 1, allowance + 2,
                            context, level)
    stream.write('})')


PrettyPrinter._pprint_frozendict = _pprint_frozendict
del _pprint_frozendict

try:
    _register_lazy_types()
except ImportError:
    pass
