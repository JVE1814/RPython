"""Generate the boilerplate methods (__init__, __repr__, __eq__, ...) of
classes that mainly hold data, from their annotated class attributes.

    @dataclass
    class Point:
        x: int
        y: int = 0

The fields of a dataclass are described by Field objects (see field() and
fields()); asdict(), astuple(), replace() and make_dataclass() are helpers
built on them.
"""

import abc
import annotationlib
import keyword
import sys
import types
from reprlib import recursive_repr

__all__ = ['dataclass',
           'field',
           'Field',
           'FrozenInstanceError',
           'InitVar',
           'KW_ONLY',
           'MISSING',

           # helpers
           'fields',
           'asdict',
           'astuple',
           'make_dataclass',
           'replace',
           'is_dataclass',
           ]


# ---------------------------------------------------------------------------
# Public markers and exceptions
# ---------------------------------------------------------------------------

class FrozenInstanceError(AttributeError):
    """Raised when a field of a frozen dataclass instance is assigned to or
    deleted."""


class _MISSING_TYPE:
    def __repr__(self):
        return 'MISSING'


# Marks "no value supplied" for defaults and the like (None is a valid
# default, so it cannot play this role).
MISSING = _MISSING_TYPE()


class _KW_ONLY_TYPE:
    def __repr__(self):
        return 'KW_ONLY'


# Annotating a pseudo-field with KW_ONLY makes all following fields
# keyword-only.
KW_ONLY = _KW_ONLY_TYPE()


class _FieldKind:
    # What an annotated class attribute turned out to be.
    def __init__(self, label):
        self.label = label

    def __repr__(self):
        return self.label


_FIELD = _FieldKind('_FIELD')
_FIELD_CLASSVAR = _FieldKind('_FIELD_CLASSVAR')
_FIELD_INITVAR = _FieldKind('_FIELD_INITVAR')

# names of the class attributes dataclass() adds
_FIELDS = '__dataclass_fields__'
_PARAMS = '__dataclass_params__'
_POST_INIT_NAME = '__post_init__'

# empty metadata shared by all fields that were given none
_NO_METADATA = frozendict()


class InitVar:
    """Annotation wrapper for init-only pseudo-fields: passed to __init__
    and on to __post_init__, but not stored as fields."""

    __slots__ = ('type', )

    def __init__(self, type):
        self.type = type

    def __repr__(self):
        if isinstance(self.type, type):
            name = self.type.__name__
        else:
            name = repr(self.type)
        return f'dataclasses.InitVar[{name}]'

    def __class_getitem__(cls, type):
        return InitVar(type)


# ---------------------------------------------------------------------------
# Field
# ---------------------------------------------------------------------------

class Field:
    """Description of one dataclass field.  Create these with field()."""

    __slots__ = ('name', 'type', 'default', 'default_factory', 'repr', 'hash',
                 'init', 'compare', 'metadata', 'kw_only', 'doc',
                 '_field_type')

    def __init__(self, default, default_factory, init, repr, hash, compare,
                 metadata, kw_only, doc):
        self.name = None
        self.type = None
        self.default = default
        self.default_factory = default_factory
        self.init = init
        self.repr = repr
        self.hash = hash
        self.compare = compare
        if metadata is None:
            self.metadata = _NO_METADATA
        else:
            self.metadata = types.MappingProxyType(metadata)
        self.kw_only = kw_only
        self.doc = doc
        self._field_type = None

    @recursive_repr()
    def __repr__(self):
        return ('Field('
                f'name={self.name!r},'
                f'type={self.type!r},'
                f'default={self.default!r},'
                f'default_factory={self.default_factory!r},'
                f'init={self.init!r},'
                f'repr={self.repr!r},'
                f'hash={self.hash!r},'
                f'compare={self.compare!r},'
                f'metadata={self.metadata!r},'
                f'kw_only={self.kw_only!r},'
                f'doc={self.doc!r},'
                f'_field_type={self._field_type}'
                ')')

    def __set_name__(self, owner, name):
        # The Field sits in the class body in place of its default; pass the
        # __set_name__ notification on to the default (looked up on its
        # type, as the interpreter would).
        hook = getattr(type(self.default), '__set_name__', None)
        if hook is not None:
            hook(self.default, owner, name)

    __class_getitem__ = classmethod(types.GenericAlias)


