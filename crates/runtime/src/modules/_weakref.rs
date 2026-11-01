//! The `_weakref` builtin module.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::type_obj;
use crate::types::{dict, int, list, none, weakref};

fn getweakrefcount(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(weakref::weakref_count(o) as i64))
}
fn getweakrefs(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(list::new_list(weakref::weakrefs_of(o)))
}
fn proxy(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("proxy", args, 1, 2)?;
    weakref::new_proxy(args[0], args.get(1).copied())
}
/// _remove_dead_weakref(dict, key): delete dict[key] if it is a dead weakref.
fn remove_dead_weakref(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_remove_dead_weakref", args, 2, 2)?;
    let (d, key) = (args[0], args[1]);
    if !dict::is_dict(d) {
        raise!(TypeError, "_remove_dead_weakref() argument 1 must be dict, not {}", crate::types::typeobject::tname(d));
    }
    let h = dict::hash_for(d, key)?;
    let (g, f) = dict::find(d, key, h)?;
    if let dict::Found::At(ix) = f {
        let v = dict::table(d).entries()[ix].value;
        let vo = unsafe { &*v };
        if weakref::is_weakref(vo) && weakref::is_dead(vo) {
            let (k, v) = dict::remove_at(d, ix, h);
            drop(g);
            unsafe {
                decref(k);
                decref(v);
            }
            return Ok(none::none_ref());
        }
    }
    drop(g);
    Ok(none::none_ref())
}

pub fn init() -> PyResult {
    let m = crate::types::module::new_module("_weakref")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("getweakrefcount", O, getweakrefcount, "getweakrefcount($module, object, /)\n--\n\nReturn the number of weak references to 'object'."),
            ("getweakrefs", O, getweakrefs, "getweakrefs($module, object, /)\n--\n\nReturn a list of all weak reference objects pointing to 'object'."),
            ("proxy", FAST, proxy, "proxy($module, object, callback=None, /)\n--\n\nCreate a proxy object that weakly references 'object'.\n\n'callback', if given, is called with a reference to the\nproxy when 'object' is about to be finalized."),
            ("_remove_dead_weakref", FAST, remove_dead_weakref, "_remove_dead_weakref($module, dct, key, /)\n--\n\nAtomically remove key from dict if it points to a dead weakref."),
        ],
        Some(&m),
    )?;
    dict::set_item_str(&d, "ref", type_obj(weakref::ref_type()))?;
    dict::set_item_str(&d, "ReferenceType", type_obj(weakref::ref_type()))?;
    dict::set_item_str(&d, "ProxyType", type_obj(weakref::proxy_type()))?;
    dict::set_item_str(&d, "CallableProxyType", type_obj(weakref::callable_proxy_type()))?;
    Ok(m)
}
