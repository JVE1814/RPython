//! Numeric builtins: round, bin, oct, hex, and sum (with CPython's int/float/complex fast
//! paths and compensated float summation).
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytes, bytearray, complex, float, int, int_methods, none, str as pystr};

fn b_round(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("round", &["number", "ndigits"], 0, 1);
    let [number, ndigits] = args::parse::<2>(&P, a, kw)?;
    let number = number.unwrap();
    let f = match ab::lookup_special(number, crate::name!("__round__"))? {
        Some(f) => f,
        None => raise!(TypeError, "type {} doesn't define __round__ method", tname(number)),
    };
    match ndigits {
        None => ab::call0(&f),
        Some(n) if n.is(none::none()) => ab::call0(&f),
        Some(n) => ab::call(&f, &[n]),
    }
}

/// PyNumber_ToBase
pub fn to_base(o: &PyObject, base: u32) -> PyResult {
    let i = ab::index(o)?;
    let prefix = match base {
        2 => "0b",
        8 => "0o",
        16 => "0x",
        _ => "",
    };
    let mag = int::int_methods_mag_string(&i, base);
    let s = if int::sign(&i) < 0 { format!("-{}{}", prefix, mag) } else { format!("{}{}", prefix, mag) };
    Ok(pystr::new_str(&s))
}

fn b_bin(_m: &PyObject, o: &PyObject) -> PyResult {
    to_base(o, 2)
}
fn b_oct(_m: &PyObject, o: &PyObject) -> PyResult {
    to_base(o, 8)
}
fn b_hex(_m: &PyObject, o: &PyObject) -> PyResult {
    to_base(o, 16)
}

/// Neumaier compensated accumulation.
#[derive(Clone, Copy)]
struct CompSum {
    hi: f64,
    lo: f64,
}
impl CompSum {
    #[inline]
    fn add(&mut self, x: f64) {
        let t = self.hi + x;
        if self.hi.abs() >= x.abs() {
            self.lo += (self.hi - t) + x;
        } else {
            self.lo += (x - t) + self.hi;
        }
        self.hi = t;
    }
    fn value(&self) -> f64 {
        if self.lo != 0.0 && self.lo.is_finite() { self.hi + self.lo } else { self.hi }
    }
}

fn b_sum(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("sum", &["iterable", "start"], 1, 1);
    let [it, start] = args::parse::<2>(&P, a, kw)?;
    let mut result = match start {
        Some(s) => {
            if pystr::is_str(s) {
                raise!(TypeError, "sum() can't sum strings [use ''.join(seq) instead]");
            }
            if bytes::is_bytes(s) {
                raise!(TypeError, "sum() can't sum bytes [use b''.join(seq) instead]");
            }
            if bytearray::is_bytearray(s) {
                raise!(TypeError, "sum() can't sum bytearray [use b''.join(seq) instead]");
            }
            s.to_ref()
        }
        None => int::new_int(0),
    };
    let it = ab::get_iter(it.unwrap())?;
    let add = |x: &PyObject, y: &PyObject| ab::binary_op(x, y, ab::NbSlot::Add);

    if int::is_int_exact(&result) {
        if let Some(mut acc) = int::as_i64(&result) {
            loop {
                let item = match ab::iter_next(&it)? {
                    Some(x) => x,
                    None => return Ok(int::new_int(acc)),
                };
                if int::is_int_exact(&item) || crate::types::bool_::is_bool(&item) {
                    if let Some(v) = int::as_i64(&item) {
                        if let Some(s) = acc.checked_add(v) {
                            acc = s;
                            continue;
                        }
                    }
                }
                // not a small int: fall back
                result = add(&int::new_int(acc), &item)?;
                break;
            }
        }
    }

    if float::is_float_exact(&result) {
        let mut s = CompSum { hi: float::float_value(&result), lo: 0.0 };
        loop {
            let item = match ab::iter_next(&it)? {
                Some(x) => x,
                None => return Ok(float::new_float(s.value())),
            };
            if float::is_float_exact(&item) {
                s.add(float::float_value(&item));
                continue;
            }
            if int::is_int(&item) {
                if let Some(v) = int::as_i64(&item) {
                    if v.unsigned_abs() < (1u64 << 53) {
                        s.add(v as f64);
                        continue;
                    }
                }
            }
            result = add(&float::new_float(s.value()), &item)?;
            break;
        }
    }

    if complex::is_complex_exact(&result) {
        let c = complex::complex_value(&result);
        let mut re = CompSum { hi: c.real, lo: 0.0 };
        let mut im = CompSum { hi: c.imag, lo: 0.0 };
        loop {
            let item = match ab::iter_next(&it)? {
                Some(x) => x,
                None => return Ok(complex::new_complex(re.value(), im.value())),
            };
            if complex::is_complex_exact(&item) {
                let z = complex::complex_value(&item);
                re.add(z.real);
                im.add(z.imag);
                continue;
            }
            if float::is_float_exact(&item) {
                re.add(float::float_value(&item));
                continue;
            }
            if int::is_int(&item) {
                if let Some(v) = int::as_i64(&item) {
                    if v.unsigned_abs() < (1u64 << 53) {
                        re.add(v as f64);
                        continue;
                    }
                }
            }
            result = add(&complex::new_complex(re.value(), im.value()), &item)?;
            break;
        }
    }

    while let Some(item) = ab::iter_next(&it)? {
        result = add(&result, &item)?;
    }
    let _ = int_methods::iref;
    Ok(result)
}

/// Install the numeric builtins into the builtins dict.
pub fn install(d: &PyObject, module: &PyObject) -> PyResult<()> {
    let methods = crate::mdefs![
        ("round", FASTKW, b_round, "round($module, /, number, ndigits=None)\n--\n\nRound a number to a given precision in decimal digits.\n\nThe return value is an integer if ndigits is omitted or None.  Otherwise\nthe return value has the same type as the number.  ndigits may be negative."),
        ("bin", O, b_bin, "bin($module, number, /)\n--\n\nReturn the binary representation of an integer.\n\n   >>> bin(2796202)\n   '0b1010101010101010101010'"),
        ("oct", O, b_oct, "oct($module, number, /)\n--\n\nReturn the octal representation of an integer.\n\n   >>> oct(342391)\n   '0o1234567'"),
        ("hex", O, b_hex, "hex($module, number, /)\n--\n\nReturn the hexadecimal representation of an integer.\n\n   >>> hex(12648430)\n   '0xc0ffee'"),
        ("sum", FASTKW, b_sum, "sum($module, iterable, /, start=0)\n--\n\nReturn the sum of a 'start' value (default: 0) plus an iterable of numbers\n\nWhen the iterable is empty, return the start value.\nThis function is intended specifically for use with numeric values and may\nreject non-numeric types."),
    ];
    crate::builtins::add_methods_to_dict(d, methods, Some(module))
}
