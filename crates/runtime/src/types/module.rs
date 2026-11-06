//! Module objects.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, none, str as pystr};
use crate::{name, raise};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct PyModuleObject {
    pub ob_base: PyObject,
    pub md_dict: *mut PyObject,
    pub md_def: *mut PyModuleDef,
    pub md_state: *mut c_void,
    pub md_weaklist: *mut PyObject,
    pub md_name: *mut PyObject,
    pub md_token: *mut c_void,
}

#[unsafe(no_mangle)]
pub static mut PyModule_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyModuleDef_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn module_type() -> *mut PyTypeObject {
    &raw mut PyModule_Type
}
pub fn is_module(o: &PyObject) -> bool {
    o.is_type(module_type()) || is_subtype(o.type_ptr(), module_type())
}
pub fn m(o: &PyObject) -> &mut PyModuleObject {
    unsafe { &mut *(o.as_ptr() as *mut PyModuleObject) }
}
pub fn module_dict(o: &PyObject) -> PyRef {
    let md = m(o);
    if md.md_dict.is_null() {
        md.md_dict = dict::new_dict().into_raw();
    }
    unsafe { PyRef::from_borrowed(md.md_dict) }
}

fn init_dict(d: &PyObject, nm: &PyObject, doc: Option<&PyObject>) -> PyResult<()> {
    dict::set_item(d, name!("__name__"), nm)?;
    dict::set_item(d, name!("__doc__"), doc.unwrap_or(none::none()))?;
    dict::set_item(d, name!("__package__"), none::none())?;
    dict::set_item(d, name!("__loader__"), none::none())?;
    dict::set_item(d, name!("__spec__"), none::none())?;
    Ok(())
}

pub fn new_module(nm: &str) -> PyResult {
    new_module_obj(&pystr::new_str(nm))
}

pub fn new_module_obj(nm: &PyObject) -> PyResult {
    unsafe {
        let op = generic_alloc(module_type(), 0);
        let md = &mut *(op as *mut PyModuleObject);
        let d = dict::new_dict();
        init_dict(&d, nm, None)?;
        md.md_dict = d.into_raw();
        md.md_name = nm.to_ref().into_raw();
        Ok(PyRef::from_raw(op))
    }
}

pub fn add_object(module: &PyObject, nm: &str, v: &PyObject) -> PyResult<()> {
    dict::set_item_str(&module_dict(module), nm, v)
}

pub fn get_attr_str(module: &PyObject, nm: &str) -> Option<PyRef> {
    dict::get_item_rs(&module_dict(module), nm)
}

unsafe extern "C" fn module_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let md = &mut *(op as *mut PyModuleObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut md.md_weaklist);
        if !md.md_def.is_null() {
            if let Some(free) = (*md.md_def).m_free {
                free(op as *mut c_void);
            }
        }
        if !md.md_state.is_null() {
            crate::alloc::raw_free(md.md_state as *mut u8);
        }
        xdecref(md.md_dict);
        xdecref(md.md_name);
        let tp = (*op).type_ptr();
        generic_free_and_type_decref(op, tp);
    }
}

unsafe extern "C" fn module_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let md = &*(op as *mut PyModuleObject);
        if !md.md_def.is_null() && (*md.md_def).m_size >= 0 && !md.md_state.is_null() {
            if let Some(t) = (*md.md_def).m_traverse {
                let r = t(op, visit, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if !md.md_dict.is_null() {
            return visit.unwrap()(md.md_dict, arg);
        }
        0
    }
}

unsafe extern "C" fn module_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let md = &mut *(op as *mut PyModuleObject);
        if !md.md_def.is_null() {
            if let Some(c) = (*md.md_def).m_clear {
                c(op);
            }
        }
        let d = std::mem::replace(&mut md.md_dict, std::ptr::null_mut());
        xdecref(d);
        0
    }
}

