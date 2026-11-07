//! (capi3) Self-tests of runtime internals, exposed by `_testinternalcapi` as `test_*`
//! functions (test_capi.test_misc collects every such attribute as a test case, and
//! `test_lock_*` ones as Test_PyLock cases). Each runs a check of our own primitive
//! (locks, critical sections, stop-the-world, once-initialisation, bit helpers, ...)
//! and raises AssertionError describing the first broken expectation.
//!
//! Also here: the debug-allocator probes `check_pyobject_*_is_freed` used by test_mem.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::none;
use std::ffi::c_void;

fn fail(what: &str) -> PyErr {
    crate::err!(AssertionError, "{}", what)
}

macro_rules! check {
    ($cond:expr, $($fmt:tt)*) => {
        if !$cond {
            return Err(fail(&format!($($fmt)*)));
        }
    };
}

// ---------------------------------------------------------------- debug allocator probes

fn expect_freed(op: *const PyObject) -> PyResult {
    check!(unsafe { crate::capi::memdebug::object_looks_freed(op) }, "object is not seen as freed");
    Ok(none::none_ref())
}

fn check_pyobject_null_is_freed(_m: &PyObject) -> PyResult {
    expect_freed(std::ptr::null())
}

/// A block from the object domain that was never initialised (the debug hooks fill it).
fn check_pyobject_uninitialized_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::capi::mem::PyObject_Malloc(std::mem::size_of::<PyObject>()) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        // the block is deliberately leaked: the process exits right after the probe
        expect_freed(op)
    }
}

/// A block too short to hold `ob_type`: the type slot lies in the trailing guard bytes.
fn check_pyobject_forbidden_bytes_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let n = std::mem::offset_of!(PyObject, ob_type);
        let op = crate::capi::mem::PyObject_Malloc(n) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        expect_freed(op)
    }
}

/// An object released the way an extension type with `tp_free = PyObject_Free` is.
fn check_pyobject_freed_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::capi::mem::PyObject_Malloc(std::mem::size_of::<PyObject>()) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        crate::capi::mem::PyObject_Init(op, crate::types::typeobject::object_type());
        crate::capi::mem::PyObject_Free(op as *mut c_void);
        expect_freed(op)
    }
}

pub fn methods() -> *mut PyMethodDef {
    crate::mdefs![
        ("check_pyobject_null_is_freed", NOARGS, check_pyobject_null_is_freed),
        ("check_pyobject_uninitialized_is_freed", NOARGS, check_pyobject_uninitialized_is_freed),
        ("check_pyobject_forbidden_bytes_is_freed", NOARGS, check_pyobject_forbidden_bytes_is_freed),
        ("check_pyobject_freed_is_freed", NOARGS, check_pyobject_freed_is_freed),
    ]
}
