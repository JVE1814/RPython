//! `list` core: `PyListObject` with a separately allocated item array, the
//! sequence/mapping slots, the list iterators and type registration.
//! Methods (incl. sort) live in `list_methods.rs`.
//!
//! Mutations take the per-object lock (`dict::ob_lock`); old items are released
//! only after the lock is dropped (their deallocation may run Python code).

use crate::abstract_ as ab;
use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::dict::ob_lock;
use crate::types::typeobject::*;
use crate::types::{bool_, exceptions, int, none, slice, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyList_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyListIter_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyListRevIter_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn list_type() -> *mut PyTypeObject {
    &raw mut PyList_Type
}
#[inline(always)]
pub fn is_list(o: &PyObject) -> bool {
    o.is_type(list_type()) || o.ty().tp_flags & Py_TPFLAGS_LIST_SUBCLASS != 0
}
#[inline(always)]
pub fn is_list_exact(o: &PyObject) -> bool {
    o.is_type(list_type())
}

#[inline(always)]
fn lp(o: &PyObject) -> *mut PyListObject {
    o.as_ptr() as *mut PyListObject
}

#[inline(always)]
pub fn list_len(o: &PyObject) -> usize {
    var_size(o) as usize
}

/// Borrowed view of the items. Valid until the list is mutated.
#[inline(always)]
pub fn list_items(o: &PyObject) -> &[&PyObject] {
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        if n == 0 {
            return &[];
        }
        std::slice::from_raw_parts((*l).ob_item as *const &PyObject, n)
    }
}

/// Owned snapshot of the items.
pub fn list_to_vec(o: &PyObject) -> Vec<PyRef> {
    let _g = ob_lock(o);
    list_items(o).iter().map(|x| x.to_ref()).collect()
}

unsafe fn alloc_items(n: usize) -> *mut *mut PyObject {
    unsafe { alloc::raw_malloc(n * 8) as *mut *mut PyObject }
}

pub fn new_list(items: Vec<PyRef>) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(list_type(), std::mem::size_of::<PyListObject>());
        let l = op as *mut PyListObject;
        let n = items.len();
        if n > 0 {
            let arr = alloc_items(n);
            for (i, it) in items.into_iter().enumerate() {
                *arr.add(i) = it.into_raw();
            }
            (*l).ob_item = arr;
            (*l).allocated = n as isize;
        }
        (*l).ob_base.ob_size = n as isize;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

/// New list from borrowed items.
pub fn new_list_from_slice(items: &[&PyObject]) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(list_type(), std::mem::size_of::<PyListObject>());
        let l = op as *mut PyListObject;
        let n = items.len();
        if n > 0 {
            let arr = alloc_items(n);
            for (i, it) in items.iter().enumerate() {
                incref(it.as_ptr());
                *arr.add(i) = it.as_ptr();
            }
            (*l).ob_item = arr;
            (*l).allocated = n as isize;
        }
        (*l).ob_base.ob_size = n as isize;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn new_list_empty() -> PyRef {
    new_list(Vec::new())
}

/// list_resize: make room for `newsize` items (CPython growth pattern). Lock held.
fn resize_locked(o: &PyObject, newsize: usize) {
    unsafe {
        let l = lp(o);
        let allocated = (*l).allocated.max(0) as usize;
        if allocated >= newsize && newsize >= (allocated >> 1) {
            (*l).ob_base.ob_size = newsize as isize;
            return;
        }
        let mut new_alloc = (newsize + (newsize >> 3) + 6) & !3usize;
        if newsize.wrapping_sub(var_size(o) as usize) > new_alloc.wrapping_sub(newsize) {
            new_alloc = (newsize + 3) & !3usize;
        }
        if newsize == 0 {
            new_alloc = 0;
        }
        let arr = if new_alloc == 0 {
            if !(*l).ob_item.is_null() {
                alloc::raw_free((*l).ob_item as *mut u8);
            }
            std::ptr::null_mut()
        } else {
            alloc::raw_realloc((*l).ob_item as *mut u8, new_alloc * 8) as *mut *mut PyObject
        };
        (*l).ob_item = arr;
        (*l).allocated = new_alloc as isize;
        (*l).ob_base.ob_size = newsize as isize;
    }
}

