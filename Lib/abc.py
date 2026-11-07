"""Abstract base classes (PEP 3119).

A class whose metaclass is ABCMeta (most simply: a subclass of ABC) cannot
be instantiated while it has abstract methods, i.e. methods decorated with
@abstractmethod that no subclass has overridden.  ABCs can also adopt
unrelated classes as "virtual subclasses" with register(); isinstance() and
issubclass() then accept them, although the ABC does not appear in their
MRO.
"""


def abstractmethod(funcobj):
    """Mark a method (or a property, classmethod, ...) as abstract.

    Only has an effect in classes whose metaclass is ABCMeta: such a class
    cannot be instantiated until every abstract method is overridden.  The
    abstract implementation can still be reached through super().
    """
    funcobj.__isabstractmethod__ = True
    return funcobj


class abstractclassmethod(classmethod):
    """Deprecated: use @classmethod stacked on @abstractmethod."""

    __isabstractmethod__ = True

    def __init__(self, callable):
        callable.__isabstractmethod__ = True
        super().__init__(callable)


class abstractstaticmethod(staticmethod):
    """Deprecated: use @staticmethod stacked on @abstractmethod."""

    __isabstractmethod__ = True

    def __init__(self, callable):
        callable.__isabstractmethod__ = True
        super().__init__(callable)


class abstractproperty(property):
    """Deprecated: use @property stacked on @abstractmethod."""

    __isabstractmethod__ = True


try:
    from _abc import (get_cache_token, _abc_init, _abc_register,
                      _abc_instancecheck, _abc_subclasscheck, _get_dump,
                      _reset_registry, _reset_caches)
except ImportError:
    # no accelerator: use the pure Python machinery
    from _py_abc import ABCMeta, get_cache_token
    ABCMeta.__module__ = 'abc'
else:
    class ABCMeta(type):
        """Metaclass of abstract base classes.

        Classes created with it track their abstract methods and keep a
        registry of virtual subclasses, consulted by isinstance() and
        issubclass().
        """

        def __new__(mcls, name, bases, namespace, /, **kwargs):
            cls = super().__new__(mcls, name, bases, namespace, **kwargs)
            _abc_init(cls)
            return cls

        def register(cls, subclass):
            """Make subclass a virtual subclass of cls and return it (so
            this can be used as a class decorator)."""
            return _abc_register(cls, subclass)

        def __instancecheck__(cls, instance):
            """isinstance(instance, cls), taking virtual subclasses into
            account."""
            return _abc_instancecheck(cls, instance)

        def __subclasscheck__(cls, subclass):
            """issubclass(subclass, cls), taking virtual subclasses into
            account."""
            return _abc_subclasscheck(cls, subclass)

        def _dump_registry(cls, file=None):
            """Print the registry and caches of cls (for debugging)."""
            registry, cache, negative, negative_version = _get_dump(cls)
            lines = [
                f"Class: {cls.__module__}.{cls.__qualname__}",
                f"Inv. counter: {get_cache_token()}",
                f"_abc_registry: {registry!r}",
                f"_abc_cache: {cache!r}",
                f"_abc_negative_cache: {negative!r}",
                f"_abc_negative_cache_version: {negative_version!r}",
            ]
            for line in lines:
                print(line, file=file)

        def _abc_registry_clear(cls):
            """Forget all registered virtual subclasses (for tests)."""
            _reset_registry(cls)

        def _abc_caches_clear(cls):
            """Empty the isinstance/issubclass caches (for tests)."""
            _reset_caches(cls)


def update_abstractmethods(cls):
    """Recompute cls.__abstractmethods__ after methods were added to or
    implemented on an already created abstract class (for example by a
    class decorator).  Returns cls; classes that are not abstract classes
    are returned unchanged.
    """
    if not hasattr(cls, '__abstractmethods__'):
        return cls
    abstract = {name for name, value in cls.__dict__.items()
                if getattr(value, '__isabstractmethod__', False)}
    # names that were abstract in a base stay abstract unless cls (or
    # something earlier in its MRO) now provides a concrete version
    for base in cls.__bases__:
        for name in getattr(base, '__abstractmethods__', ()):
            if getattr(getattr(cls, name, None), '__isabstractmethod__', False):
                abstract.add(name)
    cls.__abstractmethods__ = frozenset(abstract)
    return cls


class ABC(metaclass=ABCMeta):
    """Base class giving its subclasses ABCMeta as metaclass."""

    __slots__ = ()
