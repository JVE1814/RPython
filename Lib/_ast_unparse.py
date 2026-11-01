"""Source code generation from AST objects (the engine behind ast.unparse).

Design: expressions are rendered by ``_Unparser.expr(node, ctx)``, which
returns a string; *ctx* is the minimum binding strength the surrounding code
needs, and an expression whose own strength is weaker gets parenthesised.
Statements are appended line by line to an output buffer.
"""

import sys
from _ast import *

try:
    from enum import IntEnum, auto, _simple_enum
except ImportError:  # very early bootstrap: no enum module yet
    IntEnum = None


# Literal infinities cannot be written directly; an overflowing exponent
# parses back to inf.
_HUGE = "1e" + repr(sys.float_info.max_10_exp + 1)

_LEVEL_NAMES = (
    "NAMED_EXPR", "TUPLE", "YIELD", "TEST", "OR", "AND", "NOT", "CMP",
    "EXPR", "BXOR", "BAND", "SHIFT", "ARITH", "TERM", "FACTOR", "POWER",
    "AWAIT", "ATOM",
)

if IntEnum is not None:
    @_simple_enum(IntEnum)
    class _Precedence:
        """Binding strength of expression forms, weakest first."""

        NAMED_EXPR = auto()
        TUPLE = auto()
        YIELD = auto()
        TEST = auto()
        OR = auto()
        AND = auto()
        NOT = auto()
        CMP = auto()
        EXPR = auto()
        BOR = EXPR
        BXOR = auto()
        BAND = auto()
        SHIFT = auto()
        ARITH = auto()
        TERM = auto()
        FACTOR = auto()
        POWER = auto()
        AWAIT = auto()
        ATOM = auto()

        def next(self):
            try:
                return self.__class__(self + 1)
            except ValueError:
                return self
else:
    class _Level(int):
        def next(self):
            return _Level(min(self + 1, len(_LEVEL_NAMES)))

    class _Precedence:
        pass

    for _pos, _nm in enumerate(_LEVEL_NAMES, 1):
        setattr(_Precedence, _nm, _Level(_pos))
    _Precedence.BOR = _Precedence.EXPR
    del _pos, _nm

_P = _Precedence

_TRIPLE = ('"""', "'''")
_QUOTES = ("'", '"') + _TRIPLE

_BINOPS = {
    Add: ("+", _P.ARITH), Sub: ("-", _P.ARITH),
    Mult: ("*", _P.TERM), MatMult: ("@", _P.TERM), Div: ("/", _P.TERM),
    Mod: ("%", _P.TERM), FloorDiv: ("//", _P.TERM),
    LShift: ("<<", _P.SHIFT), RShift: (">>", _P.SHIFT),
    BitOr: ("|", _P.BOR), BitXor: ("^", _P.BXOR), BitAnd: ("&", _P.BAND),
    Pow: ("**", _P.POWER),
}
_UNARY = {Invert: ("~", _P.FACTOR), UAdd: ("+", _P.FACTOR),
          USub: ("-", _P.FACTOR), Not: ("not ", _P.NOT)}
_CMP = {Eq: "==", NotEq: "!=", Lt: "<", LtE: "<=", Gt: ">", GtE: ">=",
        Is: "is", IsNot: "is not", In: "in", NotIn: "not in"}


def _lookup(table, op):
    for cls in type(op).__mro__:
        if cls in table:
            return table[cls]
    raise ValueError(f"unknown operator {op!r}")


def _escape_text(text, keep_ws):
    """Escape backslashes and unprintable characters; newline and tab are
    left alone when *keep_ws* is true."""
    out = []
    for ch in text:
        if keep_ws and (ch == "\n" or ch == "\t"):
            out.append(ch)
        elif ch == "\\" or not ch.isprintable():
            out.append(ch.encode("unicode_escape").decode("ascii"))
        else:
            out.append(ch)
    return "".join(out)


