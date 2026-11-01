"""Record of incompatible language changes ("future statements").

Each feature name maps to a _Feature recording the release that introduced
it (optional release), the release in which it became / will become standard
(mandatory release, or None if it never will) and the compiler flag that
enables it for compile().
"""

all_feature_names = [
    "nested_scopes",
    "generators",
    "division",
    "absolute_import",
    "with_statement",
    "print_function",
    "unicode_literals",
    "barry_as_FLUFL",
    "generator_stop",
    "annotations",
]

__all__ = ["all_feature_names"] + all_feature_names

# Compiler flags (must match the CO_FUTURE_* values used by the compiler).
CO_NESTED = 0x0010
CO_GENERATOR_ALLOWED = 0
CO_FUTURE_DIVISION = 0x20000
CO_FUTURE_ABSOLUTE_IMPORT = 0x40000
CO_FUTURE_WITH_STATEMENT = 0x80000
CO_FUTURE_PRINT_FUNCTION = 0x100000
CO_FUTURE_UNICODE_LITERALS = 0x200000
CO_FUTURE_BARRY_AS_BDFL = 0x400000
CO_FUTURE_GENERATOR_STOP = 0x800000
CO_FUTURE_ANNOTATIONS = 0x1000000


class _Feature:

    def __init__(self, optionalRelease, mandatoryRelease, compiler_flag):
        self.optional = optionalRelease
        self.mandatory = mandatoryRelease
        self.compiler_flag = compiler_flag

    def getOptionalRelease(self):
        """Release in which the feature became available (a sys.version_info-like tuple)."""
        return self.optional

    def getMandatoryRelease(self):
        """Release in which the feature becomes the default, or None if never."""
        return self.mandatory

    def __repr__(self):
        return "_Feature" + repr((self.optional, self.mandatory, self.compiler_flag))


nested_scopes = _Feature((2, 1, 0, "beta", 1), (2, 2, 0, "alpha", 0), CO_NESTED)

generators = _Feature((2, 2, 0, "alpha", 1), (2, 3, 0, "final", 0), CO_GENERATOR_ALLOWED)

division = _Feature((2, 2, 0, "alpha", 2), (3, 0, 0, "alpha", 0), CO_FUTURE_DIVISION)

absolute_import = _Feature((2, 5, 0, "alpha", 1), (3, 0, 0, "alpha", 0), CO_FUTURE_ABSOLUTE_IMPORT)

with_statement = _Feature((2, 5, 0, "alpha", 1), (2, 6, 0, "alpha", 0), CO_FUTURE_WITH_STATEMENT)

print_function = _Feature((2, 6, 0, "alpha", 2), (3, 0, 0, "alpha", 0), CO_FUTURE_PRINT_FUNCTION)

unicode_literals = _Feature((2, 6, 0, "alpha", 2), (3, 0, 0, "alpha", 0), CO_FUTURE_UNICODE_LITERALS)

barry_as_FLUFL = _Feature((3, 1, 0, "alpha", 2), (4, 0, 0, "alpha", 0), CO_FUTURE_BARRY_AS_BDFL)

generator_stop = _Feature((3, 5, 0, "beta", 1), (3, 7, 0, "alpha", 0), CO_FUTURE_GENERATOR_STOP)

annotations = _Feature((3, 7, 0, "beta", 1), None, CO_FUTURE_ANNOTATIONS)
