//! The abstract object protocols (PyObject_*, PyNumber_*, PySequence_*, PyMapping_*,
//! PyIter_*), implemented over type slots exactly as C extensions see them.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, float, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::c_int;

// ------------------------------------------------------------------ attributes

pub fn getattr(o: &PyObject, nm: &PyObject) -> PyResult {
    let tp = o.ty();
    if let Some(f) = tp.tp_getattro {
        if f as usize == object_generic_getattro as usize {
            return generic_getattr(o, nm);
        }
        let r = unsafe { from_c(f(o.as_ptr(), nm.as_ptr())) };
        if r.is_err() {
            crate::types::exceptions::fill_attribute_error_context(o, nm);
        }
        return r;
    }
    if let Some(f) = tp.tp_getattr {
        let s = pystr::as_utf8(nm)?;
        let c = std::ffi::CString::new(s).unwrap_or_default();
        return unsafe { from_c(f(o.as_ptr(), c.as_ptr() as *mut _)) };
    }
    Err(attribute_error_for(o, nm))
}

pub fn getattr_str(o: &PyObject, nm: &str) -> PyResult {
    let n = pystr::intern_str(nm);
    getattr(o, &n)
}

/// getattr that returns None (no exception) on AttributeError.
pub fn getattr_opt(o: &PyObject, nm: &PyObject) -> PyResult<Option<PyRef>> {
    let tp = o.ty();
    let r = if tp.tp_getattro.map(|f| f as usize) == Some(object_generic_getattro as usize) {
        generic_getattr_opt(o, nm, None)
    } else {
        getattr(o, nm).map(Some)
    };
    match r {
        Ok(v) => Ok(v),
        Err(e) => {
            if exc_matches(crate::types::exceptions::AttributeError()) {
                clear_exc();
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

pub fn hasattr(o: &PyObject, nm: &PyObject) -> PyResult<bool> {
    Ok(getattr_opt(o, nm)?.is_some())
}

pub fn setattr(o: &PyObject, nm: &PyObject, v: &PyObject) -> PyResult<()> {
    setattr_opt(o, nm, Some(v))
}

pub fn delattr(o: &PyObject, nm: &PyObject) -> PyResult<()> {
    setattr_opt(o, nm, None)
}

pub fn setattr_opt(o: &PyObject, nm: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    if !pystr::is_str(nm) {
        raise!(TypeError, "attribute name must be string, not '{}'", tname(nm));
    }
    let tp = o.ty();
    let vp = v.map_or(std::ptr::null_mut(), |v| v.as_ptr());
    if let Some(f) = tp.tp_setattro {
        let r = unsafe { f(o.as_ptr(), nm.as_ptr(), vp) };
        return if r < 0 { Err(PyErr) } else { Ok(()) };
    }
    if let Some(f) = tp.tp_setattr {
        let s = pystr::as_utf8(nm)?;
        let c = std::ffi::CString::new(s).unwrap_or_default();
        let r = unsafe { f(o.as_ptr(), c.as_ptr() as *mut _, vp) };
        return if r < 0 { Err(PyErr) } else { Ok(()) };
    }
    if tp.tp_getattro.is_none() && tp.tp_getattr.is_none() {
        raise!(
            TypeError,
            "'{}' object has no attributes ({} .{})",
            type_name(tp),
            if v.is_some() { "assign to" } else { "del" },
            pystr::to_string_lossy(nm)
        );
    }
    raise!(
        TypeError,
        "'{}' object has only read-only attributes ({} .{})",
        type_name(tp),
        if v.is_some() { "assign to" } else { "del" },
        pystr::to_string_lossy(nm)
    );
}

// ------------------------------------------------------------------ calls

/// Read a type's vectorcall function pointer for `callable`, if it supports vectorcall.
#[inline]
pub fn vectorcall_func(callable: &PyObject) -> vectorcallfunc {
    let tp = callable.ty();
    if tp.tp_flags & Py_TPFLAGS_HAVE_VECTORCALL == 0 || tp.tp_vectorcall_offset <= 0 {
        return None;
    }
    unsafe { *((callable.as_ptr() as *mut u8).offset(tp.tp_vectorcall_offset) as *const vectorcallfunc) }
}

/// Vectorcall with raw arguments (C convention).
pub unsafe fn vectorcall_raw(
    callable: &PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        // (perf4) Python functions directly (no indirect call, no result check: the
        // function call path always sets an exception on failure)
        if crate::types::funcobject::is_function(callable)
            && crate::types::funcobject::func(callable).vectorcall.map(|p| p as usize) == Some(crate::interp::function_vectorcall as usize)
        {
            let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
            let kwn = if kwnames.is_null() { None } else { Some(&*kwnames) };
            let nkw = kwn.map_or(0, |k| tuple::tuple_len(k));
            let all: &[*mut PyObject] = if nargs + nkw == 0 { &[] } else { std::slice::from_raw_parts(args, nargs + nkw) };
            return into_c(crate::interp::call_function(callable, all, nargs, kwn));
        }
        if let Some(f) = vectorcall_func(callable) {
            return check_result(callable, f(callable.as_ptr(), args, nargsf, kwnames));
        }
        // fall back to tp_call
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let all = if nargs + nkw == 0 { &[][..] } else { std::slice::from_raw_parts(args, nargs + nkw) };
        let t = tuple::new_tuple_from_ptrs(&all[..nargs]);
        let kw = if nkw > 0 {
            let d = dict::new_dict();
            for i in 0..nkw {
                if dict::set_item(&d, tuple::tuple_get(&*kwnames, i), &*all[nargs + i]).is_err() {
                    return std::ptr::null_mut();
                }
            }
            Some(d)
        } else {
            None
        };
        into_c(call_tp_call(callable, &t, kw.as_deref()))
    }
}

#[inline]
fn check_result(callable: &PyObject, r: *mut PyObject) -> *mut PyObject {
    if r.is_null() {
        if !err_occurred() {
            let _ = err!(SystemError, "{} returned NULL without setting an exception", repr_for_error(callable));
        }
    } else if cfg!(debug_assertions) && err_occurred() {
        unsafe { decref(r) };
        let cause = take_exc();
        let _ = err!(SystemError, "{} returned a result with an exception set", repr_for_error(callable));
        if let (Some(c), Some(e)) = (cause, take_exc()) {
            crate::types::exceptions::set_exc_cause(&e, Some(&c));
            crate::types::exceptions::set_context_and_cause(&e, Some(&c));
            set_exc(e);
        }
        return std::ptr::null_mut();
    }
    r
}

fn repr_for_error(o: &PyObject) -> String {
    if crate::types::cfunction::is_cfunction(o) {
        format!("<built-in function {}>", crate::types::cfunction::cfunction_name(o))
    } else {
        format!("<{} object>", tname(o))
    }
}

pub fn call_tp_call(callable: &PyObject, args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    let f = match callable.ty().tp_call {
        Some(f) => f,
        None => {
            // `import mod; mod()` where `mod.mod` is the callable that was meant
            if crate::types::module::is_module(callable) {
                let d = crate::types::module::module_dict(callable);
                if let Some(n) = crate::types::dict::get_item_rs(&d, "__name__").filter(|n| crate::types::str::is_str(n)) {
                    if let Some(f) = crate::types::dict::get_item(&d, &n).ok().flatten() {
                        if is_callable(&f) {
                            let n = crate::types::str::to_string_lossy(&n);
                            raise!(TypeError, "'{}' object is not callable. Did you mean: '{}.{}(...)'?", tname(callable), n, n);
                        }
                    }
                }
                crate::errors::clear_exc();
            }
            raise!(TypeError, "'{}' object is not callable", tname(callable))
        }
    };
    let ts = crate::threads::current();
    if crate::interp::enter_recursive_call(ts, " while calling a Python object").is_err() {
        return Err(PyErr);
    }
    let r = unsafe { f(callable.as_ptr(), args.as_ptr(), kw.map_or(std::ptr::null_mut(), |k| k.as_ptr())) };
    crate::interp::leave_recursive_call(ts);
    unsafe { from_c(check_result(callable, r)) }
}

/// tp_call implementation for vectorcall-capable types (PyVectorcall_Call).
pub unsafe extern "C" fn vectorcall_call(callable: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = &*callable;
        // like PyVectorcall_Call: the slot at tp_vectorcall_offset, whether or not the type
        // carries the vectorcall flag (heap subclasses inherit only the offset)
        let off = c.ty().tp_vectorcall_offset;
        let f = if off > 0 { *((callable as *mut u8).offset(off) as *const vectorcallfunc) } else { None };
        let f = match f {
            Some(f) => f,
            None => {
                return into_c(Err(err!(TypeError, "'{}' object does not support vectorcall", tname(c))));
            }
        };
        let items = tuple::tuple_items(&*args);
        if kw.is_null() || dict::dict_len(&*kw) == 0 {
            return f(callable, items.as_ptr() as *const *mut PyObject, items.len(), std::ptr::null_mut());
        }
        let mut v: Vec<*mut PyObject> = items.iter().map(|x| x.as_ptr()).collect();
        let mut names = Vec::new();
        let kvs = dict::items_vec(&*kw);
        for (k, val) in kvs.iter() {
            names.push(k.clone());
            v.push(val.as_ptr());
        }
        let kwnames = tuple::new_tuple(names);
        f(callable, v.as_ptr(), items.len(), kwnames.as_ptr())
    }
}

/// Call with positional args.
pub fn call(callable: &PyObject, args: &[&PyObject]) -> PyResult {
    unsafe { from_c(vectorcall_raw(callable, args.as_ptr() as *const *mut PyObject, args.len(), std::ptr::null_mut())) }
}

pub fn call0(callable: &PyObject) -> PyResult {
    call(callable, &[])
}

/// Call with positional args and a vectorcall kwnames tuple (values appended to args).
pub fn call_kw(callable: &PyObject, args_and_kwvalues: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let nkw = kwnames.map_or(0, |k| tuple::tuple_len(k));
    unsafe {
        from_c(vectorcall_raw(
            callable,
            args_and_kwvalues.as_ptr() as *const *mut PyObject,
            args_and_kwvalues.len() - nkw,
            kwnames.map_or(std::ptr::null_mut(), |k| k.as_ptr()),
        ))
    }
}

/// Call with positional args and an optional kwargs dict.
pub fn call_with_kwargs_dict(callable: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    match kw {
        None => call(callable, args),
        Some(k) if dict::dict_len(k) == 0 => call(callable, args),
        Some(k) => {
            let mut v: Vec<&PyObject> = args.to_vec();
            let items = dict::items_vec(k);
            let mut names = Vec::with_capacity(items.len());
            for (key, _) in items.iter() {
                if !pystr::is_str(key) {
                    // non-str keys: only tp_call (which receives the dict itself) can
                    // decide; Python functions reject them there
                    if callable.ty().tp_call.is_some() && callable.ty().tp_call != Some(vectorcall_call) {
                        let t = tuple::new_tuple(args.iter().map(|a| a.to_ref()).collect());
                        return call_tp_call(callable, &t, Some(k));
                    }
                    raise!(TypeError, "keywords must be strings");
                }
                names.push(key.clone());
            }
            for (_, val) in items.iter() {
                v.push(val);
            }
            let kwnames = tuple::new_tuple(names);
            call_kw(callable, &v, Some(&kwnames))
        }
    }
}

/// CALL_FUNCTION_EX with the positional arguments already in an exact tuple
/// (perf3): callables without vectorcall get that tuple as is (no vector, no second
/// tuple); others get a stack vector.
pub fn call_ex_tuple(callable: &PyObject, self_or_null: Option<&PyObject>, args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    let kw = kw.filter(|k| dict::dict_len(k) > 0);
    // (perf4) Python functions (also bound to an instance) bind from the tuple and the
    // dict directly
    let plain_func = |c: &PyObject| {
        crate::types::funcobject::is_function(c)
            && crate::types::funcobject::func(c).vectorcall.map(|p| p as usize) == Some(crate::interp::function_vectorcall as usize)
    };
    if plain_func(callable) {
        return crate::interp::call_function_ex(callable, self_or_null, args, kw);
    }
    if self_or_null.is_none() && crate::types::funcobject::is_method(callable) {
        let (mf, ms) = crate::types::funcobject::method_parts(callable);
        if plain_func(mf) {
            return crate::interp::call_function_ex(mf, Some(ms), args, kw);
        }
    }
    let items = tuple::tuple_items(args);
    if self_or_null.is_none() && vectorcall_func(callable).is_none() && callable.ty().tp_call.is_some() {
        return call_tp_call(callable, args, kw);
    }
    let n = items.len() + self_or_null.is_some() as usize;
    if n <= 8 {
        let mut buf: [&PyObject; 8] = [callable; 8];
        let mut i = 0;
        if let Some(s) = self_or_null {
            buf[0] = s;
            i = 1;
        }
        buf[i..n].copy_from_slice(items);
        return call_with_kwargs_dict(callable, &buf[..n], kw);
    }
    let mut v: Vec<&PyObject> = Vec::with_capacity(n);
    if let Some(s) = self_or_null {
        v.push(s);
    }
    v.extend_from_slice(items);
    call_with_kwargs_dict(callable, &v, kw)
}

/// Call `f` (found on the type) with `self_` prepended, avoiding a bound method
/// for plain functions and method descriptors.
pub fn call_unbound(f: &PyObject, self_: &PyObject, args: &[&PyObject]) -> PyResult {
    let ft = f.ty();
    if ft.tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
        // (jit5) slot dunders call Python functions with a few arguments: no heap
        // vector, and plain functions go straight to the function call path
        if args.len() < 4 {
            let mut buf: [*mut PyObject; 4] = [self_.as_ptr(); 4];
            for (i, a) in args.iter().enumerate() {
                buf[i + 1] = a.as_ptr();
            }
            let n = args.len() + 1;
            if f.is_type(crate::types::funcobject::function_type())
                && crate::types::funcobject::func(f).vectorcall.map(|p| p as usize) == Some(crate::interp::function_vectorcall as usize)
            {
                return crate::interp::call_function(f, &buf[..n], n, None);
            }
            return unsafe { from_c(vectorcall_raw(f, buf.as_ptr(), n, std::ptr::null_mut())) };
        }
        let mut v: Vec<&PyObject> = Vec::with_capacity(args.len() + 1);
        v.push(self_);
        v.extend_from_slice(args);
        return call(f, &v);
    }
    match ft.tp_descr_get {
        Some(g) => {
            let bound = unsafe { from_c(g(f.as_ptr(), self_.as_ptr(), self_.type_ptr() as *mut PyObject))? };
            call(&bound, args)
        }
        None => call(f, args),
    }
}

/// `o.name(*args)`
pub fn call_method(o: &PyObject, nm: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = getattr(o, nm)?;
    call(&m, args)
}

pub fn call_method_str(o: &PyObject, nm: &str, args: &[&PyObject]) -> PyResult {
    let n = pystr::intern_str(nm);
    call_method(o, &n, args)
}

/// Look up a special method on the type and call it (`_PyObject_LookupSpecial` + call).
pub fn call_special_opt(o: &PyObject, nm: &PyObject, args: &[&PyObject]) -> PyResult<Option<PyRef>> {
    match lookup(o.type_ptr(), nm) {
        None => Ok(None),
        Some(f) => call_unbound(&f, o, args).map(Some),
    }
}

pub fn lookup_special(o: &PyObject, nm: &PyObject) -> PyResult<Option<PyRef>> {
    match lookup(o.type_ptr(), nm) {
        None => Ok(None),
        Some(f) => match f.ty().tp_descr_get {
            Some(g) => unsafe { from_c(g(f.as_ptr(), o.as_ptr(), o.type_ptr() as *mut PyObject)).map(Some) },
            None => Ok(Some(f)),
        },
    }
}

pub fn is_callable(o: &PyObject) -> bool {
    o.ty().tp_call.is_some()
}

// ------------------------------------------------------------------ repr/str/hash/bool

pub fn repr(o: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " while getting the repr of an object")?;
    let r = match o.ty().tp_repr {
        Some(f) => unsafe { from_c(f(o.as_ptr())) },
        None => Ok(pystr::new_str(&format!("<{} object at {:p}>", type_fqname(o.type_ptr()), o.as_ptr()))),
    };
    crate::interp::leave_recursive_call(ts);
    let r = r?;
    if !pystr::is_str(&r) {
        raise!(TypeError, "__repr__ returned non-string (type {})", tname(&r));
    }
    Ok(r)
}

