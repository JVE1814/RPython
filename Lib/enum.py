"""Enumerations: classes whose attributes are a fixed set of named,
singleton members.

The machinery lives in the metaclass EnumType: while a class body runs,
its namespace (an EnumDict) records which names are members; when the
class is created the members are instantiated, registered in the lookup
tables (_member_map_, _value2member_map_, ...) and installed as class
attributes.  Flag adds bitwise composition of members.
"""

import sys
import builtins as bltns
from types import DynamicClassAttribute, MappingProxyType

__all__ = [
        'EnumType', 'EnumMeta', 'EnumDict',
        'Enum', 'IntEnum', 'StrEnum', 'Flag', 'IntFlag', 'ReprEnum',
        'auto', 'unique', 'property', 'verify', 'member', 'nonmember',
        'FlagBoundary', 'STRICT', 'CONFORM', 'EJECT', 'KEEP',
        'global_flag_repr', 'global_enum_repr', 'global_str', 'global_enum',
        'EnumCheck', 'CONTINUOUS', 'NAMED_FLAGS', 'UNIQUE',
        'pickle_by_global_name', 'pickle_by_enum_name', 'show_flag_values',
        ]

# The base classes are referenced by the metaclass before they exist; they
# are bound for real further down.
Enum = Flag = ReprEnum = STRICT = EJECT = KEEP = None


# ---------------------------------------------------------------------------
# Name classification helpers
# ---------------------------------------------------------------------------

def _is_descriptor(obj):
    """True if *obj* takes part in the descriptor protocol."""
    return (hasattr(obj, '__get__') or hasattr(obj, '__set__')
            or hasattr(obj, '__delete__'))


def _is_dunder(name):
    """True for names of the form __xxx__ (at least one inner character
    that is not an underscore at either end)."""
    return (len(name) > 4
            and name.startswith('__') and name.endswith('__')
            and name[2] != '_' and name[-3] != '_')


def _is_sunder(name):
    """True for names of the form _xxx_ (single underscores)."""
    return (len(name) > 2
            and name[0] == '_' and name[-1] == '_'
            and name[1] != '_' and name[-2] != '_')


def _is_private(cls_name, name):
    """True if *name* is a mangled private name of class *cls_name*."""
    prefix = '_%s__' % (cls_name, )
    return (name.startswith(prefix) and len(name) > len(prefix)
            and not name.endswith('__'))


def _is_single_bit(num):
    """True if the integer *num* has exactly one bit set."""
    return num > 0 and (num & (num - 1)) == 0


def _iter_bits_lsb(num):
    """Yield the set bits of *num* as integers, lowest first."""
    if num < 0:
        raise ValueError('%r is not a positive integer' % num)
    while num:
        lowest = num & -num
        yield lowest
        num ^= lowest


def show_flag_values(value):
    """List the individual bit values that make up *value*."""
    return list(_iter_bits_lsb(value))


def _bit_or(values):
    total = 0
    for v in values:
        total |= v
    return total


# ---------------------------------------------------------------------------
# Pickling helpers
# ---------------------------------------------------------------------------

def _refuse_pickle(self, proto):
    raise TypeError('%r cannot be pickled' % self)


def _make_class_unpicklable(obj):
    """Make an enum class (or the namespace for one) unpicklable."""
    if isinstance(obj, dict):
        obj['__reduce_ex__'] = _refuse_pickle
        obj['__module__'] = '<unknown>'
    else:
        setattr(obj, '__reduce_ex__', _refuse_pickle)
        setattr(obj, '__module__', '<unknown>')


def pickle_by_global_name(self, proto):
    """__reduce_ex__ for members that are exported as module globals."""
    return self.name


_reduce_ex_by_global_name = pickle_by_global_name


def pickle_by_enum_name(self, proto):
    """__reduce_ex__ that pickles a member as ``getattr(EnumClass, name)``."""
    return getattr, (self.__class__, self._name_)


# ---------------------------------------------------------------------------
# Markers used in class bodies
# ---------------------------------------------------------------------------

class _AutoUnset:
    def __repr__(self):
        return '_auto_null'


_auto_null = _AutoUnset()
_not_given = object()      # "argument omitted" marker for EnumType.__call__


class auto:
    """Placeholder for a member value that _generate_next_value_ fills in
    when the member is defined."""

    def __init__(self, value=_auto_null):
        self.value = value

    def __repr__(self):
        return 'auto(%r)' % (self.value, )


class nonmember(object):
    """Protect *value* from becoming a member of the enum it is defined in."""

    def __init__(self, value):
        self.value = value


class member(object):
    """Force *value* to become a member, even if it would normally not be
    (functions, nested classes, ...)."""

    def __init__(self, value):
        self.value = value


class property(DynamicClassAttribute):
    """Descriptor for enum attributes that may share a name with a member.

    Accessed on a member it behaves like a normal property; accessed on the
    class it yields the member of that name (if any).
    """

    member = None
    # the descriptor that a member of the same name hides, used for
    # instance access
    _shadowed = None

    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        super().__init__(fget, fset, fdel, doc)
        self.name = None
        self.clsname = None

    def __set_name__(self, ownerclass, name):
        self.name = name
        self.clsname = ownerclass.__name__

    def __get__(self, instance, ownerclass=None):
        if instance is None:
            if self.member is not None:
                return self.member
            raise AttributeError(
                    '%r has no attribute %r' % (ownerclass, self.name))
        if self.fget is not None:
            return self.fget(instance)
        shadowed = self._shadowed
        if shadowed is not None and hasattr(shadowed, '__get__'):
            return shadowed.__get__(instance, ownerclass)
        if shadowed is not None:
            return shadowed
        if self.member is not None:
            return self.member
        raise AttributeError(
                '%r member has no attribute %r' % (ownerclass, self.name))

    def __set__(self, instance, value):
        if self.fset is not None:
            return self.fset(instance, value)
        shadowed = self._shadowed
        if shadowed is not None and hasattr(shadowed, '__set__'):
            return shadowed.__set__(instance, value)
        raise AttributeError(
                "<enum %r> cannot set attribute %r" % (self.clsname, self.name))

    def __delete__(self, instance):
        if self.fdel is not None:
            return self.fdel(instance)
        shadowed = self._shadowed
        if shadowed is not None and hasattr(shadowed, '__delete__'):
            return shadowed.__delete__(instance)
        raise AttributeError(
                "<enum %r> cannot delete attribute %r" % (self.clsname, self.name))


