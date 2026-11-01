//! The `_operator` accelerator module: operator functions, attrgetter, itemgetter,
//! methodcaller, _compare_digest, length_hint, call.

use crate::abstract_::{self as ab, NbSlot};
use crate::args::check_pos;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, opt, targs};
use crate::object::*;
use crate::types::{bool_, dict, int, none, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::ffi::{c_int, c_void};

fn b(v: bool) -> PyResult {
    Ok(bool_::new_bool(v))
}

macro_rules! binop {
    ($fname:ident, $pyname:expr, $slot:expr) => {
        fn $fname(_m: &PyObject, a: &[&PyObject]) -> PyResult {
            check_pos($pyname, a, 2, 2)?;
            ab::binary_op(a[0], a[1], $slot)
        }
    };
}
macro_rules! ibinop {
    ($fname:ident, $pyname:expr, $slot:expr) => {
        fn $fname(_m: &PyObject, a: &[&PyObject]) -> PyResult {
            check_pos($pyname, a, 2, 2)?;
            ab::inplace_op(a[0], a[1], $slot)
        }
    };
}
macro_rules! cmpop {
    ($fname:ident, $pyname:expr, $op:expr) => {
        fn $fname(_m: &PyObject, a: &[&PyObject]) -> PyResult {
            check_pos($pyname, a, 2, 2)?;
            ab::rich_compare(a[0], a[1], $op)
        }
    };
}

binop!(op_add, "add", NbSlot::Add);
binop!(op_sub, "sub", NbSlot::Sub);
binop!(op_mul, "mul", NbSlot::Mul);
binop!(op_matmul, "matmul", NbSlot::MatMul);
binop!(op_floordiv, "floordiv", NbSlot::FloorDiv);
binop!(op_truediv, "truediv", NbSlot::TrueDiv);
binop!(op_mod, "mod", NbSlot::Mod);
binop!(op_lshift, "lshift", NbSlot::Lshift);
binop!(op_rshift, "rshift", NbSlot::Rshift);
binop!(op_and, "and_", NbSlot::And);
binop!(op_xor, "xor", NbSlot::Xor);
binop!(op_or, "or_", NbSlot::Or);
ibinop!(op_iadd, "iadd", NbSlot::Add);
ibinop!(op_isub, "isub", NbSlot::Sub);
ibinop!(op_imul, "imul", NbSlot::Mul);
ibinop!(op_imatmul, "imatmul", NbSlot::MatMul);
ibinop!(op_ifloordiv, "ifloordiv", NbSlot::FloorDiv);
ibinop!(op_itruediv, "itruediv", NbSlot::TrueDiv);
ibinop!(op_imod, "imod", NbSlot::Mod);
ibinop!(op_ilshift, "ilshift", NbSlot::Lshift);
ibinop!(op_irshift, "irshift", NbSlot::Rshift);
ibinop!(op_iand, "iand", NbSlot::And);
ibinop!(op_ixor, "ixor", NbSlot::Xor);
ibinop!(op_ior, "ior", NbSlot::Or);
ibinop!(op_ipow, "ipow", NbSlot::Pow);
cmpop!(op_lt, "lt", Py_LT);
cmpop!(op_le, "le", Py_LE);
cmpop!(op_eq, "eq", Py_EQ);
cmpop!(op_ne, "ne", Py_NE);
cmpop!(op_gt, "gt", Py_GT);
cmpop!(op_ge, "ge", Py_GE);

fn op_pow(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("pow", a, 2, 2)?;
    ab::power(a[0], a[1], none::none())
}
fn op_neg(_m: &PyObject, a: &PyObject) -> PyResult {
    ab::negative(a)
}
fn op_pos(_m: &PyObject, a: &PyObject) -> PyResult {
    ab::positive(a)
}
fn op_abs(_m: &PyObject, a: &PyObject) -> PyResult {
    ab::absolute(a)
}
fn op_inv(_m: &PyObject, a: &PyObject) -> PyResult {
    ab::invert(a)
}
fn op_index(_m: &PyObject, a: &PyObject) -> PyResult {
    ab::index(a)
}
fn op_truth(_m: &PyObject, a: &PyObject) -> PyResult {
    b(ab::is_true(a)?)
}
fn op_not(_m: &PyObject, a: &PyObject) -> PyResult {
    b(!ab::is_true(a)?)
}
fn op_is(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("is_", a, 2, 2)?;
    b(a[0].is(a[1]))
}
fn op_is_not(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("is_not", a, 2, 2)?;
    b(!a[0].is(a[1]))
}
fn op_is_none(_m: &PyObject, a: &PyObject) -> PyResult {
    b(a.is(none::none()))
}
fn op_is_not_none(_m: &PyObject, a: &PyObject) -> PyResult {
    b(!a.is(none::none()))
}

/// PySequence_Check.
pub fn is_sequence(o: &PyObject) -> bool {
    if dict::is_dict(o) {
        return false;
    }
    let sq = o.ty().tp_as_sequence;
    !sq.is_null() && unsafe { (*sq).sq_item.is_some() }
}

fn op_concat(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("concat", a, 2, 2)?;
    unsafe {
        let sq = a[0].ty().tp_as_sequence;
        if !sq.is_null() {
            if let Some(f) = (*sq).sq_concat {
                return from_c(f(a[0].as_ptr(), a[1].as_ptr()));
            }
        }
    }
    if is_sequence(a[0]) && is_sequence(a[1]) {
        let r = ab::binary_op(a[0], a[1], NbSlot::Add);
        return r;
    }
    raise!(TypeError, "'{}' object can't be concatenated", fc::tname(a[0]));
}

fn op_iconcat(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("iconcat", a, 2, 2)?;
    unsafe {
        let sq = a[0].ty().tp_as_sequence;
        if !sq.is_null() {
            if let Some(f) = (*sq).sq_inplace_concat {
                return from_c(f(a[0].as_ptr(), a[1].as_ptr()));
            }
            if let Some(f) = (*sq).sq_concat {
                return from_c(f(a[0].as_ptr(), a[1].as_ptr()));
            }
        }
    }
    if is_sequence(a[0]) && is_sequence(a[1]) {
        return ab::inplace_op(a[0], a[1], NbSlot::Add);
    }
    raise!(TypeError, "'{}' object can't be concatenated", fc::tname(a[0]));
}

fn op_contains(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("contains", a, 2, 2)?;
    b(ab::contains(a[0], a[1])?)
}

fn op_count_of(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("countOf", a, 2, 2)?;
    let it = ab::get_iter(a[0])?;
    let mut n: i64 = 0;
    while let Some(x) = ab::iter_next(&it)? {
        if x.is(a[1]) || ab::rich_eq(&x, a[1])? {
            n += 1;
        }
    }
    Ok(int::new_int(n))
}

fn op_index_of(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("indexOf", a, 2, 2)?;
    let it = ab::get_iter(a[0])?;
    let mut n: i64 = 0;
    while let Some(x) = ab::iter_next(&it)? {
        if x.is(a[1]) || ab::rich_eq(&x, a[1])? {
            return Ok(int::new_int(n));
        }
        n += 1;
    }
    raise!(ValueError, "sequence.index(x): x not in sequence");
}

fn op_getitem(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("getitem", a, 2, 2)?;
    ab::getitem(a[0], a[1])
}
fn op_setitem(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("setitem", a, 3, 3)?;
    ab::setitem(a[0], a[1], Some(a[2]))?;
    Ok(fc::none())
}
fn op_delitem(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("delitem", a, 2, 2)?;
    ab::setitem(a[0], a[1], None)?;
    Ok(fc::none())
}

/// PyObject_LengthHint with the exact CPython semantics.
pub fn length_hint(o: &PyObject, default: isize) -> PyResult<isize> {
    let tp = o.ty();
    let has_len = unsafe {
        (!tp.tp_as_sequence.is_null() && (*tp.tp_as_sequence).sq_length.is_some())
            || (!tp.tp_as_mapping.is_null() && (*tp.tp_as_mapping).mp_length.is_some())
    };
    if has_len {
        match ab::length(o) {
            Ok(n) => return Ok(n as isize),
            Err(e) => {
                if !exc_matches(crate::types::exceptions::TypeError()) {
                    return Err(e);
                }
                clear_exc();
            }
        }
    }
    let hint = match ab::lookup_special(o, name!("__length_hint__"))? {
        Some(h) => h,
        None => return Ok(default),
    };
    let r = match ab::call(&hint, &[]) {
        Ok(r) => r,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                return Ok(default);
            }
            return Err(e);
        }
    };
    if r.is(none::not_implemented()) {
        return Ok(default);
    }
    if !int::is_int(&r) {
        raise!(TypeError, "__length_hint__ must be an integer, not {}", fc::tname(&r));
    }
    let v = fc::ssize_arg(&r)?;
    if v < 0 {
        raise!(ValueError, "__length_hint__() should return >= 0");
    }
    Ok(v)
}

