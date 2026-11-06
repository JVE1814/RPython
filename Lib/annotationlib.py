"""Introspection of annotations (PEP 649 / PEP 749).

get_annotations() returns the annotations of a module, class or function in
one of several formats: VALUE (evaluated objects), FORWARDREF (evaluated
where possible, ForwardRef objects for undefined names) or STRING (source-like
strings). The latter two are computed by running the object's __annotate__
function against "fake" namespaces in which unknown names become recording
proxies.
"""

import builtins
import enum
import keyword
import sys
import types

__all__ = [
    "Format",
    "ForwardRef",
    "call_annotate_function",
    "call_evaluate_function",
    "get_annotate_from_class_namespace",
    "get_annotations",
    "annotations_to_string",
    "type_repr",
]


class Format(enum.IntEnum):
    VALUE = 1
    VALUE_WITH_FAKE_GLOBALS = 2
    FORWARDREF = 3
    STRING = 4


_sentinel = object()

# Slots shared by ForwardRef and the _Stringifier proxies.
_SLOTS = (
    "__forward_is_argument__",
    "__forward_is_class__",
    "__forward_module__",
    "__weakref__",
    "__arg__",
    "__globals__",
    "__extra_names__",
    "__code__",
    "__cell__",
    "__owner__",
    "__stringifier_dict__",
    "__resolved_str_cache__",
)

_NAME_ERROR_MSG = "name {name!r} is not defined"


def _cell_key(cell):
    if cell is None:
        return None
    if isinstance(cell, dict):
        return tuple(sorted((k, id(v)) for k, v in cell.items()))
    return id(cell)


