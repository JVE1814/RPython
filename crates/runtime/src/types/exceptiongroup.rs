//! BaseExceptionGroup / ExceptionGroup (PEP 654): construction rules, `derive`, `split`,
//! `subgroup`, str/repr, and the interpreter support for `except*`
//! (`eg_match` for CHECK_EG_MATCH and `prep_reraise_star` for the re-raise intrinsic).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::exceptions::{self as exc, ExcGroupExc};
use crate::types::typeobject::*;
use crate::types::{dict, list, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::c_int;

fn eg(o: &PyObject) -> &mut ExcGroupExc {
    unsafe { &mut *(o.as_ptr() as *mut ExcGroupExc) }
}

pub fn is_exception_group(o: &PyObject) -> bool {
    is_subtype(o.type_ptr(), exc::BaseExceptionGroup())
}

/// The `exceptions` tuple of a group.
pub fn eg_exceptions(o: &PyObject) -> PyRef {
    let p = eg(o).excs;
    if p.is_null() { tuple::empty_tuple() } else { unsafe { PyRef::from_borrowed(p) } }
}
pub fn eg_message(o: &PyObject) -> PyRef {
    let p = eg(o).msg;
    if p.is_null() { pystr::new_str("") } else { unsafe { PyRef::from_borrowed(p) } }
}

fn set_ptr(slot: &mut *mut PyObject, v: Option<PyRef>) {
    let old = std::mem::replace(slot, v.map_or(std::ptr::null_mut(), |v| v.into_raw()));
    unsafe { xdecref(old) };
}

unsafe extern "C" fn eg_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(eg_new_impl(tp, &*args, kw)) }
}

fn eg_new_impl(tp: *mut PyTypeObject, args: &PyObject, kw: *mut PyObject) -> PyResult {
    if !kw.is_null() && dict::dict_len(unsafe { &*kw }) > 0 {
        raise!(TypeError, "{}() takes no keyword arguments", type_name(unsafe { &*tp }));
    }
    let n = tuple::tuple_len(args);
    if n != 2 {
        raise!(TypeError, "BaseExceptionGroup.__new__() takes exactly 2 arguments ({} given)", n);
    }
    let message = tuple::tuple_get(args, 0);
    let excs = tuple::tuple_get(args, 1);
    if !pystr::is_str(message) {
        raise!(TypeError, "BaseExceptionGroup.__new__() argument 1 must be str, not {}", tname(message));
    }
    if !is_sequence(excs) {
        raise!(TypeError, "second argument (exceptions) must be a sequence");
    }
    let excs_tuple = ab::sequence_tuple(excs)?;
    let items = tuple::tuple_items(&excs_tuple);
    if items.is_empty() {
        raise!(ValueError, "second argument (exceptions) must be a non-empty sequence");
    }
    let mut nested_base = false;
    for (i, e) in items.iter().enumerate() {
        if !exc::is_exception_instance(e) {
            raise!(ValueError, "Item {} of second argument (exceptions) is not an exception", i);
        }
        if !is_subtype(e.type_ptr(), exc::Exception()) {
            nested_base = true;
        }
    }
    let mut cls = tp;
    if cls == exc::BaseExceptionGroup() {
        if !nested_base {
            cls = exc::ExceptionGroup();
        }
    } else if cls == exc::ExceptionGroup() {
        if nested_base {
            raise!(TypeError, "Cannot nest BaseExceptions in an ExceptionGroup");
        }
    } else if nested_base && is_subtype(cls, exc::Exception()) {
        raise!(TypeError, "Cannot nest BaseExceptions in '{}'", type_name(unsafe { &*cls }));
    }
    // repr of the original sequence when it is not a list/tuple (it may be mutated later)
    let excs_str = if list::is_list_exact(excs) || tuple::is_tuple_exact(excs) { None } else { Some(ab::repr(excs)?) };
    let base_new = unsafe { (*exc::BaseException()).tp_new.unwrap() };
    let o = unsafe { from_c(base_new(cls, args.as_ptr(), std::ptr::null_mut()))? };
    let e = eg(&o);
    set_ptr(&mut e.msg, Some(message.to_ref()));
    set_ptr(&mut e.excs, Some(excs_tuple));
    set_ptr(&mut e.excs_str, excs_str);
    Ok(o)
}

