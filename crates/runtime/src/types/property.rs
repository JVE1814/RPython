//! `property` descriptor.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, exceptions, none};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct PropertyObject {
    ob_base: PyObject,
    fget: *mut PyObject,
    fset: *mut PyObject,
    fdel: *mut PyObject,
    doc: *mut PyObject,
    name: *mut PyObject,
    getter_doc: c_int,
}

#[unsafe(no_mangle)]
pub static mut PyProperty_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn property_type() -> *mut PyTypeObject {
    &raw mut PyProperty_Type
}
pub fn is_property(o: &PyObject) -> bool {
    o.is_type(property_type()) || is_subtype(o.type_ptr(), property_type())
}
fn po(o: &PyObject) -> &mut PropertyObject {
    unsafe { &mut *(o.as_ptr() as *mut PropertyObject) }
}
fn opt(p: *mut PyObject) -> Option<&'static PyObject> {
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}
fn setfield(slot: &mut *mut PyObject, v: Option<&PyObject>) {
    let old = std::mem::replace(slot, v.map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw()));
    unsafe { xdecref(old) };
}

fn property_error(p: &PyObject, msg: &str, obj: &PyObject) -> PyErr {
    let qual = type_qualname(obj.ty());
    let pr = po(p);
    if let Some(n) = opt(pr.name) {
        let nr = match ab::repr_string(n) {
            Ok(s) => s,
            Err(e) => return e,
        };
        return new_err(exceptions::AttributeError(), &format!("property {} of '{}' object {}", nr, qual, msg));
    }
    new_err(exceptions::AttributeError(), &format!("property of '{}' object {}", qual, msg))
}

unsafe extern "C" fn prop_descr_get(o: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if obj.is_null() || none::is_none(&*obj) {
            return o.to_ref().into_raw();
        }
        match opt(po(o).fget) {
            None => into_c(Err(property_error(o, "has no getter", &*obj))),
            Some(f) => into_c(ab::call(f, &[&*obj])),
        }
    }
}
unsafe extern "C" fn prop_descr_set(o: *mut PyObject, obj: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        let p = po(o);
        let f = if v.is_null() { opt(p.fdel) } else { opt(p.fset) };
        match f {
            None => int_into_c(Err(property_error(o, if v.is_null() { "has no deleter" } else { "has no setter" }, &*obj))),
            Some(f) => {
                let r = if v.is_null() { ab::call(f, &[&*obj]) } else { ab::call(f, &[&*obj, &*v]) };
                int_into_c(r.map(|_| ()))
            }
        }
    }
}

fn init_fields(o: &PyObject, fget: Option<&PyObject>, fset: Option<&PyObject>, fdel: Option<&PyObject>, doc: Option<&PyObject>) -> PyResult<()> {
    let nn = |x: Option<&PyObject>| x.filter(|v| !none::is_none(v)).map(|v| v.to_ref());
    let (fget, fset, fdel) = (nn(fget), nn(fset), nn(fdel));
    {
        let p = po(o);
        setfield(&mut p.fget, fget.as_deref());
        setfield(&mut p.fset, fset.as_deref());
        setfield(&mut p.fdel, fdel.as_deref());
        setfield(&mut p.doc, None);
        setfield(&mut p.name, None);
        p.getter_doc = 0;
    }
    let mut prop_doc: Option<PyRef> = None;
    if let Some(d) = doc.filter(|d| !none::is_none(d)) {
        prop_doc = Some(d.to_ref());
    } else if let Some(g) = fget.as_deref() {
        let d = ab::getattr_opt(g, crate::name!("__doc__"))?;
        if let Some(d) = d.filter(|d| !none::is_none(d)) {
            prop_doc = Some(d);
            po(o).getter_doc = 1;
        }
    }
    if o.is_type(property_type()) {
        setfield(&mut po(o).doc, prop_doc.as_deref());
        return Ok(());
    }
    let d = prop_doc.unwrap_or_else(none::none_ref);
    if let Err(e) = ab::setattr(o, crate::name!("__doc__"), &d) {
        if po(o).getter_doc == 0 && exc_matches(exceptions::AttributeError()) {
            clear_exc();
            return Ok(());
        }
        return Err(e);
    }
    Ok(())
}