def _install_member(enum_class, name, member):
    """Bind *member* as class attribute *name* of *enum_class*.

    If a base class already defines a descriptor under that name (a
    method of the mixed-in type, Enum's own ``name``/``value``, a
    property...), the member is wrapped in an enum.property so that the
    class attribute gives the member while instance access keeps using the
    inherited descriptor.
    """
    hidden = None
    for base in enum_class.__mro__[1:]:
        if name in base.__dict__:
            hidden = base.__dict__[name]
            break
    if hidden is None or not _is_descriptor(hidden):
        type.__setattr__(enum_class, name, member)
        return
    proxy = property()
    proxy.member = member
    if isinstance(hidden, (bltns.property, DynamicClassAttribute)):
        proxy.fget = hidden.fget
        proxy.fset = hidden.fset
        proxy.fdel = hidden.fdel
    else:
        proxy._shadowed = hidden
    proxy.__set_name__(enum_class, name)
    type.__setattr__(enum_class, name, proxy)


# ---------------------------------------------------------------------------
# Class namespace
# ---------------------------------------------------------------------------

# sunder names an enum class body may define
_ALLOWED_SUNDERS = frozenset((
        '_order_', '_generate_next_value_', '_numeric_repr_', '_missing_',
        '_ignore_', '_iter_member_', '_iter_member_by_value_',
        '_iter_member_by_def_', '_add_alias_', '_add_value_alias_',
        '_value_repr_',
        ))


class EnumDict(dict):
    """Namespace used while an enum class body executes.

    Assignments of plain values are recorded as members (in definition
    order); redefining a member name is an error, as are unknown _sunder_
    names.
    """

    def __init__(self, cls_name=None):
        super().__init__()
        self._member_names = {}         # ordered set of member names
        self._last_values = []
        self._ignore = []
        self._auto_called = False
        self._generate_next_value = None
        self._cls_name = cls_name

    @bltns.property
    def member_names(self):
        return list(self._member_names)

    def _next_auto_value(self, key):
        gnv = self._generate_next_value
        if gnv is None:
            raise TypeError('auto() used without _generate_next_value_')
        self._auto_called = True
        value = gnv(key, 1, len(self._member_names), self._last_values[:])
        self._last_values.append(value)
        return value

    def _resolve_autos(self, key, value):
        """Replace auto() markers in a member value; returns the value and
        whether anything was replaced."""
        if isinstance(value, auto):
            if value.value is _auto_null:
                value.value = self._next_auto_value(key)
                return value.value, True
            return value.value, False
        if isinstance(value, tuple) and any(isinstance(v, auto) for v in value):
            items = []
            for v in value:
                if isinstance(v, auto):
                    if v.value is _auto_null:
                        v.value = self._next_auto_value(key)
                    v = v.value
                items.append(v)
            if type(value) is tuple:
                return tuple(items), True
            return type(value)(*items), True
        return value, False

    def _is_nested_class(self, key, value):
        # a class defined inside the enum body (as opposed to an existing
        # class assigned to a name) is not a member
        if not isinstance(value, type):
            return False
        outer = dict.get(self, '__qualname__')
        return (outer is not None
                and getattr(value, '__qualname__', None) == '%s.%s' % (outer, key))

    def __setitem__(self, key, value):
        if self._cls_name is not None and _is_private(self._cls_name, key):
            # mangled private names are ordinary attributes
            pass
        elif _is_sunder(key):
            if key not in _ALLOWED_SUNDERS:
                raise ValueError(
                        '_sunder_ names, such as %r, are reserved for future'
                        ' Enum use' % (key, ))
            if key == '_generate_next_value_':
                if self._auto_called:
                    raise TypeError(
                            "_generate_next_value_ must be defined before members")
                if not isinstance(value, staticmethod):
                    value = staticmethod(value)
                self._generate_next_value = value.__func__
            elif key == '_ignore_':
                if isinstance(value, str):
                    names = value.replace(',', ' ').split()
                else:
                    names = list(value)
                clash = set(names) & set(self._member_names)
                if clash:
                    raise ValueError(
                            '_ignore_ cannot specify already set names: %r'
                            % (sorted(clash), ))
                self._ignore = names
        elif _is_dunder(key):
            if key == '__order__':
                key = '_order_'
        elif key in self._member_names:
            raise TypeError('%r already defined as %r' % (key, self[key]))
        elif key in self._ignore:
            pass
        elif isinstance(value, nonmember):
            value = value.value
        elif _is_descriptor(value) or self._is_nested_class(key, value):
            pass
        else:
            # a member
            if isinstance(value, member):
                value = value.value
            if key in self:
                raise TypeError('%r already defined as %r' % (key, self[key]))
            value, from_auto = self._resolve_autos(key, value)
            self._member_names[key] = None
            if not from_auto:
                self._last_values.append(value)
        super().__setitem__(key, value)

    def update(self, members=(), /, **more_members):
        if hasattr(members, 'keys'):
            for key in members.keys():
                self[key] = members[key]
        else:
            for key, value in members:
                self[key] = value
        for key, value in more_members.items():
            self[key] = value


_EnumDict = EnumDict        # name used by older code


# ---------------------------------------------------------------------------
# The metaclass
# ---------------------------------------------------------------------------

# Enum classes whose members are being created; Enum.__new__ uses it to
# detect super().__new__() calls from member constructors.
_UNDER_CONSTRUCTION = set()


class _MemberBuilder:
    """Placed (last) in the namespace of a new enum class; type.__new__
    calls __set_name__ on it before __init_subclass__, which is when the
    members get created."""

    def __init__(self, plan):
        self.plan = plan

    def __set_name__(self, enum_class, name):
        type.__delattr__(enum_class, name)
        _build_members(enum_class, self.plan)


class _BuildPlan:
    # everything _build_members needs, collected by EnumType.__new__
    __slots__ = ('names', 'values', 'member_type', 'new_member', 'use_args',
                 'order', 'is_flag', 'boundary', 'body', 'first_enum',
                 'bases')