class ForwardRef:
    """A forward reference: an annotation expression that could not be evaluated yet.

    Use evaluate() to compute its value. Instances are created by
    get_annotations() with Format.FORWARDREF and by typing for string
    annotations.
    """

    __slots__ = _SLOTS

    def __init__(self, arg, *, module=None, owner=None, is_argument=True, is_class=False):
        if not isinstance(arg, str):
            raise TypeError(f"Forward reference must be a string -- got {arg!r}")

        self.__arg__ = arg
        self.__forward_is_argument__ = is_argument
        self.__forward_is_class__ = is_class
        self.__forward_module__ = module
        self.__owner__ = owner
        # these are only set by the stringification machinery
        self.__globals__ = None
        self.__extra_names__ = None
        self.__code__ = None
        self.__cell__ = None
        self.__stringifier_dict__ = None
        self.__resolved_str_cache__ = None

    def __init_subclass__(cls, /, *args, **kwds):
        raise TypeError("Cannot subclass ForwardRef")

    def _source_with_names_resolved(self):
        # The source text, with the placeholder names standing for real
        # objects replaced by those objects' annotation reprs (computed once).
        extra = self.__extra_names__
        if not extra:
            return self.__forward_arg__
        cached = self.__resolved_str_cache__
        if cached is None:
            cached = self.__forward_arg__
            for name, value in extra.items():
                cached = cached.replace(name, type_repr(value))
            self.__resolved_str_cache__ = cached
        return cached

    def evaluate(self, *, globals=None, locals=None, type_params=None, owner=None,
                 format=Format.VALUE):
        """Evaluate the reference and return its value.

        Names are looked up in locals, globals (by default those of the
        owner or of the module the reference was created for), the owner's
        type parameters and enclosing cells, then builtins. With
        Format.FORWARDREF, a reference that cannot be evaluated is returned
        unchanged; Format.STRING returns the source string.
        """
        if format == Format.STRING:
            return self._source_with_names_resolved()
        if format == Format.VALUE:
            is_forwardref_format = False
        elif format == Format.FORWARDREF:
            is_forwardref_format = True
        else:
            raise NotImplementedError(format)

        if isinstance(self.__cell__, types.CellType):
            try:
                return self.__cell__.cell_contents
            except ValueError:
                pass
        if owner is None:
            owner = self.__owner__

        if globals is None and self.__forward_module__ is not None:
            globals = getattr(sys.modules.get(self.__forward_module__, None), "__dict__", None)
        if globals is None:
            globals = self.__globals__
        if globals is None:
            if isinstance(owner, type):
                module_name = getattr(owner, "__module__", None)
                if module_name:
                    module = sys.modules.get(module_name, None)
                    if module:
                        globals = getattr(module, "__dict__", None)
            elif isinstance(owner, types.ModuleType):
                globals = getattr(owner, "__dict__", None)
            elif callable(owner):
                globals = getattr(owner, "__globals__", None)

        # eval() with None would use the globals of this module
        if globals is None:
            globals = {}

        if type_params is None and owner is not None:
            type_params = getattr(owner, "__type_params__", None)

        if locals is None:
            locals = {}
            if isinstance(owner, type):
                locals.update(vars(owner))
        elif (type_params is not None or isinstance(self.__cell__, dict)
              or self.__extra_names__):
            # do not mutate the caller's mapping
            locals = dict(locals)

        # type parameters behave like an enclosing scope (locals win)
        if type_params is not None:
            for param in type_params:
                locals.setdefault(param.__name__, param)

        # as do the free variables captured from enclosing functions
        if isinstance(self.__cell__, dict):
            for cell_name, cell in self.__cell__.items():
                try:
                    cell_value = cell.cell_contents
                except ValueError:
                    pass
                else:
                    locals.setdefault(cell_name, cell_value)

        if self.__extra_names__:
            locals.update(self.__extra_names__)

        arg = self.__forward_arg__
        if arg.isidentifier() and not keyword.iskeyword(arg):
            if arg in locals or arg in globals:
                value = locals[arg] if arg in locals else globals[arg]
                if type(value) is _LazyImportType:
                    # a lazy import is resolved like a name load would
                    try:
                        value = value.resolve()
                    except Exception:
                        if is_forwardref_format:
                            return self
                        raise
                return value
            elif hasattr(builtins, arg):
                return getattr(builtins, arg)
            elif is_forwardref_format:
                return self
            else:
                raise NameError(_NAME_ERROR_MSG.format(name=arg), name=arg)
        else:
            code = self.__forward_code__
            try:
                return eval(code, globals, locals)
            except Exception:
                if not is_forwardref_format:
                    raise
            # Evaluate again with a namespace in which unknown names become
            # proxies; the result may then still contain ForwardRefs.
            new_locals = _StringifierDict(
                {**builtins.__dict__, **globals, **locals},
                globals=globals,
                owner=owner,
                is_class=self.__forward_is_class__,
                format=format,
            )
            try:
                result = eval(code, globals, new_locals)
            except Exception:
                return self
            else:
                new_locals.transmogrify(self.__cell__)
                return result

    def _evaluate(self, globalns, localns, type_params=_sentinel, *, recursive_guard):
        import typing
        import warnings

        if type_params is _sentinel:
            typing._deprecation_warning_for_no_type_params_passed(
                "typing.ForwardRef._evaluate"
            )
            type_params = ()
        warnings._deprecated(
            "ForwardRef._evaluate",
            "{name} is a private API and is retained for compatibility, but will be removed"
            " in Python 3.16. Use ForwardRef.evaluate() or typing.evaluate_forward_ref() instead.",
            remove=(3, 16),
        )
        return typing.evaluate_forward_ref(
            self,
            globals=globalns,
            locals=localns,
            type_params=type_params,
            _recursive_guard=recursive_guard,
        )

    @property
    def __forward_arg__(self):
        if self.__arg__ is not None:
            return self.__arg__
        raise AssertionError("Attempted to access '__forward_arg__' on an uninitialized ForwardRef")

    @property
    def __forward_code__(self):
        if self.__code__ is not None:
            return self.__code__
        arg = self.__forward_arg__
        try:
            self.__code__ = compile(_rewrite_star_unpack(arg), "<string>", "eval")
        except SyntaxError:
            raise SyntaxError(f"Forward reference must be an expression -- got {arg!r}")
        return self.__code__

    def __eq__(self, other):
        if not isinstance(other, ForwardRef):
            return NotImplemented
        return (
            self.__forward_arg__ == other.__forward_arg__
            and self.__forward_module__ == other.__forward_module__
            # identity, since dicts are not hashable (see __hash__)
            and self.__globals__ is other.__globals__
            and self.__forward_is_class__ == other.__forward_is_class__
            and _cell_key(self.__cell__) == _cell_key(other.__cell__)
            and self.__owner__ == other.__owner__
            and (
                (tuple(sorted(self.__extra_names__.items())) if self.__extra_names__ else None) ==
                (tuple(sorted(other.__extra_names__.items())) if other.__extra_names__ else None)
            )
        )

    def __hash__(self):
        return hash((
            self.__forward_arg__,
            self.__forward_module__,
            id(self.__globals__),
            self.__forward_is_class__,
            _cell_key(self.__cell__),
            self.__owner__,
            tuple(sorted(self.__extra_names__.items())) if self.__extra_names__ else None,
        ))

    def __or__(self, other):
        return _union_of(self, other)

    def __ror__(self, other):
        return _union_of(other, self)

    def __repr__(self):
        extra = []
        if self.__forward_module__ is not None:
            extra.append(f", module={self.__forward_module__!r}")
        if self.__forward_is_class__:
            extra.append(", is_class=True")
        if self.__owner__ is not None:
            extra.append(f", owner={self.__owner__!r}")
        return f"ForwardRef({self._source_with_names_resolved()!r}{''.join(extra)})"


