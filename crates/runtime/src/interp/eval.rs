//! The tier-0 bytecode interpreter loop.

use super::frame::{Frame, FrameState};
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::exceptions as exc;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, float, funcobject, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use rpy_compiler::bytecode::{self as bc, BinOp, Intrinsic1, Intrinsic2, Op};

pub enum Exit {
    Return(PyRef),
    Yield(PyRef),
}

macro_rules! push {
    ($f:expr, $v:expr) => {{
        let __v: *mut PyObject = $v;
        $f.slots[$f.stack_top] = __v;
        $f.stack_top += 1;
    }};
}
macro_rules! pop {
    ($f:expr) => {{
        $f.stack_top -= 1;
        std::mem::replace(&mut $f.slots[$f.stack_top], std::ptr::null_mut())
    }};
}
macro_rules! top {
    ($f:expr) => {
        $f.slots[$f.stack_top - 1]
    };
}
macro_rules! peek {
    ($f:expr, $n:expr) => {
        $f.slots[$f.stack_top - $n]
    };
}
/// Pop into an owned PyRef (must not be NULL).
macro_rules! popref {
    ($f:expr) => {
        unsafe { PyRef::from_raw(pop!($f)) }
    };
}
macro_rules! popopt {
    ($f:expr) => {
        unsafe { PyRef::from_raw_opt(pop!($f)) }
    };
}

fn binop_slot(op: BinOp) -> (ab::NbSlot, bool) {
    use ab::NbSlot::*;
    match op {
        BinOp::Add => (Add, false),
        BinOp::And => (And, false),
        BinOp::FloorDivide => (FloorDiv, false),
        BinOp::Lshift => (Lshift, false),
        BinOp::MatrixMultiply => (MatMul, false),
        BinOp::Multiply => (Mul, false),
        BinOp::Remainder => (Mod, false),
        BinOp::Or => (Or, false),
        BinOp::Power => (Pow, false),
        BinOp::Rshift => (Rshift, false),
        BinOp::Subtract => (Sub, false),
        BinOp::TrueDivide => (TrueDiv, false),
        BinOp::Xor => (Xor, false),
        BinOp::InplaceAdd => (Add, true),
        BinOp::InplaceAnd => (And, true),
        BinOp::InplaceFloorDivide => (FloorDiv, true),
        BinOp::InplaceLshift => (Lshift, true),
        BinOp::InplaceMatrixMultiply => (MatMul, true),
        BinOp::InplaceMultiply => (Mul, true),
        BinOp::InplaceRemainder => (Mod, true),
        BinOp::InplaceOr => (Or, true),
        BinOp::InplacePower => (Pow, true),
        BinOp::InplaceRshift => (Rshift, true),
        BinOp::InplaceSubtract => (Sub, true),
        BinOp::InplaceTrueDivide => (TrueDiv, true),
        BinOp::InplaceXor => (Xor, true),
    }
}

const BINOPS: [BinOp; 26] = [
    BinOp::Add,
    BinOp::And,
    BinOp::FloorDivide,
    BinOp::Lshift,
    BinOp::MatrixMultiply,
    BinOp::Multiply,
    BinOp::Remainder,
    BinOp::Or,
    BinOp::Power,
    BinOp::Rshift,
    BinOp::Subtract,
    BinOp::TrueDivide,
    BinOp::Xor,
    BinOp::InplaceAdd,
    BinOp::InplaceAnd,
    BinOp::InplaceFloorDivide,
    BinOp::InplaceLshift,
    BinOp::InplaceMatrixMultiply,
    BinOp::InplaceMultiply,
    BinOp::InplaceRemainder,
    BinOp::InplaceOr,
    BinOp::InplacePower,
    BinOp::InplaceRshift,
    BinOp::InplaceSubtract,
    BinOp::InplaceTrueDivide,
    BinOp::InplaceXor,
];

/// Fast paths for common binary operations on exact builtin types.
#[inline(always)]
fn binary_fast(op: BinOp, a: &PyObject, b: &PyObject) -> Option<PyRef> {
    super::specialize::binary_fast(op, a, b) // jit workstream: fast paths live in specialize.rs
}

pub fn binary_op(op: BinOp, a: &PyObject, b: &PyObject) -> PyResult {
    if let Some(r) = binary_fast(op, a, b) {
        return Ok(r);
    }
    let (slot, inplace) = binop_slot(op);
    if inplace { ab::inplace_op(a, b, slot) } else { ab::binary_op(a, b, slot) }
}

thread_local! {
    /// Operands that the innermost BINARY_OP of this thread took off its value stack and is
    /// passing to a type's number slot (0 = none). See `is_stack_temporary`.
    static STACK_OPERANDS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0, 0]) };
}

/// `binary_op` for operands that came off a frame's value stack (the interpreter and the JIT
/// helpers). While a slot function runs, the operands are recorded so that
/// `PyUnstable_Object_IsUniqueReferencedTemporary` can tell a stack temporary (which an
/// extension such as numpy may reuse for the result) from an object that C code owns.
pub fn binary_op_on_stack(op: BinOp, a: &PyObject, b: &PyObject) -> PyResult {
    if let Some(r) = binary_fast(op, a, b) {
        return Ok(r);
    }
    let saved = STACK_OPERANDS.with(|c| c.replace([a.as_ptr() as usize, b.as_ptr() as usize]));
    let (slot, inplace) = binop_slot(op);
    let r = if inplace { ab::inplace_op(a, b, slot) } else { ab::binary_op(a, b, slot) };
    STACK_OPERANDS.with(|c| c.set(saved));
    r
}

/// True if `p` is an operand of the binary operation the interpreter is currently
/// dispatching on this thread.
pub fn is_stack_temporary(p: *const PyObject) -> bool {
    let p = p as usize;
    p != 0 && STACK_OPERANDS.with(|c| c.get().contains(&p))
}

/// Find the exception handler for instruction `i` (first matching entry).
fn find_handler(code: &PyObject, i: u32) -> Option<bc::ExceptionEntry> {
    let d = crate::types::code::code_data(code);
    for e in d.exception_table.iter() {
        if e.start <= i && i < e.end {
            return Some(*e);
        }
    }
    None
}

/// Exception currently being handled (sys.exception()).
pub fn handled_exception(ts: &ThreadState) -> *mut PyObject {
    unsafe {
        let mut item = ts.c.exc_info;
        while !item.is_null() {
            let v = (*item).exc_value;
            if !v.is_null() && !(*v).is(none::none()) {
                return v;
            }
            item = (*item).previous_item;
        }
        std::ptr::null_mut()
    }
}

fn set_handled_exception(ts: &ThreadState, v: *mut PyObject) -> *mut PyObject {
    unsafe {
        let item = ts.c.exc_info;
        let old = (*item).exc_value;
        (*item).exc_value = v;
        old
    }
}

/// Attach __context__ = currently handled exception (avoiding cycles).
fn chain_context(ts: &ThreadState, e: &PyObject) {
    let handled = handled_exception(ts);
    if handled.is_null() || handled == e.as_ptr() {
        return;
    }
    if exc::exc_context(e).is_some() {
        return;
    }
    // break cycles: walk handled's context chain and cut a link to e
    unsafe {
        let mut o = handled;
        // the chain may itself contain a cycle: a second pointer moving at half
        // speed detects it (Floyd)
        let mut slow = handled;
        let mut step = false;
        loop {
            let ctx = exc::exc_context(&*o);
            match ctx {
                None => break,
                Some(c) => {
                    if c.as_ptr() == e.as_ptr() {
                        exc::set_context_and_cause(&*o, None);
                        break;
                    }
                    o = c.as_ptr();
                    if step {
                        slow = exc::exc_context(&*slow).map_or(std::ptr::null_mut(), |s| s.as_ptr());
                    }
                    step = !step;
                    if o == slow {
                        break;
                    }
                }
            }
        }
        exc::set_context_and_cause(e, Some(&*handled));
    }
}

/// Make sure the frame has a PyFrameObject and return it (borrowed).
pub fn frame_object(f: &mut Frame) -> *mut PyObject {
    if f.frame_obj.is_null() {
        f.frame_obj = crate::types::frameobject::new_frame_object(f);
    }
    f.frame_obj
}

fn add_traceback(f: &mut Frame, e: &PyObject) {
    let fo = frame_object(f);
    let prev = exc::exc_traceback(e);
    let lineno = f.lineno();
    let tb = crate::types::traceback::new_traceback(prev.as_deref(), unsafe { &*fo }, crate::types::code::lasti_bytes(f.code(), f.lasti as usize) as i32, lineno);
    exc::set_exc_traceback(e, Some(&tb));
}

fn normalize_raise(v: &PyObject) -> PyResult {
    if exc::is_exception_class(v) {
        let inst = ab::call(v, &[])?;
        if !exc::is_exception_instance(&inst) {
            raise!(
                TypeError,
                "calling {} should have returned an instance of BaseException, not {}",
                ab::repr_string(v)?,
                tname(&inst)
            );
        }
        return Ok(inst);
    }
    if exc::is_exception_instance(v) {
        return Ok(v.to_ref());
    }
    raise!(TypeError, "exceptions must derive from BaseException");
}

fn do_raise(ts: &ThreadState, e: Option<&PyObject>, cause: Option<&PyObject>) -> PyErr {
    let e = match e {
        None => {
            let h = handled_exception(ts);
            if h.is_null() {
                return err!(RuntimeError, "No active exception to reraise");
            }
            set_exc(unsafe { PyRef::from_borrowed(h) });
            return PyErr;
        }
        Some(e) => e,
    };
    let inst = match normalize_raise(e) {
        Ok(i) => i,
        Err(x) => return x,
    };
    if let Some(c) = cause {
        let c = if c.is(none::none()) {
            None
        } else if exc::is_exception_class(c) {
            match ab::call(c, &[]) {
                Ok(ci) => {
                    if !exc::is_exception_instance(&ci) {
                        return err!(
                            TypeError,
                            "calling {} should have returned an instance of BaseException, not {}",
                            ab::repr_string(c).unwrap_or_default(),
                            tname(&ci)
                        );
                    }
                    Some(ci)
                }
                Err(x) => return x,
            }
        } else if exc::is_exception_instance(c) {
            Some(c.to_ref())
        } else {
            return err!(TypeError, "exception causes must derive from BaseException");
        };
        exc::set_exc_cause(&inst, c.as_deref());
    }
    set_exc(inst);
    PyErr
}

fn name_error(f: &Frame, nm: &PyObject) -> PyErr {
    let s = pystr::to_string_lossy(nm);
    let _ = f;
    exc::name_error_with(exc::NameError(), &format!("name '{}' is not defined", s), nm)
}

pub(super) fn unbound_local(f: &Frame, idx: usize) -> PyErr {
    let d = crate::types::code::code_data(f.code());
    let nm = &d.localsplusnames[idx];
    let kind = d.localspluskinds[idx];
    if kind & bc::fast::FREE != 0 {
        return exc::name_error_with(
            exc::NameError(),
            &format!(
                "cannot access free variable '{}' where it is not associated with a value in enclosing scope",
                pystr::to_string_lossy(nm)
            ),
            nm,
        );
    }
    exc::name_error_with(
        exc::UnboundLocalError(),
        &format!("cannot access local variable '{}' where it is not associated with a value", pystr::to_string_lossy(nm)),
        nm,
    )
}