pub fn str(o: &PyObject) -> PyResult {
    if pystr::is_str_exact(o) {
        return Ok(o.to_ref());
    }
    let f = match o.ty().tp_str {
        Some(f) => f,
        None => return repr(o),
    };
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " while getting the str of an object")?;
    let r = unsafe { from_c(f(o.as_ptr())) };
    crate::interp::leave_recursive_call(ts);
    let r = r?;
    if !pystr::is_str(&r) {
        raise!(TypeError, "__str__ returned non-string (type {})", tname(&r));
    }
    Ok(r)
}

pub fn repr_string(o: &PyObject) -> PyResult<String> {
    Ok(pystr::to_string_lossy(&*repr(o)?))
}
pub fn str_string(o: &PyObject) -> PyResult<String> {
    Ok(pystr::to_string_lossy(&*str(o)?))
}

pub fn ascii(o: &PyObject) -> PyResult {
    let r = repr(o)?;
    if pystr::str_is_ascii(&r) {
        return Ok(r);
    }
    let mut out = String::new();
    for c in pystr::str_data(&r).iter() {
        if c < 0x80 {
            out.push(c as u8 as char);
        } else if c <= 0xff {
            out.push_str(&format!("\\x{:02x}", c));
        } else if c <= 0xffff {
            out.push_str(&format!("\\u{:04x}", c));
        } else {
            out.push_str(&format!("\\U{:08x}", c));
        }
    }
    Ok(pystr::new_str(&out))
}

