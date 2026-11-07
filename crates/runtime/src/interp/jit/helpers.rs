//! Runtime helpers called from JIT code (extern "C", SysV ABI).
//!
//! Conventions: `sp` points one past the top of the value stack (the slot for depth D).
//! A helper pops its inputs (taking ownership), pushes its outputs, and returns 0 on
//! success. On error it returns non-zero with the exception set and its inputs
//! consumed (released); the JIT then exits to the interpreter with the stack depth
//! "D - inputs" for the unwinder. Semantics mirror tier 0 (eval.rs) exactly; shared
//! logic is called, not copied, wherever eval.rs exposes it.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::interp::cache::Entry;
use crate::interp::eval;
use crate::interp::frame::Frame;
use crate::interp::specialize;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, funcobject, int, list, none, str as pystr, tuple};
use rpy_compiler::bytecode::{self as bc, BinOp};

pub const OK: u64 = 0;
pub const ERR: u64 = 1;

#[inline(always)]
unsafe fn take(p: *mut *mut PyObject) -> PyRef {
    unsafe { PyRef::from_raw(std::ptr::replace(p, std::ptr::null_mut())) }
}
#[inline(always)]
unsafe fn take_opt(p: *mut *mut PyObject) -> Option<PyRef> {
    unsafe { PyRef::from_raw_opt(std::ptr::replace(p, std::ptr::null_mut())) }
}
#[inline(always)]
unsafe fn put(p: *mut *mut PyObject, v: PyRef) {
    unsafe { *p = v.into_raw() };
}
#[inline(always)]
fn st<T>(r: PyResult<T>) -> Result<T, u64> {
    r.map_err(|_| ERR)
}

macro_rules! helper {
    ($body:block) => {{
        let r: Result<u64, u64> = (|| -> Result<u64, u64> { $body })();
        match r {
            Ok(v) => v,
            Err(e) => e,
        }
    }};
}

// ------------------------------------------------------------------ profiling

