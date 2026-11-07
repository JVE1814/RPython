//! `range` objects (arbitrary-size ints, with an i64 fast path), `range_iterator`
//! (machine ints) and `longrange_iterator` (Python ints).
use crate::abstract_ as ab;
use crate::abstract_::NbSlot;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, int, none, slice, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct RangeObject {
    ob_base: PyObject,
    start: *mut PyObject,
    stop: *mut PyObject,
    step: *mut PyObject,
    length: *mut PyObject,
    /// i64 fast representation (valid if `fast != 0`)
    fast: i64,
    fstart: i64,
    fstep: i64,
    flen: i64,
}

#[unsafe(no_mangle)]
pub static mut PyRange_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyRangeIter_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyLongRangeIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn range_type() -> *mut PyTypeObject {
    &raw mut PyRange_Type
}
pub fn is_range(o: &PyObject) -> bool {
    o.is_type(range_type())
}
fn r(o: &PyObject) -> &RangeObject {
    unsafe { &*(o.as_ptr() as *const RangeObject) }
}
fn obj(p: *mut PyObject) -> &'static PyObject {
    unsafe { &*p }
}

// ---- small int helpers (Python int semantics)
fn add(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, NbSlot::Add)
}
fn sub(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, NbSlot::Sub)
}
fn mul(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, NbSlot::Mul)
}
fn floordiv(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, NbSlot::FloorDiv)
}
fn rem(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, NbSlot::Mod)
}
fn lt(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    ab::rich_compare_bool(a, b, Py_LT)
}
fn eq(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    ab::rich_compare_bool(a, b, Py_EQ)
}
fn i64_of(o: &PyObject) -> Option<i64> {
    int::as_i64(o)
}

/// Length of range(start, stop, step) for Python ints.
fn compute_len(start: &PyObject, stop: &PyObject, step: &PyObject) -> PyResult {
    if let (Some(a), Some(b), Some(c)) = (i64_of(start), i64_of(stop), i64_of(step)) {
        let (a, b, c) = (a as i128, b as i128, c as i128);
        let n = if c > 0 {
            if a < b { (b - a - 1) / c + 1 } else { 0 }
        } else if a > b {
            (a - b - 1) / (-c) + 1
        } else {
            0
        };
        return Ok(int::new_int_i128(n));
    }
    let zero = int::new_int(0);
    let one = int::new_int(1);
    let (lo, hi, st) = if lt(&zero, step)? {
        (start.to_ref(), stop.to_ref(), step.to_ref())
    } else {
        (stop.to_ref(), start.to_ref(), ab::negative(step)?)
    };
    if !lt(&lo, &hi)? {
        return Ok(zero);
    }
    let diff = sub(&*sub(&hi, &lo)?, &one)?;
    add(&*floordiv(&diff, &st)?, &one)
}

pub fn make_range(start: PyRef, stop: PyRef, step: PyRef) -> PyResult {
    let length = compute_len(&start, &stop, &step)?;
    unsafe {
        let op = crate::alloc::alloc_object(range_type(), std::mem::size_of::<RangeObject>());
        let ro = &mut *(op as *mut RangeObject);
        if let (Some(a), Some(b), Some(c), Some(n)) = (i64_of(&start), i64_of(&stop), i64_of(&step), i64_of(&length)) {
            let _ = b;
            ro.fast = 1;
            ro.fstart = a;
            ro.fstep = c;
            ro.flen = n;
        }
        ro.start = start.into_raw();
        ro.stop = stop.into_raw();
        ro.step = step.into_raw();
        ro.length = length.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

pub fn new_range_i64(start: i64, stop: i64, step: i64) -> PyResult {
    make_range(int::new_int(start), int::new_int(stop), int::new_int(step))
}

unsafe extern "C" fn range_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "range() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            let (start, stop, step) = match a.len() {
                1 => (int::new_int(0), exact_index(a[0])?, int::new_int(1)),
                2 => (exact_index(a[0])?, exact_index(a[1])?, int::new_int(1)),
                3 => {
                    let s = exact_index(a[0])?;
                    let e = exact_index(a[1])?;
                    let st = exact_index(a[2])?;
                    if int::sign(&st) == 0 {
                        raise!(ValueError, "range() arg 3 must not be zero");
                    }
                    (s, e, st)
                }
                0 => raise!(TypeError, "range expected at least 1 argument, got 0"),
                n => raise!(TypeError, "range expected at most 3 arguments, got {}", n),
            };
            make_range(start, stop, step)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn range_dealloc(o: *mut PyObject) {
    unsafe {
        let ro = &*(o as *mut RangeObject);
        xdecref(ro.start);
        xdecref(ro.stop);
        xdecref(ro.step);
        xdecref(ro.length);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn range_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ro = r(&*o);
        let res = (|| -> PyResult {
            let s = ab::repr_string(obj(ro.start))?;
            let e = ab::repr_string(obj(ro.stop))?;
            if int::is_compact(obj(ro.step)) && int::compact_value(obj(ro.step)) == 1 {
                Ok(pystr::new_str(&format!("range({}, {})", s, e)))
            } else {
                Ok(pystr::new_str(&format!("range({}, {}, {})", s, e, ab::repr_string(obj(ro.step))?)))
            }
        })();
        into_c(res)
    }
}
unsafe extern "C" fn range_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        let ro = r(&*o);
        if ro.fast != 0 {
            return ro.flen as isize;
        }
        match i64_of(obj(ro.length)) {
            Some(n) => n as isize,
            None => {
                let _ = err!(OverflowError, "Python int too large to convert to C ssize_t");
                -1
            }
        }
    }
}
unsafe extern "C" fn range_bool(o: *mut PyObject) -> c_int {
    unsafe { (int::sign(obj(r(&*o).length)) != 0) as c_int }
}

