//! types.UnionType == typing.Union (3.14+): `int | str`, `Union[int, str]`, flattening and
//! de-duplication, `__args__`/`__parameters__`, substitution, repr, equality/hash, and
//! isinstance/issubclass support.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::lang_util::{self as lu, field, set_field};
use crate::types::typeobject::*;
use crate::types::{bool_, list, none, set, str as pystr, tuple};
use crate::{name, raise};

#[repr(C)]
pub struct UnionObject {
    pub ob_base: PyObject,
    pub args: *mut PyObject,
    pub hashable_args: *mut PyObject,   // frozenset
    pub unhashable_args: *mut PyObject, // tuple or null
    pub parameters: *mut PyObject,
    pub weakreflist: *mut PyObject,
}
const F_ARGS: usize = 0;
const F_HASHABLE: usize = 1;
const F_UNHASHABLE: usize = 2;
const F_PARAMS: usize = 3;

#[unsafe(no_mangle)]
pub static mut _PyUnion_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn union_type() -> *mut PyTypeObject {
    &raw mut _PyUnion_Type
}
pub fn is_union(o: &PyObject) -> bool {
    o.is_type(union_type())
}
pub fn union_args(o: &PyObject) -> PyRef {
    lu::field_ref(o, F_ARGS).unwrap_or_else(tuple::empty_tuple)
}

fn is_unionable(o: &PyObject) -> bool {
    o.is(none::none())
        || is_type(o)
        || crate::types::genericalias::is_generic_alias(o)
        || is_union(o)
        || crate::types::typevar::is_typealias(o)
}

/// Accumulates union members (flattening nested unions, de-duplicating).
struct Builder {
    args: Vec<PyRef>,
    hashable: PyRef,
    unhashable: Option<PyRef>,
}

impl Builder {
    fn new() -> PyResult<Builder> {
        Ok(Builder { args: Vec::new(), hashable: set::new_set_from_vec(Vec::new())?, unhashable: None })
    }
    fn add_unchecked(&mut self, arg: &PyObject) -> PyResult<()> {
        match ab::hash(arg) {
            Err(_) => {
                clear_exc();
                match &self.unhashable {
                    None => self.unhashable = Some(list::new_list_empty()),
                    Some(l) => {
                        if ab::contains(l, arg)? {
                            return Ok(());
                        }
                    }
                }
                list::append(self.unhashable.as_ref().unwrap(), arg.to_ref());
            }
            Ok(_) => {
                if set::set_contains(&self.hashable, arg)? {
                    return Ok(());
                }
                set::set_add(&self.hashable, arg)?;
            }
        }
        self.args.push(arg.to_ref());
        Ok(())
    }
    fn add_single(&mut self, arg: &PyObject) -> PyResult<()> {
        if arg.is(none::none()) {
            return self.add_unchecked(type_obj(none::none().type_ptr()));
        }
        if is_union(arg) {
            let a = union_args(arg);
            for x in tuple::tuple_items(&a) {
                self.add_unchecked(x)?;
            }
            return Ok(());
        }
        self.add_unchecked(arg)
    }
    fn make(self) -> PyResult {
        if self.args.is_empty() {
            raise!(TypeError, "Cannot take a Union of no types.");
        }
        if self.args.len() == 1 {
            return Ok(self.args.into_iter().next().unwrap());
        }
        let o = lu::alloc(union_type())?;
        set_field(&o, F_ARGS, Some(tuple::new_tuple(self.args)));
        let items = set::set_items(&self.hashable);
        set_field(&o, F_HASHABLE, Some(set::new_frozenset_from_vec(items)?));
        set_field(&o, F_UNHASHABLE, self.unhashable.map(|l| tuple::new_tuple(list::list_to_vec(&l))));
        Ok(o)
    }
}

/// Run `typing._type_check(arg, msg)` for non-unionable union members.
fn type_check(arg: &PyObject) -> PyResult {
    if arg.is(none::none()) {
        return Ok(type_ref(none::none().type_ptr()));
    }
    if is_unionable(arg) {
        return Ok(arg.to_ref());
    }
    let f = lu::import_attr("typing", "_type_check")?;
    let msg = pystr::new_str("Union[arg, ...]: each arg must be a type.");
    ab::call(&f, &[arg, &msg])
}

/// `_Py_union_from_tuple`: Union[args] with type checking.
pub fn union_from_tuple(args: &PyObject) -> PyResult {
    let items: Vec<PyRef> = if tuple::is_tuple(args) { tuple::tuple_to_vec(args) } else { vec![args.to_ref()] };
    let mut b = Builder::new()?;
    for a in items {
        let c = type_check(&a)?;
        b.add_single(&c)?;
    }
    b.make()
}

/// `X | Y` for types / generic aliases / unions (`_Py_union_type_or`).
pub fn union_or(a: &PyObject, b: &PyObject) -> PyResult {
    if !is_unionable(a) || !is_unionable(b) {
        return Ok(none::not_implemented().to_ref());
    }
    let mut bl = Builder::new()?;
    bl.add_single(a)?;
    bl.add_single(b)?;
    bl.make()
}

unsafe extern "C" fn union_nb_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| -> PyResult {
            let mut bl = Builder::new()?;
            for x in [a, b] {
                let c = if is_union(x) { x.to_ref() } else { type_check(x)? };
                bl.add_single(&c)?;
            }
            bl.make()
        })();
        into_c(r)
    }
}