/// Ensure capacity for `n` items.
pub fn reserve(o: &PyObject, n: usize) {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        if (*l).allocated.max(0) as usize >= n {
            return;
        }
        let arr = alloc::raw_realloc((*l).ob_item as *mut u8, n * 8) as *mut *mut PyObject;
        (*l).ob_item = arr;
        (*l).allocated = n as isize;
    }
}

#[inline]
pub fn append(o: &PyObject, item: PyRef) {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        if (n as isize) < (*l).allocated {
            *(*l).ob_item.add(n) = item.into_raw();
            (*l).ob_base.ob_size = (n + 1) as isize;
            return;
        }
        resize_locked(o, n + 1);
        *(*l).ob_item.add(n) = item.into_raw();
    }
}

pub fn get(o: &PyObject, i: usize) -> Option<PyRef> {
    let _g = ob_lock(o);
    let items = list_items(o);
    items.get(i).map(|x| x.to_ref())
}

/// Replace item `i` (must be in range).
/// Replace item `i` (negative counts from the end); `false` if out of range.
pub fn set_index(o: &PyObject, i: isize, v: PyRef) -> bool {
    let old = {
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            let n = (*l).ob_base.ob_size;
            let i = if i < 0 { i + n } else { i };
            if i < 0 || i >= n {
                return false;
            }
            let slot = (*l).ob_item.add(i as usize);
            let old = *slot;
            *slot = v.into_raw();
            old
        }
    };
    unsafe { xdecref(old) };
    true
}

pub fn set(o: &PyObject, i: usize, v: PyRef) {
    let old = {
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            if i >= (*l).ob_base.ob_size as usize {
                return;
            }
            let slot = (*l).ob_item.add(i);
            let old = *slot;
            *slot = v.into_raw();
            old
        }
    };
    unsafe { xdecref(old) };
}

/// Replace the full contents.
pub fn set_contents(o: &PyObject, items: Vec<PyRef>) {
    let (old_arr, old_n) = {
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            let old_n = (*l).ob_base.ob_size as usize;
            let old_arr = (*l).ob_item;
            let n = items.len();
            let arr = if n > 0 { alloc_items(n) } else { std::ptr::null_mut() };
            for (i, it) in items.into_iter().enumerate() {
                *arr.add(i) = it.into_raw();
            }
            (*l).ob_item = arr;
            (*l).allocated = n as isize;
            (*l).ob_base.ob_size = n as isize;
            (old_arr, old_n)
        }
    };
    release_array(old_arr, old_n);
}

fn release_array(arr: *mut *mut PyObject, n: usize) {
    unsafe {
        if arr.is_null() {
            return;
        }
        // decref back to front like CPython's list_clear
        let mut i = n;
        while i > 0 {
            i -= 1;
            xdecref(*arr.add(i));
        }
        alloc::raw_free(arr as *mut u8);
    }
}

/// Replace items[lo..hi] with `items` (owned).
pub fn splice(o: &PyObject, lo: usize, hi: usize, items: Vec<PyRef>) {
    let removed: Vec<*mut PyObject> = {
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            let n = (*l).ob_base.ob_size as usize;
            let lo = lo.min(n);
            let hi = hi.clamp(lo, n);
            let d = hi - lo;
            let k = items.len();
            let removed: Vec<*mut PyObject> = (lo..hi).map(|i| *(*l).ob_item.add(i)).collect();
            let newsize = n - d + k;
            if k > d {
                resize_locked(o, newsize);
                let arr = (*l).ob_item;
                std::ptr::copy(arr.add(hi), arr.add(lo + k), n - hi);
            } else if k < d {
                let arr = (*l).ob_item;
                std::ptr::copy(arr.add(hi), arr.add(lo + k), n - hi);
                resize_locked(o, newsize);
            }
            let arr = (*l).ob_item;
            for (i, it) in items.into_iter().enumerate() {
                *arr.add(lo + i) = it.into_raw();
            }
            removed
        }
    };
    for p in removed {
        unsafe { xdecref(p) };
    }
}

