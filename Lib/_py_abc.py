"""ABCMeta written in Python, used when the _abc accelerator is missing.

Each ABC keeps four attributes: _abc_registry (registered virtual
subclasses), _abc_cache (classes known to be subclasses), _abc_negative_cache
(classes known not to be) and _abc_negative_cache_version (the global
registration counter at the time the negative cache was filled; any
register() call on any ABC makes all negative caches stale).
"""

from _weakrefset import WeakSet


def get_cache_token():
    """Return a value that changes (increases) whenever any ABC registers a
    virtual subclass."""
    return ABCMeta._abc_invalidation_counter


def _abstract_names(cls, namespace, bases):
    names = {name for name, value in namespace.items()
             if getattr(value, "__isabstractmethod__", False)}
    for base in bases:
        for name in getattr(base, "__abstractmethods__", ()):
            if getattr(getattr(cls, name, None), "__isabstractmethod__", False):
                names.add(name)
    return frozenset(names)


class ABCMeta(type):
    """Metaclass of abstract base classes (pure Python version)."""

    _abc_invalidation_counter = 0

    def __new__(mcls, name, bases, namespace, /, **kwargs):
        cls = super().__new__(mcls, name, bases, namespace, **kwargs)
        cls.__abstractmethods__ = _abstract_names(cls, namespace, bases)
        cls._abc_registry = WeakSet()
        cls._abc_cache = WeakSet()
        cls._abc_negative_cache = WeakSet()
        cls._abc_negative_cache_version = ABCMeta._abc_invalidation_counter
        return cls

    def register(cls, subclass):
        """Make subclass a virtual subclass of cls; return subclass."""
        if not isinstance(subclass, type):
            raise TypeError("Can only register classes")
        if issubclass(subclass, cls):
            return subclass             # nothing to do
        if issubclass(cls, subclass):
            raise RuntimeError("Refusing to create an inheritance cycle")
        cls._abc_registry.add(subclass)
        ABCMeta._abc_invalidation_counter += 1
        return subclass

    def _dump_registry(cls, file=None):
        """Print the registry and caches of cls (for debugging)."""
        print(f"Class: {cls.__module__}.{cls.__qualname__}", file=file)
        print(f"Inv. counter: {get_cache_token()}", file=file)
        for attr in ('_abc_registry', '_abc_cache', '_abc_negative_cache',
                     '_abc_negative_cache_version'):
            value = getattr(cls, attr)
            if isinstance(value, WeakSet):
                value = set(value)
            print(f"{attr}: {value!r}", file=file)

    def _abc_registry_clear(cls):
        """Forget all registered virtual subclasses (for tests)."""
        cls._abc_registry.clear()

    def _abc_caches_clear(cls):
        """Empty the positive and negative caches (for tests)."""
        cls._abc_cache.clear()
        cls._abc_negative_cache.clear()

    def __instancecheck__(cls, instance):
        """isinstance(instance, cls)"""
        real_type = type(instance)
        # instance.__class__ may differ from type(instance) (proxies)
        claimed = getattr(instance, '__class__', real_type)
        if claimed in cls._abc_cache:
            return True
        if claimed is real_type:
            if (claimed in cls._abc_negative_cache
                    and cls._abc_negative_cache_version
                    == ABCMeta._abc_invalidation_counter):
                return False
            return cls.__subclasscheck__(claimed)
        return cls.__subclasscheck__(claimed) or \
            cls.__subclasscheck__(real_type)

    def __subclasscheck__(cls, subclass):
        """issubclass(subclass, cls)"""
        if not isinstance(subclass, type):
            raise TypeError('issubclass() arg 1 must be a class')
        if subclass in cls._abc_cache:
            return True
        if cls._abc_negative_cache_version != ABCMeta._abc_invalidation_counter:
            cls._abc_negative_cache = WeakSet()
            cls._abc_negative_cache_version = ABCMeta._abc_invalidation_counter
        elif subclass in cls._abc_negative_cache:
            return False

        verdict = cls.__subclasshook__(subclass)
        if verdict is NotImplemented:
            verdict = _is_derived(cls, subclass)
        else:
            assert isinstance(verdict, bool)
        (cls._abc_cache if verdict else cls._abc_negative_cache).add(subclass)
        return verdict


def _is_derived(cls, subclass):
    # a real subclass, a subclass of a registered class, or a subclass of
    # one of cls's own subclasses (which may have registrations of their own)
    if cls in getattr(subclass, '__mro__', ()):
        return True
    for registered in cls._abc_registry:
        if issubclass(subclass, registered):
            return True
    for child in cls.__subclasses__():
        if issubclass(subclass, child):
            return True
    return False