fn op_length_hint(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("length_hint", a, 1, 2)?;
    let default = if a.len() > 1 {
        if !ab::has_index(a[1]) {
            raise!(TypeError, "'{}' object cannot be interpreted as an integer", fc::tname(a[1]));
        }
        fc::ssize_arg(a[1])?
    } else {
        0
    };
    Ok(int::new_int(length_hint(a[0], default)? as i64))
}

fn op_call(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let nkw = kw.map_or(0, tuple::tuple_len);
    if a.len() - nkw < 1 {
        raise!(TypeError, "call() missing required argument 'obj' (pos 1)");
    }
    ab::call_kw(a[0], &a[1..], kw)
}

fn op_compare_digest(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    check_pos("_compare_digest", a, 2, 2)?;
    let (x, y) = (a[0], a[1]);
    let (bx, by): (Vec<u8>, Vec<u8>);
    if pystr::is_str(x) && pystr::is_str(y) {
        if !pystr::str_is_ascii(x) || !pystr::str_is_ascii(y) {
            raise!(TypeError, "comparing strings with non-ASCII characters is not supported");
        }
        bx = pystr::to_string_lossy(x).into_bytes();
        by = pystr::to_string_lossy(y).into_bytes();
    } else {
        let gx = crate::types::bytes::bytes_like_data(x);
        let gy = crate::types::bytes::bytes_like_data(y);
        match (gx, gy) {
            (Some(p), Some(q)) if !pystr::is_str(x) && !pystr::is_str(y) => {
                bx = p;
                by = q;
            }
            _ => raise!(
                TypeError,
                "unsupported operand types(s) or combination of types: '{}' and '{}'",
                fc::tname(x),
                fc::tname(y)
            ),
        }
    }
    // constant time with respect to the length of `b`
    let mut result: u8 = if bx.len() == by.len() { 0 } else { 1 };
    let left: &[u8] = if bx.len() == by.len() { &bx } else { &by };
    for (i, c) in by.iter().enumerate() {
        result |= std::hint::black_box(left[i]) ^ c;
    }
    b(result == 0)
}