/// Remove and return item `i` (negative counts from the end); `Err(len)` if out of
/// range. The range check happens under the lock.
pub fn pop_index(o: &PyObject, i: isize) -> Result<PyRef, usize> {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        let i = if i < 0 { i + n as isize } else { i };
        if i < 0 || i as usize >= n {
            return Err(n);
        }
        let i = i as usize;
        let arr = (*l).ob_item;
        let it = *arr.add(i);
        std::ptr::copy(arr.add(i + 1), arr.add(i), n - i - 1);
        resize_locked(o, n - 1);
        Ok(PyRef::from_raw(it))
    }
}

/// Remove and return item `i`; out of range returns None.
pub fn pop(o: &PyObject, i: usize) -> PyRef {
    match pop_index(o, i as isize) {
        Ok(v) => v,
        Err(_) => none::none_ref(),
    }
}

pub fn insert(o: &PyObject, i: usize, v: PyRef) {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        let i = i.min(n);
        resize_locked(o, n + 1);
        let arr = (*l).ob_item;
        std::ptr::copy(arr.add(i), arr.add(i + 1), n - i);
        *arr.add(i) = v.into_raw();
    }
}

pub fn clear(o: &PyObject) {
    let (arr, n) = {
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            let r = ((*l).ob_item, (*l).ob_base.ob_size as usize);
            (*l).ob_item = std::ptr::null_mut();
            (*l).ob_base.ob_size = 0;
            (*l).allocated = 0;
            r
        }
    };
    release_array(arr, n);
}

/// Take the item array out of the list (leaving it empty). Used by sort.
pub fn take_items(o: &PyObject) -> Vec<*mut PyObject> {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        let v = (0..n).map(|i| *(*l).ob_item.add(i)).collect();
        if !(*l).ob_item.is_null() {
            alloc::raw_free((*l).ob_item as *mut u8);
        }
        (*l).ob_item = std::ptr::null_mut();
        (*l).ob_base.ob_size = 0;
        (*l).allocated = -1;
        v
    }
}
/// Put an owned item array back (the list must be empty); returns the items
/// that were in the list meanwhile (owned) so the caller can release them.
pub fn restore_items(o: &PyObject, items: Vec<*mut PyObject>) -> Vec<*mut PyObject> {
    let _g = ob_lock(o);
    unsafe {
        let l = lp(o);
        let n0 = (*l).ob_base.ob_size as usize;
        let old: Vec<*mut PyObject> = (0..n0).map(|i| *(*l).ob_item.add(i)).collect();
        if !(*l).ob_item.is_null() {
            alloc::raw_free((*l).ob_item as *mut u8);
        }
        let n = items.len();
        let arr = if n > 0 { alloc_items(n) } else { std::ptr::null_mut() };
        for (i, p) in items.into_iter().enumerate() {
            *arr.add(i) = p;
        }
        (*l).ob_item = arr;
        (*l).ob_base.ob_size = n as isize;
        (*l).allocated = n as isize;
        old
    }
}

