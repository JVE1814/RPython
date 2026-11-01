//! The `_bisect` accelerator.

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::{int, list, none};
use crate::{mdefs, raise};

struct Args<'a> {
    a: &'a PyObject,
    x: &'a PyObject,
    lo: isize,
    hi: isize,
    key: Option<&'a PyObject>,
}

fn parse<'a>(fname: &str, args: &'a [&'a PyObject], kw: Option<&'a PyObject>) -> PyResult<Args<'a>> {
    let p = Params::new(fname, &["a", "x", "lo", "hi", "key"], 0, 2).kwonly(1);
    let [a, x, lo, hi, key] = crate::args::parse::<5>(&p, args, kw)?;
    let lo = match lo {
        Some(l) => fc::ssize_arg(l)?,
        None => 0,
    };
    if lo < 0 {
        raise!(ValueError, "lo must be non-negative");
    }
    let a = a.unwrap();
    let hi = match hi {
        Some(h) if !h.is(none::none()) => fc::ssize_arg(h)?,
        _ => ab::length(a)? as isize,
    };
    Ok(Args { a, x: x.unwrap(), lo, hi, key: key.filter(|k| !k.is(none::none())) })
}

fn item(a: &PyObject, i: isize) -> PyResult {
    if list::is_list_exact(a) {
        if let Some(v) = list::get(a, i as usize) {
            return Ok(v);
        }
    }
    ab::getitem(a, &int::new_int(i as i64))
}

fn bisect(p: &Args, right: bool) -> PyResult<isize> {
    let (mut lo, mut hi) = (p.lo, p.hi);
    while lo < hi {
        let mid = ((lo as usize + hi as usize) / 2) as isize;
        let mut v = item(p.a, mid)?;
        if let Some(k) = p.key {
            v = ab::call(k, &[&v])?;
        }
        let go_left = if right { ab::rich_compare_bool(p.x, &v, Py_LT)? } else { !ab::rich_compare_bool(&v, p.x, Py_LT)? };
        if go_left {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo)
}

fn bisect_right(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = parse("bisect_right", args, kw)?;
    Ok(int::new_int(bisect(&p, true)? as i64))
}
fn bisect_left(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = parse("bisect_left", args, kw)?;
    Ok(int::new_int(bisect(&p, false)? as i64))
}

fn insort(fname: &str, args: &[&PyObject], kw: Option<&PyObject>, right: bool) -> PyResult {
    let p = parse(fname, args, kw)?;
    let pos = match p.key {
        None => bisect(&p, right)?,
        Some(k) => {
            let kx = ab::call(k, &[p.x])?;
            let q = Args { a: p.a, x: &kx, lo: p.lo, hi: p.hi, key: p.key };
            bisect(&q, right)?
        }
    };
    if list::is_list_exact(p.a) {
        let n = list::list_len(p.a) as isize;
        list::insert(p.a, pos.clamp(0, n) as usize, p.x.to_ref());
    } else {
        ab::call_method_str(p.a, "insert", &[&int::new_int(pos as i64), p.x])?;
    }
    Ok(fc::none())
}

fn insort_right(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    insort("insort_right", args, kw, true)
}
fn insort_left(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    insort("insort_left", args, kw, false)
}

pub fn init_module() -> PyResult {
    let methods = mdefs![
        ("bisect_right", FASTKW, bisect_right, "bisect_right($module, /, a, x, lo=0, hi=None, *, key=None)\n--\n\nReturn the index where to insert item x in list a, assuming a is sorted.\n\nThe return value i is such that all e in a[:i] have e <= x, and all e in\na[i:] have e > x.  So if x already appears in the list, a.insert(i, x) will\ninsert just after the rightmost x already there.\n\nOptional args lo (default 0) and hi (default len(a)) bound the\nslice of a to be searched.\n\nA custom key function can be supplied to customize the sort order."),
        ("insort_right", FASTKW, insort_right, "insort_right($module, /, a, x, lo=0, hi=None, *, key=None)\n--\n\nInsert item x in list a, and keep it sorted assuming a is sorted.\n\nIf x is already in a, insert it to the right of the rightmost x.\n\nOptional args lo (default 0) and hi (default len(a)) bound the\nslice of a to be searched.\n\nA custom key function can be supplied to customize the sort order."),
        ("bisect_left", FASTKW, bisect_left, "bisect_left($module, /, a, x, lo=0, hi=None, *, key=None)\n--\n\nReturn the index where to insert item x in list a, assuming a is sorted.\n\nThe return value i is such that all e in a[:i] have e < x, and all e in\na[i:] have e >= x.  So if x already appears in the list, a.insert(i, x) will\ninsert just before the leftmost x already there.\n\nOptional args lo (default 0) and hi (default len(a)) bound the\nslice of a to be searched.\n\nA custom key function can be supplied to customize the sort order."),
        ("insort_left", FASTKW, insort_left, "insort_left($module, /, a, x, lo=0, hi=None, *, key=None)\n--\n\nInsert item x in list a, and keep it sorted assuming a is sorted.\n\nIf x is already in a, insert it to the left of the leftmost x.\n\nOptional args lo (default 0) and hi (default len(a)) bound the\nslice of a to be searched.\n\nA custom key function can be supplied to customize the sort order."),
    ];
    let (m, _d) = fc::new_module("_bisect", methods, Some("Bisection algorithms.\n\nThis module provides support for maintaining a list in sorted order without\nhaving to sort the list after each insertion. For long lists of items with\nexpensive comparison operations, this can be an improvement over the more\ncommon approach.\n"))?;
    Ok(m)
}
