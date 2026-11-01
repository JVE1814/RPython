//! object.__reduce_ex__ / object.__reduce__ / object.__getstate__ support
//! (the C half of the copyreg protocol), owned by the "data" workstream.
//!
//! * `object_getstate(o, required)` is `object.__getstate__` (default state:
//!   instance `__dict__` copy and/or `(dict, slots)` pair).
//! * `reduce_ex(o, proto)` is the shared `object.__reduce_ex__` fallback:
//!   protocol >= 2 uses `copyreg.__newobj__` / `__newobj_ex__`, protocols 0
//!   and 1 delegate to `copyreg._reduce_ex`.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::{name, raise};
use crate::types::typeobject::*;
use crate::types::{dict, list, none, tuple};

fn copyreg() -> PyResult {
    crate::import::import_absolute("copyreg")
}

/// `_PyType_GetSlotNames`: `cls.__dict__['__slotnames__']` or `copyreg._slotnames(cls)`.
fn slot_names(tp: *mut PyTypeObject) -> PyResult {
    let d = type_dict(tp);
    if let Some(v) = dict::get_item(d, name!("__slotnames__"))? {
        if !list::is_list(&v) && !v.is(none::none()) {
            raise!(
                TypeError,
                "{}.__slotnames__ should be a list or None, not {}",
                type_name(unsafe { &*tp }),
                tname(&v)
            );
        }
        return Ok(v);
    }
    let cr = copyreg()?;
    let f = ab::getattr_str(&cr, "_slotnames")?;
    let r = ab::call(&f, &[type_obj(tp)])?;
    if !list::is_list(&r) && !r.is(none::none()) {
        raise!(TypeError, "copyreg._slotnames didn't return a list or None");
    }
    Ok(r)
}

fn instance_dict_is_empty(o: &PyObject) -> bool {
    match instance_dict_existing(o) {
        Some(d) => dict::dict_len(&d) == 0,
        None => true,
    }
}

fn object_getstate_default(o: &PyObject, required: bool) -> PyResult {
    let tp = o.type_ptr();
    if required && unsafe { (*tp).tp_itemsize } != 0 {
        raise!(TypeError, "cannot pickle '{}' object", type_name(unsafe { &*tp }));
    }
    let mut state: PyRef = if instance_dict_is_empty(o) {
        none::none_ref()
    } else {
        match instance_dict_existing(o) {
            Some(d) => dict::copy(&d)?,
            None => none::none_ref(),
        }
    };
    let names = slot_names(tp)?;
    if required {
        let mut basicsize = unsafe { (*object_type()).tp_basicsize } as usize;
        let ptr = std::mem::size_of::<*mut PyObject>();
        unsafe {
            if (*tp).tp_dictoffset != 0 && (*tp).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 {
                basicsize += ptr;
            }
            if (*tp).tp_weaklistoffset > 0 {
                basicsize += ptr;
            }
        }
        if list::is_list(&names) {
            basicsize += ptr * list::list_len(&names);
        }
        if unsafe { (*tp).tp_basicsize } as usize > basicsize {
            raise!(TypeError, "cannot pickle '{}' object", type_name(unsafe { &*tp }));
        }
    }
    if list::is_list(&names) && list::list_len(&names) > 0 {
        let slots = dict::new_dict();
        let mut i = 0;
        // The list may be mutated by __getattr__ (issue 24097): re-read each time.
        while i < list::list_len(&names) {
            let nm = match list::get(&names, i) {
                Some(n) => n,
                None => break,
            };
            match ab::getattr_opt(o, &nm)? {
                Some(v) => dict::set_item(&slots, &nm, &v)?,
                None => {}
            }
            i += 1;
        }
        if dict::dict_len(&slots) > 0 {
            state = tuple::tuple2(state, slots);
        }
    }
    Ok(state)
}

/// Is `f` the bound builtin `object.__getstate__` of `o`?
fn is_default_getstate(f: &PyObject, o: &PyObject) -> bool {
    if !crate::types::cfunction::is_cfunction(f) {
        return false;
    }
    let cf = unsafe { &*(f.as_ptr() as *const PyCFunctionObject) };
    if cf.m_self != o.as_ptr() {
        return false;
    }
    match dict::get_item_rs(type_dict(object_type()), "__getstate__") {
        Some(d) => {
            if !d.is_type(&raw mut crate::types::descr::PyMethodDescr_Type) {
                return false;
            }
            let md = unsafe { &*(d.as_ptr() as *const PyMethodDescrObject) };
            md.d_method == cf.m_ml
        }
        None => false,
    }
}