unsafe extern "C" fn union_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let args = union_args(o);
            let mut parts = Vec::new();
            let nonetype = type_obj(none::none().type_ptr());
            for a in tuple::tuple_items(&args) {
                if a.is(nonetype) {
                    parts.push("None".to_string());
                } else {
                    parts.push(crate::types::genericalias::type_repr(a)?);
                }
            }
            Ok(pystr::new_str(&parts.join(" | ")))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn union_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let o = &*o;
        if let Some(u) = field(o, F_UNHASHABLE) {
            let items = tuple::tuple_items(u);
            for x in items {
                if ab::hash(x).is_err() {
                    return -1;
                }
            }
            let _ = err!(TypeError, "union contains {} unhashable elements", items.len());
            return -1;
        }
        match field(o, F_HASHABLE) {
            Some(h) => ab::hash(h).unwrap_or(-1),
            None => 0,
        }
    }
}

unsafe extern "C" fn union_richcompare(a: *mut PyObject, b: *mut PyObject, op: std::ffi::c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_union(b) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult<bool> {
            match (field(a, F_UNHASHABLE), field(b, F_UNHASHABLE)) {
                (Some(ua), Some(ub)) => {
                    if tuple::tuple_len(ua) != tuple::tuple_len(ub) {
                        return Ok(false);
                    }
                    for x in tuple::tuple_items(ua) {
                        if !ab::contains(ub, x)? {
                            return Ok(false);
                        }
                    }
                    for x in tuple::tuple_items(ub) {
                        if !ab::contains(ua, x)? {
                            return Ok(false);
                        }
                    }
                }
                (None, None) => {}
                _ => return Ok(false),
            }
            ab::rich_eq(field(a, F_HASHABLE).unwrap(), field(b, F_HASHABLE).unwrap())
        })();
        into_c(r.map(|eq| bool_::new_bool(if op == Py_EQ { eq } else { !eq })))
    }
}

fn union_parameters(o: &PyObject) -> PyResult {
    if let Some(p) = lu::field_ref(o, F_PARAMS) {
        return Ok(p);
    }
    let p = crate::types::genericalias::make_parameters(&union_args(o))?;
    set_field(o, F_PARAMS, Some(p.clone()));
    Ok(p)
}

unsafe extern "C" fn union_getitem(o: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let params = union_parameters(o)?;
            let newargs = crate::types::genericalias::subs_parameters(o, &union_args(o), &params, &*item)?;
            union_from_tuple(&newargs)
        })();
        into_c(r)
    }
}

fn get_args(o: &PyObject) -> PyResult {
    Ok(union_args(o))
}
fn get_parameters(o: &PyObject) -> PyResult {
    union_parameters(o)
}
fn get_origin(_o: &PyObject) -> PyResult {
    Ok(type_ref(union_type()))
}
fn get_name(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("Union"))
}

fn union_instancecheck(o: &PyObject, inst: &PyObject) -> PyResult {
    let args = union_args(o);
    for a in tuple::tuple_items(&args) {
        if ab::isinstance(inst, a)? {
            return Ok(bool_::new_bool(true));
        }
    }
    Ok(bool_::new_bool(false))
}
fn union_subclasscheck(o: &PyObject, sub: &PyObject) -> PyResult {
    if !is_type(sub) {
        raise!(TypeError, "issubclass() arg 1 must be a class");
    }
    let args = union_args(o);
    for a in tuple::tuple_items(&args) {
        if ab::issubclass(sub, a)? {
            return Ok(bool_::new_bool(true));
        }
    }
    Ok(bool_::new_bool(false))
}
fn union_mro_entries(o: &PyObject, _bases: &PyObject) -> PyResult {
    raise!(TypeError, "Cannot subclass {}", ab::repr_string(o)?);
}
fn union_reduce(o: &PyObject) -> PyResult {
    let getitem = lu::import_attr("operator", "getitem")?;
    Ok(tuple::new_tuple(vec![getitem, tuple::new_tuple(vec![type_ref(union_type()), union_args(o)])]))
}
fn union_class_getitem(_cls: &PyObject, args: &PyObject) -> PyResult {
    union_from_tuple(args)
}

pub fn init_types() {
    unsafe {
        let t = union_type();
        lu::setup_ptr_type(t, "typing.Union", std::mem::size_of::<UnionObject>(), 0);
        let tt = &mut *t;
        tt.tp_doc = c"Represent a union type\n\nE.g. for int | str".as_ptr();
        tt.tp_weaklistoffset = std::mem::offset_of!(UnionObject, weakreflist) as isize;
        tt.tp_repr = Some(union_repr);
        tt.tp_hash = Some(union_hash);
        tt.tp_richcompare = Some(union_richcompare);
        static mut U_MAP: PyMappingMethods = PyMappingMethods::ZERO;
        (*(&raw mut U_MAP)).mp_subscript = Some(union_getitem);
        tt.tp_as_mapping = &raw mut U_MAP;
        static mut U_NUM: PyNumberMethods = PyNumberMethods::ZERO;
        (*(&raw mut U_NUM)).nb_or = Some(union_nb_or);
        tt.tp_as_number = &raw mut U_NUM;
        tt.tp_methods = crate::mdefs![
            ("__instancecheck__", O, union_instancecheck),
            ("__subclasscheck__", O, union_subclasscheck),
            ("__mro_entries__", O, union_mro_entries),
            ("__reduce__", NOARGS, union_reduce),
            ("__class_getitem__", O, union_class_getitem; METH_CLASS),
        ];
        tt.tp_getset = crate::getset_table![
            ("__args__", get_args),
            ("__parameters__", get_parameters; "Type variables in the types.UnionType."),
            ("__origin__", get_origin),
            ("__name__", get_name),
            ("__qualname__", get_name),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    lu::ready_type(union_type())?;
    let _ = name!("__args__");
    Ok(())
}
