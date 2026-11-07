//! `tuple`: layout `PyTupleObject` (ob_item inline), creation and access, slots,
//! methods, the tuple iterator and type registration.

use crate::abstract_ as ab;
use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, exceptions, int, none, slice, str as pystr};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyTuple_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyTupleIter_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn tuple_type() -> *mut PyTypeObject {
    &raw mut PyTuple_Type
}
#[inline(always)]
pub fn is_tuple(o: &PyObject) -> bool {
    o.is_type(tuple_type()) || o.ty().tp_flags & Py_TPFLAGS_TUPLE_SUBCLASS != 0
}
#[inline(always)]
pub fn is_tuple_exact(o: &PyObject) -> bool {
    o.is_type(tuple_type())
}

const HEADER: usize = std::mem::offset_of!(PyTupleObject, ob_item);

/// Allocate a tuple of type `tp` with `n` NULL items (caller must fill all).
pub unsafe fn alloc_tuple_of(tp: *mut PyTypeObject, n: usize) -> *mut PyObject {
    unsafe {
        let basic = (*tp).tp_basicsize as usize;
        let size = basic.max(HEADER) + n * std::mem::size_of::<*mut PyObject>();
        let size = (size + 7) & !7;
        let op = alloc::alloc_object(tp, size);
        set_var_size(op, n as isize);
        (*(op as *mut PyTupleObject)).ob_hash = -1;
        op
    }
}

pub fn empty_tuple() -> PyRef {
    static EMPTY: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    let p = EMPTY.load(std::sync::atomic::Ordering::Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    unsafe {
        let op = alloc_tuple_of(tuple_type(), 0);
        make_immortal(op);
        match EMPTY.compare_exchange(std::ptr::null_mut(), op, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire) {
            Ok(_) => PyRef::from_raw(op),
            Err(o) => PyRef::from_borrowed(o),
        }
    }
}

/// Create a tuple from owned references.
pub fn new_tuple(items: Vec<PyRef>) -> PyRef {
    if items.is_empty() {
        return empty_tuple();
    }
    unsafe {
        let n = items.len();
        let op = alloc_tuple_of(tuple_type(), n);
        let base = item_ptr(op);
        for (i, it) in items.into_iter().enumerate() {
            *base.add(i) = it.into_raw();
        }
        track_if_needed(op);
        PyRef::from_raw(op)
    }
}

pub fn new_tuple_from_slice(items: &[PyRef]) -> PyRef {
    new_tuple(items.to_vec())
}

/// Create from borrowed objects.
pub fn new_tuple_from_refs(items: &[&PyObject]) -> PyRef {
    unsafe { new_tuple_from_ptrs(std::slice::from_raw_parts(items.as_ptr() as *const *mut PyObject, items.len())) }
}

/// Create from borrowed raw pointers.
pub unsafe fn new_tuple_from_ptrs(items: &[*mut PyObject]) -> PyRef {
    if items.is_empty() {
        return empty_tuple();
    }
    unsafe {
        let op = alloc_tuple_of(tuple_type(), items.len());
        let base = item_ptr(op);
        for (i, &it) in items.iter().enumerate() {
            incref(it);
            *base.add(i) = it;
        }
        track_if_needed(op);
        PyRef::from_raw(op)
    }
}

/// GC-track a freshly filled exact tuple only if one of its items may be part of
/// a cycle (a tracked GC object); tuples of atomic values are never tracked.
pub unsafe fn track_if_needed(op: *mut PyObject) {
    unsafe {
        if !(*op).is_type(tuple_type()) {
            crate::gc::track(op);
            return;
        }
        let n = var_size(&*op) as usize;
        let base = item_ptr(op);
        for i in 0..n {
            let it = *base.add(i);
            if !it.is_null() && crate::gc::is_tracked(&*it) {
                crate::gc::track(op);
                return;
            }
        }
        crate::gc::untrack(op);
    }
}

/// Is `t` referenced only by the caller's cached pointer (so it can be refilled)?
#[inline]
pub fn tuple_reusable(t: &PyObject) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    t.ob_tid.load(Relaxed) == thread_id() && t.ob_ref_local.load(Relaxed) == 1 && t.ob_ref_shared.load(Relaxed) == 0
}
/// Refill a reusable result tuple with owned items (old items released after).
pub fn tuple_refill(t: &PyObject, items: Vec<PyRef>) {
    let mut old = Vec::with_capacity(items.len());
    unsafe {
        let base = item_ptr(t.as_ptr());
        for (i, it) in items.into_iter().enumerate() {
            old.push(*base.add(i));
            *base.add(i) = it.into_raw();
        }
        (*(t.as_ptr() as *mut PyTupleObject)).ob_hash = -1;
        track_if_needed(t.as_ptr());
    }
    for p in old {
        unsafe { xdecref(p) };
    }
}