class EnumType(type):
    """Metaclass for Enum."""

    @classmethod
    def __prepare__(metacls, cls, bases, **kwds):
        metacls._check_for_existing_members_(cls, bases)
        enum_dict = EnumDict(cls)
        member_type, first_enum = metacls._get_mixins_(cls, bases)
        if first_enum is not None:
            enum_dict._generate_next_value = getattr(
                    first_enum, '_generate_next_value_', None)
        return enum_dict

    def __new__(metacls, cls, bases, classdict, *, boundary=None, **kwds):
        if not isinstance(classdict, EnumDict):
            plain = classdict
            classdict = metacls.__prepare__(cls, bases)
            for key, value in plain.items():
                classdict[key] = value

        ignored = list(classdict._ignore)
        member_type, first_enum = metacls._get_mixins_(cls, bases)
        new_member, user_new, use_args = metacls._choose_member_new(
                classdict, member_type, first_enum)

        body = dict(classdict)
        for name in ignored + ['_ignore_']:
            body.pop(name, None)

        names = list(classdict._member_names)
        bad = [n for n in names if n in ('mro', '')]
        if bad:
            raise ValueError('invalid enum member name(s) %s'
                             % ', '.join(repr(n) for n in bad))
        values = {}
        for name in names:
            values[name] = body.pop(name)

        order = body.pop('_order_', None)
        if isinstance(order, str):
            order = order.replace(',', ' ').split()

        if user_new is not None:
            body['__new_member__'] = user_new
            body['__new__'] = staticmethod(Enum.__new__)

        body['_new_member_'] = new_member
        body['_use_args_'] = use_args
        body['_member_names_'] = []
        body['_member_map_'] = {}
        body['_value2member_map_'] = {}
        body['_unhashable_values_'] = []
        body['_member_type_'] = member_type

        is_flag = Flag is not None and any(issubclass(b, Flag) for b in bases)
        if Flag is None and boundary is not None:
            # defining Flag itself
            is_flag = True
        if is_flag:
            boundary = (boundary
                        or getattr(first_enum, '_boundary_', None)
                        or STRICT)
            body['_boundary_'] = boundary
            body['_flag_mask_'] = 0
            body['_singles_mask_'] = 0
            body['_all_bits_'] = 0

        if '_value_repr_' not in body:
            value_repr = _choose_value_repr(member_type, first_enum)
            if value_repr is not _INHERIT:
                body['_value_repr_'] = value_repr

        plan = _BuildPlan()
        plan.names = names
        plan.values = values
        plan.member_type = member_type
        plan.new_member = new_member
        plan.use_args = use_args
        plan.order = order
        plan.is_flag = is_flag
        plan.boundary = boundary
        plan.body = frozenset(body)
        plan.first_enum = first_enum
        plan.bases = bases
        body['_enum_member_builder_'] = _MemberBuilder(plan)

        return super().__new__(metacls, cls, bases, body, **kwds)

    def __init__(cls, *args, **kwds):
        super().__init__(*args)

    # -- helpers used while creating a class -------------------------------

    @classmethod
    def _check_for_existing_members_(mcls, class_name, bases):
        for chain in bases:
            for base in chain.__mro__:
                if isinstance(base, EnumType) and base._member_map_:
                    kind = 'flag' if Flag is not None and issubclass(base, Flag) else 'enum'
                    raise TypeError(
                            "<%s %r> cannot extend %r"
                            % (kind, class_name, base))

    @classmethod
    def _get_mixins_(mcls, class_name, bases):
        """Return (data type, last enum base) for a new enum class."""
        if not bases:
            return object, Enum
        last = bases[-1]
        if not isinstance(last, EnumType):
            raise TypeError("new enumerations should be created as "
                    "`EnumName([mixin_type, ...] [data_type,] enum_type)`")
        return mcls._find_data_type_(class_name, bases), last

    @classmethod
    def _find_data_type_(mcls, class_name, bases):
        found = set()
        for chain in bases:
            candidate = None
            for base in chain.__mro__:
                if base is object:
                    continue
                if isinstance(base, EnumType):
                    if base._member_type_ is not object:
                        found.add(base._member_type_)
                        break
                    continue
                if '__new__' in base.__dict__ or '__dataclass_fields__' in base.__dict__:
                    found.add(candidate or base)
                    break
                candidate = candidate or base
        if len(found) > 1:
            raise TypeError('too many data types for %r: %r'
                            % (class_name, found))
        return found.pop() if found else object

    @classmethod
    def _choose_member_new(mcls, classdict, member_type, first_enum):
        """Pick the callable that creates member instances.

        Returns (constructor, user-defined __new__ or None, use_args).
        """
        if Enum is None:
            # creating Enum itself: its __new__ is the value lookup
            return object.__new__, None, False
        user_new = None
        for key in ('__new_member__', '__new__'):
            candidate = dict.get(classdict, key)
            if candidate is not None:
                if isinstance(candidate, staticmethod):
                    candidate = candidate.__func__
                if candidate is not Enum.__new__:
                    user_new = candidate
                    break
        if user_new is not None:
            return user_new, user_new, True
        inherited = getattr(first_enum, '_new_member_', None)
        if inherited is not None and inherited is not object.__new__:
            return inherited, None, True
        if member_type is not object and member_type.__new__ is not object.__new__:
            return member_type.__new__, None, True
        return object.__new__, None, False

    @classmethod
    def _fix_inherited_methods(mcls, enum_class, body, member_type, first_enum, bases):
        """Make sure Enum's own behaviour wins over the mixed-in data type
        (whose methods come first in the MRO)."""
        if Enum is None:
            return
        if Enum.__new__ is not enum_class.__new__:
            type.__setattr__(enum_class, '__new__', staticmethod(Enum.__new__))

        own = set(body)
        if ReprEnum is not None and ReprEnum in bases:
            if member_type is object:
                raise TypeError(
                        'ReprEnum subclasses must be mixed with a data type'
                        ' (i.e. int, str, float, etc.)')
            if '__format__' not in body:
                type.__setattr__(enum_class, '__format__', member_type.__format__)
                own.add('__format__')
            if '__str__' not in body:
                text = member_type.__str__
                if text is object.__str__:
                    text = member_type.__repr__
                type.__setattr__(enum_class, '__str__', text)
                own.add('__str__')

        for name in ('__repr__', '__str__', '__format__', '__reduce_ex__'):
            if name in own:
                continue
            current = getattr(enum_class, name)
            if (current is getattr(object, name)
                    or current is getattr(member_type, name)
                    or _defined_outside_enums(enum_class, name, member_type)):
                type.__setattr__(enum_class, name, getattr(first_enum, name))

        if Flag is not None and issubclass(enum_class, Flag) and member_type is not object:
            for name in ('__or__', '__and__', '__xor__',
                         '__ror__', '__rand__', '__rxor__', '__invert__'):
                if name not in body:
                    type.__setattr__(enum_class, name, getattr(Flag, name))

    # -- class-level protocol ---------------------------------------------

    def __call__(cls, value, names=_not_given, *values, module=None, qualname=None,
                 type=None, start=1, boundary=None):
        """Look up a member by value, or (on an enum without members)
        create a new enum class through the functional API."""
        if cls._member_map_:
            if names is not _not_given:
                value = (value, names) + values
            return cls.__new__(cls, value)
        if names is _not_given and type is None:
            raise TypeError(
                    "%r has no members; specify `names=()` if you meant to"
                    " create a new, empty, enum" % (cls, ))
        return cls._create_(
                class_name=value, names=None if names is _not_given else names,
                module=module,
                qualname=qualname, type=type, start=start, boundary=boundary)

    def __contains__(cls, value):
        """Return True if `value` is in `cls`.

        `value` is in `cls` if:
        1) `value` is a member of `cls`, or
        2) `value` is the value of one of the `cls`'s members.
        3) `value` is a pseudo-member (flags)
        """
        if isinstance(value, cls):
            return True
        if Flag is not None and issubclass(cls, Flag):
            try:
                result = cls._missing_(value)
            except ValueError:
                return False
            return isinstance(result, cls)
        return _find_by_value(cls, value) is not _NOT_FOUND

    def __delattr__(cls, attr):
        if attr in cls.__dict__.get('_member_map_', {}):
            raise AttributeError('%r cannot delete member %r.' % (cls.__name__, attr))
        super().__delattr__(attr)

    def __dir__(cls):
        interesting = {
                '__class__', '__contains__', '__doc__', '__getitem__',
                '__iter__', '__len__', '__members__', '__module__',
                '__name__', '__qualname__',
                }
        if Flag is not None and issubclass(cls, Flag):
            interesting.update(cls._member_map_)
        else:
            interesting.update(cls._member_names_)
        if cls._new_member_ is not object.__new__:
            interesting.add('__new__')
        if cls.__init_subclass__ is not object.__init_subclass__:
            interesting.add('__init_subclass__')
        if cls._member_type_ is not object:
            interesting.update(dir(cls._member_type_))
        return sorted(interesting)

    def __getitem__(cls, name):
        """Return the member matching `name`."""
        return cls._member_map_[name]

    def __iter__(cls):
        """Return members in definition order."""
        return (cls._member_map_[name] for name in cls._member_names_)

    def __len__(cls):
        """Return the number of members (no aliases)"""
        return len(cls._member_names_)

    @bltns.property
    def __members__(cls):
        """Returns a mapping of member name->value.

        This mapping lists all enum members, including aliases.  Note that
        this is a read-only view of the internal mapping."""
        return MappingProxyType(cls._member_map_)

    def __repr__(cls):
        if Flag is not None and issubclass(cls, Flag):
            return "<flag %r>" % cls.__name__
        return "<enum %r>" % cls.__name__

    def __reversed__(cls):
        """Return members in reverse definition order."""
        return (cls._member_map_[name] for name in reversed(cls._member_names_))

    def __bool__(cls):
        # classes are true even when they have no members
        return True

    def __setattr__(cls, name, value):
        if name in cls.__dict__.get('_member_map_', {}):
            raise AttributeError('cannot reassign member %r' % (name, ))
        super().__setattr__(name, value)

    @bltns.property
    def __signature__(cls):
        from inspect import Parameter, Signature
        if cls._member_names_:
            return Signature([Parameter('values', Parameter.VAR_POSITIONAL)])
        return Signature([
                Parameter('new_class_name', Parameter.POSITIONAL_ONLY),
                Parameter('names', Parameter.POSITIONAL_OR_KEYWORD),
                Parameter('module', Parameter.KEYWORD_ONLY, default=None),
                Parameter('qualname', Parameter.KEYWORD_ONLY, default=None),
                Parameter('type', Parameter.KEYWORD_ONLY, default=None),
                Parameter('start', Parameter.KEYWORD_ONLY, default=1),
                Parameter('boundary', Parameter.KEYWORD_ONLY, default=None),
                ])

    # -- functional API and conversions --------------------------------------

    def _create_(cls, class_name, names, *, module=None, qualname=None,
                 type=None, start=1, boundary=None):
        """Create a new enum class named *class_name* with members *names*.

        *names* may be a string of names separated by whitespace and/or
        commas, a sequence of names, a sequence of (name, value) pairs, or
        a mapping of names to values.
        """
        metacls = cls.__class__
        bases = (cls, ) if type is None else (type, cls)
        _, first_enum = cls._get_mixins_(class_name, bases)
        classdict = metacls.__prepare__(class_name, bases)

        if isinstance(names, str):
            names = names.replace(',', ' ').split()
        if names is None:
            names = ()
        if isinstance(names, (tuple, list)) and names and isinstance(names[0], str):
            generated = []
            previous = []
            for count, name in enumerate(names):
                value = first_enum._generate_next_value_(name, start, count, previous[:])
                previous.append(value)
                generated.append((name, value))
            names = generated

        for item in names:
            if isinstance(item, str):
                classdict[item] = names[item]
            else:
                key, value = item
                classdict[key] = value

        if module is None:
            module = _caller_module(3)
        if module is None:
            _make_class_unpicklable(classdict)
        else:
            classdict['__module__'] = module
        if qualname is not None:
            classdict['__qualname__'] = qualname
        return metacls.__new__(metacls, class_name, bases, classdict, boundary=boundary)

    def _convert_(cls, name, module, filter, source=None, *, boundary=None, as_global=False):
        """Create an enum named *name* in *module* from the module-level
        constants (of *source*, default the module) selected by *filter*."""
        module_globals = sys.modules[module].__dict__
        if source is None:
            source = module_globals
        else:
            source = source.__dict__
        selected = [(key, value) for key, value in source.items() if filter(key)]
        try:
            # lowest value first; equal values are ordered by name so the
            # alphabetically first name becomes the canonical member
            selected.sort(key=lambda item: (item[1], item[0]))
        except TypeError:
            selected.sort(key=lambda item: item[0])
        body = {key: value for key, value in selected}
        body['__module__'] = module
        template = bltns.type(name, (object, ), body)
        new_cls = _simple_enum(etype=cls, boundary=boundary or KEEP)(template)
        new_cls.__reduce_ex__ = _reduce_ex_by_global_name
        if as_global:
            global_enum(new_cls)
        else:
            module_globals.update(new_cls.__members__)
        module_globals[name] = new_cls
        return new_cls