fn is_sequence(o: &PyObject) -> bool {
    if dict::is_dict(o) {
        return false;
    }
    let sq = o.ty().tp_as_sequence;
    !sq.is_null() && unsafe { (*sq).sq_item.is_some() }
}

unsafe extern "C" fn eg_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        if !kw.is_null() && dict::dict_len(&*kw) > 0 {
            let _ = err!(TypeError, "{}() takes no keyword arguments", type_name((*op).ty()));
            return -1;
        }
        let e = exc::exc(&*op);
        let old = std::mem::replace(&mut e.args, (*args).to_ref().into_raw());
        xdecref(old);
        0
    }
}

unsafe extern "C" fn eg_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let msg = eg_message(o);
        let n = tuple::tuple_len(&eg_exceptions(o));
        let r = ab::str(&msg).map(|m| {
            pystr::new_str(&format!(
                "{} ({} sub-exception{})",
                pystr::to_string_lossy(&m),
                n,
                if n == 1 { "" } else { "s" }
            ))
        });
        into_c(r)
    }
}

unsafe extern "C" fn eg_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(eg_repr_impl(&*op)) }
}

fn eg_repr_impl(o: &PyObject) -> PyResult {
    let e = eg(o);
    let msg = ab::repr_string(&eg_message(o))?;
    let excs_repr = if !e.excs_str.is_null() {
        pystr::to_string_lossy(unsafe { &*e.excs_str })
    } else {
        let excs = eg_exceptions(o);
        let args = exc::exc_args(o);
        let as_list = tuple::tuple_len(&args) >= 2 && list::is_list(tuple::tuple_get(&args, 1));
        if as_list {
            let mut parts = Vec::new();
            for x in tuple::tuple_items(&excs) {
                parts.push(ab::repr_string(x)?);
            }
            format!("[{}]", parts.join(", "))
        } else {
            ab::repr_string(&excs)?
        }
    };
    Ok(pystr::new_str(&format!("{}({}, {})", type_name(o.ty()), msg, excs_repr)))
}

fn get_message(o: &PyObject) -> PyResult {
    Ok(eg_message(o))
}
fn get_exceptions(o: &PyObject) -> PyResult {
    Ok(eg_exceptions(o))
}

fn eg_derive(o: &PyObject, excs: &PyObject) -> PyResult {
    let msg = eg_message(o);
    ab::call(type_obj(exc::BaseExceptionGroup()), &[&msg, excs])
}

// ---------------------------------------------------------------- split / subgroup

enum Matcher {
    Type(PyRef),
    Predicate(PyRef),
    InstanceIds(Vec<usize>),
}

fn get_matcher(value: &PyObject) -> PyResult<Matcher> {
    if ab::is_callable(value) && !is_type(value) {
        return Ok(Matcher::Predicate(value.to_ref()));
    }
    if exc::is_exception_class(value) {
        return Ok(Matcher::Type(value.to_ref()));
    }
    if tuple::is_tuple(value) {
        if tuple::tuple_items(value).iter().all(|c| exc::is_exception_class(c)) {
            return Ok(Matcher::Type(value.to_ref()));
        }
    }
    raise!(TypeError, "expected an exception type, a tuple of exception types, or a callable (other than a class)");
}

fn matcher_matches(m: &Matcher, e: &PyObject) -> PyResult<bool> {
    match m {
        Matcher::Type(t) => crate::interp::eval::exception_matches(e, t),
        Matcher::Predicate(p) => {
            let r = ab::call(p, &[e])?;
            ab::is_true(&r)
        }
        Matcher::InstanceIds(ids) => Ok(ids.contains(&(e.as_ptr() as usize))),
    }
}