def _union_of(a, b):
    import typing
    return typing.Union[a, b]


def _rewrite_star_unpack(arg):
    # "*Ts" is not an expression; evaluate it as an unpacked tuple element.
    if arg.lstrip().startswith("*"):
        return f"({arg},)[0]"
    return arg


# ---------------------------------------------------------------- stringification
#
# A _Stringifier stands for an unknown name (or an expression built from
# one). Every operation on it returns a new _Stringifier recording the source
# text of the operation, with enough parentheses to preserve precedence.

_PREC_TUPLE = 0
_PREC_CMP = 5
_PREC_BOR = 6
_PREC_BXOR = 7
_PREC_BAND = 8
_PREC_SHIFT = 9
_PREC_ARITH = 10
_PREC_TERM = 11
_PREC_FACTOR = 12
_PREC_POWER = 13
_PREC_ATOM = 16

_BINOPS = {
    "add": ("+", _PREC_ARITH), "sub": ("-", _PREC_ARITH),
    "mul": ("*", _PREC_TERM), "matmul": ("@", _PREC_TERM),
    "truediv": ("/", _PREC_TERM), "floordiv": ("//", _PREC_TERM),
    "mod": ("%", _PREC_TERM), "pow": ("**", _PREC_POWER),
    "lshift": ("<<", _PREC_SHIFT), "rshift": (">>", _PREC_SHIFT),
    "and": ("&", _PREC_BAND), "xor": ("^", _PREC_BXOR), "or": ("|", _PREC_BOR),
}

_CMPOPS = {"lt": "<", "le": "<=", "eq": "==", "ne": "!=", "gt": ">", "ge": ">="}


def _render(value, prec, sdict):
    """Source text for an operand at the given precedence."""
    if isinstance(value, _Stringifier):
        text = value.__arg__
        own = _PRECS.get(id(value), _PREC_ATOM)
        if sdict is None:
            sdict = value.__stringifier_dict__
        if own < prec:
            return f"({text})"
        return text
    return _value_source(value, sdict)


