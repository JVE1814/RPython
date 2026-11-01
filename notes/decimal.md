# decimal workstream

Files: `crates/runtime/src/modules/_decimal/` (Rust `_decimal`), `Lib/decimal.py`,
`Lib/_pydecimal.py`.  One line registers the module in `modules/mod.rs`.

## Design
* Engine (no runtime dependency; `run_engine_tests.sh` builds it standalone with rustc):
  - `nat.rs`: naturals in base 10^19 (`u64` limbs).  Division of 128-bit values by 10^19 uses
    a precomputed reciprocal (no `__udivti3`), Karatsuba above 40 limbs.
  - `ntt.rs`: three-prime NTT (primes < 2^62, Montgomery arithmetic, CRT) above 700 limbs;
    Newton-reciprocal division above 400 limbs.  Prime/root validity is checked by a test.
  - `dec.rs`: `Dec` (sign, kind, exponent, `Coef`), `Ctx`, condition bits (values equal the
    `Dec*` constants of CPython's extra-functionality build), `finalize` (single rounding
    incl. subnormal/overflow/clamp rules; u128 fast path), parser, sci/eng strings.
    `Coef` keeps <= 2 limbs (< 10^38) inline: no heap allocation for typical precisions.
  - `ops.rs`: arithmetic, comparisons, total order, quantize, reduce, integral rounding,
    logb/scaleb/shift/rotate, logical ops, next_*, classification.  Additions with huge
    exponent gaps replace the negligible operand by a sticky unit (exact rounding).
  - `transc.rs`: sqrt (exact integer sqrt), exp/ln/log10/power: fixed-point evaluation
    (argument reduction by ln10 / halving, Taylor and atanh series) with an error bound and a
    Ziv acceptance test (both ends of the error interval must round identically).  Exact
    power results are detected first (integer powers computed exactly when the result has
    at most 3*prec+1000 digits; rational exponents via exact 2/5-roots), so the loop always
    terminates.  Results are correctly rounded in every rounding mode (exp/ln/log10/sqrt use
    ROUND_HALF_EVEN like the reference).  Non-integral exact powers are padded to prec+1
    digits and signal Inexact/Rounded (spec behaviour; pwsx803/805 pass for both variants).
  - `format.rs`: format mini-language (fill/align/sign/z/#/0/width/,_/.prec + fraction
    grouping / eEfFgGn% and deprecated N), locale grouping semantics.
  - `dectest.rs` (`#[cfg(test)]`): runs every `.decTest` case with the semantics of
    test_decimal's IBMTestCases: all 64,757 cases pass in < 1 s.
* Python binding: `py_dec.rs` (Decimal type, number protocol, rich compare incl. float /
  complex / numbers.Rational, numeric hash, methods, Context arithmetic methods),
  `py_ctx.rs` (signals as exception classes, Context, SignalDictMixin + `SignalDict`
  (MutableMapping subclass, built at import), ContextManager, IEEEContext, templates).
* `Lib/_pydecimal.py` is an independent pure-Python design (int coefficients, bit-mask
  status turned into flags/exceptions at the end of each operation, same algorithms as the
  Rust engine).  `__name__` is set to 'decimal' so pickles/reprs interoperate.

## Decisions
* Current context: thread-local in `_decimal` (one Context per thread, leaked at thread exit
  to avoid running decrefs after interpreter teardown); `_pydecimal` uses a ContextVar.
  `HAVE_CONTEXTVAR` is True in both.  TODO: switch `_decimal` to the runtime's ContextVar
  once there is a cheap Rust-level API (asyncio tasks currently share the thread's context).
* MAX_PREC/MAX_EMAX = 999999999999999999 (64-bit values), IEEE_CONTEXT_MAX_BITS = 512.
* No `Dec*` constants / `_flags` / `_traps` (regular, non-EXTRA_FUNCTIONALITY build).

## Status
* `python -m test -u decimal test_decimal`: 663 tests, all IBM decTest files pass for both
  the C and the Py variant; remaining 2 problems are runtime bugs outside this workstream:
  - three-argument `pow(10, Decimal(2), 7)` (Py variant): the slot wrapper for Python
    classes does not pass the modulus to `__rpow__` (3.14+ behaviour).
  - `check_disallow_instantiation(ContextManager)`: `tp_new_wrapper`'s "not safe" check
    runs before the Py_TPFLAGS_DISALLOW_INSTANTIATION check, so the message differs.
  (Running needed temporary stubs for `py_compile`/`doctest`, which are not in Lib yet.)
* Benchmarks (pyperformance, our build): decimal_pi 1.4 s, decimal_factorial 0.8 s, telco
  0.045 s.  Both baselines in /opt/baseline were built without libmpdec (they fall back to
  _pydecimal: decimal_pi 19-20 s, telco 0.34 s, decimal_factorial fails on the int-str limit).
  Profiles show the interpreter dominates decimal_pi/factorial.