pub unsafe extern "C" fn list_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let l = op as *mut PyListObject;
        let n = (*l).ob_base.ob_size as usize;
        let arr = (*l).ob_item;
        (*l).ob_item = std::ptr::null_mut();
        (*l).ob_base.ob_size = 0;
        release_array(arr, n);
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn list_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let l = op as *mut PyListObject;
        let n = (*l).ob_base.ob_size as usize;
        let f = visit.unwrap();
        for i in 0..n {
            let it = *(*l).ob_item.add(i);
            if !it.is_null() {
                let r = f(it, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

pub unsafe extern "C" fn list_clear_slot(op: *mut PyObject) -> c_int {
    unsafe { clear(&*op) };
    0
}

pub fn list_from_iter(it: &PyObject) -> PyResult<PyRef> {
    let l = new_list_empty();
    extend(&l, it)?;
    Ok(l)
}

/// list.extend(iterable) semantics.
pub fn extend(o: &PyObject, it: &PyObject) -> PyResult<()> {
    if is_list_exact(it) || tuple::is_tuple_exact(it) || o.is(it) {
        let items: Vec<PyRef> = if tuple::is_tuple_exact(it) { tuple::tuple_to_vec(it) } else { list_to_vec(it) };
        if items.is_empty() {
            return Ok(());
        }
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            let n = (*l).ob_base.ob_size as usize;
            resize_locked(o, n + items.len());
            let arr = (*l).ob_item;
            for (i, x) in items.into_iter().enumerate() {
                *arr.add(n + i) = x.into_raw();
            }
        }
        return Ok(());
    }
    let iter = ab::get_iter(it)?;
    let hint = ab::length_hint(it, 8)?;
    if hint > 0 {
        let n = list_len(o);
        let _g = ob_lock(o);
        unsafe {
            let l = lp(o);
            if ((*l).allocated.max(0) as usize) < n + hint {
                let want = n + hint.min(1 << 24);
                let arr = alloc::raw_realloc((*l).ob_item as *mut u8, want * 8) as *mut *mut PyObject;
                (*l).ob_item = arr;
                (*l).allocated = want as isize;
            }
        }
    }
    let next = iter.ty().tp_iternext;
    loop {
        let x = match next {
            Some(f) => {
                let r = unsafe { f(iter.as_ptr()) };
                if r.is_null() {
                    if err_occurred() {
                        if exc_matches(exceptions::StopIteration()) {
                            clear_exc();
                            break;
                        }
                        return Err(PyErr);
                    }
                    break;
                }
                unsafe { PyRef::from_raw(r) }
            }
            None => match ab::iter_next(&iter)? {
                Some(x) => x,
                None => break,
            },
        };
        append(o, x);
    }
    Ok(())
}

// ================================================================== slots

fn index_error(msg: &str) -> PyErr {
    new_err(exceptions::IndexError(), msg)
}

unsafe extern "C" fn list_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if list_len(o) == 0 {
            return pystr::new_str("[]").into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(pystr::new_str("[...]"));
            }
            let mut sb = pystr::StrBuilder::new();
            sb.push('[' as u32);
            let res = (|| {
                let mut i = 0;
                while let Some(it) = get(o, i) {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    sb.push_obj(&*ab::repr(&it)?);
                    i += 1;
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            sb.push(']' as u32);
            Ok(sb.finish())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn list_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { var_size(&*o) }
}
unsafe extern "C" fn list_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        match if i < 0 { None } else { get(&*o, i as usize) } {
            Some(v) => v.into_raw(),
            None => into_c(Err(index_error("list index out of range"))),
        }
    }
}

/// Elements selected by a resolved slice.
pub fn slice_items(items: &[&PyObject], start: isize, step: isize, n: isize) -> Vec<PyRef> {
    let mut v = Vec::with_capacity(n.max(0) as usize);
    let mut i = start;
    for _ in 0..n {
        v.push(items[i as usize].to_ref());
        i = i.wrapping_add(step);
    }
    v
}

fn list_getitem(o: &PyObject, k: &PyObject) -> PyResult {
    if int::is_int_exact(k) && int::is_compact(k) {
        let mut i = int::compact_value(k) as isize;
        let _g = ob_lock(o);
        let items = list_items(o);
        if i < 0 {
            i += items.len() as isize;
        }
        if i < 0 || i as usize >= items.len() {
            return Err(index_error("list index out of range"));
        }
        return Ok(items[i as usize].to_ref());
    }
    if slice::is_slice(k) {
        let (mut start, mut stop, step) = slice::unpack(k)?;
        let _g = ob_lock(o);
        let items = list_items(o);
        let n = slice::adjust_indices(items.len() as isize, &mut start, &mut stop, step);
        if step == 1 {
            return Ok(new_list_from_slice(&items[start as usize..(start + n) as usize]));
        }
        return Ok(new_list(slice_items(items, start, step, n)));
    }
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
        let _g = ob_lock(o);
        let items = list_items(o);
        if i < 0 {
            i += items.len() as isize;
        }
        if i < 0 || i as usize >= items.len() {
            return Err(index_error("list index out of range"));
        }
        return Ok(items[i as usize].to_ref());
    }
    raise!(TypeError, "list indices must be integers or slices, not {}", tname(k));
}
pub unsafe extern "C" fn list_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(list_getitem(&*o, &*k)) }
}

