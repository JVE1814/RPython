# Workstream "numbers" — notes and decisions

Owner files: `crates/runtime/src/types/{int.rs,int_methods.rs,float.rs,float_math.rs,float_methods.rs,
complex.rs,complex_pow.rs,complex_methods.rs,bool_.rs}`, `crates/runtime/src/modules/{math.rs,cmath.rs,
_struct.rs,binascii.rs,_random.rs,builtins_numbers.rs,numbers_registry.rs}`, `Lib/{numbers,fractions,
random,struct}.py`.

## Layout
* float and complex type definitions were moved out of `core_slots.rs` into `float_methods.rs` /
  `complex_methods.rs` (`init_types()` called from `types::init_all`).
* Shared numerics: `float_math.rs` (CPython float pow special cases, float divmod/mod,
  PyNumber_Float, PyLong_FromDouble, errno-style errors), `complex.rs` (Annex G multiplication and
  division with recovery of infinities/zeros, real/complex mixed division `rc_quot`, `c_abs`),
  `complex_pow.rs` (`c_pow`, integer-exponent fast path |n| <= 100 as CPython).
* `modules/numbers_registry.rs::register()` registers math, math.integer, cmath, _struct, binascii,
  _random (one line in `modules::register_all`).
* `modules/builtins_numbers.rs::install()` (called after the builtins table) provides round, bin,
  oct, hex and `sum` with CPython's int/float/complex fast paths and Neumaier compensated float
  summation (3.12+ semantics).

## Decisions (no spec text available — chosen from memory, untested by the suite)
* ZeroDivisionError texts: "division by zero" for int/float `/`, `//`, divmod and complex `/`;
  "integer modulo by zero" for int `%`, "float modulo by zero" for float `%`.
* `0.0 ** -1` -> "0.0 cannot be raised to a negative power"; complex: "zero to a negative or
  complex power"; complex overflow: "complex exponentiation".
* math domain messages follow 3.14 ("expected a positive input, got %s" etc.; the tested ones match
  test_math exactly), the rest "math domain error" / "math range error".
* `math.gamma`/`lgamma`/`erf`/`erfc` use glibc libm (accuracy within test tolerances; all 433
  math_testcases pass). `math.sumprod` float path uses an exact (FMA split + Shewchuk partials)
  accumulator instead of CPython's triple-length one (results are correctly rounded).
* `math.integer` is a separate builtin module also inserted into sys.modules when math is imported.
* cmath: special values implemented as C99 Annex G rules per function (no tables); all 2097
  cmath_testcases pass.
* _struct: 3.15 complex codes F, D, Zf, Zd supported; Struct `__new__`/`__init__` follow the 3.15
  deprecation rules from test_struct (lenient `__new__` when `__init__` is overridden).
  Non-ASCII format -> ValueError mentioning "non-ASCII". `struct.error` is created with
  `type()`; module=struct.
* binascii: implemented the 3.15 API (alphabet=, ignorechars=, strict_mode default = ignorechars
  given, padded=, canonical=, wrapcol=, base32/base85/ascii85 functions). Non-strict base64 no longer
  stops at padding (decodes across '=' as the 3.15 tests require). Messages chosen to satisfy the
  test regexes ("Leading padding not allowed", "Excess data after padding", "Non-zero padding bits
  are not allowed", ...).
* _random: MT19937 in Rust; seeding happens in `tp_init` (so Python subclasses control seeding);
  `__reduce__` raises TypeError (bpo-41052).
* binomialvariate guards random() == 0.0 by redrawing (gh-149222 tests).

## Missing / known gaps
* Lib/statistics.py not written yet.
* Tests could not be run through unittest yet (no unittest/collections/enum in the tree at the
  time); validated with ad-hoc scripts and the full math/cmath data files.
* `_struct.Buf` is a local buffer-protocol helper; switch to the shared BufView once merged.
* float/complex/int C-API functions (PyFloat_*, PyLong_*, PyComplex_*) are not part of this work.