class _DataclassParams:
    # The arguments dataclass() was applied with (__dataclass_params__).
    __slots__ = ('init', 'repr', 'eq', 'order', 'unsafe_hash', 'frozen',
                 'match_args', 'kw_only', 'slots', 'weakref_slot')

    def __init__(self, init, repr, eq, order, unsafe_hash, frozen,
                 match_args, kw_only, slots, weakref_slot):
        self.init = init
        self.repr = repr
        self.eq = eq
        self.order = order
        self.unsafe_hash = unsafe_hash
        self.frozen = frozen
        self.match_args = match_args
        self.kw_only = kw_only
        self.slots = slots
        self.weakref_slot = weakref_slot

    def __repr__(self):
        values = ','.join(f'{name}={getattr(self, name)!r}'
                          for name in self.__slots__)
        return f'_DataclassParams({values})'


def field(*, default=MISSING, default_factory=MISSING, init=True, repr=True,
          hash=None, compare=True, metadata=None, kw_only=MISSING, doc=None):
    """Describe a dataclass field in more detail than a bare default.

    default / default_factory: the default value, or a zero-argument
      callable producing it (only one of the two may be given).
    init, repr, compare: include the field in __init__, __repr__ and the
      comparison methods.
    hash: include it in __hash__ (None means "same as compare").
    metadata: read-only mapping for third-party use.
    kw_only: make the __init__ parameter keyword-only.
    doc: docstring (used for the slot when slots=True).
    """
    if default is not MISSING and default_factory is not MISSING:
        raise ValueError('cannot specify both default and default_factory')
    return Field(default, default_factory, init, repr, hash, compare,
                 metadata, kw_only, doc)


def fields(class_or_instance):
    """Return the Field objects of a dataclass (or instance), excluding
    ClassVar and InitVar pseudo-fields, in definition order."""
    try:
        all_fields = getattr(class_or_instance, _FIELDS)
    except AttributeError:
        raise TypeError('must be called with a dataclass type or instance') from None
    return tuple(f for f in all_fields.values() if f._field_type is _FIELD)


def _is_dataclass_instance(obj):
    return hasattr(type(obj), _FIELDS)


def is_dataclass(obj):
    """True if *obj* is a dataclass or an instance of one."""
    cls = obj if isinstance(obj, type) else type(obj)
    return hasattr(cls, _FIELDS)


# ---------------------------------------------------------------------------
# Classifying annotations
# ---------------------------------------------------------------------------

_ANNOTATION_HEAD = None      # compiled lazily: module-qualified name prefix


def _annotation_head(text):
    """The leading dotted name of a string annotation, as a list of parts
    (``"typing.ClassVar[int]"`` gives ``["typing", "ClassVar"]``)."""
    global _ANNOTATION_HEAD
    if _ANNOTATION_HEAD is None:
        import re
        _ANNOTATION_HEAD = re.compile(r'^\s*(\w+(?:\s*\.\s*\w+)*)')
    found = _ANNOTATION_HEAD.match(text)
    if found is None:
        return None
    return [part.strip() for part in found.group(1).split('.')]


def _string_names(text, cls, module, test):
    """For a string annotation: does its leading name, resolved in the
    namespace of the module defining *cls* (following attributes for a
    dotted name), satisfy *test*?"""
    parts = _annotation_head(text)
    if not parts:
        return False
    home = sys.modules.get(cls.__module__)
    if home is None:
        return False
    # resolve e.g. "ClassVar", "typing.ClassVar" or "proxy.Nested.ClassVar"
    value = home.__dict__.get(parts[0])
    for part in parts[1:]:
        value = getattr(value, part, None)
    return test(value, module)


def _is_classvar(annotation, typing):
    return (annotation is typing.ClassVar
            or (type(annotation) is typing._GenericAlias
                and annotation.__origin__ is typing.ClassVar))


def _is_initvar(annotation, dataclasses):
    return annotation is dataclasses.InitVar or type(annotation) is dataclasses.InitVar


def _is_kw_only(annotation, dataclasses):
    return annotation is dataclasses.KW_ONLY


def _annotation_kind(cls, annotation):
    """Return _FIELD, _FIELD_CLASSVAR or _FIELD_INITVAR for *annotation*."""
    typing = sys.modules.get('typing')
    if typing is not None:
        if _is_classvar(annotation, typing):
            return _FIELD_CLASSVAR
        if isinstance(annotation, str) and _string_names(annotation, cls, typing, _is_classvar):
            return _FIELD_CLASSVAR
    this = sys.modules[__name__]
    if _is_initvar(annotation, this):
        return _FIELD_INITVAR
    if isinstance(annotation, str) and _string_names(annotation, cls, this, _is_initvar):
        return _FIELD_INITVAR
    return _FIELD


def _marks_kw_only(cls, annotation):
    this = sys.modules[__name__]
    if _is_kw_only(annotation, this):
        return True
    return isinstance(annotation, str) and _string_names(annotation, cls, this, _is_kw_only)