def _value_source(value, sdict):
    if value is ...:
        return "..."
    if isinstance(value, slice):
        parts = [
            "" if value.start is None else _render(value.start, _PREC_TUPLE + 1, sdict),
            "" if value.stop is None else _render(value.stop, _PREC_TUPLE + 1, sdict),
        ]
        if value.step is not None:
            parts.append(_render(value.step, _PREC_TUPLE + 1, sdict))
        return ":".join(parts)
    if isinstance(value, tuple):
        items = [_render(v, _PREC_TUPLE + 1, sdict) for v in value]
        if len(items) == 1:
            return f"({items[0]},)"
        return "(" + ", ".join(items) + ")"
    if isinstance(value, list):
        return "[" + ", ".join(_render(v, _PREC_TUPLE + 1, sdict) for v in value) + "]"
    if isinstance(value, dict):
        return "{" + ", ".join(
            f"{_render(k, _PREC_TUPLE + 1, sdict)}: {_render(v, _PREC_TUPLE + 1, sdict)}"
            for k, v in value.items()) + "}"
    if isinstance(value, (set, frozenset)) and value:
        return "{" + ", ".join(_render(v, _PREC_TUPLE + 1, sdict) for v in value) + "}"
    if isinstance(value, (str, bytes, int, float, complex, bool, type(None))):
        return repr(value)
    if isinstance(value, _Template):
        source = _template_source(value, sdict)
        if source is not None:
            return source
    # A real object inside an expression.  For FORWARDREF it is referred to
    # by a fresh name (bound in __extra_names__) so the expression can be
    # evaluated later; as a string there is nothing better than its repr.
    if sdict is not None and sdict.format == Format.FORWARDREF:
        return sdict.create_unique_name(value)
    return repr(value)


_Template = type(t"")
_Interpolation = type(t"{0}".interpolations[0])


def _template_source(template, sdict):
    """t-string source text for a Template, or None if an interpolation's
    expression text cannot be put back between braces."""
    pieces = []
    for item in template:
        if isinstance(item, str):
            pieces.append(item.replace("{", "{{").replace("}", "}}"))
            continue
        expr = item.expression
        if not expr.strip() or "!" in expr.replace("!=", ""):
            return None
        piece = "{" + expr
        if item.conversion is not None:
            piece += "!" + item.conversion
        if item.format_spec:
            piece += ":" + item.format_spec
        pieces.append(piece + "}")
    body = "".join(pieces)
    if "\n" in body:
        return 't"""' + body + '"""'
    quoted = repr(body)
    return "t" + quoted


def _subscript_source(key, sdict):
    if isinstance(key, tuple) and key:
        items = [_render(k, _PREC_TUPLE + 1, sdict) for k in key]
        if len(items) == 1:
            return items[0] + ","
        return ", ".join(items)
    return _render(key, _PREC_TUPLE + 1, sdict)


class _Stringifier:
    # Must match the slots on ForwardRef, so we can turn an instance of one
    # into an instance of the other in place.
    __slots__ = _SLOTS

    def __init__(self, arg, *, globals=None, owner=None, is_class=False, cell=None,
                 stringifier_dict=None, prec=_PREC_ATOM, extra_names=None):
        assert isinstance(arg, str)
        self.__arg__ = arg
        self.__forward_is_argument__ = False
        self.__forward_is_class__ = is_class
        self.__forward_module__ = None
        self.__code__ = None
        self.__globals__ = globals
        self.__cell__ = cell
        self.__owner__ = owner
        self.__stringifier_dict__ = stringifier_dict
        self.__extra_names__ = extra_names
        self.__resolved_str_cache__ = None
        _PRECS[id(self)] = prec


    def __make_new(self, text, prec, *others):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        extra = {}
        for o in (self, *others):
            if isinstance(o, _Stringifier):
                en = object.__getattribute__(o, "__extra_names__")
                if en:
                    extra.update(en)
        if sdict is not None and sdict.extra_names:
            extra.update(sdict.extra_names)
        new = _Stringifier(
            text,
            globals=object.__getattribute__(self, "__globals__"),
            owner=object.__getattribute__(self, "__owner__"),
            is_class=object.__getattribute__(self, "__forward_is_class__"),
            stringifier_dict=sdict,
            prec=prec,
            extra_names=extra or None,
        )
        if sdict is not None:
            sdict.stringifiers.append(new)
        return new

    def __hash__(self):
        return id(self)

    def __getitem__(self, other):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        text = f"{_render(self, _PREC_ATOM, sdict)}[{_subscript_source(other, sdict)}]"
        return self.__make_new(text, _PREC_ATOM, other)

    def __getattr__(self, attr):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        return self.__make_new(f"{_render(self, _PREC_ATOM, sdict)}.{attr}", _PREC_ATOM)

    def __call__(self, *args, **kwargs):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        parts = [_render(a, _PREC_TUPLE + 1, sdict) for a in args]
        parts += [f"{k}={_render(v, _PREC_TUPLE + 1, sdict)}" for k, v in kwargs.items()]
        text = f"{_render(self, _PREC_ATOM, sdict)}({', '.join(parts)})"
        return self.__make_new(text, _PREC_ATOM, *args, *kwargs.values())

    def __iter__(self):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        yield self.__make_new(f"*{_render(self, _PREC_BOR, sdict)}", _PREC_TUPLE + 1)

    def __repr__(self):
        return object.__getattribute__(self, "__arg__")

    def __format__(self, format_spec):
        raise TypeError("Cannot stringify annotation containing string formatting")

    def _binop(self, other, name, reflected):
        op, prec = _BINOPS[name]
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        # ** is right-associative
        lprec = prec + 1 if name == "pow" else prec
        rprec = prec if name == "pow" else prec + 1
        if reflected:
            text = f"{_render(other, lprec, sdict)} {op} {_render(self, rprec, sdict)}"
        else:
            text = f"{_render(self, lprec, sdict)} {op} {_render(other, rprec, sdict)}"
        return self.__make_new(text, prec, other)

    def _cmpop(self, other, name):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        text = f"{_render(self, _PREC_CMP + 1, sdict)} {_CMPOPS[name]} {_render(other, _PREC_CMP + 1, sdict)}"
        return self.__make_new(text, _PREC_CMP, other)

    def _unary(self, op):
        sdict = object.__getattribute__(self, "__stringifier_dict__")
        return self.__make_new(f"{op}{_render(self, _PREC_FACTOR, sdict)}", _PREC_FACTOR)

    def __neg__(self):
        return self._unary("-")

    def __pos__(self):
        return self._unary("+")

    def __invert__(self):
        return self._unary("~")