// ================================================================== attrgetter

#[repr(C)]
struct AttrGetter {
    ob_base: PyObject,
    /// tuple of attribute paths; each path is a tuple of interned str (or a single str)
    attrs: *mut PyObject,
    vectorcall: vectorcallfunc,
}

#[unsafe(no_mangle)]
pub static mut _PyOperator_AttrGetter_Type: PyTypeObject = PyTypeObject::ZERO;
fn attrgetter_type() -> *mut PyTypeObject {
    &raw mut _PyOperator_AttrGetter_Type
}

unsafe extern "C" fn attrgetter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("attrgetter", kw)?;
            let a = targs(args);
            if a.is_empty() {
                raise!(TypeError, "attrgetter expected 1 argument, got 0");
            }
            let mut paths = Vec::with_capacity(a.len());
            for item in a {
                if !pystr::is_str(item) {
                    raise!(TypeError, "attribute name must be a string");
                }
                let s = pystr::to_string_lossy(item);
                if s.contains('.') {
                    let parts: Vec<PyRef> = s.split('.').map(|p| pystr::intern_str(p)).collect();
                    paths.push(tuple::new_tuple(parts));
                } else {
                    paths.push(pystr::intern(item.to_ref()));
                }
            }
            let op = fc::alloc(tp)?;
            let g = op as *mut AttrGetter;
            (*g).attrs = tuple::new_tuple(paths).into_raw();
            (*g).vectorcall = Some(attrgetter_vectorcall);
            Ok(PyRef::from_raw(op))
        })())
    }
}

fn dotted_getattr(obj: &PyObject, path: &PyObject) -> PyResult {
    if tuple::is_tuple(path) {
        let mut cur = obj.to_ref();
        for p in tuple::tuple_items(path) {
            cur = ab::getattr(&cur, p)?;
        }
        Ok(cur)
    } else {
        ab::getattr(obj, path)
    }
}

fn attrgetter_call_impl(g: &AttrGetter, obj: &PyObject) -> PyResult {
    let attrs = unsafe { &*g.attrs };
    let items = tuple::tuple_items(attrs);
    if items.len() == 1 {
        return dotted_getattr(obj, items[0]);
    }
    let mut out = Vec::with_capacity(items.len());
    for p in items {
        out.push(dotted_getattr(obj, p)?);
    }
    Ok(tuple::new_tuple(out))
}

