//! moduleobject.h / modsupport.h module creation: PyModule_New*, PyModuleDef_Init,
//! single-phase (PyModule_Create2) and multi-phase (PyModule_FromDefAndSpec2 +
//! PyModule_ExecDef) initialisation, PEP 793 slot-based modules, module state.

use super::*;
use crate::abstract_ as ab;
use crate::types::module::{self as tmod, PyModuleObject};
use crate::types::{dict, none, str as pystr};
use std::ffi::c_void;

// slot ids (new numbering; old ids 1..4 are translated)
pub const MOD_CREATE: c_int = 84;
pub const MOD_EXEC: c_int = 85;
pub const MOD_MULTIPLE_INTERPRETERS: c_int = 86;
pub const MOD_GIL: c_int = 87;


unsafe fn md<'a>(m: *mut PyObject) -> &'a mut PyModuleObject {
    unsafe { &mut *(m as *mut PyModuleObject) }
}

pub(crate) unsafe fn module_token(m: *mut PyModuleObject) -> *mut c_void {
    unsafe {
        if !(*m).md_token.is_null() || (*m).md_from_slots {
            return (*m).md_token;
        }
        (*m).md_def as *mut c_void
    }
}

unsafe fn chk_module<'a>(m: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if m.is_null() || !tmod::is_module(&*m) {
            return Err(type_error(&format!(
                "expected a module object, got {}",
                if m.is_null() { "NULL".into() } else { tname(&*m) }
            )));
        }
        Ok(&*m)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_NewObject(name: *mut PyObject) -> *mut PyObject {
    unsafe { ret(tmod::new_module_obj(&*name)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_New(name: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => ret(tmod::new_module_obj(&n)),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetDict(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        if m.is_null() || !tmod::is_module(&*m) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let d = tmod::module_dict(&*m);
        let p = d.as_ptr();
        drop(d); // borrowed
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetNameObject(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(mo) = chk_module(m) else {
            clear_exc();
            let _ = bad_internal();
            return std::ptr::null_mut();
        };
        let d = tmod::module_dict(mo);
        match dict::get_item_rs(&d, "__name__") {
            Some(n) if pystr::is_str(&n) => n.into_raw(),
            _ => {
                let _ = system_error("nameless module");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetName(m: *mut PyObject) -> *const c_char {
    unsafe {
        let n = PyModule_GetNameObject(m);
        if n.is_null() {
            return std::ptr::null();
        }
        let p = super::unicode::PyUnicode_AsUTF8(n);
        decref(n); // still owned by the module dict
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetFilenameObject(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(mo) = chk_module(m) else {
            clear_exc();
            let _ = bad_internal();
            return std::ptr::null_mut();
        };
        let d = tmod::module_dict(mo);
        match dict::get_item_rs(&d, "__file__") {
            Some(n) if pystr::is_str(&n) => n.into_raw(),
            _ => {
                let _ = system_error("module filename missing");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetFilename(m: *mut PyObject) -> *const c_char {
    unsafe {
        let n = PyModule_GetFilenameObject(m);
        if n.is_null() {
            return std::ptr::null();
        }
        let p = super::unicode::PyUnicode_AsUTF8(n);
        decref(n);
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetDef(m: *mut PyObject) -> *mut PyModuleDef {
    unsafe {
        if m.is_null() || !tmod::is_module(&*m) {
            let _ = chk_module(m);
            return std::ptr::null_mut();
        }
        if md(m).md_from_slots {
            return std::ptr::null_mut();
        }
        md(m).md_def
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetState(m: *mut PyObject) -> *mut c_void {
    unsafe {
        if m.is_null() || !tmod::is_module(&*m) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        md(m).md_state
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetState_DuringGC(m: *mut PyObject) -> *mut c_void {
    unsafe { md(m).md_state }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetStateSize(m: *mut PyObject, result: *mut Py_ssize_t) -> c_int {
    unsafe {
        *result = 0;
        let Ok(_) = chk_module(m) else { return -1 };
        let d = md(m).md_def;
        if !d.is_null() {
            *result = (*d).m_size.max(-1);
        } else if let Some(n) = dict::get_item_rs(&tmod::module_dict(&*m), "__name__").filter(|n| pystr::is_str(n)) {
            // modules built into the runtime behave like single-phase modules
            let n = pystr::to_string_lossy(&n);
            if crate::import::builtin_module_names().contains(&n.as_str()) && (n == "sys" || n == "builtins" || crate::import::is_builtin_module_object(&*m, &n)) {
                *result = -1;
            }
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetToken(m: *mut PyObject, result: *mut *mut c_void) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        let Ok(_) = chk_module(m) else { return -1 };
        *result = module_token(m as *mut PyModuleObject);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetToken_DuringGC(m: *mut PyObject, result: *mut *mut c_void) -> c_int {
    unsafe {
        // no exception can be raised during GC: a non-module only gives -1
        if m.is_null() || !tmod::is_module(&*m) {
            *result = std::ptr::null_mut();
            return -1;
        }
        *result = module_token(m as *mut PyModuleObject);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Module_SetGIL(_m: *mut PyObject, _gil: *mut c_void) -> c_int {
    0 // no GIL in this runtime: the request is recorded nowhere and has no effect
}

// ---------------------------------------------------------------- module defs

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModuleDef_Init(def: *mut PyModuleDef) -> *mut PyObject {
    unsafe {
        let o = def as *mut PyObject;
        if (*o).ob_type.load(std::sync::atomic::Ordering::Relaxed).is_null() {
            (*o).ob_type.store(&raw mut tmod::PyModuleDef_Type, std::sync::atomic::Ordering::Relaxed);
        }
        if (*def).m_base.m_index == 0 {
            static NEXT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(1);
            (*def).m_base.m_index = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        make_immortal(o);
        o
    }
}

/// Add the def's methods (bound to the module) and docstring.
unsafe fn add_methods_and_doc(m: &PyObject, def: *mut PyModuleDef) -> PyResult<()> {
    unsafe {
        if !(*def).m_methods.is_null() {
            add_functions(m, (*def).m_methods)?;
        }
        if !(*def).m_doc.is_null() {
            let d = tmod::module_dict(m);
            dict::set_item_str(&d, "__doc__", &*(cstr_obj((*def).m_doc)?))?;
        }
        Ok(())
    }
}

pub(crate) unsafe fn add_functions(m: &PyObject, mut ml: *mut PyMethodDef) -> PyResult<()> {
    unsafe {
        let d = tmod::module_dict(m);
        let modname = dict::get_item_rs(&d, "__name__");
        while !(*ml).ml_name.is_null() {
            if (*ml).ml_flags & (METH_CLASS | METH_STATIC) != 0 {
                return Err(value_error("module functions cannot set METH_CLASS or METH_STATIC"));
            }
            let f = crate::types::cfunction::new_cfunction(ml, Some(m.to_ref()), modname.clone());
            if (*ml).ml_flags & METH_METHOD != 0 {
                return Err(system_error("module functions cannot set METH_METHOD"));
            }
            dict::set_item(&d, &pystr::intern(cstr_obj((*ml).ml_name)?), &f)?;
            ml = ml.add(1);
        }
        Ok(())
    }
}

unsafe fn alloc_state(m: &PyObject, size: Py_ssize_t) -> PyResult<()> {
    unsafe {
        if size > 0 && md(m.as_ptr()).md_state.is_null() {
            let p = crate::alloc::raw_calloc(size as usize);
            if p.is_null() {
                return Err(memory_error());
            }
            md(m.as_ptr()).md_state = p as *mut c_void;
        }
        Ok(())
    }
}

/// Single-phase init (PyModule_Create2).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Create2(def: *mut PyModuleDef, _apiver: c_int) -> *mut PyObject {
    unsafe {
        PyModuleDef_Init(def);
        ret((|| -> PyResult {
            let mut name = cs((*def).m_name).into_owned();
            // use the full dotted name the importer is loading, if any
            if let Some(ctx) = super::import::package_context() {
                if ctx.rsplit('.').next() == Some(name.as_str()) {
                    name = ctx;
                }
            }
            let m = tmod::new_module(&name)?;
            md(m.as_ptr()).md_def = def;
            alloc_state(&m, (*def).m_size)?;
            add_methods_and_doc(&m, def)?;
            if !(*def).m_slots.is_null() {
                return Err(system_error(&format!("module {} has slots, use PyModule_FromDefAndSpec", name)));
            }
            Ok(m)
        })())
    }
}

/// Multi-phase creation step.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_FromDefAndSpec2(def: *mut PyModuleDef, spec: *mut PyObject, _apiver: c_int) -> *mut PyObject {
    unsafe {
        PyModuleDef_Init(def);
        ret((|| -> PyResult {
            let name = ab::getattr_str(&*spec, "name")?;
            let mut create: Option<unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject> = None;
            let mut walker = super::slots::Walker::new(super::slots::Kind::Module, super::slots::Api::Def, format!("module {}", pystr::to_string_lossy(&name)));
            walker.walk_mod_slots((*def).m_slots, 0)?;
            for e in &walker.out {
                let value = e.value;
                match e.id as c_int {
                    MOD_CREATE => create = Some(std::mem::transmute(value)),
                    // slots that restate a PyModuleDef field must agree with it
                    id @ (100..=106 | 110) => {
                        let d = &*def;
                        let (field, slot_name): (usize, &str) = match id {
                            100 => (d.m_name as usize, "Py_mod_name"),
                            101 => (d.m_doc as usize, "Py_mod_doc"),
                            102 => (d.m_size as usize, "Py_mod_state_size"),
                            103 => (d.m_methods as usize, "Py_mod_methods"),
                            104 => (d.m_traverse.map_or(0, |f| f as usize), "Py_mod_state_traverse"),
                            105 => (d.m_clear.map_or(0, |f| f as usize), "Py_mod_state_clear"),
                            106 => (d.m_free.map_or(0, |f| f as usize), "Py_mod_state_free"),
                            _ => (def as usize, "Py_mod_token"),
                        };
                        if value as usize != field {
                            return Err(system_error(&format!(
                                "module {}: {} slot contradicts the PyModuleDef field",
                                pystr::to_string_lossy(&name),
                                slot_name
                            )));
                        }
                    }
                    _ => {}
                }
            }
            let m = match create {
                Some(c) => {
                    // PEP 793: a create slot of a slot-defined module receives no def
                    let from_slots = FROM_SLOTS.with(|c| c.replace(false));
                    let r = c(spec, if from_slots { std::ptr::null_mut() } else { def });
                    if r.is_null() && !err_occurred() {
                        return Err(system_error(&format!("creation of module {} failed without setting an exception", pystr::to_string_lossy(&name))));
                    }
                    if !r.is_null() && err_occurred() {
                        let cause = take_exc();
                        decref(r);
                        let e = system_error(&format!("creation of module {} raised unreported exception", pystr::to_string_lossy(&name)));
                        if let (Some(c), Some(ex)) = (cause, take_exc()) {
                            crate::types::exceptions::set_exc_cause(&ex, Some(&c));
                            set_exc(ex);
                        }
                        return Err(e);
                    }
                    let r = from_c(r)?;
                    if tmod::is_module(&r) {
                        if md(r.as_ptr()).md_def.is_null() {
                            md(r.as_ptr()).md_def = def;
                        }
                    } else if (*def).m_size > 0 || (*def).m_traverse.is_some() || (*def).m_clear.is_some() || (*def).m_free.is_some() {
                        return Err(system_error(&format!(
                            "module {} is not a module object, but requests module state",
                            pystr::to_string_lossy(&name)
                        )));
                    }
                    r
                }
                None => {
                    let m = tmod::new_module_obj(&name)?;
                    md(m.as_ptr()).md_def = def;
                    m
                }
            };
            if tmod::is_module(&m) {
                alloc_state(&m, (*def).m_size)?;
            }
            // functions and doc are added to non-module objects through setattr
            if tmod::is_module(&m) {
                add_methods_and_doc(&m, def)?;
            } else if !(*def).m_methods.is_null() {
                let mut ml = (*def).m_methods;
                while !(*ml).ml_name.is_null() {
                    let f = crate::types::cfunction::new_cfunction(ml, Some(m.clone()), Some(name.clone()));
                    ab::setattr(&m, &*(cstr_obj((*ml).ml_name)?), &f)?;
                    ml = ml.add(1);
                }
            }
            Ok(m)
        })())
    }
}

/// Run the Py_mod_exec slots of `def` on `m`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_ExecDef(m: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            if tmod::is_module(&*m) {
                alloc_state(&*m, (*def).m_size)?;
            }
            let mut walker = super::slots::Walker::new(super::slots::Kind::Module, super::slots::Api::Def, "module".into());
            walker.walk_mod_slots((*def).m_slots, 0)?;
            for e in &walker.out {
                if e.id as c_int == MOD_EXEC {
                    let f: unsafe extern "C" fn(*mut PyObject) -> c_int = std::mem::transmute(e.value);
                    let rc = f(m);
                    if rc != 0 {
                        if !err_occurred() {
                            let n = PyModule_GetNameObject(m);
                            let nm = if n.is_null() { clear_exc(); "?".into() } else { pystr::to_string_lossy(&PyRef::from_raw(n)) };
                            return Err(system_error(&format!("execution of module {} failed without setting an exception", nm)));
                        }
                        return Err(PyErr);
                    }
                    if err_occurred() {
                        let e = take_exc();
                        let n = PyModule_GetNameObject(m);
                        let nm = if n.is_null() { clear_exc(); "?".into() } else { pystr::to_string_lossy(&PyRef::from_raw(n)) };
                        let _ = system_error(&format!("execution of module {} raised unreported exception", nm));
                        if let (Some(c), Some(ne)) = (e, take_exc()) {
                            crate::types::exceptions::set_exc_cause(&ne, Some(&c));
                            set_exc(ne);
                        }
                        return Err(PyErr);
                    }
                }
            }
            Ok(())
        })();
        ret_i(r)
    }
}

/// PyModule_Exec (3.15): run exec slots of a module created from slots or a def.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Exec(m: *mut PyObject) -> c_int {
    unsafe {
        if chk_module(m).is_err() {
            return -1;
        }
        let def = md(m).md_def;
        if def.is_null() {
            return 0;
        }
        PyModule_ExecDef(m, def)
    }
}
/// PEP 793: create a module from a PySlot array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_FromSlotsAndSpec(slots: *const super::typeobj::PySlot, spec: *mut PyObject) -> *mut PyObject {
    unsafe {
        // the spec's name is looked up first (a spec without one is an AttributeError)
        let name = match ab::getattr_str(&*spec, "name") {
            Ok(n) => n,
            Err(_) => return std::ptr::null_mut(),
        };
        let mut walker = super::slots::Walker::new(super::slots::Kind::Module, super::slots::Api::Slots, format!("module {}", pystr::to_string_lossy(&name)));
        if walker.walk_slots(slots, 0).is_err() {
            return std::ptr::null_mut();
        }
        if walker.out.is_empty() {
            let _ = system_error(&format!("module {}: empty slot array", pystr::to_string_lossy(&name)));
            return std::ptr::null_mut();
        }
        // synthesise a (leaked) PyModuleDef so the module object's dealloc/traverse work;
        // the module does not expose it (PyModule_GetDef gives NULL)
        let def: *mut PyModuleDef = Box::into_raw(Box::new(std::mem::zeroed::<PyModuleDef>()));
        let mut mslots: Vec<PyModuleDef_Slot> = Vec::new();
        let mut token: *mut c_void = std::ptr::null_mut();
        let mut abi: Vec<*mut c_void> = Vec::new();
        for e in &walker.out {
            let p = e.value;
            match e.id {
                84..=87 => mslots.push(PyModuleDef_Slot { slot: e.id as c_int, value: p }),
                100 => (*def).m_name = p as *const c_char,
                101 => (*def).m_doc = p as *const c_char,
                102 => (*def).m_size = p as Py_ssize_t,
                103 => (*def).m_methods = p as *mut PyMethodDef,
                104 => (*def).m_traverse = std::mem::transmute(p),
                105 => (*def).m_clear = std::mem::transmute(p),
                106 => (*def).m_free = std::mem::transmute(p),
                109 => abi.push(p),
                110 => token = p,
                _ => {}
            }
        }
        for p in abi {
            if super::modsupport::PyABIInfo_Check(p as *mut super::modsupport::PyABIInfo, (*def).m_name) < 0 {
                return std::ptr::null_mut();
            }
        }
        mslots.push(PyModuleDef_Slot { slot: 0, value: std::ptr::null_mut() });
        (*def).m_slots = Box::leak(mslots.into_boxed_slice()).as_mut_ptr();
        FROM_SLOTS.with(|c| c.set(true));
        let m = PyModule_FromDefAndSpec2(def, spec, 1013);
        FROM_SLOTS.with(|c| c.set(false));
        if !m.is_null() && tmod::is_module(&*m) && md(m).md_def == def {
            md(m).md_token = token;
            md(m).md_from_slots = true;
        }
        m
    }
}

#[allow(dead_code)]
fn _unused() -> PyRef {
    none::none_ref()
}

thread_local! {
    /// Set while PyModule_FromSlotsAndSpec builds its module from a synthesized def.
    static FROM_SLOTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