EnumMeta = EnumType          # old name


_INHERIT = object()


def _choose_value_repr(member_type, first_enum):
    """The function an enum class uses to show member values in repr()."""
    if member_type is object:
        return _INHERIT
    fields = member_type.__dict__.get('__dataclass_fields__')
    params = member_type.__dict__.get('__dataclass_params__')
    if fields is not None and params is not None and getattr(params, 'repr', False) \
            and '__repr__' in member_type.__dict__:
        return _dataclass_value_repr
    for klass in member_type.__mro__:
        if '__repr__' in klass.__dict__:
            return klass.__dict__['__repr__']
    return _INHERIT


def _dataclass_value_repr(value):
    fields = type(value).__dataclass_fields__
    return ', '.join('%s=%r' % (name, getattr(value, name))
                     for name, field in fields.items() if field.repr)


def _defined_outside_enums(enum_class, name, member_type):
    # True if *name* resolves to a method of the data type's hierarchy
    # (e.g. int.__repr__ reached through a subclass of int)
    for klass in enum_class.__mro__:
        if name in klass.__dict__:
            if isinstance(klass, EnumType):
                return False
            return issubclass(member_type, klass) and klass is not object
    return False


def _caller_module(depth):
    try:
        return sys._getframemodulename(depth)
    except AttributeError:
        pass
    try:
        return sys._getframe(depth).f_globals['__name__']
    except (AttributeError, ValueError, KeyError):
        return None