/// Slice assignment / deletion (`v` None = delete).
pub fn ass_slice(o: &PyObject, k: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let (mut start, mut stop, step) = slice::unpack(k)?;
    // Convert the value first: doing so may run code that mutates the list.
    let collected: Option<Vec<PyRef>> = match v {
        None => None,
        Some(v) => Some(if v.is(o) || is_list_exact(v) || tuple::is_tuple_exact(v) {
            if tuple::is_tuple_exact(v) { tuple::tuple_to_vec(v) } else { list_to_vec(v) }
        } else {
            let it = match ab::get_iter(v) {
                Ok(it) => it,
                Err(e) => {
                    if exc_matches(exceptions::TypeError()) {
                        clear_exc();
                        if step == 1 {
                            raise!(TypeError, "can only assign an iterable");
                        }
                        raise!(TypeError, "must assign iterable to extended slice");
                    }
                    return Err(e);
                }
            };
            let mut out = Vec::new();
            while let Some(x) = ab::iter_next(&it)? {
                out.push(x);
            }
            out
        }),
    };
    let len = list_len(o) as isize;
    let n = slice::adjust_indices(len, &mut start, &mut stop, step);
    if step == 1 {
        let items = collected.unwrap_or_default();
        let stop = stop.max(start);
        splice(o, start as usize, stop as usize, items);
        return Ok(());
    }
    // extended slice
    match collected {
        None => {
            if n <= 0 {
                return Ok(());
            }
            let removed: Vec<PyRef>;
            {
                let mut cur = take_items(o);
                let (mut s, st) = (start, step);
                if st < 0 {
                    // normalise to increasing order
                    s = start + st * (n - 1);
                }
                let ast = st.abs() as usize;
                let mut rem = Vec::with_capacity(n as usize);
                let mut keep = Vec::with_capacity(cur.len().saturating_sub(n as usize));
                let mut next_del = s as usize;
                let mut left = n;
                for (i, p) in cur.drain(..).enumerate() {
                    if left > 0 && i == next_del {
                        rem.push(unsafe { PyRef::from_raw(p) });
                        next_del = next_del.wrapping_add(ast);
                        left -= 1;
                    } else {
                        keep.push(p);
                    }
                }
                let extra = restore_items(o, keep);
                removed = rem.into_iter().chain(extra.into_iter().map(|p| unsafe { PyRef::from_raw(p) })).collect();
            }
            drop(removed);
            Ok(())
        }
        Some(seq) => {
            if seq.len() as isize != n {
                raise!(ValueError, "attempt to assign sequence of size {} to extended slice of size {}", seq.len(), n);
            }
            let mut old = Vec::with_capacity(n as usize);
            {
                let _g = ob_lock(o);
                unsafe {
                    let l = lp(o);
                    let cur = (*l).ob_base.ob_size;
                    let mut i = start;
                    for it in seq {
                        if i < 0 || i >= cur {
                            break;
                        }
                        let slot = (*l).ob_item.add(i as usize);
                        old.push(*slot);
                        *slot = it.into_raw();
                        i = i.wrapping_add(step);
                    }
                }
            }
            for p in old {
                unsafe { xdecref(p) };
            }
            Ok(())
        }
    }
}

