//! abstract.h: call protocol (incl. vectorcall), number, sequence, mapping and
//! iterator protocols.

use super::*;
use crate::abstract_ as ab;
use crate::abstract_::NbSlot;
use crate::types::typeobject::{self as to};
use crate::types::{dict, int, list, none, slice, str as pystr, tuple};

// ---------------------------------------------------------------- calls

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Call(callable: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if args.is_null() || !tuple::is_tuple(&*args) {
            let _ = type_error("argument list must be a tuple");
            return std::ptr::null_mut();
        }
        if !kw.is_null() && !dict::is_dict(&*kw) {
            let _ = type_error("keyword list must be a dictionary");
            return std::ptr::null_mut();
        }
        let items = tuple::tuple_items(&*args);
        ret(ab::call_with_kwargs_dict(&*callable, items, opt(kw)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallObject(callable: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    unsafe {
        if args.is_null() {
            return ret(ab::call(&*callable, &[]));
        }
        PyObject_Call(callable, args, std::ptr::null_mut())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallNoArgs(callable: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::call(&*callable, &[])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallOneArg(callable: *mut PyObject, a: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::call(&*callable, &[&*a])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Vectorcall(callable: *mut PyObject, args: *const *mut PyObject, nargsf: usize, kwnames: *mut PyObject) -> *mut PyObject {
    unsafe { ab::vectorcall_raw(&*callable, args, nargsf, kwnames) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_VectorcallDict(callable: *mut PyObject, args: *const *mut PyObject, nargsf: usize, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let a: &[&PyObject] = if n == 0 { &[] } else { std::slice::from_raw_parts(args as *const &PyObject, n) };
        ret(ab::call_with_kwargs_dict(&*callable, a, opt(kw)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_VectorcallMethod(name: *mut PyObject, args: *const *mut PyObject, nargsf: usize, kwnames: *mut PyObject) -> *mut PyObject {
    unsafe {
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        if n == 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let slf = &**args;
        let m = match ab::getattr(slf, &*name) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        ab::vectorcall_raw(&m, args.add(1), n - 1, kwnames)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyVectorcall_NARGS(n: usize) -> Py_ssize_t {
    (n & !PY_VECTORCALL_ARGUMENTS_OFFSET) as Py_ssize_t
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyVectorcall_Function(callable: *mut PyObject) -> vectorcallfunc {
    unsafe { ab::vectorcall_func(&*callable) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyVectorcall_Call(callable: *mut PyObject, tup: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        // the slot at tp_vectorcall_offset, even without the type flag (heap subclasses of a
        // vectorcall type inherit only the offset, e.g. Python subclasses of msgspec's StructMeta)
        let off = (*callable).ty().tp_vectorcall_offset;
        let f = if off > 0 { *((callable as *mut u8).offset(off) as *const vectorcallfunc) } else { None };
        let Some(f) = f else {
            let _ = type_error(&format!("'{}' object does not support vectorcall", tname(&*callable)));
            return std::ptr::null_mut();
        };
        let items = tuple::tuple_items(&*tup);
        let mut v: Vec<*mut PyObject> = items.iter().map(|x| x.as_ptr()).collect();
        let n = v.len();
        let mut names = Vec::new();
        if !kw.is_null() {
            for (k, val) in dict::items_vec(&*kw) {
                names.push(k);
                v.push(val.into_raw());
            }
        }
        let kwn = if names.is_empty() { None } else { Some(tuple::new_tuple(names)) };
        let r = f(callable, v.as_ptr(), n, kwn.as_ref().map_or(std::ptr::null_mut(), |k| k.as_ptr()));
        for p in &v[n..] {
            decref(*p);
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyStack_AsDict(values: *const *mut PyObject, kwnames: *mut PyObject) -> *mut PyObject {
    unsafe {
        let d = dict::new_dict();
        let n = tuple::tuple_len(&*kwnames);
        for i in 0..n {
            if dict::set_item(&d, tuple::tuple_get(&*kwnames, i), &**values.add(i)).is_err() {
                return std::ptr::null_mut();
            }
        }
        d.into_raw()
    }
}

/// PyObject_CallMethod-style helper: getattr + call with a tuple/arguments.
pub(crate) unsafe fn call_with_built_args(callable: &PyObject, args: Option<PyRef>, is_tuple_semantics: bool) -> PyResult {
    match args {
        None => ab::call(callable, &[]),
        Some(a) => {
            if is_tuple_semantics && tuple::is_tuple(&a) {
                ab::call(callable, tuple::tuple_items(&a))
            } else {
                ab::call(callable, &[&a])
            }
        }
    }
}

// ---------------------------------------------------------------- number protocol

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Check(o: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() {
            return 0;
        }
        let nb = (*o).ty().tp_as_number;
        if !nb.is_null() && ((*nb).nb_index.is_some() || (*nb).nb_int.is_some() || (*nb).nb_float.is_some()) {
            return 1;
        }
        crate::types::complex::is_complex(&*o) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIndex_Check(o: *mut PyObject) -> c_int {
    unsafe { ab::has_index(&*o) as c_int }
}

macro_rules! binop {
    ($name:ident, $slot:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                if a.is_null() || b.is_null() {
                    return null_arg();
                }
                ret(ab::binary_op(&*a, &*b, NbSlot::$slot))
            }
        }
    };
}
macro_rules! ibinop {
    ($name:ident, $slot:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                if a.is_null() || b.is_null() {
                    return null_arg();
                }
                ret(ab::inplace_op(&*a, &*b, NbSlot::$slot))
            }
        }
    };
}
binop!(PyNumber_Add, Add);
binop!(PyNumber_Subtract, Sub);
binop!(PyNumber_Multiply, Mul);
binop!(PyNumber_MatrixMultiply, MatMul);
binop!(PyNumber_FloorDivide, FloorDiv);
binop!(PyNumber_TrueDivide, TrueDiv);
binop!(PyNumber_Remainder, Mod);
binop!(PyNumber_Divmod, Divmod);
binop!(PyNumber_Lshift, Lshift);
binop!(PyNumber_Rshift, Rshift);
binop!(PyNumber_And, And);
binop!(PyNumber_Xor, Xor);
binop!(PyNumber_Or, Or);
ibinop!(PyNumber_InPlaceAdd, Add);
ibinop!(PyNumber_InPlaceSubtract, Sub);
ibinop!(PyNumber_InPlaceMultiply, Mul);
ibinop!(PyNumber_InPlaceMatrixMultiply, MatMul);
ibinop!(PyNumber_InPlaceFloorDivide, FloorDiv);
ibinop!(PyNumber_InPlaceTrueDivide, TrueDiv);
ibinop!(PyNumber_InPlaceRemainder, Mod);
ibinop!(PyNumber_InPlaceLshift, Lshift);
ibinop!(PyNumber_InPlaceRshift, Rshift);
ibinop!(PyNumber_InPlaceAnd, And);
ibinop!(PyNumber_InPlaceXor, Xor);
ibinop!(PyNumber_InPlaceOr, Or);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Power(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe { if a.is_null() || b.is_null() { return null_arg(); } ret(ab::power(&*a, &*b, opt(m).unwrap_or(none::none()))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_InPlacePower(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let nb = (*a).ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_inplace_power {
                let r = f(a, b, opt(m).unwrap_or(none::none()).as_ptr());
                if r.is_null() {
                    return r;
                }
                if r != none::not_implemented().as_ptr() {
                    return r;
                }
                decref(r);
            }
        }
        PyNumber_Power(a, b, m)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Negative(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            return null_arg();
        }
        ret(ab::negative(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Positive(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            return null_arg();
        }
        ret(ab::positive(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Invert(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            return null_arg();
        }
        ret(ab::invert(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Absolute(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            return null_arg();
        }
        ret(ab::absolute(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Index(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::index(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_AsSsize_t(a: *mut PyObject, exc: *mut PyObject) -> Py_ssize_t {
    unsafe {
        if a.is_null() {
            null_arg();
            return -1;
        }
        let v = match ab::index(&*a) {
            Ok(v) => v,
            Err(_) => return -1,
        };
        match int::as_i64(&v) {
            Some(x) => x as Py_ssize_t,
            None => {
                if exc.is_null() {
                    if int::sign(&v) < 0 { isize::MIN } else { isize::MAX }
                } else {
                    let _ = crate::errors::new_err(
                        exc as *mut PyTypeObject,
                        &format!("cannot fit '{}' into an index-sized integer", tname(&*a)),
                    );
                    -1
                }
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Long(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(crate::types::int_methods::number_long(&*a))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Float(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if (*a).is_type(crate::types::float::float_type()) {
            return (*a).to_ref().into_raw();
        }
        ret(ab::call(to::type_obj(crate::types::float::float_type()), &[&*a]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_ToBase(a: *mut PyObject, base: c_int) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            return null_arg();
        }
        let f = match base {
            2 => "bin",
            8 => "oct",
            10 => "str",
            16 => "hex",
            _ => {
                let _ = system_error("PyNumber_ToBase: base must be 2, 8, 10 or 16");
                return std::ptr::null_mut();
            }
        };
        ret((|| -> PyResult {
            let i = ab::index(&*a)?;
            if base == 10 {
                return ab::str(&i);
            }
            let b = crate::builtins::builtins_module();
            let func = ab::getattr_str(&b, f)?;
            ab::call(&func, &[&i])
        })())
    }
}

// ---------------------------------------------------------------- sequence protocol

unsafe fn sq<'a>(o: &PyObject) -> Option<&'a PySequenceMethods> {
    unsafe {
        let p = o.ty().tp_as_sequence;
        if p.is_null() { None } else { Some(&*p) }
    }
}
unsafe fn mp<'a>(o: &PyObject) -> Option<&'a PyMappingMethods> {
    unsafe {
        let p = o.ty().tp_as_mapping;
        if p.is_null() { None } else { Some(&*p) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Check(o: *mut PyObject) -> c_int {
    unsafe {
        if dict::is_dict(&*o) {
            return 0;
        }
        sq(&*o).is_some_and(|s| s.sq_item.is_some()) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1;
        }
        if let Some(f) = sq(&*o).and_then(|s| s.sq_length) {
            return f(o);
        }
        if mp(&*o).is_some_and(|m| m.mp_length.is_some()) {
            let _ = type_error(&format!("{} is not a sequence", tname(&*o)));
            return -1;
        }
        let _ = type_error(&format!("object of type '{}' has no len()", tname(&*o)));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Length(o: *mut PyObject) -> Py_ssize_t {
    unsafe { PySequence_Size(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetItem(o: *mut PyObject, mut i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if let Some(s) = sq(&*o) {
            if let Some(f) = s.sq_item {
                if i < 0 {
                    if let Some(l) = s.sq_length {
                        let n = l(o);
                        if n < 0 {
                            return std::ptr::null_mut();
                        }
                        i += n;
                    }
                }
                return f(o, i);
            }
        }
        if mp(&*o).is_some_and(|m| m.mp_subscript.is_some()) {
            let _ = type_error(&format!("{} is not a sequence", tname(&*o)));
            return std::ptr::null_mut();
        }
        let _ = type_error(&format!("'{}' object does not support indexing", tname(&*o)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetItem(o: *mut PyObject, mut i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1;
        }
        if let Some(s) = sq(&*o) {
            if let Some(f) = s.sq_ass_item {
                if i < 0 {
                    if let Some(l) = s.sq_length {
                        let n = l(o);
                        if n < 0 {
                            return -1;
                        }
                        i += n;
                    }
                }
                return f(o, i, v);
            }
        }
        if mp(&*o).is_some_and(|m| m.mp_ass_subscript.is_some()) {
            let _ = type_error(&format!("{} is not a sequence", tname(&*o)));
            return -1;
        }
        let what = if v.is_null() { "deletion" } else { "assignment" };
        let _ = type_error(&format!("'{}' object does not support item {}", tname(&*o), what));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelItem(o: *mut PyObject, i: Py_ssize_t) -> c_int {
    unsafe { PySequence_SetItem(o, i, std::ptr::null_mut()) }
}
fn slice_obj(i1: Py_ssize_t, i2: Py_ssize_t) -> PyRef {
    slice::new_slice(&int::new_int(i1 as i64), &int::new_int(i2 as i64), none::none())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetSlice(o: *mut PyObject, i1: Py_ssize_t, i2: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if mp(&*o).is_some_and(|m| m.mp_subscript.is_some()) {
            return ret(ab::getitem(&*o, &slice_obj(i1, i2)));
        }
        let _ = type_error(&format!("'{}' object is unsliceable", tname(&*o)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetSlice(o: *mut PyObject, i1: Py_ssize_t, i2: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1;
        }
        if mp(&*o).is_some_and(|m| m.mp_ass_subscript.is_some()) {
            return ret_i(ab::setitem(&*o, &slice_obj(i1, i2), opt(v)));
        }
        let what = if v.is_null() { "deletion" } else { "assignment" };
        let _ = type_error(&format!("'{}' object doesn't support slice {}", tname(&*o), what));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelSlice(o: *mut PyObject, i1: Py_ssize_t, i2: Py_ssize_t) -> c_int {
    unsafe { PySequence_SetSlice(o, i1, i2, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() || b.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_concat) {
            return f(a, b);
        }
        if PySequence_Check(a) != 0 && PySequence_Check(b) != 0 {
            return ret(ab::binary_op(&*a, &*b, NbSlot::Add));
        }
        let _ = type_error(&format!("'{}' object can't be concatenated", tname(&*a)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceConcat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        if a.is_null() || b.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_inplace_concat) {
            return f(a, b);
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_concat) {
            return f(a, b);
        }
        if PySequence_Check(a) != 0 && PySequence_Check(b) != 0 {
            return ret(ab::inplace_op(&*a, &*b, NbSlot::Add));
        }
        let _ = type_error(&format!("'{}' object can't be concatenated", tname(&*a)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_repeat) {
            return f(a, n);
        }
        if PySequence_Check(a) != 0 {
            return ret(ab::binary_op(&*a, &int::new_int(n as i64), NbSlot::Mul));
        }
        let _ = type_error(&format!("'{}' object can't be repeated", tname(&*a)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceRepeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if a.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_inplace_repeat) {
            return f(a, n);
        }
        if let Some(f) = sq(&*a).and_then(|s| s.sq_repeat) {
            return f(a, n);
        }
        if PySequence_Check(a) != 0 {
            return ret(ab::inplace_op(&*a, &int::new_int(n as i64), NbSlot::Mul));
        }
        let _ = type_error(&format!("'{}' object can't be repeated", tname(&*a)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { ret_b(ab::contains(&*o, &*v)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_In(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { PySequence_Contains(o, v) }
}
unsafe fn iter_search(o: *mut PyObject, v: *mut PyObject, count: bool) -> Py_ssize_t {
    unsafe {
        if o.is_null() || v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let it = match ab::get_iter(&*o) {
            Ok(it) => it,
            Err(_) => {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    let _ = type_error(&format!("argument of type '{}' is not iterable", tname(&*o)));
                }
                return -1;
            }
        };
        let mut n: Py_ssize_t = 0;
        let mut i: Py_ssize_t = 0;
        loop {
            match ab::iter_next(&it) {
                Ok(Some(x)) => {
                    match ab::rich_eq(&x, &*v) {
                        Ok(true) => {
                            if !count {
                                return i;
                            }
                            n += 1;
                        }
                        Ok(false) => {}
                        Err(_) => return -1,
                    }
                    i += 1;
                }
                Ok(None) => break,
                Err(_) => return -1,
            }
        }
        if count {
            n
        } else {
            let _ = value_error("sequence.index(x): x not in sequence");
            -1
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Count(o: *mut PyObject, v: *mut PyObject) -> Py_ssize_t {
    unsafe { iter_search(o, v, true) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Index(o: *mut PyObject, v: *mut PyObject) -> Py_ssize_t {
    unsafe { iter_search(o, v, false) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Tuple(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::sequence_tuple(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_List(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::sequence_list(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Fast(o: *mut PyObject, m: *const c_char) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if list::is_list_exact(&*o) || tuple::is_tuple_exact(&*o) {
            return (*o).to_ref().into_raw();
        }
        match ab::get_iter(&*o) {
            Ok(it) => ret(ab::sequence_list(&it)),
            Err(_) => {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    let _ = type_error(&cs(m));
                }
                std::ptr::null_mut()
            }
        }
    }
}

// ---------------------------------------------------------------- mapping protocol

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Check(o: *mut PyObject) -> c_int {
    unsafe { (!o.is_null() && mp(&*o).is_some_and(|m| m.mp_subscript.is_some())) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1;
        }
        if let Some(f) = mp(&*o).and_then(|m| m.mp_length) {
            return f(o);
        }
        if sq(&*o).is_some_and(|s| s.sq_length.is_some()) {
            let _ = type_error(&format!("{} is not a mapping", tname(&*o)));
            return -1;
        }
        let _ = type_error(&format!("object of type '{}' has no len()", tname(&*o)));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Length(o: *mut PyObject) -> Py_ssize_t {
    unsafe { PyMapping_Size(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1;
        }
        ret_n(ab::length(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Length(o: *mut PyObject) -> Py_ssize_t {
    unsafe { PyObject_Size(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_LengthHint(o: *mut PyObject, d: Py_ssize_t) -> Py_ssize_t {
    unsafe { ret_n(ab::length_hint(&*o, d.max(0) as usize)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetItem(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() || k.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::getitem(&*o, &*k))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetItem(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || k.is_null() || v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        ret_i(ab::setitem(&*o, &*k, Some(&*v)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelItem(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || k.is_null() {
            let _ = bad_internal();
            return -1;
        }
        ret_i(ab::setitem(&*o, &*k, None))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelItemString(o: *mut PyObject, k: *const c_char) -> c_int {
    unsafe {
        if o.is_null() || k.is_null() {
            let _ = bad_internal();
            return -1;
        }
        match cstr_obj(k) {
            Ok(key) => ret_i(ab::setitem(&*o, &key, None)),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_GetItemString(o: *mut PyObject, k: *const c_char) -> *mut PyObject {
    unsafe {
        if o.is_null() || k.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        match cstr_obj(k) {
            Ok(key) => ret(ab::getitem(&*o, &key)),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_GetOptionalItem(o: *mut PyObject, k: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        if dict::is_dict_exact(&*o) {
            return match dict::get_item(&*o, &*k) {
                Ok(Some(v)) => {
                    *result = v.into_raw();
                    1
                }
                Ok(None) => 0,
                Err(_) => -1,
            };
        }
        match ab::getitem(&*o, &*k) {
            Ok(v) => {
                *result = v.into_raw();
                1
            }
            Err(_) => {
                if exc_matches(crate::types::exceptions::KeyError()) {
                    clear_exc();
                    0
                } else {
                    -1
                }
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_GetOptionalItemString(o: *mut PyObject, k: *const c_char, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        match cstr_obj(k) {
            Ok(key) => PyMapping_GetOptionalItem(o, key.as_ptr(), result),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_SetItemString(o: *mut PyObject, k: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || k.is_null() || v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        match cstr_obj(k) {
            Ok(key) => ret_i(ab::setitem(&*o, &key, Some(&*v))),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKeyWithError(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let mut r = std::ptr::null_mut();
        let x = PyMapping_GetOptionalItem(o, k, &mut r);
        xdecref(r);
        x
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKeyStringWithError(o: *mut PyObject, k: *const c_char) -> c_int {
    unsafe {
        let mut r = std::ptr::null_mut();
        let x = PyMapping_GetOptionalItemString(o, k, &mut r);
        xdecref(r);
        x
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKey(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let r = PyMapping_HasKeyWithError(o, k);
        if r < 0 {
            let msg = pystr::new_str("Exception ignored in PyMapping_HasKey(); consider using PyMapping_HasKeyWithError(), PyMapping_GetOptionalItem() or PyObject_GetItem()");
            super::errors::write_unraisable_msg(Some(msg), std::ptr::null_mut());
            return 0;
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKeyString(o: *mut PyObject, k: *const c_char) -> c_int {
    unsafe {
        let r = PyMapping_HasKeyStringWithError(o, k);
        if r < 0 {
            let msg = pystr::new_str("Exception ignored in PyMapping_HasKeyString(); consider using PyMapping_HasKeyStringWithError(), PyMapping_GetOptionalItemString() or PyMapping_GetItemString()");
            super::errors::write_unraisable_msg(Some(msg), std::ptr::null_mut());
            return 0;
        }
        r
    }
}
unsafe fn mapping_view(o: *mut PyObject, m: &str) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if dict::is_dict_exact(&*o) {
            let v: Vec<PyRef> = match m {
                "keys" => dict::keys_vec(&*o),
                "values" => dict::items_vec(&*o).into_iter().map(|(_, v)| v).collect(),
                _ => dict::items_vec(&*o).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect(),
            };
            return list::new_list(v).into_raw();
        }
        ret((|| -> PyResult {
            let r = ab::call_method_str(&*o, m, &[])?;
            if list::is_list_exact(&r) {
                return Ok(r);
            }
            match ab::get_iter(&r) {
                Ok(it) => ab::sequence_list(&it),
                Err(e) => {
                    if exc_matches(crate::types::exceptions::TypeError()) {
                        clear_exc();
                        return Err(type_error(&format!("{}.{}() returned a non-iterable (type {})", tname(&*o), m, tname(&r))));
                    }
                    Err(e)
                }
            }
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Keys(o: *mut PyObject) -> *mut PyObject {
    unsafe { mapping_view(o, "keys") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Values(o: *mut PyObject) -> *mut PyObject {
    unsafe { mapping_view(o, "values") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Items(o: *mut PyObject) -> *mut PyObject {
    unsafe { mapping_view(o, "items") }
}

// ---------------------------------------------------------------- iterators

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetIter(o: *mut PyObject) -> *mut PyObject {
    unsafe { if o.is_null() { return null_arg(); } ret(ab::get_iter(&*o)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAIter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let am = (*o).ty().tp_as_async;
        if am.is_null() || (*am).am_aiter.is_none() {
            let _ = type_error(&format!("'{}' object is not an async iterable", tname(&*o)));
            return std::ptr::null_mut();
        }
        let r = (*am).am_aiter.unwrap()(o);
        if r.is_null() {
            return r;
        }
        if PyAIter_Check(r) == 0 {
            let _ = type_error(&format!("aiter() returned not an async iterator of type '{}'", tname(&*r)));
            decref(r);
            return std::ptr::null_mut();
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_Check(o: *mut PyObject) -> c_int {
    unsafe {
        let f = (*o).ty().tp_iternext;
        (f.is_some() && f.map(|f| f as usize) != Some(crate::capi::typeobj::not_implemented_iternext as usize)) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyAIter_Check(o: *mut PyObject) -> c_int {
    unsafe {
        let am = (*o).ty().tp_as_async;
        (!am.is_null() && (*am).am_anext.is_some()) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_Next(it: *mut PyObject) -> *mut PyObject {
    unsafe {
        match ab::iter_next(&*it) {
            Ok(Some(x)) => x.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_NextItem(it: *mut PyObject, item: *mut *mut PyObject) -> c_int {
    unsafe {
        *item = std::ptr::null_mut();
        if (*it).ty().tp_iternext.is_none() {
            let _ = type_error(&format!("expected an iterator, got '{}'", tname(&*it)));
            return -1;
        }
        match ab::iter_next(&*it) {
            Ok(Some(x)) => {
                *item = x.into_raw();
                1
            }
            Ok(None) => 0,
            Err(_) => -1,
        }
    }
}
/// PySendResult: PYGEN_RETURN = 0, PYGEN_ERROR = -1, PYGEN_NEXT = 1
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_Send(iter: *mut PyObject, arg: *mut PyObject, presult: *mut *mut PyObject) -> c_int {
    unsafe {
        let am = (*iter).ty().tp_as_async;
        if !am.is_null() {
            if let Some(f) = (*am).am_send {
                return f(iter, arg, presult);
            }
        }
        let r = if arg.is_null() || none::is_none(&*arg) {
            match ab::iter_next(&*iter) {
                Ok(Some(x)) => Ok(x),
                Ok(None) => {
                    *presult = none::none_ref().into_raw();
                    return 0;
                }
                Err(e) => Err(e),
            }
        } else {
            ab::call_method_str(&*iter, "send", &[&*arg])
        };
        match r {
            Ok(x) => {
                *presult = x.into_raw();
                1
            }
            Err(_) => {
                if exc_matches(crate::types::exceptions::StopIteration()) {
                    let e = take_exc().unwrap();
                    *presult = crate::types::exceptions::stop_iteration_value(&e).into_raw();
                    return 0;
                }
                *presult = std::ptr::null_mut();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySeqIter_New(seq: *mut PyObject) -> *mut PyObject {
    unsafe { crate::types::iterobject::new_seq_iter((*seq).to_ref()).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCallIter_New(f: *mut PyObject, sentinel: *mut PyObject) -> *mut PyObject {
    unsafe { ret(crate::types::iterobject::new_callable_iter(&*f, &*sentinel)) }
}
