//! object.__reduce_ex__ / object.__getstate__ (the default pickling protocol).
//!
//! Protocols 0 and 1 delegate to `copyreg._reduce_ex`; protocols >= 2 build the
//! `copyreg.__newobj__` / `__newobj_ex__` form directly.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::{lookup, object_type, type_ref};
use crate::types::{dict, list, none, tuple};
use crate::{name, raise};

fn tname(o: &PyObject) -> String {
    crate::types::typeobject::tname(o)
}

/// copyreg._slotnames(cls), cached in cls.__slotnames__ by copyreg itself.
fn slotnames(cls: &PyObject) -> PyResult<Option<PyRef>> {
    let tp = crate::types::typeobject::as_type(cls);
    let d = crate::types::typeobject::type_dict(tp);
    if let Some(v) = dict::get_item_str(d, name!("__slotnames__")) {
        return Ok(if v.is(none::none()) { None } else { Some(v) });
    }
    let copyreg = crate::import::import_absolute("copyreg")?;
    let f = ab::getattr_str(&copyreg, "_slotnames")?;
    let r = ab::call(&f, &[cls])?;
    if r.is(none::none()) {
        return Ok(None);
    }
    if !list::is_list(&r) {
        raise!(TypeError, "copyreg._slotnames didn't return a list or None");
    }
    Ok(Some(r))
}

/// The default `object.__getstate__` behaviour.
pub fn object_getstate(o: &PyObject, required: bool) -> PyResult {
    let tp = o.type_ptr();
    if required && unsafe { (*tp).tp_itemsize } != 0 {
        raise!(TypeError, "cannot pickle {} objects", crate::types::typeobject::type_fqname(tp));
    }
    let mut state: PyRef = match crate::types::typeobject::instance_dict_existing(o) {
        Some(d) if dict::dict_len(&d) > 0 => d,
        _ => none::none_ref(),
    };
    let cls = type_ref(tp);
    let names = slotnames(&cls)?;
    if required {
        // Objects whose layout carries native data beyond __dict__, __weakref__
        // and __slots__ cannot be reconstructed from the state.
        let nslots = names.as_ref().map_or(0, |n| list::list_len(n)) as isize;
        let ptr = std::mem::size_of::<usize>() as isize;
        unsafe {
            let mut allowed = (*object_type()).tp_basicsize + nslots * ptr;
            if (*tp).tp_dictoffset > 0 {
                allowed += ptr;
            }
            if (*tp).tp_weaklistoffset > 0 {
                allowed += ptr;
            }
            if (*tp).tp_basicsize > allowed {
                raise!(TypeError, "cannot pickle '{}' object", crate::types::typeobject::type_fqname(tp));
            }
        }
    }
    if let Some(names) = names {
        let slots = dict::new_dict();
        for nm in list::list_to_vec(&names) {
            match ab::getattr_opt(o, &nm)? {
                Some(v) => dict::set_item(&slots, &nm, &v)?,
                None => {}
            }
        }
        if dict::dict_len(&slots) > 0 {
            state = tuple::tuple2(state, slots);
        }
    }
    Ok(state)
}

/// `_PyObject_GetState`: call an overridden __getstate__, else the default.
fn get_state(o: &PyObject, required: bool) -> PyResult {
    let default = lookup(object_type(), name!("__getstate__"));
    let mine = lookup(o.type_ptr(), name!("__getstate__"));
    match (default, mine) {
        (Some(a), Some(b)) if a.is(&b) => object_getstate(o, required),
        _ => ab::call_method(o, name!("__getstate__"), &[]),
    }
}

fn getnewargs(o: &PyObject) -> PyResult<(Option<PyRef>, Option<PyRef>)> {
    if let Some(f) = ab::lookup_special(o, name!("__getnewargs_ex__"))? {
        let r = ab::call(&f, &[])?;
        if !tuple::is_tuple(&r) {
            raise!(TypeError, "__getnewargs_ex__ should return a tuple, not '{}'", tname(&r));
        }
        if tuple::tuple_len(&r) != 2 {
            raise!(TypeError, "__getnewargs_ex__ should return a tuple of length 2, not {}", tuple::tuple_len(&r));
        }
        let (a, k) = (tuple::tuple_get(&r, 0), tuple::tuple_get(&r, 1));
        if !tuple::is_tuple(a) {
            raise!(TypeError, "first item of the tuple returned by __getnewargs_ex__ must be a tuple, not '{}'", tname(a));
        }
        if !dict::is_dict(k) {
            raise!(TypeError, "second item of the tuple returned by __getnewargs_ex__ must be a dict, not '{}'", tname(k));
        }
        return Ok((Some(a.to_ref()), Some(k.to_ref())));
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

fn reduce_newobj(o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    if unsafe { (*tp).tp_new.is_none() } {
        raise!(TypeError, "cannot pickle '{}' object", crate::types::typeobject::type_fqname(tp));
    }
    let (args, kwargs) = getnewargs(o)?;
    let hasargs = args.is_some();
    let args = args.unwrap_or_else(tuple::empty_tuple);
    let copyreg = crate::import::import_absolute("copyreg")?;
    let cls = type_ref(tp);
    let (newobj, newargs) = match kwargs.filter(|k| dict::dict_len(k) > 0) {
        None => {
            let f = ab::getattr_str(&copyreg, "__newobj__")?;
            let mut v = vec![cls];
            v.extend(tuple::tuple_to_vec(&args));
            (f, tuple::new_tuple(v))
        }
        Some(k) => {
            let f = ab::getattr_str(&copyreg, "__newobj_ex__")?;
            let k = if dict::is_dict_exact(&k) { k } else { dict::copy(&k)? };
            (f, tuple::new_tuple(vec![cls, args, k]))
        }
    };
    let is_list = list::is_list(o);
    let is_dict = dict::is_dict(o);
    let state = get_state(o, !(hasargs || is_list || is_dict))?;
    let listitems = if is_list { ab::get_iter(o)? } else { none::none_ref() };
    let dictitems = if is_dict {
        let items = ab::call_method_str(o, "items", &[])?;
        ab::get_iter(&items)?
    } else {
        none::none_ref()
    };
    Ok(tuple::new_tuple(vec![newobj, newargs, state, listitems, dictitems]))
}

/// object.__reduce_ex__(proto) once an overridden __reduce__ has been ruled out
/// (object.__reduce__ calls this with protocol 0).
pub fn reduce_ex(o: &PyObject, proto: i64) -> PyResult {
    if proto >= 2 {
        return reduce_newobj(o);
    }
    let copyreg = crate::import::import_absolute("copyreg")?;
    let f = ab::getattr_str(&copyreg, "_reduce_ex")?;
    ab::call(&f, &[o, &crate::types::int::new_int(proto)])
}

#[allow(dead_code)]
fn _unused(_: *mut PyTypeObject) {}
