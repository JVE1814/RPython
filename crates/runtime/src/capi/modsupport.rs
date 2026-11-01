//! modsupport.h: adding objects to modules, PyABIInfo, PyState_* and version packing.

use super::*;
use crate::abstract_ as ab;
use crate::types::module as tmod;
use crate::types::{dict, int, str as pystr};

unsafe fn module_dict_of(m: *mut PyObject) -> PyResult<PyRef> {
    unsafe {
        if m.is_null() || !tmod::is_module(&*m) {
            return Err(type_error(&format!(
                "PyModule_AddObjectRef() first argument must be a module, not {}",
                if m.is_null() { "NULL".into() } else { tname(&*m) }
            )));
        }
        Ok(tmod::module_dict(&*m))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddObjectRef(m: *mut PyObject, name: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        let d = match module_dict_of(m) {
            Ok(d) => d,
            Err(_) => return -1,
        };
        if v.is_null() {
            if !err_occurred() {
                let _ = system_error("PyModule_AddObjectRef() must be called with an exception raised if value is NULL");
            }
            return -1;
        }
        match cstr_obj(name) {
            Ok(n) => ret_i(dict::set_item(&d, &pystr::intern(n), &*v)),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Add(m: *mut PyObject, name: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        let r = PyModule_AddObjectRef(m, name, v);
        xdecref(v);
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddObject(m: *mut PyObject, name: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        let r = PyModule_AddObjectRef(m, name, v);
        if r == 0 {
            decref(v);
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddIntConstant(m: *mut PyObject, name: *const c_char, v: std::ffi::c_long) -> c_int {
    unsafe { PyModule_Add(m, name, int::new_int(v).into_raw()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddStringConstant(m: *mut PyObject, name: *const c_char, v: *const c_char) -> c_int {
    unsafe {
        let s = match cstr_obj(v) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        };
        PyModule_Add(m, name, s)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddType(m: *mut PyObject, t: *mut PyTypeObject) -> c_int {
    unsafe {
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 && super::typeobj::PyType_Ready(t) < 0 {
            return -1;
        }
        let name = super::typeobj::_PyType_Name(t);
        PyModule_AddObjectRef(m, name, t as *mut PyObject)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_SetDocString(m: *mut PyObject, doc: *const c_char) -> c_int {
    unsafe {
        match cstr_obj(doc) {
            Ok(d) => ret_i(ab::setattr(&*m, crate::name!("__doc__"), &d)),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddFunctions(m: *mut PyObject, ml: *mut PyMethodDef) -> c_int {
    unsafe { ret_i(super::module::add_functions(&*m, ml)) }
}

// ---------------------------------------------------------------- PyABIInfo (3.15)

#[repr(C)]
pub struct PyABIInfo {
    pub abiinfo_major_version: u8,
    pub abiinfo_minor_version: u8,
    pub flags: u16,
    pub build_version: u32,
    pub abi_version: u32,
}
const ABI_STABLE: u16 = 1;
const ABI_GIL: u16 = 2;
const ABI_FREETHREADED: u16 = 4;
const ABI_INTERNAL: u16 = 8;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyABIInfo_Check(info: *mut PyABIInfo, module_name: *const c_char) -> c_int {
    unsafe {
        if info.is_null() {
            return 0;
        }
        let i = &*info;
        let name = if module_name.is_null() { "<unknown>".into() } else { cs(module_name).into_owned() };
        if i.abiinfo_major_version != 1 {
            let _ = crate::err!(ImportError, "module {} uses unsupported PyABIInfo version {}", name, i.abiinfo_major_version);
            return -1;
        }
        let ft = i.flags & (ABI_GIL | ABI_FREETHREADED);
        if ft == ABI_GIL {
            let _ = crate::err!(ImportError, "module {} was built for a GIL-enabled build of Python, not free-threaded", name);
            return -1;
        }
        if i.flags & ABI_STABLE == 0 {
            // version-specific ABI: must be 3.15
            if i.abi_version >> 16 != 0x030f {
                let _ = crate::err!(ImportError, "module {}: incompatible ABI version {:#x}", name, i.abi_version);
                return -1;
            }
        } else if i.abi_version > 0x030f02f0 {
            let _ = crate::err!(ImportError, "module {} needs a newer stable ABI ({:#x})", name, i.abi_version);
            return -1;
        }
        let _ = ABI_INTERNAL;
        0
    }
}

// ---------------------------------------------------------------- PyState_*

static STATE_MODULES: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_FindModule(def: *mut PyModuleDef) -> *mut PyObject {
    let g = STATE_MODULES.lock().unwrap();
    g.iter().find(|(d, _)| *d == def as usize).map_or(std::ptr::null_mut(), |(_, m)| *m as *mut PyObject)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_AddModule(m: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    unsafe {
        if def.is_null() {
            let _ = system_error("module definition is NULL");
            return -1;
        }
        if !(*def).m_slots.is_null() {
            let _ = system_error("PyState_AddModule called on module with slots");
            return -1;
        }
        let mut g = STATE_MODULES.lock().unwrap();
        g.retain(|(d, _)| *d != def as usize);
        incref(m);
        g.push((def as usize, m as usize));
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_RemoveModule(def: *mut PyModuleDef) -> c_int {
    let mut g = STATE_MODULES.lock().unwrap();
    let n = g.len();
    g.retain(|(d, _)| *d != def as usize);
    if g.len() == n {
        let _ = system_error("PyState_RemoveModule: module index out of bounds.");
        return -1;
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn Py_PACK_FULL_VERSION(x: c_int, y: c_int, z: c_int, level: c_int, serial: c_int) -> u32 {
    ((x as u32 & 0xff) << 24) | ((y as u32 & 0xff) << 16) | ((z as u32 & 0xff) << 8) | ((level as u32 & 0xf) << 4) | (serial as u32 & 0xf)
}
#[unsafe(no_mangle)]
pub extern "C" fn Py_PACK_VERSION(x: c_int, y: c_int) -> u32 {
    Py_PACK_FULL_VERSION(x, y, 0, 0, 0)
}