/// i-th element (i already normalised and in range).
fn compute_item(ro: &RangeObject, i: &PyObject) -> PyResult {
    if ro.fast != 0 {
        if let Some(iv) = i64_of(i) {
            return Ok(int::new_int_i128(ro.fstart as i128 + iv as i128 * ro.fstep as i128));
        }
    }
    add(obj(ro.start), &*mul(i, obj(ro.step))?)
}
fn range_item_obj(o: &PyObject, k: &PyObject) -> PyResult {
    let ro = r(o);
    if ro.fast != 0 && int::is_int_exact(k) && int::is_compact(k) {
        let mut i = int::compact_value(k);
        if i < 0 {
            i += ro.flen;
        }
        if i < 0 || i >= ro.flen {
            raise!(IndexError, "range object index out of range");
        }
        return Ok(int::new_int_i128(ro.fstart as i128 + i as i128 * ro.fstep as i128));
    }
    let mut i = ab::index(k)?;
    let len = obj(ro.length);
    if int::sign(&i) < 0 {
        i = add(&i, len)?;
    }
    if int::sign(&i) < 0 || !lt(&i, len)? {
        raise!(IndexError, "range object index out of range");
    }
    compute_item(ro, &i)
}
fn range_subscript_rs(o: &PyObject, k: &PyObject) -> PyResult {
    if slice::is_slice(k) {
        let ro = r(o);
        let (s, e, st) = slice::long_indices(k, obj(ro.length))?;
        let start = add(obj(ro.start), &*mul(&s, obj(ro.step))?)?;
        let stop = add(obj(ro.start), &*mul(&e, obj(ro.step))?)?;
        let step = mul(&st, obj(ro.step))?;
        return make_range(start, stop, step);
    }
    if ab::has_index(k) {
        return range_item_obj(o, k);
    }
    raise!(TypeError, "range indices must be integers or slices, not {}", tname(k));
}
unsafe extern "C" fn range_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(range_subscript_rs(&*o, &*k)) }
}
unsafe extern "C" fn range_sq_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe { into_c(range_item_obj(&*o, &int::new_int(i as i64))) }
}