def _make_field(cls, name, annotation, kw_only_default):
    """Build the Field for annotated class attribute *name*."""
    present = getattr(cls, name, MISSING)
    if isinstance(present, Field):
        result = present
    else:
        if isinstance(present, types.MemberDescriptorType):
            # a slot declared by the class, not a default value
            present = MISSING
        result = field(default=present)
    result.name = name
    result.type = annotation
    kind = _annotation_kind(cls, annotation)
    result._field_type = kind

    if kind is not _FIELD and result.default_factory is not MISSING:
        raise TypeError(f'field {name} cannot have a default factory')
    if kind is _FIELD_CLASSVAR:
        if result.kw_only is not MISSING:
            raise TypeError(f'field {name} is a ClassVar but specifies kw_only')
    elif result.kw_only is MISSING:
        result.kw_only = kw_only_default

    if kind is _FIELD and type(result.default).__hash__ is None:
        # list, dict, set and other unhashable (mutable) values would be
        # shared between instances
        raise ValueError(f'mutable default {type(result.default)} for field '
                         f'{name} is not allowed: use default_factory')
    return result


# ---------------------------------------------------------------------------
# Generating methods
# ---------------------------------------------------------------------------

class _MethodSource:
    """Collects the source of generated methods and compiles them in one
    go.  Objects the bodies need (defaults, factories, helpers) are passed
    in as closure variables of an enclosing factory function, so the
    methods' globals stay the defining module's namespace."""

    def __init__(self, module_globals):
        self.module_globals = module_globals
        self.chunks = []
        self.names = []
        self.closure = {}

    def provide(self, name, value):
        self.closure[name] = value
        return name

    def add(self, name, signature, body):
        lines = [f'  def {name}({signature}):']
        lines.extend(f'    {line}' for line in body or ['pass'])
        self.chunks.append('\n'.join(lines))
        self.names.append(name)

    def compile(self):
        # The factory is named __create_fn__: generated functions keep a
        # qualname of "__create_fn__.<locals>.NAME" (for the __repr__ that
        # pprint recognises as generated) until _set_new_attribute time.
        if not self.names:
            return {}
        params = ', '.join(self.closure)
        source = (f'def __create_fn__({params}):\n'
                  + '\n'.join(self.chunks)
                  + f'\n  return ({", ".join(self.names)},)\n')
        scope = {}
        exec(source, self.module_globals, scope)
        made = scope['__create_fn__'](**self.closure)
        return dict(zip(self.names, made))


def _set_new_attribute(cls, name, value):
    """Set cls.name = value unless the class itself defines name; returns
    True if it was already there."""
    if name in cls.__dict__:
        return True
    setattr(cls, name, value)
    return False


def _tidy_method(cls, method):
    method.__qualname__ = f'{cls.__qualname__}.{method.__name__}'
    return method


def _init_body(cls, fields, std_fields, kw_fields, frozen, slots, has_post_init,
               self_name, src):
    """Return (signature, body lines) of the generated __init__."""
    seen_default = None
    for f in std_fields:
        if f.default is not MISSING or f.default_factory is not MISSING:
            seen_default = f
        elif seen_default is not None:
            raise TypeError(f'non-default argument {f.name!r} '
                            f'follows default argument {seen_default.name!r}')

    factory_marker = src.provide('__dc_use_factory', _HAS_DEFAULT_FACTORY)
    setter = src.provide('__dc_object_setattr', object.__setattr__)

    def param(f):
        if f.default_factory is not MISSING:
            return f'{f.name}={factory_marker}'
        if f.default is not MISSING:
            return f'{f.name}={src.provide(f"__dc_default_{f.name}", f.default)}'
        return f.name

    params = [self_name] + [param(f) for f in std_fields]
    if kw_fields:
        params.append('*')
        params.extend(param(f) for f in kw_fields)

    def store(name, expr):
        if frozen:
            return f'{setter}({self_name}, {name!r}, {expr})'
        return f'{self_name}.{name} = {expr}'

    body = []
    for f in fields.values():
        if f._field_type is _FIELD_INITVAR:
            continue
        if f._field_type is not _FIELD:
            continue
        if f.default_factory is not MISSING:
            factory = src.provide(f'__dc_factory_{f.name}', f.default_factory)
            if f.init:
                value = f'{factory}() if {f.name} is {factory_marker} else {f.name}'
            else:
                value = f'{factory}()'
        elif f.init:
            value = f.name
        elif f.default is not MISSING and slots:
            # with __slots__ the class attribute holding the default is gone
            value = src.provide(f'__dc_default_{f.name}', f.default)
        else:
            continue
        body.append(store(f.name, value))

    if has_post_init:
        initvars = ', '.join(f.name for f in fields.values()
                             if f._field_type is _FIELD_INITVAR)
        body.append(f'{self_name}.{_POST_INIT_NAME}({initvars})')
    return ', '.join(params), body


