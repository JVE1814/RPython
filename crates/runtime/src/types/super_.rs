//! super() (minimal).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::tuple;
use crate::raise;
use std::ffi::c_int;

#[repr(C)]
pub struct SuperObject {
    pub ob_base: PyObject,
    pub typ: *mut PyObject,
    pub obj: *mut PyObject,
    pub obj_type: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PySuper_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn super_type() -> *mut PyTypeObject {
    &raw mut PySuper_Type
}

fn supercheck(typ: *mut PyTypeObject, obj: &PyObject) -> PyResult<PyRef> {
    if is_type(obj) && is_subtype(as_type(obj), typ) {
        return Ok(obj.to_ref());
    }
    if is_subtype(obj.type_ptr(), typ) {
        return Ok(type_ref(obj.type_ptr()));
    }
    if let Some(c) = ab::getattr_opt(obj, crate::name!("__class__"))? {
        if is_type(&c) && !c.is(type_obj(obj.type_ptr())) && is_subtype(as_type(&c), typ) {
            return Ok(c);
        }
    }
    raise!(TypeError, "super(type, obj): obj ({} {}) is not an instance or subtype of type ({}).",
        if is_type(obj) { "type" } else { "instance of" },
        if is_type(obj) { type_name(unsafe { &*as_type(obj) }) } else { tname(obj) },
        type_name(unsafe { &*typ }));
}

pub fn new_super(typ: &PyObject, obj: &PyObject) -> PyResult {
    if !is_type(typ) {
        raise!(TypeError, "super() argument 1 must be a type, not {}", tname(typ));
    }
    let ot = supercheck(as_type(typ), obj)?;
    unsafe {
        let op = generic_alloc(super_type(), 0);
        let s = &mut *(op as *mut SuperObject);
        s.typ = typ.to_ref().into_raw();
        s.obj = obj.to_ref().into_raw();
        s.obj_type = ot.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn super_getattro(op: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*(op as *mut SuperObject);
        let n = &*nm;
        if !s.obj_type.is_null() && !crate::types::str::str_eq_ascii(n, "__class__") {
            let starttype = as_type(&*s.obj_type);
            let mro = (*starttype).tp_mro;
            if !mro.is_null() {
                let items = tuple::tuple_items(&*mro);
                let mut i = 0;
                while i < items.len() && !items[i].is(&*s.typ) {
                    i += 1;
                }
                i += 1;
                while i < items.len() {
                    let d = (*as_type(items[i])).tp_dict;
                    if !d.is_null() {
                        if let Some(res) = crate::types::dict::get_item_str(&*d, n) {
                            if let Some(g) = res.ty().tp_descr_get {
                                let obj = if s.obj == s.obj_type { std::ptr::null_mut() } else { s.obj };
                                return g(res.as_ptr(), obj, s.obj_type);
                            }
                            return res.into_raw();
                        }
                    }
                    i += 1;
                }
            }
        }
        object_generic_getattro(op, nm)
    }
}

unsafe extern "C" fn super_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = tuple::tuple_items(&*args);
        let r = (|| -> PyResult<()> {
            if !kw.is_null() && crate::types::dict::dict_len(&*kw) != 0 {
                raise!(TypeError, "super() takes no keyword arguments");
            }
            let (typ, obj) = match a.len() {
                0 => crate::modules::builtins_super::zero_arg_super()?,
                1 => (a[0].to_ref(), None),
                2 => (a[0].to_ref(), Some(a[1].to_ref())),
                n => raise!(TypeError, "super() expected at most 2 arguments, got {}", n),
            };
            if !is_type(&typ) {
                raise!(TypeError, "super() argument 1 must be a type, not {}", tname(&typ));
            }
            let s = &mut *(op as *mut SuperObject);
            let (mut no, mut nt) = (std::ptr::null_mut(), std::ptr::null_mut());
            if let Some(o) = obj.filter(|o| !o.is(crate::types::none::none())) {
                let ot = supercheck(as_type(&typ), &o)?;
                no = o.into_raw();
                nt = ot.into_raw();
            }
            // __init__ may be called again: release the previous values
            let old = [std::mem::replace(&mut s.obj, no), std::mem::replace(&mut s.obj_type, nt), std::mem::replace(&mut s.typ, typ.into_raw())];
            for p in old {
                xdecref(p);
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn super_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let s = &*(op as *mut SuperObject);
        xdecref(s.typ);
        xdecref(s.obj);
        xdecref(s.obj_type);
        let tp = (*op).type_ptr();
        generic_free_and_type_decref(op, tp);
    }
}

pub fn init_types() {
    unsafe {
        let t = &mut *super_type();
        t.tp_name = c"super".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SuperObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(super_dealloc);
        t.tp_getattro = Some(super_getattro);
        t.tp_init = Some(super_init);
        t.tp_new = Some(crate::types::object_type::generic_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_traverse = Some(super_traverse);
        t.tp_descr_get = Some(super_descr_get);
        t.tp_repr = Some(super_repr);
        t.tp_doc = c"super() -> same as super(__class__, <first argument>)\nsuper(type) -> unbound super object\nsuper(type, obj) -> bound super object; requires isinstance(obj, type)\nsuper(type, type2) -> bound super object; requires issubclass(type2, type)".as_ptr();
        t.tp_members = crate::member_table![
            ("__thisclass__", Py_T_OBJECT, std::mem::offset_of!(SuperObject, typ), Py_READONLY),
            ("__self__", Py_T_OBJECT, std::mem::offset_of!(SuperObject, obj), Py_READONLY),
            ("__self_class__", Py_T_OBJECT, std::mem::offset_of!(SuperObject, obj_type), Py_READONLY),
        ];
    }
}

unsafe extern "C" fn super_traverse(op: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let s = &*(op as *mut SuperObject);
        for p in [s.typ, s.obj, s.obj_type] {
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

/// An unbound super object stored as a class attribute binds to the instance.
unsafe extern "C" fn super_descr_get(op: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*(op as *mut SuperObject);
        if obj.is_null() || crate::types::none::is_none(&*obj) || !s.obj.is_null() {
            return (*op).to_ref().into_raw();
        }
        let t = (*op).type_ptr();
        if t != super_type() {
            // a subclass of super: call it like the type
            return into_c(ab::call(type_obj(t), &[&*s.typ, &*obj]));
        }
        into_c(new_super(&*s.typ, &*obj))
    }
}

unsafe extern "C" fn super_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*(op as *mut SuperObject);
        let tn = if s.typ.is_null() { "NULL".to_string() } else { type_name(&*as_type(&*s.typ)).to_string() };
        let r = if s.obj_type.is_null() {
            format!("<super: <class '{}'>, NULL>", tn)
        } else {
            format!("<super: <class '{}'>, <{} object>>", tn, type_name(&*as_type(&*s.obj_type)))
        };
        crate::types::str::new_str(&r).into_raw()
    }
}