_NOT_FOUND = object()


def _find_by_value(cls, value):
    """The member (or value alias) whose value equals *value*."""
    try:
        return cls._value2member_map_[value]
    except KeyError:
        for candidate, found in cls._unhashable_values_:
            if candidate == value:
                return found
        return _NOT_FOUND
    except TypeError:
        # unhashable: compare against every member value
        for found in cls._member_map_.values():
            if found._value_ == value:
                return found
        for candidate, found in cls._unhashable_values_:
            if candidate == value:
                return found
        return _NOT_FOUND


def _register_value(cls, value, member):
    """Record *value* -> *member* in the lookup tables of *cls*."""
    try:
        cls._value2member_map_.setdefault(value, member)
    except TypeError:
        cls._unhashable_values_.append((value, member))


# ---------------------------------------------------------------------------
# Member creation
# ---------------------------------------------------------------------------

def _build_members(enum_class, plan):
    EnumType._fix_inherited_methods(enum_class, plan.body, plan.member_type,
                                    plan.first_enum, plan.bases)
    _UNDER_CONSTRUCTION.add(enum_class)
    try:
        deferred = []
        for name in plan.names:
            raw = plan.values[name]
            if plan.is_flag and isinstance(raw, int) and raw < 0:
                # an inverted flag (e.g. ``~A``) can only be resolved once
                # all positive members are known
                deferred.append(name)
                continue
            _create_member(enum_class, plan, name, raw)
        if plan.is_flag:
            _update_flag_masks(enum_class)
            for name in deferred:
                raw = plan.values[name]
                if enum_class._boundary_ in (EJECT, KEEP):
                    resolved = raw & enum_class._all_bits_
                else:
                    resolved = raw & enum_class._singles_mask_
                _create_member(enum_class, plan, name, resolved)
            _update_flag_masks(enum_class)
    finally:
        _UNDER_CONSTRUCTION.discard(enum_class)

    if plan.order is not None:
        expected = [n for n in plan.order
                    if n not in enum_class._member_map_
                    or n in enum_class._member_names_]
        if expected != enum_class._member_names_:
            raise TypeError('member order does not match _order_:\n  %r\n  %r'
                            % (enum_class._member_names_, expected))


def _create_member(enum_class, plan, name, value):
    if isinstance(value, tuple):
        args = value
    else:
        args = (value, )
    if plan.member_type is tuple:
        args = (args, )

    if plan.use_args:
        new_member = plan.new_member(enum_class, *args)
    else:
        new_member = plan.new_member(enum_class)

    if not hasattr(new_member, '_value_'):
        if plan.member_type is object:
            new_member._value_ = value
        else:
            try:
                new_member._value_ = plan.member_type(*args)
            except Exception as exc:
                problem = TypeError('_value_ not set in __new__, unable to create it')
                problem.__cause__ = exc
                try:
                    raise problem
                finally:
                    problem = exc = None
    value = new_member._value_
    new_member._name_ = name
    new_member.__objclass__ = enum_class
    new_member.__init__(*args)

    # an alias if a member with an equal value already exists (__init__ ran
    # first so that it can veto aliases)
    existing = _find_by_value(enum_class, value)
    if existing is not _NOT_FOUND and existing._name_ is not None:
        _install_member(enum_class, name, existing)
        enum_class._member_map_[name] = existing
        return

    new_member._sort_order_ = len(enum_class._member_names_)

    if not plan.is_flag or (isinstance(value, int) and _is_single_bit(value)):
        enum_class._member_names_.append(name)
    _install_member(enum_class, name, new_member)
    enum_class._member_map_[name] = new_member
    _register_value(enum_class, value, new_member)


def _update_flag_masks(enum_class):
    singles = 0
    everything = 0
    for found in enum_class._member_map_.values():
        value = found._value_
        if isinstance(value, int) and value > 0:
            everything |= value
            if _is_single_bit(value):
                singles |= value
    type.__setattr__(enum_class, '_singles_mask_', singles)
    type.__setattr__(enum_class, '_flag_mask_', everything)
    type.__setattr__(enum_class, '_all_bits_', (1 << everything.bit_length()) - 1)


# ---------------------------------------------------------------------------
# Enum
# ---------------------------------------------------------------------------

