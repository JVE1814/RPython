//! tupleobject.h / listobject.h / setobject.h / sliceobject.h.

use super::*;
use crate::abstract_ as ab;
use crate::types::typeobject::type_obj;
use crate::types::{int, list, none, set, slice, tuple};

// ---------------------------------------------------------------- tuple

unsafe fn chk_tuple<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !tuple::is_tuple(&*o) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_New(n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if n < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if n == 0 {
            return tuple::empty_tuple().into_raw();
        }
        let op = tuple::alloc_tuple_of(tuple::tuple_type(), n as usize);
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        std::ptr::write_bytes(tuple::item_ptr(op), 0, n as usize);
        crate::gc::track(op);
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_FromArray(items: *const *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if n < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if n == 0 {
            return tuple::empty_tuple().into_raw();
        }
        tuple::new_tuple_from_ptrs(std::slice::from_raw_parts(items, n as usize)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_tuple(o) {
            Ok(t) => tuple::tuple_len(t) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_GetItem(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let Ok(t) = chk_tuple(o) else { return std::ptr::null_mut() };
        if i < 0 || i as usize >= tuple::tuple_len(t) {
            let _ = crate::err!(IndexError, "tuple index out of range");
            return std::ptr::null_mut();
        }
        *tuple::item_ptr(o).add(i as usize)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_SetItem(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !tuple::is_tuple(&*o) || (*o).refcnt() != 1 && !(*o).is_immortal() && false {
            xdecref(v);
            let _ = bad_internal();
            return -1;
        }
        if i < 0 || i as usize >= tuple::tuple_len(&*o) {
            xdecref(v);
            let _ = crate::err!(IndexError, "tuple assignment index out of range");
            return -1;
        }
        let slot = tuple::item_ptr(o).add(i as usize);
        let old = *slot;
        *slot = v;
        xdecref(old);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_GetSlice(o: *mut PyObject, lo: Py_ssize_t, hi: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let Ok(t) = chk_tuple(o) else { return std::ptr::null_mut() };
        let n = tuple::tuple_len(t) as isize;
        let lo = lo.clamp(0, n);
        let hi = hi.clamp(lo, n);
        if lo == 0 && hi == n && tuple::is_tuple_exact(t) {
            return t.to_ref().into_raw();
        }
        let items = &tuple::tuple_items(t)[lo as usize..hi as usize];
        tuple::new_tuple(items.iter().map(|x| x.to_ref()).collect()).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyTuple_Resize(p: *mut *mut PyObject, n: Py_ssize_t) -> c_int {
    unsafe {
        let o = *p;
        if o.is_null() || !tuple::is_tuple(&*o) || n < 0 {
            *p = std::ptr::null_mut();
            xdecref(o);
            let _ = bad_internal();
            return -1;
        }
        let old_n = tuple::tuple_len(&*o);
        let new = PyTuple_New(n);
        if new.is_null() {
            *p = std::ptr::null_mut();
            decref(o);
            return -1;
        }
        let src = tuple::item_ptr(o);
        let dst = tuple::item_ptr(new);
        let k = old_n.min(n as usize);
        for i in 0..k {
            *dst.add(i) = *src.add(i);
            *src.add(i) = std::ptr::null_mut();
        }
        decref(o);
        *p = new;
        0
    }
}

// ---------------------------------------------------------------- list

unsafe fn chk_list<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !list::is_list(&*o) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_New(n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if n < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let l = list::new_list_empty();
        if n > 0 {
            let lp = l.as_ptr() as *mut PyListObject;
            let arr = crate::alloc::buf_calloc(n as usize * 8) as *mut *mut PyObject;
            if arr.is_null() {
                let _ = memory_error();
                return std::ptr::null_mut();
            }
            (*lp).ob_item = arr;
            (*lp).allocated = n;
            (*lp).ob_base.ob_size = n;
        }
        l.into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_list(o) {
            Ok(l) => list::list_len(l) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
unsafe fn list_slot(o: *mut PyObject, i: Py_ssize_t) -> Option<*mut *mut PyObject> {
    unsafe {
        let l = o as *mut PyListObject;
        if i < 0 || i >= (*l).ob_base.ob_size {
            return None;
        }
        Some((*l).ob_item.add(i as usize))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetItem(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if chk_list(o).is_err() {
            return std::ptr::null_mut();
        }
        match list_slot(o, i) {
            Some(s) => *s,
            None => {
                let _ = crate::err!(IndexError, "list index out of range");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetItemRef(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if o.is_null() || !list::is_list(&*o) {
            let _ = type_error("expected a list");
            return std::ptr::null_mut();
        }
        // the strong-reference variant is safe against concurrent writers
        match if i < 0 { None } else { list::get_signed(&*o, i as i64) } {
            Some(v) => v.into_raw(),
            None => {
                let _ = crate::err!(IndexError, "list index out of range");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetItem(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        if chk_list(o).is_err() {
            xdecref(v);
            return -1;
        }
        let old = {
            let _g = crate::types::dict::ob_lock(&*o);
            list_slot(o, i).map(|s| std::mem::replace(&mut *s, v))
        };
        match old {
            Some(old) => {
                if !old.is_null() {
                    crate::qsbr::decref_deferred(old); // lock-free readers (list.rs)
                }
                0
            }
            None => {
                xdecref(v);
                let _ = crate::err!(IndexError, "list assignment index out of range");
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Insert(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let Ok(l) = chk_list(o) else { return -1 };
        if v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let n = list::list_len(l) as isize;
        let mut i = i;
        if i < 0 {
            i += n;
            if i < 0 {
                i = 0;
            }
        }
        if i > n {
            i = n;
        }
        list::insert(l, i as usize, (*v).to_ref());
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Append(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let Ok(l) = chk_list(o) else { return -1 };
        if v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        list::append(l, (*v).to_ref());
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_GetSlice(o: *mut PyObject, lo: Py_ssize_t, hi: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let Ok(l) = chk_list(o) else { return std::ptr::null_mut() };
        let _g = crate::types::dict::ob_lock(l);
        let n = list::list_len(l) as isize;
        let lo = lo.clamp(0, n);
        let hi = hi.clamp(lo, n);
        let items = &list::list_items(l)[lo as usize..hi as usize];
        list::new_list(items.iter().map(|x| x.to_ref()).collect()).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_SetSlice(o: *mut PyObject, lo: Py_ssize_t, hi: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let Ok(l) = chk_list(o) else { return -1 };
        let n = list::list_len(l) as isize;
        let lo = lo.clamp(0, n);
        let hi = hi.clamp(lo, n);
        let items = if v.is_null() {
            Vec::new()
        } else {
            match ab::collect_iterable(&*v) {
                Ok(x) => x,
                Err(_) => {
                    if exc_matches(crate::types::exceptions::TypeError()) {
                        clear_exc();
                        let _ = type_error(&format!("can only assign an iterable"));
                    }
                    return -1;
                }
            }
        };
        list::splice(l, lo as usize, hi as usize, items);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Sort(o: *mut PyObject) -> c_int {
    unsafe {
        let Ok(l) = chk_list(o) else { return -1 };
        ret_i(ab::call_method_str(l, "sort", &[]).map(|_| ()))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Reverse(o: *mut PyObject) -> c_int {
    unsafe {
        let Ok(l) = chk_list(o) else { return -1 };
        let mut v = list::list_to_vec(l);
        v.reverse();
        list::set_contents(l, v);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_AsTuple(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(l) = chk_list(o) else { return std::ptr::null_mut() };
        tuple::new_tuple(list::list_to_vec(l)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Extend(o: *mut PyObject, it: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !list::is_list(&*o) {
            let _ = bad_internal();
            return -1;
        }
        ret_i(ab::call_method_str(&*o, "extend", &[&*it]).map(|_| ()))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyList_Clear(o: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !list::is_list(&*o) {
            let _ = bad_internal();
            return -1;
        }
        list::clear(&*o);
        0
    }
}

// ---------------------------------------------------------------- set

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_New(it: *mut PyObject) -> *mut PyObject {
    unsafe {
        match opt(it) {
            None => ret(set::new_set_from_vec(Vec::new())),
            Some(i) => ret(ab::call(type_obj(set::set_type()), &[i])),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrozenSet_New(it: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = match opt(it) {
            None => set::new_frozenset_from_vec(Vec::new()),
            Some(i) => ab::call(type_obj(set::frozenset_type()), &[i]),
        };
        match r {
            Ok(s) => {
                if !set::set_items(&s).iter().any(|x| crate::gc::is_tracked(x)) {
                    crate::gc::untrack(s.as_ptr());
                }
                s.into_raw()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe fn chk_anyset<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !set::is_anyset(&*o) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_anyset(o) {
            Ok(s) => set::set_len(s) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let Ok(s) = chk_anyset(o) else { return -1 };
        // unlike `k in s`, the C API does not retry an unhashable set as a frozenset
        let Some(k) = opt(k) else {
            null_arg();
            return -1;
        };
        ret_b(ab::hash(k).and_then(|h| set::contains_entry(s, k, h)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Add(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let Ok(s) = chk_anyset(o) else { return -1 };
        // frozensets may be filled while they are brand new (refcount 1)
        if set::is_frozenset(s) && s.refcnt() != 1 {
            let _ = bad_internal();
            return -1;
        }
        if set::set_add(s, &*k).is_err() {
            return -1;
        }
        if set::is_frozenset(s) && crate::gc::is_tracked(&*k) {
            crate::gc::track(o);
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Discard(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !set::is_set(&*o) {
            let _ = bad_internal();
            return -1;
        }
        ret_b(set::set_discard(&*o, &*k))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Pop(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() || !set::is_set(&*o) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::call_method_str(&*o, "pop", &[]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySet_Clear(o: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || !set::is_set(&*o) {
            let _ = bad_internal();
            return -1;
        }
        ret_i(ab::call_method_str(&*o, "clear", &[]).map(|_| ()))
    }
}

// ---------------------------------------------------------------- slice

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySlice_New(start: *mut PyObject, stop: *mut PyObject, step: *mut PyObject) -> *mut PyObject {
    unsafe {
        let n = none::none();
        slice::new_slice(opt(start).unwrap_or(n), opt(stop).unwrap_or(n), opt(step).unwrap_or(n)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PySlice_FromIndices(start: Py_ssize_t, stop: Py_ssize_t) -> *mut PyObject {
    slice::new_slice(&int::new_int(start as i64), &int::new_int(stop as i64), none::none()).into_raw()
}
fn clamp_index(v: &PyObject) -> PyResult<isize> {
    // like _PyEval_SliceIndex: clamp to [PY_SSIZE_T_MIN, PY_SSIZE_T_MAX]
    if !ab::has_index(v) {
        return Err(type_error("slice indices must be integers or None or have an __index__ method"));
    }
    let i = ab::index(v)?;
    Ok(match int::as_i64(&i) {
        Some(x) => x as isize,
        None => {
            if int::sign(&i) < 0 { isize::MIN } else { isize::MAX }
        }
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySlice_Unpack(o: *mut PyObject, start: *mut Py_ssize_t, stop: *mut Py_ssize_t, step: *mut Py_ssize_t) -> c_int {
    unsafe {
        let (a, b, c) = slice::parts(&*o);
        let n = none::none();
        let r = (|| -> PyResult<()> {
            if c.is(n) {
                *step = 1;
            } else {
                let s = clamp_index(c)?;
                if s == 0 {
                    return Err(value_error("slice step cannot be zero"));
                }
                *step = s.max(-isize::MAX);
            }
            *start = if a.is(n) { if *step < 0 { isize::MAX } else { 0 } } else { clamp_index(a)? };
            *stop = if b.is(n) { if *step < 0 { isize::MIN } else { isize::MAX } } else { clamp_index(b)? };
            Ok(())
        })();
        ret_i(r)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySlice_AdjustIndices(len: Py_ssize_t, start: *mut Py_ssize_t, stop: *mut Py_ssize_t, step: Py_ssize_t) -> Py_ssize_t {
    unsafe {
        if *start < 0 {
            *start += len;
            if *start < 0 {
                *start = if step < 0 { -1 } else { 0 };
            }
        } else if *start >= len {
            *start = if step < 0 { len - 1 } else { len };
        }
        if *stop < 0 {
            *stop += len;
            if *stop < 0 {
                *stop = if step < 0 { -1 } else { 0 };
            }
        } else if *stop >= len {
            *stop = if step < 0 { len - 1 } else { len };
        }
        if step < 0 {
            if *stop < *start {
                return (*start - *stop - 1) / (-step) + 1;
            }
        } else if *start < *stop {
            return (*stop - *start - 1) / step + 1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySlice_GetIndicesEx(
    o: *mut PyObject,
    len: Py_ssize_t,
    start: *mut Py_ssize_t,
    stop: *mut Py_ssize_t,
    step: *mut Py_ssize_t,
    slicelen: *mut Py_ssize_t,
) -> c_int {
    unsafe {
        if PySlice_Unpack(o, start, stop, step) < 0 {
            return -1;
        }
        *slicelen = PySlice_AdjustIndices(len, start, stop, *step);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySlice_GetIndices(o: *mut PyObject, len: Py_ssize_t, start: *mut Py_ssize_t, stop: *mut Py_ssize_t, step: *mut Py_ssize_t) -> c_int {
    unsafe {
        let (a, b, c) = slice::parts(&*o);
        let n = none::none();
        let get = |v: &PyObject| -> Option<isize> { if int::is_int(v) { int::as_i64(v).map(|x| x as isize) } else { None } };
        if c.is(n) {
            *step = 1;
        } else {
            match get(c) {
                Some(0) | None => return -1,
                Some(s) => *step = s,
            }
        }
        if a.is(n) {
            *start = if *step < 0 { len - 1 } else { 0 };
        } else {
            match get(a) {
                Some(s) => *start = if s < 0 { s + len } else { s },
                None => return -1,
            }
        }
        if b.is(n) {
            *stop = if *step < 0 { -1 } else { len };
        } else {
            match get(b) {
                Some(s) => *stop = if s < 0 { s + len } else { s },
                None => return -1,
            }
        }
        if *stop > len || *start >= len {
            return -1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PySlice_GetLongIndices(o: *mut PyObject, len: *mut PyObject, start: *mut *mut PyObject, stop: *mut *mut PyObject, step: *mut *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<(PyRef, PyRef, PyRef)> {
            let t = ab::call_method_str(&*o, "indices", &[&*len])?;
            let it = tuple::tuple_items(&t);
            Ok((it[0].to_ref(), it[1].to_ref(), it[2].to_ref()))
        })();
        match r {
            Ok((a, b, c)) => {
                *start = a.into_raw();
                *stop = b.into_raw();
                *step = c.into_raw();
                0
            }
            Err(_) => -1,
        }
    }
}