#[inline]
pub fn hash(o: &PyObject) -> PyResult<isize> {
    if pystr::is_str_exact(o) {
        return Ok(pystr::str_hash(o));
    }
    match o.ty().tp_hash {
        Some(f) => {
            let h = unsafe { f(o.as_ptr()) };
            if h == -1 && err_occurred() { Err(PyErr) } else { Ok(h) }
        }
        None => raise!(TypeError, "unhashable type: '{}'", tname(o)),
    }
}

#[inline]
pub fn is_true(o: &PyObject) -> PyResult<bool> {
    if o.is(bool_::true_obj()) {
        return Ok(true);
    }
    if o.is(bool_::false_obj()) || o.is(none::none()) {
        return Ok(false);
    }
    let tp = o.ty();
    unsafe {
        if !tp.tp_as_number.is_null() {
            if let Some(f) = (*tp.tp_as_number).nb_bool {
                let r = f(o.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r != 0) };
            }
        }
        if !tp.tp_as_mapping.is_null() {
            if let Some(f) = (*tp.tp_as_mapping).mp_length {
                let r = f(o.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r != 0) };
            }
        }
        if !tp.tp_as_sequence.is_null() {
            if let Some(f) = (*tp.tp_as_sequence).sq_length {
                let r = f(o.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r != 0) };
            }
        }
    }
    Ok(true)
}

// ------------------------------------------------------------------ comparison

const SWAPPED: [c_int; 6] = [Py_GT, Py_GE, Py_EQ, Py_NE, Py_LT, Py_LE];
const OPSTR: [&str; 6] = ["<", "<=", "==", "!=", ">", ">="];

pub fn rich_compare(a: &PyObject, b: &PyObject, op: c_int) -> PyResult {
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " in comparison")?;
    let r = do_richcompare(a, b, op);
    crate::interp::leave_recursive_call(ts);
    r
}