class Enum(metaclass=EnumType):
    """Base class for enumerations.

    Subclass it and assign the members as class attributes::

        class Color(Enum):
            RED = 1
            GREEN = 2

    Members are looked up by value with ``Color(1)`` and by name with
    ``Color['RED']``; iterating over the class yields the members (without
    aliases) in definition order.
    """

    _value_repr_ = None

    def __new__(cls, value):
        # value lookup; the member constructor is cls._new_member_
        if type(value) is cls:
            return value
        found = _find_by_value(cls, value)
        if found is not _NOT_FOUND:
            return found
        if cls in _UNDER_CONSTRUCTION:
            raise TypeError(
                    'do not use `super().__new__; call the appropriate'
                    ' __new__ directly')
        failure = None
        try:
            result = cls._missing_(value)
        except Exception as exc:
            failure = exc
            result = None
        if isinstance(result, cls):
            return result
        if (Flag is not None and issubclass(cls, Flag)
                and cls._boundary_ is EJECT and isinstance(result, int)):
            return result
        if failure is None and result is not None:
            failure = TypeError(
                    'error in %s._missing_: returned %r instead of None or a'
                    ' valid member' % (cls.__name__, result))
        if failure is None:
            raise ValueError('%r is not a valid %s' % (value, cls.__qualname__))
        if not isinstance(failure, ValueError):
            failure.__context__ = ValueError(
                    '%r is not a valid %s' % (value, cls.__qualname__))
        try:
            raise failure
        finally:
            failure = None

    def __init__(self, *args, **kwds):
        pass

    def _add_alias_(self, name):
        """Make *name* another name of this member."""
        cls = self.__class__
        current = cls._member_map_.get(name)
        if current is not None:
            if current is not self:
                raise NameError('%r is already bound: %r' % (name, current))
            return
        _install_member(cls, name, self)
        cls._member_map_[name] = self

    def _add_value_alias_(self, value):
        """Make *value* another value that looks up this member."""
        cls = self.__class__
        current = _find_by_value(cls, value)
        if current is not _NOT_FOUND:
            if current is not self:
                raise ValueError('%r is already bound: %r' % (value, current))
            return
        _register_value(cls, value, self)

    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        """Value for an auto() member: one more than the highest value so
        far, or *start* for the first member.
        """
        if not count or not last_values:
            return start
        try:
            highest = sorted(last_values)[-1]
        except TypeError:
            raise TypeError('unable to sort non-numeric values') from None
        try:
            return highest + 1
        except TypeError:
            raise TypeError('unable to increment %r' % (highest, )) from None

    @classmethod
    def _missing_(cls, value):
        return None

    def __repr__(self):
        show = self.__class__._value_repr_ or repr
        return "<%s.%s: %s>" % (self.__class__.__name__, self._name_, show(self._value_))

    def __str__(self):
        return "%s.%s" % (self.__class__.__name__, self._name_, )

    def __dir__(self):
        cls = self.__class__
        if cls._member_type_ is object:
            names = {'__class__', '__doc__', '__eq__', '__hash__',
                     '__module__', 'name', 'value'}
        else:
            names = set(object.__dir__(self))
        member_map = self._member_map_
        for klass in cls.mro():
            for key, obj in klass.__dict__.items():
                if key[0] == '_':
                    continue
                if isinstance(obj, property):
                    if obj.fget is not None or key not in member_map:
                        names.add(key)
                    else:
                        names.discard(key)
                elif key not in member_map:
                    names.add(key)
        for key in getattr(self, '__dict__', ()):
            if key[0] != '_' and key not in member_map:
                names.add(key)
        return sorted(names)

    def __format__(self, format_spec):
        return str.__format__(str(self), format_spec)

    def __hash__(self):
        return hash(self._name_)

    def __reduce_ex__(self, proto):
        # by value: unpickling looks the member up again
        return self.__class__, (self._value_, )

    def __deepcopy__(self, memo):
        return self

    def __copy__(self):
        return self

    @property
    def name(self):
        """The name of the Enum member."""
        return self._name_

    @property
    def value(self):
        """The value of the Enum member."""
        return self._value_


class ReprEnum(Enum):
    """Enum whose members use the mixed-in type's str() and format();
    only repr() shows the enum."""


class IntEnum(int, ReprEnum):
    """Enum whose members are also (and compare like) ints."""


class StrEnum(str, ReprEnum):
    """Enum whose members are also (and compare like) strings."""

    def __new__(cls, *values):
        if len(values) > 3:
            raise TypeError('too many arguments for str(): %r' % (values, ))
        if len(values) == 1 and not isinstance(values[0], str):
            raise TypeError('%r is not a string' % (values[0], ))
        if len(values) >= 2 and not isinstance(values[1], str):
            raise TypeError('encoding must be a string, not %r' % (values[1], ))
        if len(values) == 3 and not isinstance(values[2], str):
            raise TypeError('errors must be a string, not %r' % (values[2], ))
        text = str(*values)
        new = str.__new__(cls, text)
        new._value_ = text
        return new

    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        """auto() gives the lower-cased member name."""
        return name.lower()


# ---------------------------------------------------------------------------
# Flag
# ---------------------------------------------------------------------------

class FlagBoundary(StrEnum):
    """How a Flag treats values with bits that no member defines:
    STRICT raises ValueError, CONFORM drops the unknown bits, EJECT returns
    a plain int, KEEP keeps the bits in the pseudo-member."""

    STRICT = auto()
    CONFORM = auto()
    EJECT = auto()
    KEEP = auto()


STRICT, CONFORM, EJECT, KEEP = FlagBoundary