/// `_PyObject_GetState`: call `obj.__getstate__()`, with the default fast path.
pub fn get_state(o: &PyObject, required: bool) -> PyResult {
    let f = ab::getattr(o, name!("__getstate__"))?;
    if is_default_getstate(&f, o) {
        object_getstate_default(o, required)
    } else {
        ab::call(&f, &[])
    }
}

/// `object.__getstate__()`.
pub fn object_getstate(o: &PyObject, required: bool) -> PyResult {
    object_getstate_default(o, required)
}

/// `(args, kwargs)` from `__getnewargs_ex__` / `__getnewargs__` (special lookup).
fn get_new_arguments(o: &PyObject) -> PyResult<(Option<PyRef>, Option<PyRef>)> {
    if let Some(f) = ab::lookup_special(o, name!("__getnewargs_ex__"))? {
        let r = ab::call(&f, &[])?;
        if !tuple::is_tuple(&r) {
            raise!(TypeError, "__getnewargs_ex__ should return a tuple, not '{}'", tname(&r));
        }
        if tuple::tuple_len(&r) != 2 {
            raise!(
                ValueError,
                "__getnewargs_ex__ should return a tuple of length 2, not {}",
                tuple::tuple_len(&r)
            );
        }
        let a = tuple::tuple_get(&r, 0).to_ref();
        let k = tuple::tuple_get(&r, 1).to_ref();
        if !tuple::is_tuple(&a) {
            raise!(
                TypeError,
                "first item of the tuple returned by __getnewargs_ex__ must be a tuple, not '{}'",
                tname(&a)
            );
        }
        if !dict::is_dict(&k) {
            raise!(
                TypeError,
                "second item of the tuple returned by __getnewargs_ex__ must be a dict, not '{}'",
                tname(&k)
            );
        }
        return Ok((Some(a), Some(k)));
    }
    if let Some(f) = ab::lookup_special(o, name!("__getnewargs__"))? {
        let r = ab::call(&f, &[])?;
        if !tuple::is_tuple(&r) {
            raise!(TypeError, "__getnewargs__ should return a tuple, not '{}'", tname(&r));
        }
        return Ok((Some(r), None));
    }
    Ok((None, None))
}

fn get_items_iter(o: &PyObject) -> PyResult<(PyRef, PyRef)> {
    let listitems = if list::is_list(o) { ab::get_iter(o)? } else { none::none_ref() };
    let dictitems = if dict::is_dict(o) {
        let items = ab::call_method(o, name!("items"), &[])?;
        ab::get_iter(&items)?
    } else {
        none::none_ref()
    };
    Ok((listitems, dictitems))
}

fn reduce_newobj(o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    if unsafe { (*tp).tp_new.is_none() } {
        raise!(TypeError, "cannot pickle '{}' object", type_name(unsafe { &*tp }));
    }
    let (args, kwargs) = get_new_arguments(o)?;
    let cr = copyreg()?;
    let hasargs = args.is_some();
    let kw_empty = match &kwargs {
        None => true,
        Some(k) => dict::dict_len(k) == 0,
    };
    let (newobj, newargs) = if kw_empty {
        let f = ab::getattr_str(&cr, "__newobj__")?;
        let mut v = vec![type_ref(tp)];
        if let Some(a) = &args {
            v.extend(tuple::tuple_to_vec(a));
        }
        (f, tuple::new_tuple(v))
    } else {
        let f = ab::getattr_str(&cr, "__newobj_ex__")?;
        (f, tuple::new_tuple(vec![type_ref(tp), args.clone().unwrap(), kwargs.clone().unwrap()]))
    };
    let required = !hasargs && !list::is_list(o) && !dict::is_dict(o);
    let state = get_state(o, required)?;
    let (li, di) = get_items_iter(o)?;
    Ok(tuple::new_tuple(vec![newobj, newargs, state, li, di]))
}

/// `_common_reduce`: the default `object.__reduce_ex__(proto)` / `object.__reduce__()`.
pub fn reduce_ex(o: &PyObject, proto: i64) -> PyResult {
    if proto >= 2 {
        return reduce_newobj(o);
    }
    let cr = copyreg()?;
    let f = ab::getattr_str(&cr, "_reduce_ex")?;
    ab::call(&f, &[o, &crate::types::int::new_int(proto)])
}