fn do_richcompare(a: &PyObject, b: &PyObject, op: c_int) -> PyResult {
    let at = a.type_ptr();
    let bt = b.type_ptr();
    let mut checked_reverse = false;
    unsafe {
        if at != bt && is_subtype(bt, at) {
            if let Some(f) = (*bt).tp_richcompare {
                checked_reverse = true;
                let r = from_c(f(b.as_ptr(), a.as_ptr(), SWAPPED[op as usize]))?;
                if !r.is(none::not_implemented()) {
                    return Ok(r);
                }
            }
        }
        if let Some(f) = (*at).tp_richcompare {
            let r = from_c(f(a.as_ptr(), b.as_ptr(), op))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
        if !checked_reverse {
            if let Some(f) = (*bt).tp_richcompare {
                let r = from_c(f(b.as_ptr(), a.as_ptr(), SWAPPED[op as usize]))?;
                if !r.is(none::not_implemented()) {
                    return Ok(r);
                }
            }
        }
    }
    match op {
        Py_EQ => Ok(bool_::new_bool(a.is(b))),
        Py_NE => Ok(bool_::new_bool(!a.is(b))),
        _ => raise!(
            TypeError,
            "'{}' not supported between instances of '{}' and '{}'",
            OPSTR[op as usize],
            crate::types::typeobject::tp_name_of(a),
            crate::types::typeobject::tp_name_of(b)
        ),
    }
}

/// Comparison returning a bool; identity implies equality (PyObject_RichCompareBool).
pub fn rich_compare_bool(a: &PyObject, b: &PyObject, op: c_int) -> PyResult<bool> {
    if a.is(b) {
        if op == Py_EQ {
            return Ok(true);
        }
        if op == Py_NE {
            return Ok(false);
        }
    }
    let r = rich_compare(a, b, op)?;
    if r.is(bool_::true_obj()) {
        return Ok(true);
    }
    if r.is(bool_::false_obj()) {
        return Ok(false);
    }
    is_true(&r)
}

/// Equality of two objects when it can be decided without running Python code
/// (perf3): identical objects, exact str/int/float pairs, and exact tuples of such
/// (compared item by item, nested tuples to a bounded depth). None when the answer
/// needs an `__eq__` call (or the nesting is deeper than the bound).
pub fn plain_eq(a: &PyObject, b: &PyObject) -> Option<bool> {
    plain_eq_depth(a, b, 0)
}

fn plain_eq_depth(a: &PyObject, b: &PyObject, depth: u32) -> Option<bool> {
    if a.is(b) {
        return Some(true);
    }
    let ta = a.type_ptr();
    if ta != b.type_ptr() {
        return None;
    }
    let ta = ta as *mut crate::ffi::PyTypeObject;
    if ta == pystr::str_type() {
        return Some(pystr::str_eq(a, b));
    }
    if ta == int::int_type() {
        return Some(int::sign(a) == int::sign(b) && int::digits(a) == int::digits(b));
    }
    if ta == crate::types::float::float_type() {
        return Some(crate::types::float::float_value(a) == crate::types::float::float_value(b));
    }
    // tuples and tuple subclasses that keep tuple equality (namedtuples)
    if depth < 16 && (ta == tuple::tuple_type() || unsafe { (*ta).tp_richcompare == (*tuple::tuple_type()).tp_richcompare && (*ta).tp_flags & crate::ffi::Py_TPFLAGS_TUPLE_SUBCLASS != 0 }) {
        let (x, y) = (tuple::tuple_items(a), tuple::tuple_items(b));
        if x.len() != y.len() {
            return Some(false);
        }
        for (p, q) in x.iter().zip(y.iter()) {
            if !plain_eq_depth(p, q, depth + 1)? {
                return Some(false);
            }
        }
        return Some(true);
    }
    None
}

#[inline]
pub fn rich_eq(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    if a.is(b) {
        return Ok(true);
    }
    if let Some(r) = plain_eq(a, b) {
        return Ok(r);
    }
    if pystr::is_str_exact(a) && pystr::is_str_exact(b) {
        return Ok(pystr::str_eq(a, b));
    }
    if int::is_int_exact(a) && int::is_int_exact(b) && int::is_compact(a) && int::is_compact(b) {
        return Ok(int::compact_value(a) == int::compact_value(b));
    }
    rich_compare_bool(a, b, Py_EQ)
}

// ------------------------------------------------------------------ numbers

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NbSlot {
    Add,
    Sub,
    Mul,
    Mod,
    Divmod,
    Pow,
    Lshift,
    Rshift,
    And,
    Xor,
    Or,
    FloorDiv,
    TrueDiv,
    MatMul,
}

impl NbSlot {
    fn get(self, nb: *mut PyNumberMethods) -> binaryfunc {
        if nb.is_null() {
            return None;
        }
        unsafe {
            let n = &*nb;
            match self {
                NbSlot::Add => n.nb_add,
                NbSlot::Sub => n.nb_subtract,
                NbSlot::Mul => n.nb_multiply,
                NbSlot::Mod => n.nb_remainder,
                NbSlot::Divmod => n.nb_divmod,
                NbSlot::Pow => None,
                NbSlot::Lshift => n.nb_lshift,
                NbSlot::Rshift => n.nb_rshift,
                NbSlot::And => n.nb_and,
                NbSlot::Xor => n.nb_xor,
                NbSlot::Or => n.nb_or,
                NbSlot::FloorDiv => n.nb_floor_divide,
                NbSlot::TrueDiv => n.nb_true_divide,
                NbSlot::MatMul => n.nb_matrix_multiply,
            }
        }
    }
    fn get_inplace(self, nb: *mut PyNumberMethods) -> binaryfunc {
        if nb.is_null() {
            return None;
        }
        unsafe {
            let n = &*nb;
            match self {
                NbSlot::Add => n.nb_inplace_add,
                NbSlot::Sub => n.nb_inplace_subtract,
                NbSlot::Mul => n.nb_inplace_multiply,
                NbSlot::Mod => n.nb_inplace_remainder,
                NbSlot::Divmod => None,
                NbSlot::Pow => None,
                NbSlot::Lshift => n.nb_inplace_lshift,
                NbSlot::Rshift => n.nb_inplace_rshift,
                NbSlot::And => n.nb_inplace_and,
                NbSlot::Xor => n.nb_inplace_xor,
                NbSlot::Or => n.nb_inplace_or,
                NbSlot::FloorDiv => n.nb_inplace_floor_divide,
                NbSlot::TrueDiv => n.nb_inplace_true_divide,
                NbSlot::MatMul => n.nb_inplace_matrix_multiply,
            }
        }
    }
    pub fn symbol(self) -> &'static str {
        match self {
            NbSlot::Add => "+",
            NbSlot::Sub => "-",
            NbSlot::Mul => "*",
            NbSlot::Mod => "%",
            NbSlot::Divmod => "divmod()",
            NbSlot::Pow => "** or pow()",
            NbSlot::Lshift => "<<",
            NbSlot::Rshift => ">>",
            NbSlot::And => "&",
            NbSlot::Xor => "^",
            NbSlot::Or => "|",
            NbSlot::FloorDiv => "//",
            NbSlot::TrueDiv => "/",
            NbSlot::MatMul => "@",
        }
    }
}