unsafe extern "C" fn attrgetter_vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        if !kwnames.is_null() && tuple::tuple_len(&*kwnames) > 0 {
            return into_c(Err(err!(TypeError, "attrgetter() takes no keyword arguments")));
        }
        if n != 1 {
            return into_c(Err(err!(TypeError, "attrgetter expected 1 argument, got {}", n)));
        }
        into_c(attrgetter_call_impl(&*(callable as *mut AttrGetter), &**args))
    }
}

unsafe extern "C" fn attrgetter_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("attrgetter", kw)?;
            let a = targs(args);
            check_pos("attrgetter", a, 1, 1)?;
            attrgetter_call_impl(&*(o as *mut AttrGetter), a[0])
        })())
    }
}

fn path_to_str(p: &PyObject) -> PyRef {
    if tuple::is_tuple(p) {
        let parts: Vec<String> = tuple::tuple_items(p).iter().map(|x| pystr::to_string_lossy(x)).collect();
        pystr::new_str(&parts.join("."))
    } else {
        p.to_ref()
    }
}

unsafe extern "C" fn attrgetter_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let g = &*(o as *mut AttrGetter);
            let mut parts = Vec::new();
            for p in tuple::tuple_items(&*g.attrs) {
                parts.push(ab::repr_string(&path_to_str(p))?);
            }
            Ok(pystr::new_str(&format!("{}({})", fc::raw_tp_name(&*o), parts.join(", "))))
        })())
    }
}

fn attrgetter_reduce(o: &PyObject) -> PyResult {
    let g = unsafe { &*(o.as_ptr() as *mut AttrGetter) };
    let names: Vec<PyRef> = tuple::tuple_items(unsafe { &*g.attrs }).iter().map(|p| path_to_str(p)).collect();
    Ok(tuple::tuple2(crate::types::typeobject::type_ref(o.type_ptr()), tuple::new_tuple(names)))
}

unsafe extern "C" fn attrgetter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut AttrGetter)).attrs);
        fc::free(o);
    }
}
unsafe extern "C" fn attrgetter_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    crate::visit_all!(v, arg; unsafe { (*(o as *mut AttrGetter)).attrs });
    0
}

// ================================================================== itemgetter

#[repr(C)]
struct ItemGetter {
    ob_base: PyObject,
    items: *mut PyObject, // tuple
    index: isize,         // >= 0 if single item that is a small non-negative int
    vectorcall: vectorcallfunc,
}

#[unsafe(no_mangle)]
pub static mut _PyOperator_ItemGetter_Type: PyTypeObject = PyTypeObject::ZERO;
fn itemgetter_type() -> *mut PyTypeObject {
    &raw mut _PyOperator_ItemGetter_Type
}

unsafe extern "C" fn itemgetter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("itemgetter", kw)?;
            let a = targs(args);
            if a.is_empty() {
                raise!(TypeError, "itemgetter expected 1 argument, got 0");
            }
            let op = fc::alloc(tp)?;
            let g = op as *mut ItemGetter;
            (*g).items = (*args).to_ref().into_raw();
            (*g).index = -1;
            if a.len() == 1 && int::is_int_exact(a[0]) && int::is_compact(a[0]) {
                let v = int::compact_value(a[0]);
                if v >= 0 {
                    (*g).index = v as isize;
                }
            }
            (*g).vectorcall = Some(itemgetter_vectorcall);
            Ok(PyRef::from_raw(op))
        })())
    }
}

fn itemgetter_call_impl(g: &ItemGetter, obj: &PyObject) -> PyResult {
    let items = tuple::tuple_items(unsafe { &*g.items });
    if items.len() == 1 {
        if g.index >= 0 && tuple::is_tuple_exact(obj) {
            let n = tuple::tuple_len(obj);
            if (g.index as usize) < n {
                return Ok(tuple::tuple_get(obj, g.index as usize).to_ref());
            }
        }
        return ab::getitem(obj, items[0]);
    }
    let mut out = Vec::with_capacity(items.len());
    for k in items {
        out.push(ab::getitem(obj, k)?);
    }
    Ok(tuple::new_tuple(out))
}

unsafe extern "C" fn itemgetter_vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        if !kwnames.is_null() && tuple::tuple_len(&*kwnames) > 0 {
            return into_c(Err(err!(TypeError, "itemgetter() takes no keyword arguments")));
        }
        if n != 1 {
            return into_c(Err(err!(TypeError, "itemgetter expected 1 argument, got {}", n)));
        }
        into_c(itemgetter_call_impl(&*(callable as *mut ItemGetter), &**args))
    }
}