_PRECS = {}


def _install_ops():
    for name in _BINOPS:
        def fwd(self, other, name=name):
            return self._binop(other, name, False)

        def rev(self, other, name=name):
            return self._binop(other, name, True)
        setattr(_Stringifier, f"__{name}__", fwd)
        setattr(_Stringifier, f"__r{name}__", rev)
    for name in _CMPOPS:
        def cmp(self, other, name=name):
            return self._cmpop(other, name)
        setattr(_Stringifier, f"__{name}__", cmp)
    # keep __hash__ (defining __eq__ would otherwise remove it)
    _Stringifier.__hash__ = lambda self: id(self)

_install_ops()
del _install_ops


class _StringifierDict(dict):
    """Namespace in which missing names become _Stringifier proxies."""

    def __init__(self, namespace, *, globals=None, owner=None, is_class=False, format):
        super().__init__(namespace)
        self.namespace = namespace
        self.globals = globals
        self.owner = owner
        self.is_class = is_class
        self.stringifiers = []
        self.next_id = 1
        self.extra_names = {}
        self.format = format

    def __missing__(self, key):
        fwdref = _Stringifier(
            key,
            globals=self.globals,
            owner=self.owner,
            is_class=self.is_class,
            stringifier_dict=self,
        )
        self.stringifiers.append(fwdref)
        return fwdref

    def transmogrify(self, cell_dict):
        """Turn every _Stringifier created in this namespace into a ForwardRef."""
        for obj in self.stringifiers:
            obj.__class__ = ForwardRef
            obj.__stringifier_dict__ = None
            obj.__code__ = None
            # a proxy for one closure variable keeps that cell; everything
            # else can refer to any of the captured cells
            if not isinstance(obj.__cell__, types.CellType):
                obj.__cell__ = cell_dict
            _PRECS.pop(id(obj), None)

    def create_unique_name(self, value):
        name = f"__annotationlib_name_{self.next_id}__"
        self.next_id += 1
        self.extra_names[name] = value
        return name


