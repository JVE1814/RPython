"""Attribute-forwarding base class used to stack text filters."""


class Delegator:
    """Forward unknown attributes to *delegate*.

    Forwarded attributes are cached on the instance (so the next lookup is
    fast); the cache is dropped when the delegate changes.
    """

    def __init__(self, delegate=None):
        self.delegate = delegate
        self.__cache = set()

    def __getattr__(self, name):
        value = getattr(self.delegate, name)
        setattr(self, name, value)
        self.__cache.add(name)
        return value

    def resetcache(self):
        """Forget all cached attributes of the delegate."""
        for name in self.__cache:
            try:
                delattr(self, name)
            except AttributeError:
                pass
        self.__cache.clear()

    def setdelegate(self, delegate):
        """Forward to *delegate* from now on."""
        self.resetcache()
        self.delegate = delegate


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_delegator', verbosity=2)
