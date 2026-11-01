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

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

// ============================================================== tuple

unsafe extern "C" fn tuple_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let items = tuple::tuple_items(o);
        if items.is_empty() {
            return str::new_str("()").into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(str::new_str("(...)"));
            }
            let mut sb = str::StrBuilder::new();
            sb.push('(' as u32);
            let res = (|| {
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    sb.push_obj(&*ab::repr(it)?);
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            if items.len() == 1 {
                sb.push(',' as u32);
            }
            sb.push(')' as u32);
            Ok(sb.finish())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn tuple_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        match tuple::tuple_hash(&*o) {
            Ok(h) => h,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn tuple_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { tuple::tuple_len(&*o) as isize }
}
unsafe extern "C" fn tuple_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let items = tuple::tuple_items(&*o);
        if i < 0 || i as usize >= items.len() {
            return into_c(Err(err!(IndexError, "tuple index out of range")));
        }
        items[i as usize].to_ref().into_raw()
    }
}
fn seq_subscript(items: &[&PyObject], k: &PyObject, what: &str) -> PyResult<SeqIdx> {
    if slice::is_slice(k) {
        let (start, _stop, step, len) = slice::indices(k, items.len() as isize)?;
        let mut v = Vec::with_capacity(len as usize);
        let mut i = start;
        for _ in 0..len {
            v.push(items[i as usize].to_ref());
            i += step;
        }
        return Ok(SeqIdx::Slice(v));
    }
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
        if i < 0 {
            i += items.len() as isize;
        }
        if i < 0 || i as usize >= items.len() {
            raise!(IndexError, "{} index out of range", what);
        }
        return Ok(SeqIdx::Item(items[i as usize].to_ref()));
    }
    raise!(TypeError, "{} indices must be integers or slices, not {}", what, tname(k));
}
enum SeqIdx {
    Item(PyRef),
    Slice(Vec<PyRef>),
}
unsafe extern "C" fn tuple_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c(seq_subscript(tuple::tuple_items(&*o), &*k, "tuple").map(|r| match r {
            SeqIdx::Item(x) => x,
            SeqIdx::Slice(v) => tuple::new_tuple(v),
        }))
    }
}
fn seq_richcompare(a: &[&PyObject], b: &[&PyObject], op: c_int) -> PyResult<bool> {
    if (op == Py_EQ || op == Py_NE) && a.len() != b.len() {
        return Ok(op == Py_NE);
    }
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n {
        if !ab::rich_eq(a[i], b[i])? {
            break;
        }
        i += 1;
    }
    if i >= n {
        return Ok(cmp_result(a.len().cmp(&b.len()), op));
    }
    if op == Py_EQ {
        return Ok(false);
    }
    if op == Py_NE {
        return Ok(true);
    }
    ab::rich_compare_bool(a[i], b[i], op)
}
unsafe extern "C" fn tuple_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !tuple::is_tuple(b) {
            return ni();
        }
        into_c(seq_richcompare(tuple::tuple_items(a), tuple::tuple_items(b), op).map(bool_::new_bool))
    }
}
unsafe extern "C" fn tuple_contains(a: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        for it in tuple::tuple_items(&*a) {
            match ab::rich_eq(it, &*v) {
                Ok(true) => return 1,
                Ok(false) => {}
                Err(_) => return -1,
            }
        }
        0
    }
}
unsafe extern "C" fn tuple_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !tuple::is_tuple(b) {
            return into_c(Err(err!(TypeError, "can only concatenate tuple (not \"{}\") to tuple", tname(b))));
        }
        let mut v = tuple::tuple_to_vec(a);
        v.extend(tuple::tuple_to_vec(b));
        tuple::new_tuple(v).into_raw()
    }
}
unsafe extern "C" fn tuple_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if n <= 0 {
            return tuple::empty_tuple().into_raw();
        }
        if n == 1 && tuple::is_tuple_exact(a) {
            return a.to_ref().into_raw();
        }
        let items = tuple::tuple_to_vec(a);
        if items.len().checked_mul(n as usize).map_or(true, |t| t > (isize::MAX as usize) / 8) {
            return into_c(Err(memory_error()));
        }
        let mut v = Vec::with_capacity(items.len() * n as usize);
        for _ in 0..n {
            v.extend(items.iter().cloned());
        }
        tuple::new_tuple(v).into_raw()
    }
}
unsafe extern "C" fn tuple_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "tuple() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "tuple expected at most 1 argument, got {}", a.len());
            }
            let items = match a.first() {
                None => Vec::new(),
                Some(x) => ab::collect_iterable(x)?,
            };
            if tp == tuple::tuple_type() {
                return Ok(tuple::new_tuple(items));
            }
            let op = tuple::alloc_tuple_of(tp, items.len());
            let base = tuple::item_ptr(op);
            for (i, it) in items.into_iter().enumerate() {
                *base.add(i) = it.into_raw();
            }
            crate::gc::track(op);
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}
#[repr(C)]
pub struct SeqIterObj {
    ob_base: PyObject,
    idx: usize,
    seq: *mut PyObject,
}
static mut TUPLE_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut LIST_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
unsafe extern "C" fn tuple_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut TUPLE_ITER_TYPE, std::mem::size_of::<SeqIterObj>());
        (*(op as *mut SeqIterObj)).seq = (*o).to_ref().into_raw();
        op
    }
}
unsafe extern "C" fn tuple_iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut SeqIterObj);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        let items = tuple::tuple_items(&*it.seq);
        if it.idx < items.len() {
            let v = items[it.idx].to_ref();
            it.idx += 1;
            return v.into_raw();
        }
        let p = std::mem::replace(&mut it.seq, std::ptr::null_mut());
        decref(p);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn list_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut LIST_ITER_TYPE, std::mem::size_of::<SeqIterObj>());
        (*(op as *mut SeqIterObj)).seq = (*o).to_ref().into_raw();
        op
    }
}
unsafe extern "C" fn list_iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut SeqIterObj);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        let items = list::list_items(&*it.seq);
        if it.idx < items.len() {
            let v = items[it.idx].to_ref();
            it.idx += 1;
            return v.into_raw();
        }
        let p = std::mem::replace(&mut it.seq, std::ptr::null_mut());
        decref(p);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn seqiter_dealloc(o: *mut PyObject) {
    unsafe {
        let it = &mut *(o as *mut SeqIterObj);
        xdecref(it.seq);
        crate::alloc::free_object(o);
    }
}
fn tuple_count(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0;
    for it in tuple::tuple_items(o) {
        if ab::rich_eq(it, v)? {
            n += 1;
        }
    }
    Ok(int::new_int(n))
}
fn tuple_index(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    for (i, it) in tuple::tuple_items(o).iter().enumerate() {
        if ab::rich_eq(it, args[0])? {
            return Ok(int::new_int(i as i64));
        }
    }
    raise!(ValueError, "tuple.index(x): x not in tuple");
}
fn tuple_getnewargs(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![tuple::new_tuple(tuple::tuple_to_vec(o))]))
}