def _build_closure(annotate, owner, is_class, stringifier_dict, *, allow_evaluation):
    if not annotate.__closure__:
        return None, None
    freevars = annotate.__code__.co_freevars
    new_closure = []
    cell_dict = {}
    for i, cell in enumerate(annotate.__closure__):
        if i < len(freevars):
            name = freevars[i]
        else:
            name = "__cell__"
        new_cell = None
        if name == "__classdict__" and not allow_evaluation:
            # class-scope names are looked up in this mapping first
            cell_dict[name] = cell
            new_closure.append(types.CellType(stringifier_dict))
            continue
        if allow_evaluation:
            try:
                cell.cell_contents
            except ValueError:
                pass
            else:
                new_cell = cell
        if new_cell is None:
            fwdref = _Stringifier(
                name,
                cell=cell,
                owner=owner,
                globals=annotate.__globals__,
                is_class=is_class,
                stringifier_dict=stringifier_dict,
            )
            stringifier_dict.stringifiers.append(fwdref)
            new_cell = types.CellType(fwdref)
        cell_dict[name] = cell
        new_closure.append(new_cell)
    return tuple(new_closure), cell_dict


def _stringify_single(anno):
    if anno is ...:
        return "..."
    # str values are already strings (PEP 563 stringified annotations)
    if isinstance(anno, str):
        return anno
    if isinstance(anno, _Stringifier):
        return object.__getattribute__(anno, "__arg__")
    if isinstance(anno, (tuple, list, dict, slice, _Template)):
        return _value_source(anno, None)
    return repr(anno)


def _make_fake_function(annotate, globals, closure):
    return types.FunctionType(
        annotate.__code__,
        globals,
        closure=closure,
        argdefs=annotate.__defaults__,
        kwdefaults=annotate.__kwdefaults__,
    )


def _accepts_fake_globals(annotate):
    # Probe with the real environment.  Only NotImplementedError means the
    # function does not implement VALUE_WITH_FAKE_GLOBALS; any other failure
    # (typically a NameError) is what the fake environment is there for.
    try:
        annotate(Format.VALUE_WITH_FAKE_GLOBALS)
    except NotImplementedError:
        return False
    except Exception:
        pass
    return True


def _value_as_strings(annotate, is_evaluate):
    value = annotate(Format.VALUE)
    if is_evaluate:
        return value if isinstance(value, str) else type_repr(value)
    return annotations_to_string(value)


def call_annotate_function(annotate, format, *, owner=None, _is_evaluate=False):
    """Call an __annotate__ function in the requested format.

    Formats the function does not implement itself (FORWARDREF and STRING)
    are computed by running its code with fake globals and closure cells
    (format VALUE_WITH_FAKE_GLOBALS), or from VALUE if that is not supported.
    """
    if format == Format.VALUE_WITH_FAKE_GLOBALS:
        raise ValueError("The VALUE_WITH_FAKE_GLOBALS format is for internal use only")
    try:
        return annotate(format)
    except NotImplementedError:
        pass

    is_class = isinstance(owner, type)
    if format == Format.STRING:
        if (not _accepts_fake_globals(annotate)
                or getattr(annotate, "__code__", None) is None):
            return _value_as_strings(annotate, _is_evaluate)
        # every name lookup produces a _Stringifier
        globals = _StringifierDict({}, format=format)
        closure, _ = _build_closure(annotate, owner, is_class, globals,
                                    allow_evaluation=False)
        func = _make_fake_function(annotate, globals, closure)
        annos = func(Format.VALUE_WITH_FAKE_GLOBALS)
        if _is_evaluate:
            return _stringify_single(annos)
        return {key: _stringify_single(val) for key, val in annos.items()}
    elif format == Format.FORWARDREF:
        if not _accepts_fake_globals(annotate):
            return annotate(Format.VALUE)
        # Real values where they can be computed, ForwardRefs elsewhere.
        real_builtins = annotate.__builtins__
        real_globals = annotate.__globals__
        globals = _StringifierDict(
            {}, globals=real_globals, owner=owner, is_class=is_class, format=format,
        )
        if isinstance(real_builtins, types.ModuleType):
            real_builtins = real_builtins.__dict__
        globals.update(real_builtins)
        for name, value in real_globals.items():
            if type(value) is _LazyImportType:
                # resolve now; a failing lazy import becomes a ForwardRef
                try:
                    value = value.resolve()
                except Exception:
                    globals.pop(name, None)
                    continue
            globals[name] = value
        closure, cell_dict = _build_closure(annotate, owner, is_class, globals,
                                            allow_evaluation=True)
        func = _make_fake_function(annotate, globals, closure)
        try:
            result = func(Format.VALUE_WITH_FAKE_GLOBALS)
        except Exception:
            pass
        else:
            globals.transmogrify(cell_dict)
            return result

        # Second attempt: no real values at all (some operations on real
        # objects may fail when combined with proxies).
        globals = _StringifierDict(
            {}, globals=real_globals, owner=owner, is_class=is_class, format=format,
        )
        closure, cell_dict = _build_closure(annotate, owner, is_class, globals,
                                            allow_evaluation=False)
        func = _make_fake_function(annotate, globals, closure)
        result = func(Format.VALUE_WITH_FAKE_GLOBALS)
        globals.transmogrify(cell_dict)
        if _is_evaluate:
            if isinstance(result, ForwardRef):
                return result.evaluate(format=Format.FORWARDREF)
            return result
        return {
            key: val.evaluate(format=Format.FORWARDREF) if isinstance(val, ForwardRef) else val
            for key, val in result.items()
        }
    elif format == Format.VALUE:
        # __annotate__ functions must support VALUE
        raise RuntimeError("annotate function does not support VALUE format")
    else:
        raise ValueError(f"Invalid format: {format!r}")


