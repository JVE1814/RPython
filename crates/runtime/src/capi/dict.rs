//! dictobject.h: dict API (PyDict_Next uses stable entry positions).

use super::*;
use crate::abstract_ as ab;
use crate::types::dict;
use crate::types::{list, none, str as pystr, tuple};

unsafe fn chk<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !dict::is_dict(&*o) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}
fn is_any_dict(o: &PyObject) -> bool {
    dict::is_dict(o) || crate::types::frozendict::is_frozendict(o)
}
/// dict or frozendict (PyAnyDict_Check): accepted by the read-only functions.
unsafe fn chk_any<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !(dict::is_dict(&*o) || crate::types::frozendict::is_frozendict(&*o)) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}
/// A dict to modify; a frozendict gets the TypeError the item operations raise.
unsafe fn chk_mut<'a>(o: *mut PyObject, what: &str) -> PyResult<&'a PyObject> {
    unsafe {
        if !o.is_null() && crate::types::frozendict::is_frozendict(&*o) {
            return Err(type_error(&format!("frozendict object does not support item {}", what)));
        }
        chk(o)
    }
}
unsafe fn key_str(k: *const c_char) -> PyResult {
    unsafe { Ok(pystr::intern(cstr_obj(k)?)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_New() -> *mut PyObject {
    dict::new_dict().into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyDict_NewPresized(n: Py_ssize_t) -> *mut PyObject {
    dict::new_dict_presized(n.max(0) as usize).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrozenDict_New(it: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let b = crate::builtins::builtins_module();
            let fd = ab::getattr_str(&b, "frozendict")?;
            match opt(it) {
                Some(i) => ab::call(&fd, &[i]),
                None => ab::call(&fd, &[]),
            }
        })())
    }
}

/// Borrowed lookup without exception (PyDict_GetItem semantics: errors suppressed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItem(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() || !is_any_dict(&*o) {
            return std::ptr::null_mut();
        }
        let saved = take_exc();
        let r = match ab::hash(&*k).and_then(|h| dict::get_item_hash(&*o, &*k, h)) {
            Ok(Some(v)) => {
                let p = v.as_ptr();
                drop(v); // borrowed: the dict keeps it alive
                p
            }
            Ok(None) => std::ptr::null_mut(),
            Err(_) => {
                // the error cannot be returned: it is reported like any ignored exception
                crate::modules::sys_full::write_unraisable(Some("Exception ignored in PyDict_GetItem()"), None);
                std::ptr::null_mut()
            }
        };
        clear_exc();
        if let Some(e) = saved {
            set_exc(e);
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemWithError(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk_any(o) else { return std::ptr::null_mut() };
        match dict::get_item(d, &*k) {
            Ok(Some(v)) => {
                let p = v.as_ptr();
                drop(v);
                p
            }
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyDict_GetItem_KnownHash(o: *mut PyObject, k: *mut PyObject, h: Py_hash_t) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk_any(o) else { return std::ptr::null_mut() };
        match dict::get_item_hash(d, &*k, h) {
            Ok(Some(v)) => {
                let p = v.as_ptr();
                drop(v);
                p
            }
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemString(o: *mut PyObject, k: *const c_char) -> *mut PyObject {
    unsafe {
        let saved = take_exc();
        let r = match key_str(k) {
            Ok(key) => PyDict_GetItem(o, key.as_ptr()),
            Err(_) => {
                crate::modules::sys_full::write_unraisable(Some("Exception ignored in PyDict_GetItemString()"), None);
                std::ptr::null_mut()
            }
        };
        clear_exc();
        if let Some(e) = saved {
            set_exc(e);
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyDict_GetItemStringWithError(o: *mut PyObject, k: *const c_char) -> *mut PyObject {
    unsafe {
        match key_str(k) {
            Ok(key) => PyDict_GetItemWithError(o, key.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemRef(o: *mut PyObject, k: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        if o.is_null() || chk_any(o).is_err() {
            let _ = bad_internal();
            return -1;
        }
        match dict::get_item(&*o, &*k) {
            Ok(Some(v)) => {
                *result = v.into_raw();
                1
            }
            Ok(None) => 0,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_GetItemStringRef(o: *mut PyObject, k: *const c_char, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        match key_str(k) {
            Ok(key) => PyDict_GetItemRef(o, key.as_ptr(), result),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetItem(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let Ok(d) = chk_mut(o, "assignment") else { return -1 };
        if k.is_null() || v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        ret_i(dict::set_item(d, &*k, &*v))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetItemString(o: *mut PyObject, k: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        match key_str(k) {
            Ok(key) => PyDict_SetItem(o, key.as_ptr(), v),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_DelItem(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let Ok(d) = chk_mut(o, "deletion") else { return -1 };
        match dict::del_item(d, &*k) {
            Ok(true) => 0,
            Ok(false) => {
                let _ = crate::errors::set_err_obj(crate::types::exceptions::KeyError(), (*k).to_ref());
                -1
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_DelItemString(o: *mut PyObject, k: *const c_char) -> c_int {
    unsafe {
        match key_str(k) {
            Ok(key) => PyDict_DelItem(o, key.as_ptr()),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let Ok(d) = chk_any(o) else { return -1 };
        ret_b(dict::contains(d, &*k))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_ContainsString(o: *mut PyObject, k: *const c_char) -> c_int {
    unsafe {
        match key_str(k) {
            Ok(key) => PyDict_Contains(o, key.as_ptr()),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Clear(o: *mut PyObject) {
    unsafe {
        if let Ok(d) = chk(o) {
            dict::clear(d);
        } else {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_any(o) {
            Ok(d) => dict::dict_len(d) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Copy(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk(o) else { return std::ptr::null_mut() };
        ret(dict::copy(d))
    }
}
/// Iterate by stable entry position; returns borrowed references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Next(o: *mut PyObject, pos: *mut Py_ssize_t, key: *mut *mut PyObject, value: *mut *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !is_any_dict(&*o) {
            return 0;
        }
        let t = dict::table(&*o);
        let mut i = (*pos).max(0) as usize;
        while i < t.entries().len() {
            let e = t.entries()[i];
            i += 1;
            if !e.key.is_null() {
                *pos = i as Py_ssize_t;
                if !key.is_null() {
                    *key = e.key;
                }
                if !value.is_null() {
                    *value = e.value;
                }
                return 1;
            }
        }
        *pos = i as Py_ssize_t;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Keys(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk_any(o) else { return std::ptr::null_mut() };
        list::new_list(dict::keys_vec(d)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Values(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk_any(o) else { return std::ptr::null_mut() };
        list::new_list(dict::items_vec(d).into_iter().map(|(_, v)| v).collect()).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Items(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(d) = chk_any(o) else { return std::ptr::null_mut() };
        list::new_list(dict::items_vec(d).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect()).into_raw()
    }
}
/// PyDict_Merge(a, b, override): override 0 = keep, 1 = replace, 2 = raise KeyError on dup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Merge(a: *mut PyObject, b: *mut PyObject, over: c_int) -> c_int {
    unsafe {
        let Ok(d) = chk_mut(a, "assignment") else { return -1 };
        if b.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let r = (|| -> PyResult<()> {
            let src = &*b;
            let pairs: Vec<(PyRef, PyRef)> = if dict::is_dict(src) && src.ty().tp_iter.map(|f| f as usize) == (*dict::dict_type()).tp_iter.map(|f| f as usize) {
                dict::items_vec(src)
            } else {
                let keys = ab::call_method_str(src, "keys", &[])?;
                let mut v = Vec::new();
                for k in ab::collect_iterable(&keys)? {
                    let val = ab::getitem(src, &k)?;
                    v.push((k, val));
                }
                v
            };
            for (k, v) in pairs {
                if over != 1 && dict::contains(d, &k)? {
                    if over == 2 {
                        return Err(crate::errors::set_err_obj(crate::types::exceptions::KeyError(), k));
                    }
                    continue;
                }
                dict::set_item(d, &k, &v)?;
            }
            Ok(())
        })();
        ret_i(r)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Update(a: *mut PyObject, b: *mut PyObject) -> c_int {
    unsafe {
        if !b.is_null() && ab::getattr_opt(&*b, crate::name!("keys")).ok().flatten().is_none() {
            if !err_occurred() {
                let _ = crate::err!(AttributeError, "'{}' object has no attribute 'keys'", tname(&*b));
            }
            return -1;
        }
        PyDict_Merge(a, b, 1)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_MergeFromSeq2(a: *mut PyObject, seq: *mut PyObject, over: c_int) -> c_int {
    unsafe {
        let Ok(d) = chk_mut(a, "assignment") else { return -1 };
        let r = (|| -> PyResult<()> {
            let it = ab::get_iter(&*seq)?;
            let mut i = 0usize;
            while let Some(item) = ab::iter_next(&it)? {
                let pair = match ab::collect_iterable(&item) {
                    Ok(p) => p,
                    Err(e) => {
                        if exc_matches(crate::types::exceptions::TypeError()) {
                            clear_exc();
                            return Err(type_error(&format!(
                                "cannot convert dictionary update sequence element #{} to a sequence",
                                i
                            )));
                        }
                        return Err(e);
                    }
                };
                if pair.len() != 2 {
                    return Err(value_error(&format!(
                        "dictionary update sequence element #{} has length {}; 2 is required",
                        i,
                        pair.len()
                    )));
                }
                if over != 0 || !dict::contains(d, &pair[0])? {
                    dict::set_item(d, &pair[0], &pair[1])?;
                }
                i += 1;
            }
            Ok(())
        })();
        ret_i(r)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetDefault(o: *mut PyObject, k: *mut PyObject, dflt: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mut r = std::ptr::null_mut();
        if PyDict_SetDefaultRef(o, k, dflt, &mut r) < 0 {
            return std::ptr::null_mut();
        }
        // borrowed result
        decref(r);
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_SetDefaultRef(o: *mut PyObject, k: *mut PyObject, dflt: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        if !result.is_null() {
            *result = std::ptr::null_mut();
        }
        let Ok(d) = chk_mut(o, "assignment") else { return -1 };
        match dict::get_item(d, &*k) {
            Ok(Some(v)) => {
                if !result.is_null() {
                    *result = v.into_raw();
                }
                1
            }
            Ok(None) => {
                if dict::set_item(d, &*k, &*dflt).is_err() {
                    return -1;
                }
                if !result.is_null() {
                    *result = (*dflt).to_ref().into_raw();
                }
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Pop(o: *mut PyObject, k: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        if !result.is_null() {
            *result = std::ptr::null_mut();
        }
        let Ok(d) = chk_mut(o, "deletion") else { return -1 };
        if dict::dict_len(d) == 0 {
            return 0; // nothing to remove: the key is not even hashed
        }
        match dict::get_item(d, &*k) {
            Ok(Some(v)) => {
                if dict::del_item(d, &*k).is_err() {
                    return -1;
                }
                if !result.is_null() {
                    *result = v.into_raw();
                }
                1
            }
            Ok(None) => 0,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_PopString(o: *mut PyObject, k: *const c_char, result: *mut *mut PyObject) -> c_int {
    unsafe {
        match key_str(k) {
            Ok(key) => PyDict_Pop(o, key.as_ptr(), result),
            Err(_) => {
                if !result.is_null() {
                    *result = std::ptr::null_mut();
                }
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyDict_Pop(o: *mut PyObject, k: *mut PyObject, dflt: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mut r = std::ptr::null_mut();
        match PyDict_Pop(o, k, &mut r) {
            1 => r,
            0 => {
                if dflt.is_null() {
                    let _ = crate::errors::set_err_obj(crate::types::exceptions::KeyError(), (*k).to_ref());
                    std::ptr::null_mut()
                } else {
                    (*dflt).to_ref().into_raw()
                }
            }
            _ => std::ptr::null_mut(),
        }
    }
}

// watchers: accepted but never fire (no specialising interpreter relies on them)
static WATCHERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_AddWatcher(cb: *mut std::ffi::c_void) -> c_int {
    let mut w = WATCHERS.lock().unwrap();
    for (i, slot) in w.iter_mut().enumerate() {
        if *slot == 0 {
            *slot = cb as usize;
            return i as c_int;
        }
    }
    let _ = crate::err!(RuntimeError, "no more dict watcher IDs available");
    -1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_ClearWatcher(id: c_int) -> c_int {
    let mut w = WATCHERS.lock().unwrap();
    if !(0..8).contains(&id) || w[id as usize] == 0 {
        let _ = value_error(&format!("Invalid dict watcher ID {}", id));
        return -1;
    }
    w[id as usize] = 0;
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Watch(id: c_int, d: *mut PyObject) -> c_int {
    unsafe {
        if d.is_null() || !dict::is_dict(&*d) {
            let _ = value_error("Cannot watch non-dictionary");
            return -1;
        }
        if !(0..8).contains(&id) {
            let _ = value_error(&format!("Invalid dict watcher ID {}", id));
            return -1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDict_Unwatch(id: c_int, d: *mut PyObject) -> c_int {
    unsafe { PyDict_Watch(id, d) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDictProxy_New(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let types = crate::import::import_absolute("types")?;
            let mp = ab::getattr_str(&types, "MappingProxyType")?;
            ab::call(&mp, &[&*m])
        })())
    }
}

// ordered dict (collections.OrderedDict)
// (native type in modules/odict.rs)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyODict_New() -> *mut PyObject {
    unsafe {
        ret(crate::modules::odict::init_types()
            .and_then(|_| ab::call(crate::types::typeobject::type_obj(crate::modules::odict::odict_type()), &[])))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyODict_SetItem(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { ret_i(crate::modules::odict::setitem(&*o, &*k, &*v)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyODict_DelItem(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe { ret_i(crate::modules::odict::delitem(&*o, &*k)) }
}

#[allow(dead_code)]
fn _unused() -> PyRef {
    none::none_ref()
}