fn is_intlike(o: &PyObject) -> bool {
    int::is_int_exact(o) || bool_::is_bool(o)
}
/// Membership for exact ints/bools.
fn contains_long(ro: &RangeObject, ob: &PyObject) -> PyResult<bool> {
    if ro.fast != 0 {
        if let Some(v) = i64_of(ob) {
            if ro.flen == 0 {
                return Ok(false);
            }
            let (s, st) = (ro.fstart as i128, ro.fstep as i128);
            let last = s + (ro.flen as i128 - 1) * st;
            let v = v as i128;
            let inr = if st > 0 { s <= v && v <= last } else { last <= v && v <= s };
            return Ok(inr && (v - s) % st == 0);
        }
    }
    let zero = int::new_int(0);
    let (start, stop, step) = (obj(ro.start), obj(ro.stop), obj(ro.step));
    let inr = if lt(&zero, step)? {
        !lt(ob, start)? && lt(ob, stop)?
    } else {
        !lt(start, ob)? && lt(stop, ob)?
    };
    if !inr {
        return Ok(false);
    }
    let d = sub(ob, start)?;
    let m = rem(&d, step)?;
    Ok(int::sign(&m) == 0)
}
unsafe extern "C" fn range_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let (o, v) = (&*o, &*v);
        let res = if is_intlike(v) { contains_long(r(o), v) } else { ab::iter_search_contains(o, v) };
        match res {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn range_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_range(b) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let res = (|| -> PyResult<bool> {
            if a.is(b) {
                return Ok(true);
            }
            let (x, y) = (r(a), r(b));
            if !eq(obj(x.length), obj(y.length))? {
                return Ok(false);
            }
            if int::sign(obj(x.length)) == 0 {
                return Ok(true);
            }
            if !eq(obj(x.start), obj(y.start))? {
                return Ok(false);
            }
            if eq(obj(x.length), &int::new_int(1))? {
                return Ok(true);
            }
            eq(obj(x.step), obj(y.step))
        })();
        into_c(res.map(|e| bool_::new_bool(if op == Py_EQ { e } else { !e })))
    }
}
unsafe extern "C" fn range_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let ro = r(&*o);
        let res = (|| -> PyResult<isize> {
            let len = obj(ro.length);
            let t = if int::sign(len) == 0 {
                tuple::new_tuple(vec![len.to_ref(), none::none_ref(), none::none_ref()])
            } else if eq(len, &int::new_int(1))? {
                tuple::new_tuple(vec![len.to_ref(), obj(ro.start).to_ref(), none::none_ref()])
            } else {
                tuple::new_tuple(vec![len.to_ref(), obj(ro.start).to_ref(), obj(ro.step).to_ref()])
            };
            ab::hash(&t)
        })();
        res.unwrap_or(-1)
    }
}
fn count_m(o: &PyObject, v: &PyObject) -> PyResult {
    if is_intlike(v) {
        return Ok(int::new_int(contains_long(r(o), v)? as i64));
    }
    let it = ab::get_iter(o)?;
    let mut n = 0i64;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::rich_compare_bool(&x, v, Py_EQ)? {
            n += 1;
        }
    }
    Ok(int::new_int(n))
}
fn index_m(o: &PyObject, v: &PyObject) -> PyResult {
    let ro = r(o);
    if is_intlike(v) {
        if !contains_long(ro, v)? {
            raise!(ValueError, "{} is not in range", ab::repr_string(v)?);
        }
        let d = sub(v, obj(ro.start))?;
        return floordiv(&d, obj(ro.step));
    }
    let it = ab::get_iter(o)?;
    let mut n = 0i64;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::rich_compare_bool(&x, v, Py_EQ)? {
            return Ok(int::new_int(n));
        }
        n += 1;
    }
    raise!(ValueError, "sequence.index(x): x not in sequence");
}
fn reduce_m(o: &PyObject) -> PyResult {
    let ro = r(o);
    Ok(tuple::new_tuple(vec![
        type_ref(range_type()),
        tuple::new_tuple(vec![obj(ro.start).to_ref(), obj(ro.stop).to_ref(), obj(ro.step).to_ref()]),
    ]))
}
fn reversed_m(o: &PyObject) -> PyResult {
    let ro = r(o);
    if ro.fast != 0 {
        let n = ro.flen;
        if n == 0 {
            return Ok(new_fast_iter(0, 1, 0));
        }
        let last = ro.fstart as i128 + (n as i128 - 1) * ro.fstep as i128;
        let neg = -(ro.fstep as i128);
        if last >= i64::MIN as i128 && last <= i64::MAX as i128 && neg <= i64::MAX as i128 {
            return Ok(new_fast_iter(last as i64, neg as i64, n));
        }
    }
    let len = obj(ro.length);
    if int::sign(len) == 0 {
        return new_long_iter(int::new_int(0), int::new_int(1), int::new_int(0));
    }
    let lm1 = sub(len, &int::new_int(1))?;
    let last = add(obj(ro.start), &*mul(&lm1, obj(ro.step))?)?;
    let step = ab::negative(obj(ro.step))?;
    new_long_iter(last, step, len.to_ref())
}
fn start_g(o: &PyObject) -> PyResult {
    Ok(obj(r(o).start).to_ref())
}
fn stop_g(o: &PyObject) -> PyResult {
    Ok(obj(r(o).stop).to_ref())
}
fn step_g(o: &PyObject) -> PyResult {
    Ok(obj(r(o).step).to_ref())
}

// ================================================================== iterators