unsafe extern "C" fn itemgetter_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("itemgetter", kw)?;
            let a = targs(args);
            check_pos("itemgetter", a, 1, 1)?;
            itemgetter_call_impl(&*(o as *mut ItemGetter), a[0])
        })())
    }
}

unsafe extern "C" fn itemgetter_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let g = &*(o as *mut ItemGetter);
            let name = fc::raw_tp_name(&*o);
            let guard = match fc::ReprGuard::enter(&*o) {
                Some(gd) => gd,
                None => return Ok(pystr::new_str(&format!("{}(...)", name))),
            };
            let items = tuple::tuple_items(&*g.items);
            let mut parts = Vec::new();
            for i in items {
                parts.push(ab::repr_string(i)?);
            }
            drop(guard);
            Ok(pystr::new_str(&format!("{}({})", name, parts.join(", "))))
        })())
    }
}

fn itemgetter_reduce(o: &PyObject) -> PyResult {
    let g = unsafe { &*(o.as_ptr() as *mut ItemGetter) };
    Ok(tuple::tuple2(crate::types::typeobject::type_ref(o.type_ptr()), unsafe { (*g.items).to_ref() }))
}

unsafe extern "C" fn itemgetter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut ItemGetter)).items);
        fc::free(o);
    }
}
unsafe extern "C" fn itemgetter_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    crate::visit_all!(v, arg; unsafe { (*(o as *mut ItemGetter)).items });
    0
}

// ================================================================== methodcaller

#[repr(C)]
struct MethodCaller {
    ob_base: PyObject,
    name: *mut PyObject,
    args: *mut PyObject,     // tuple
    kwds: *mut PyObject,     // dict
    vc_args: *mut PyObject,  // tuple: (placeholder, *args, *kwvalues)
    vc_names: *mut PyObject, // tuple of kw names or NULL
    vectorcall: vectorcallfunc,
}

#[unsafe(no_mangle)]
pub static mut _PyOperator_MethodCaller_Type: PyTypeObject = PyTypeObject::ZERO;
fn methodcaller_type() -> *mut PyTypeObject {
    &raw mut _PyOperator_MethodCaller_Type
}

unsafe extern "C" fn methodcaller_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let a = targs(args);
            if a.is_empty() {
                raise!(TypeError, "methodcaller needs at least one argument, the method name");
            }
            if !pystr::is_str(a[0]) {
                raise!(TypeError, "method name must be a string");
            }
            let op = fc::alloc(tp)?;
            let m = op as *mut MethodCaller;
            (*m).name = pystr::intern(a[0].to_ref()).into_raw();
            (*m).args = tuple::new_tuple(a[1..].iter().map(|x| x.to_ref()).collect()).into_raw();
            let kwds = if kw.is_null() { dict::new_dict() } else { dict::copy(&*kw)? };
            let (vals, names) = fc::vc_from_kwdict(&a[1..], Some(&kwds));
            (*m).kwds = kwds.into_raw();
            (*m).vc_args = tuple::new_tuple(vals).into_raw();
            (*m).vc_names = names.map_or(std::ptr::null_mut(), |n| n.into_raw());
            (*m).vectorcall = Some(methodcaller_vectorcall);
            Ok(PyRef::from_raw(op))
        })())
    }
}

fn methodcaller_call_impl(m: &MethodCaller, obj: &PyObject) -> PyResult {
    let meth = ab::getattr(obj, unsafe { &*m.name })?;
    let vals = tuple::tuple_items(unsafe { &*m.vc_args });
    ab::call_kw(&meth, vals, unsafe { opt(m.vc_names) })
}

unsafe extern "C" fn methodcaller_vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        if !kwnames.is_null() && tuple::tuple_len(&*kwnames) > 0 {
            return into_c(Err(err!(TypeError, "methodcaller() takes no keyword arguments")));
        }
        if n != 1 {
            return into_c(Err(err!(TypeError, "methodcaller expected 1 argument, got {}", n)));
        }
        into_c(methodcaller_call_impl(&*(callable as *mut MethodCaller), &**args))
    }
}

unsafe extern "C" fn methodcaller_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("methodcaller", kw)?;
            let a = targs(args);
            check_pos("methodcaller", a, 1, 1)?;
            methodcaller_call_impl(&*(o as *mut MethodCaller), a[0])
        })())
    }
}

