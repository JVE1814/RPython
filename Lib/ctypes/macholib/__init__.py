"""Mach-O dynamic library name helpers (used by ctypes.util on Apple platforms)."""


def __getattr__(name):
    if name == "__version__":
        import warnings
        warnings._deprecated("__version__", remove=(3, 20))
        return "1.0"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