unsafe extern "C" fn prop_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            static P: crate::args::Params = crate::args::Params::new("property", &["fget", "fset", "fdel", "doc"], 0, 0);
            let (vals, names) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [fget, fset, fdel, doc] = crate::args::parse::<4>(&P, &refs, names.as_deref())?;
            init_fields(&*o, fget, fset, fdel, doc)
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn prop_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let p = &mut *(o as *mut PropertyObject);
        for f in [p.fget, p.fset, p.fdel, p.doc, p.name] {
            xdecref(f);
        }
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn prop_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let p = &*(o as *mut PropertyObject);
        let f = visit.unwrap();
        for x in [p.fget, p.fset, p.fdel, p.doc, p.name] {
            if !x.is_null() {
                let r = f(x, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
unsafe extern "C" fn prop_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let p = &mut *(o as *mut PropertyObject);
        for f in [&mut p.fget, &mut p.fset, &mut p.fdel, &mut p.doc, &mut p.name] {
            let old = std::mem::replace(f, std::ptr::null_mut());
            xdecref(old);
        }
        0
    }
}

/// property_copy: new property of the same type with one accessor replaced.
fn copy_with(o: &PyObject, get: Option<&PyObject>, set: Option<&PyObject>, del: Option<&PyObject>) -> PyResult {
    let p = po(o);
    let pick = |new: Option<&PyObject>, old: *mut PyObject| -> PyRef {
        match new {
            Some(x) if !none::is_none(x) => x.to_ref(),
            _ => opt(old).map_or_else(none::none_ref, |x| x.to_ref()),
        }
    };
    let get = pick(get, p.fget);
    let set = pick(set, p.fset);
    let del = pick(del, p.fdel);
    let doc = if p.getter_doc != 0 && !none::is_none(&get) {
        none::none_ref()
    } else {
        opt(p.doc).map_or_else(none::none_ref, |x| x.to_ref())
    };
    let tp = type_ref(o.type_ptr());
    let new = ab::call(&tp, &[&get, &set, &del, &doc])?;
    if is_property(&new) {
        let name = opt(po(o).name);
        setfield(&mut po(&new).name, name);
    }
    Ok(new)
}
fn getter_m(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, Some(f), None, None)
}
fn setter_m(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, None, Some(f), None)
}
fn deleter_m(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, None, None, Some(f))
}
fn set_name_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "__set_name__() takes 2 positional arguments but {} were given", args.len());
    }
    setfield(&mut po(o).name, Some(args[1]));
    Ok(none::none_ref())
}

fn is_abstract(x: Option<&PyObject>) -> PyResult<bool> {
    match x {
        None => Ok(false),
        Some(x) => match ab::getattr_opt(x, crate::name!("__isabstractmethod__"))? {
            None => Ok(false),
            Some(v) => ab::is_true(&v),
        },
    }
}
fn isabstract_g(o: &PyObject) -> PyResult {
    let p = po(o);
    for f in [p.fget, p.fset, p.fdel] {
        if is_abstract(opt(f))? {
            return Ok(bool_::new_bool(true));
        }
    }
    Ok(bool_::new_bool(false))
}
fn name_g(o: &PyObject) -> PyResult {
    let p = po(o);
    if let Some(n) = opt(p.name) {
        return Ok(n.to_ref());
    }
    if let Some(g) = opt(p.fget) {
        if let Some(n) = ab::getattr_opt(g, crate::name!("__name__"))? {
            return Ok(n);
        }
    }
    raise!(AttributeError, "'{}' object has no attribute '__name__'", tname(o));
}
fn name_s(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    setfield(&mut po(o).name, v);
    Ok(())
}

pub fn init_types() {
    unsafe {
        let t = &mut *property_type();
        t.tp_name = c"property".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PropertyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(prop_dealloc);
        t.tp_traverse = Some(prop_traverse);
        t.tp_clear = Some(prop_clear);
        t.tp_descr_get = Some(prop_descr_get);
        t.tp_descr_set = Some(prop_descr_set);
        t.tp_init = Some(prop_init);
        t.tp_new = Some(crate::types::object_type::generic_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_doc = c"property(fget=None, fset=None, fdel=None, doc=None)\n--\n\nProperty attribute.\n\n  fget\n    function to be used for getting an attribute value\n  fset\n    function to be used for setting an attribute value\n  fdel\n    function to be used for del'ing an attribute\n  doc\n    docstring\n\nTypical use is to define a managed attribute x:\n\nclass C(object):\n    def getx(self): return self._x\n    def setx(self, value): self._x = value\n    def delx(self): del self._x\n    x = property(getx, setx, delx, \"I'm the 'x' property.\")\n\nDecorators make defining new properties or modifying existing ones easy:\n\nclass C(object):\n    @property\n    def x(self):\n        \"I am the 'x' property.\"\n        return self._x\n    @x.setter\n    def x(self, value):\n        self._x = value\n    @x.deleter\n    def x(self):\n        del self._x".as_ptr();
        t.tp_members = crate::member_table![
            ("fget", Py_T_OBJECT, std::mem::offset_of!(PropertyObject, fget), Py_READONLY),
            ("fset", Py_T_OBJECT, std::mem::offset_of!(PropertyObject, fset), Py_READONLY),
            ("fdel", Py_T_OBJECT, std::mem::offset_of!(PropertyObject, fdel), Py_READONLY),
            ("__doc__", Py_T_OBJECT, std::mem::offset_of!(PropertyObject, doc), 0),
        ];
        t.tp_getset = crate::getset_table![
            ("__isabstractmethod__", isabstract_g),
            ("__name__", name_g, name_s),
        ];
        t.tp_methods = crate::mdefs![
            ("getter", O, getter_m, "Descriptor to obtain a copy of the property with a different getter."),
            ("setter", O, setter_m, "Descriptor to obtain a copy of the property with a different setter."),
            ("deleter", O, deleter_m, "Descriptor to obtain a copy of the property with a different deleter."),
            ("__set_name__", FAST, set_name_m, "Method to set name of a property."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(property_type())
}