// ============================================================== list

unsafe extern "C" fn list_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if list::list_len(o) == 0 {
            return str::new_str("[]").into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(str::new_str("[...]"));
            }
            let mut sb = str::StrBuilder::new();
            sb.push('[' as u32);
            let res = (|| {
                let mut i = 0;
                while i < list::list_len(o) {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    let it = list::get(o, i).unwrap();
                    sb.push_obj(&*ab::repr(&it)?);
                    i += 1;
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            sb.push(']' as u32);
            Ok(sb.finish())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn list_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { list::list_len(&*o) as isize }
}
unsafe extern "C" fn list_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        match if i < 0 { None } else { list::get(&*o, i as usize) } {
            Some(v) => v.into_raw(),
            None => into_c(Err(err!(IndexError, "list index out of range"))),
        }
    }
}
unsafe extern "C" fn list_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let items: Vec<PyRef> = list::list_to_vec(&*o);
        let refs: Vec<&PyObject> = items.iter().map(|x| &**x).collect();
        into_c(seq_subscript(&refs, &*k, "list").map(|r| match r {
            SeqIdx::Item(x) => x,
            SeqIdx::Slice(v) => list::new_list(v),
        }))
    }
}
unsafe extern "C" fn list_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let (o, k) = (&*o, &*k);
        let r = (|| -> PyResult<()> {
            let n = list::list_len(o) as isize;
            if slice::is_slice(k) {
                let (start, stop, step, len) = slice::indices(k, n)?;
                if step == 1 {
                    let stop = stop.max(start);
                    let items = if v.is_null() { Vec::new() } else { ab::collect_iterable(&*v)? };
                    list::splice(o, start as usize, stop as usize, items);
                    return Ok(());
                }
                let mut cur = list::list_to_vec(o);
                let mut idxs: Vec<usize> = Vec::new();
                let mut i = start;
                for _ in 0..len {
                    idxs.push(i as usize);
                    i += step;
                }
                if v.is_null() {
                    idxs.sort_unstable();
                    for &ix in idxs.iter().rev() {
                        cur.remove(ix);
                    }
                } else {
                    let items = ab::collect_iterable(&*v)?;
                    if items.len() != idxs.len() {
                        raise!(
                            ValueError,
                            "attempt to assign sequence of size {} to extended slice of size {}",
                            items.len(),
                            idxs.len()
                        );
                    }
                    for (ix, it) in idxs.into_iter().zip(items) {
                        cur[ix] = it;
                    }
                }
                list::set_contents(o, cur);
                return Ok(());
            }
            let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))
                .map_err(|e| e)
                .or_else(|e| {
                    if exc_matches(exceptions::TypeError()) {
                        clear_exc();
                        Err(err!(TypeError, "list indices must be integers or slices, not {}", tname(k)))
                    } else {
                        Err(e)
                    }
                })?;
            if i < 0 {
                i += n;
            }
            if i < 0 || i >= n {
                raise!(IndexError, "list assignment index out of range");
            }
            if v.is_null() {
                drop(list::pop(o, i as usize));
            } else {
                list::set(o, i as usize, (*v).to_ref());
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn list_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !list::is_list(b) {
            return ni();
        }
        let x = list::list_to_vec(a);
        let y = list::list_to_vec(b);
        let xr: Vec<&PyObject> = x.iter().map(|r| &**r).collect();
        let yr: Vec<&PyObject> = y.iter().map(|r| &**r).collect();
        into_c(seq_richcompare(&xr, &yr, op).map(bool_::new_bool))
    }
}
unsafe extern "C" fn list_contains(a: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let mut i = 0;
        while let Some(it) = list::get(&*a, i) {
            match ab::rich_eq(&it, &*v) {
                Ok(true) => return 1,
                Ok(false) => {}
                Err(_) => return -1,
            }
            i += 1;
        }
        0
    }
}
unsafe extern "C" fn list_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !list::is_list(b) {
            return into_c(Err(err!(TypeError, "can only concatenate list (not \"{}\") to list", tname(b))));
        }
        let mut v = list::list_to_vec(a);
        v.extend(list::list_to_vec(b));
        list::new_list(v).into_raw()
    }
}
unsafe extern "C" fn list_inplace_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        match ab::collect_iterable(&*b) {
            Ok(v) => {
                for x in v {
                    list::append(&*a, x);
                }
                (*a).to_ref().into_raw()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn list_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let items = list::list_to_vec(&*a);
        if n <= 0 {
            return list::new_list_empty().into_raw();
        }
        if items.len().checked_mul(n as usize).map_or(true, |t| t > (isize::MAX as usize) / 8) {
            return into_c(Err(memory_error()));
        }
        let mut v = Vec::with_capacity(items.len() * n as usize);
        for _ in 0..n {
            v.extend(items.iter().cloned());
        }
        list::new_list(v).into_raw()
    }
}
unsafe extern "C" fn list_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "list() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "list expected at most 1 argument, got {}", a.len());
            }
            let items = match a.first() {
                None => Vec::new(),
                Some(x) => ab::collect_iterable(x)?,
            };
            list::set_contents(&*o, items);
            Ok(())
        })();
        int_into_c(r)
    }
}
fn list_append_m(o: &PyObject, v: &PyObject) -> PyResult {
    list::append(o, v.to_ref());
    Ok(none::none_ref())
}
fn list_extend_m(o: &PyObject, v: &PyObject) -> PyResult {
    for x in ab::collect_iterable(v)? {
        list::append(o, x);
    }
    Ok(none::none_ref())
}
fn list_pop_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 0, 1)?;
    let n = list::list_len(o) as isize;
    if n == 0 {
        raise!(IndexError, "pop from empty list");
    }
    let mut i = match args.first() {
        Some(a) => ab::index_as_ssize(a, Some(exceptions::IndexError()))?,
        None => -1,
    };
    if i < 0 {
        i += n;
    }
    if i < 0 || i >= n {
        raise!(IndexError, "pop index out of range");
    }
    Ok(list::pop(o, i as usize))
}
fn list_insert_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", args, 2, 2)?;
    let n = list::list_len(o) as isize;
    let mut i = ab::index_as_ssize(args[0], None)?;
    if i < 0 {
        i = (i + n).max(0);
    }
    let i = i.min(n);
    list::insert(o, i as usize, args[1].to_ref());
    Ok(none::none_ref())
}
fn list_sort_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("sort", &["key", "reverse"], 0, 0).kwonly(2);
    let [key, reverse] = crate::args::parse::<2>(&P, args, kw)?;
    let key = key.filter(|k| !k.is(none::none()));
    let rev = match reverse {
        Some(r) => ab::is_true(r)?,
        None => false,
    };
    list_methods::sort_list(o, key, rev)?;
    Ok(none::none_ref())
}
fn list_reverse_m(o: &PyObject) -> PyResult {
    let mut v = list::list_to_vec(o);
    v.reverse();
    list::set_contents(o, v);
    Ok(none::none_ref())
}
fn list_index_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    let mut i = 0;
    while let Some(it) = list::get(o, i) {
        if ab::rich_eq(&it, args[0])? {
            return Ok(int::new_int(i as i64));
        }
        i += 1;
    }
    raise!(ValueError, "list.index(x): x not in list");
}
fn list_count_m(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0;
    for it in list::list_to_vec(o) {
        if ab::rich_eq(&it, v)? {
            n += 1;
        }
    }
    Ok(int::new_int(n))
}
fn list_remove_m(o: &PyObject, v: &PyObject) -> PyResult {
    let mut i = 0;
    while let Some(it) = list::get(o, i) {
        if ab::rich_eq(&it, v)? {
            drop(list::pop(o, i));
            return Ok(none::none_ref());
        }
        i += 1;
    }
    raise!(ValueError, "list.remove(x): x not in list");
}
fn list_clear_m(o: &PyObject) -> PyResult {
    list::clear(o);
    Ok(none::none_ref())
}
fn list_copy_m(o: &PyObject) -> PyResult {
    Ok(list::new_list(list::list_to_vec(o)))
}

