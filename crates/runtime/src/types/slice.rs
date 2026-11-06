//! `slice` objects and the slice-index helpers used by sequences.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, int, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PySlice_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn slice_type() -> *mut PyTypeObject {
    &raw mut PySlice_Type
}
#[inline]
pub fn is_slice(o: &PyObject) -> bool {
    o.is_type(slice_type())
}
pub fn new_slice(start: &PyObject, stop: &PyObject, step: &PyObject) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(slice_type(), std::mem::size_of::<PySliceObject>());
        let s = &mut *(op as *mut PySliceObject);
        s.start = start.to_ref().into_raw();
        s.stop = stop.to_ref().into_raw();
        s.step = step.to_ref().into_raw();
        // only slices holding GC objects can be part of a cycle (perf2: the common
        // int/None slices are not tracked)
        if [start, stop, step].iter().any(|x| crate::alloc::type_is_gc(x.type_ptr())) {
            crate::gc::track(op);
        }
        PyRef::from_raw(op)
    }
}
pub fn parts(o: &PyObject) -> (&PyObject, &PyObject, &PyObject) {
    unsafe {
        let s = &*(o.as_ptr() as *const PySliceObject);
        (&*s.start, &*s.stop, &*s.step)
    }
}

/// `_PyEval_SliceIndex`: an index for slicing, clamped to the Py_ssize_t range.
pub fn slice_index(v: &PyObject) -> PyResult<isize> {
    if int::is_int_exact(v) && int::is_compact(v) {
        let x = int::compact_value(v);
        return Ok(x as isize);
    }
    if !ab::has_index(v) {
        raise!(TypeError, "slice indices must be integers or None or have an __index__ method");
    }
    let i = ab::index(v)?;
    Ok(match int::as_i64(&i) {
        Some(x) => x as isize,
        None => {
            if int::sign(&i) < 0 { isize::MIN } else { isize::MAX }
        }
    })
}

/// PySlice_Unpack: (start, stop, step) with None defaults, clamped.
pub fn unpack(o: &PyObject) -> PyResult<(isize, isize, isize)> {
    let (start, stop, step) = parts(o);
    let step = if none::is_none(step) {
        1
    } else {
        let s = slice_index(step)?;
        if s == 0 {
            raise!(ValueError, "slice step cannot be zero");
        }
        s.max(-isize::MAX)
    };
    let start = if none::is_none(start) {
        if step < 0 { isize::MAX } else { 0 }
    } else {
        slice_index(start)?
    };
    let stop = if none::is_none(stop) {
        if step < 0 { isize::MIN } else { isize::MAX }
    } else {
        slice_index(stop)?
    };
    Ok((start, stop, step))
}