/// binary_op1: try a.slot(a, b) and b.slot(a, b) with subclass priority.
fn binary_op1(a: &PyObject, b: &PyObject, s: NbSlot) -> PyResult {
    let at = a.ty();
    let bt = b.ty();
    let slota = s.get(at.tp_as_number);
    let mut slotb = if !std::ptr::eq(at, bt) { s.get(bt.tp_as_number) } else { None };
    if slotb.map(|f| f as usize) == slota.map(|f| f as usize) {
        slotb = None;
    }
    unsafe {
        if let Some(fa) = slota {
            if let Some(fb) = slotb {
                if is_subtype(b.type_ptr(), a.type_ptr()) {
                    let r = from_c(fb(a.as_ptr(), b.as_ptr()))?;
                    if !r.is(none::not_implemented()) {
                        return Ok(r);
                    }
                    slotb = None;
                }
            }
            let r = from_c(fa(a.as_ptr(), b.as_ptr()))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
        if let Some(fb) = slotb {
            let r = from_c(fb(a.as_ptr(), b.as_ptr()))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
    }
    Ok(none::not_implemented().to_ref())
}

fn binop_type_error(a: &PyObject, b: &PyObject, s: &str) -> PyErr {
    err!(TypeError, "unsupported operand type(s) for {}: '{}' and '{}'", s, tname(a), tname(b))
}

pub fn binary_op(a: &PyObject, b: &PyObject, s: NbSlot) -> PyResult {
    if s == NbSlot::Pow {
        return power(a, b, none::none());
    }
    let r = binary_op1(a, b, s)?;
    if !r.is(none::not_implemented()) {
        return Ok(r);
    }
    // sequence fallbacks
    unsafe {
        if s == NbSlot::Add {
            let sq = a.ty().tp_as_sequence;
            if !sq.is_null() {
                if let Some(f) = (*sq).sq_concat {
                    return from_c(f(a.as_ptr(), b.as_ptr()));
                }
            }
        }
        if s == NbSlot::Mul {
            if let Some(r) = sequence_repeat_fallback(a, b)? {
                return Ok(r);
            }
        }
    }
    if s == NbSlot::Rshift && crate::types::cfunction::is_cfunction(a) && crate::types::cfunction::cfunction_name(a) == "print" {
        raise!(
            TypeError,
            "unsupported operand type(s) for >>: '{}' and '{}'. Did you mean \"print(<message>, file=<output_stream>)\"?",
            tname(a),
            tname(b)
        );
    }
    Err(binop_type_error(a, b, s.symbol()))
}

fn sequence_repeat_fallback(a: &PyObject, b: &PyObject) -> PyResult<Option<PyRef>> {
    unsafe {
        let sqa = a.ty().tp_as_sequence;
        if !sqa.is_null() {
            if let Some(f) = (*sqa).sq_repeat {
                return sequence_repeat(f, a, b).map(Some);
            }
        }
        let sqb = b.ty().tp_as_sequence;
        if !sqb.is_null() {
            if let Some(f) = (*sqb).sq_repeat {
                return sequence_repeat(f, b, a).map(Some);
            }
        }
    }
    Ok(None)
}

fn sequence_repeat(f: unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut PyObject, seq: &PyObject, n: &PyObject) -> PyResult {
    if !has_index(n) {
        raise!(TypeError, "can't multiply sequence by non-int of type '{}'", tname(n));
    }
    let count = index_as_ssize(n, Some(crate::types::exceptions::OverflowError()))?;
    unsafe { from_c(f(seq.as_ptr(), count)) }
}

pub fn inplace_op(a: &PyObject, b: &PyObject, s: NbSlot) -> PyResult {
    if s == NbSlot::Pow {
        return inplace_power(a, b);
    }
    unsafe {
        if let Some(f) = s.get_inplace(a.ty().tp_as_number) {
            let r = from_c(f(a.as_ptr(), b.as_ptr()))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
    }
    let r = binary_op1(a, b, s)?;
    if !r.is(none::not_implemented()) {
        return Ok(r);
    }
    unsafe {
        let sq = a.ty().tp_as_sequence;
        if s == NbSlot::Add && !sq.is_null() {
            if let Some(f) = (*sq).sq_inplace_concat {
                return from_c(f(a.as_ptr(), b.as_ptr()));
            }
            if let Some(f) = (*sq).sq_concat {
                return from_c(f(a.as_ptr(), b.as_ptr()));
            }
        }
        if s == NbSlot::Mul {
            if !sq.is_null() {
                if let Some(f) = (*sq).sq_inplace_repeat {
                    return sequence_repeat(f, a, b);
                }
            }
            if let Some(r) = sequence_repeat_fallback(a, b)? {
                return Ok(r);
            }
        }
    }
    let sym = format!("{}=", s.symbol());
    Err(binop_type_error(a, b, &sym))
}

pub fn power(a: &PyObject, b: &PyObject, m: &PyObject) -> PyResult {
    let at = a.ty();
    let bt = b.ty();
    let get = |t: &PyTypeObject| -> ternaryfunc {
        if t.tp_as_number.is_null() { None } else { unsafe { (*t.tp_as_number).nb_power } }
    };
    let slota = get(at);
    let mut slotb = if !std::ptr::eq(at, bt) { get(bt) } else { None };
    if slotb.map(|f| f as usize) == slota.map(|f| f as usize) {
        slotb = None;
    }
    unsafe {
        if let Some(fa) = slota {
            if let Some(fb) = slotb {
                if is_subtype(b.type_ptr(), a.type_ptr()) {
                    let r = from_c(fb(a.as_ptr(), b.as_ptr(), m.as_ptr()))?;
                    if !r.is(none::not_implemented()) {
                        return Ok(r);
                    }
                    slotb = None;
                }
            }
            let r = from_c(fa(a.as_ptr(), b.as_ptr(), m.as_ptr()))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
        if let Some(fb) = slotb {
            let r = from_c(fb(a.as_ptr(), b.as_ptr(), m.as_ptr()))?;
            if !r.is(none::not_implemented()) {
                return Ok(r);
            }
        }
        if !m.is(none::none()) {
            if let Some(fm) = get(m.ty()) {
                if Some(fm as usize) != slota.map(|f| f as usize) && Some(fm as usize) != slotb.map(|f| f as usize) {
                    let r = from_c(fm(a.as_ptr(), b.as_ptr(), m.as_ptr()))?;
                    if !r.is(none::not_implemented()) {
                        return Ok(r);
                    }
                }
            }
        }
    }
    if m.is(none::none()) {
        raise!(TypeError, "unsupported operand type(s) for ** or pow(): '{}' and '{}'", tname(a), tname(b));
    }
    raise!(
        TypeError,
        "unsupported operand type(s) for ** or pow(): '{}', '{}', '{}'",
        tname(a),
        tname(b),
        tname(m)
    );
}

fn inplace_power(a: &PyObject, b: &PyObject) -> PyResult {
    unsafe {
        let nb = a.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_inplace_power {
                let r = from_c(f(a.as_ptr(), b.as_ptr(), none::none().as_ptr()))?;
                if !r.is(none::not_implemented()) {
                    return Ok(r);
                }
            }
        }
    }
    match power(a, b, none::none()) {
        Ok(r) => Ok(r),
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                let msg = crate::types::exceptions::exc_args(&crate::errors::take_exc().unwrap());
                let _ = msg;
                raise!(TypeError, "unsupported operand type(s) for **=: '{}' and '{}'", tname(a), tname(b));
            }
            Err(e)
        }
    }
}

macro_rules! unary {
    ($fname:ident, $slot:ident, $sym:expr) => {
        pub fn $fname(a: &PyObject) -> PyResult {
            unsafe {
                let nb = a.ty().tp_as_number;
                if !nb.is_null() {
                    if let Some(f) = (*nb).$slot {
                        return from_c(f(a.as_ptr()));
                    }
                }
            }
            raise!(TypeError, "bad operand type for {}: '{}'", $sym, tname(a));
        }
    };
}
unary!(negative, nb_negative, "unary -");
unary!(positive, nb_positive, "unary +");
unary!(invert, nb_invert, "unary ~");

pub fn absolute(a: &PyObject) -> PyResult {
    unsafe {
        let nb = a.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_absolute {
                return from_c(f(a.as_ptr()));
            }
        }
    }
    raise!(TypeError, "bad operand type for abs(): '{}'", tname(a));
}