// ============================================================== dict

unsafe extern "C" fn dict_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if dict::dict_len(o) == 0 {
            return str::new_str("{}").into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(str::new_str("{...}"));
            }
            let mut sb = str::StrBuilder::new();
            sb.push('{' as u32);
            let res = (|| {
                for (i, (k, v)) in dict::items_vec(o).into_iter().enumerate() {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    sb.push_obj(&*ab::repr(&k)?);
                    sb.push_str(": ");
                    sb.push_obj(&*ab::repr(&v)?);
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            sb.push('}' as u32);
            Ok(sb.finish())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn dict_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(&*o) as isize }
}
unsafe extern "C" fn dict_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (o, k) = (&*o, &*k);
        match dict::get_item(o, k) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => {
                if !dict::is_dict_exact(o) {
                    if let Ok(Some(m)) = ab::lookup_special(o, crate::name!("__missing__")) {
                        return into_c(ab::call(&m, &[k]));
                    }
                }
                dict_methods::raise_key_error(k);
                std::ptr::null_mut()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn dict_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let (o, k) = (&*o, &*k);
        if v.is_null() {
            return match dict::del_item(o, k) {
                Ok(true) => 0,
                Ok(false) => {
                    dict_methods::raise_key_error(k);
                    -1
                }
                Err(_) => -1,
            };
        }
        int_into_c(dict::set_item(o, k, &*v))
    }
}
unsafe extern "C" fn dict_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match dict::contains(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn dict_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let l = list::new_list(dict::keys_vec(&*o));
        list_iter(l.as_ptr())
    }
}
unsafe extern "C" fn dict_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !dict::is_dict(b) || (op != Py_EQ && op != Py_NE) {
            return ni();
        }
        let r = (|| -> PyResult<bool> {
            if dict::dict_len(a) != dict::dict_len(b) {
                return Ok(false);
            }
            for (k, v) in dict::items_vec(a) {
                match dict::get_item(b, &k)? {
                    Some(v2) => {
                        if !ab::rich_eq(&v, &v2)? {
                            return Ok(false);
                        }
                    }
                    None => return Ok(false),
                }
            }
            Ok(true)
        })();
        into_c(r.map(|eq| bool_::new_bool(if op == Py_EQ { eq } else { !eq })))
    }
}
unsafe extern "C" fn dict_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if !op.is_null() {
            dict::init_dict_storage(op);
        }
        op
    }
}
unsafe extern "C" fn dict_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "dict expected at most 1 argument, got {}", a.len());
            }
            if let Some(src) = a.first() {
                dict_update_any(&*o, src)?;
            }
            if !kw.is_null() {
                dict::merge_dict(&*o, &*kw, true)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
pub fn dict_update_any(d: &PyObject, src: &PyObject) -> PyResult<()> {
    if dict::is_dict(src) {
        return dict::merge_dict(d, src, true);
    }
    if ab::hasattr(src, crate::name!("keys"))? {
        return dict_methods::dict_update_from(d, src);
    }
    for (i, item) in ab::collect_iterable(src)?.into_iter().enumerate() {
        let pair = match ab::collect_iterable(&item) {
            Ok(p) => p,
            Err(e) => {
                if exc_matches(exceptions::TypeError()) {
                    clear_exc();
                    raise!(TypeError, "cannot convert dictionary update sequence element #{} to a sequence", i);
                }
                return Err(e);
            }
        };
        if pair.len() != 2 {
            raise!(ValueError, "dictionary update sequence element #{} has length {}; 2 is required", i, pair.len());
        }
        dict::set_item(d, &pair[0], &pair[1])?;
    }
    Ok(())
}
fn dict_get_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", args, 1, 2)?;
    match dict::get_item(o, args[0])? {
        Some(v) => Ok(v),
        None => Ok(args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref)),
    }
}
fn dict_setdefault_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setdefault", args, 1, 2)?;
    if let Some(v) = dict::get_item(o, args[0])? {
        return Ok(v);
    }
    let d = args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref);
    dict::set_item(o, args[0], &d)?;
    Ok(d)
}
fn dict_pop_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 1, 2)?;
    match dict::get_item(o, args[0])? {
        Some(v) => {
            dict::del_item(o, args[0])?;
            Ok(v)
        }
        None => match args.get(1) {
            Some(d) => Ok(d.to_ref()),
            None => {
                dict_methods::raise_key_error(args[0]);
                Err(PyErr)
            }
        },
    }
}
fn dict_popitem_m(o: &PyObject) -> PyResult {
    match dict::pop_last(o) {
        Some((k, v)) => Ok(tuple::new_tuple(vec![k, v])),
        None => raise!(KeyError, "popitem(): dictionary is empty"),
    }
}
fn dict_keys_m(o: &PyObject) -> PyResult {
    crate::types::dictview::new_view(o, crate::types::dictview::Kind::Keys)
}
fn dict_values_m(o: &PyObject) -> PyResult {
    crate::types::dictview::new_view(o, crate::types::dictview::Kind::Values)
}
fn dict_items_m(o: &PyObject) -> PyResult {
    crate::types::dictview::new_view(o, crate::types::dictview::Kind::Items)
}
fn dict_update_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if pos.len() > 1 {
        raise!(TypeError, "update expected at most 1 argument, got {}", pos.len());
    }
    if let Some(src) = pos.first() {
        dict_update_any(o, src)?;
    }
    for (k, v) in kws {
        dict::set_item(o, k, v)?;
    }
    Ok(none::none_ref())
}
fn dict_clear_m(o: &PyObject) -> PyResult {
    dict::clear(o);
    Ok(none::none_ref())
}
fn dict_copy_m(o: &PyObject) -> PyResult {
    dict::copy(o)
}
fn dict_fromkeys_m(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fromkeys", args, 1, 2)?;
    let d = ab::call(cls, &[])?;
    let v = args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref);
    for k in ab::collect_iterable(args[0])? {
        ab::setitem(&d, &k, Some(&v))?;
    }
    Ok(d)
}