class _HasDefaultFactory:
    def __repr__(self):
        return '<factory>'


_HAS_DEFAULT_FACTORY = _HasDefaultFactory()


def _make_init_annotate(cls, init_fields):
    """__annotate__ for a generated __init__: the field annotations of the
    class (in the requested format) for the __init__ parameters."""
    names = [f.name for f in init_fields]

    def __annotate__(format, /):
        Format = annotationlib.Format
        if format not in (Format.VALUE, Format.FORWARDREF, Format.STRING):
            raise NotImplementedError(format)
        source = {}
        for klass in reversed(cls.__mro__):
            if _FIELDS in klass.__dict__ or klass is cls:
                try:
                    source.update(annotationlib.get_annotations(klass, format=format))
                except AttributeError:
                    pass
        result = {}
        for name in names:
            if name in source:
                result[name] = source[name]
        result['return'] = 'None' if format == Format.STRING else None
        return result

    __annotate__.__generated_by_dataclasses__ = True
    return __annotate__


def _hash_action(unsafe_hash, eq, frozen, has_explicit_hash, cls):
    """What to do with __hash__: 'add', 'none', or None (leave alone)."""
    if unsafe_hash:
        if has_explicit_hash:
            raise TypeError(f'Cannot overwrite attribute __hash__ in class {cls.__name__}')
        return 'add'
    if eq and not has_explicit_hash:
        return 'add' if frozen else 'none'
    return None


def _own_annotations(cls):
    """The annotations a class itself defines, in FORWARDREF format.

    The class's annotate function is asked for FORWARDREF directly, so an
    annotate function that can produce forward references without
    evaluating everything (as make_dataclass() creates) is not forced
    through the VALUE format first.
    """
    namespace = cls.__dict__
    annotate = annotationlib.get_annotate_from_class_namespace(namespace)
    if annotate is None or '__annotations__' in namespace:
        return annotationlib.get_annotations(
                cls, format=annotationlib.Format.FORWARDREF)
    return dict(annotationlib.call_annotate_function(
            annotate, annotationlib.Format.FORWARDREF, owner=cls))