#[repr(C)]
struct FastIter {
    ob_base: PyObject,
    start: i64,
    step: i64,
    len: i64,
}
#[repr(C)]
struct LongIter {
    ob_base: PyObject,
    start: *mut PyObject,
    step: *mut PyObject,
    len: *mut PyObject,
}

fn new_fast_iter(start: i64, step: i64, len: i64) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PyRangeIter_Type, std::mem::size_of::<FastIter>());
        let it = &mut *(op as *mut FastIter);
        it.start = start;
        it.step = step;
        it.len = len;
        PyRef::from_raw(op)
    }
}
fn new_long_iter(start: PyRef, step: PyRef, len: PyRef) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PyLongRangeIter_Type, std::mem::size_of::<LongIter>());
        let it = &mut *(op as *mut LongIter);
        it.start = start.into_raw();
        it.step = step.into_raw();
        it.len = len.into_raw();
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn range_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ro = r(&*o);
        if ro.fast != 0 && i64_of(obj(ro.stop)).is_some() {
            return new_fast_iter(ro.fstart, ro.fstep, ro.flen).into_raw();
        }
        // the machine-int iterator only needs start, step and length to fit (the
        // stop of a pickled iterator's range can lie just outside)
        // -- but every value it yields must fit as well
        if let (Some(s), Some(st), Some(l)) = (i64_of(obj(ro.start)), i64_of(obj(ro.step)), i64_of(obj(ro.length))) {
            let last = s as i128 + (l.max(1) as i128 - 1) * st as i128;
            if last >= i64::MIN as i128 && last <= i64::MAX as i128 {
                return new_fast_iter(s, st, l).into_raw();
            }
        }
        into_c(new_long_iter(obj(ro.start).to_ref(), obj(ro.step).to_ref(), obj(ro.length).to_ref()))
    }
}
unsafe extern "C" fn fastiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut FastIter);
        if it.len > 0 {
            let v = it.start;
            it.start = it.start.wrapping_add(it.step);
            it.len -= 1;
            return int::new_int(v).into_raw();
        }
        std::ptr::null_mut()
    }
}
fn fi(o: &PyObject) -> &mut FastIter {
    unsafe { &mut *(o.as_ptr() as *mut FastIter) }
}
fn li(o: &PyObject) -> &mut LongIter {
    unsafe { &mut *(o.as_ptr() as *mut LongIter) }
}
fn fastiter_len(o: &PyObject) -> PyResult {
    Ok(int::new_int(fi(o).len))
}
fn fastiter_reduce(o: &PyObject) -> PyResult {
    let it = fi(o);
    let start = int::new_int(it.start);
    let stop = int::new_int_i128(it.start as i128 + it.len as i128 * it.step as i128);
    let step = int::new_int(it.step);
    let rng = make_range(start, stop, step)?;
    let iter_fn = crate::builtins::builtin("iter")?;
    Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![rng]), int::new_int(0)]))
}
fn fastiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if !int::is_int(state) {
        raise!(TypeError, "an integer is required");
    }
    let it = fi(o);
    let idx: i64 = match i64_of(state) {
        Some(v) => v.clamp(0, it.len),
        None => {
            if int::sign(state) < 0 { 0 } else { it.len }
        }
    };
    it.start = (it.start as i128 + idx as i128 * it.step as i128) as i64;
    it.len -= idx;
    Ok(none::none_ref())
}
unsafe extern "C" fn longiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut LongIter);
        if int::sign(&*it.len) <= 0 {
            return std::ptr::null_mut();
        }
        let res = (|| -> PyResult {
            let v = (*it.start).to_ref();
            let ns = add(&v, &*it.step)?;
            let nl = sub(&*it.len, &int::new_int(1))?;
            let old_s = std::mem::replace(&mut it.start, ns.into_raw());
            let old_l = std::mem::replace(&mut it.len, nl.into_raw());
            decref(old_s);
            decref(old_l);
            Ok(v)
        })();
        into_c(res)
    }
}
unsafe extern "C" fn longiter_dealloc(o: *mut PyObject) {
    unsafe {
        let it = &*(o as *mut LongIter);
        xdecref(it.start);
        xdecref(it.step);
        xdecref(it.len);
        crate::alloc::free_object(o);
    }
}
fn longiter_len(o: &PyObject) -> PyResult {
    Ok(obj(li(o).len).to_ref())
}
fn longiter_reduce(o: &PyObject) -> PyResult {
    let it = li(o);
    let start = obj(it.start).to_ref();
    let stop = add(&start, &*mul(obj(it.len), obj(it.step))?)?;
    let rng = make_range(start, stop, obj(it.step).to_ref())?;
    let iter_fn = crate::builtins::builtin("iter")?;
    Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![rng]), int::new_int(0)]))
}
fn longiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if !int::is_int(state) {
        raise!(TypeError, "an integer is required");
    }
    let it = li(o);
    let zero = int::new_int(0);
    let len = obj(it.len).to_ref();
    let idx = if lt(state, &zero)? {
        zero
    } else if lt(&len, state)? {
        len.clone()
    } else {
        state.to_ref()
    };
    let ns = add(obj(it.start), &*mul(&idx, obj(it.step))?)?;
    let nl = sub(&len, &idx)?;
    unsafe {
        let old_s = std::mem::replace(&mut it.start, ns.into_raw());
        let old_l = std::mem::replace(&mut it.len, nl.into_raw());
        decref(old_s);
        decref(old_l);
    }
    Ok(none::none_ref())
}