fn list_setitem(o: &PyObject, k: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    if slice::is_slice(k) {
        return ass_slice(o, k, v);
    }
    if !ab::has_index(k) {
        raise!(TypeError, "list indices must be integers or slices, not {}", tname(k));
    }
    let i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
    let ok = match v {
        None => pop_index(o, i).is_ok(),
        Some(v) => set_index(o, i, v.to_ref()),
    };
    if !ok {
        return Err(index_error("list assignment index out of range"));
    }
    Ok(())
}
unsafe extern "C" fn list_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(list_setitem(&*o, &*k, if v.is_null() { None } else { Some(&*v) })) }
}
unsafe extern "C" fn list_ass_item(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        let ok = if i < 0 {
            false
        } else if v.is_null() {
            pop_index(o, i).is_ok()
        } else {
            set_index(o, i, (*v).to_ref())
        };
        if !ok {
            return int_into_c(Err(index_error("list assignment index out of range")));
        }
        0
    }
}

/// Lexicographic sequence comparison (shared by list and tuple).
pub fn seq_richcompare(a: &dyn Fn(usize) -> Option<PyRef>, alen: &dyn Fn() -> usize, b: &dyn Fn(usize) -> Option<PyRef>, blen: &dyn Fn() -> usize, op: c_int) -> PyResult<bool> {
    if (op == Py_EQ || op == Py_NE) && alen() != blen() {
        return Ok(op == Py_NE);
    }
    let mut i = 0;
    loop {
        let (x, y) = match (a(i), b(i)) {
            (Some(x), Some(y)) => (x, y),
            _ => break,
        };
        if !x.is(&y) && !ab::rich_compare_bool(&x, &y, Py_EQ)? {
            // first differing item -- unless the comparison shrank a sequence
            if i >= alen() || i >= blen() {
                break;
            }
            if op == Py_EQ {
                return Ok(false);
            }
            if op == Py_NE {
                return Ok(true);
            }
            return ab::rich_compare_bool(&x, &y, op);
        }
        i += 1;
    }
    let (la, lb) = (alen(), blen());
    Ok(match op {
        Py_LT => la < lb,
        Py_LE => la <= lb,
        Py_EQ => la == lb,
        Py_NE => la != lb,
        Py_GT => la > lb,
        _ => la >= lb,
    })
}

unsafe extern "C" fn list_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_list(b) || !is_list(a) {
            return none::not_implemented().to_ref().into_raw();
        }
        if a.is(b) && (op == Py_EQ || op == Py_NE) {
            // identity implies equal items only if every item equals itself; fall through
        }
        let r = seq_richcompare(&|i| get(a, i), &|| list_len(a), &|i| get(b, i), &|| list_len(b), op);
        into_c(r.map(bool_::new_bool))
    }
}
unsafe extern "C" fn list_contains(a: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let v = &*v;
        let mut i = 0;
        while let Some(it) = get(&*a, i) {
            if it.is(v) {
                return 1;
            }
            match ab::rich_compare_bool(&it, v, Py_EQ) {
                Ok(true) => return 1,
                Ok(false) => {}
                Err(_) => return -1,
            }
            i += 1;
        }
        0
    }
}
unsafe extern "C" fn list_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_list(b) {
            return into_c(Err(err!(TypeError, "can only concatenate list (not \"{}\") to list", tname(b))));
        }
        let mut v = list_to_vec(a);
        v.extend(list_to_vec(b));
        new_list(v).into_raw()
    }
}
unsafe extern "C" fn list_inplace_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        match extend(&*a, &*b) {
            Ok(()) => (*a).to_ref().into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
fn repeat_vec(items: &[PyRef], n: isize) -> PyResult<Vec<PyRef>> {
    if n <= 0 || items.is_empty() {
        return Ok(Vec::new());
    }
    match items.len().checked_mul(n as usize) {
        Some(t) if t <= (isize::MAX as usize) / 8 => {
            let mut v = Vec::with_capacity(t);
            for _ in 0..n {
                v.extend(items.iter().cloned());
            }
            Ok(v)
        }
        _ => Err(memory_error()),
    }
}
unsafe extern "C" fn list_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let items = list_to_vec(&*a);
        into_c(repeat_vec(&items, n).map(new_list))
    }
}
unsafe extern "C" fn list_inplace_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let o = &*a;
        let items = list_to_vec(o);
        match repeat_vec(&items, n) {
            Ok(v) => {
                set_contents(o, v);
                o.to_ref().into_raw()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn list_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            if !kw.is_null()
                && crate::types::dict::dict_len(&*kw) > 0
                && ((*o).is_type(list_type()) || (*(*o).type_ptr()).tp_new == (*list_type()).tp_new)
            {
                raise!(TypeError, "list() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "list expected at most 1 argument, got {}", a.len());
            }
            let o = &*o;
            if list_len(o) > 0 {
                clear(o);
            }
            if let Some(x) = a.first() {
                // preallocate exactly when len() is available (sys.getsizeof is observable)
                let tp = x.ty();
                let has_len = (!tp.tp_as_sequence.is_null() && (*tp.tp_as_sequence).sq_length.is_some())
                    || (!tp.tp_as_mapping.is_null() && (*tp.tp_as_mapping).mp_length.is_some());
                if has_len {
                    match ab::length(x) {
                        Ok(n) if n > 0 && n < (1 << 28) && list_len(o) == 0 => reserve(o, n),
                        Ok(_) => {}
                        Err(e) => {
                            if !exc_matches(exceptions::TypeError()) {
                                return Err(e);
                            }
                            clear_exc();
                        }
                    }
                }
                extend(o, x)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn list_new(tp: *mut PyTypeObject, _args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tp == list_type() && !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "list() takes no keyword arguments")));
        }
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        alloc(tp, 0)
    }
}

