//! Minimal core slots for the fundamental builtin types (enough to boot);
//! the complete method suites live in the per-type `*_methods.rs` files.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::*;
use crate::raise;
use std::ffi::c_int;

fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}

fn cmp_result(c: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => c == Less,
        Py_LE => c != Greater,
        Py_EQ => c == Equal,
        Py_NE => c != Equal,
        Py_GT => c == Greater,
        _ => c != Less,
    }
}

// ============================================================== str
// (moved to str_methods.rs)

/// Compatibility re-export (dict slots live in dict_methods.rs).
pub use crate::types::dict_methods::dict_update_any;

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}


// ============================================================== registration

static mut STR_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut STR_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut FLOAT_NUM: PyNumberMethods = PyNumberMethods::ZERO;

fn iter_type(t: *mut PyTypeObject, name: &'static std::ffi::CStr, next: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject, dealloc: unsafe extern "C" fn(*mut PyObject), size: usize) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(dealloc);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(next);
        t.tp_getattro = Some(object_generic_getattro);
    }
}

pub fn init_types() {
    unsafe {
        // ---- str (slots and methods live in str_methods.rs)
        crate::types::str_methods::init_types();

    }
}
