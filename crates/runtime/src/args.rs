//! Argument parsing for Rust builtins using the FASTKW convention:
//! `args` holds all positional values followed by keyword values; `kwnames`
//! is the tuple naming the trailing keyword values.

use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
use crate::types::{str as pystr, tuple};

pub struct Params<'a> {
    /// Function name used in error messages (e.g. "sorted", "str.split").
    pub fname: &'a str,
    /// Parameter names in order (positional-or-keyword after `posonly`, kw-only last).
    pub names: &'a [&'a str],
    /// Number of leading positional-only parameters.
    pub posonly: usize,
    /// Number of leading parameters that are required.
    pub required: usize,
    /// Number of trailing keyword-only parameters.
    pub kwonly: usize,
    /// Required keyword-only names (subset of the kw-only names).
    pub kwonly_required: &'a [&'a str],
}

impl<'a> Params<'a> {
    pub const fn new(fname: &'a str, names: &'a [&'a str], posonly: usize, required: usize) -> Self {
        Params { fname, names, posonly, required, kwonly: 0, kwonly_required: &[] }
    }
    pub const fn kwonly(mut self, n: usize) -> Self {
        self.kwonly = n;
        self
    }
}

/// Split vectorcall-style args into (positional, keyword pairs).
pub fn split<'b>(args: &[&'b PyObject], kwnames: Option<&'b PyObject>) -> (&'b [&'b PyObject], Vec<(&'b PyObject, &'b PyObject)>)
where
    'b: 'b,
{
    let nkw = kwnames.map_or(0, |k| tuple::tuple_len(k));
    let npos = args.len() - nkw;
    let pos: &[&PyObject] = unsafe { std::mem::transmute::<&[&PyObject], &'b [&'b PyObject]>(&args[..npos]) };
    let mut kws = Vec::with_capacity(nkw);
    if let Some(k) = kwnames {
        for i in 0..nkw {
            kws.push((tuple::tuple_get(k, i), args[npos + i]));
        }
    }
    let kws = unsafe { std::mem::transmute::<Vec<(&PyObject, &PyObject)>, Vec<(&'b PyObject, &'b PyObject)>>(kws) };
    (pos, kws)
}

pub fn parse<'b, const N: usize>(p: &Params, args: &[&'b PyObject], kwnames: Option<&'b PyObject>) -> PyResult<[Option<&'b PyObject>; N]> {
    debug_assert_eq!(N, p.names.len());
    let mut out: [Option<&'b PyObject>; N] = [None; N];
    // perf3: positional part and keyword names straight from the vector (no Vec,
    // no String per keyword)
    let nkw = kwnames.map_or(0, |k| tuple::tuple_len(k));
    let pos: &[&PyObject] = &args[..args.len() - nkw];
    let max_pos = N - p.kwonly;
    if pos.len() > max_pos {
        if p.kwonly > 0 || p.required != max_pos {
            if max_pos == 0 {
                raise!(TypeError, "{}() takes no positional arguments", p.fname);
            }
            raise!(
                TypeError,
                "{}() takes at most {} positional argument{} ({} given)",
                p.fname,
                max_pos,
                if max_pos == 1 { "" } else { "s" },
                pos.len()
            );
        }
        raise!(
            TypeError,
            "{}() takes exactly {} argument{} ({} given)",
            p.fname,
            max_pos,
            if max_pos == 1 { "" } else { "s" },
            pos.len()
        );
    }
    for (i, a) in pos.iter().enumerate() {
        out[i] = Some(*a);
    }
    for j in 0..nkw {
        let k = tuple::tuple_get(kwnames.unwrap(), j);
        let v: &'b PyObject = args[pos.len() + j];
        let owned;
        let ks: &str = match pystr::ascii_str(k) {
            Some(s) => s,
            None => {
                owned = pystr::to_string_lossy(k);
                &owned
            }
        };
        match p.names.iter().position(|n| *n == ks) {
            Some(i) if i >= p.posonly => {
                if out[i].is_some() {
                    raise!(TypeError, "argument for {}() given by name ('{}') and position ({})", p.fname, ks, i + 1);
                }
                out[i] = Some(v);
            }
            _ => {
                raise!(TypeError, "{}() got an unexpected keyword argument '{}'", p.fname, ks);
            }
        }
    }
    for i in 0..p.required.min(N) {
        if out[i].is_none() {
            if i < p.posonly {
                raise!(
                    TypeError,
                    "{}() takes {} {} argument{} ({} given)",
                    p.fname,
                    if p.required == max_pos { "exactly" } else { "at least" },
                    p.required,
                    if p.required == 1 { "" } else { "s" },
                    pos.len()
                );
            }
            raise!(TypeError, "{}() missing required argument '{}' (pos {})", p.fname, p.names[i], i + 1);
        }
    }
    for name in p.kwonly_required {
        let i = p.names.iter().position(|n| n == name).unwrap();
        if out[i].is_none() {
            raise!(TypeError, "{}() missing required keyword-only argument '{}'", p.fname, name);
        }
    }
    Ok(out)
}

/// For functions taking only positional arguments: check count range.
pub fn check_pos(fname: &str, args: &[&PyObject], min: usize, max: usize) -> PyResult<()> {
    let n = args.len();
    if n < min || n > max {
        if min == max {
            if min == 0 {
                raise!(TypeError, "{}() takes no arguments ({} given)", fname, n);
            }
            raise!(TypeError, "{} expected {} argument{}, got {}", fname, min, if min == 1 { "" } else { "s" }, n);
        }
        if n < min {
            raise!(TypeError, "{} expected at least {} argument{}, got {}", fname, min, if min == 1 { "" } else { "s" }, n);
        }
        raise!(TypeError, "{} expected at most {} argument{}, got {}", fname, max, if max == 1 { "" } else { "s" }, n);
    }
    Ok(())
}

pub fn no_kwargs(fname: &str, kwnames: Option<&PyObject>) -> PyResult<()> {
    if let Some(k) = kwnames {
        if tuple::tuple_len(k) > 0 {
            raise!(TypeError, "{}() takes no keyword arguments", fname);
        }
    }
    Ok(())
}

/// Convert (args tuple, kwargs dict or NULL) to vectorcall form (values, kwnames).
pub fn kwdict_to_vectorcall(args: &PyObject, kw: *mut PyObject) -> (Vec<crate::object::PyRef>, Option<crate::object::PyRef>) {
    let mut v: Vec<crate::object::PyRef> = tuple::tuple_to_vec(args);
    if kw.is_null() {
        return (v, None);
    }
    let kw = unsafe { &*kw };
    let items = crate::types::dict::items_vec(kw);
    if items.is_empty() {
        return (v, None);
    }
    let mut names = Vec::with_capacity(items.len());
    for (k, val) in items {
        names.push(k);
        v.push(val);
    }
    (v, Some(tuple::new_tuple(names)))
}