unsafe extern "C" fn methodcaller_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let m = &*(o as *mut MethodCaller);
            let tn = fc::raw_tp_name(&*o);
            let _g = match fc::ReprGuard::enter(&*o) {
                Some(g) => g,
                None => return Ok(pystr::new_str(&format!("{}(...)", tn))),
            };
            let mut parts = vec![ab::repr_string(&*m.name)?];
            for a in tuple::tuple_items(&*m.args) {
                parts.push(ab::repr_string(a)?);
            }
            for (k, v) in dict::items_vec(&*m.kwds) {
                parts.push(format!("{}={}", pystr::to_string_lossy(&k), ab::repr_string(&v)?));
            }
            Ok(pystr::new_str(&format!("{}({})", tn, parts.join(", "))))
        })())
    }
}

fn methodcaller_reduce(o: &PyObject) -> PyResult {
    let m = unsafe { &*(o.as_ptr() as *mut MethodCaller) };
    let tp = crate::types::typeobject::type_ref(o.type_ptr());
    unsafe {
        if dict::dict_len(&*m.kwds) == 0 {
            let mut v = vec![(*m.name).to_ref()];
            v.extend(tuple::tuple_to_vec(&*m.args));
            return Ok(tuple::tuple2(tp, tuple::new_tuple(v)));
        }
        // partial(methodcaller, name, **kwds)(*args)
        let functools = crate::import::import_absolute("functools")?;
        let partial = ab::getattr_str(&functools, "partial")?;
        let p = fc::call_kwdict(&partial, &[&tp, &*m.name], Some(&*m.kwds))?;
        Ok(tuple::tuple2(p, (*m.args).to_ref()))
    }
}

unsafe extern "C" fn methodcaller_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let m = o as *mut MethodCaller;
        fc::clear_field(&raw mut (*m).name);
        fc::clear_field(&raw mut (*m).args);
        fc::clear_field(&raw mut (*m).kwds);
        fc::clear_field(&raw mut (*m).vc_args);
        fc::clear_field(&raw mut (*m).vc_names);
        fc::free(o);
    }
}
unsafe extern "C" fn methodcaller_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    let m = o as *mut MethodCaller;
    crate::visit_all!(v, arg; unsafe { (*m).name }, unsafe { (*m).args }, unsafe { (*m).kwds }, unsafe { (*m).vc_args });
    0
}

// ================================================================== module