def call_evaluate_function(evaluate, format, *, owner=None):
    """Call an evaluate function (TypeVar.evaluate_bound, TypeAliasType.evaluate_value, ...)."""
    return call_annotate_function(evaluate, format, owner=owner, _is_evaluate=True)


def get_annotate_from_class_namespace(obj):
    """The __annotate__ function stored in a class namespace (for metaclasses), or None."""
    try:
        return obj["__annotate__"]
    except KeyError:
        return obj.get("__annotate_func__", None)


def get_annotations(obj, *, globals=None, locals=None, eval_str=False, format=Format.VALUE):
    """Compute the annotations dict for an object.

    obj may be a callable, class, module, or any object with __annotate__ or
    __annotations__. format selects VALUE, FORWARDREF or STRING; eval_str=True
    evaluates string annotations (VALUE format only) in the given or inferred
    globals and locals. A new dict is returned every time.
    """
    if eval_str and format != Format.VALUE:
        raise ValueError("eval_str=True is only supported with format=Format.VALUE")

    if format == Format.VALUE_WITH_FAKE_GLOBALS:
        raise ValueError("The VALUE_WITH_FAKE_GLOBALS format is for internal use only")
    elif format == Format.VALUE:
        ann = _get_dunder_annotations(obj)
        if ann is None:
            # no __annotations__: an __annotate__ alone also counts
            ann = _get_and_call_annotate(obj, format)
    elif format == Format.FORWARDREF:
        # prefer stored annotations when they exist and evaluate cleanly
        try:
            ann = _get_dunder_annotations(obj)
        except Exception:
            pass
        else:
            if ann is not None:
                return dict(ann)

        annotate = _get_and_call_annotate(obj, format)
        if annotate is not None:
            return annotate
        ann = _get_dunder_annotations(obj)
    elif format == Format.STRING:
        annotate = _get_and_call_annotate(obj, format)
        if annotate is not None:
            return annotate
        ann = _get_dunder_annotations(obj)
        if ann is not None:
            return annotations_to_string(ann)
    else:
        raise ValueError(f"Unsupported format {format!r}")

    if ann is None:
        if isinstance(obj, type) or callable(obj):
            return {}
        raise TypeError(f"{obj!r} does not have annotations")

    if not ann:
        return {}

    if not eval_str:
        return dict(ann)

    if globals is None or locals is None:
        if isinstance(obj, type):
            # class
            obj_globals = None
            module_name = getattr(obj, "__module__", None)
            if module_name:
                module = sys.modules.get(module_name, None)
                if module:
                    obj_globals = getattr(module, "__dict__", None)
            obj_locals = dict(vars(obj))
            unwrap = obj
        elif isinstance(obj, types.ModuleType):
            obj_globals = getattr(obj, "__dict__", None)
            obj_locals = None
            unwrap = None
        elif callable(obj):
            obj_globals = getattr(obj, "__globals__", None)
            obj_locals = None
            unwrap = obj
        else:
            obj_globals = obj_locals = unwrap = None

        if unwrap is not None:
            # follow __wrapped__ chains and functools.partial objects
            seen = {id(unwrap)}
            functools = sys.modules.get("functools")
            partial_type = getattr(functools, "partial", None)
            while True:
                if hasattr(unwrap, "__wrapped__"):
                    candidate = unwrap.__wrapped__
                elif partial_type is not None and isinstance(unwrap, partial_type):
                    candidate = unwrap.func
                else:
                    break
                if id(candidate) in seen:
                    break
                seen.add(id(candidate))
                unwrap = candidate
            if hasattr(unwrap, "__globals__"):
                obj_globals = unwrap.__globals__

        if globals is None:
            globals = obj_globals
        if locals is None:
            locals = obj_locals

    # eval() has no annotation scope: make the object's type parameters
    # visible as locals, with real locals taking precedence.
    if type_params := getattr(obj, "__type_params__", ()):
        if locals is None:
            locals = {}
        locals = {param.__name__: param for param in type_params} | locals

    return {
        key: _eval_annotation_str(value, globals, locals)
        if isinstance(value, str) else value
        for key, value in ann.items()
    }