pub fn has_index(o: &PyObject) -> bool {
    if int::is_int(o) {
        return true;
    }
    let nb = o.ty().tp_as_number;
    !nb.is_null() && unsafe { (*nb).nb_index.is_some() }
}

/// operator.index(o) -> exact-or-subclass int.
pub fn index(o: &PyObject) -> PyResult {
    if int::is_int(o) {
        return Ok(o.to_ref());
    }
    unsafe {
        let nb = o.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_index {
                let r = from_c(f(o.as_ptr()))?;
                if int::is_int_exact(&r) {
                    return Ok(r);
                }
                if !int::is_int(&r) {
                    raise!(TypeError, "__index__ returned non-int (type {})", tname(&r));
                }
                crate::modules::warnings::warn(
                    crate::types::exceptions::DeprecationWarning(),
                    &format!(
                        "__index__ returned non-int (type {}).  The ability to return an instance of a strict subclass of int is deprecated, and may be removed in a future version of Python.",
                        tname(&r)
                    ),
                    1,
                )?;
                return Ok(r);
            }
        }
    }
    raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
}

/// Convert via __index__ to isize. If `overflow_exc` is None, clamp on overflow.
pub fn index_as_ssize(o: &PyObject, overflow_exc: Option<*mut PyTypeObject>) -> PyResult<isize> {
    if int::is_int(o) && int::is_compact(o) {
        return Ok(int::compact_value(o) as isize);
    }
    let i = index(o)?;
    match int::as_i64(&i) {
        Some(v) => Ok(v as isize),
        None => match overflow_exc {
            Some(e) => Err(new_err(e, &format!("cannot fit '{}' into an index-sized integer", tname(o)))),
            None => Ok(if int::sign(&i) < 0 { isize::MIN } else { isize::MAX }),
        },
    }
}

pub fn as_i128_index(o: &PyObject) -> PyResult<i128> {
    let i = index(o)?;
    match int::as_i128(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

/// i64 from an int-like via __index__ with OverflowError.
pub fn as_i64(o: &PyObject) -> PyResult<i64> {
    let i = index(o)?;
    match int::as_i64(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

/// float(o) value (PyFloat_AsDouble semantics, __float__ then __index__).
pub fn float_value(o: &PyObject) -> PyResult<f64> {
    if float::is_float(o) {
        return Ok(float::float_value(o));
    }
    unsafe {
        let nb = o.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_float {
                let r = from_c(f(o.as_ptr()))?;
                if !float::is_float(&r) {
                    raise!(TypeError, "{}.__float__ returned non-float (type {})", tname(o), tname(&r));
                }
                if !float::is_float_exact(&r) {
                    crate::modules::warnings::warn(
                        crate::types::exceptions::DeprecationWarning(),
                        &format!(
                            "{}.__float__ returned non-float (type {}).  The ability to return an instance of a strict subclass of float is deprecated, and may be removed in a future version of Python.",
                            tname(o),
                            tname(&r)
                        ),
                        1,
                    )?;
                }
                return Ok(float::float_value(&r));
            }
            if let Some(_) = (*nb).nb_index {
                let i = index(o)?;
                return int::to_f64(&i);
            }
        }
    }
    if int::is_int(o) {
        return int::to_f64(o);
    }
    raise!(TypeError, "must be real number, not {}", tname(o));
}

// ------------------------------------------------------------------ sequences / mappings

pub fn length(o: &PyObject) -> PyResult<usize> {
    let tp = o.ty();
    unsafe {
        if !tp.tp_as_sequence.is_null() {
            if let Some(f) = (*tp.tp_as_sequence).sq_length {
                let r = f(o.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r as usize) };
            }
        }
        if !tp.tp_as_mapping.is_null() {
            if let Some(f) = (*tp.tp_as_mapping).mp_length {
                let r = f(o.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r as usize) };
            }
        }
    }
    raise!(TypeError, "object of type '{}' has no len()", tname(o));
}

pub fn object_len_or_zero(o: &PyObject) -> usize {
    if dict::is_dict(o) {
        return dict::dict_len(o);
    }
    match length(o) {
        Ok(n) => n,
        Err(_) => {
            clear_exc();
            0
        }
    }
}

/// Validate a __len__ result.
pub fn len_result_to_ssize(r: &PyObject) -> PyResult<isize> {
    if !has_index(r) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(r));
    }
    let i = index(r)?;
    if int::sign(&i) < 0 {
        raise!(ValueError, "__len__() should return >= 0");
    }
    match int::as_i64(&i) {
        Some(v) => Ok(v as isize),
        None => raise!(OverflowError, "cannot fit 'int' into an index-sized integer"),
    }
}