// ================================================================== iterators

#[repr(C)]
pub struct ListIter {
    ob_base: PyObject,
    index: isize,
    seq: *mut PyObject,
}

fn new_iter(tp: *mut PyTypeObject, seq: &PyObject, index: isize) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(tp, std::mem::size_of::<ListIter>());
        let it = &mut *(op as *mut ListIter);
        it.seq = seq.to_ref().into_raw();
        it.index = index;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
unsafe extern "C" fn list_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { new_iter(&raw mut PyListIter_Type, &*o, 0).into_raw() }
}
pub fn list_reversed(o: &PyObject) -> PyResult {
    Ok(new_iter(&raw mut PyListRevIter_Type, o, list_len(o) as isize - 1))
}
fn iter_finish(it: &mut ListIter) {
    let p = std::mem::replace(&mut it.seq, std::ptr::null_mut());
    unsafe { xdecref(p) };
}
unsafe extern "C" fn listiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut ListIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        if it.index >= 0 {
            if let Some(v) = get(&*it.seq, it.index as usize) {
                it.index += 1;
                return v.into_raw();
            }
        }
        iter_finish(it);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn listreviter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut ListIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        if it.index >= 0 {
            if let Some(v) = get(&*it.seq, it.index as usize) {
                it.index -= 1;
                return v.into_raw();
            }
        }
        it.index = -1;
        iter_finish(it);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn listiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut ListIter)).seq);
        alloc::free_object(o);
    }
}
unsafe extern "C" fn listiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut ListIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
pub unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn it_ref(o: &PyObject) -> &mut ListIter {
    unsafe { &mut *(o.as_ptr() as *mut ListIter) }
}
fn listiter_len(o: &PyObject) -> PyResult {
    let it = it_ref(o);
    let n = if it.seq.is_null() { 0 } else { (list_len(unsafe { &*it.seq }) as isize - it.index).max(0) };
    Ok(int::new_int(n as i64))
}
fn listreviter_len(o: &PyObject) -> PyResult {
    let it = it_ref(o);
    let n = if it.seq.is_null() || it.index < 0 || list_len(unsafe { &*it.seq }) as isize <= it.index { 0 } else { it.index + 1 };
    Ok(int::new_int(n as i64))
}
fn builtin_fn(name: &str) -> PyResult {
    crate::builtins::builtin(name)
}
fn listiter_reduce(o: &PyObject) -> PyResult {
    let iter_fn = builtin_fn("iter")?;
    let it = it_ref(o);
    if it.seq.is_null() {
        return Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![new_list_empty()])]));
    }
    Ok(tuple::new_tuple(vec![
        iter_fn,
        tuple::new_tuple(vec![unsafe { (*it.seq).to_ref() }]),
        int::new_int(it.index as i64),
    ]))
}
fn listreviter_reduce(o: &PyObject) -> PyResult {
    // look the builtin up first: the lookup may run code that exhausts us
    let rev = builtin_fn("reversed")?;
    let it = it_ref(o);
    if it.seq.is_null() {
        return Ok(tuple::new_tuple(vec![rev, tuple::new_tuple(vec![new_list_empty()])]));
    }
    Ok(tuple::new_tuple(vec![
        rev,
        tuple::new_tuple(vec![unsafe { (*it.seq).to_ref() }]),
        int::new_int(it.index as i64),
    ]))
}
fn listiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = it_ref(o);
    let mut idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        let n = list_len(unsafe { &*it.seq }) as isize;
        if idx < 0 {
            idx = 0;
        } else if idx > n {
            idx = n;
        }
        it.index = idx;
    }
    Ok(none::none_ref())
}
fn listreviter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = it_ref(o);
    let mut idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        let n = list_len(unsafe { &*it.seq }) as isize;
        if idx < -1 {
            idx = -1;
        } else if idx > n - 1 {
            idx = n - 1;
        }
        it.index = idx;
    }
    Ok(none::none_ref())
}