/// PySlice_AdjustIndices: returns the slice length and adjusts start/stop.
pub fn adjust_indices(len: isize, start: &mut isize, stop: &mut isize, step: isize) -> isize {
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

/// Resolve a slice against a sequence length: (start, stop, step, slicelength).
pub fn indices(o: &PyObject, len: isize) -> PyResult<(isize, isize, isize, isize)> {
    let (mut start, mut stop, step) = unpack(o)?;
    let n = adjust_indices(len, &mut start, &mut stop, step);
    Ok((start, stop, step, n))
}

/// Slice indices for arbitrary-size integers (slice.indices / range slicing).
pub fn long_indices(o: &PyObject, length: &PyObject) -> PyResult<(PyRef, PyRef, PyRef)> {
    let (start, stop, step) = parts(o);
    let zero = int::new_int(0);
    let step = if none::is_none(step) {
        int::new_int(1)
    } else {
        let s = ab::index(step)?;
        if int::sign(&s) == 0 {
            raise!(ValueError, "slice step cannot be zero");
        }
        s
    };
    let neg = int::sign(&step) < 0;
    let lt = |a: &PyObject, b: &PyObject| ab::rich_compare_bool(a, b, Py_LT);
    let (lower, upper) = if neg {
        let lower = int::new_int(-1);
        let upper = ab::binary_op(length, &lower, ab::NbSlot::Add)?;
        (lower, upper)
    } else {
        (zero.clone(), length.to_ref())
    };
    let conv = |v: &PyObject, default: &PyRef| -> PyResult<PyRef> {
        if none::is_none(v) {
            return Ok(default.clone());
        }
        let mut x = ab::index(v)?;
        if int::sign(&x) < 0 {
            x = ab::binary_op(&x, length, ab::NbSlot::Add)?;
            if lt(&x, &lower)? {
                x = lower.clone();
            }
        } else if lt(&upper, &x)? {
            x = upper.clone();
        }
        Ok(x)
    };
    let start = conv(start, if neg { &upper } else { &lower })?;
    let stop = conv(stop, if neg { &lower } else { &upper })?;
    Ok((start, stop, step))
}

// ------------------------------------------------------------------ type

unsafe extern "C" fn slice_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b, c) = parts(&*o);
        into_c((|| Ok(pystr::new_str(&format!("slice({}, {}, {})", ab::repr_string(a)?, ab::repr_string(b)?, ab::repr_string(c)?))))())
    }
}
unsafe extern "C" fn slice_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "slice() takes no keyword arguments")));
        }
        let a = tuple::tuple_items(&*args);
        let n = none::none();
        match a.len() {
            1 => new_slice(n, a[0], n).into_raw(),
            2 => new_slice(a[0], a[1], n).into_raw(),
            3 => new_slice(a[0], a[1], a[2]).into_raw(),
            0 => into_c(Err(err!(TypeError, "slice expected at least 1 argument, got 0"))),
            k => into_c(Err(err!(TypeError, "slice expected at most 3 arguments, got {}", k))),
        }
    }
}
unsafe extern "C" fn slice_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let s = &*(o as *mut PySliceObject);
        xdecref(s.start);
        xdecref(s.stop);
        xdecref(s.step);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn slice_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = &*(o as *mut PySliceObject);
        let f = visit.unwrap();
        for p in [s.start, s.stop, s.step] {
            if !p.is_null() {
                let r = f(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
unsafe extern "C" fn slice_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let (a, b, c) = parts(&*o);
        let mut h = crate::hash::TupleHasher::new(3);
        for x in [a, b, c] {
            match ab::hash(x) {
                Ok(v) => h.add(v),
                Err(_) => return -1,
            }
        }
        h.finish()
    }
}
unsafe extern "C" fn slice_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_slice(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        if a.is(b) && (op == Py_EQ || op == Py_NE) {
            return bool_::new_bool(op == Py_EQ).into_raw();
        }
        let (x0, x1, x2) = parts(a);
        let (y0, y1, y2) = parts(b);
        let t1 = tuple::new_tuple(vec![x0.to_ref(), x1.to_ref(), x2.to_ref()]);
        let t2 = tuple::new_tuple(vec![y0.to_ref(), y1.to_ref(), y2.to_ref()]);
        into_c(ab::rich_compare(&t1, &t2, op))
    }
}
fn slice_start(o: &PyObject) -> PyResult {
    Ok(parts(o).0.to_ref())
}
fn slice_stop(o: &PyObject) -> PyResult {
    Ok(parts(o).1.to_ref())
}
fn slice_step(o: &PyObject) -> PyResult {
    Ok(parts(o).2.to_ref())
}
fn slice_indices_m(o: &PyObject, n: &PyObject) -> PyResult {
    let len = ab::index(n)?;
    if int::sign(&len) < 0 {
        raise!(ValueError, "length should not be negative");
    }
    let (a, b, c) = long_indices(o, &len)?;
    Ok(tuple::new_tuple(vec![a, b, c]))
}
fn slice_reduce_m(o: &PyObject) -> PyResult {
    let (a, b, c) = parts(o);
    Ok(tuple::new_tuple(vec![type_ref(slice_type()), tuple::new_tuple(vec![a.to_ref(), b.to_ref(), c.to_ref()])]))
}

pub fn init_types() {
    unsafe {
        let t = &mut *slice_type();
        t.tp_name = c"slice".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PySliceObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(slice_dealloc);
        t.tp_traverse = Some(slice_traverse);
        t.tp_repr = Some(slice_repr);
        t.tp_new = Some(slice_new);
        t.tp_hash = Some(slice_hash);
        t.tp_richcompare = Some(slice_richcompare);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"slice(stop)\nslice(start, stop[, step])\n\nCreate a slice object.\n\nThis is used for extended slicing (e.g. a[0:10:2]).".as_ptr();
        t.tp_members = crate::member_table![
            ("start", Py_T_OBJECT, std::mem::offset_of!(PySliceObject, start), Py_READONLY),
            ("stop", Py_T_OBJECT, std::mem::offset_of!(PySliceObject, stop), Py_READONLY),
            ("step", Py_T_OBJECT, std::mem::offset_of!(PySliceObject, step), Py_READONLY),
        ];
        let _ = (slice_start as fn(&PyObject) -> PyResult, slice_stop as fn(&PyObject) -> PyResult, slice_step as fn(&PyObject) -> PyResult);
        t.tp_methods = crate::mdefs![
            ("indices", O, slice_indices_m, "S.indices(len) -> (start, stop, stride)\n\nAssuming a sequence of length len, calculate the start and stop\nindices, and the stride length of the extended slice described by\nS. Out of bounds indices are clipped in a manner consistent with the\nhandling of normal slices."),
            ("__reduce__", NOARGS, slice_reduce_m, "Return state information for pickling."),
        ];
    }
}