// ============================================================== set

unsafe extern "C" fn set_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let items = set::set_items(o);
        let tn = tname(o);
        if items.is_empty() {
            return str::new_str(&format!("{}()", tn)).into_raw();
        }
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(str::new_str(&format!("{}(...)", tn)));
            }
            let mut parts = Vec::new();
            let res = (|| {
                for it in &items {
                    parts.push(ab::repr_string(it)?);
                }
                Ok::<(), PyErr>(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            let inner = format!("{{{}}}", parts.join(", "));
            if set::is_set(o) && o.is_type(set::set_type()) {
                Ok(str::new_str(&inner))
            } else {
                Ok(str::new_str(&format!("{}({})", tn, inner)))
            }
        })();
        into_c(r)
    }
}
unsafe extern "C" fn set_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { set::set_len(&*o) as isize }
}
unsafe extern "C" fn set_contains_slot(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let k = &*k;
        let r = set::set_contains(&*o, k);
        match r {
            Ok(b) => b as c_int,
            Err(_) => {
                if set::is_set(k) && exc_matches(exceptions::TypeError()) {
                    clear_exc();
                    let f = set::new_frozenset_from_vec(set::set_items(k));
                    return match f.and_then(|f| set::set_contains(&*o, &f)) {
                        Ok(b) => b as c_int,
                        Err(_) => -1,
                    };
                }
                -1
            }
        }
    }
}
unsafe extern "C" fn set_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let l = list::new_list(set::set_items(&*o));
        list_iter(l.as_ptr())
    }
}
unsafe extern "C" fn set_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let s = set::so(&*o);
        xdecref(s.storage);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn set_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let s = set::alloc_set(tp)?;
            if crate::types::typeobject::is_subtype(tp, set::frozenset_type()) {
                if !kw.is_null() && dict::dict_len(&*kw) > 0 && tp == set::frozenset_type() {
                    raise!(TypeError, "frozenset() takes no keyword arguments");
                }
                let a = tuple::tuple_items(&*args);
                if a.len() > 1 {
                    raise!(TypeError, "frozenset expected at most 1 argument, got {}", a.len());
                }
                if let Some(it) = a.first() {
                    set::set_update(&s, it)?;
                }
            }
            Ok(s)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn set_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 && (*o).is_type(set::set_type()) {
                raise!(TypeError, "set() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "set expected at most 1 argument, got {}", a.len());
            }
            dict::clear(set::storage(&*o));
            if let Some(it) = a.first() {
                set::set_update(&*o, it)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn set_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !set::is_anyset(b) {
            return ni();
        }
        let r = (|| -> PyResult<bool> {
            let sub = |x: &PyObject, y: &PyObject| -> PyResult<bool> {
                for k in set::set_items(x) {
                    if !set::set_contains(y, &k)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            };
            let (la, lb) = (set::set_len(a), set::set_len(b));
            Ok(match op {
                Py_EQ => la == lb && sub(a, b)?,
                Py_NE => !(la == lb && sub(a, b)?),
                Py_LE => sub(a, b)?,
                Py_LT => la < lb && sub(a, b)?,
                Py_GE => sub(b, a)?,
                _ => la > lb && sub(b, a)?,
            })
        })();
        into_c(r.map(bool_::new_bool))
    }
}
unsafe extern "C" fn frozenset_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let s = set::so(&*o);
        if s.hash != -1 {
            return s.hash;
        }
        let mut h: u64 = 0;
        for k in set::set_items(&*o) {
            let kh = match ab::hash(&k) {
                Ok(x) => x as u64,
                Err(_) => return -1,
            };
            h ^= ((kh ^ (kh << 16) ^ 89869747) as u64).wrapping_mul(3644798167);
        }
        h ^= (set::set_len(&*o) as u64 + 1).wrapping_mul(1927868237);
        h ^= (h >> 11) ^ (h >> 25);
        h = h.wrapping_mul(69069).wrapping_add(907133923);
        let mut r = h as isize;
        if r == -1 {
            r = 590923713;
        }
        s.hash = r;
        r
    }
}
fn set_add_m(o: &PyObject, k: &PyObject) -> PyResult {
    set::set_add(o, k)?;
    Ok(none::none_ref())
}
fn set_discard_m(o: &PyObject, k: &PyObject) -> PyResult {
    set::set_discard(o, k)?;
    Ok(none::none_ref())
}
fn set_remove_m(o: &PyObject, k: &PyObject) -> PyResult {
    if !set::set_discard(o, k)? {
        dict_methods::raise_key_error(k);
        return Err(PyErr);
    }
    Ok(none::none_ref())
}
fn set_update_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    for a in args {
        set::set_update(o, a)?;
    }
    Ok(none::none_ref())
}
fn set_copy_m(o: &PyObject) -> PyResult {
    let s = set::alloc_set(if set::is_frozenset(o) { set::frozenset_type() } else { set::set_type() })?;
    set::set_update(&s, o)?;
    Ok(s)
}
fn set_pop_m(o: &PyObject) -> PyResult {
    match dict::pop_last(set::storage(o)) {
        Some((k, _)) => {
            let s = set::so(o);
            s.used -= 1;
            s.fill = s.used;
            Ok(k)
        }
        None => raise!(KeyError, "pop from an empty set"),
    }
}
fn set_clear_m(o: &PyObject) -> PyResult {
    dict::clear(set::storage(o));
    set::so(o).used = 0;
    Ok(none::none_ref())
}