/// Look up a global name: globals then builtins.
#[inline]
pub(super) fn load_global(f: &Frame, nm: &PyObject) -> PyResult {
    let g = f.globals();
    if dict::is_dict_exact(g) {
        if let Some(v) = dict::get_item_str(g, nm) {
            return lazy_check(g, nm, v);
        }
    } else {
        // dict subclass: go through __getitem__ (honours __missing__)
        match ab::getitem(g, nm) {
            Ok(v) => return lazy_check(g, nm, v),
            Err(e) => {
                if !exc_matches(exc::KeyError()) {
                    return Err(e);
                }
                clear_exc();
            }
        }
    }
    let b = f.builtins();
    if dict::is_dict_exact(b) {
        if let Some(v) = dict::get_item_str(b, nm) {
            return lazy_check(b, nm, v);
        }
        return Err(name_error(f, nm));
    }
    match ab::getitem(b, nm) {
        Ok(v) => lazy_check(b, nm, v),
        Err(e) => {
            if exc_matches(exc::KeyError()) {
                clear_exc();
                return Err(name_error(f, nm));
            }
            Err(e)
        }
    }
}

pub(super) fn mapping_get(m: &PyObject, key: &PyObject) -> PyResult<Option<PyRef>> {
    if dict::is_dict_exact(m) {
        return dict::get_item(m, key);
    }
    match ab::getitem(m, key) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            if exc_matches(exc::KeyError()) {
                clear_exc();
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

pub(super) fn load_method(o: &PyObject, nm: &PyObject) -> PyResult<(PyRef, Option<PyRef>)> {
    let tp = o.ty();
    if tp.tp_getattro.map(|f| f as usize) == Some(object_generic_getattro as usize) {
        if let Some(d) = lookup(o.type_ptr(), nm) {
            if d.ty().tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
                // instance dict (or inline values) may shadow
                let shadow = match crate::shape::get(o, nm) {
                    crate::shape::Get::Found(v) => Some(v),
                    crate::shape::Get::Missing => None,
                    crate::shape::Get::NotValues => match instance_dict_existing(o) {
                        Some(id) => dict::get_item(&id, nm)?,
                        None => None,
                    },
                };
                match shadow {
                    Some(v) => return Ok((v, None)),
                    None => return Ok((d, Some(o.to_ref()))),
                }
            }
        }
    }
    Ok((ab::getattr(o, nm)?, None))
}

pub(super) fn unpack_sequence(seq: &PyObject, n: usize, out: &mut Vec<PyRef>) -> PyResult<()> {
    if tuple::is_tuple_exact(seq) || list::is_list_exact(seq) {
        let items: Vec<PyRef> = if tuple::is_tuple_exact(seq) { tuple::tuple_to_vec(seq) } else { list::list_to_vec(seq) };
        if items.len() != n {
            if items.len() < n {
                raise!(ValueError, "not enough values to unpack (expected {}, got {})", n, items.len());
            }
            raise!(ValueError, "too many values to unpack (expected {}, got {})", n, items.len());
        }
        *out = items;
        return Ok(());
    }
    let it = match ab::get_iter(seq) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(exc::TypeError()) && seq.ty().tp_iter.is_none() {
                clear_exc();
                raise!(TypeError, "cannot unpack non-iterable {} object", tname(seq));
            }
            return Err(e);
        }
    };
    for i in 0..n {
        match ab::iter_next(&it)? {
            Some(v) => out.push(v),
            None => raise!(ValueError, "not enough values to unpack (expected {}, got {})", n, i),
        }
    }
    if ab::iter_next(&it)?.is_some() {
        if let Ok(len) = ab::length(seq) {
            raise!(ValueError, "too many values to unpack (expected {}, got {})", n, len);
        }
        clear_exc();
        raise!(ValueError, "too many values to unpack (expected {})", n);
    }
    Ok(())
}

pub(super) fn unpack_ex(seq: &PyObject, before: usize, after: usize) -> PyResult<Vec<PyRef>> {
    let it = match ab::get_iter(seq) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(exc::TypeError()) && seq.ty().tp_iter.is_none() {
                clear_exc();
                raise!(TypeError, "cannot unpack non-iterable {} object", tname(seq));
            }
            return Err(e);
        }
    };
    let mut out = Vec::with_capacity(before + after + 1);
    for i in 0..before {
        match ab::iter_next(&it)? {
            Some(v) => out.push(v),
            None => raise!(ValueError, "not enough values to unpack (expected at least {}, got {})", before + after, i),
        }
    }
    let rest = ab::collect_iterable(&it)?;
    if rest.len() < after {
        raise!(
            ValueError,
            "not enough values to unpack (expected at least {}, got {})",
            before + after,
            before + rest.len()
        );
    }
    let split = rest.len() - after;
    let mut rest = rest;
    let tail: Vec<PyRef> = rest.split_off(split);
    out.push(list::new_list(rest));
    out.extend(tail);
    Ok(out)
}

fn check_exc_match_type(t: &PyObject, star: bool) -> PyResult<()> {
    let ok = |c: &PyObject| -> bool { exc::is_exception_class(c) };
    if tuple::is_tuple(t) {
        for c in tuple::tuple_items(t) {
            if !ok(c) {
                raise!(TypeError, "catching classes that do not inherit from BaseException is not allowed");
            }
            if star && is_subtype(as_type(c), exc::BaseExceptionGroup()) {
                raise!(TypeError, "catching ExceptionGroup with except* is not allowed. Use except instead.");
            }
        }
    } else {
        if !ok(t) {
            raise!(TypeError, "catching classes that do not inherit from BaseException is not allowed");
        }
        if star && is_subtype(as_type(t), exc::BaseExceptionGroup()) {
            raise!(TypeError, "catching ExceptionGroup with except* is not allowed. Use except instead.");
        }
    }
    Ok(())
}