pub fn tuple2(a: PyRef, b: PyRef) -> PyRef {
    new_tuple(vec![a, b])
}

#[inline(always)]
pub unsafe fn item_ptr(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).add(HEADER) as *mut *mut PyObject }
}

#[inline(always)]
pub fn tuple_len(t: &PyObject) -> usize {
    var_size(t) as usize
}

/// Borrowed item slice.
#[inline(always)]
pub fn tuple_items(t: &PyObject) -> &[&PyObject] {
    unsafe { std::slice::from_raw_parts(item_ptr(t.as_ptr()) as *const &PyObject, tuple_len(t)) }
}

#[inline(always)]
pub fn tuple_get(t: &PyObject, i: usize) -> &PyObject {
    tuple_items(t)[i]
}

pub fn tuple_to_vec(t: &PyObject) -> Vec<PyRef> {
    tuple_items(t).iter().map(|o| o.to_ref()).collect()
}

pub unsafe extern "C" fn tuple_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let n = var_size(&*op) as usize;
        let base = item_ptr(op);
        for i in 0..n {
            xdecref(*base.add(i));
        }
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn tuple_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let n = var_size(&*op) as usize;
        let base = item_ptr(op);
        let f = visit.unwrap();
        for i in 0..n {
            let it = *base.add(i);
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

pub fn tuple_hash(t: &PyObject) -> PyResult<isize> {
    let cached = unsafe { std::sync::atomic::AtomicIsize::from_ptr(&raw mut (*(t.as_ptr() as *mut PyTupleObject)).ob_hash) };
    let h0 = cached.load(std::sync::atomic::Ordering::Relaxed);
    // (jit6) subclasses too (namedtuples as dict keys): every allocation path of a
    // tuple type starts with -1 (alloc_tuple_of, typeobject generic_alloc, capi
    // _PyObject_NewVar), and the items never change
    if h0 != -1 {
        return Ok(h0);
    }
    let items = tuple_items(t);
    let mut h = crate::hash::TupleHasher::new(items.len());
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " while hashing a tuple")?;
    let r = (|| {
        for it in items {
            h.add(ab::hash(it)?);
        }
        Ok(())
    })();
    crate::interp::leave_recursive_call(ts);
    r?;
    let v = h.finish();
    cached.store(v, std::sync::atomic::Ordering::Relaxed);
    Ok(v)
}

// ================================================================== slots