class Flag(Enum, boundary=STRICT):
    """Enum whose members can be combined with the bitwise operators."""

    _numeric_repr_ = repr

    @staticmethod
    def _generate_next_value_(name, start, count, last_values):
        """auto() gives the next power of two above the highest value."""
        if not count:
            return start if start is not None else 1
        highest = max(last_values)
        try:
            bits = highest.bit_length()
        except Exception:
            raise TypeError('invalid flag value %r' % highest) from None
        return 1 << bits

    @classmethod
    def _iter_member_by_value_(cls, value):
        """The single-bit members contained in *value*, lowest bit first."""
        for bit in _iter_bits_lsb(value & cls._flag_mask_):
            found = cls._value2member_map_.get(bit)
            if found is not None:
                yield found

    @classmethod
    def _iter_member_by_def_(cls, value):
        """The single-bit members contained in *value*, in definition order."""
        for name in cls._member_names_:
            found = cls._member_map_[name]
            if found._value_ & value == found._value_:
                yield found

    _iter_member_ = _iter_member_by_def_

    @classmethod
    def _missing_(cls, value):
        """Create (or find) the pseudo-member for a combination of flags."""
        if not isinstance(value, int):
            raise ValueError('%r is not a valid %s' % (value, cls.__qualname__))
        boundary = cls._boundary_
        if value < 0:
            if boundary in (EJECT, KEEP):
                width = max(cls._all_bits_.bit_length(), (~value).bit_length())
                value = value & ((1 << width) - 1)
            else:
                value = value & cls._singles_mask_
        outside = value & ~cls._flag_mask_
        if outside:
            if boundary is STRICT:
                raise ValueError('%r invalid value %r\n    given %s\n  allowed %s'
                                 % (cls, value, bin(value), bin(cls._flag_mask_)))
            if boundary is CONFORM:
                value = value & cls._flag_mask_
            elif boundary is EJECT:
                return int(value)
        known = cls._value2member_map_.get(value)
        if known is not None:
            return known

        if cls._member_type_ is object:
            pseudo = object.__new__(cls)
        else:
            pseudo = cls._member_type_.__new__(cls, value)
        if not hasattr(pseudo, '_value_'):
            pseudo._value_ = value
        pseudo._name_ = cls._compose_name(value)
        return cls._value2member_map_.setdefault(value, pseudo)

    @classmethod
    def _compose_name(cls, value):
        # names of the members covering *value*: single bits first, then
        # multi-bit members needed for the rest; leftover bits are shown
        # numerically
        names = []
        covered = 0
        for found in cls._iter_member_(value):
            names.append(found._name_)
            covered |= found._value_
        for key, found in cls._member_map_.items():
            bits = found._value_
            if (found._name_ != key or not isinstance(bits, int) or bits <= 0
                    or _is_single_bit(bits)):
                continue
            if bits & value == bits and bits & ~covered:
                names.append(key)
                covered |= bits
        if not names:
            return None
        leftover = value & ~covered
        if leftover:
            names.append(cls._numeric_repr_(leftover))
        return '|'.join(names)

    def __repr__(self):
        cls_name = self.__class__.__name__
        show = self.__class__._value_repr_ or repr
        if self._name_ is None:
            return "<%s: %s>" % (cls_name, show(self._value_))
        return "<%s.%s: %s>" % (cls_name, self._name_, show(self._value_))

    def __str__(self):
        cls_name = self.__class__.__name__
        if self._name_ is None:
            return '%s(%r)' % (cls_name, self._value_)
        return '%s.%s' % (cls_name, self._name_)

    def __bool__(self):
        return bool(self._value_)

    def __contains__(self, other):
        """True if all the bits of *other* are set in this flag."""
        if not isinstance(other, self.__class__):
            raise TypeError(
                "unsupported operand type(s) for 'in': %r and %r" % (
                    type(other).__qualname__, self.__class__.__qualname__))
        return other._value_ & self._value_ == other._value_

    def __iter__(self):
        """The single-bit members contained in this flag."""
        yield from self._iter_member_(self._value_)

    def __len__(self):
        return sum(1 for _ in self)

    def _operand_value(self, other):
        if isinstance(other, self.__class__):
            return other._value_
        if self._member_type_ is not object and isinstance(other, self._member_type_):
            return other
        return NotImplemented

    def _combine(self, other, sign, operation):
        other_value = self._operand_value(other)
        if other_value is NotImplemented:
            return NotImplemented
        for flag in (self, other):
            if self._operand_value(flag) is None:
                raise TypeError(
                        "'%s' cannot be combined with other flags with %s"
                        % (flag, sign))
        return self.__class__(operation(self._value_, other_value))

    def __or__(self, other):
        return self._combine(other, '|', lambda a, b: a | b)

    def __and__(self, other):
        return self._combine(other, '&', lambda a, b: a & b)

    def __xor__(self, other):
        return self._combine(other, '^', lambda a, b: a ^ b)

    __ror__ = __or__
    __rand__ = __and__
    __rxor__ = __xor__

    def __invert__(self):
        if self._operand_value(self) is None:
            raise TypeError("'%s' cannot be inverted" % self)
        cached = self.__dict__.get('_inverted_')
        if cached is None:
            if self._boundary_ in (EJECT, KEEP):
                cached = self.__class__(~self._value_)
            else:
                cached = self.__class__(self._singles_mask_ & ~self._value_)
            if isinstance(cached, self.__class__):
                self.__dict__['_inverted_'] = cached
        return cached


class IntFlag(int, ReprEnum, Flag, boundary=KEEP):
    """Flag whose members are also (and compare like) ints."""


# ---------------------------------------------------------------------------
# Decorators and module-level helpers
# ---------------------------------------------------------------------------

def _alias_report(enumeration):
    return ['%s -> %s' % (alias, found._name_)
            for alias, found in enumeration.__members__.items()
            if alias != found._name_]


def unique(enumeration):
    """Class decorator rejecting enums that have aliases."""
    duplicates = _alias_report(enumeration)
    if duplicates:
        raise ValueError('duplicate values found in %r: %s'
                         % (enumeration, ', '.join(duplicates)))
    return enumeration


def global_enum_repr(self):
    """repr() of a member exported to its module: ``module.NAME``."""
    module = self.__class__.__module__.split('.')[-1]
    return '%s.%s' % (module, self._name_)


def global_flag_repr(self):
    """repr() of a flag exported to its module: ``module.A|module.B``."""
    module = self.__class__.__module__.split('.')[-1]
    if self._name_ is None:
        return '%s.%s(%r)' % (module, self.__class__.__name__, self._value_)
    parts = []
    for part in self._name_.split('|'):
        if part.isidentifier():
            parts.append('%s.%s' % (module, part))
        else:
            parts.append(part)
    return '|'.join(parts)


def global_str(self):
    """str() of a member exported to its module: just the name."""
    if self._name_ is None:
        return '%s(%r)' % (self.__class__.__name__, self._value_)
    return self._name_


def global_enum(cls, update_str=False):
    """Class decorator that exports the members into the enum's module and
    makes repr() (and, for non-ReprEnum enums, str()) refer to them as
    module globals."""
    if issubclass(cls, Flag):
        cls.__repr__ = global_flag_repr
    else:
        cls.__repr__ = global_enum_repr
    if update_str or not issubclass(cls, ReprEnum):
        cls.__str__ = global_str
    sys.modules[cls.__module__].__dict__.update(cls.__members__)
    return cls


def _simple_enum(etype=Enum, *, boundary=None, use_args=None):
    """Class decorator that turns an ordinary class into an enum of type
    *etype*; the class attributes become the members."""
    def convert_class(cls):
        metacls = type(etype)
        bases = (etype, )
        kwds = {}
        if boundary is not None:
            kwds['boundary'] = boundary
        classdict = metacls.__prepare__(cls.__name__, bases, **kwds)
        for key, value in cls.__dict__.items():
            if key in ('__dict__', '__weakref__'):
                continue
            classdict[key] = value
        return metacls(cls.__name__, bases, classdict, **kwds)
    return convert_class


