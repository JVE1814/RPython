# rpy-bigint — notes and API

Arbitrary-precision integers with exact Python `int` semantics, written from scratch (no
dependencies). Magnitudes are little-endian `u32` digits holding 30 bits each (base 2^30), i.e.
exactly the `ob_digit` array of CPython's free-threaded `PyLongObject`; the sign is separate.

## Representation

* `IntRef<'a> { neg: bool, mag: &'a [u32] }` — borrowed view (e.g. directly over object
  storage). `mag` must be normalized (no high zero digits); zero is `mag == []`, `neg == false`.
  `IntRef::new(neg, mag)` (debug-asserts normalization), `IntRef::new_trim` trims.
* `BigInt { neg: bool, mag: Vec<u32> }` — owned value, same invariant. `as_ref()` gives an
  `IntRef`. Implements `Add/Sub/Mul/BitAnd/BitOr/BitXor/Neg/Not/Shl<u64>/Shr<u64>` on
  references and values, `Div/Rem` with Python floor semantics (panic on zero), `Ord`,
  `Display` (decimal), `FromStr` (Python `int(s)` rules), `From<i64/u64/i128/u128/i32>`.
* Constants `SHIFT = 30`, `BASE = 2^30`, `MASK`.

## Errors

`IntError` (with `.kind() -> ExcKind {ZeroDivisionError, OverflowError, ValueError,
MemoryError}` and `.message()` giving CPython's text):
`DivisionByZero` ("integer division or modulo by zero"), `ModuloByZero` ("integer modulo by
zero"), `TrueDivisionByZero` ("division by zero"), `TrueDivOverflow` ("integer division result
too large for a float"), `FloatOverflow` ("int too large to convert to float"),
`NegativeShiftCount`, `TooManyDigits` ("too many digits in integer"), `MemoryError`,
`PowModulusZero` ("pow() 3rd argument cannot be 0"), `NotInvertible` ("base is not invertible
for the given modulus"), `NegativeExponent` (no modulus: caller computes the float power),
`FloatInfinity`, `FloatNaN`, `IntTooBig` ("int too big to convert"), `NegativeToUnsigned`
("can't convert negative int to unsigned"), `Overflow` (fixed-size conversion; caller picks the
message), `InvalidLiteral { base }` (caller formats "invalid literal for int() with base %d: %R"
with the original object), `InvalidBase`, `StrToIntDigitLimit { digits, limit }`,
`IntToStrDigitLimit { limit }`, `IsqrtNegative`, `FactorialNegative`, `FactorialTooLarge`,
`NNegative`, `KNegative`, `PermKTooLarge`, `CombKTooLarge`, `MathDomain`.

Note: CPython's ZeroDivisionError texts differ per operation; the runtime may want to choose
its own message per op (the variants tell which op failed).

## Signed operations — `ops` (all Python semantics)

```rust
fn cmp(a: IntRef, b: IntRef) -> Ordering
fn add(a, b) -> BigInt;  fn sub(a, b) -> BigInt;  fn mul(a, b) -> BigInt
fn divmod(a, b) -> Result<(BigInt, BigInt), IntError>     // floor division + modulo
fn floordiv(a, b) -> Result<BigInt, IntError>
fn modulo(a, b) -> Result<BigInt, IntError>               // sign of b
fn divrem_trunc(a, b) -> Result<(BigInt, BigInt), IntError> // C-style truncation
fn truediv(a, b) -> Result<f64, IntError>                 // correctly rounded, -0.0 kept
fn pow(a, e: IntRef) -> Result<BigInt, IntError>          // e < 0 -> NegativeExponent
fn pow_u64(a, e: u64) -> Result<BigInt, IntError>
fn pow_mod(a, e, m) -> Result<BigInt, IntError>           // e < 0 => modular inverse
fn mod_inverse(a: &[u32], m: &[u32]) -> Option<Vec<u32>>
fn lshift(a, n: u64) -> Result<BigInt, IntError>;  fn lshift_big(a, n: IntRef) -> Result<..>
fn rshift(a, n: u64) -> BigInt;                    fn rshift_big(a, n: IntRef) -> Result<..>
fn and(a, b) / or / xor -> BigInt   (infinite two's complement);  fn invert(a) -> BigInt
fn neg(a) / abs(a) -> BigInt
fn round_neg_ndigits(a, n: u64) -> BigInt      // round(a, -n), ties to even
fn ldexp_exact(x: f64, e: i64) -> f64;  fn pow2f(e) -> f64
// allocation-free variants writing into caller storage (e.g. a fresh PyLongObject):
fn add_into(out, a, b) -> (neg, len)   // out.len() >= max(len)+1
fn sub_into(out, a, b) -> (neg, len)   // out.len() >= max(len)+1
fn mul_into(out, a, b) -> (neg, len)   // out.len() >= a.len()+b.len()
fn lshift_into(out, a, n) -> (neg, len) // out.len() >= a.len()+n/30+1
fn rshift_into(out, a, n) -> (neg, len) // out.len() >= a.len()-n/30+1 (>= 1)
fn divmod_into(q_out, r_out, a, b) -> Result<(qneg, qlen, rneg, rlen), IntError>
                                        // q_out >= a.len()-b.len()+2, r_out >= b.len()
const MAX_DIGITS; fn alloc_digits(n) -> Result<Vec<u32>, IntError> // MemoryError, no abort
```

`BigInt` has the same operations as methods (`add`, `floordiv`, `divmod`, `truediv`, `pow`,
`pow_u64`, `pow_mod`, `shl`, `shr`, `and`, `or`, `xor`, `invert`, `neg`, `abs`,
`bit_length`, `bit_count`, `hash`, `isqrt`, `gcd`, `to_f64`, `from_f64`, `to_i64`, ...,
`parse(bytes, base)`, `from_str_radix`, `to_str_radix`).

## i64 fast paths — `small`

`add/sub/mul(a, b) -> Option<i64>`, `mul_wide -> i128`, `floordiv(a, b) -> Option<i64>`
(None only for `MIN // -1`), `modulo(a, b) -> i64`, `divmod`, `truediv(a, b) -> f64`
(correctly rounded), `pow(a, e: u64) -> Option<i64>`, `pow_mod(a, e, m) -> i64`,
`lshift(a, n) -> Option<i64>`, `rshift(a, n) -> i64`, `bit_length`, `bit_count`, `hash`,
`gcd`, `is_single_digit`, `compact_value(neg, mag)`, `two_digit_value(neg, mag)`.
Divisors must be checked for zero by the caller. Also `conv::i64_to_digits(v, &mut [u32;3])`.

## Conversions — `conv`

`to_i64/to_u64/to_i128/to_u128/to_i32(IntRef) -> Option<_>`, `to_u64_wrapping` (mask
semantics), `BigInt::from_i64/u64/i128/u128`, `from_f64(f64) -> Result<BigInt>` (truncates;
`FloatInfinity`/`FloatNaN`), `to_f64(IntRef) -> Result<f64>` (round-half-even,
`FloatOverflow`), `frexp(IntRef) -> (f64, u64)` (mantissa in [0.5,1), never overflows),
`ln(IntRef) -> Result<f64>` (for math.log of huge ints), `cmp_f64(IntRef, f64) ->
Option<Ordering>` (exact int/float comparison, None for NaN), `hash(IntRef) -> i64`,
`hash_i64`, `HASH_MODULUS`, `to_bytes(a, length, little, signed) -> Result<Vec<u8>>`,
`from_bytes(&[u8], little, signed) -> BigInt`, `byte_length(a, signed) -> usize`.

## Strings — `radix`

* `to_string(a, radix) -> String` — radix 2..=36, lowercase, `-` sign, no prefix.
  `to_string_prefixed(a, radix, "0x")` puts the prefix after the sign.
  Power-of-two radices are linear; others use divide-and-conquer with cached powers
  (Barrett division with Newton reciprocals for the big levels) — subquadratic.
* `to_decimal_string_limited(a, max_str_digits) -> Result<String>` — CPython's
  `sys.int_max_str_digits` check (fast rejection before converting; 0 = unlimited).
* `from_digits(&[u8], radix) -> Option<Vec<u32>>` — pure digit string (caller stripped
  sign/underscores/prefix). Subquadratic (divide-and-conquer with powers) for long inputs.
* `parse_int(s: &[u8], base, max_str_digits) -> Result<BigInt, IntError>` — full Python
  `int(s, base)`: ASCII whitespace strip, sign, `0x/0o/0b` prefixes (base 0 or matching
  base), single underscores between digits and after a prefix, base-0 leading-zero rule,
  digit limit for non-power-of-two bases. The runtime must first map Unicode decimal digits to
  ASCII and Unicode whitespace to b' ' (CPython's `_PyUnicode_TransformDecimalAndSpaceToASCII`).
* `digit_value(c)`, `chunk_power(radix)`.

## Math — `math`

`gcd(a, b)`, `gcd_mag`, `gcd_u64` (Lehmer with 62-bit leading parts, binary gcd for u64),
`lcm`, `isqrt` / `isqrt_mag` / `isqrt_u64` (Newton-style adaptive precision),
`factorial(n)` / `factorial_mag(u64)` (odd-part binary splitting + shift),
`comb(n, k)`, `perm(n, k)` (range products by binary splitting; big n with small k supported).

## Magnitude layer — `nat`, `mul`, `div`, `modpow`

`nat`: `trim`, `normalize`, `cmp`, `bit_length`, `bit_count`, `trailing_zeros`, `test_bit`,
`add`, `sub`, `add_into`, `sub_into`, `add_to/sub_to/add_assign/sub_assign` (kernels),
`mul_u32`, `mul_digit_to/add/sub`, `divrem_u32`, `rem_digit`, `shl/shr/shl_into`,
`mul`, `sqr`, `divrem`, `extract_bits`, `pow2`, `from_u64/to_u64`, ...
`mul`: `mul`, `sqr`, `mul_into`; `mul_schoolbook/mul_recursive/sqr_recursive` (testing);
`set_thresholds/thresholds` (tuning hooks).
`div`: `divrem` (dispatch), `divrem_knuth`, `divrem_bz` (Burnikel–Ziegler),
`divrem_large` (Newton+Barrett), `Divisor` (precomputed reciprocal for repeated division),
`recip` (exact floor(B^2n/d)), `recip_approx`.
`modpow::modpow(base, e, m)` (magnitudes).
`set_simd_limit(0|1|2)` limits SIMD (portable / AVX2 / AVX-512) — results identical.

## Algorithms

* Linear kernels (add, sub, mul by digit, submul, shifts) compute every digit independently
  (carry into digit i derived from digit i-1 only) and fall back to a sequential ripple pass
  only when a digit overflows; they auto-vectorize (AVX2/AVX-512 variants selected at runtime).
* Schoolbook multiplication/squaring accumulate products lazily in u64 (15 rows between carry
  passes) — vectorized multiply-add. Karatsuba >= 96 digits, Toom-3 >= 300, NTT >= 1200
  (squaring 1500); unbalanced operands are chunked.
* NTT: three primes < 2^31 (2013265921, 1811939329, 469762049), 32-bit Montgomery, one digit
  per coefficient, transform up to 2^26 points (bigger products fall back to Toom-3 splits);
  Garner CRT.
* Division: single digit; small quotient (estimate from top 64 bits + one fused SIMD
  multiply-subtract pass); Knuth D (SIMD submul) below 250 digits; Burnikel–Ziegler above.
  Newton reciprocal (from below, no exact fix-up needed) + Barrett for cached divisors.
* Modular pow: sliding window; Montgomery with SIMD lazy-carry REDC for odd moduli, Barrett /
  dispatch division for even.
* to_string: D&C over powers 10^(9*2^k) with cached Barrett reciprocals for powers used >= 6
  times; quadratic base case below 120 digits. from_str: D&C over 10^(9*2^k).

## Performance (this machine: Xeon 2.1 GHz, AVX-512; noisy shared host)

| operation | time |
|---|---|
| pidigits(10000), benchmarks-game algorithm | ~1.1–1.2 s |
| pidigits(2000) | ~30 ms |
| mul 1e6 × 1e6 decimal digits | ~24 ms |
| str() of a 1e6-digit int | ~240–300 ms |
| int() of a 1e6-digit str | ~100 ms |
| divmod 2e6 / 1e6 digits | ~135 ms |
| factorial(100000) | ~23 ms |
| str/int of 10M digits | ~3.8 s / ~1.6 s |
| mul 1000×1000 digits (~9000 dec) | ~110–190 µs |
| add / mul_u32 of 8000 digits | ~1.7 µs / ~3.4 µs |
| pow(a, e, m) 2048-bit | ~8 ms |

Benchmark binaries: `cargo run --release -p rpy-bigint --example bench [medium|pidigits|huge|tenmillion]`,
`--example pidprof` (per-op breakdown), `--example multune` / `--example divtune` (tuning).

## Tests

`cargo test -p rpy-bigint`: randomized cross-checks against i128 (`tests/small_values.rs`),
algorithm cross-checks and identities for large values (`tests/large.rs`), kernel/ripple and
division/reciprocal checks (`tests/kernels.rs`), every SIMD level vs scalar references
(`tests/isa.rs`), cases ported from test_long / test_int / test_math_integer / test_pow
(`tests/spec_cases.rs`).

## Gaps / notes

* NTT products larger than 2^26 digits (~600M decimal digits) are split with Toom-3 (slower).
* Unbalanced NTT products re-transform the short operand per chunk.
* `to_string` could use wrap-around (mod B^N-1) products in Barrett to save ~25%.
* ZeroDivisionError message per operation is the runtime's choice (see above).
* `parse_int` expects ASCII input (runtime transforms Unicode digits/spaces first).