unsafe extern "C" fn tuple_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let items = tuple_items(o);
        if items.is_empty() {
            return pystr::new_str("()").into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(pystr::new_str("(...)"));
            }
            let mut sb = pystr::StrBuilder::new();
            sb.push('(' as u32);
            let res = (|| {
                // a tuple from PyTuple_New may still hold NULL items: shown as <NULL>
                let raw = item_ptr(o.as_ptr());
                for i in 0..items.len() {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    let p = *raw.add(i);
                    if p.is_null() {
                        sb.push_str("<NULL>");
                    } else {
                        sb.push_obj(&*ab::repr(&*p)?);
                    }
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            if items.len() == 1 {
                sb.push(',' as u32);
            }
            sb.push(')' as u32);
            Ok(sb.finish())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn tuple_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { tuple_hash(&*o).unwrap_or(-1) }
}
unsafe extern "C" fn tuple_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { var_size(&*o) }
}
unsafe extern "C" fn tuple_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let items = tuple_items(&*o);
        if i < 0 || i as usize >= items.len() {
            return into_c(Err(err!(IndexError, "tuple index out of range")));
        }
        items[i as usize].to_ref().into_raw()
    }
}
fn tuple_getitem(o: &PyObject, k: &PyObject) -> PyResult {
    let items = tuple_items(o);
    if int::is_int_exact(k) && int::is_compact(k) {
        let mut i = int::compact_value(k) as isize;
        if i < 0 {
            i += items.len() as isize;
        }
        if i < 0 || i as usize >= items.len() {
            raise!(IndexError, "tuple index out of range");
        }
        return Ok(items[i as usize].to_ref());
    }
    if slice::is_slice(k) {
        let (mut start, mut stop, step) = slice::unpack(k)?;
        let n = slice::adjust_indices(items.len() as isize, &mut start, &mut stop, step);
        if n <= 0 {
            return Ok(empty_tuple());
        }
        if start == 0 && step == 1 && n as usize == items.len() && is_tuple_exact(o) {
            return Ok(o.to_ref());
        }
        if step == 1 {
            return Ok(new_tuple_from_refs(&items[start as usize..(start + n) as usize]));
        }
        return Ok(new_tuple(crate::types::list::slice_items(items, start, step, n)));
    }
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
        if i < 0 {
            i += items.len() as isize;
        }
        if i < 0 || i as usize >= items.len() {
            raise!(IndexError, "tuple index out of range");
        }
        return Ok(items[i as usize].to_ref());
    }
    raise!(TypeError, "tuple indices must be integers or slices, not {}", tname(k));
}
unsafe extern "C" fn tuple_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(tuple_getitem(&*o, &*k)) }
}
unsafe extern "C" fn tuple_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_tuple(b) || !is_tuple(a) {
            return none::not_implemented().to_ref().into_raw();
        }
        // (jit6, flagged: runtime fast path for mdp's tuple-keyed dicts) tuples are
        // immutable and kept alive by the caller, so their items are compared borrowed,
        // without the generic per-item closures and reference counting
        let (x, y) = (tuple_items(a), tuple_items(b));
        if (op == Py_EQ || op == Py_NE) && x.len() != y.len() {
            return bool_::new_bool(op == Py_NE).into_raw();
        }
        let n = x.len().min(y.len());
        for i in 0..n {
            let (xi, yi) = (&x[i], &y[i]);
            if xi.is(yi) {
                continue;
            }
            match ab::rich_compare_bool(xi, yi, Py_EQ) {
                Ok(true) => continue,
                Ok(false) => {
                    let r = match op {
                        Py_EQ => Ok(false),
                        Py_NE => Ok(true),
                        _ => ab::rich_compare_bool(xi, yi, op),
                    };
                    return into_c(r.map(bool_::new_bool));
                }
                Err(e) => return into_c(Err::<PyRef, _>(e)),
            }
        }
        let (la, lb) = (x.len(), y.len());
        let r = match op {
            Py_LT => la < lb,
            Py_LE => la <= lb,
            Py_EQ => la == lb,
            Py_NE => la != lb,
            Py_GT => la > lb,
            _ => la >= lb,
        };
        bool_::new_bool(r).into_raw()
    }
}
unsafe extern "C" fn tuple_contains(a: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let v = &*v;
        for it in tuple_items(&*a) {
            if it.is(v) {
                return 1;
            }
            match ab::rich_compare_bool(it, v, Py_EQ) {
                Ok(true) => return 1,
                Ok(false) => {}
                Err(_) => return -1,
            }
        }
        0
    }
}
unsafe extern "C" fn tuple_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_tuple(b) {
            return into_c(Err(err!(TypeError, "can only concatenate tuple (not \"{}\") to tuple", tname(b))));
        }
        if tuple_len(b) == 0 && is_tuple_exact(a) {
            return a.to_ref().into_raw();
        }
        if tuple_len(a) == 0 && is_tuple_exact(b) {
            return b.to_ref().into_raw();
        }
        let mut v: Vec<&PyObject> = Vec::with_capacity(tuple_len(a) + tuple_len(b));
        v.extend_from_slice(tuple_items(a));
        v.extend_from_slice(tuple_items(b));
        new_tuple_from_refs(&v).into_raw()
    }
}
unsafe extern "C" fn tuple_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        let items = tuple_items(a);
        if n <= 0 || items.is_empty() {
            return empty_tuple().into_raw();
        }
        if n == 1 && is_tuple_exact(a) {
            return a.to_ref().into_raw();
        }
        let total = match items.len().checked_mul(n as usize) {
            Some(t) if t <= (isize::MAX as usize) / 8 => t,
            _ => return into_c(Err(memory_error())),
        };
        let op = alloc_tuple_of(tuple_type(), total);
        if op.is_null() {
            return into_c(Err(memory_error()));
        }
        let base = item_ptr(op);
        let mut k = 0;
        for _ in 0..n {
            for it in items {
                incref(it.as_ptr());
                *base.add(k) = it.as_ptr();
                k += 1;
            }
        }
        track_if_needed(op);
        op
    }
}
/// Build a tuple of type `tp` (a tuple subclass) from owned items.
pub fn new_tuple_typed(tp: *mut PyTypeObject, items: Vec<PyRef>) -> PyRef {
    if tp == tuple_type() {
        return new_tuple(items);
    }
    unsafe {
        let op = alloc_tuple_of(tp, items.len());
        let base = item_ptr(op);
        for (i, it) in items.into_iter().enumerate() {
            *base.add(i) = it.into_raw();
        }
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
unsafe extern "C" fn tuple_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null()
                && crate::types::dict::dict_len(&*kw) > 0
                && (tp == tuple_type() || (*tp).tp_init == (*tuple_type()).tp_init)
            {
                raise!(TypeError, "tuple() takes no keyword arguments");
            }
            let a = tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "tuple expected at most 1 argument, got {}", a.len());
            }
            if tp == tuple_type() {
                if let Some(x) = a.first() {
                    if is_tuple_exact(x) {
                        return Ok(x.to_ref());
                    }
                }
            }
            let items = match a.first() {
                None => Vec::new(),
                Some(x) => ab::collect_iterable(x)?,
            };
            Ok(new_tuple_typed(tp, items))
        })();
        into_c(r)
    }
}