pub fn exception_matches(e: &PyObject, t: &PyObject) -> PyResult<bool> {
    if tuple::is_tuple(t) {
        for c in tuple::tuple_items(t) {
            if exception_matches(e, c)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    Ok(is_subtype(e.type_ptr(), as_type(t)))
}

fn import_from(module: &PyObject, nm: &PyObject) -> PyResult {
    if let Some(v) = ab::getattr_opt(module, nm)? {
        return Ok(v);
    }
    // fall back to sys.modules[f"{module.__name__}.{name}"] (circular imports)
    let pkgname = match ab::getattr_opt(module, name!("__name__"))? {
        Some(n) if pystr::is_str(&n) => n,
        _ => {
            return Err(crate::import::import_error_with(
                &format!("cannot import name '{}' from '<unknown module name>' (unknown location)", pystr::to_string_lossy(nm)),
                None,
                None,
                Some(nm),
            ));
        }
    };
    let full = pystr::new_str(&format!("{}.{}", pystr::to_string_lossy(&pkgname), pystr::to_string_lossy(nm)));
    if let Some(m) = dict::get_item(&crate::modules::sys::modules_dict(), &full)? {
        return Ok(m);
    }
    crate::import::cannot_import_name(module, &pkgname, nm)
}

pub(super) fn format_value(v: &PyObject, spec: Option<&PyObject>) -> PyResult {
    match spec {
        None => {
            if pystr::is_str_exact(v) {
                return Ok(v.to_ref());
            }
            let empty = pystr::new_str("");
            ab::format(v, &empty)
        }
        Some(s) => ab::format(v, s),
    }
}

pub(super) fn call_intrinsic1(ts: &ThreadState, f: &mut Frame, which: u32, v: PyRef) -> PyResult {
    let which: Intrinsic1 = unsafe { std::mem::transmute(which as u8) };
    match which {
        Intrinsic1::Print => {
            let hook = crate::modules::sys::get_sys_attr("displayhook")
                .ok_or_else(|| err!(RuntimeError, "lost sys.displayhook"))?;
            ab::call(&hook, &[&v])
        }
        Intrinsic1::ImportStar => {
            let locals = if f.locals.is_null() {
                raise!(SystemError, "no locals found during 'import *'");
            } else {
                unsafe { &*f.locals }
            };
            crate::import::import_all_from(locals, &v)?;
            Ok(none::none_ref())
        }
        Intrinsic1::StopIterationError => {
            // v is the exception being raised out of a generator
            let flags = crate::types::code::code(f.code()).flags();
            let r = if exc::is_exception_instance(&v) && is_subtype(v.type_ptr(), exc::StopIteration()) {
                let msg = if flags & bc::co::ASYNC_GENERATOR != 0 {
                    "async generator raised StopIteration"
                } else if flags & bc::co::COROUTINE != 0 {
                    "coroutine raised StopIteration"
                } else {
                    "generator raised StopIteration"
                };
                Some(msg)
            } else if flags & bc::co::ASYNC_GENERATOR != 0 && is_subtype(v.type_ptr(), exc::StopAsyncIteration()) {
                Some("async generator raised StopAsyncIteration")
            } else {
                None
            };
            match r {
                Some(msg) => {
                    let new = exc::make_exception(exc::RuntimeError(), &[pystr::new_str(msg)])?;
                    exc::set_exc_cause(&new, Some(&v));
                    exc::set_context_and_cause(&new, Some(&v));
                    let _ = ts;
                    Ok(new)
                }
                None => Ok(v),
            }
        }
        Intrinsic1::UnaryPositive => ab::positive(&v),
        Intrinsic1::ListToTuple => Ok(tuple::new_tuple(list::list_to_vec(&v))),
        Intrinsic1::TypeVar => crate::types::typevar::make_typevar(&v, None, None, false),
        Intrinsic1::ParamSpec => crate::types::typevar::make_paramspec(&v),
        Intrinsic1::TypeVarTuple => crate::types::typevar::make_typevartuple(&v),
        Intrinsic1::SubscriptGeneric => crate::types::typevar::subscript_generic(&v),
        Intrinsic1::TypeAlias => crate::types::typevar::make_typealias(&v),
        Intrinsic1::AsyncGenWrap => crate::types::asyncgen::wrap_value(v),
    }
}

pub(super) fn call_intrinsic2(f: &mut Frame, which: u32, a: PyRef, b: PyRef) -> PyResult {
    let which: Intrinsic2 = unsafe { std::mem::transmute(which as u8) };
    let _ = f;
    match which {
        Intrinsic2::PrepReraiseStar => {
            let r = crate::types::exceptiongroup::prep_reraise_star(&a, &b)?;
            // a group made here (the wrapper of a naked exception, or a new
            // combination) has not been raised yet: its traceback starts at
            // this frame's except* statement
            if exc::is_exception_instance(&r) && exc::exc_traceback(&r).is_none() {
                add_traceback(f, &r);
            }
            Ok(r)
        }
        Intrinsic2::TypeVarWithBound => crate::types::typevar::make_typevar(&a, Some(&b), None, false),
        Intrinsic2::TypeVarWithConstraints => crate::types::typevar::make_typevar(&a, None, Some(&b), false),
        Intrinsic2::SetFunctionTypeParams => {
            let fo = funcobject::func(&a);
            let old = fo.func_typeparams;
            fo.func_typeparams = b.clone().into_raw();
            unsafe { xdecref(old) };
            Ok(a)
        }
        Intrinsic2::SetTypeparamDefault => crate::types::typevar::set_default(&a, &b),
    }
}

pub(crate) fn get_awaitable(o: &PyObject, where_: u32) -> PyResult {
    match crate::types::genobject::get_awaitable_iter(o) {
        Ok(r) => {
            if crate::types::genobject::is_coroutine_exact(&r) && crate::types::genobject::coro_is_running_await(&r) {
                raise!(RuntimeError, "coroutine is being awaited already");
            }
            Ok(r)
        }
        Err(e) => {
            if exc_matches(exc::TypeError()) {
                let msg = match where_ {
                    1 => Some(format!(
                        "'async with' received an object from __aenter__ that does not implement __await__: {}",
                        tname(o)
                    )),
                    2 => Some(format!(
                        "'async with' received an object from __aexit__ that does not implement __await__: {}",
                        tname(o)
                    )),
                    _ => None,
                };
                if let Some(m) = msg {
                    clear_exc();
                    return Err(err!(TypeError, "{}", m));
                }
            }
            Err(e)
        }
    }
}

/// LOAD_SPECIAL: look up a context-manager method on type(o). Returns (callable,
/// self_or_null) like LoadMethod.
pub(super) fn load_special(o: &PyObject, which: u32) -> PyResult<(PyRef, Option<PyRef>)> {
    let names = [name!("__enter__"), name!("__exit__"), name!("__aenter__"), name!("__aexit__")];
    let nm = names[(which & 3) as usize];
    match lookup(o.type_ptr(), nm) {
        Some(m) => {
            if m.ty().tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
                return Ok((m, Some(o.to_ref())));
            }
            let bound = match m.ty().tp_descr_get {
                Some(g) => unsafe { from_c(g(m.as_ptr(), o.as_ptr(), o.type_ptr() as *mut PyObject))? },
                None => m,
            };
            Ok((bound, None))
        }
        None => {
            let sync = which < 2;
            let kind = if sync { "" } else { "asynchronous " };
            let (oe, ox, hint) = if sync {
                (name!("__aenter__"), name!("__aexit__"), " but it supports the asynchronous context manager protocol. Did you mean to use 'async with'?")
            } else {
                (name!("__enter__"), name!("__exit__"), " but it supports the context manager protocol. Did you mean to use 'with'?")
            };
            let other = lookup(o.type_ptr(), oe).is_some() && lookup(o.type_ptr(), ox).is_some();
            raise!(
                TypeError,
                "'{}' object does not support the {}context manager protocol (missed {} method){}",
                tname(o),
                kind,
                pystr::to_string_lossy(nm),
                if other { hint } else { "" }
            );
        }
    }
}

/// Run (or resume) a frame. `throw`: an exception to raise at the resume point.
pub fn eval_frame(ts: &ThreadState, f: &mut Frame, throw: Option<PyRef>) -> PyResult<Exit> {
    let prev = ts.frame.get();
    f.prev = prev;
    ts.frame.set(f as *mut Frame);
    f.state = FrameState::Executing;
    // jit workstream: compiled code runs the activation directly when it can
    let r = match super::jit::resume(ts, f, throw) {
        Ok(r) => r,
        Err(throw) => eval_inner(ts, f, throw),
    };
    ts.frame.set(prev);
    f.prev = std::ptr::null_mut();
    match &r {
        Ok(Exit::Yield(_)) => f.state = FrameState::Suspended,
        _ => f.state = FrameState::Completed,
    }
    r
}

pub(crate) fn eval_inner(ts: &ThreadState, f: &mut Frame, throw: Option<PyRef>) -> PyResult<Exit> {
    let code_obj: PyRef = f.code().to_ref();
    let cd = crate::types::code::code_data(&code_obj);
    let code: &[u32] = &cd.code;
    let caches = cd.caches.tables(code); // jit workstream: inline caches / profiles
    // bc-vm: specialisation view of public code (interp/adaptive.rs); `adapt_once` for
    // classifications too costly to repeat
    macro_rules! adapt {
        ($e:expr) => {
            if super::adaptive::active(cd) {
                super::adaptive::note(cd, f.lasti as usize, $e);
            }
        };
    }
    macro_rules! adapt_once {
        ($e:expr) => {
            if super::adaptive::active(cd) && !super::adaptive::seen(cd, f.lasti as usize) {
                super::adaptive::note(cd, f.lasti as usize, $e);
            }
        };
    }
    let mut pc: usize = f.next_instr as usize;
    f.yield_from_send = -1;
    let tracing = crate::modules::sys::tracing_active();
    if tracing && f.state != FrameState::Suspended {
        crate::modules::sys::trace_call(ts, f)?;
    }

    // pending exception to raise immediately (generator.throw)
    let mut pending_err: bool = false;
    if let Some(t) = throw {
        f.lasti = if pc > 0 { (pc - 1) as u32 } else { 0 };
        if crate::modules::sys::tracing_active() {
            crate::modules::sys_trace::fire_exc_event(ts, f, crate::modules::sys_trace::PY_THROW, &t)?;
        }
        set_exc(t);
        pending_err = true;
    }

    'outer: loop {
        if !pending_err {
            // ---------------------------------------------------------- dispatch loop
            let result: PyResult<Option<Exit>> = (|| -> PyResult<Option<Exit>> {
                // jit workstream: run compiled code (tier 1) from here if available
                if !tracing {
                    match super::jit::try_run(ts, f, cd, pc) {
                        super::jit::Run::NotRun => {}
                        super::jit::Run::Return(v) => return Ok(Some(Exit::Return(v))),
                        super::jit::Run::Error => return Err(PyErr),
                        super::jit::Run::Deopt(npc) => pc = npc,
                        super::jit::Run::Yield(v) => return Ok(Some(Exit::Yield(v))),
                    }
                }
                let mut ext: u32 = 0;
                loop {
                    let w = unsafe { *code.get_unchecked(pc) };
                    let op = Op::from_u8((w & 0xff) as u8);
                    let arg = (w >> 8) | ext;
                    f.lasti = pc as u32;
                    pc += 1;
                    if op == Op::ExtendedArg {
                        ext = arg << 24;
                        continue;
                    }
                    ext = 0;
                    if crate::modules::sys::tracing_active() {
                        crate::modules::sys::trace_instr(ts, f)?;
                        if f.jump_to >= 0 {
                            // the trace function assigned f_lineno
                            pc = f.jump_to as usize;
                            f.jump_to = -1;
                            continue;
                        }
                    }
                    match op {
                        Op::Nop | Op::Resume => {
                            if op == Op::Resume { adapt!(rpy_compiler::wordcode::opcodes::RESUME_CHECK); }
                            let eb = ts.c.eval_breaker.load(std::sync::atomic::Ordering::Relaxed);
                            if eb != 0 {
                                handle_eval_breaker(ts)?;
                            }
                        }
                        Op::PopTop => unsafe { xdecref(pop!(f)) },
                        Op::PushNull => push!(f, std::ptr::null_mut()),
                        Op::Copy => {
                            let v = peek!(f, arg as usize);
                            unsafe { xincref(v) };
                            push!(f, v);
                        }
                        Op::Swap => {
                            let n = arg as usize;
                            let t = f.stack_top;
                            f.slots.swap(t - 1, t - n);
                        }
                        Op::LoadConst => {
                            let v = cd.consts[arg as usize].clone();
                            push!(f, v.into_raw());
                        }
                        Op::LoadFast => {
                            let v = f.slots[arg as usize];
                            if v.is_null() {
                                return Err(unbound_local(f, arg as usize));
                            }
                            unsafe { incref(v) };
                            push!(f, v);
                        }
                        Op::LoadFastCheck => {
                            let v = f.slots[arg as usize];
                            if v.is_null() {
                                return Err(unbound_local(f, arg as usize));
                            }
                            unsafe { incref(v) };
                            push!(f, v);
                        }
                        Op::LoadFastAndClear => {
                            let v = std::mem::replace(&mut f.slots[arg as usize], std::ptr::null_mut());
                            push!(f, v);
                        }
                        Op::StoreFast => {
                            let v = pop!(f);
                            let old = std::mem::replace(&mut f.slots[arg as usize], v);
                            unsafe { xdecref(old) };
                        }
                        Op::DeleteFast => {
                            let old = std::mem::replace(&mut f.slots[arg as usize], std::ptr::null_mut());
                            if old.is_null() {
                                return Err(unbound_local(f, arg as usize));
                            }
                            unsafe { decref(old) };
                        }
                        Op::MakeCell => {
                            let i = arg as usize;
                            let v = std::mem::replace(&mut f.slots[i], std::ptr::null_mut());
                            let cell = funcobject::new_cell(unsafe { PyRef::from_raw_opt(v) });
                            f.slots[i] = cell.into_raw();
                        }
                        Op::CopyFreeVars => {
                            let n = arg as usize;
                            let func = f.func;
                            if func.is_null() {
                                // exec(code, closure=...) placed the cells already
                                if (f.nlocalsplus - n..f.nlocalsplus).all(|i| !f.slots[i].is_null()) {
                                    continue;
                                }
                                return Err(system_error("CopyFreeVars without function"));
                            }
                            let clos = funcobject::func(unsafe { &*func }).func_closure;
                            if clos.is_null() {
                                return Err(system_error("missing closure"));
                            }
                            let items = tuple::tuple_items(unsafe { &*clos });
                            let base = f.nlocalsplus - n;
                            for i in 0..n {
                                let c = items[i].to_ref().into_raw();
                                let old = std::mem::replace(&mut f.slots[base + i], c);
                                unsafe { xdecref(old) };
                            }
                        }
                        Op::LoadDeref => {
                            let cell = f.slots[arg as usize];
                            let v = if cell.is_null() { None } else { funcobject::cell_get(unsafe { &*cell }) };
                            match v {
                                Some(v) => push!(f, v.into_raw()),
                                None => return Err(unbound_local(f, arg as usize)),
                            }
                        }
                        Op::StoreDeref => {
                            let v = popref!(f);
                            let cell = f.slots[arg as usize];
                            funcobject::cell_set(unsafe { &*cell }, Some(v));
                        }
                        // shared with tier-1 code (jit::helpers::h_misc)
                        Op::DeleteDeref | Op::LoadFromDictOrDeref | Op::DeleteGlobal | Op::LoadFromDictOrGlobals | Op::GetAiter | Op::GetAnext | Op::EndAsyncFor | Op::GetAwaitable | Op::CleanupThrow | Op::LoadBuildClass | Op::PushExcInfo | Op::PopExcept | Op::CheckExcMatch | Op::CheckEgMatch | Op::WithExceptStart | Op::BeforeWith | Op::BeforeAsyncWith | Op::ImportName | Op::ImportFrom | Op::BuildTemplate | Op::BuildInterpolation | Op::MatchClass | Op::MatchMapping | Op::MatchSequence | Op::MatchKeys | Op::GetLen | Op::LoadCommonConstant | Op::LoadSpecial => exec_misc(ts, f, cd, op, arg)?,
                        Op::LoadLocals => {
                            if f.locals.is_null() {
                                raise!(SystemError, "no locals found");
                            }
                            let l = unsafe { (*f.locals).to_ref() };
                            push!(f, l.into_raw());
                        }
                        Op::LoadName => {
                            let nm = &cd.names[arg as usize];
                            let mut v = None;
                            if !f.locals.is_null() {
                                v = mapping_get(unsafe { &*f.locals }, nm)?;
                                if let Some(x) = v {
                                    v = Some(lazy_check(unsafe { &*f.locals }, nm, x)?);
                                }
                            }
                            let v = match v {
                                Some(v) => v,
                                None => load_global(f, nm)?,
                            };
                            push!(f, v.into_raw());
                        }
                        Op::StoreName => {
                            let v = popref!(f);
                            let nm = &cd.names[arg as usize];
                            if f.locals.is_null() {
                                raise!(SystemError, "no locals found when storing {}", pystr::to_string_lossy(nm));
                            }
                            let l = unsafe { &*f.locals };
                            if dict::is_dict_exact(l) {
                                dict::set_item(l, nm, &v)?;
                            } else {
                                ab::setitem(l, nm, Some(&v))?;
                            }
                        }
                        Op::DeleteName => {
                            let nm = &cd.names[arg as usize];
                            if f.locals.is_null() {
                                raise!(SystemError, "no locals when deleting {}", pystr::to_string_lossy(nm));
                            }
                            let l = unsafe { &*f.locals };
                            let ok = if dict::is_dict_exact(l) {
                                dict::del_item(l, nm)?
                            } else {
                                match ab::setitem(l, nm, None) {
                                    Ok(()) => true,
                                    Err(e) => {
                                        if exc_matches(exc::KeyError()) {
                                            clear_exc();
                                            false
                                        } else {
                                            return Err(e);
                                        }
                                    }
                                }
                            };
                            if !ok {
                                return Err(name_error(f, nm));
                            }
                        }
                        Op::LoadGlobal => {
                            let nm = &cd.names[arg as usize];
                            adapt_once!(super::adaptive::load_global(f.globals(), f.builtins(), nm));
                            let v = super::specialize::load_global(f, nm, caches.entry(f.lasti as usize))?;
                            push!(f, v.into_raw());
                        }
                        Op::StoreGlobal => {
                            let v = popref!(f);
                            dict::set_item(f.globals(), &cd.names[arg as usize], &v)?;
                        }
                        Op::LoadAttr => {
                            let o = popref!(f);
                            adapt!(super::adaptive::load_attr(&o, &cd.names[arg as usize], false));
                            let v = super::specialize::load_attr(&o, &cd.names[arg as usize], caches.poly(f.lasti as usize))?;
                            push!(f, v.into_raw());
                        }
                        Op::LoadMethod => {
                            let o = popref!(f);
                            adapt!(super::adaptive::load_attr(&o, &cd.names[arg as usize], true));
                            let (m, s) = super::specialize::load_method(&o, &cd.names[arg as usize], caches.poly(f.lasti as usize))?;
                            push!(f, m.into_raw());
                            push!(f, s.map_or(std::ptr::null_mut(), |s| s.into_raw()));
                        }
                        Op::StoreAttr => {
                            let o = popref!(f);
                            adapt!(super::adaptive::store_attr(&o, &cd.names[arg as usize]));
                            let v = popref!(f);
                            super::specialize::store_attr(&o, &cd.names[arg as usize], &v, caches.poly(f.lasti as usize))?;
                        }
                        Op::DeleteAttr => {
                            let o = popref!(f);
                            ab::delattr(&o, &cd.names[arg as usize])?;
                        }
                        Op::LoadSuperAttr => {
                            adapt!(super::adaptive::load_super_attr(unsafe { &*peek!(f, 3) }, arg & 1 != 0));
                            let slf = popref!(f);
                            let cls = popref!(f);
                            let gsuper = popref!(f);
                            let nm = &cd.names[(arg >> 2) as usize];
                            let is_method = arg & 1 != 0;
                            let sup = if gsuper.is(type_obj(crate::types::super_::super_type())) {
                                crate::types::super_::new_super(&cls, &slf)?
                            } else {
                                let two = arg & 2 != 0;
                                if two { ab::call(&gsuper, &[&cls, &slf])? } else { ab::call(&gsuper, &[])? }
                            };
                            if is_method {
                                let v = ab::getattr(&sup, nm)?;
                                push!(f, v.into_raw());
                                push!(f, std::ptr::null_mut());
                            } else {
                                let v = ab::getattr(&sup, nm)?;
                                push!(f, v.into_raw());
                            }
                        }
                        Op::BinaryOp => {
                            adapt!(super::adaptive::binary(arg, unsafe { &*peek!(f, 2) }, unsafe { &*peek!(f, 1) }));
                            let b = popref!(f);
                            let a = popref!(f);
                            let r = binary_op_on_stack(BINOPS[arg as usize], &a, &b)?;
                            push!(f, r.into_raw());
                        }
                        Op::UnaryNegative => {
                            let a = popref!(f);
                            let r = if int::is_int_exact(&a) && int::is_compact(&a) {
                                int::new_int(-int::compact_value(&a))
                            } else {
                                ab::negative(&a)?
                            };
                            push!(f, r.into_raw());
                        }
                        Op::UnaryPositive => {
                            let a = popref!(f);
                            push!(f, ab::positive(&a)?.into_raw());
                        }
                        Op::UnaryInvert => {
                            let a = popref!(f);
                            push!(f, ab::invert(&a)?.into_raw());
                        }
                        Op::UnaryNot => {
                            let a = popref!(f);
                            let t = ab::is_true(&a)?;
                            push!(f, bool_::new_bool(!t).into_raw());
                        }
                        Op::ToBool => {
                            adapt!(super::adaptive::to_bool(unsafe { &*peek!(f, 1) }));
                            let a = popref!(f);
                            let t = ab::is_true(&a)?;
                            push!(f, bool_::new_bool(t).into_raw());
                        }
                        Op::CompareOp => {
                            adapt!(super::adaptive::compare(arg & 15, unsafe { &*peek!(f, 2) }, unsafe { &*peek!(f, 1) }));
                            let b = popref!(f);
                            let a = popref!(f);
                            let cop = (arg & 15) as std::ffi::c_int;
                            let r = if let Some(t) = super::specialize::compare_fast(cop, &a, &b) {
                                bool_::new_bool(t)
                            } else {
                                let r = ab::rich_compare(&a, &b, cop)?;
                                if arg & 16 != 0 && !bool_::is_bool(&r) { bool_::new_bool(ab::is_true(&r)?) } else { r }
                            };
                            push!(f, r.into_raw());
                        }
                        Op::IsOp => {
                            let b = popref!(f);
                            let a = popref!(f);
                            push!(f, bool_::new_bool(a.is(&b) != (arg == 1)).into_raw());
                        }
                        Op::ContainsOp => {
                            adapt!(super::adaptive::contains(unsafe { &*peek!(f, 1) }));
                            let b = popref!(f);
                            let a = popref!(f);
                            let r = ab::contains(&b, &a)?;
                            push!(f, bool_::new_bool(r != (arg == 1)).into_raw());
                        }
                        Op::BinarySubscr => {
                            adapt!(super::adaptive::binary(26, unsafe { &*peek!(f, 2) }, unsafe { &*peek!(f, 1) }));
                            let k = popref!(f);
                            let c = popref!(f);
                            let r = if list::is_list_exact(&c) && int::is_int_exact(&k) && int::is_compact(&k) {
                                match list::get_signed(&c, int::compact_value(&k)) {
                                    Some(v) => v,
                                    None => raise!(IndexError, "list index out of range"),
                                }
                            } else if tuple::is_tuple_exact(&c) && int::is_int_exact(&k) && int::is_compact(&k) {
                                let mut i = int::compact_value(&k);
                                let n = tuple::tuple_len(&c) as i64;
                                if i < 0 {
                                    i += n;
                                }
                                if i < 0 || i >= n {
                                    raise!(IndexError, "tuple index out of range");
                                }
                                tuple::tuple_get(&c, i as usize).to_ref()
                            } else if dict::is_dict_exact(&c) {
                                match dict::get_item(&c, &k)? {
                                    Some(v) => v,
                                    None => {
                                        crate::types::dict_methods::raise_key_error(&k);
                                        return Err(PyErr);
                                    }
                                }
                            } else {
                                ab::getitem(&c, &k)?
                            };
                            push!(f, r.into_raw());
                        }
                        Op::StoreSubscr => {
                            adapt!(super::adaptive::store_subscr(unsafe { &*peek!(f, 2) }, unsafe { &*peek!(f, 1) }));
                            let k = popref!(f);
                            let c = popref!(f);
                            let v = popref!(f);
                            if dict::is_dict_exact(&c) {
                                dict::set_item(&c, &k, &v)?;
                            } else {
                                ab::setitem(&c, &k, Some(&v))?;
                            }
                        }
                        Op::DeleteSubscr => {
                            let k = popref!(f);
                            let c = popref!(f);
                            ab::setitem(&c, &k, None)?;
                        }
                        Op::BinarySlice => {
                            let stop = popref!(f);
                            let start = popref!(f);
                            let c = popref!(f);
                            let s = crate::types::slice::new_slice(&start, &stop, none::none());
                            push!(f, ab::getitem(&c, &s)?.into_raw());
                        }
                        Op::StoreSlice => {
                            let stop = popref!(f);
                            let start = popref!(f);
                            let c = popref!(f);
                            let v = popref!(f);
                            let s = crate::types::slice::new_slice(&start, &stop, none::none());
                            ab::setitem(&c, &s, Some(&v))?;
                        }
                        Op::BuildSlice => {
                            let step = if arg == 3 { Some(popref!(f)) } else { None };
                            let stop = popref!(f);
                            let start = popref!(f);
                            let s = crate::types::slice::new_slice(&start, &stop, step.as_deref().unwrap_or(none::none()));
                            push!(f, s.into_raw());
                        }
                        Op::BuildTuple => {
                            let n = arg as usize;
                            let base = f.stack_top - n;
                            let items: Vec<PyRef> = (0..n)
                                .map(|i| unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + i], std::ptr::null_mut())) })
                                .collect();
                            f.stack_top = base;
                            push!(f, tuple::new_tuple(items).into_raw());
                        }
                        Op::BuildList => {
                            let n = arg as usize;
                            let base = f.stack_top - n;
                            let items: Vec<PyRef> = (0..n)
                                .map(|i| unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + i], std::ptr::null_mut())) })
                                .collect();
                            f.stack_top = base;
                            push!(f, list::new_list(items).into_raw());
                        }
                        Op::BuildSet => {
                            let n = arg as usize;
                            let base = f.stack_top - n;
                            let items: Vec<PyRef> = (0..n)
                                .map(|i| unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + i], std::ptr::null_mut())) })
                                .collect();
                            f.stack_top = base;
                            let s = crate::types::set::new_set_from_vec(items)?;
                            push!(f, s.into_raw());
                        }
                        Op::BuildMap => {
                            let n = arg as usize;
                            let base = f.stack_top - 2 * n;
                            let d = dict::new_dict_presized(n);
                            let mut res = Ok(());
                            for i in 0..n {
                                let k = unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + 2 * i], std::ptr::null_mut())) };
                                let v = unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + 2 * i + 1], std::ptr::null_mut())) };
                                if res.is_ok() {
                                    res = dict::set_item(&d, &k, &v);
                                }
                            }
                            f.stack_top = base;
                            res?;
                            push!(f, d.into_raw());
                        }
                        Op::BuildString => {
                            let n = arg as usize;
                            let base = f.stack_top - n;
                            let mut sb = pystr::StrBuilder::new();
                            for i in 0..n {
                                let s = unsafe { PyRef::from_raw(std::mem::replace(&mut f.slots[base + i], std::ptr::null_mut())) };
                                sb.push_obj(&s);
                            }
                            f.stack_top = base;
                            push!(f, sb.finish().into_raw());
                        }
                        Op::ListAppend => {
                            let v = popref!(f);
                            let l = peek!(f, arg as usize);
                            list::append(unsafe { &*l }, v);
                        }
                        Op::SetAdd => {
                            let v = popref!(f);
                            let s = peek!(f, arg as usize);
                            if !crate::types::set::is_set(unsafe { &*s }) {
                                raise!(TypeError, "'{}' object is not a set", tname(unsafe { &*s }));
                            }
                            crate::types::set::set_add(unsafe { &*s }, &v)?;
                        }
                        Op::MapAdd => {
                            let v = popref!(f);
                            let k = popref!(f);
                            let d = peek!(f, arg as usize);
                            dict::set_item(unsafe { &*d }, &k, &v)?;
                        }
                        Op::ListExtend => {
                            let it = popref!(f);
                            let l = peek!(f, arg as usize);
                            match ab::collect_iterable(&it) {
                                Ok(v) => {
                                    for x in v {
                                        list::append(unsafe { &*l }, x);
                                    }
                                }
                                Err(e) => {
                                    if exc_matches(exc::TypeError()) && it.ty().tp_iter.is_none() && !is_sequence_like(&it) {
                                        clear_exc();
                                        raise!(TypeError, "Value after * must be an iterable, not {}", tname(&it));
                                    }
                                    return Err(e);
                                }
                            }
                        }
                        Op::SetUpdate => {
                            let it = popref!(f);
                            let s = peek!(f, arg as usize);
                            crate::types::set::set_update(unsafe { &*s }, &it)?;
                        }
                        Op::DictUpdate => {
                            let o = popref!(f);
                            let d = peek!(f, arg as usize);
                            crate::types::dict_methods::dict_update_display(unsafe { &*d }, &o)?;
                        }
                        Op::DictMerge => {
                            let o = popref!(f);
                            let d = peek!(f, arg as usize);
                            // stack: callable, self-or-NULL, positional args, ..., the dict
                            let func = peek!(f, arg as usize + 3);
                            crate::types::dict_methods::dict_merge_kwargs(unsafe { &*d }, &o, func)?;
                        }
                        Op::UnpackSequence => {
                            adapt!(super::adaptive::unpack(unsafe { &*peek!(f, 1) }, arg));
                            let seq = popref!(f);
                            let n = arg as usize;
                            let mut items = Vec::with_capacity(n);
                            unpack_sequence(&seq, n, &mut items)?;
                            for v in items.into_iter().rev() {
                                push!(f, v.into_raw());
                            }
                        }
                        Op::UnpackEx => {
                            let seq = popref!(f);
                            let before = (arg & 0xff) as usize;
                            let after = (arg >> 8) as usize;
                            let items = unpack_ex(&seq, before, after)?;
                            for v in items.into_iter().rev() {
                                push!(f, v.into_raw());
                            }
                        }
                        Op::Jump => {
                            if (arg as usize) < pc { adapt!(rpy_compiler::wordcode::opcodes::JUMP_BACKWARD_NO_JIT); }
                            if crate::modules::sys::tracing_active() {
                                crate::modules::sys_trace::fire_branch(ts, f, crate::modules::sys_trace::JUMP, arg as usize)?;
                            }
                            if (arg as usize) < pc {
                                // backedge: safepoint poll + tiering counter (weighted by the loop
                                // length: long loop bodies get compiled after fewer iterations)
                                cd.backedge_count.fetch_add(1 + ((pc - arg as usize) / 32) as u32, std::sync::atomic::Ordering::Relaxed);
                                let eb = ts.c.eval_breaker.load(std::sync::atomic::Ordering::Relaxed);
                                if eb != 0 {
                                    handle_eval_breaker(ts)?;
                                }
                                // jit workstream: on-stack replacement into compiled code
                                if !tracing && super::jit::osr_check(f, cd, arg as usize) {
                                    pc = arg as usize;
                                    return Ok(None);
                                }
                            }
                            pc = arg as usize;
                        }
                        Op::PopJumpIfFalse => {
                            let v = popref!(f);
                            let t = if v.is(bool_::false_obj()) { false } else if v.is(bool_::true_obj()) { true } else { ab::is_true(&v)? };
                            if !t {
                                pc = arg as usize;
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, !t, pc)?;
                            }
                        }
                        Op::PopJumpIfTrue => {
                            let v = popref!(f);
                            let t = if v.is(bool_::true_obj()) { true } else if v.is(bool_::false_obj()) { false } else { ab::is_true(&v)? };
                            if t {
                                pc = arg as usize;
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, t, pc)?;
                            }
                        }
                        Op::PopJumpIfNone => {
                            let v = popref!(f);
                            if v.is(none::none()) {
                                pc = arg as usize;
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, v.is(none::none()), pc)?;
                            }
                        }
                        Op::PopJumpIfNotNone => {
                            let v = popref!(f);
                            if !v.is(none::none()) {
                                pc = arg as usize;
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, !v.is(none::none()), pc)?;
                            }
                        }
                        Op::JumpIfFalseOrPop => {
                            let t = ab::is_true(unsafe { &*top!(f) })?;
                            if !t {
                                pc = arg as usize;
                            } else {
                                unsafe { decref(pop!(f)) };
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, !t, pc)?;
                            }
                        }
                        Op::JumpIfTrueOrPop => {
                            let t = ab::is_true(unsafe { &*top!(f) })?;
                            if t {
                                pc = arg as usize;
                            } else {
                                unsafe { decref(pop!(f)) };
                            }
                            if crate::modules::sys::tracing_active() {
                                branch_event(ts, f, t, pc)?;
                            }
                        }
                        Op::GetIter => {
                            adapt!(super::adaptive::get_iter(unsafe { &*peek!(f, 1) }));
                            let o = popref!(f);
                            push!(f, ab::get_iter(&o)?.into_raw());
                        }
                        Op::ForIter => {
                            adapt_once!(super::adaptive::for_iter(unsafe { &*peek!(f, 1) }));
                            let it = unsafe { &*top!(f) };
                            match ab::iter_next(it)? {
                                Some(v) => push!(f, v.into_raw()),
                                None => {
                                    unsafe { decref(pop!(f)) };
                                    pc = arg as usize;
                                }
                            }
                            if crate::modules::sys::tracing_active() {
                                for_iter_event(ts, f, pc == arg as usize, pc)?;
                            }
                        }
                        Op::GetYieldFromIter => {
                            let o = popref!(f);
                            let r = if crate::types::genobject::is_coroutine_exact(&o) {
                                let flags = crate::types::code::code(f.code()).flags();
                                if flags & (bc::co::COROUTINE | bc::co::ITERABLE_COROUTINE) == 0 {
                                    raise!(TypeError, "cannot 'yield from' a coroutine object in a non-coroutine generator");
                                }
                                o
                            } else if crate::types::genobject::is_generator_exact(&o) {
                                o
                            } else {
                                ab::get_iter(&o)?
                            };
                            push!(f, r.into_raw());
                        }
                        Op::Send => {
                            adapt!(super::adaptive::send(unsafe { &*peek!(f, 2) }));
                            let v = popref!(f);
                            let recv = unsafe { &*top!(f) };
                            match crate::types::genobject::send_to(recv, &v)? {
                                crate::types::genobject::SendResult::Yield(r) => push!(f, r.into_raw()),
                                crate::types::genobject::SendResult::Return(r) => {
                                    unsafe { decref(pop!(f)) };
                                    push!(f, r.into_raw());
                                    pc = arg as usize;
                                }
                            }
                        }
                        Op::YieldValue => {
                            let v = popref!(f);
                            f.next_instr = pc as u32;
                            f.yield_from_send = -1;
                            if pc >= 2 {
                                let (pop, _) = bc::decode(code[pc - 2]);
                                if pop == Op::Send {
                                    f.yield_from_send = (pc - 2) as i32;
                                }
                            }
                            return Ok(Some(Exit::Yield(v)));
                        }
                        Op::ReturnValue => {
                            let v = popref!(f);
                            return Ok(Some(Exit::Return(v)));
                        }
                        Op::Call | Op::CallKw => {
                            if super::adaptive::active(cd) {
                                let nk = if op == Op::CallKw { 1 } else { 0 };
                                let n = arg as usize;
                                let base = f.stack_top - n - 2 - nk;
                                let c = unsafe { &*f.slots[base] };
                                let s = f.slots[base + 1];
                                let so = if s.is_null() { None } else { Some(unsafe { &*s }) };
                                if op == Op::CallKw { adapt!(super::adaptive::call_kw(c, so)); } else { adapt!(super::adaptive::call(c, so, n)); }
                            }
                            let (n, kwnames) = if op == Op::CallKw {
                                (arg as usize, Some(popref!(f)))
                            } else {
                                (arg as usize, None)
                            };
                            let base = f.stack_top - n - 2;
                            let callable = f.slots[base];
                            let self_or_null = f.slots[base + 1];
                            let call_info = if crate::modules::sys::tracing_active() {
                                let first = if n > 0 { Some(unsafe { &*f.slots[base + 2] }) } else { None };
                                let so = if self_or_null.is_null() { None } else { Some(unsafe { &*self_or_null }) };
                                crate::modules::sys_trace::before_call(ts, f, unsafe { &*callable }, so, first, true)?
                            } else {
                                None
                            };
                            let r = unsafe {
                                if self_or_null.is_null() {
                                    let args = f.slots.as_ptr().add(base + 2) as *const *mut PyObject;
                                    let nkw = kwnames.as_ref().map_or(0, |k| tuple::tuple_len(k));
                                    ab::vectorcall_raw(
                                        &*callable,
                                        args,
                                        (n - nkw) | PY_VECTORCALL_ARGUMENTS_OFFSET,
                                        kwnames.as_ref().map_or(std::ptr::null_mut(), |k| k.as_ptr()),
                                    )
                                } else {
                                    let args = f.slots.as_ptr().add(base + 1) as *const *mut PyObject;
                                    let nkw = kwnames.as_ref().map_or(0, |k| tuple::tuple_len(k));
                                    ab::vectorcall_raw(
                                        &*callable,
                                        args,
                                        n + 1 - nkw,
                                        kwnames.as_ref().map_or(std::ptr::null_mut(), |k| k.as_ptr()),
                                    )
                                }
                            };
                            // pop args, self, callable
                            for i in (base..f.stack_top).rev() {
                                let p = std::mem::replace(&mut f.slots[i], std::ptr::null_mut());
                                unsafe { xdecref(p) };
                            }
                            f.stack_top = base;
                            if let Some(info) = call_info {
                                if let Err(e) = crate::modules::sys_trace::after_call(ts, f, info, !r.is_null()) {
                                    unsafe { xdecref(r) };
                                    return Err(e);
                                }
                            }
                            if r.is_null() {
                                return Err(PyErr);
                            }
                            push!(f, r);
                        }
                        Op::CallFunctionEx => {
                            adapt!(super::adaptive::call_ex(unsafe { &*peek!(f, 4) }));
                            let kw = popopt!(f);
                            let args = popref!(f);
                            let self_or_null = popopt!(f);
                            let callable = popref!(f);
                            let args = if tuple::is_tuple_exact(&args) {
                                args
                            } else {
                                match ab::sequence_tuple(&args) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        if exc_matches(exc::TypeError()) && args.ty().tp_iter.is_none() && !is_sequence_like(&args) {
                                            clear_exc();
                                            raise!(
                                                TypeError,
                                                "Value after * must be an iterable, not {}",
                                                tname(&args)
                                            );
                                        }
                                        return Err(e);
                                    }
                                }
                            };
                            let mut v: Vec<&PyObject> = Vec::with_capacity(tuple::tuple_len(&args) + 1);
                            if let Some(s) = &self_or_null {
                                v.push(s);
                            }
                            v.extend(tuple::tuple_items(&args).iter().copied());
                            let kwd = match &kw {
                                Some(k) if !dict::is_dict(k) => {
                                    let d = dict::new_dict();
                                    crate::types::dict_methods::dict_merge_kwargs(&d, k, callable.as_ptr())?;
                                    Some(d)
                                }
                                Some(k) => Some(k.clone()),
                                None => None,
                            };
                            if let Some(d) = &kwd {
                                check_kwargs_keys(d)?;
                            }
                            let call_info = if crate::modules::sys::tracing_active() {
                                crate::modules::sys_trace::before_call(ts, f, &callable, self_or_null.as_deref(), v.first().copied(), false)?
                            } else {
                                None
                            };
                            let r = ab::call_with_kwargs_dict(&callable, &v, kwd.as_deref());
                            if let Some(info) = call_info {
                                crate::modules::sys_trace::after_call(ts, f, info, r.is_ok())?;
                            }
                            let r = r?;
                            push!(f, r.into_raw());
                        }
                        Op::CallIntrinsic1 => {
                            let v = popref!(f);
                            let r = call_intrinsic1(ts, f, arg, v)?;
                            push!(f, r.into_raw());
                        }
                        Op::CallIntrinsic2 => {
                            let b = popref!(f);
                            let a = popref!(f);
                            let r = call_intrinsic2(f, arg, a, b)?;
                            push!(f, r.into_raw());
                        }
                        Op::MakeFunction => {
                            let code = popref!(f);
                            let func = funcobject::new_function(&code, f.globals(), None)?;
                            push!(f, func.into_raw());
                        }
                        Op::SetFunctionAttribute => {
                            let func = popref!(f);
                            let attr = popref!(f);
                            let fo = funcobject::func(&func);
                            let slot = match arg {
                                bc::make_fn::DEFAULTS => &mut fo.func_defaults,
                                bc::make_fn::KWDEFAULTS => &mut fo.func_kwdefaults,
                                bc::make_fn::ANNOTATIONS => &mut fo.func_annotations,
                                bc::make_fn::CLOSURE => &mut fo.func_closure,
                                bc::make_fn::ANNOTATE => &mut fo.func_annotate,
                                _ => raise!(SystemError, "bad SetFunctionAttribute flag"),
                            };
                            let old = std::mem::replace(slot, attr.into_raw());
                            unsafe { xdecref(old) };
                            push!(f, func.into_raw());
                        }
                        Op::RaiseVarargs => {
                            let (e, cause) = match arg {
                                0 => (None, None),
                                1 => (Some(popref!(f)), None),
                                _ => {
                                    let c = popref!(f);
                                    (Some(popref!(f)), Some(c))
                                }
                            };
                            let r = do_raise(ts, e.as_deref(), cause.as_deref());
                            if arg == 0 && !handled_exception(ts).is_null() {
                                // re-raise: keep the existing traceback, don't add this frame twice
                                let _ = r;
                                return Err(ReraiseMarker::mark(ts));
                            }
                            return Err(r);
                        }
                        Op::Reraise => {
                            f.unwind_pc = (pc - 1) as i32;
                            let e = popref!(f);
                            if arg & 0x100 != 0 {
                                // translated RERAISE n: the n values stay on the stack; the
                                // deepest one (n > 0) is the lasti the handler pushed
                                let n = (arg & 0xff) as usize;
                                if n > 0 && f.stack_top >= f.nlocalsplus + n {
                                    let v = peek!(f, n);
                                    if !v.is_null() && int::is_int_exact(unsafe { &*v }) && int::is_compact(unsafe { &*v }) {
                                        let li = int::compact_value(unsafe { &*v });
                                        if li >= 0 && (li as usize) < code.len() {
                                            f.lasti = li as u32;
                                        }
                                    }
                                }
                            } else if arg != 0 {
                                // CPython restores frame->instr_ptr from lasti, but the handler
                                // lookup uses the RERAISE position: keep f.lasti (used by
                                // find_handler) pointing at this instruction.
                                let _lasti = popref!(f);
                            }
                            set_exc(e);
                            return Err(ReraiseMarker::mark(ts));
                        }
                        Op::ReturnGenerator => {
                            // end of the eager generator prologue (see interp::call_function)
                            f.next_instr = pc as u32;
                            return Ok(Some(Exit::Yield(none::none_ref())));
                        }
                        Op::SetupAnnotations => {
                            if f.locals.is_null() {
                                raise!(SystemError, "no locals found when setting up annotations");
                            }
                            let l = unsafe { &*f.locals };
                            if mapping_get(l, name!("__annotations__"))?.is_none() {
                                let d = dict::new_dict();
                                ab::setitem(l, name!("__annotations__"), Some(&d))?;
                            }
                        }
                        Op::FormatSimple => {
                            let v = popref!(f);
                            push!(f, format_value(&v, None)?.into_raw());
                        }
                        Op::FormatWithSpec => {
                            let spec = popref!(f);
                            let v = popref!(f);
                            push!(f, format_value(&v, Some(&spec))?.into_raw());
                        }
                        Op::ConvertValue => {
                            let v = popref!(f);
                            let r = match arg {
                                1 => ab::str(&v)?,
                                2 => ab::repr(&v)?,
                                3 => ab::ascii(&v)?,
                                _ => raise!(SystemError, "bad conversion"),
                            };
                            push!(f, r.into_raw());
                        }
                        Op::ExtendedArg => unreachable!(),
                        Op::Unused1 | Op::Unused2 | Op::Unused3 => match &cd.invalid {
                            Some(msg) => raise!(SystemError, "{}", msg),
                            None => raise!(SystemError, "unknown opcode {}", op as u8),
                        },
                    }
                }
            })();
            match result {
                Ok(Some(exit)) => {
                    if crate::modules::sys::tracing_active() {
                        crate::modules::sys::trace_exit(ts, f, &exit)?;
                    }
                    if let Exit::Return(_) = exit {
                        f.clear_stack();
                    }
                    return Ok(exit);
                }
                Ok(None) => continue 'outer, // jit: re-dispatch (OSR entry)
                Err(_) => {}
            }
        }
        pending_err = false;
        // ---------------------------------------------------------- exception unwinding
        let mut e = match take_exc() {
            Some(e) => e,
            None => {
                let _ = err!(SystemError, "error return without exception set");
                take_exc().unwrap()
            }
        };
        let reraise = ReraiseMarker::take(ts);
        let propagated = ts.propagated_exc.replace((0, 0)) == exc_identity(&e);
        if !reraise {
            // __context__ is attached where an exception is raised, not again in
            // every frame it propagates through.
            if !propagated {
                chain_context(ts, &e);
            }
            add_traceback(f, &e);
            if crate::modules::sys::tracing_active() {
                if crate::modules::sys::trace_exception(ts, f, &e).is_err() {
                    // the tracer raised: its exception replaces the original one and
                    // unwinding continues (cleanup handlers must still run)
                    e = take_exc().unwrap();
                    add_traceback(f, &e);
                }
            }
        } else if crate::modules::sys::tracing_active() {
            if crate::modules::sys_trace::fire_exc_event(ts, f, crate::modules::sys_trace::RERAISE, &e).is_err() {
                e = take_exc().unwrap();
                add_traceback(f, &e);
            }
        }
        let lookup_pc = if f.unwind_pc >= 0 { f.unwind_pc as u32 } else { f.lasti };
        f.unwind_pc = -1;
        match find_handler(&code_obj, lookup_pc) {
            Some(h) => {
                let depth = f.nlocalsplus + h.depth as usize;
                while f.stack_top > depth {
                    let p = pop!(f);
                    unsafe { xdecref(p) };
                }
                if h.lasti {
                    push!(f, int::new_int(f.lasti as i64).into_raw());
                }
                if crate::modules::sys::tracing_active()
                    && crate::modules::sys_trace::fire_exc_event(ts, f, crate::modules::sys_trace::EXCEPTION_HANDLED, &e).is_err()
                {
                    e = take_exc().unwrap();
                    add_traceback(f, &e);
                }
                push!(f, e.into_raw());
                pc = h.target as usize;
                continue 'outer;
            }
            None => {
                f.clear_stack();
                ts.propagated_exc.set(exc_identity(&e));
                set_exc(e);
                if crate::modules::sys::tracing_active() {
                    let _ = crate::modules::sys::trace_unwind(ts, f);
                }
                return Err(PyErr);
            }
        }
    }
}