/// Create the subset group via `derive` and copy traceback/context/cause/notes.
fn exceptiongroup_subset(orig: &PyObject, excs: Vec<PyRef>) -> PyResult<Option<PyRef>> {
    if excs.is_empty() {
        return Ok(None);
    }
    let l = list::new_list(excs);
    let r = ab::call_method(orig, name!("derive"), &[&l])?;
    if !is_exception_group(&r) {
        raise!(TypeError, "derive must return an instance of BaseExceptionGroup");
    }
    if let Some(tb) = exc::exc_traceback(orig) {
        exc::set_exc_traceback(&r, Some(&tb));
    }
    let oe = exc::exc(orig);
    let (ctx, cause) = (oe.context, oe.cause);
    let re = exc::exc(&r);
    set_ptr(&mut re.context, if ctx.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(ctx) }) });
    set_ptr(&mut re.cause, if cause.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(cause) }) });
    if let Some(notes) = ab::getattr_opt(orig, name!("__notes__"))? {
        if is_sequence(&notes) && !pystr::is_str(&notes) {
            let copy = ab::sequence_list(&notes)?;
            ab::setattr(&r, name!("__notes__"), &copy)?;
        }
    }
    Ok(Some(r))
}

struct SplitResult {
    matched: Option<PyRef>,
    rest: Option<PyRef>,
}

fn split_recursive(e: &PyObject, m: &Matcher, construct_rest: bool) -> PyResult<SplitResult> {
    if matcher_matches(m, e)? {
        return Ok(SplitResult { matched: Some(e.to_ref()), rest: None });
    }
    if !is_exception_group(e) {
        return Ok(SplitResult { matched: None, rest: if construct_rest { Some(e.to_ref()) } else { None } });
    }
    let excs = eg_exceptions(e);
    let mut match_list = Vec::new();
    let mut rest_list = Vec::new();
    let ts = crate::threads::current();
    for x in tuple::tuple_items(&excs) {
        crate::interp::enter_recursive_call(ts, " in exceptiongroup_split_recursive")?;
        let r = split_recursive(x, m, construct_rest);
        crate::interp::leave_recursive_call(ts);
        let r = r?;
        if let Some(mm) = r.matched {
            match_list.push(mm);
        }
        if let Some(rr) = r.rest {
            rest_list.push(rr);
        }
    }
    let matched = exceptiongroup_subset(e, match_list)?;
    let rest = if construct_rest { exceptiongroup_subset(e, rest_list)? } else { None };
    Ok(SplitResult { matched, rest })
}

fn eg_split(o: &PyObject, value: &PyObject) -> PyResult {
    let m = get_matcher(value)?;
    let r = split_recursive(o, &m, true)?;
    Ok(tuple::new_tuple(vec![r.matched.unwrap_or_else(none::none_ref), r.rest.unwrap_or_else(none::none_ref)]))
}

fn eg_subgroup(o: &PyObject, value: &PyObject) -> PyResult {
    let m = get_matcher(value)?;
    let r = split_recursive(o, &m, false)?;
    Ok(r.matched.unwrap_or_else(none::none_ref))
}

fn class_getitem(cls: &PyObject, arg: &PyObject) -> PyResult {
    crate::types::genericalias::new_generic_alias(cls, arg)
}

pub fn setup_type(tp: *mut PyTypeObject, is_base: bool) {
    if !is_base {
        // class ExceptionGroup(BaseExceptionGroup, Exception)
        unsafe {
            let bases = tuple::new_tuple(vec![type_ref(exc::BaseExceptionGroup()), type_ref(exc::Exception())]);
            (*tp).tp_bases = bases.into_raw();
        }
        return;
    }
    unsafe {
        let t = &mut *tp;
        t.tp_new = Some(eg_new);
        t.tp_init = Some(eg_init);
        t.tp_str = Some(eg_str);
        t.tp_repr = Some(eg_repr);
        t.tp_getset = crate::getset_table![
            ("message", get_message; "exception message"),
            ("exceptions", get_exceptions; "nested exceptions"),
        ];
        t.tp_methods = crate::mdefs![
            ("derive", O, eg_derive),
            ("split", O, eg_split),
            ("subgroup", O, eg_subgroup),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];
    }
}

/// Create `BaseExceptionGroup(msg, excs)` (ExceptionGroup when possible).
pub fn create_exception_group(msg: &str, excs: Vec<PyRef>) -> PyResult {
    let m = pystr::new_str(msg);
    let l = list::new_list(excs);
    ab::call(type_obj(exc::BaseExceptionGroup()), &[&m, &l])
}