// ================================================================== methods

fn count_m(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0;
    for it in tuple_items(o) {
        if it.is(v) || ab::rich_compare_bool(it, v, Py_EQ)? {
            n += 1;
        }
    }
    Ok(int::new_int(n))
}
fn index_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    let items = tuple_items(o);
    let n = items.len() as isize;
    let norm = |v: &PyObject| -> PyResult<isize> {
        let mut x = slice::slice_index(v)?;
        if x < 0 {
            x += n;
            if x < 0 {
                x = 0;
            }
        }
        Ok(x)
    };
    let start = match args.get(1) {
        Some(v) => norm(v)?,
        None => 0,
    };
    let stop = match args.get(2) {
        Some(v) => norm(v)?.min(n),
        None => n,
    };
    let mut i = start;
    while i < stop {
        let it = items[i as usize];
        if it.is(args[0]) || ab::rich_compare_bool(it, args[0], Py_EQ)? {
            return Ok(int::new_int(i as i64));
        }
        i += 1;
    }
    raise!(ValueError, "tuple.index(x): x not in tuple");
}
fn getnewargs_m(o: &PyObject) -> PyResult {
    Ok(new_tuple(vec![new_tuple(tuple_to_vec(o))]))
}
fn getitem_m(o: &PyObject, k: &PyObject) -> PyResult {
    tuple_getitem(o, k)
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

// ================================================================== iterator

#[repr(C)]
struct TupleIter {
    ob_base: PyObject,
    index: isize,
    seq: *mut PyObject,
}
unsafe extern "C" fn tuple_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = alloc::alloc_object(&raw mut PyTupleIter_Type, std::mem::size_of::<TupleIter>());
        let it = &mut *(op as *mut TupleIter);
        it.seq = (*o).to_ref().into_raw();
        it.index = 0;
        crate::gc::track(op);
        op
    }
}
unsafe extern "C" fn tupleiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut TupleIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        let items = tuple_items(&*it.seq);
        if (it.index as usize) < items.len() {
            let v = items[it.index as usize].to_ref();
            it.index += 1;
            return v.into_raw();
        }
        let p = std::mem::replace(&mut it.seq, std::ptr::null_mut());
        decref(p);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn tupleiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut TupleIter)).seq);
        alloc::free_object(o);
    }
}
unsafe extern "C" fn tupleiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut TupleIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
fn ti(o: &PyObject) -> &mut TupleIter {
    unsafe { &mut *(o.as_ptr() as *mut TupleIter) }
}
fn tupleiter_len(o: &PyObject) -> PyResult {
    let it = ti(o);
    let n = if it.seq.is_null() { 0 } else { (tuple_len(unsafe { &*it.seq }) as isize - it.index).max(0) };
    Ok(int::new_int(n as i64))
}
fn tupleiter_reduce(o: &PyObject) -> PyResult {
    let it = ti(o);
    let iter_fn = crate::builtins::eval_builtin("iter")?;
    if it.seq.is_null() {
        return Ok(new_tuple(vec![iter_fn, new_tuple(vec![empty_tuple()])]));
    }
    Ok(new_tuple(vec![iter_fn, new_tuple(vec![unsafe { (*it.seq).to_ref() }]), int::new_int(it.index as i64)]))
}
fn tupleiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = ti(o);
    let mut idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        let n = tuple_len(unsafe { &*it.seq }) as isize;
        idx = idx.clamp(0, n);
        it.index = idx;
    }
    Ok(none::none_ref())
}