pub fn getitem(o: &PyObject, key: &PyObject) -> PyResult {
    let tp = o.ty();
    unsafe {
        if !tp.tp_as_mapping.is_null() {
            if let Some(f) = (*tp.tp_as_mapping).mp_subscript {
                return from_c(f(o.as_ptr(), key.as_ptr()));
            }
        }
        if !tp.tp_as_sequence.is_null() {
            if let Some(f) = (*tp.tp_as_sequence).sq_item {
                if has_index(key) {
                    let mut i = index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
                    if i < 0 {
                        if let Some(l) = (*tp.tp_as_sequence).sq_length {
                            let n = l(o.as_ptr());
                            if n < 0 {
                                return Err(PyErr);
                            }
                            i += n;
                        }
                    }
                    return from_c(f(o.as_ptr(), i));
                }
                raise!(TypeError, "sequence index must be integer, not '{}'", tname(key));
            }
        }
    }
    if is_type(o) {
        if o.is(crate::types::typeobject::type_obj(crate::types::typeobject::type_type())) {
            return crate::types::genericalias::new_generic_alias(o, key);
        }
        // __class_getitem__
        if let Some(m) = getattr_opt(o, name!("__class_getitem__"))? {
            return call(&m, &[key]);
        }
        raise!(TypeError, "type '{}' is not subscriptable", type_name(unsafe { &*as_type(o) }));
    }
    raise!(TypeError, "'{}' object is not subscriptable", tname(o));
}

pub fn setitem(o: &PyObject, key: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let tp = o.ty();
    let vp = v.map_or(std::ptr::null_mut(), |v| v.as_ptr());
    unsafe {
        if !tp.tp_as_mapping.is_null() {
            if let Some(f) = (*tp.tp_as_mapping).mp_ass_subscript {
                return if f(o.as_ptr(), key.as_ptr(), vp) < 0 { Err(PyErr) } else { Ok(()) };
            }
        }
        if !tp.tp_as_sequence.is_null() {
            if let Some(f) = (*tp.tp_as_sequence).sq_ass_item {
                if has_index(key) {
                    let mut i = index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
                    if i < 0 {
                        if let Some(l) = (*tp.tp_as_sequence).sq_length {
                            let n = l(o.as_ptr());
                            if n < 0 {
                                return Err(PyErr);
                            }
                            i += n;
                        }
                    }
                    return if f(o.as_ptr(), i, vp) < 0 { Err(PyErr) } else { Ok(()) };
                }
                raise!(TypeError, "sequence index must be integer, not '{}'", tname(key));
            }
        }
    }
    if v.is_some() {
        raise!(TypeError, "'{}' object does not support item assignment", tname(o));
    }
    raise!(TypeError, "'{}' object does not support item deletion", tname(o));
}

pub fn contains(container: &PyObject, item: &PyObject) -> PyResult<bool> {
    unsafe {
        let sq = container.ty().tp_as_sequence;
        if !sq.is_null() {
            if let Some(f) = (*sq).sq_contains {
                let r = f(container.as_ptr(), item.as_ptr());
                return if r < 0 { Err(PyErr) } else { Ok(r != 0) };
            }
        }
    }
    iter_search_contains(container, item)
}