def _process_class(cls, init, repr, eq, order, unsafe_hash, frozen,
                   match_args, kw_only, slots, weakref_slot):
    fields = {}

    if cls.__module__ in sys.modules:
        module_globals = sys.modules[cls.__module__].__dict__
    else:
        module_globals = {}

    setattr(cls, _PARAMS, _DataclassParams(init, repr, eq, order, unsafe_hash,
                                            frozen, match_args, kw_only, slots,
                                            weakref_slot))

    # Fields inherited from dataclass bases, most distant base first.
    dataclass_bases = 0
    frozen_bases = 0
    for base in cls.__mro__[-1:0:-1]:
        base_fields = getattr(base, _FIELDS, None)
        if base_fields is not None:
            dataclass_bases += 1
            for f in base_fields.values():
                fields[f.name] = f
            if getattr(base, _PARAMS).frozen:
                frozen_bases += 1

    own_annotations = _own_annotations(cls)

    own_fields = []
    kw_only_now = kw_only
    kw_marker = None
    for name, annotation in own_annotations.items():
        if _marks_kw_only(cls, annotation):
            if kw_marker is not None:
                raise TypeError(f'{name!r} is KW_ONLY, but KW_ONLY '
                                'has already been specified')
            kw_marker = name
            kw_only_now = True
            continue
        own_fields.append(_make_field(cls, name, annotation, kw_only_now))

    for f in own_fields:
        fields[f.name] = f
        # The class attribute holds the Field object; replace it with the
        # plain default, or remove it if there is none.
        if isinstance(getattr(cls, f.name, None), Field):
            if f.default is MISSING:
                delattr(cls, f.name)
            else:
                setattr(cls, f.name, f.default)

    for name, value in cls.__dict__.items():
        if isinstance(value, Field) and name not in own_annotations:
            raise TypeError(f'{name!r} is a field but has no type annotation')

    if dataclass_bases:
        if frozen_bases and not frozen:
            raise TypeError('cannot inherit non-frozen dataclass from a frozen one')
        if frozen and frozen_bases != dataclass_bases:
            raise TypeError('cannot inherit frozen dataclass from a non-frozen one')

    setattr(cls, _FIELDS, fields)

    own_hash = cls.__dict__.get('__hash__', MISSING)
    has_explicit_hash = not (own_hash is MISSING
                             or (own_hash is None and '__eq__' in cls.__dict__))

    if order and not eq:
        raise ValueError('eq must be true if order is true')

    init_fields = [f for f in fields.values()
                   if f._field_type in (_FIELD, _FIELD_INITVAR) and f.init]
    std_init_fields = [f for f in init_fields if not f.kw_only]
    kw_init_fields = [f for f in init_fields if f.kw_only]
    real_fields = [f for f in fields.values() if f._field_type is _FIELD]

    src = _MethodSource(module_globals)
    wanted = {}             # method name -> how to install it

    if init:
        self_name = '__dataclass_self__' if 'self' in fields else 'self'
        signature, body = _init_body(
                cls, fields, std_init_fields, kw_init_fields, frozen, slots,
                hasattr(cls, _POST_INIT_NAME), self_name, src)
        src.add('__init__', signature, body)
        wanted['__init__'] = 'init'

    if repr:
        parts = ', '.join(f'{f.name}={{self.{f.name}!r}}'
                          for f in real_fields if f.repr)
        src.add('__repr__', 'self',
                [f'return f"{{self.__class__.__qualname__}}({parts})"'])
        wanted['__repr__'] = 'repr'

    compared = [f for f in real_fields if f.compare]
    if eq:
        if compared:
            test = ' and '.join(f'self.{f.name} == other.{f.name}' for f in compared)
        else:
            test = 'True'
        src.add('__eq__', 'self, other', [
                'if self is other:',
                '    return True',
                'if other.__class__ is self.__class__:',
                f'    return {test}',
                'return NotImplemented'])
        wanted['__eq__'] = 'plain'

    if order:
        mine = '(' + ''.join(f'self.{f.name},' for f in compared) + ')'
        theirs = '(' + ''.join(f'other.{f.name},' for f in compared) + ')'
        for name, op in (('__lt__', '<'), ('__le__', '<='),
                         ('__gt__', '>'), ('__ge__', '>=')):
            src.add(name, 'self, other', [
                    'if other.__class__ is self.__class__:',
                    f'    return {mine} {op} {theirs}',
                    'return NotImplemented'])
            wanted[name] = 'order'

    if frozen:
        owner = src.provide('__dc_cls', cls)
        frozen_error = src.provide('__dc_frozen_error', FrozenInstanceError)
        names = src.provide('__dc_field_names',
                            frozenset(f.name for f in real_fields))
        src.add('__setattr__', 'self, name, value', [
                f'if type(self) is {owner} or name in {names}:',
                f'    raise {frozen_error}(f"cannot assign to field {{name!r}}")',
                f'super({owner}, self).__setattr__(name, value)'])
        src.add('__delattr__', 'self, name', [
                f'if type(self) is {owner} or name in {names}:',
                f'    raise {frozen_error}(f"cannot delete field {{name!r}}")',
                f'super({owner}, self).__delattr__(name)'])
        wanted['__setattr__'] = 'frozen'
        wanted['__delattr__'] = 'frozen'

    hash_action = _hash_action(unsafe_hash, eq, frozen, has_explicit_hash, cls)
    if hash_action == 'add':
        hashed = [f for f in real_fields
                  if (f.compare if f.hash is None else f.hash)]
        items = ''.join(f'self.{f.name},' for f in hashed)
        src.add('__hash__', 'self', [f'return hash(({items}))'])
        wanted['__hash__'] = 'hash'

    made = src.compile()

    for name, how in wanted.items():
        method = made[name]
        if how == 'repr':
            # pprint looks at the wrapped function to recognise a generated
            # __repr__; only the wrapper gets the class-based qualname
            _set_new_attribute(cls, name, _tidy_method(cls, recursive_repr()(method)))
            continue
        method = _tidy_method(cls, method)
        if how == 'init':
            method.__annotate__ = _make_init_annotate(cls, init_fields)
            _set_new_attribute(cls, name, method)
        elif how == 'order':
            if _set_new_attribute(cls, name, method):
                raise TypeError(f'Cannot overwrite attribute {name} '
                                f'in class {cls.__name__}. Consider using '
                                'functools.total_ordering')
        elif how == 'frozen':
            if _set_new_attribute(cls, name, method):
                raise TypeError(f'Cannot overwrite attribute {name} '
                                f'in class {cls.__name__}')
        elif how == 'hash':
            cls.__hash__ = method
        else:
            _set_new_attribute(cls, name, method)
    if hash_action == 'none':
        cls.__hash__ = None

    if not getattr(cls, '__doc__'):
        # computed on first access: annotations may refer to names that
        # only exist once the class statement has completed
        cls.__doc__ = _SignatureDoc()

    if match_args:
        _set_new_attribute(cls, '__match_args__',
                           tuple(f.name for f in std_init_fields))

    if weakref_slot and not slots:
        raise TypeError('weakref_slot is True but slots is False')
    if slots:
        cls = _add_slots(cls, frozen, weakref_slot, fields)

    _set_new_attribute(cls, '__replace__', _replace)
    abc.update_abstractmethods(cls)
    return cls