// ================================================================== registration

static mut RANGE_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut RANGE_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut RANGE_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *range_type();
        t.tp_name = c"range".as_ptr();
        t.tp_basicsize = std::mem::size_of::<RangeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(range_dealloc);
        t.tp_repr = Some(range_repr);
        t.tp_hash = Some(range_hash);
        t.tp_richcompare = Some(range_richcompare);
        t.tp_iter = Some(range_iter);
        t.tp_new = Some(range_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"range(stop) -> range object\nrange(start, stop[, step]) -> range object\n\nReturn an object that produces a sequence of integers from start (inclusive)\nto stop (exclusive) by step.  range(i, j) produces i, i+1, i+2, ..., j-1.\nstart defaults to 0, and stop is omitted!  range(4) produces 0, 1, 2, 3.\nThese are exactly the valid indices for a list of 4 elements.\nWhen step is given, it specifies the increment (or decrement).".as_ptr();
        let s = &mut *(&raw mut RANGE_SEQ);
        s.sq_length = Some(range_len);
        s.sq_item = Some(range_sq_item);
        s.sq_contains = Some(range_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut RANGE_MAP);
        m.mp_length = Some(range_len);
        m.mp_subscript = Some(range_subscript);
        t.tp_as_mapping = m;
        let n = &mut *(&raw mut RANGE_NUM);
        n.nb_bool = Some(range_bool);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("__reversed__", NOARGS, reversed_m, "Return a reverse iterator."),
            ("__reduce__", NOARGS, reduce_m),
            ("count", O, count_m, "rangeobject.count(value) -> integer -- return number of occurrences of value"),
            ("index", O, index_m, "rangeobject.index(value) -> integer -- return index of value.\nRaise ValueError if the value is not present."),
        ];
        t.tp_members = crate::member_table![
            ("start", Py_T_OBJECT, std::mem::offset_of!(RangeObject, start), Py_READONLY),
            ("stop", Py_T_OBJECT, std::mem::offset_of!(RangeObject, stop), Py_READONLY),
            ("step", Py_T_OBJECT, std::mem::offset_of!(RangeObject, step), Py_READONLY),
        ];
        let _ = (start_g as fn(&PyObject) -> PyResult, stop_g as fn(&PyObject) -> PyResult, step_g as fn(&PyObject) -> PyResult);

        let t = &mut *(&raw mut PyRangeIter_Type);
        t.tp_name = c"range_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<FastIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_iter = Some(crate::types::list::self_iter);
        t.tp_iternext = Some(fastiter_next);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, fastiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, fastiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, fastiter_setstate, "Set state information for unpickling."),
        ];
        let t = &mut *(&raw mut PyLongRangeIter_Type);
        t.tp_name = c"longrange_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<LongIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(longiter_dealloc);
        t.tp_iter = Some(crate::types::list::self_iter);
        t.tp_iternext = Some(longiter_next);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, longiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, longiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, longiter_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(range_type())?;
    type_ready(&raw mut PyRangeIter_Type)?;
    type_ready(&raw mut PyLongRangeIter_Type)
}

#[allow(dead_code)]
fn _unused(_: *mut c_void) {}

/// PyNumber_Index: an exact int (subclass instances such as bools are copied).
fn exact_index(o: &PyObject) -> PyResult {
    let i = ab::index(o)?;
    if int::is_int_exact(&i) {
        return Ok(i);
    }
    Ok(crate::types::int_methods::exact_copy(&i))
}