/// Identity of an exception in flight: its address and its traceback's.
fn exc_identity(e: &PyObject) -> (usize, usize) {
    let tb = exc::exc_traceback(e).map_or(0, |t| t.as_ptr() as usize);
    (e.as_ptr() as usize, tb)
}

/// Distinguishes a bare re-raise (no new traceback entry) from a fresh raise.
pub struct ReraiseMarker;
impl ReraiseMarker {
    fn mark(ts: &ThreadState) -> PyErr {
        ts.reraise_flag.set(true);
        PyErr
    }
    fn take(ts: &ThreadState) -> bool {
        ts.reraise_flag.replace(false)
    }
}

pub(super) fn is_sequence_like(o: &PyObject) -> bool {
    let sq = o.ty().tp_as_sequence;
    !sq.is_null() && unsafe { (*sq).sq_item.is_some() }
}

/// `f(**mapping)`: keyword names must be strings (duplicates are reported
/// before this, when the mappings are merged).
pub(crate) fn check_kwargs_keys(d: &PyObject) -> PyResult<()> {
    for k in dict::keys_vec(d) {
        if !pystr::is_str(&k) {
            raise!(TypeError, "keywords must be strings");
        }
    }
    Ok(())
}

pub(super) fn func_desc(o: &PyObject) -> String {
    if funcobject::is_function(o) {
        let q = funcobject::func(o).func_qualname;
        return format!("{}()", pystr::to_string_lossy(unsafe { &*q }));
    }
    if crate::types::cfunction::is_cfunction(o) {
        return format!("{}()", crate::types::cfunction::cfunction_name(o));
    }
    format!("{} object", tname(o))
}