// ================================================================== registration

static mut TUPLE_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut TUPLE_MAP: PyMappingMethods = PyMappingMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *tuple_type();
        t.tp_name = c"tuple".as_ptr();
        t.tp_basicsize = HEADER as isize;
        t.tp_itemsize = 8;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_TUPLE_SUBCLASS | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(tuple_dealloc);
        t.tp_traverse = Some(tuple_traverse);
        t.tp_repr = Some(tuple_repr);
        t.tp_hash = Some(tuple_hash_slot);
        t.tp_richcompare = Some(tuple_richcompare);
        t.tp_iter = Some(tuple_iter);
        t.tp_new = Some(tuple_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_doc = c"tuple(iterable=(), /)\n--\n\nBuilt-in immutable sequence.\n\nIf no argument is given, the constructor returns an empty tuple.\nIf iterable is specified the tuple is initialized from iterable's items.\n\nIf the argument is a tuple, the return value is the same object.".as_ptr();
        let s = &mut *(&raw mut TUPLE_SEQ);
        s.sq_length = Some(tuple_len_slot);
        s.sq_concat = Some(tuple_concat);
        s.sq_repeat = Some(tuple_repeat);
        s.sq_item = Some(tuple_item);
        s.sq_contains = Some(tuple_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut TUPLE_MAP);
        m.mp_length = Some(tuple_len_slot);
        m.mp_subscript = Some(tuple_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::mdefs![
            ("__getitem__", O, getitem_m, "__getitem__($self, key, /)\n--\n\nReturn self[key]."; METH_COEXIST),
            ("__getnewargs__", NOARGS, getnewargs_m),
            ("index", FAST, index_m, "index($self, value, start=0, stop=sys.maxsize, /)\n--\n\nReturn first index of value.\n\nRaises ValueError if the value is not present."),
            ("count", O, count_m, "count($self, value, /)\n--\n\nReturn number of occurrences of value."),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
        ];

        let t = &mut *(&raw mut PyTupleIter_Type);
        t.tp_name = c"tuple_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<TupleIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(tupleiter_dealloc);
        t.tp_traverse = Some(tupleiter_traverse);
        t.tp_iter = Some(crate::types::list::self_iter);
        t.tp_iternext = Some(tupleiter_next);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, tupleiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, tupleiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, tupleiter_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(&raw mut PyTupleIter_Type)
}