class EnumCheck(StrEnum):
    """Properties verify() can check."""

    CONTINUOUS = "values form an unbroken range"
    NAMED_FLAGS = "every bit of every multi-bit flag has a name"
    UNIQUE = "no value has more than one name"


CONTINUOUS, NAMED_FLAGS, UNIQUE = EnumCheck


class verify:
    """Class decorator checking an enumeration for the given EnumChecks."""

    def __init__(self, *checks):
        self.checks = checks

    def __call__(self, enumeration):
        if issubclass(enumeration, Flag):
            kind = 'flag'
        elif issubclass(enumeration, Enum):
            kind = 'enum'
        else:
            raise TypeError("the 'verify' decorator only works with Enum and Flag")
        for check in self.checks:
            if check is UNIQUE:
                duplicates = _alias_report(enumeration)
                if duplicates:
                    raise ValueError('aliases found in %r: %s'
                                     % (enumeration, ', '.join(duplicates)))
            elif check is CONTINUOUS:
                _verify_continuous(enumeration, kind)
            elif check is NAMED_FLAGS:
                _verify_named_flags(enumeration)
        return enumeration


def _verify_continuous(enumeration, kind):
    values = {m._value_ for m in enumeration}
    if len(values) < 2:
        return
    low, high = min(values), max(values)
    if kind == 'flag':
        wanted = [1 << bit for bit in range(low.bit_length(), high.bit_length() - 1)]
    else:
        wanted = list(range(low + 1, high))
    missing = [v for v in wanted if v not in values]
    if missing:
        message = 'invalid %s %r: missing values %s' % (
                kind, enumeration.__name__, ', '.join(str(v) for v in missing))
        raise ValueError(message[:256])


def _verify_named_flags(enumeration):
    named = {m._value_ for m in enumeration}
    offenders = []
    missing_bits = 0
    for key, found in enumeration._member_map_.items():
        if key in enumeration._member_names_:
            continue
        bits = found._value_
        if not isinstance(bits, int) or bits < 0:
            continue
        unnamed = [b for b in _iter_bits_lsb(bits) if b not in named]
        if unnamed:
            offenders.append(key)
            missing_bits |= _bit_or(unnamed)
    if not offenders:
        return
    if len(offenders) == 1:
        who = 'alias %s is missing' % offenders[0]
    else:
        who = 'aliases %s and %s are missing' % (
                ', '.join(offenders[:-1]), offenders[-1])
    if _is_single_bit(missing_bits):
        what = 'value 0x%x' % missing_bits
    else:
        what = 'combined values of 0x%x' % missing_bits
    raise ValueError('invalid Flag %r: %s %s [use enum.show_flag_values(value) for details]'
                     % (enumeration.__name__, who, what))


# ---------------------------------------------------------------------------
# Support for the standard library's own tests
# ---------------------------------------------------------------------------

_IGNORED_IN_COMPARISON = frozenset((
        '__module__', '__doc__', '__firstlineno__', '__static_attributes__',
        '__dict__', '__weakref__',
        '_member_map_', '_value2member_map_', '_unhashable_values_',
        '_member_names_', '_new_member_', '__new_member__', '_value_repr_',
        ))


def _test_simple_enum(checked_enum, simple_enum):
    """Raise TypeError if an enum built with _simple_enum differs from the
    equivalent class built the ordinary way."""
    problems = []
    checked = checked_enum.__dict__
    simple = simple_enum.__dict__
    members = set(checked_enum._member_map_) | set(simple_enum._member_map_)
    for key in sorted(set(checked) | set(simple), key=str):
        if key in _IGNORED_IN_COMPARISON or key in members:
            continue
        if key not in simple:
            problems.append('missing key: %r' % (key, ))
        elif key not in checked:
            problems.append('extra key: %r' % (key, ))
        else:
            a, b = checked[key], simple[key]
            if callable(a) or isinstance(a, (bltns.property, DynamicClassAttribute,
                                             staticmethod, classmethod)):
                continue
            if a != b:
                problems.append('%r: %r != %r' % (key, a, b))
    if list(checked_enum._member_names_) != list(simple_enum._member_names_):
        problems.append('member order: %r != %r'
                        % (checked_enum._member_names_, simple_enum._member_names_))
    for name in sorted(members):
        if name not in simple_enum._member_map_:
            problems.append('missing member: %r' % (name, ))
            continue
        if name not in checked_enum._member_map_:
            problems.append('extra member: %r' % (name, ))
            continue
        a = checked_enum._member_map_[name]
        b = simple_enum._member_map_[name]
        for attr in ('_name_', '_value_'):
            if getattr(a, attr) != getattr(b, attr):
                problems.append('%s.%s: %r != %r'
                                % (name, attr, getattr(a, attr), getattr(b, attr)))
        if type(a._value_) is not type(b._value_):
            problems.append('%s value type: %r != %r'
                            % (name, type(a._value_), type(b._value_)))
        extra_a = {k: v for k, v in getattr(a, '__dict__', {}).items()
                   if k not in ('__objclass__', '_inverted_', '_sort_order_')}
        extra_b = {k: v for k, v in getattr(b, '__dict__', {}).items()
                   if k not in ('__objclass__', '_inverted_', '_sort_order_')}
        if extra_a != extra_b:
            problems.append('%s attributes: %r != %r' % (name, extra_a, extra_b))
    if problems:
        raise TypeError('enum mismatch:\n   %s' % '\n   '.join(problems))


def _old_convert_(etype, name, module, filter, source=None, *, boundary=None):
    """Build an enum from module constants through the functional API (the
    reference that _convert_ results are tested against)."""
    module_globals = sys.modules[module].__dict__
    if source is None:
        source = module_globals
    else:
        source = source.__dict__
    selected = [(key, value) for key, value in source.items() if filter(key)]
    try:
        selected.sort(key=lambda item: (item[1], item[0]))
    except TypeError:
        selected.sort(key=lambda item: item[0])
    new_cls = etype(name, selected, module=module, boundary=boundary or KEEP)
    new_cls.__reduce_ex__ = _reduce_ex_by_global_name
    return new_cls