// ================================================================== registration

static mut LIST_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut LIST_MAP: PyMappingMethods = PyMappingMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *list_type();
        t.tp_name = c"list".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyListObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_LIST_SUBCLASS | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(list_dealloc);
        t.tp_traverse = Some(list_traverse);
        t.tp_clear = Some(list_clear_slot);
        t.tp_repr = Some(list_repr);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_richcompare = Some(list_richcompare);
        t.tp_iter = Some(list_iter);
        t.tp_init = Some(list_init);
        t.tp_new = Some(list_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Built-in mutable sequence.\n\nIf no argument is given, the constructor creates a new empty list.\nThe argument must be an iterable if specified.".as_ptr();
        let s = &mut *(&raw mut LIST_SEQ);
        s.sq_length = Some(list_len_slot);
        s.sq_concat = Some(list_concat);
        s.sq_repeat = Some(list_repeat);
        s.sq_item = Some(list_item);
        s.sq_ass_item = Some(list_ass_item);
        s.sq_contains = Some(list_contains);
        s.sq_inplace_concat = Some(list_inplace_concat);
        s.sq_inplace_repeat = Some(list_inplace_repeat);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut LIST_MAP);
        m.mp_length = Some(list_len_slot);
        m.mp_subscript = Some(list_subscript);
        m.mp_ass_subscript = Some(list_ass_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::types::list_methods::method_table();

        for (tp, nm, next, len, red, st) in [
            (
                &raw mut PyListIter_Type,
                c"list_iterator",
                listiter_next as unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
                listiter_len as fn(&PyObject) -> PyResult,
                listiter_reduce as fn(&PyObject) -> PyResult,
                listiter_setstate as fn(&PyObject, &PyObject) -> PyResult,
            ),
            (&raw mut PyListRevIter_Type, c"list_reverseiterator", listreviter_next, listreviter_len, listreviter_reduce, listreviter_setstate),
        ] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<ListIter>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(listiter_dealloc);
            t.tp_traverse = Some(listiter_traverse);
            t.tp_iter = Some(self_iter);
            t.tp_iternext = Some(next);
            t.tp_getattro = Some(object_generic_getattro);
            let _ = (len, red, st);
        }
        (*(&raw mut PyListIter_Type)).tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, listiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, listiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, listiter_setstate, "Set state information for unpickling."),
        ];
        (*(&raw mut PyListRevIter_Type)).tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, listreviter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, listreviter_reduce, "Return state information for pickling."),
            ("__setstate__", O, listreviter_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(&raw mut PyListIter_Type)?;
    type_ready(&raw mut PyListRevIter_Type)
}