// ============================================================== bytes / complex / slice / bytearray

unsafe extern "C" fn bytes_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { str::new_str(&bytes_repr_string(bytes::bytes_data(&*o), "b", "")).into_raw() }
}
pub fn bytes_repr_string(b: &[u8], prefix: &str, suffix: &str) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let q = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::with_capacity(b.len() + 3);
    out.push_str(prefix);
    out.push(q);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            _ if c as char == q => {
                out.push('\\');
                out.push(q);
            }
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\x{:02x}", c)),
        }
    }
    out.push(q);
    out.push_str(suffix);
    out
}
unsafe extern "C" fn bytes_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { bytes::bytes_hash(&*o) }
}
unsafe extern "C" fn bytes_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !bytes::is_bytes(b) {
            if (op == Py_EQ || op == Py_NE) && str::is_str(b) && crate::modules::sys::bytes_warning() > 0 {
                if crate::modules::warnings::warn(exceptions::BytesWarning(), "Comparison between bytes and string", 1).is_err() {
                    return std::ptr::null_mut();
                }
            }
            return ni();
        }
        bool_::new_bool(cmp_result(bytes::bytes_data(a).cmp(bytes::bytes_data(b)), op)).into_raw()
    }
}
unsafe extern "C" fn bytes_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { var_size(&*o) }
}
unsafe extern "C" fn bytes_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (o, k) = (&*o, &*k);
        let d = bytes::bytes_data(o);
        if slice::is_slice(k) {
            return into_c((|| {
                let (start, _s, step, len) = slice::indices(k, d.len() as isize)?;
                let mut v = Vec::with_capacity(len as usize);
                let mut i = start;
                for _ in 0..len {
                    v.push(d[i as usize]);
                    i += step;
                }
                Ok(bytes::new_bytes(&v))
            })());
        }
        if ab::has_index(k) {
            return into_c((|| {
                let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
                if i < 0 {
                    i += d.len() as isize;
                }
                if i < 0 || i as usize >= d.len() {
                    raise!(IndexError, "index out of range");
                }
                Ok(int::new_int(d[i as usize] as i64))
            })());
        }
        into_c(Err(err!(TypeError, "byte indices must be integers or slices, not {}", tname(k))))
    }
}
unsafe extern "C" fn bytes_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        match bytes::bytes_like_data(b) {
            Some(bd) => {
                let mut v = bytes::bytes_data(a).to_vec();
                v.extend_from_slice(&bd);
                bytes::new_bytes(&v).into_raw()
            }
            None => into_c(Err(err!(TypeError, "can't concat {} to {}", tname(b), tname(a)))),
        }
    }
}
unsafe extern "C" fn bytes_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let _ = flags;
        let v = &mut *view;
        v.buf = bytes::bytes_ptr(&*o) as *mut _;
        v.obj = (*o).to_ref().into_raw();
        v.len = var_size(&*o);
        v.itemsize = 1;
        v.readonly = 1;
        v.ndim = 1;
        v.format = c"B".as_ptr() as *mut _;
        v.shape = std::ptr::null_mut();
        v.strides = std::ptr::null_mut();
        v.suboffsets = std::ptr::null_mut();
        v.internal = std::ptr::null_mut();
        0
    }
}
unsafe extern "C" fn bytes_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let _ = kw;
            let a = tuple::tuple_items(&*args);
            let data: Vec<u8> = match a.first() {
                None => Vec::new(),
                Some(x) if str::is_str(x) => {
                    let enc = a.get(1).copied();
                    let errs = a.get(2).copied();
                    let r = crate::modules::codecs_core::encode_str(x, enc, errs)?;
                    bytes::bytes_data(&r).to_vec()
                }
                Some(x) if int::is_int(x) => {
                    let n = ab::as_i64(x)?;
                    if n < 0 {
                        raise!(ValueError, "negative count");
                    }
                    vec![0u8; n as usize]
                }
                Some(x) => match bytes::bytes_like_data(x) {
                    Some(d) => d,
                    None => {
                        let mut v = Vec::new();
                        for it in ab::collect_iterable(x)? {
                            let n = ab::as_i64(&it)?;
                            if !(0..256).contains(&n) {
                                raise!(ValueError, "bytes must be in range(0, 256)");
                            }
                            v.push(n as u8);
                        }
                        v
                    }
                },
            };
            if tp == bytes::bytes_type() {
                return Ok(bytes::new_bytes(&data));
            }
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, data.len() as isize + 1);
            if op.is_null() {
                return Err(PyErr);
            }
            set_var_size(op, data.len() as isize);
            (*(op as *mut PyBytesObject)).ob_shash = -1;
            std::ptr::copy_nonoverlapping(data.as_ptr(), bytes::bytes_ptr(&*op), data.len());
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}
fn bytes_decode_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("decode", &["encoding", "errors"], 0, 0);
    let [enc, errs] = crate::args::parse::<2>(&P, args, kw)?;
    crate::modules::codecs_core::decode_object(o, enc, errs)
}