def _quote_options(text, allowed, keep_ws=True):
    """Find delimiters able to enclose *text* (escaped as needed).

    Returns ``(body, delimiters)``: the escaped body and the delimiters (in
    order of preference) that can be used around it.
    """
    body = _escape_text(text, keep_ws)
    ok = [q for q in allowed
          if q not in body and ("\n" not in body or len(q) == 3)]
    if not ok:
        rep = repr(text)
        first = rep[0]
        pick = next((q for q in allowed if first in q), first)
        return rep[1:-1], [pick]
    if body:
        last = body[-1]
        # a delimiter whose quote character ends the body is a last resort
        ok = [q for q in ok if q[0] != last] + [q for q in ok if q[0] == last]
        if ok[0][0] == last:
            body = body[:-1] + "\\" + last
    return body, ok


class _Unparser:
    """Turns an AST into Python source text."""

    def __init__(self):
        self._out = []
        self._level = 0
        self._ignores = {}
        self._interactive = False
        self._try_star = False

    # ----------------------------------------------------------------- output

    def _line(self, text="", compound=False):
        """Start a new logical line containing *text*.

        At the top level of an Interactive tree, simple statements are
        joined with semicolons instead of newlines."""
        if self._interactive and not self._level and not compound:
            if self._out:
                self._out.append("; ")
            self._out.append(text)
            return
        if self._out:
            self._out.append("\n")
        self._out.append("    " * self._level + text)

    def _emit(self, text):
        self._out.append(text)

    def _suite(self, stmts, comment=None):
        self._emit(":")
        if comment:
            self._emit(comment)
        self._level += 1
        try:
            self.stmts(stmts)
        finally:
            self._level -= 1

    def visit(self, node):
        self._out = []
        if isinstance(node, list):
            self.stmts(node)
        elif isinstance(node, (mod, stmt, excepthandler)):
            self.stmt(node)
        else:
            self._out.append(self.render(node))
        return "".join(self._out)

    # ------------------------------------------------------------ dispatching

    def stmts(self, nodes):
        for n in nodes:
            self.stmt(n)

    def stmt(self, node):
        handler = getattr(self, "s_" + type(node).__name__, None)
        if handler is None:
            for cls in type(node).__mro__[1:]:
                handler = getattr(self, "s_" + cls.__name__, None)
                if handler is not None:
                    break
            else:
                raise ValueError(f"cannot unparse {node!r}")
        handler(node)

    def render(self, node, ctx=_P.TEST):
        """Source text for a non-statement node in a context that needs at
        least binding strength *ctx*."""
        handler = getattr(self, "e_" + type(node).__name__, None)
        if handler is None:
            for cls in type(node).__mro__[1:]:
                handler = getattr(self, "e_" + cls.__name__, None)
                if handler is not None:
                    break
            else:
                raise ValueError(f"cannot unparse {node!r}")
        return handler(node, ctx)

    expr = render

    @staticmethod
    def _wrap(text, own, ctx):
        return f"({text})" if ctx > own else text

    def _join(self, nodes, ctx=_P.TEST, sep=", "):
        return sep.join(self.render(n, ctx) for n in nodes)

    def _seq(self, nodes, ctx=_P.TEST):
        """Comma separated items, with the trailing comma a 1-tuple needs."""
        text = self._join(nodes, ctx)
        return text + "," if len(nodes) == 1 else text

    # ------------------------------------------------------------- modules

    @staticmethod
    def _docstring_node(node):
        body = getattr(node, "body", None)
        if not body or not isinstance(body[0], Expr):
            return None
        value = body[0].value
        if isinstance(value, Constant) and isinstance(value.value, str):
            return value
        return None

    def _body_with_docstring(self, node):
        doc = self._docstring_node(node)
        if doc is None:
            self.stmts(node.body)
            return
        self._line(compound=True)
        if doc.kind == "u":
            self._emit("u")
        body, delims = _quote_options(doc.value, _TRIPLE)
        self._emit(delims[0] + body + delims[0])
        self.stmts(node.body[1:])

    def s_Module(self, node):
        self._ignores = {ti.lineno: "ignore" + ti.tag for ti in node.type_ignores}
        try:
            self._body_with_docstring(node)
        finally:
            self._ignores = {}

    def s_Interactive(self, node):
        saved = self._interactive
        self._interactive = True
        try:
            self.stmts(node.body)
        finally:
            self._interactive = saved

    def s_Expression(self, node):
        self._out.append(self.render(node.body))

    def s_FunctionType(self, node):
        args = self._join(node.argtypes)
        self._out.append(f"({args}) -> {self.render(node.returns)}")

    def _type_comment(self, node):
        text = self._ignores.get(node.lineno) or node.type_comment
        return None if text is None else " # type: " + text

    # ---------------------------------------------------------- statements

    def s_Expr(self, node):
        self._line(self.render(node.value, _P.YIELD))

    def s_Assign(self, node):
        parts = [self.render(t, _P.TUPLE) + " = " for t in node.targets]
        parts.append(self.render(node.value))
        comment = self._type_comment(node)
        if comment:
            parts.append(comment)
        self._line("".join(parts))

    def s_AugAssign(self, node):
        sym = _lookup(_BINOPS, node.op)[0]
        self._line(f"{self.render(node.target)} {sym}= {self.render(node.value)}")

    def s_AnnAssign(self, node):
        target = self.render(node.target)
        if not node.simple and isinstance(node.target, Name):
            target = f"({target})"
        text = f"{target}: {self.render(node.annotation)}"
        if node.value:
            text += " = " + self.render(node.value)
        self._line(text)

    def s_Return(self, node):
        self._line("return" if not node.value else "return " + self.render(node.value))

    def s_Pass(self, node):
        self._line("pass")

    def s_Break(self, node):
        self._line("break")

    def s_Continue(self, node):
        self._line("continue")

    def s_Delete(self, node):
        self._line("del " + self._join(node.targets))

    def s_Assert(self, node):
        text = "assert " + self.render(node.test)
        if node.msg:
            text += ", " + self.render(node.msg)
        self._line(text)

    def s_Global(self, node):
        self._line("global " + ", ".join(node.names))

    def s_Nonlocal(self, node):
        self._line("nonlocal " + ", ".join(node.names))

    def s_Raise(self, node):
        if not node.exc:
            if node.cause:
                raise ValueError("Node can't use cause without an exception.")
            self._line("raise")
            return
        text = "raise " + self.render(node.exc)
        if node.cause:
            text += " from " + self.render(node.cause)
        self._line(text)

    def _names(self, names):
        return ", ".join(self.render(a) for a in names)

    def s_Import(self, node):
        lazy = "lazy " if getattr(node, "is_lazy", 0) else ""
        self._line(f"{lazy}import {self._names(node.names)}")

    def s_ImportFrom(self, node):
        lazy = "lazy " if getattr(node, "is_lazy", 0) else ""
        where = "." * (node.level or 0) + (node.module or "")
        self._line(f"{lazy}from {where} import {self._names(node.names)}")

    def s_TypeAlias(self, node):
        self._line("type " + self.render(node.name) + self._type_params(node.type_params)
                   + " = " + self.render(node.value))

    def _decorators(self, node):
        if self._out:
            self._emit("\n")
        for d in node.decorator_list:
            self._line("@" + self.render(d), compound=True)

    def _type_params(self, params):
        if not params:
            return ""
        return "[" + self._join(params) + "]"

    def _def(self, node, keyword):
        self._decorators(node)
        head = f"{keyword} {node.name}"
        head += self._type_params(getattr(node, "type_params", None))
        head += "(" + self.render(node.args) + ")"
        if node.returns:
            head += " -> " + self.render(node.returns)
        self._line(head, compound=True)
        self._emit(":")
        comment = self._type_comment(node)
        if comment:
            self._emit(comment)
        self._level += 1
        try:
            self._body_with_docstring(node)
        finally:
            self._level -= 1

    def s_FunctionDef(self, node):
        self._def(node, "def")

    def s_AsyncFunctionDef(self, node):
        self._def(node, "async def")

    def s_ClassDef(self, node):
        self._decorators(node)
        head = "class " + node.name
        head += self._type_params(getattr(node, "type_params", None))
        pieces = [self.render(b) for b in node.bases]
        pieces += [self.render(k) for k in node.keywords]
        if pieces:
            head += "(" + ", ".join(pieces) + ")"
        self._line(head, compound=True)
        self._emit(":")
        self._level += 1
        try:
            self._body_with_docstring(node)
        finally:
            self._level -= 1

    def _else(self, orelse):
        if orelse:
            self._line("else", compound=True)
            self._suite(orelse)

    def _loop(self, node, keyword):
        self._line(f"{keyword} {self.render(node.target, _P.TUPLE)} in {self.render(node.iter)}",
                   compound=True)
        self._suite(node.body, self._type_comment(node))
        self._else(node.orelse)

    def s_For(self, node):
        self._loop(node, "for")

    def s_AsyncFor(self, node):
        self._loop(node, "async for")

    def s_While(self, node):
        self._line("while " + self.render(node.test), compound=True)
        self._suite(node.body)
        self._else(node.orelse)

    def s_If(self, node):
        keyword = "if "
        while True:
            self._line(keyword + self.render(node.test), compound=True)
            self._suite(node.body)
            rest = node.orelse
            if len(rest) == 1 and isinstance(rest[0], If):
                node = rest[0]
                keyword = "elif "
                continue
            self._else(rest)
            break

    def _with(self, node, keyword):
        self._line(keyword + " " + self._join(node.items), compound=True)
        self._suite(node.body, self._type_comment(node))

    def s_With(self, node):
        self._with(node, "with")

    def s_AsyncWith(self, node):
        self._with(node, "async with")

    def _try(self, node, star):
        saved = self._try_star
        self._try_star = star
        try:
            self._line("try", compound=True)
            self._suite(node.body)
            for h in node.handlers:
                self.stmt(h)
            self._else(node.orelse)
            if node.finalbody:
                self._line("finally", compound=True)
                self._suite(node.finalbody)
        finally:
            self._try_star = saved

    def s_Try(self, node):
        self._try(node, False)

    def s_TryStar(self, node):
        self._try(node, True)

    def s_ExceptHandler(self, node):
        head = "except*" if self._try_star else "except"
        if node.type:
            head += " " + self.render(node.type)
        if node.name:
            head += " as " + node.name
        self._line(head, compound=True)
        self._suite(node.body)

    def s_Match(self, node):
        self._line("match " + self.render(node.subject), compound=True)
        self._emit(":")
        self._level += 1
        try:
            for case in node.cases:
                head = "case " + self.render(case.pattern)
                if case.guard:
                    head += " if " + self.render(case.guard)
                self._line(head, compound=True)
                self._suite(case.body)
        finally:
            self._level -= 1

    # --------------------------------------------------- auxiliary nodes

    def e_alias(self, node, ctx):
        return node.name + (" as " + node.asname if node.asname else "")

    def e_withitem(self, node, ctx):
        text = self.render(node.context_expr)
        if node.optional_vars:
            text += " as " + self.render(node.optional_vars)
        return text

    def e_keyword(self, node, ctx):
        prefix = "**" if node.arg is None else node.arg + "="
        return prefix + self.render(node.value)

    def e_arg(self, node, ctx):
        if node.annotation:
            return f"{node.arg}: {self.render(node.annotation)}"
        return node.arg

    def e_arguments(self, node, ctx):
        parts = []
        positional = list(node.posonlyargs) + list(node.args)
        missing = len(positional) - len(node.defaults)
        for i, a in enumerate(positional):
            text = self.render(a)
            if i >= missing:
                text += "=" + self.render(node.defaults[i - missing])
            parts.append(text)
            if i + 1 == len(node.posonlyargs):
                parts.append("/")
        if node.vararg or node.kwonlyargs:
            parts.append("*" + (self.render(node.vararg) if node.vararg else ""))
        for a, d in zip(node.kwonlyargs, node.kw_defaults):
            text = self.render(a)
            if d:
                text += "=" + self.render(d)
            parts.append(text)
        if node.kwarg:
            parts.append("**" + self.render(node.kwarg))
        return ", ".join(parts)

    def e_comprehension(self, node, ctx):
        text = " async for " if node.is_async else " for "
        text += self.render(node.target, _P.TUPLE)
        text += " in " + self.render(node.iter, _P.TEST.next())
        for cond in node.ifs:
            text += " if " + self.render(cond, _P.TEST.next())
        return text

    def e_TypeVar(self, node, ctx):
        text = node.name
        if node.bound:
            text += ": " + self.render(node.bound)
        if node.default_value:
            text += " = " + self.render(node.default_value)
        return text

    def e_TypeVarTuple(self, node, ctx):
        text = "*" + node.name
        if node.default_value:
            text += " = " + self.render(node.default_value)
        return text

    def e_ParamSpec(self, node, ctx):
        text = "**" + node.name
        if node.default_value:
            text += " = " + self.render(node.default_value)
        return text

    # --------------------------------------------------------- expressions

    def e_Name(self, node, ctx):
        return node.id

    def _literal(self, value):
        if isinstance(value, (float, complex)):
            # infinities and NaNs have no literal spelling
            return (repr(value).replace("inf", _HUGE)
                    .replace("nan", f"({_HUGE}-{_HUGE})"))
        return repr(value)

    def e_Constant(self, node, ctx):
        value = node.value
        if value is ...:
            return "..."
        if isinstance(value, tuple):
            items = [self._literal(v) for v in value]
            inner = items[0] + "," if len(items) == 1 else ", ".join(items)
            return "(" + inner + ")"
        prefix = "u" if node.kind == "u" else ""
        return prefix + self._literal(value)

    def e_NamedExpr(self, node, ctx):
        text = f"{self.render(node.target, _P.ATOM)} := {self.render(node.value, _P.ATOM)}"
        return self._wrap(text, _P.NAMED_EXPR, ctx)

    def e_Tuple(self, node, ctx):
        text = self._seq(node.elts)
        if not node.elts or ctx > _P.TUPLE:
            return "(" + text + ")"
        return text

    def e_List(self, node, ctx):
        return "[" + self._join(node.elts) + "]"

    def e_Set(self, node, ctx):
        if not node.elts:
            # `{}` is a dict; `set()` could be shadowed
            return "{*()}"
        return "{" + self._join(node.elts) + "}"

    def e_Dict(self, node, ctx):
        items = []
        for k, v in zip(node.keys, node.values):
            if k is None:
                items.append("**" + self.render(v, _P.EXPR))
            else:
                items.append(f"{self.render(k)}: {self.render(v)}")
        return "{" + ", ".join(items) + "}"

    def _generators(self, gens):
        return "".join(self.render(g) for g in gens)

    def e_ListComp(self, node, ctx):
        return "[" + self.render(node.elt) + self._generators(node.generators) + "]"

    def e_SetComp(self, node, ctx):
        return "{" + self.render(node.elt) + self._generators(node.generators) + "}"

    def e_GeneratorExp(self, node, ctx):
        return "(" + self.render(node.elt) + self._generators(node.generators) + ")"

    def e_DictComp(self, node, ctx):
        if node.value is None:
            head = "**" + self.render(node.key, _P.EXPR)
        else:
            head = f"{self.render(node.key)}: {self.render(node.value)}"
        return "{" + head + self._generators(node.generators) + "}"

    def e_Await(self, node, ctx):
        text = "await"
        if node.value:
            text += " " + self.render(node.value, _P.ATOM)
        return self._wrap(text, _P.AWAIT, ctx)

    def e_Yield(self, node, ctx):
        text = "yield"
        if node.value:
            text += " " + self.render(node.value, _P.ATOM)
        return self._wrap(text, _P.YIELD, ctx)

    def e_YieldFrom(self, node, ctx):
        if not node.value:
            raise ValueError("Node can't be used without a value attribute.")
        return self._wrap("yield from " + self.render(node.value, _P.ATOM), _P.YIELD, ctx)

    def e_IfExp(self, node, ctx):
        inner = _P.TEST.next()
        text = (f"{self.render(node.body, inner)} if {self.render(node.test, inner)}"
                f" else {self.render(node.orelse, _P.TEST)}")
        return self._wrap(text, _P.TEST, ctx)

    def e_Lambda(self, node, ctx):
        params = self.render(node.args)
        head = "lambda " + params if params else "lambda"
        return self._wrap(f"{head}: {self.render(node.body, _P.TEST)}", _P.TEST, ctx)

    def e_UnaryOp(self, node, ctx):
        sym, own = _lookup(_UNARY, node.op)
        return self._wrap(sym + self.render(node.operand, own), own, ctx)

    def e_BinOp(self, node, ctx):
        sym, own = _lookup(_BINOPS, node.op)
        if own == _P.POWER:   # right associative
            lctx, rctx = own.next(), own
        else:
            lctx, rctx = own, own.next()
        text = f"{self.render(node.left, lctx)} {sym} {self.render(node.right, rctx)}"
        return self._wrap(text, own, ctx)

    def e_BoolOp(self, node, ctx):
        if isinstance(node.op, And):
            word, own = " and ", _P.AND
        else:
            word, own = " or ", _P.OR
        text = word.join(self.render(v, own.next()) for v in node.values)
        return self._wrap(text, own, ctx)

    def e_Compare(self, node, ctx):
        inner = _P.CMP.next()
        pieces = [self.render(node.left, inner)]
        for op, right in zip(node.ops, node.comparators):
            pieces.append(_lookup(_CMP, op))
            pieces.append(self.render(right, inner))
        return self._wrap(" ".join(pieces), _P.CMP, ctx)

    def e_Attribute(self, node, ctx):
        base = self.render(node.value, _P.ATOM)
        if isinstance(node.value, Constant) and isinstance(node.value.value, int):
            base += " "   # `1.real` would read as a float literal
        return base + "." + node.attr

    def e_Call(self, node, ctx):
        args = [self.render(a) for a in node.args]
        args += [self.render(k) for k in node.keywords]
        return self.render(node.func, _P.ATOM) + "(" + ", ".join(args) + ")"

    def e_Subscript(self, node, ctx):
        index = node.slice
        if isinstance(index, Tuple) and index.elts:
            inner = self._seq(index.elts)
        else:
            inner = self.render(index)
        return self.render(node.value, _P.ATOM) + "[" + inner + "]"

    def e_Starred(self, node, ctx):
        return "*" + self.render(node.value, _P.EXPR)

    def e_Slice(self, node, ctx):
        text = (self.render(node.lower) if node.lower else "") + ":"
        if node.upper:
            text += self.render(node.upper)
        if node.step:
            text += ":" + self.render(node.step)
        return text

    # --------------------------------------------------- f- and t-strings

    def _replacement_field(self, node, source=None):
        if source is None:
            source = _Unparser().render(node.value, _P.TEST.next())
        pieces = ["{"]
        if source.startswith("{"):
            pieces.append(" ")   # `{{` would be an escaped brace
        pieces.append(source)
        if node.conversion != -1:
            pieces.append("!" + chr(node.conversion))
        if node.format_spec:
            pieces.append(":" + self._template_text(node.format_spec, True))
        pieces.append("}")
        return "".join(pieces)

    def _template_text(self, node, in_spec=False):
        """Text between the quotes of an f/t-string for *node* (one part or
        a JoinedStr of parts)."""
        if isinstance(node, JoinedStr):
            return "".join(self._template_text(v, in_spec) for v in node.values)
        if isinstance(node, Constant) and isinstance(node.value, str):
            text = node.value.replace("{", "{{").replace("}", "}}")
            if in_spec:
                for old, new in (("\\", "\\\\"), ("'", "\\'"), ('"', '\\"'),
                                 ("\n", "\\n")):
                    text = text.replace(old, new)
            return text
        if isinstance(node, FormattedValue):
            return self._replacement_field(node)
        if isinstance(node, Interpolation):
            src = node.str if isinstance(node.str, str) else None
            return self._replacement_field(node, src)
        raise ValueError(f"Unexpected node inside JoinedStr, {node!r}")

    def _template(self, prefix, values):
        parts = [(self._template_text(v), isinstance(v, Constant)) for v in values]
        allowed = list(_QUOTES)
        chunks = []
        for text, literal in parts:
            if literal:
                body, fits = _quote_options(text, allowed, keep_ws=False)
                if not any(q in allowed for q in fits):
                    break
                allowed = fits
                chunks.append(body)
            else:
                if "\n" in text:
                    allowed = [q for q in allowed if len(q) == 3]
                narrowed = [q for q in allowed if q not in text]
                if narrowed:
                    allowed = narrowed
                chunks.append(text)
        else:
            return prefix + allowed[0] + "".join(chunks) + allowed[0]
        # No delimiter works for every literal part: use ''' and let repr()
        # do the escaping of the literal parts.
        chunks = []
        for text, literal in parts:
            if literal:
                text = repr('"' + text)[2:-1]
            chunks.append(text)
        return prefix + "'''" + "".join(chunks) + "'''"

    def e_JoinedStr(self, node, ctx):
        return self._template("f", node.values)

    def e_TemplateStr(self, node, ctx):
        return self._template("t", node.values)

    def e_FormattedValue(self, node, ctx):
        return self._replacement_field(node)

    def e_Interpolation(self, node, ctx):
        src = node.str if isinstance(node.str, str) else None
        return self._replacement_field(node, src)

    # ------------------------------------------------------------ patterns

    def e_MatchValue(self, node, ctx):
        return self.render(node.value)

    def e_MatchSingleton(self, node, ctx):
        return self._literal(node.value)

    def e_MatchSequence(self, node, ctx):
        return "[" + self._join(node.patterns) + "]"

    def e_MatchStar(self, node, ctx):
        return "*" + (node.name if node.name is not None else "_")

    def e_MatchMapping(self, node, ctx):
        items = [f"{self.render(k)}: {self.render(p)}"
                 for k, p in zip(node.keys, node.patterns, strict=True)]
        if node.rest is not None:
            items.append("**" + node.rest)
        return "{" + ", ".join(items) + "}"

    def e_MatchClass(self, node, ctx):
        items = [self.render(p) for p in node.patterns]
        items += [f"{a}={self.render(p)}"
                  for a, p in zip(node.kwd_attrs, node.kwd_patterns, strict=True)]
        return self.render(node.cls, _P.ATOM) + "(" + ", ".join(items) + ")"

    def e_MatchAs(self, node, ctx):
        if node.name is None:
            return "_"
        if node.pattern is None:
            return node.name
        text = f"{self.render(node.pattern, _P.BOR)} as {node.name}"
        return self._wrap(text, _P.TEST, ctx)

    def e_MatchOr(self, node, ctx):
        text = " | ".join(self.render(p, _P.BOR.next()) for p in node.patterns)
        return self._wrap(text, _P.BOR, ctx)


Unparser = _Unparser


def unparse(node):
    return _Unparser().visit(node)
