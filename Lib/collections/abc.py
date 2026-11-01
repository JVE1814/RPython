from _collections_abc import *
from _collections_abc import __all__
from _collections_abc import _CallableGenericAlias


def __getattr__(attr):
    if attr == "ByteString":
        import warnings
        warnings._deprecated("collections.abc.ByteString", remove=(3, 17))
        from _collections_abc import _ByteString
        globals()["_ByteString"] = _ByteString
        return _ByteString
    raise AttributeError(f"module {__name__!r} has no attribute {attr!r}")