pub fn iter_search_contains(container: &PyObject, item: &PyObject) -> PyResult<bool> {
    let it = match get_iter(container) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "argument of type '{}' is not a container or iterable", tname(container));
            }
            return Err(e);
        }
    };
    while let Some(x) = iter_next(&it)? {
        if rich_eq(&x, item)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ------------------------------------------------------------------ iteration

pub fn get_iter(o: &PyObject) -> PyResult {
    let tp = o.ty();
    if let Some(f) = tp.tp_iter {
        let r = unsafe { from_c(f(o.as_ptr()))? };
        if r.ty().tp_iternext.is_none() {
            raise!(TypeError, "iter() returned non-iterator of type '{}'", tname(&r));
        }
        return Ok(r);
    }
    unsafe {
        if !tp.tp_as_sequence.is_null() && (*tp.tp_as_sequence).sq_item.is_some() && !dict::is_dict(o) {
            return Ok(crate::types::iterobject::new_seq_iter(o.to_ref()));
        }
    }
    raise!(TypeError, "'{}' object is not iterable", tname(o));
}

/// next(it): Ok(None) when exhausted (StopIteration cleared).
#[inline]
pub fn iter_next(it: &PyObject) -> PyResult<Option<PyRef>> {
    let f = match it.ty().tp_iternext {
        Some(f) => f,
        None => raise!(TypeError, "'{}' object is not an iterator", tname(it)),
    };
    let r = unsafe { f(it.as_ptr()) };
    if !r.is_null() {
        return Ok(Some(unsafe { PyRef::from_raw(r) }));
    }
    if err_occurred() {
        if exc_matches(crate::types::exceptions::StopIteration()) {
            clear_exc();
            return Ok(None);
        }
        return Err(PyErr);
    }
    Ok(None)
}

pub fn collect_iterable(o: &PyObject) -> PyResult<Vec<PyRef>> {
    if tuple::is_tuple_exact(o) {
        return Ok(tuple::tuple_to_vec(o));
    }
    if list::is_list_exact(o) {
        return Ok(list::list_to_vec(o));
    }
    if crate::types::set::is_anyset_exact(o) {
        return Ok(crate::types::set::set_items(o));
    }
    let it = get_iter(o)?;
    let mut v = Vec::new();
    if let Ok(n) = length_hint(o, 0) {
        v.reserve(n.min(1 << 20));
    }
    while let Some(x) = iter_next(&it)? {
        v.push(x);
    }
    Ok(v)
}

pub fn length_hint(o: &PyObject, default: usize) -> PyResult<usize> {
    // only types with a length slot are asked for len() (perf2: building and
    // discarding a TypeError per generator was expensive)
    let has_len = unsafe {
        let t = o.ty();
        (!t.tp_as_sequence.is_null() && (*t.tp_as_sequence).sq_length.is_some())
            || (!t.tp_as_mapping.is_null() && (*t.tp_as_mapping).mp_length.is_some())
    };
    if has_len {
        if let Ok(n) = length(o) {
            return Ok(n);
        }
        if !exc_matches(crate::types::exceptions::TypeError()) {
            return Err(PyErr);
        }
        clear_exc();
    }
    match call_special_opt(o, name!("__length_hint__"), &[])? {
        None => Ok(default),
        Some(r) => {
            if r.is(none::not_implemented()) {
                return Ok(default);
            }
            if !int::is_int(&r) {
                raise!(TypeError, "__length_hint__ must be an integer, not {}", tname(&r));
            }
            let v = as_i64(&r)?;
            if v < 0 {
                raise!(ValueError, "__length_hint__() should return >= 0");
            }
            Ok(v as usize)
        }
    }
}

pub fn sequence_tuple(o: &PyObject) -> PyResult {
    if tuple::is_tuple_exact(o) {
        return Ok(o.to_ref());
    }
    if list::is_list_exact(o) {
        return Ok(list::list_to_tuple(o));
    }
    Ok(tuple::new_tuple(collect_iterable(o)?))
}

pub fn sequence_list(o: &PyObject) -> PyResult {
    Ok(list::new_list(collect_iterable(o)?))
}

// ------------------------------------------------------------------ isinstance / issubclass

fn abstract_get_bases(cls: &PyObject) -> PyResult<Option<PyRef>> {
    match getattr_opt(cls, name!("__bases__"))? {
        Some(b) if tuple::is_tuple(&b) => Ok(Some(b)),
        _ => Ok(None),
    }
}

fn abstract_issubclass(derived: &PyObject, cls: &PyObject) -> PyResult<bool> {
    let mut d = derived.to_ref();
    loop {
        if d.is(cls) {
            return Ok(true);
        }
        let bases = match abstract_get_bases(&d)? {
            Some(b) => b,
            None => return Ok(false),
        };
        let n = tuple::tuple_len(&bases);
        if n == 0 {
            return Ok(false);
        }
        if n == 1 {
            d = tuple::tuple_get(&bases, 0).to_ref();
            continue;
        }
        let ts = crate::threads::current();
        crate::interp::enter_recursive_call(ts, " in __subclasscheck__")?;
        let mut found = Ok(false);
        for b in tuple::tuple_items(&bases) {
            match abstract_issubclass(b, cls) {
                Ok(true) => {
                    found = Ok(true);
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    found = Err(e);
                    break;
                }
            }
        }
        crate::interp::leave_recursive_call(ts);
        return found;
    }
}

fn recursive_isinstance(inst: &PyObject, cls: &PyObject) -> PyResult<bool> {
    if is_type(cls) {
        if isinstance_exact_or_sub(inst, as_type(cls)) {
            return Ok(true);
        }
        if let Some(c) = getattr_opt(inst, name!("__class__"))? {
            if !c.is(type_obj(inst.type_ptr())) && is_type(&c) {
                return Ok(is_subtype(as_type(&c), as_type(cls)));
            }
        }
        return Ok(false);
    }
    if abstract_get_bases(cls)?.is_none() {
        raise!(TypeError, "isinstance() arg 2 must be a type, a tuple of types, or a union");
    }
    match getattr_opt(inst, name!("__class__"))? {
        Some(c) => abstract_issubclass(&c, cls),
        None => Ok(false),
    }
}

pub fn isinstance(inst: &PyObject, cls: &PyObject) -> PyResult<bool> {
    if inst.type_ptr() as *mut PyObject == cls.as_ptr() {
        return Ok(true);
    }
    if is_type(cls) && cls.is_type(type_type()) {
        return recursive_isinstance(inst, cls);
    }
    if tuple::is_tuple(cls) {
        // nested tuples recurse: guard the native stack
        let ts = crate::threads::current();
        crate::interp::enter_recursive_call(ts, " in __instancecheck__")?;
        let mut r = Ok(false);
        for c in tuple::tuple_items(cls) {
            match isinstance(inst, c) {
                Ok(false) => {}
                other => {
                    r = other;
                    break;
                }
            }
        }
        crate::interp::leave_recursive_call(ts);
        return r;
    }
    if let Some(chk) = lookup_special(cls, name!("__instancecheck__"))? {
        let ts = crate::threads::current();
        crate::interp::enter_recursive_call(ts, " in __instancecheck__")?;
        let r = call(&chk, &[inst]);
        crate::interp::leave_recursive_call(ts);
        return is_true(&*r?);
    }
    recursive_isinstance(inst, cls)
}

fn recursive_issubclass(derived: &PyObject, cls: &PyObject) -> PyResult<bool> {
    if is_type(cls) && is_type(derived) {
        return Ok(is_subtype(as_type(derived), as_type(cls)));
    }
    if abstract_get_bases(derived)?.is_none() {
        raise!(TypeError, "issubclass() arg 1 must be a class");
    }
    if abstract_get_bases(cls)?.is_none() {
        raise!(TypeError, "issubclass() arg 2 must be a class, a tuple of classes, or a union");
    }
    abstract_issubclass(derived, cls)
}

pub fn issubclass(derived: &PyObject, cls: &PyObject) -> PyResult<bool> {
    if is_type(cls) && cls.is_type(type_type()) {
        if derived.is(cls) {
            return Ok(true);
        }
        return recursive_issubclass(derived, cls);
    }
    if tuple::is_tuple(cls) {
        // nested tuples recurse: guard the native stack
        let ts = crate::threads::current();
        crate::interp::enter_recursive_call(ts, " in __instancecheck__")?;
        let mut r = Ok(false);
        for c in tuple::tuple_items(cls) {
            match issubclass(derived, c) {
                Ok(false) => {}
                other => {
                    r = other;
                    break;
                }
            }
        }
        crate::interp::leave_recursive_call(ts);
        return r;
    }
    if let Some(chk) = lookup_special(cls, name!("__subclasscheck__"))? {
        let ts = crate::threads::current();
        crate::interp::enter_recursive_call(ts, " in __subclasscheck__")?;
        let r = call(&chk, &[derived]);
        crate::interp::leave_recursive_call(ts);
        return is_true(&*r?);
    }
    recursive_issubclass(derived, cls)
}

// ------------------------------------------------------------------ misc

pub fn format(o: &PyObject, spec: &PyObject) -> PyResult {
    if pystr::is_str_exact(o) && pystr::str_len(spec) == 0 {
        return Ok(o.to_ref());
    }
    if int::is_int_exact(o) && pystr::str_len(spec) == 0 {
        return str(o);
    }
    let meth = match lookup_special(o, name!("__format__"))? {
        Some(m) => m,
        None => raise!(TypeError, "Type {} doesn't define __format__", tname(o)),
    };
    let r = call(&meth, &[spec])?;
    if !pystr::is_str(&r) {
        raise!(TypeError, "__format__ must return a str, not {}", tname(&r));
    }
    Ok(r)
}

/// dir() helper: sorted list of attribute names.
pub fn dir(o: &PyObject) -> PyResult {
    let r = match lookup_special(o, name!("__dir__"))? {
        Some(m) => call(&m, &[])?,
        None => raise!(TypeError, "object does not provide __dir__"),
    };
    let l = sequence_list(&r)?;
    crate::types::list_methods::sort_list(&l, None, false)?;
    Ok(l)
}