class _SignatureDoc:
    """Stand-in __doc__ of a dataclass without a docstring; becomes the
    constructor signature when first read."""

    def __get__(self, instance, owner):
        text = _signature_doc(owner)
        if owner.__dict__.get('__doc__') is self:
            try:
                type.__setattr__(owner, '__doc__', text)
            except (TypeError, AttributeError):
                pass
        return text


def _signature_doc(cls):
    """Docstring for a dataclass without one: its constructor signature."""
    try:
        import inspect
        text = str(inspect.signature(
                cls, annotation_format=annotationlib.Format.FORWARDREF))
    except (TypeError, ValueError):
        text = ''
    return cls.__name__ + text.replace(' -> None', '')


# ---------------------------------------------------------------------------
# __slots__ support
# ---------------------------------------------------------------------------

def _dataclass_getstate(self):
    return [getattr(self, f.name) for f in fields(self)]


def _dataclass_setstate(self, state):
    for f, value in zip(fields(self), state):
        # bypass a frozen class's __setattr__
        object.__setattr__(self, f.name, value)


def _declared_slots(cls):
    """The names a class's own __slots__ declares."""
    declared = cls.__dict__.get('__slots__')
    if declared is None:
        return ()
    if isinstance(declared, str):
        return (declared, )
    if hasattr(declared, '__next__'):
        # an iterator was already consumed when the class was created
        raise TypeError(f"Slots of '{cls.__name__}' cannot be determined")
    return tuple(declared)


def _inherited_slots(cls):
    names = set()
    for base in cls.__mro__[1:-1]:
        names.update(_declared_slots(base))
    return names


def _provides_weakref(cls):
    return any('__weakref__' in base.__dict__ for base in cls.__mro__[1:-1])


def _class_cells_fixed(holder, old, new, seen):
    """Point closure cells of the function(s) behind *holder* that refer
    to *old* at *new* instead (for zero-argument super() and __class__)."""
    if isinstance(holder, (staticmethod, classmethod)):
        holder = holder.__func__
    elif isinstance(holder, property):
        for part in (holder.fget, holder.fset, holder.fdel):
            if part is not None:
                _class_cells_fixed(part, old, new, seen)
        return
    while holder is not None and id(holder) not in seen:
        seen.add(id(holder))
        closure = getattr(holder, '__closure__', None)
        if closure:
            for cell in closure:
                try:
                    contents = cell.cell_contents
                except ValueError:
                    continue
                if contents is old:
                    cell.cell_contents = new
        holder = getattr(holder, '__wrapped__', None)


def _reown_forward_refs(annotation, old, new, seen):
    # forward references in field types created for the replaced class
    # must point at its successor (and not keep the old class alive)
    if id(annotation) in seen:
        return
    seen.add(id(annotation))
    if isinstance(annotation, annotationlib.ForwardRef):
        if annotation.__owner__ is old:
            annotation.__owner__ = new
        return
    for arg in getattr(annotation, '__args__', None) or ():
        _reown_forward_refs(arg, old, new, seen)


def _forget_old_namespace(new_cls, old_cls):
    # The compiler-generated annotate function of a class body keeps the
    # class namespace in a closure cell; where that is the old class's own
    # dict, its __dict__/__weakref__ descriptors would keep the replaced
    # class alive.  Give the function a copy without them.
    marker = old_cls.__dict__.get('__dict__')
    if marker is None:
        return
    for value in new_cls.__dict__.values():
        closure = getattr(value, '__closure__', None)
        if not closure:
            continue
        for cell in closure:
            try:
                contents = cell.cell_contents
            except ValueError:
                continue
            if isinstance(contents, dict) and contents.get('__dict__') is marker:
                cell.cell_contents = {k: v for k, v in contents.items()
                                      if k not in ('__dict__', '__weakref__')}