// ---------------------------------------------------------------- except*

/// CHECK_EG_MATCH: returns (rest, match); either may be None.
pub fn eg_match(e: &PyObject, t: &PyObject) -> PyResult<(PyRef, PyRef)> {
    if e.is(none::none()) {
        return Ok((none::none_ref(), none::none_ref()));
    }
    if crate::interp::eval::exception_matches(e, t)? {
        // full match
        if is_exception_group(e) {
            return Ok((none::none_ref(), e.to_ref()));
        }
        // naked exception: wrap it
        let wrapped = create_exception_group("", vec![e.to_ref()])?;
        if let Some(tb) = exc::exc_traceback(e) {
            exc::set_exc_traceback(&wrapped, Some(&tb));
        }
        return Ok((none::none_ref(), wrapped));
    }
    if is_exception_group(e) {
        let pair = ab::call_method(e, name!("split"), &[t])?;
        if !tuple::is_tuple(&pair) {
            raise!(TypeError, "{}.split must return a tuple, not {}", type_name(e.ty()), tname(&pair));
        }
        if tuple::tuple_len(&pair) < 2 {
            raise!(TypeError, "{}.split must return a 2-tuple, got tuple of size {}", type_name(e.ty()), tuple::tuple_len(&pair));
        }
        let m = tuple::tuple_get(&pair, 0).to_ref();
        let r = tuple::tuple_get(&pair, 1).to_ref();
        return Ok((r, m));
    }
    Ok((e.to_ref(), none::none_ref()))
}

fn same_metadata(a: &PyObject, b: &PyObject) -> bool {
    let (x, y) = (exc::exc(a), exc::exc(b));
    x.notes == y.notes && x.traceback == y.traceback && x.cause == y.cause && x.context == y.context
}

fn collect_leaf_ids(e: &PyObject, out: &mut Vec<usize>) -> PyResult<()> {
    if e.is(none::none()) {
        return Ok(());
    }
    if !is_exception_group(e) {
        out.push(e.as_ptr() as usize);
        return Ok(());
    }
    let excs = eg_exceptions(e);
    let ts = crate::threads::current();
    for x in tuple::tuple_items(&excs) {
        crate::interp::enter_recursive_call(ts, " in collect_exception_group_leaf_ids")?;
        let r = collect_leaf_ids(x, out);
        crate::interp::leave_recursive_call(ts);
        r?;
    }
    Ok(())
}

/// The subgroup of `eg` containing the leaves of the exceptions in `keep`.
fn exception_group_projection(e: &PyObject, keep: &[PyRef]) -> PyResult<Option<PyRef>> {
    if keep.is_empty() {
        return Ok(None);
    }
    let mut ids = Vec::new();
    for k in keep {
        collect_leaf_ids(k, &mut ids)?;
    }
    let r = split_recursive(e, &Matcher::InstanceIds(ids), false)?;
    Ok(r.matched)
}

/// Combine the exceptions raised/re-raised by the except* clauses (`excs`, a list) with
/// the original exception `orig`. Returns the exception to re-raise, or None.
pub fn prep_reraise_star(orig: &PyObject, excs: &PyObject) -> PyResult {
    let items: Vec<PyRef> = if list::is_list(excs) { list::list_to_vec(excs) } else { ab::collect_iterable(excs)? };
    if items.is_empty() {
        return Ok(none::none_ref());
    }
    if !is_exception_group(orig) {
        // a naked exception was caught and wrapped: at most one exception to raise
        return Ok(items[0].clone());
    }
    let mut raised = Vec::new();
    let mut reraised = Vec::new();
    for e in items {
        if e.is(none::none()) {
            continue;
        }
        if same_metadata(&e, orig) {
            reraised.push(e);
        } else {
            raised.push(e);
        }
    }
    let reraised_eg = exception_group_projection(orig, &reraised)?;
    if raised.is_empty() {
        return Ok(reraised_eg.unwrap_or_else(none::none_ref));
    }
    if let Some(r) = reraised_eg {
        raised.push(r);
    }
    if raised.len() == 1 {
        return Ok(raised.pop().unwrap());
    }
    create_exception_group("", raised)
}