pub fn handle_eval_breaker(ts: &ThreadState) -> PyResult<()> {
    let eb = ts.c.eval_breaker.load(std::sync::atomic::Ordering::Acquire);
    if eb & crate::threads::EVAL_MERGE_REFCOUNTS != 0 {
        crate::threads::process_merge_queue(ts);
    }
    if eb & crate::threads::EVAL_SAFEPOINT != 0 {
        crate::gc::safepoint_poll(ts);
    }
    if eb & crate::threads::EVAL_GC != 0 {
        crate::gc::maybe_collect(ts);
    }
    if eb & crate::qsbr::EVAL_QSBR != 0 {
        crate::qsbr::on_eval_breaker(ts); // perf-runtime: deferred reclamation
    }
    if eb & crate::threads::EVAL_SIGNALS != 0 {
        crate::modules::signal::run_pending_handlers(ts)?;
    }
    if eb & crate::threads::EVAL_ASYNC_EXC != 0 {
        crate::modules::thread::raise_async_exc(ts)?;
    }
    if eb & crate::threads::EVAL_PENDING != 0 {
        crate::capi::pystate::run_pending_calls()?;
    }
    Ok(())
}

pub fn match_class(subject: &PyObject, cls: &PyObject, nargs: usize, names: &PyObject) -> PyResult {
    if !is_type(cls) {
        raise!(TypeError, "called match pattern must be a class");
    }
    if !ab::isinstance(subject, cls)? {
        return Ok(none::none_ref());
    }
    let mut out: Vec<PyRef> = Vec::new();
    let mut seen: Vec<PyRef> = Vec::new();
    let match_self = |cls: &PyObject| -> bool {
        let t = as_type(cls);
        [
            bool_::bool_type(),
            crate::types::bytearray::bytearray_type(),
            crate::types::bytes::bytes_type(),
            dict::dict_type(),
            float::float_type(),
            crate::types::set::frozenset_type(),
            int::int_type(),
            list::list_type(),
            crate::types::set::set_type(),
            pystr::str_type(),
            tuple::tuple_type(),
        ]
        .iter()
        .any(|&b| is_subtype(t, b))
    };
    if nargs > 0 {
        let match_args = ab::getattr_opt(cls, name!("__match_args__"))?;
        match match_args {
            Some(ma) => {
                if !tuple::is_tuple(&ma) {
                    raise!(TypeError, "{}.__match_args__ must be a tuple (got {})", type_name(unsafe { &*as_type(cls) }), tname(&ma));
                }
                let ml = tuple::tuple_len(&ma);
                if nargs > ml {
                    raise!(
                        TypeError,
                        "{}() accepts {} positional sub-pattern{} ({} given)",
                        type_name(unsafe { &*as_type(cls) }),
                        ml,
                        if ml == 1 { "" } else { "s" },
                        nargs
                    );
                }
                for i in 0..nargs {
                    let nm = tuple::tuple_get(&ma, i);
                    if !pystr::is_str(nm) {
                        raise!(TypeError, "__match_args__ elements must be strings (got {})", tname(nm));
                    }
                    if seen.iter().any(|s| pystr::str_eq(s, nm)) {
                        raise!(
                            TypeError,
                            "{}() got multiple sub-patterns for attribute '{}'",
                            type_name(unsafe { &*as_type(cls) }),
                            pystr::to_string_lossy(nm)
                        );
                    }
                    seen.push(nm.to_ref());
                    match ab::getattr_opt(subject, nm)? {
                        Some(v) => out.push(v),
                        None => return Ok(none::none_ref()),
                    }
                }
            }
            None => {
                if match_self(cls) {
                    if nargs > 1 {
                        raise!(
                            TypeError,
                            "{}() accepts 1 positional sub-pattern ({} given)",
                            type_name(unsafe { &*as_type(cls) }),
                            nargs
                        );
                    }
                    out.push(subject.to_ref());
                } else {
                    raise!(
                        TypeError,
                        "{}() accepts 0 positional sub-patterns ({} given)",
                        type_name(unsafe { &*as_type(cls) }),
                        nargs
                    );
                }
            }
        }
    }
    for nm in tuple::tuple_items(names) {
        if seen.iter().any(|s| pystr::str_eq(s, nm)) {
            raise!(
                TypeError,
                "{}() got multiple sub-patterns for attribute '{}'",
                type_name(unsafe { &*as_type(cls) }),
                pystr::to_string_lossy(nm)
            );
        }
        seen.push(nm.to_ref());
        match ab::getattr_opt(subject, nm)? {
            Some(v) => out.push(v),
            None => return Ok(none::none_ref()),
        }
    }
    Ok(tuple::new_tuple(out))
}