unsafe extern "C" fn slice_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b, c) = slice::parts(&*o);
        into_c((|| Ok(str::new_str(&format!("slice({}, {}, {})", ab::repr_string(a)?, ab::repr_string(b)?, ab::repr_string(c)?))))())
    }
}
unsafe extern "C" fn slice_new(_tp: *mut PyTypeObject, args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = tuple::tuple_items(&*args);
        let n = none::none();
        match a.len() {
            1 => slice::new_slice(n, a[0], n).into_raw(),
            2 => slice::new_slice(a[0], a[1], n).into_raw(),
            3 => slice::new_slice(a[0], a[1], a[2]).into_raw(),
            0 => into_c(Err(err!(TypeError, "slice expected at least 1 argument, got 0"))),
            k => into_c(Err(err!(TypeError, "slice expected at most 3 arguments, got {}", k))),
        }
    }
}
fn slice_start(o: &PyObject) -> PyResult {
    Ok(slice::parts(o).0.to_ref())
}
fn slice_stop(o: &PyObject) -> PyResult {
    Ok(slice::parts(o).1.to_ref())
}
fn slice_step(o: &PyObject) -> PyResult {
    Ok(slice::parts(o).2.to_ref())
}
fn slice_indices_m(o: &PyObject, n: &PyObject) -> PyResult {
    let len = ab::index_as_ssize(n, Some(exceptions::OverflowError()))?;
    if len < 0 {
        raise!(ValueError, "length should not be negative");
    }
    let (a, b, c, _) = slice::indices(o, len)?;
    Ok(tuple::new_tuple(vec![int::new_int(a as i64), int::new_int(b as i64), int::new_int(c as i64)]))
}
unsafe extern "C" fn slice_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let s = &*(o as *mut PySliceObject);
        xdecref(s.start);
        xdecref(s.stop);
        xdecref(s.step);
        crate::alloc::free_object(o);
    }
}