unsafe extern "C" fn module_getattro(op: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let n = &*nm;
        match generic_getattr_opt(o, n, None) {
            Ok(Some(v)) => {
                if crate::types::lazyimport::is_lazy(&v) {
                    // a lazy import bound in the module: reify and rebind
                    return into_c(crate::types::lazyimport::reify_in_dict(&module_dict(o), n, &v));
                }
                return v.into_raw();
            }
            Ok(None) => {}
            Err(_) => return std::ptr::null_mut(),
        }
        let d = module_dict(o);
        match crate::types::lazyimport::pending_submodule(o, n) {
            Ok(Some(v)) => return v.into_raw(),
            Ok(None) => {}
            Err(_) => return std::ptr::null_mut(),
        }
        if let Some(ga) = dict::get_item_str(&d, name!("__getattr__")) {
            return into_c(ab::call(&ga, &[n]));
        }
        let modname = dict::get_item_str(&d, name!("__name__"));
        let msg = match modname {
            Some(mn) if pystr::is_str(&mn) => match crate::import::module_attribute_error(&d, &mn, n) {
                Ok(m) => m,
                Err(_) => return std::ptr::null_mut(),
            },
            _ => format!("module has no attribute '{}'", pystr::to_string_lossy(n)),
        };
        let _ = crate::types::exceptions::attribute_error_with(&msg, o, n);
        std::ptr::null_mut()
    }
}

unsafe extern "C" fn module_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(crate::import::module_repr(&*op)) }
}

unsafe extern "C" fn module_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("module", &["name", "doc"], 0, 1);
        let r = (|| -> PyResult<()> {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [nm, doc] = crate::args::parse::<2>(&P, &refs, kwn.as_deref())?;
            let nm = nm.unwrap();
            if !pystr::is_str(nm) {
                raise!(TypeError, "module.__init__() argument 'name' must be str, not {}", tname(nm));
            }
            let d = module_dict(&*op);
            init_dict(&d, nm, doc)?;
            let md = m(&*op);
            let old = std::mem::replace(&mut md.md_name, nm.to_ref().into_raw());
            xdecref(old);
            Ok(())
        })();
        int_into_c(r)
    }
}

fn get_dict(o: &PyObject) -> PyResult {
    Ok(module_dict(o))
}
fn module_dir(o: &PyObject) -> PyResult {
    // through the attribute: a subclass may shadow __dict__
    let d = ab::getattr(o, name!("__dict__"))?;
    if !dict::is_dict(&d) {
        let nm = ab::getattr_opt(o, name!("__name__")).ok().flatten().map(|n| pystr::to_string_lossy(&n)).unwrap_or_else(|| "?".into());
        raise!(TypeError, "{}.__dict__ is not a dictionary", nm);
    }
    if let Some(f) = dict::get_item_str(&d, name!("__dir__")) {
        return ab::call(&f, &[]);
    }
    Ok(crate::types::list::new_list(dict::keys_vec(&d)))
}
fn get_annotations(o: &PyObject) -> PyResult {
    crate::modules::annotations::module_get_annotations(o)
}
fn set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    crate::modules::annotations::module_set_annotations(o, v)
}
fn get_annotate(o: &PyObject) -> PyResult {
    let d = module_dict(o);
    Ok(dict::get_item_str(&d, name!("__annotate__")).unwrap_or_else(none::none_ref))
}
fn set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = module_dict(o);
    match v {
        None => raise!(TypeError, "cannot delete __annotate__ attribute"),
        Some(v) => {
            if !v.is(none::none()) && !ab::is_callable(v) {
                raise!(TypeError, "__annotate__ must be callable or None");
            }
            dict::set_item(&d, name!("__annotate__"), v)?;
            if !v.is(none::none()) {
                dict::del_item(&d, name!("__annotations__"))?;
            }
            Ok(())
        }
    }
}

pub fn init_types() {
    unsafe {
        let t = &mut *module_type();
        t.tp_name = c"module".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyModuleObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(module_dealloc);
        t.tp_traverse = Some(module_traverse);
        t.tp_clear = Some(module_clear);
        t.tp_getattro = Some(module_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_repr = Some(module_repr);
        t.tp_init = Some(module_init);
        t.tp_new = Some(crate::types::object_type::generic_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_dictoffset = std::mem::offset_of!(PyModuleObject, md_dict) as isize;
        t.tp_weaklistoffset = std::mem::offset_of!(PyModuleObject, md_weaklist) as isize;
        t.tp_doc = c"module(name, doc=None)\n--\n\nCreate a module object.\n\nThe name must be a string; the optional doc argument can have any type.".as_ptr();
        t.tp_getset = crate::getset_table![
            ("__dict__", get_dict),
            ("__annotations__", get_annotations, set_annotations),
            ("__annotate__", get_annotate, set_annotate),
        ];
        t.tp_methods = crate::mdefs![("__dir__", NOARGS, module_dir)];
        let d = &mut *(&raw mut PyModuleDef_Type);
        d.tp_name = c"moduledef".as_ptr();
        d.tp_basicsize = std::mem::size_of::<PyModuleDef>() as isize;
        d.tp_flags = Py_TPFLAGS_DEFAULT;
    }
}
