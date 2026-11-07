//! pycapsule.h: PyCapsule objects.

use super::*;
use crate::abstract_ as ab;
use crate::types::str as pystr;
use std::ffi::c_void;

pub type PyCapsule_Destructor = Option<unsafe extern "C" fn(*mut PyObject)>;

#[repr(C)]
pub struct PyCapsule {
    pub ob_base: PyObject,
    pub pointer: *mut c_void,
    pub name: *const c_char,
    pub context: *mut c_void,
    pub destructor: PyCapsule_Destructor,
}

#[unsafe(no_mangle)]
pub static mut PyCapsule_Type: PyTypeObject = PyTypeObject::ZERO;

unsafe extern "C" fn capsule_dealloc(o: *mut PyObject) {
    unsafe {
        let c = &mut *(o as *mut PyCapsule);
        if let Some(d) = c.destructor {
            d(o);
        }
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn capsule_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = &*(o as *mut PyCapsule);
        let s = if c.name.is_null() {
            format!("<capsule object NULL at {:p}>", o)
        } else {
            format!("<capsule object \"{}\" at {:p}>", cs(c.name), o)
        };
        pystr::new_str(&s).into_raw()
    }
}

pub fn init_type() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let t = &mut *(&raw mut PyCapsule_Type);
        t.tp_name = c"PyCapsule".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyCapsule>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(capsule_dealloc);
        t.tp_repr = Some(capsule_repr);
        t.tp_doc = c"Capsule objects let you wrap a C \"void *\" pointer in a Python\nobject.".as_ptr();
        let _ = crate::types::typeobject::type_ready(t);
    });
}
pub fn capsule_type() -> *mut PyTypeObject {
    init_type();
    &raw mut PyCapsule_Type
}

unsafe fn name_matches(a: *const c_char, b: *const c_char) -> bool {
    unsafe {
        if a.is_null() || b.is_null() {
            return a == b;
        }
        libc::strcmp(a, b) == 0
    }
}
unsafe fn legal(o: *mut PyObject, what: &str) -> Option<&'static mut PyCapsule> {
    unsafe {
        if o.is_null() || !(*o).is_type(capsule_type()) || (*(o as *mut PyCapsule)).pointer.is_null() {
            let _ = value_error(&format!("{} called with invalid PyCapsule object", what));
            return None;
        }
        Some(&mut *(o as *mut PyCapsule))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_New(pointer: *mut c_void, name: *const c_char, destructor: PyCapsule_Destructor) -> *mut PyObject {
    unsafe {
        if pointer.is_null() {
            let _ = value_error("PyCapsule_New called with null pointer");
            return std::ptr::null_mut();
        }
        let op = crate::alloc::alloc_object(capsule_type(), std::mem::size_of::<PyCapsule>());
        let c = &mut *(op as *mut PyCapsule);
        c.pointer = pointer;
        c.name = name;
        c.context = std::ptr::null_mut();
        c.destructor = destructor;
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_IsValid(o: *mut PyObject, name: *const c_char) -> c_int {
    unsafe {
        (!o.is_null()
            && (*o).is_type(capsule_type())
            && !(*(o as *mut PyCapsule)).pointer.is_null()
            && name_matches((*(o as *mut PyCapsule)).name, name)) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetPointer(o: *mut PyObject, name: *const c_char) -> *mut c_void {
    unsafe {
        let Some(c) = legal(o, "PyCapsule_GetPointer") else { return std::ptr::null_mut() };
        if !name_matches(c.name, name) {
            let _ = value_error("PyCapsule_GetPointer called with incorrect name");
            return std::ptr::null_mut();
        }
        c.pointer
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetName(o: *mut PyObject) -> *const c_char {
    unsafe { legal(o, "PyCapsule_GetName").map_or(std::ptr::null(), |c| c.name) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetDestructor(o: *mut PyObject) -> PyCapsule_Destructor {
    unsafe { legal(o, "PyCapsule_GetDestructor").and_then(|c| c.destructor) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetContext(o: *mut PyObject) -> *mut c_void {
    unsafe { legal(o, "PyCapsule_GetContext").map_or(std::ptr::null_mut(), |c| c.context) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetPointer(o: *mut PyObject, p: *mut c_void) -> c_int {
    unsafe {
        if p.is_null() {
            let _ = value_error("PyCapsule_SetPointer called with null pointer");
            return -1;
        }
        let Some(c) = legal(o, "PyCapsule_SetPointer") else { return -1 };
        c.pointer = p;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetName(o: *mut PyObject, name: *const c_char) -> c_int {
    unsafe {
        let Some(c) = legal(o, "PyCapsule_SetName") else { return -1 };
        c.name = name;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetDestructor(o: *mut PyObject, d: PyCapsule_Destructor) -> c_int {
    unsafe {
        let Some(c) = legal(o, "PyCapsule_SetDestructor") else { return -1 };
        c.destructor = d;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetContext(o: *mut PyObject, ctx: *mut c_void) -> c_int {
    unsafe {
        let Some(c) = legal(o, "PyCapsule_SetContext") else { return -1 };
        c.context = ctx;
        0
    }
}
/// Import "pkg.module.attr" and return the capsule's pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_Import(name: *const c_char, _no_block: c_int) -> *mut c_void {
    unsafe {
        // "module.attr.attr...": only the module is imported, the rest are attribute
        // lookups (capi2)
        let full = CStr::from_ptr(name).to_bytes();
        let (modpart, rest) = match full.iter().position(|&b| b == b'.') {
            Some(i) => (&full[..i], Some(&full[i + 1..])),
            None => (full, None),
        };
        let display = String::from_utf8_lossy(full).into_owned();
        let r = (|| -> PyResult<PyRef> {
            let modname = std::str::from_utf8(modpart).ok().filter(|m| !m.is_empty());
            let module = match modname.map(crate::import::import_absolute) {
                Some(Ok(m)) => m,
                other => {
                    if let Some(Err(_)) = other {
                        clear_exc();
                    }
                    return Err(crate::err!(
                        ImportError,
                        "PyCapsule_Import could not import module \"{}\"",
                        String::from_utf8_lossy(modpart)
                    ));
                }
            };
            let Some(rest) = rest else {
                return Err(crate::err!(AttributeError, "PyCapsule_Import \"{}\" is not valid", display));
            };
            let mut obj = module;
            for comp in rest.split(|&b| b == b'.') {
                let attr = super::unicode::decode_utf8(comp, None)?;
                obj = ab::getattr(&obj, &attr)?;
            }
            Ok(obj)
        })();
        let Ok(o) = r else { return std::ptr::null_mut() };
        if PyCapsule_IsValid(o.as_ptr(), name) != 0 {
            return (*(o.as_ptr() as *mut PyCapsule)).pointer;
        }
        let _ = crate::err!(AttributeError, "PyCapsule_Import \"{}\" is not valid", display);
        std::ptr::null_mut()
    }
}