// ============================================================== registration

static mut STR_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut STR_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut FLOAT_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut TUPLE_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut TUPLE_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut LIST_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut LIST_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut DICT_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut DICT_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut SET_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut BYTES_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut BYTES_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut BYTES_BUF: PyBufferProcs = PyBufferProcs::ZERO;

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

        // ---- tuple
        let t = &mut *tuple::tuple_type();
        t.tp_name = c"tuple".as_ptr();
        t.tp_basicsize = std::mem::offset_of!(PyTupleObject, ob_item) as isize;
        t.tp_itemsize = 8;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_TUPLE_SUBCLASS | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(tuple::tuple_dealloc);
        t.tp_traverse = Some(tuple::tuple_traverse);
        t.tp_repr = Some(tuple_repr);
        t.tp_hash = Some(tuple_hash_slot);
        t.tp_richcompare = Some(tuple_richcompare);
        t.tp_iter = Some(tuple_iter);
        t.tp_new = Some(tuple_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_doc = c"Built-in immutable sequence.\n\nIf no argument is given, the constructor returns an empty tuple.\nIf iterable is specified the tuple is initialized from iterable's items.\n\nIf the argument is a tuple, the return value is the same object.".as_ptr();
        let s = &mut *(&raw mut TUPLE_SEQ);
        s.sq_length = Some(tuple_len_slot);
        s.sq_concat = Some(tuple_concat);
        s.sq_repeat = Some(tuple_repeat);
        s.sq_item = Some(tuple_item);
        s.sq_contains = Some(tuple_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut TUPLE_MAP);
        m.mp_length = Some(tuple_len_slot);
        m.mp_subscript = Some(tuple_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::mdefs![
            ("count", O, tuple_count, "count($self, value, /)\n--\n\nReturn number of occurrences of value."),
            ("index", FAST, tuple_index, "index($self, value, start=0, stop=sys.maxsize, /)\n--\n\nReturn first index of value."),
            ("__getnewargs__", NOARGS, tuple_getnewargs),
        ];
        iter_type(&raw mut TUPLE_ITER_TYPE, c"tuple_iterator", tuple_iter_next, seqiter_dealloc, std::mem::size_of::<SeqIterObj>());

        // ---- list
        let t = &mut *list::list_type();
        t.tp_name = c"list".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyListObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_LIST_SUBCLASS | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(list::list_dealloc);
        t.tp_traverse = Some(list::list_traverse);
        t.tp_clear = Some(list::list_clear_slot);
        t.tp_repr = Some(list_repr);
        t.tp_hash = Some(slots::hash_not_implemented);
        t.tp_richcompare = Some(list_richcompare);
        t.tp_iter = Some(list_iter);
        t.tp_init = Some(list_init);
        t.tp_new = Some(object_type::generic_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Built-in mutable sequence.\n\nIf no argument is given, the constructor creates a new empty list.\nThe argument must be an iterable if specified.".as_ptr();
        let s = &mut *(&raw mut LIST_SEQ);
        s.sq_length = Some(list_len_slot);
        s.sq_concat = Some(list_concat);
        s.sq_repeat = Some(list_repeat);
        s.sq_item = Some(list_item);
        s.sq_contains = Some(list_contains);
        s.sq_inplace_concat = Some(list_inplace_concat);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut LIST_MAP);
        m.mp_length = Some(list_len_slot);
        m.mp_subscript = Some(list_subscript);
        m.mp_ass_subscript = Some(list_ass_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::mdefs![
            ("append", O, list_append_m, "append($self, object, /)\n--\n\nAppend object to the end of the list."),
            ("extend", O, list_extend_m, "extend($self, iterable, /)\n--\n\nExtend list by appending elements from the iterable."),
            ("pop", FAST, list_pop_m, "pop($self, index=-1, /)\n--\n\nRemove and return item at index (default last)."),
            ("insert", FAST, list_insert_m, "insert($self, index, object, /)\n--\n\nInsert object before index."),
            ("sort", FASTKW, list_sort_m, "sort($self, /, *, key=None, reverse=False)\n--\n\nSort the list in ascending order and return None."),
            ("reverse", NOARGS, list_reverse_m, "reverse($self, /)\n--\n\nReverse *IN PLACE*."),
            ("index", FAST, list_index_m, "index($self, value, start=0, stop=sys.maxsize, /)\n--\n\nReturn first index of value."),
            ("count", O, list_count_m, "count($self, value, /)\n--\n\nReturn number of occurrences of value."),
            ("remove", O, list_remove_m, "remove($self, value, /)\n--\n\nRemove first occurrence of value."),
            ("clear", NOARGS, list_clear_m, "clear($self, /)\n--\n\nRemove all items from list."),
            ("copy", NOARGS, list_copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of the list."),
        ];
        iter_type(&raw mut LIST_ITER_TYPE, c"list_iterator", list_iter_next, seqiter_dealloc, std::mem::size_of::<SeqIterObj>());

        // ---- dict
        let t = &mut *dict::dict_type();
        t.tp_name = c"dict".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyDictObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_DICT_SUBCLASS | Py_TPFLAGS_MAPPING;
        t.tp_dealloc = Some(dict::dict_dealloc);
        t.tp_traverse = Some(dict::dict_traverse);
        t.tp_clear = Some(dict::dict_clear_slot);
        t.tp_repr = Some(dict_repr);
        t.tp_hash = Some(slots::hash_not_implemented);
        t.tp_richcompare = Some(dict_richcompare);
        t.tp_iter = Some(dict_iter);
        t.tp_new = Some(dict_new);
        t.tp_init = Some(dict_init);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"dict() -> new empty dictionary\ndict(mapping) -> new dictionary initialized from a mapping object's\n    (key, value) pairs\ndict(iterable) -> new dictionary initialized as if via:\n    d = {}\n    for k, v in iterable:\n        d[k] = v\ndict(**kwargs) -> new dictionary initialized with the name=value pairs\n    in the keyword argument list.  For example:  dict(one=1, two=2)".as_ptr();
        let s = &mut *(&raw mut DICT_SEQ);
        s.sq_contains = Some(dict_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut DICT_MAP);
        m.mp_length = Some(dict_len_slot);
        m.mp_subscript = Some(dict_subscript);
        m.mp_ass_subscript = Some(dict_ass_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::mdefs![
            ("get", FAST, dict_get_m, "get($self, key, default=None, /)\n--\n\nReturn the value for key if key is in the dictionary, else default."),
            ("setdefault", FAST, dict_setdefault_m, "setdefault($self, key, default=None, /)\n--\n\nInsert key with a value of default if key is not in the dictionary."),
            ("pop", FAST, dict_pop_m, "pop($self, key, default=<unrepresentable>, /)\n--\n\nD.pop(k[,d]) -> v, remove specified key and return the corresponding value."),
            ("popitem", NOARGS, dict_popitem_m, "popitem($self, /)\n--\n\nRemove and return a (key, value) pair as a 2-tuple."),
            ("keys", NOARGS, dict_keys_m, "keys($self, /)\n--\n\nReturn a set-like object providing a view on the dict's keys."),
            ("values", NOARGS, dict_values_m, "values($self, /)\n--\n\nReturn an object providing a view on the dict's values."),
            ("items", NOARGS, dict_items_m, "items($self, /)\n--\n\nReturn a set-like object providing a view on the dict's items."),
            ("update", FASTKW, dict_update_m, "D.update([E, ]**F) -> None.  Update D from mapping/iterable E and F."),
            ("clear", NOARGS, dict_clear_m, "clear($self, /)\n--\n\nRemove all items from the dict."),
            ("copy", NOARGS, dict_copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of the dict."),
            ("fromkeys", FAST, dict_fromkeys_m, "fromkeys($type, iterable, value=None, /)\n--\n\nCreate a new dictionary with keys from iterable and values set to value."; METH_CLASS),
        ];

        // ---- set / frozenset
        for (tp, nm) in [(set::set_type(), c"set"), (set::frozenset_type(), c"frozenset")] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<set::PySetObject>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(set_dealloc);
            t.tp_repr = Some(set_repr);
            t.tp_richcompare = Some(set_richcompare);
            t.tp_iter = Some(set_iter);
            t.tp_new = Some(set_new);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_weaklistoffset = std::mem::offset_of!(set::PySetObject, weakreflist) as isize;
            let s = &mut *(&raw mut SET_SEQ);
            s.sq_length = Some(set_len_slot);
            s.sq_contains = Some(set_contains_slot);
            t.tp_as_sequence = s;
        }
        let t = &mut *set::set_type();
        t.tp_hash = Some(slots::hash_not_implemented);
        t.tp_init = Some(set_init);
        t.tp_methods = crate::mdefs![
            ("add", O, set_add_m),
            ("discard", O, set_discard_m),
            ("remove", O, set_remove_m),
            ("update", FAST, set_update_m),
            ("copy", NOARGS, set_copy_m),
            ("pop", NOARGS, set_pop_m),
            ("clear", NOARGS, set_clear_m),
        ];
        let t = &mut *set::frozenset_type();
        t.tp_hash = Some(frozenset_hash);
        t.tp_methods = crate::mdefs![("copy", NOARGS, set_copy_m)];

        // ---- bytes
        let t = &mut *bytes::bytes_type();
        t.tp_name = c"bytes".as_ptr();
        t.tp_basicsize = std::mem::offset_of!(PyBytesObject, ob_sval) as isize;
        t.tp_itemsize = 1;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_BYTES_SUBCLASS;
        t.tp_repr = Some(bytes_repr);
        t.tp_hash = Some(bytes_hash_slot);
        t.tp_richcompare = Some(bytes_richcompare);
        t.tp_new = Some(bytes_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_alloc = Some(generic_alloc);
        let s = &mut *(&raw mut BYTES_SEQ);
        s.sq_length = Some(bytes_len_slot);
        s.sq_concat = Some(bytes_concat);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut BYTES_MAP);
        m.mp_length = Some(bytes_len_slot);
        m.mp_subscript = Some(bytes_subscript);
        t.tp_as_mapping = m;
        let b = &mut *(&raw mut BYTES_BUF);
        b.bf_getbuffer = Some(bytes_getbuffer);
        t.tp_as_buffer = b;
        t.tp_methods = crate::mdefs![("decode", FASTKW, bytes_decode_m)];

        // ---- bytearray (placeholder type)
        let t = &mut *bytearray::bytearray_type();
        t.tp_name = c"bytearray".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyByteArrayObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_getattro = Some(object_generic_getattro);

        // ---- slice
        let t = &mut *slice::slice_type();
        t.tp_name = c"slice".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PySliceObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(slice_dealloc);
        t.tp_repr = Some(slice_repr);
        t.tp_new = Some(slice_new);
        t.tp_hash = Some(slots::hash_not_implemented);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![("start", slice_start), ("stop", slice_stop), ("step", slice_step)];
        t.tp_methods = crate::mdefs![("indices", O, slice_indices_m)];
    }
}