fn match_keys(subject: &PyObject, keys: &PyObject) -> PyResult {
    let n = tuple::tuple_len(keys);
    if n == 0 {
        return Ok(tuple::empty_tuple());
    }
    let get = ab::getattr(subject, name!("get"))?;
    let dummy = crate::types::object_type::sentinel();
    let mut out = Vec::with_capacity(n);
    let mut seen: Vec<PyRef> = Vec::new();
    for k in tuple::tuple_items(keys) {
        for s in &seen {
            if ab::rich_eq(s, k)? {
                raise!(ValueError, "mapping pattern checks duplicate key ({})", ab::repr_string(k)?);
            }
        }
        seen.push(k.to_ref());
        let v = ab::call(&get, &[k, &dummy])?;
        if v.is(&dummy) {
            return Ok(none::none_ref());
        }
        out.push(v);
    }
    Ok(tuple::new_tuple(out))
}

/// Is a suspended frame inside a yield-from / await send loop?
pub fn is_in_send_loop(f: &Frame) -> bool {
    f.yield_from_send >= 0
}

/// The delegated-to iterator finished during throw(): pop the receiver and
/// arrange to resume at the Send instruction's exit target.
pub fn finish_send_loop(f: &mut Frame) {
    if f.yield_from_send < 0 {
        return;
    }
    let d = crate::types::code::code_data(f.code());
    let (_, target) = bc::decode(d.code[f.yield_from_send as usize]);
    if f.stack_top > f.nlocalsplus {
        f.stack_top -= 1;
        let p = std::mem::replace(&mut f.slots[f.stack_top], std::ptr::null_mut());
        unsafe { xdecref(p) };
    }
    f.next_instr = target;
    f.yield_from_send = -1;
}