fn init_types() -> PyResult<()> {
    unsafe {
        let t = attrgetter_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_type(t, c"operator.attrgetter", std::mem::size_of::<AttrGetter>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL);
        (*t).tp_new = Some(attrgetter_new);
        (*t).tp_call = Some(attrgetter_call);
        (*t).tp_repr = Some(attrgetter_repr);
        (*t).tp_dealloc = Some(attrgetter_dealloc);
        (*t).tp_traverse = Some(attrgetter_traverse);
        (*t).tp_vectorcall_offset = std::mem::offset_of!(AttrGetter, vectorcall) as isize;
        (*t).tp_doc = c"attrgetter(attr, /, *attrs)\n--\n\nReturn a callable object that fetches the given attribute(s) from its operand.\nAfter f = attrgetter('name'), the call f(r) returns r.name.\nAfter g = attrgetter('name', 'date'), the call g(r) returns (r.name, r.date).\nAfter h = attrgetter('name.first', 'name.last'), the call h(r) returns\n(r.name.first, r.name.last).".as_ptr();
        (*t).tp_methods = mdefs![("__reduce__", NOARGS, |o: &PyObject| attrgetter_reduce(o))];

        let t = itemgetter_type();
        fc::init_type(t, c"operator.itemgetter", std::mem::size_of::<ItemGetter>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL);
        (*t).tp_new = Some(itemgetter_new);
        (*t).tp_call = Some(itemgetter_call);
        (*t).tp_repr = Some(itemgetter_repr);
        (*t).tp_dealloc = Some(itemgetter_dealloc);
        (*t).tp_traverse = Some(itemgetter_traverse);
        (*t).tp_vectorcall_offset = std::mem::offset_of!(ItemGetter, vectorcall) as isize;
        (*t).tp_doc = c"itemgetter(item, /, *items)\n--\n\nReturn a callable object that fetches the given item(s) from its operand.\nAfter f = itemgetter(2), the call f(r) returns r[2].\nAfter g = itemgetter(2, 5, 3), the call g(r) returns (r[2], r[5], r[3])".as_ptr();
        (*t).tp_methods = mdefs![("__reduce__", NOARGS, |o: &PyObject| itemgetter_reduce(o))];

        let t = methodcaller_type();
        fc::init_type(t, c"operator.methodcaller", std::mem::size_of::<MethodCaller>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL);
        (*t).tp_new = Some(methodcaller_new);
        (*t).tp_call = Some(methodcaller_call);
        (*t).tp_repr = Some(methodcaller_repr);
        (*t).tp_dealloc = Some(methodcaller_dealloc);
        (*t).tp_traverse = Some(methodcaller_traverse);
        (*t).tp_vectorcall_offset = std::mem::offset_of!(MethodCaller, vectorcall) as isize;
        (*t).tp_doc = c"methodcaller(name, /, *args, **kwargs)\n--\n\nReturn a callable object that calls the given method on its operand.\nAfter f = methodcaller('name'), the call f(r) returns r.name().\nAfter g = methodcaller('name', 'date', foo=1), the call g(r) returns\nr.name('date', foo=1).".as_ptr();
        (*t).tp_methods = mdefs![("__reduce__", NOARGS, |o: &PyObject| methodcaller_reduce(o))];
        for t in [attrgetter_type(), itemgetter_type(), methodcaller_type()] {
            crate::types::typeobject::type_ready(t)?;
            crate::types::typeobject::type_dict_set(t, "__text_signature__", &pystr::new_str("(obj, /)"));
        }
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    let methods = mdefs![
        ("abs", O, op_abs, "abs($module, a, /)\n--\n\nSame as abs(a)."),
        ("add", FAST, op_add, "add($module, a, b, /)\n--\n\nSame as a + b."),
        ("and_", FAST, op_and, "and_($module, a, b, /)\n--\n\nSame as a & b."),
        ("call", FASTKW, op_call, "call($module, obj, /, *args, **kwargs)\n--\n\nSame as obj(*args, **kwargs)."),
        ("concat", FAST, op_concat, "concat($module, a, b, /)\n--\n\nSame as a + b, for a and b sequences."),
        ("contains", FAST, op_contains, "contains($module, a, b, /)\n--\n\nSame as b in a (note reversed operands)."),
        ("countOf", FAST, op_count_of, "countOf($module, a, b, /)\n--\n\nReturn the number of items in a which are, or which equal, b."),
        ("delitem", FAST, op_delitem, "delitem($module, a, b, /)\n--\n\nSame as del a[b]."),
        ("eq", FAST, op_eq, "eq($module, a, b, /)\n--\n\nSame as a == b."),
        ("floordiv", FAST, op_floordiv, "floordiv($module, a, b, /)\n--\n\nSame as a // b."),
        ("ge", FAST, op_ge, "ge($module, a, b, /)\n--\n\nSame as a >= b."),
        ("getitem", FAST, op_getitem, "getitem($module, a, b, /)\n--\n\nSame as a[b]."),
        ("gt", FAST, op_gt, "gt($module, a, b, /)\n--\n\nSame as a > b."),
        ("iadd", FAST, op_iadd, "iadd($module, a, b, /)\n--\n\nSame as a += b."),
        ("iand", FAST, op_iand, "iand($module, a, b, /)\n--\n\nSame as a &= b."),
        ("iconcat", FAST, op_iconcat, "iconcat($module, a, b, /)\n--\n\nSame as a += b, for a and b sequences."),
        ("ifloordiv", FAST, op_ifloordiv, "ifloordiv($module, a, b, /)\n--\n\nSame as a //= b."),
        ("ilshift", FAST, op_ilshift, "ilshift($module, a, b, /)\n--\n\nSame as a <<= b."),
        ("imatmul", FAST, op_imatmul, "imatmul($module, a, b, /)\n--\n\nSame as a @= b."),
        ("imod", FAST, op_imod, "imod($module, a, b, /)\n--\n\nSame as a %= b."),
        ("imul", FAST, op_imul, "imul($module, a, b, /)\n--\n\nSame as a *= b."),
        ("index", O, op_index, "index($module, a, /)\n--\n\nSame as a.__index__()"),
        ("indexOf", FAST, op_index_of, "indexOf($module, a, b, /)\n--\n\nReturn the first index of b in a."),
        ("inv", O, op_inv, "inv($module, a, /)\n--\n\nSame as ~a."),
        ("invert", O, op_inv, "invert($module, a, /)\n--\n\nSame as ~a."),
        ("ior", FAST, op_ior, "ior($module, a, b, /)\n--\n\nSame as a |= b."),
        ("ipow", FAST, op_ipow, "ipow($module, a, b, /)\n--\n\nSame as a **= b."),
        ("irshift", FAST, op_irshift, "irshift($module, a, b, /)\n--\n\nSame as a >>= b."),
        ("is_", FAST, op_is, "is_($module, a, b, /)\n--\n\nSame as a is b."),
        ("is_not", FAST, op_is_not, "is_not($module, a, b, /)\n--\n\nSame as a is not b."),
        ("is_none", O, op_is_none, "is_none($module, a, /)\n--\n\nSame as a is None."),
        ("is_not_none", O, op_is_not_none, "is_not_none($module, a, /)\n--\n\nSame as a is not None."),
        ("isub", FAST, op_isub, "isub($module, a, b, /)\n--\n\nSame as a -= b."),
        ("itruediv", FAST, op_itruediv, "itruediv($module, a, b, /)\n--\n\nSame as a /= b."),
        ("ixor", FAST, op_ixor, "ixor($module, a, b, /)\n--\n\nSame as a ^= b."),
        ("le", FAST, op_le, "le($module, a, b, /)\n--\n\nSame as a <= b."),
        ("length_hint", FAST, op_length_hint, "length_hint($module, obj, default=0, /)\n--\n\nReturn an estimate of the number of items in obj.\n\nThis is useful for presizing containers when building from an iterable.\n\nIf the object supports len(), the result will be exact.\nOtherwise, it may over- or under-estimate by an arbitrary amount.\nThe result will be an integer >= 0."),
        ("lshift", FAST, op_lshift, "lshift($module, a, b, /)\n--\n\nSame as a << b."),
        ("lt", FAST, op_lt, "lt($module, a, b, /)\n--\n\nSame as a < b."),
        ("matmul", FAST, op_matmul, "matmul($module, a, b, /)\n--\n\nSame as a @ b."),
        ("mod", FAST, op_mod, "mod($module, a, b, /)\n--\n\nSame as a % b."),
        ("mul", FAST, op_mul, "mul($module, a, b, /)\n--\n\nSame as a * b."),
        ("ne", FAST, op_ne, "ne($module, a, b, /)\n--\n\nSame as a != b."),
        ("neg", O, op_neg, "neg($module, a, /)\n--\n\nSame as -a."),
        ("not_", O, op_not, "not_($module, a, /)\n--\n\nSame as not a."),
        ("or_", FAST, op_or, "or_($module, a, b, /)\n--\n\nSame as a | b."),
        ("pos", O, op_pos, "pos($module, a, /)\n--\n\nSame as +a."),
        ("pow", FAST, op_pow, "pow($module, a, b, /)\n--\n\nSame as a ** b."),
        ("rshift", FAST, op_rshift, "rshift($module, a, b, /)\n--\n\nSame as a >> b."),
        ("setitem", FAST, op_setitem, "setitem($module, a, b, c, /)\n--\n\nSame as a[b] = c."),
        ("sub", FAST, op_sub, "sub($module, a, b, /)\n--\n\nSame as a - b."),
        ("truediv", FAST, op_truediv, "truediv($module, a, b, /)\n--\n\nSame as a / b."),
        ("truth", O, op_truth, "truth($module, a, /)\n--\n\nReturn True if a is true, False otherwise."),
        ("xor", FAST, op_xor, "xor($module, a, b, /)\n--\n\nSame as a ^ b."),
        ("_compare_digest", FAST, op_compare_digest, "_compare_digest($module, a, b, /)\n--\n\nReturn 'a == b'.\n\nThis function uses an approach designed to prevent\ntiming analysis, making it appropriate for cryptography.\n\na and b must both be of the same type: either str (ASCII only),\nor any bytes-like object.\n\nNote: If a and b are of different lengths, or if an error occurs,\na timing attack could theoretically reveal information about the\ntypes and lengths of a and b--but not their values."),
    ];
    let (m, d) = fc::new_module("_operator", methods, Some("Operator interface.\n\nThis module exports a set of functions implemented in C corresponding\nto the intrinsic operators of Python.  For example, operator.add(x, y)\nis equivalent to the expression x+y.  The function names are those\nused for special methods; variants without leading and trailing\n'__' are also provided for convenience."))?;
    fc::add_type(&d, "attrgetter", attrgetter_type())?;
    fc::add_type(&d, "itemgetter", itemgetter_type())?;
    fc::add_type(&d, "methodcaller", methodcaller_type())?;
    Ok(m)
}