def _add_slots(cls, is_frozen, weakref_slot, fields):
    if '__slots__' in cls.__dict__:
        raise TypeError(f'{cls.__name__} already specifies __slots__')
    # A class's __slots__ cannot be added after creation, so build a new
    # class from the old one's namespace.
    namespace = dict(cls.__dict__)
    names = [f.name for f in fields.values() if f._field_type is _FIELD]
    known = _inherited_slots(cls)
    new_slots = [name for name in names if name not in known]
    if weakref_slot and '__weakref__' not in known and not _provides_weakref(cls):
        new_slots.append('__weakref__')
    docs = {f.name: f.doc for f in fields.values()
            if f._field_type is _FIELD and f.name in new_slots}
    if any(doc is not None for doc in docs.values()):
        namespace['__slots__'] = {name: docs.get(name) for name in new_slots}
    else:
        namespace['__slots__'] = tuple(new_slots)

    for name in names:
        # remove defaults: they would clash with the slot descriptors
        namespace.pop(name, None)
    namespace.pop('__dict__', None)
    namespace.pop('__weakref__', None)

    qualname = getattr(cls, '__qualname__', None)
    new_cls = type(cls)(cls.__name__, cls.__bases__, namespace)
    if qualname is not None:
        new_cls.__qualname__ = qualname

    if is_frozen:
        if '__getstate__' not in namespace:
            new_cls.__getstate__ = _dataclass_getstate
        if '__setstate__' not in namespace:
            new_cls.__setstate__ = _dataclass_setstate

    seen = set()
    for value in new_cls.__dict__.values():
        _class_cells_fixed(value, cls, new_cls, seen)
    _forget_old_namespace(new_cls, cls)
    for f in fields.values():
        _reown_forward_refs(f.type, cls, new_cls, set())
    init = new_cls.__dict__.get('__init__')
    if init is not None:
        annotate = getattr(init, '__annotate__', None)
        if getattr(annotate, '__generated_by_dataclasses__', False):
            _class_cells_fixed(annotate, cls, new_cls, seen)
    return new_cls


# ---------------------------------------------------------------------------
# The decorator
# ---------------------------------------------------------------------------

def dataclass(cls=None, /, *, init=True, repr=True, eq=True, order=False,
              unsafe_hash=False, frozen=False, match_args=True,
              kw_only=False, slots=False, weakref_slot=False):
    """Turn a class with annotated attributes into a dataclass.

    Generated: __init__ (init), __repr__ (repr), __eq__ (eq), the ordering
    methods (order), __hash__ (see unsafe_hash/eq/frozen), frozen
    __setattr__/__delattr__ (frozen), __match_args__ (match_args).
    kw_only makes all fields keyword-only; slots builds a new class with
    __slots__ (plus a __weakref__ slot if weakref_slot).
    """
    def wrap(cls):
        return _process_class(cls, init, repr, eq, order, unsafe_hash,
                              frozen, match_args, kw_only, slots,
                              weakref_slot)

    if cls is None:
        return wrap
    return wrap(cls)


# ---------------------------------------------------------------------------
# Helpers working on instances
# ---------------------------------------------------------------------------

_ATOMIC_TYPES = frozenset({
        types.NoneType, bool, int, float, str, complex, bytes,
        types.EllipsisType, types.NotImplementedType, types.CodeType,
        types.BuiltinFunctionType, types.FunctionType, type, range,
        property,
        })


def _convert_value(value, convert_instance, *args):
    """Rebuild *value* recursively, converting dataclass instances with
    *convert_instance* and deep-copying anything unknown."""
    kind = type(value)
    if kind in _ATOMIC_TYPES:
        return value
    if hasattr(kind, _FIELDS):
        return convert_instance(value, *args)
    if isinstance(value, tuple) and hasattr(value, '_fields'):
        # named tuples take their items as separate arguments
        return kind(*[_convert_value(v, convert_instance, *args) for v in value])
    if isinstance(value, (list, tuple)):
        return kind(_convert_value(v, convert_instance, *args) for v in value)
    if isinstance(value, (dict, frozendict)):
        if hasattr(kind, 'default_factory'):
            # defaultdict-like: the factory is a required argument
            result = kind(value.default_factory)
            for k, v in value.items():
                result[_convert_value(k, convert_instance, *args)] = \
                    _convert_value(v, convert_instance, *args)
            return result
        return kind((_convert_value(k, convert_instance, *args),
                     _convert_value(v, convert_instance, *args))
                    for k, v in value.items())
    import copy
    return copy.deepcopy(value)


def _instance_to_dict(obj, dict_factory):
    pairs = [(f.name, _convert_value(getattr(obj, f.name), _instance_to_dict, dict_factory))
             for f in fields(obj)]
    if dict_factory is dict:
        return dict(pairs)
    return dict_factory(pairs)


def _instance_to_tuple(obj, tuple_factory):
    return tuple_factory([_convert_value(getattr(obj, f.name), _instance_to_tuple, tuple_factory)
                          for f in fields(obj)])