/// A name lookup that finds a lazy import placeholder reifies it (PEP 810) and
/// replaces the binding in the mapping it was found in.
#[inline]
pub(super) fn lazy_check(d: &PyObject, nm: &PyObject, v: PyRef) -> PyResult {
    if crate::types::lazyimport::is_lazy(&v) {
        return crate::types::lazyimport::reify_in_dict(d, nm, &v);
    }
    Ok(v)
}

/// Set the current thread's handled exception (NULL = none); returns the old one (owned).
pub fn restore_handled_exception(v: *mut PyObject) -> *mut PyObject {
    set_handled_exception(crate::threads::current(), v)
}

/// BRANCH_RIGHT (jump taken) or BRANCH_LEFT (fall through) to `dest`.
#[cold]
fn branch_event(ts: &ThreadState, f: &mut Frame, taken: bool, dest: usize) -> PyResult<()> {
    use crate::modules::sys_trace as st;
    st::fire_branch(ts, f, if taken { st::BRANCH_RIGHT } else { st::BRANCH_LEFT }, dest)
}

/// FOR_ITER: continuing the loop is the left branch, leaving it the right one; an
/// exhausted generator reports STOP_ITERATION (no exception is raised for it).
#[cold]
fn for_iter_event(ts: &ThreadState, f: &mut Frame, exhausted: bool, dest: usize) -> PyResult<()> {
    use crate::modules::sys_trace as st;
    if exhausted && st::wants(st::STOP_ITERATION) {
        let si = ab::call(type_obj(exc::StopIteration()), &[])?;
        st::fire_exc_event(ts, f, st::STOP_ITERATION, &si)?;
    }
    st::fire_branch(ts, f, if exhausted { st::BRANCH_RIGHT } else { st::BRANCH_LEFT }, dest)
}

/// Instructions without control flow whose implementation tiers 0 and 1 share: the
/// interpreter dispatches them here, tier-1 code calls them through
/// `jit::helpers::h_misc` with `f.stack_top` set to the stack depth of the instruction.
#[inline]
pub(crate) fn is_misc(op: Op) -> bool {
    matches!(
        op,
        Op::DeleteDeref
            | Op::LoadFromDictOrDeref
            | Op::DeleteGlobal
            | Op::LoadFromDictOrGlobals
            | Op::GetAiter
            | Op::GetAnext
            | Op::EndAsyncFor
            | Op::GetAwaitable
            | Op::CleanupThrow
            | Op::LoadBuildClass
            | Op::PushExcInfo
            | Op::PopExcept
            | Op::CheckExcMatch
            | Op::CheckEgMatch
            | Op::WithExceptStart
            | Op::BeforeWith
            | Op::BeforeAsyncWith
            | Op::ImportName
            | Op::ImportFrom
            | Op::BuildTemplate
            | Op::BuildInterpolation
            | Op::MatchClass
            | Op::MatchMapping
            | Op::MatchSequence
            | Op::MatchKeys
            | Op::GetLen
            | Op::LoadCommonConstant
            | Op::LoadSpecial
    )
}