def _eval_annotation_str(value, globals, locals):
    if value.lstrip().startswith("*"):
        # a starred annotation (*args: *Ts) is only valid inside a tuple
        return eval(f"({value},)", globals, locals)[0]
    return eval(value, globals, locals)


def type_repr(value):
    """Repr of an object as it should appear in an annotation string."""
    if isinstance(value, (type, types.FunctionType, types.BuiltinFunctionType)):
        module = value.__module__
        if module is None:
            # methods of builtin and extension types: use the owner's module
            owner = getattr(value, "__self__", None)
            if owner is not None:
                cls = owner if isinstance(owner, type) else type(owner)
                module = cls.__module__
        if module == "builtins" or module is None:
            return value.__qualname__
        return f"{module}.{value.__qualname__}"
    elif isinstance(value, types.MethodType):
        owner = value.__self__
        cls = owner if isinstance(owner, type) else type(owner)
        if not cls.__flags__ & _TPFLAGS_HEAPTYPE:
            # methods of native types whose code is Python here (datetime)
            # are shown like the builtin methods they are in C
            return f"{cls.__module__}.{value.__qualname__}"
    elif isinstance(value, _Template):
        source = _template_source(value, None)
        if source is not None:
            return source
        # no usable expression text: spell it as a constructor call
        return "Template(" + ", ".join(map(repr, value)) + ")"
    elif value is ...:
        return "..."
    return repr(value)


_TPFLAGS_HEAPTYPE = 1 << 9
_LazyImportType = getattr(types, "LazyImportType", None)


def annotations_to_string(annotations):
    """Convert an annotations dict of values into a dict of strings."""
    return {
        n: t if isinstance(t, str) else type_repr(t)
        for n, t in annotations.items()
    }


def _get_and_call_annotate(obj, format):
    """Call obj.__annotate__ in the given format; None if there is none."""
    annotate = getattr(obj, "__annotate__", None)
    if annotate is not None:
        ann = call_annotate_function(annotate, format, owner=obj)
        if not isinstance(ann, dict):
            raise ValueError(f"{obj!r}.__annotate__ returned a non-dict")
        return ann
    return None


_BASE_GET_ANNOTATIONS = type.__dict__["__annotations__"].__get__


def _get_dunder_annotations(obj):
    """Return obj.__annotations__ (copied, validated), or None if absent."""
    if isinstance(obj, type):
        try:
            ann = _BASE_GET_ANNOTATIONS(obj)
        except AttributeError:
            # e.g. builtin types; fall back to a metaclass descriptor, if any
            ann = getattr(obj, "__annotations__", None)
            if ann is None:
                return None
    else:
        # For non-class objects, __annotations__ is just an attribute.
        ann = getattr(obj, "__annotations__", None)
        if ann is None:
            return None

    if not isinstance(ann, dict):
        raise ValueError(f"{obj!r}.__annotations__ is neither a dict nor None")
    return dict(ann)