def asdict(obj, *, dict_factory=dict):
    """Convert a dataclass instance to a dict of its fields, recursing into
    dataclasses, lists, tuples and dicts (other values are deep-copied).

    *dict_factory* builds each dict from a list of (name, value) pairs.
    """
    if not _is_dataclass_instance(obj):
        raise TypeError("asdict() should be called on dataclass instances")
    return _instance_to_dict(obj, dict_factory)


def astuple(obj, *, tuple_factory=tuple):
    """Convert a dataclass instance to a tuple of its field values, with the
    same recursion as asdict().

    *tuple_factory* builds each tuple from a list of values.
    """
    if not _is_dataclass_instance(obj):
        raise TypeError("astuple() should be called on dataclass instances")
    return _instance_to_tuple(obj, tuple_factory)


def _replace(self, /, **changes):
    # field values for the new instance: changed ones, else current ones
    for f in getattr(self, _FIELDS).values():
        if f._field_type is _FIELD_CLASSVAR:
            continue
        if not f.init:
            if f.name in changes:
                raise TypeError(f'field {f.name} is declared with '
                                f'init=False, it cannot be specified with '
                                f'replace()')
            continue
        if f.name not in changes:
            if f._field_type is _FIELD_INITVAR and f.default is MISSING:
                raise TypeError(f"InitVar {f.name!r} "
                                f'must be specified with replace()')
            if f._field_type is _FIELD:
                changes[f.name] = getattr(self, f.name)
    return self.__class__(**changes)


def replace(obj, /, **changes):
    """Return a copy of dataclass instance *obj* with the given fields
    replaced (by calling the class with all init fields)."""
    if not _is_dataclass_instance(obj):
        raise TypeError("replace() should be called on dataclass instances")
    return _replace(obj, **changes)


class _AnyPlaceholder:
    # stands for typing.Any in make_dataclass() without importing typing
    pass


def make_dataclass(cls_name, fields, *, bases=(), namespace=None, init=True,
                   repr=True, eq=True, order=False, unsafe_hash=False,
                   frozen=False, match_args=True, kw_only=False, slots=False,
                   weakref_slot=False, module=None, decorator=dataclass):
    """Create a dataclass named *cls_name* dynamically.

    *fields* items are a name, a (name, type) pair, or a
    (name, type, Field-or-default) triple.  The remaining arguments are
    passed on to *decorator* (dataclass by default).
    """
    namespace = {} if namespace is None else namespace.copy()

    seen = set()
    annotations = {}
    defaults = {}
    for item in fields:
        if isinstance(item, str):
            name, tp = item, _AnyPlaceholder
        elif len(item) == 2:
            name, tp = item
        elif len(item) == 3:
            name, tp, spec = item
            defaults[name] = spec
        else:
            raise TypeError(f'Invalid field: {item!r}')
        if not isinstance(name, str) or not name.isidentifier():
            raise TypeError(f'Field names must be valid identifiers: {name!r}')
        if keyword.iskeyword(name):
            raise TypeError(f'Field names must not be keywords: {name!r}')
        if name in seen:
            raise TypeError(f'Field name duplicated: {name!r}')
        seen.add(name)
        annotations[name] = tp

    def annotate(format, /):
        Format = annotationlib.Format
        if format == Format.STRING:
            return {name: ('typing.Any' if tp is _AnyPlaceholder
                           else annotationlib.type_repr(tp))
                    for name, tp in annotations.items()}
        if format == Format.VALUE:
            result = {}
            for name, tp in annotations.items():
                if tp is _AnyPlaceholder:
                    import typing
                    tp = typing.Any
                result[name] = tp
            return result
        if format == Format.FORWARDREF:
            result = {}
            for name, tp in annotations.items():
                if tp is _AnyPlaceholder:
                    typing = sys.modules.get('typing')
                    if typing is not None:
                        tp = typing.Any
                    else:
                        tp = annotationlib.ForwardRef('Any', module='typing')
                result[name] = tp
            return result
        raise NotImplementedError(format)

    def fill(ns):
        ns.update(namespace)
        ns.update(defaults)
        ns['__annotate__'] = annotate

    cls = types.new_class(cls_name, bases, {}, fill)

    if module is None:
        try:
            module = sys._getframemodulename(1) or '__main__'
        except AttributeError:
            try:
                module = sys._getframe(1).f_globals.get('__name__', '__main__')
            except (AttributeError, ValueError):
                pass
    if module is not None:
        cls.__module__ = module

    return decorator(cls, init=init, repr=repr, eq=eq, order=order,
                     unsafe_hash=unsafe_hash, frozen=frozen,
                     match_args=match_args, kw_only=kw_only, slots=slots,
                     weakref_slot=weakref_slot)