/// Execute one `is_misc` instruction on the value stack of the frame (`f.stack_top`).
pub(crate) fn exec_misc(ts: &ThreadState, f: &mut Frame, cd: &crate::types::code::CodeData, op: Op, arg: u32) -> PyResult<()> {
    match op {
        Op::DeleteDeref => {
            let cell = f.slots[arg as usize];
            if funcobject::cell_get(unsafe { &*cell }).is_none() {
                return Err(unbound_local(f, arg as usize));
            }
            funcobject::cell_set(unsafe { &*cell }, None);
        }
        Op::LoadFromDictOrDeref => {
            let mapping = popref!(f);
            let nm = &cd.localsplusnames[arg as usize];
            match mapping_get(&mapping, nm)? {
                Some(v) => push!(f, v.into_raw()),
                None => {
                    let cell = f.slots[arg as usize];
                    match funcobject::cell_get(unsafe { &*cell }) {
                        Some(v) => push!(f, v.into_raw()),
                        None => return Err(unbound_local(f, arg as usize)),
                    }
                }
            }
        }
        Op::DeleteGlobal => {
            let nm = &cd.names[arg as usize];
            if !dict::del_item(f.globals(), nm)? {
                return Err(name_error(f, nm));
            }
        }
        Op::LoadFromDictOrGlobals => {
            let mapping = popref!(f);
            let nm = &cd.names[arg as usize];
            let v = match mapping_get(&mapping, nm)? {
                Some(v) => lazy_check(&mapping, nm, v)?,
                None => load_global(f, nm)?,
            };
            push!(f, v.into_raw());
        }
        Op::GetAiter => {
            let o = popref!(f);
            let r = crate::types::genobject::get_aiter(&o)?;
            push!(f, r.into_raw());
        }
        Op::GetAnext => {
            let aiter = unsafe { &*top!(f) };
            let r = crate::types::genobject::get_anext_awaitable(aiter)?;
            push!(f, r.into_raw());
        }
        Op::EndAsyncFor => {
            let e = popref!(f);
            let aiter = popref!(f);
            drop(aiter);
            if is_subtype(e.type_ptr(), exc::StopAsyncIteration()) {
                // done
            } else {
                set_exc(e);
                return Err(PyErr);
            }
        }
        Op::GetAwaitable => {
            let o = popref!(f);
            push!(f, get_awaitable(&o, arg)?.into_raw());
        }
        Op::CleanupThrow => {
            let e = popref!(f);
            let _last = popopt!(f);
            let _sub = popopt!(f);
            if is_subtype(e.type_ptr(), exc::StopIteration()) {
                let val = exc::stop_iteration_value(&e);
                push!(f, none::none_ref().into_raw());
                push!(f, val.into_raw());
            } else {
                set_exc(e);
                return Err(PyErr);
            }
        }
        Op::LoadBuildClass => {
            let b = f.builtins();
            let bc_ = if dict::is_dict(b) { dict::get_item_str(b, name!("__build_class__")) } else { ab::getitem(b, name!("__build_class__")).ok() };
            match bc_ {
                Some(v) => push!(f, v.into_raw()),
                None => raise!(NameError, "__build_class__ not found"),
            }
        }
        Op::PushExcInfo => {
            let e = pop!(f);
            unsafe { incref(e) };
            let prev = set_handled_exception(ts, e);
            push!(f, if prev.is_null() { none::none_ref().into_raw() } else { prev });
            push!(f, e);
        }
        Op::PopExcept => {
            let prev = pop!(f);
            let prevv = if !prev.is_null() && unsafe { (*prev).is(none::none()) } {
                unsafe { decref(prev) };
                std::ptr::null_mut()
            } else {
                prev
            };
            let old = set_handled_exception(ts, prevv);
            unsafe { xdecref(old) };
        }
        Op::CheckExcMatch => {
            let t = popref!(f);
            let e = unsafe { &*top!(f) };
            check_exc_match_type(&t, false)?;
            let r = exception_matches(e, &t)?;
            push!(f, bool_::new_bool(r).into_raw());
        }
        Op::CheckEgMatch => {
            let t = popref!(f);
            let e = popref!(f);
            check_exc_match_type(&t, true)?;
            let (rest, matched) = crate::types::exceptiongroup::eg_match(&e, &t)?;
            if !matched.is(none::none()) {
                // the matched group becomes the handled exception
                let prev = set_handled_exception(ts, matched.clone().into_raw());
                unsafe { xdecref(prev) };
            }
            push!(f, rest.into_raw());
            push!(f, matched.into_raw());
        }
        Op::WithExceptStart => {
            let val = peek!(f, 1);
            let exit_self = peek!(f, 4);
            let exit_func = peek!(f, 5);
            let v = unsafe { &*val };
            let tp = type_ref(v.type_ptr());
            let tb = exc::exc_traceback(v).unwrap_or_else(none::none_ref);
            let r = if exit_self.is_null() {
                ab::call(unsafe { &*exit_func }, &[&tp, v, &tb])?
            } else {
                ab::call(unsafe { &*exit_func }, &[unsafe { &*exit_self }, &tp, v, &tb])?
            };
            push!(f, r.into_raw());
        }
        Op::BeforeWith | Op::BeforeAsyncWith => {
            let mgr = popref!(f);
            let (enter_n, exit_n) = if op == Op::BeforeWith {
                (name!("__enter__"), name!("__exit__"))
            } else {
                (name!("__aenter__"), name!("__aexit__"))
            };
            let enter = lookup(mgr.type_ptr(), enter_n);
            let exit = lookup(mgr.type_ptr(), exit_n);
            let (enter, exit) = match (enter, exit) {
                (Some(a), Some(b)) => (a, b),
                (en, ex) => {
                    let sync = op == Op::BeforeWith;
                    let kind = if sync { "" } else { "asynchronous " };
                    // a missing exit method is reported before a missing enter
                    let missing = if ex.is_none() {
                        if sync { "__exit__" } else { "__aexit__" }
                    } else if sync {
                        "__enter__"
                    } else {
                        "__aenter__"
                    };
                    let _ = en;
                    // suggest the other protocol when the object implements it
                    let (oe, ox, hint) = if sync {
                        (name!("__aenter__"), name!("__aexit__"), " but it supports the asynchronous context manager protocol. Did you mean to use 'async with'?")
                    } else {
                        (name!("__enter__"), name!("__exit__"), " but it supports the context manager protocol. Did you mean to use 'with'?")
                    };
                    let other = lookup(mgr.type_ptr(), oe).is_some() && lookup(mgr.type_ptr(), ox).is_some();
                    raise!(
                        TypeError,
                        "'{}' object does not support the {}context manager protocol (missed {} method){}",
                        tname(&mgr),
                        kind,
                        missing,
                        if other { hint } else { "" }
                    );
                }
            };
            // push exit as (func, self) pair
            if exit.ty().tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
                push!(f, exit.into_raw());
                push!(f, mgr.clone().into_raw());
            } else {
                let bound = match exit.ty().tp_descr_get {
                    Some(g) => unsafe { from_c(g(exit.as_ptr(), mgr.as_ptr(), mgr.type_ptr() as *mut PyObject))? },
                    None => exit,
                };
                push!(f, bound.into_raw());
                push!(f, std::ptr::null_mut());
            }
            let r = ab::call_unbound(&enter, &mgr, &[])?;
            if op == Op::BeforeAsyncWith {
                let aw = get_awaitable(&r, 1)?;
                push!(f, aw.into_raw());
            } else {
                push!(f, r.into_raw());
            }
        }
        Op::ImportName => {
            let fromlist = popref!(f);
            let level = popref!(f);
            let nm = &cd.names[(arg & 0x7fffff) as usize];
            let locals = if f.locals.is_null() { none::none_ref() } else { unsafe { (*f.locals).to_ref() } };
            let m = if arg >> 23 != 0 {
                let module_level = !f.locals.is_null() && f.locals == f.globals;
                if arg & (1 << 23) != 0 && !locals.is(f.globals()) {
                    // `lazy import` compiled by exec() inside a function or class body
                    raise!(SyntaxError, "lazy import is only allowed at module level");
                }
                crate::types::lazyimport::import_name(
                    f.globals(),
                    &locals,
                    nm,
                    &fromlist,
                    &level,
                    f.builtins(),
                    (arg >> 23) & 1 != 0,
                    (arg >> 24) & 1 != 0,
                    module_level,
                )?
            } else {
                crate::import::import_name_from_frame(f.globals(), &locals, nm, &fromlist, &level, f.builtins())?
            };
            push!(f, m.into_raw());
        }
        Op::ImportFrom => {
            let m = unsafe { &*top!(f) };
            let v = if crate::types::lazyimport::is_lazy(m) {
                crate::types::lazyimport::import_from(m, &cd.names[arg as usize])?
            } else {
                import_from(m, &cd.names[arg as usize])?
            };
            push!(f, v.into_raw());
        }
        Op::BuildTemplate => {
            let interps = popref!(f);
            let strings = popref!(f);
            let t = crate::types::templatelib::new_template(&strings, &interps)?;
            push!(f, t.into_raw());
        }
        Op::BuildInterpolation => {
            let spec = if arg & 1 != 0 { Some(popref!(f)) } else { None };
            let s = popref!(f);
            let v = popref!(f);
            let conv = arg >> 2;
            let r = crate::types::templatelib::new_interpolation(&v, &s, conv, spec.as_deref())?;
            push!(f, r.into_raw());
        }
        Op::MatchClass => {
            let names = popref!(f);
            let cls = popref!(f);
            let subject = popref!(f);
            let r = crate::interp::eval::match_class(&subject, &cls, arg as usize, &names)?;
            push!(f, r.into_raw());
        }
        Op::MatchMapping => {
            let s = unsafe { &*top!(f) };
            let r = s.ty().tp_flags & Py_TPFLAGS_MAPPING != 0;
            push!(f, bool_::new_bool(r).into_raw());
        }
        Op::MatchSequence => {
            let s = unsafe { &*top!(f) };
            let r = s.ty().tp_flags & Py_TPFLAGS_SEQUENCE != 0;
            push!(f, bool_::new_bool(r).into_raw());
        }
        Op::MatchKeys => {
            let keys = unsafe { &*top!(f) };
            let subject = unsafe { &*peek!(f, 2) };
            let r = match_keys(subject, keys)?;
            push!(f, r.into_raw());
        }
        Op::GetLen => {
            let o = unsafe { &*top!(f) };
            let n = ab::length(o)?;
            push!(f, int::new_int(n as i64).into_raw());
        }
        Op::LoadCommonConstant => {
            let v = match arg {
                0 => type_ref(exc::AssertionError()),
                1 => type_ref(exc::NotImplementedError()),
                2 => type_ref(tuple::tuple_type()),
                3 | 4 => {
                    // the original builtins.all / builtins.any (identity is what the
                    // `all(genexpr)` fast path tests), captured on first use
                    static ORIG: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
                    let (a, b) = *ORIG.get_or_init(|| {
                        let a = crate::builtins::builtin("all").map(|v| v.into_raw() as usize).unwrap_or(0);
                        let b = crate::builtins::builtin("any").map(|v| v.into_raw() as usize).unwrap_or(0);
                        (a, b)
                    });
                    let p = if arg == 3 { a } else { b };
                    if p == 0 {
                        raise!(SystemError, "builtins.all/any unavailable");
                    }
                    unsafe { PyRef::from_borrowed(p as *mut PyObject) }
                }
                5 => type_ref(list::list_type()),
                _ => type_ref(crate::types::set::set_type()),
            };
            push!(f, v.into_raw());
        }
        Op::LoadSpecial => {
            let o = popref!(f);
            let (m, s) = load_special(&o, arg)?;
            push!(f, m.into_raw());
            push!(f, s.map_or(std::ptr::null_mut(), |s| s.into_raw()));
        }
        _ => unreachable!("exec_misc: {:?}", op),
    }
    Ok(())
}
