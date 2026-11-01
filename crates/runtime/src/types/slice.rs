//! `slice` objects.
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
#[unsafe(no_mangle)]
pub static mut PySlice_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn slice_type() -> *mut PyTypeObject {
    &raw mut PySlice_Type
}
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
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
pub fn parts(o: &PyObject) -> (&PyObject, &PyObject, &PyObject) {
    unsafe {
        let s = &*(o.as_ptr() as *const PySliceObject);
        (&*s.start, &*s.stop, &*s.step)
    }
}
/// Resolve a slice against a sequence length: (start, stop, step, slicelength).
pub fn indices(o: &PyObject, len: isize) -> PyResult<(isize, isize, isize, isize)> {
    let (start, stop, step) = parts(o);
    let none = crate::types::none::none();
    let step = if step.is(none) {
        1
    } else {
        let s = crate::abstract_::index_as_ssize(step, None)?;
        if s == 0 {
            crate::raise!(ValueError, "slice step cannot be zero");
        }
        s.max(-isize::MAX)
    };
    let (lower, upper) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let conv = |v: &PyObject, default: isize| -> PyResult<isize> {
        if v.is(none) {
            return Ok(default);
        }
        let mut x = crate::abstract_::index_as_ssize(v, None)?;
        if x < 0 {
            x += len;
            if x < lower {
                x = lower;
            }
        } else if x > upper {
            x = upper;
        }
        Ok(x)
    };
    let start = conv(start, if step < 0 { upper } else { lower })?;
    let stop = conv(stop, if step < 0 { lower } else { upper })?;
    let slicelen = if step < 0 {
        if stop < start { (start - stop - 1) / (-step) + 1 } else { 0 }
    } else if start < stop {
        (stop - start - 1) / step + 1
    } else {
        0
    };
    Ok((start, stop, step, slicelen))
}