/// Record operand types (slow paths of tier-1 code): `a` in the low byte of the
/// profile word, `b` (may be NULL) in the high byte.
pub unsafe extern "C" fn h_prof(p: *const std::sync::atomic::AtomicU16, a: *mut PyObject, b: *mut PyObject) {
    unsafe {
        let mut bits = if a.is_null() { 0 } else { specialize::profile_bits(&*a) as u16 };
        if !b.is_null() {
            bits |= (specialize::profile_bits(&*b) as u16) << 8;
        }
        let p = &*p;
        let old = p.load(std::sync::atomic::Ordering::Relaxed);
        if old | bits != old {
            p.store(old | bits, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Iterator-kind profile bits for ForIter (`cache::prof::IT_*`).
pub fn iter_profile_bits(it: &PyObject) -> u8 {
    use crate::interp::cache::prof;
    let t = it.type_ptr();
    if t == &raw mut crate::types::range::PyRangeIter_Type {
        prof::IT_RANGE
    } else if t == &raw mut list::PyListIter_Type {
        prof::IT_LIST
    } else if t == &raw mut tuple::PyTupleIter_Type {
        prof::IT_TUPLE
    } else if t == crate::types::genobject::gen_type() {
        prof::IT_GEN
    } else if t == crate::types::enumerate::enumerate_type() {
        prof::IT_ENUM
    } else {
        prof::OTHER
    }
}

pub unsafe extern "C" fn h_prof_iter(p: *const std::sync::atomic::AtomicU16, it: *mut PyObject) {
    unsafe {
        let bits = iter_profile_bits(&*it) as u16;
        let p = &*p;
        let old = p.load(std::sync::atomic::Ordering::Relaxed);
        if old | bits != old {
            p.store(old | bits, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ------------------------------------------------------------------ refcounts / boxing

pub unsafe extern "C" fn h_decref(o: *mut PyObject) {
    unsafe { decref(o) }
}
pub unsafe extern "C" fn h_merge_zero(o: *mut PyObject) {
    unsafe { merge_zero_local_refcount(o) }
}
pub unsafe extern "C" fn h_decref_shared(o: *mut PyObject) {
    unsafe { decref_shared(o) }
}
pub unsafe extern "C" fn h_incref_shared(o: *mut PyObject) {
    unsafe { incref(o) }
}
pub unsafe extern "C" fn h_new_int(v: i64) -> *mut PyObject {
    int::new_int(v).into_raw()
}
pub unsafe extern "C" fn h_new_float(v: f64) -> *mut PyObject {
    crate::types::float::new_float(v).into_raw()
}

/// 1 if the exact dict `d` has no key `name` (an exact str), else 0.
pub unsafe extern "C" fn h_dict_lacks(d: *mut PyObject, name: *mut PyObject) -> u64 {
    unsafe {
        match dict::lookup_index(&*d, &*name, pystr::str_hash(&*name)) {
            Ok(None) => 1,
            Ok(Some(_)) => 0,
            Err(_) => {
                clear_exc();
                0
            }
        }
    }
}

/// Replace the value at entry index `ix` of exact dict `d` if its key is `name`
/// (taking ownership of `v`): 0 done, 1 key not there (nothing consumed).
pub unsafe extern "C" fn h_dict_replace(d: *mut PyObject, ix: u64, name: *mut PyObject, v: *mut PyObject) -> u64 {
    unsafe {
        let g = dict::ob_lock(&*d);
        let t = dict::table(&*d);
        match t.entries_mut().get_mut(ix as usize) {
            Some(e) if e.key == name => {
                let old = std::mem::replace(&mut e.value, v);
                t.version = crate::interp::cache::next_dict_version();
                crate::interp::cache::on_dict_write(&*d, name);
                drop(g);
                crate::qsbr::decref_deferred(old); // perf-runtime: lock-free readers (D6)
                0
            }
            _ => 1,
        }
    }
}

// ------------------------------------------------------------------ frame / control

pub unsafe extern "C" fn h_eval_breaker(ts: *const ThreadState) -> u64 {
    match eval::handle_eval_breaker(unsafe { &*ts }) {
        Ok(()) => OK,
        Err(_) => ERR,
    }
}

pub unsafe extern "C" fn h_unbound_local(f: *mut Frame, idx: u64) -> u64 {
    let _ = eval::unbound_local(unsafe { &*f }, idx as usize);
    ERR
}

/// truth value of TOS, consuming it: 0 false, 1 true, 2 error
pub unsafe extern "C" fn h_truth_pop(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let v = take(sp.sub(1));
        match ab::is_true(&v) {
            Ok(t) => t as u64,
            Err(_) => 2,
        }
    }
}
/// truth value of TOS without popping: 0 false, 1 true, 2 error (TOS still owned)
pub unsafe extern "C" fn h_truth_peek(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        match ab::is_true(&**sp.sub(1)) {
            Ok(t) => t as u64,
            Err(_) => 2,
        }
    }
}

// ------------------------------------------------------------------ names / attributes

pub unsafe extern "C" fn h_load_global(f: *mut Frame, sp: *mut *mut PyObject, name: *mut PyObject, e: *const Entry) -> u64 {
    unsafe {
        match specialize::load_global(&*f, &*name, &*e) {
            Ok(v) => {
                put(sp, v);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_load_attr(sp: *mut *mut PyObject, name: *mut PyObject, e: *const Entry) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        match specialize::load_attr(&o, &*name, std::slice::from_raw_parts(e, crate::interp::cache::POLY)) {
            Ok(v) => {
                put(sp.sub(1), v);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_load_method(sp: *mut *mut PyObject, name: *mut PyObject, e: *const Entry) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        match specialize::load_method(&o, &*name, std::slice::from_raw_parts(e, crate::interp::cache::POLY)) {
            Ok((m, s)) => {
                put(sp.sub(1), m);
                *sp = s.map_or(std::ptr::null_mut(), |s| s.into_raw());
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_store_attr(sp: *mut *mut PyObject, name: *mut PyObject, e: *const Entry) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        let v = take(sp.sub(2));
        match specialize::store_attr(&o, &*name, &v, std::slice::from_raw_parts(e, crate::interp::cache::POLY)) {
            Ok(()) => OK,
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_delete_attr(sp: *mut *mut PyObject, name: *mut PyObject) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        match ab::delattr(&o, &*name) {
            Ok(()) => OK,
            Err(_) => ERR,
        }
    }
}

/// LoadMethod on an object with inline values (jit3): does shape `sh` lack the
/// interned `name`? If so, remember the shape's id in the cache entry's memo. 1/0.
pub unsafe extern "C" fn h_shape_lacks_memo(sh: *const crate::shape::Shape, name: *mut PyObject, memo: *const std::sync::atomic::AtomicU32) -> u64 {
    unsafe {
        if (*sh).keys().contains(&name) {
            return 0;
        }
        (*memo).store((*sh).id, std::sync::atomic::Ordering::Relaxed);
        1
    }
}

pub unsafe extern "C" fn h_store_global(f: *mut Frame, sp: *mut *mut PyObject, name: *mut PyObject) -> u64 {
    unsafe {
        let v = take(sp.sub(1));
        match dict::set_item((*f).globals(), &*name, &v) {
            Ok(()) => OK,
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_load_deref(f: *mut Frame, sp: *mut *mut PyObject, idx: u64) -> u64 {
    unsafe {
        let cell = (&(*f).slots)[idx as usize];
        let v = if cell.is_null() { None } else { funcobject::cell_get(&*cell) };
        match v {
            Some(v) => {
                put(sp, v);
                OK
            }
            None => {
                let _ = eval::unbound_local(&*f, idx as usize);
                ERR
            }
        }
    }
}

pub unsafe extern "C" fn h_store_deref(f: *mut Frame, sp: *mut *mut PyObject, idx: u64) -> u64 {
    unsafe {
        let v = take(sp.sub(1));
        let cell = (&(*f).slots)[idx as usize];
        funcobject::cell_set(&*cell, Some(v));
        OK
    }
}

pub unsafe extern "C" fn h_make_cell(f: *mut Frame, idx: u64) -> u64 {
    unsafe {
        let i = idx as usize;
        let slots = &mut (*f).slots;
        let v = std::mem::replace(&mut slots[i], std::ptr::null_mut());
        slots[i] = funcobject::new_cell(PyRef::from_raw_opt(v)).into_raw();
        OK
    }
}

pub unsafe extern "C" fn h_copy_free_vars(f: *mut Frame, n: u64) -> u64 {
    unsafe {
        let f = &mut *f;
        let n = n as usize;
        if f.func.is_null() {
            let _ = system_error("CopyFreeVars without function");
            return ERR;
        }
        let clos = funcobject::func(&*f.func).func_closure;
        if clos.is_null() {
            let _ = system_error("missing closure");
            return ERR;
        }
        let items = tuple::tuple_items(&*clos);
        let base = f.nlocalsplus - n;
        for i in 0..n {
            let c = items[i].to_ref().into_raw();
            let old = std::mem::replace(&mut f.slots[base + i], c);
            xdecref(old);
        }
        OK
    }
}

// ------------------------------------------------------------------ operators

pub unsafe extern "C" fn h_binary_op(sp: *mut *mut PyObject, op: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        let op: BinOp = std::mem::transmute(op as u8);
        match eval::binary_op_on_stack(op, &a, &b) {
            Ok(r) => {
                put(sp.sub(2), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

fn compare_obj(a: &PyObject, b: &PyObject, arg: u32) -> PyResult {
    let cop = (arg & 15) as std::ffi::c_int;
    if let Some(t) = specialize::compare_fast(cop, a, b) {
        return Ok(bool_::new_bool(t));
    }
    let r = ab::rich_compare(a, b, cop)?;
    if arg & 16 != 0 && !bool_::is_bool(&r) { Ok(bool_::new_bool(ab::is_true(&r)?)) } else { Ok(r) }
}

pub unsafe extern "C" fn h_compare(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        match compare_obj(&a, &b, arg as u32) {
            Ok(r) => {
                put(sp.sub(2), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

/// CompareOp (bool result) fused with a conditional jump: 0 false, 1 true, 2 error.
pub unsafe extern "C" fn h_compare_truth(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        let cop = (arg & 15) as std::ffi::c_int;
        if let Some(t) = specialize::compare_fast(cop, &a, &b) {
            return t as u64;
        }
        let r = match ab::rich_compare(&a, &b, cop) {
            Ok(r) => r,
            Err(_) => return 2,
        };
        drop(a);
        drop(b);
        match ab::is_true(&r) {
            Ok(t) => t as u64,
            Err(_) => 2,
        }
    }
}

pub unsafe extern "C" fn h_is_op(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        put(sp.sub(2), bool_::new_bool(a.is(&b) != (arg == 1)));
        OK
    }
}

pub unsafe extern "C" fn h_contains_op(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        match ab::contains(&b, &a) {
            Ok(r) => {
                put(sp.sub(2), bool_::new_bool(r != (arg == 1)));
                OK
            }
            Err(_) => ERR,
        }
    }
}

/// which: 0 negative, 1 positive, 2 invert, 3 not, 4 to_bool
pub unsafe extern "C" fn h_unary(sp: *mut *mut PyObject, which: u64) -> u64 {
    unsafe {
        let a = take(sp.sub(1));
        let r = match which {
            0 => {
                if int::is_int_exact(&a) && int::is_compact(&a) {
                    Ok(int::new_int(-int::compact_value(&a)))
                } else {
                    ab::negative(&a)
                }
            }
            1 => ab::positive(&a),
            2 => ab::invert(&a),
            3 => ab::is_true(&a).map(|t| bool_::new_bool(!t)),
            _ => ab::is_true(&a).map(bool_::new_bool),
        };
        match r {
            Ok(r) => {
                put(sp.sub(1), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_binary_subscr(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let k = take(sp.sub(1));
        let c = take(sp.sub(2));
        helper!({
            let r = if list::is_list_exact(&c) && int::is_int_exact(&k) && int::is_compact(&k) {
                match list::get_signed(&c, int::compact_value(&k)) {
                    Some(v) => v,
                    None => {
                        let _ = err!(IndexError, "list index out of range");
                        return Err(ERR);
                    }
                }
            } else if tuple::is_tuple_exact(&c) && int::is_int_exact(&k) && int::is_compact(&k) {
                let mut i = int::compact_value(&k);
                let n = tuple::tuple_len(&c) as i64;
                if i < 0 {
                    i += n;
                }
                if i < 0 || i >= n {
                    let _ = err!(IndexError, "tuple index out of range");
                    return Err(ERR);
                }
                tuple::tuple_get(&c, i as usize).to_ref()
            } else if dict::is_dict_exact(&c) {
                match st(dict::get_item(&c, &k))? {
                    Some(v) => v,
                    None => {
                        crate::types::dict_methods::raise_key_error(&k);
                        return Err(ERR);
                    }
                }
            } else {
                st(ab::getitem(&c, &k))?
            };
            put(sp.sub(2), r);
            Ok(OK)
        })
    }
}

pub unsafe extern "C" fn h_store_subscr(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let k = take(sp.sub(1));
        let c = take(sp.sub(2));
        let v = take(sp.sub(3));
        let r = if dict::is_dict_exact(&c) { dict::set_item(&c, &k, &v) } else { ab::setitem(&c, &k, Some(&v)) };
        if r.is_ok() { OK } else { ERR }
    }
}

pub unsafe extern "C" fn h_delete_subscr(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let k = take(sp.sub(1));
        let c = take(sp.sub(2));
        if ab::setitem(&c, &k, None).is_ok() { OK } else { ERR }
    }
}

/// (jit5) Bounds of a step-1 slice `[start:stop]` over a sequence of length `len`
/// when both are None or compact ints: (lo, hi) with lo <= hi.
#[inline]
fn simple_bounds(start: &PyObject, stop: &PyObject, len: usize) -> Option<(usize, usize)> {
    let b = |o: &PyObject, dflt: isize| -> Option<isize> {
        if o.is(none::none()) {
            Some(dflt)
        } else if int::is_int_exact(o) && int::is_compact(o) {
            Some(int::compact_value(o) as isize)
        } else {
            None
        }
    };
    let (mut lo, mut hi) = (b(start, 0)?, b(stop, isize::MAX)?);
    crate::types::slice::adjust_indices(len as isize, &mut lo, &mut hi, 1);
    Some((lo as usize, (hi.max(lo)) as usize))
}

pub unsafe extern "C" fn h_binary_slice(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let stop = take(sp.sub(1));
        let start = take(sp.sub(2));
        let c = take(sp.sub(3));
        // (jit5) exact list/tuple with None/int bounds: no slice object
        if list::is_list_exact(&c) {
            let _g = crate::types::dict::ob_lock(&c);
            let items = list::list_items(&c);
            if let Some((lo, hi)) = simple_bounds(&start, &stop, items.len()) {
                put(sp.sub(3), list::new_list_from_slice(&items[lo..hi]));
                return OK;
            }
        } else if tuple::is_tuple_exact(&c) {
            let items = tuple::tuple_items(&c);
            if let Some((lo, hi)) = simple_bounds(&start, &stop, items.len()) {
                let r = if lo == 0 && hi == items.len() {
                    c.clone()
                } else {
                    tuple::new_tuple(items[lo..hi].iter().map(|x| x.to_ref()).collect())
                };
                put(sp.sub(3), r);
                return OK;
            }
        }
        let s = crate::types::slice::new_slice(&start, &stop, none::none());
        match ab::getitem(&c, &s) {
            Ok(r) => {
                put(sp.sub(3), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_store_slice(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let stop = take(sp.sub(1));
        let start = take(sp.sub(2));
        let c = take(sp.sub(3));
        let v = take(sp.sub(4));
        // (jit5) list[a:b] = other_list of the same length (fannkuch's reversal):
        // replace the items in place, no slice object or vectors
        if list::is_list_exact(&c) && list::is_list_exact(&v) && !v.is(&c) {
            let done = {
                // two list locks: always in address order
                let (first, second) = if (c.as_ptr() as usize) < (v.as_ptr() as usize) { (&c, &v) } else { (&v, &c) };
                let _g1 = crate::types::dict::ob_lock(first);
                let _g2 = crate::types::dict::ob_lock(second);
                let items = list::list_items(&c);
                let new = list::list_items(&v);
                match simple_bounds(&start, &stop, items.len()) {
                    Some((lo, hi)) if hi - lo == new.len() && new.len() <= 32 => {
                        let mut old: [*mut PyObject; 32] = [std::ptr::null_mut(); 32];
                        let base = items.as_ptr() as *mut *mut PyObject;
                        for (k, x) in new.iter().enumerate() {
                            incref(x.as_ptr());
                            let slot = base.add(lo + k);
                            old[k] = *slot;
                            std::sync::atomic::AtomicPtr::from_ptr(slot).store(x.as_ptr(), std::sync::atomic::Ordering::Release);
                        }
                        Some((old, new.len()))
                    }
                    _ => None,
                }
            };
            if let Some((old, n)) = done {
                for &p in &old[..n] {
                    crate::qsbr::decref_deferred(p);
                }
                return OK;
            }
        }
        let s = crate::types::slice::new_slice(&start, &stop, none::none());
        if ab::setitem(&c, &s, Some(&v)).is_ok() { OK } else { ERR }
    }
}

pub unsafe extern "C" fn h_build_slice(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let step = if n == 3 { Some(take(sp.sub(1))) } else { None };
        let sp = if n == 3 { sp.sub(1) } else { sp };
        let stop = take(sp.sub(1));
        let start = take(sp.sub(2));
        let s = crate::types::slice::new_slice(&start, &stop, step.as_deref().unwrap_or(none::none()));
        put(sp.sub(2), s);
        OK
    }
}

// ------------------------------------------------------------------ builders

unsafe fn take_n(sp: *mut *mut PyObject, n: usize) -> Vec<PyRef> {
    unsafe { (0..n).map(|i| take(sp.sub(n - i))).collect() }
}

pub unsafe extern "C" fn h_build_tuple(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let items = take_n(sp, n);
        put(sp.sub(n), tuple::new_tuple(items));
        OK
    }
}

pub unsafe extern "C" fn h_build_list(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let items = take_n(sp, n);
        put(sp.sub(n), list::new_list(items));
        OK
    }
}

pub unsafe extern "C" fn h_build_set(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let items = take_n(sp, n);
        match crate::types::set::new_set_from_vec(items) {
            Ok(s) => {
                put(sp.sub(n), s);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_build_map(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let items = take_n(sp, 2 * n);
        let d = dict::new_dict_presized(n);
        for i in 0..n {
            if dict::set_item(&d, &items[2 * i], &items[2 * i + 1]).is_err() {
                return ERR;
            }
        }
        put(sp.sub(2 * n), d);
        OK
    }
}

pub unsafe extern "C" fn h_build_string(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let items = take_n(sp, n);
        let mut sb = pystr::StrBuilder::new();
        for s in &items {
            sb.push_obj(s);
        }
        put(sp.sub(n), sb.finish());
        OK
    }
}

/// ListAppend/SetAdd/MapAdd/ListExtend/SetUpdate/DictUpdate/DictMerge.
/// which: 0 list_append 1 set_add 2 map_add 3 list_extend 4 set_update 5 dict_update 6 dict_merge
pub unsafe extern "C" fn h_container_op(sp: *mut *mut PyObject, which: u64, arg: u64) -> u64 {
    unsafe {
        let arg = arg as usize;
        helper!({
            match which {
                0 => {
                    let v = take(sp.sub(1));
                    let l = *sp.sub(1 + arg);
                    list::append(&*l, v);
                }
                1 => {
                    let v = take(sp.sub(1));
                    let s = *sp.sub(1 + arg);
                    if !crate::types::set::is_set(&*s) {
                        let _ = err!(TypeError, "'{}' object is not a set", tname(&*s));
                        return Err(ERR);
                    }
                    st(crate::types::set::set_add(&*s, &v))?;
                }
                2 => {
                    let v = take(sp.sub(1));
                    let k = take(sp.sub(2));
                    let d = *sp.sub(2 + arg);
                    st(dict::set_item(&*d, &k, &v))?;
                }
                3 => {
                    let it = take(sp.sub(1));
                    let l = *sp.sub(1 + arg);
                    match ab::collect_iterable(&it) {
                        Ok(v) => {
                            for x in v {
                                list::append(&*l, x);
                            }
                        }
                        Err(_) => {
                            if exc_matches(crate::types::exceptions::TypeError())
                                && it.ty().tp_iter.is_none()
                                && !eval::is_sequence_like(&it)
                            {
                                clear_exc();
                                let _ = err!(TypeError, "Value after * must be an iterable, not {}", tname(&it));
                            }
                            return Err(ERR);
                        }
                    }
                }
                4 => {
                    let it = take(sp.sub(1));
                    let s = *sp.sub(1 + arg);
                    st(crate::types::set::set_update(&*s, &it))?;
                }
                5 => {
                    let o = take(sp.sub(1));
                    let d = *sp.sub(1 + arg);
                    st(crate::types::dict_methods::dict_update_display(&*d, &o))?;
                }
                _ => {
                    let o = take(sp.sub(1));
                    let d = *sp.sub(1 + arg);
                    let func = *sp.sub(1 + arg + 3); // callable, self-or-NULL, args, dict
                    st(crate::types::dict_methods::dict_merge_kwargs(&*d, &o, func))?;
                }
            }
            Ok(OK)
        })
    }
}

pub unsafe extern "C" fn h_unpack_sequence(sp: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let seq = take(sp.sub(1));
        let base = sp.sub(1);
        if tuple::is_tuple_exact(&seq) && tuple::tuple_len(&seq) == n {
            let items = tuple::tuple_items(&seq);
            for i in 0..n {
                *base.add(n - 1 - i) = items[i].to_ref().into_raw();
            }
            return OK;
        }
        if list::is_list_exact(&seq) {
            if let Some(items) = list::items_if_len(&seq, n) {
                for (i, v) in items.into_iter().enumerate() {
                    *base.add(n - 1 - i) = v.into_raw();
                }
                return OK;
            }
        }
        let mut items = Vec::with_capacity(n);
        if eval::unpack_sequence(&seq, n, &mut items).is_err() {
            return ERR;
        }
        for (i, v) in items.into_iter().enumerate() {
            *base.add(n - 1 - i) = v.into_raw();
        }
        OK
    }
}

pub unsafe extern "C" fn h_unpack_ex(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let seq = take(sp.sub(1));
        let before = (arg & 0xff) as usize;
        let after = (arg >> 8) as usize;
        match eval::unpack_ex(&seq, before, after) {
            Ok(items) => {
                let base = sp.sub(1);
                let n = items.len();
                for (i, v) in items.into_iter().enumerate() {
                    *base.add(n - 1 - i) = v.into_raw();
                }
                OK
            }
            Err(_) => ERR,
        }
    }
}

// ------------------------------------------------------------------ iteration

pub unsafe extern "C" fn h_get_iter(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        match ab::get_iter(&o) {
            Ok(it) => {
                put(sp.sub(1), it);
                OK
            }
            Err(_) => ERR,
        }
    }
}

/// 0: value pushed; 1: exhausted (iterator popped); 2: error (iterator still on the stack)
/// (jit6) ForIter at the head of a draining consumer loop (tier1.rs `drain_pattern`)
/// whose iterator is an exact generator: a generator that has not started yet runs
/// in drain mode and hands its values to the consumer directly (genobject.rs
/// `jit_send_drain`). kind_arg = DRAIN_* | container distance below sp << 8.
/// Returns 0: a value for the consumer's own loop body at *sp, 1: exhausted
/// (iterator released), 2: error, 3: not applicable (nothing changed).
pub unsafe extern "C" fn h_drain(sp: *mut *mut PyObject, ts: *const ThreadState, kind_arg: u64) -> u64 {
    use crate::types::genobject as gm;
    unsafe {
        let it = *sp.sub(1);
        let fp = gm::gen_frame(&*it);
        if fp.is_null() || (*fp).state != crate::interp::frame::FrameState::Created {
            return 3;
        }
        let code = &*(*fp).code;
        let cd = crate::types::code::code_data(code);
        let Some(c) = super::drain_code(cd, code) else { return 3 };
        let entry = match (&(*c).entries).get((*fp).next_instr as usize) {
            Some(&o) if o != u32::MAX => (*c).addr + o as usize,
            _ => return 3,
        };
        let kind = kind_arg & 0xff;
        let container = if kind <= gm::DRAIN_SET { *sp.sub((kind_arg >> 8) as usize) } else { std::ptr::null_mut() };
        let (st, v) = gm::jit_send_drain(&*ts, it, container, kind, entry);
        match st {
            gm::JS_YIELD => {
                *sp = v;
                0
            }
            gm::JS_RETURN => {
                decref(v);
                decref(std::ptr::replace(sp.sub(1), std::ptr::null_mut()));
                1
            }
            gm::JS_ERROR => {
                if !exc_matches(crate::types::exceptions::StopIteration()) {
                    return 2;
                }
                clear_exc();
                decref(std::ptr::replace(sp.sub(1), std::ptr::null_mut()));
                1
            }
            _ => 3,
        }
    }
}

pub unsafe extern "C" fn h_for_iter(sp: *mut *mut PyObject, ts: *const ThreadState) -> u64 {
    unsafe {
        let it = &**sp.sub(1);
        if it.is_type(crate::types::genobject::gen_type()) {
            use crate::types::genobject as gm;
            let (st, v) = gm::jit_send(&*ts, it.as_ptr(), std::ptr::null_mut());
            match st {
                gm::JS_YIELD => {
                    *sp = v;
                    return 0;
                }
                gm::JS_RETURN => {
                    decref(v);
                    decref(std::ptr::replace(sp.sub(1), std::ptr::null_mut()));
                    return 1;
                }
                gm::JS_ERROR => {
                    if !exc_matches(crate::types::exceptions::StopIteration()) {
                        return 2;
                    }
                    clear_exc();
                    decref(std::ptr::replace(sp.sub(1), std::ptr::null_mut()));
                    return 1;
                }
                _ => {}
            }
        }
        let f = match it.ty().tp_iternext {
            Some(f) => f,
            None => {
                let _ = err!(TypeError, "'{}' object is not an iterator", tname(it));
                return 2;
            }
        };
        let r = f(it.as_ptr());
        if !r.is_null() {
            *sp = r;
            return 0;
        }
        if err_occurred() {
            if exc_matches(crate::types::exceptions::StopIteration()) {
                clear_exc();
            } else {
                return 2;
            }
        }
        decref(std::ptr::replace(sp.sub(1), std::ptr::null_mut()));
        1
    }
}

/// Send: [recv, v] -> [recv, yielded] (0) | [result] (1, jump) | error (2, [recv])
pub unsafe extern "C" fn h_send(sp: *mut *mut PyObject, ts: *const ThreadState) -> u64 {
    unsafe {
        let recvp = *sp.sub(2);
        let rt = (*recvp).type_ptr();
        if rt == crate::types::genobject::gen_type() || rt == crate::types::genobject::coro_type() {
            use crate::types::genobject as gm;
            let (st, r) = gm::jit_send(&*ts, recvp, *sp.sub(1));
            match st {
                gm::JS_YIELD => {
                    *sp.sub(1) = r;
                    return 0;
                }
                gm::JS_RETURN => {
                    *sp.sub(1) = std::ptr::null_mut();
                    decref(std::ptr::replace(sp.sub(2), r));
                    return 1;
                }
                gm::JS_ERROR => {
                    *sp.sub(1) = std::ptr::null_mut();
                    if !exc_matches(crate::types::exceptions::StopIteration()) {
                        return 2;
                    }
                    let x = take_exc().unwrap();
                    let val = crate::types::exceptions::stop_iteration_value(&x);
                    decref(std::ptr::replace(sp.sub(2), val.into_raw()));
                    return 1;
                }
                _ => {}
            }
        }
        let v = take(sp.sub(1));
        let recv = &**sp.sub(2);
        match crate::types::genobject::send_to(recv, &v) {
            Ok(crate::types::genobject::SendResult::Yield(r)) => {
                put(sp.sub(1), r);
                0
            }
            Ok(crate::types::genobject::SendResult::Return(r)) => {
                decref(std::ptr::replace(sp.sub(2), std::ptr::null_mut()));
                put(sp.sub(2), r);
                1
            }
            Err(_) => 2,
        }
    }
}

/// GetYieldFromIter (code_flags: the frame's co_flags)
pub unsafe extern "C" fn h_get_yield_from_iter(sp: *mut *mut PyObject, code_flags: u64) -> u64 {
    unsafe {
        let o = take(sp.sub(1));
        let r = if crate::types::genobject::is_coroutine_exact(&o) {
            if code_flags as u32 & (bc::co::COROUTINE | bc::co::ITERABLE_COROUTINE) == 0 {
                let _ = err!(TypeError, "cannot 'yield from' a coroutine object in a non-coroutine generator");
                return ERR;
            }
            o
        } else if crate::types::genobject::is_generator_exact(&o) {
            o
        } else {
            match ab::get_iter(&o) {
                Ok(i) => i,
                Err(_) => return ERR,
            }
        };
        put(sp.sub(1), r);
        OK
    }
}

// ------------------------------------------------------------------ calls

/// Call / CallKw: [callable, self_or_null, args..(, kwnames)] -> [result]
pub unsafe extern "C" fn h_call(sp: *mut *mut PyObject, n: u64, kw: u64) -> u64 {
    unsafe {
        let n = n as usize;
        let (top, kwnames) = if kw != 0 { (sp.sub(1), take_opt(sp.sub(1))) } else { (sp, None) };
        let base = top.sub(n + 2);
        let callable = *base;
        let self_or_null = *base.add(1);
        let nkw = kwnames.as_ref().map_or(0, |k| tuple::tuple_len(k));
        let kwp = kwnames.as_ref().map_or(std::ptr::null_mut(), |k| k.as_ptr());
        let r = if self_or_null.is_null() {
            let args = base.add(2) as *const *mut PyObject;
            if funcobject::is_function(&*callable) {
                let all = std::slice::from_raw_parts(args, n);
                into_c(crate::interp::call_function(&*callable, all, n - nkw, kwnames.as_deref()))
            } else {
                ab::vectorcall_raw(&*callable, args, (n - nkw) | PY_VECTORCALL_ARGUMENTS_OFFSET, kwp)
            }
        } else {
            let args = base.add(1) as *const *mut PyObject;
            if funcobject::is_function(&*callable) {
                let all = std::slice::from_raw_parts(args, n + 1);
                into_c(crate::interp::call_function(&*callable, all, n + 1 - nkw, kwnames.as_deref()))
            } else {
                ab::vectorcall_raw(&*callable, args, n + 1 - nkw, kwp)
            }
        };
        drop(kwnames);
        let mut p = top;
        while p > base {
            p = p.sub(1);
            xdecref(std::ptr::replace(p, std::ptr::null_mut()));
        }
        if r.is_null() {
            return ERR;
        }
        *base = r;
        OK
    }
}

/// CallFunctionEx: [callable, self_or_null, args, kwargs_or_NULL] -> [result]
pub unsafe extern "C" fn h_call_ex(sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let kw = take_opt(sp.sub(1));
        let args = take(sp.sub(2));
        let self_or_null = take_opt(sp.sub(3));
        let callable = take(sp.sub(4));
        helper!({
            let args = if tuple::is_tuple_exact(&args) {
                args
            } else {
                match ab::sequence_tuple(&args) {
                    Ok(t) => t,
                    Err(_) => {
                        if exc_matches(crate::types::exceptions::TypeError())
                            && args.ty().tp_iter.is_none()
                            && !eval::is_sequence_like(&args)
                        {
                            clear_exc();
                            let _ = err!(
                                TypeError,
                                "Value after * must be an iterable, not {}",
                                tname(&args)
                            );
                        }
                        return Err(ERR);
                    }
                }
            };
            let mut v: Vec<&PyObject> = Vec::with_capacity(tuple::tuple_len(&args) + 1);
            if let Some(s) = &self_or_null {
                v.push(s);
            }
            v.extend(tuple::tuple_items(&args).iter().copied());
            let kwd = match &kw {
                Some(k) if !dict::is_dict(k) => {
                    let d = dict::new_dict();
                    st(crate::types::dict_methods::dict_merge_kwargs(&d, k, callable.as_ptr()))?;
                    Some(d)
                }
                Some(k) => Some(k.clone()),
                None => None,
            };
            if let Some(d) = &kwd {
                st(eval::check_kwargs_keys(d))?;
            }
            let r = st(ab::call_with_kwargs_dict(&callable, &v, kwd.as_deref()))?;
            put(sp.sub(4), r);
            Ok(OK)
        })
    }
}

pub unsafe extern "C" fn h_intrinsic1(ts: *const ThreadState, f: *mut Frame, sp: *mut *mut PyObject, which: u64) -> u64 {
    unsafe {
        let v = take(sp.sub(1));
        match eval::call_intrinsic1(&*ts, &mut *f, which as u32, v) {
            Ok(r) => {
                put(sp.sub(1), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_intrinsic2(f: *mut Frame, sp: *mut *mut PyObject, which: u64) -> u64 {
    unsafe {
        let b = take(sp.sub(1));
        let a = take(sp.sub(2));
        match eval::call_intrinsic2(&mut *f, which as u32, a, b) {
            Ok(r) => {
                put(sp.sub(2), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_make_function(f: *mut Frame, sp: *mut *mut PyObject) -> u64 {
    unsafe {
        let code = take(sp.sub(1));
        match funcobject::new_function(&code, (*f).globals(), None) {
            Ok(func) => {
                put(sp.sub(1), func);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_set_function_attribute(sp: *mut *mut PyObject, flag: u64) -> u64 {
    unsafe {
        let func = take(sp.sub(1));
        let attr = take(sp.sub(2));
        let fo = funcobject::func(&func);
        let slot = match flag as u32 {
            bc::make_fn::DEFAULTS => &mut fo.func_defaults,
            bc::make_fn::KWDEFAULTS => &mut fo.func_kwdefaults,
            bc::make_fn::ANNOTATIONS => &mut fo.func_annotations,
            bc::make_fn::CLOSURE => &mut fo.func_closure,
            bc::make_fn::ANNOTATE => &mut fo.func_annotate,
            _ => {
                let _ = err!(SystemError, "bad SetFunctionAttribute flag");
                return ERR;
            }
        };
        let old = std::mem::replace(slot, attr.into_raw());
        xdecref(old);
        put(sp.sub(2), func);
        OK
    }
}

// ------------------------------------------------------------------ formatting

/// which: 0 FormatSimple, 1 FormatWithSpec, 2.. ConvertValue(which-2+1)
pub unsafe extern "C" fn h_format(sp: *mut *mut PyObject, which: u64) -> u64 {
    unsafe {
        let r = match which {
            0 => {
                let v = take(sp.sub(1));
                eval::format_value(&v, None)
            }
            1 => {
                let spec = take(sp.sub(1));
                let v = take(sp.sub(2));
                eval::format_value(&v, Some(&spec))
            }
            k => {
                let v = take(sp.sub(1));
                match k - 1 {
                    1 => ab::str(&v),
                    2 => ab::repr(&v),
                    3 => ab::ascii(&v),
                    _ => Err(err!(SystemError, "bad conversion")),
                }
            }
        };
        let out = if which == 1 { sp.sub(2) } else { sp.sub(1) };
        match r {
            Ok(r) => {
                put(out, r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

pub unsafe extern "C" fn h_load_super_attr(sp: *mut *mut PyObject, arg: u64, name: *mut PyObject) -> u64 {
    unsafe {
        let slf = take(sp.sub(1));
        let cls = take(sp.sub(2));
        let gsuper = take(sp.sub(3));
        helper!({
            let sup = if gsuper.is(type_obj(crate::types::super_::super_type())) {
                st(crate::types::super_::new_super(&cls, &slf))?
            } else if arg & 2 != 0 {
                st(ab::call(&gsuper, &[&cls, &slf]))?
            } else {
                st(ab::call(&gsuper, &[]))?
            };
            let v = st(ab::getattr(&sup, &*name))?;
            put(sp.sub(3), v);
            if arg & 1 != 0 {
                *sp.sub(2) = std::ptr::null_mut();
            }
            Ok(OK)
        })
    }
}

// ------------------------------------------------------------------ shared instructions

/// GetAwaitable: [o] -> [awaitable]. A coroutine that is not suspended in an await
/// is its own awaitable (the common `await coro()` case needs no further work).
pub unsafe extern "C" fn h_get_awaitable(sp: *mut *mut PyObject, arg: u64) -> u64 {
    unsafe {
        let o = &**sp.sub(1);
        if crate::types::genobject::is_coroutine_exact(o) && !crate::types::genobject::coro_is_running_await(o) {
            return OK;
        }
        let o = take(sp.sub(1));
        match eval::get_awaitable(&o, arg as u32) {
            Ok(r) => {
                put(sp.sub(1), r);
                OK
            }
            Err(_) => ERR,
        }
    }
}

/// Instructions implemented once in `eval::exec_misc` (exception handlers, with,
/// async iteration, imports, pattern matching, ...). `oparg` = op | arg << 8; the
/// value stack ends at `sp`. On error the stack is cut back to `err_top` (absolute
/// slot index: the depth tier-1 code reports to the unwinder), releasing whatever the
/// instruction left above it.
pub unsafe extern "C" fn h_misc(ts: *const ThreadState, f: *mut Frame, sp: *mut *mut PyObject, oparg: u64, err_top: u64) -> u64 {
    unsafe {
        let f = &mut *f;
        // compiled code does not maintain stack_top: it is accurate only while the
        // instruction runs, and restored afterwards (slots above the static depth
        // that compiled code later pops are not cleared, so a stale stack_top would
        // expose freed objects to frame traversal)
        let saved = f.stack_top;
        f.stack_top = sp.offset_from(f.slots.as_ptr()) as usize;
        let op = bc::Op::from_u8((oparg & 0xff) as u8);
        let cd = &*(crate::types::code::code_data(f.code()) as *const crate::types::code::CodeData);
        match eval::exec_misc(&*ts, f, cd, op, (oparg >> 8) as u32) {
            Ok(()) => {
                f.stack_top = saved;
                OK
            }
            Err(_) => {
                let e = err_top as usize;
                while f.stack_top > e {
                    f.stack_top -= 1;
                    xdecref(std::mem::replace(&mut f.slots[f.stack_top], std::ptr::null_mut()));
                }
                ERR
            }
        }
    }
}
