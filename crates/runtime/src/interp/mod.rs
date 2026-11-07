//! Tier-0 interpreter: function calls, argument binding, recursion control.

pub mod cache;
pub mod eval;
pub mod frame;
pub mod jump;
pub mod jit;
pub mod specialize;
pub mod adaptive;
pub mod pep523;
pub mod translate;
pub mod wc;

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::funcobject::{self, PyFunctionObject};
use crate::types::{code, dict, str as pystr, tuple};
use frame::Frame;
use rpy_compiler::bytecode as bc;

pub use eval::{Exit, eval_frame};

/// Recursion check covering both the Python-level limit and the native stack.
#[inline]
pub fn enter_recursive_call(ts: &ThreadState, where_: &str) -> PyResult<()> {
    let d = ts.recursion_depth.get() + 1;
    ts.recursion_depth.set(d);
    // (perf4) the common case in two compares; limits/headroom in the slow path
    if d <= ts.c.py_recursion_limit && stack_pointer() >= ts.stack_soft_limit() {
        return Ok(());
    }
    enter_recursive_slow(ts, d, where_)
}

#[cold]
#[inline(never)]
fn enter_recursive_slow(ts: &ThreadState, d: i32, where_: &str) -> PyResult<()> {
    let limit = ts.c.py_recursion_limit;
    if d > limit + if ts.recursion_headroom_active() { 50 } else { 0 } || native_stack_exhausted(ts) {
        if !ts.recursion_headroom_active() {
            ts.set_recursion_headroom(true);
            let r = Err(err!(RecursionError, "maximum recursion depth exceeded{}", where_));
            ts.recursion_depth.set(d - 1);
            ts.set_recursion_headroom(false);
            return r;
        }
        if native_stack_exhausted_hard(ts) {
            ts.recursion_depth.set(d - 1);
            return Err(err!(RecursionError, "maximum recursion depth exceeded{}", where_));
        }
    }
    Ok(())
}

#[inline]
pub fn leave_recursive_call(ts: &ThreadState) {
    ts.recursion_depth.set(ts.recursion_depth.get() - 1);
}

#[inline(always)]
pub(crate) fn stack_pointer() -> usize {
    let x = 0u8;
    &x as *const u8 as usize
}

#[inline]
fn native_stack_exhausted(ts: &ThreadState) -> bool {
    stack_pointer() < ts.stack_soft_limit()
}
#[inline]
fn native_stack_exhausted_hard(ts: &ThreadState) -> bool {
    stack_pointer() < ts.stack_hard_limit()
}

/// globals of the currently executing Python frame.
pub fn current_globals() -> Option<PyRef> {
    let ts = crate::threads::current();
    let f = ts.frame.get();
    if f.is_null() {
        // (interp1) no Python frame, but the interpreter is running code for
        // `_interpreters`: that code's globals are __main__'s
        return crate::subinterp::running_main_globals();
    }
    unsafe { Some(PyRef::from_borrowed((*f).globals)) }
}

/// builtins mapping of the currently executing frame, else the builtins dict
/// (PyEval_GetBuiltins).
pub fn current_builtins() -> PyRef {
    let f = crate::threads::current().frame.get();
    if !f.is_null() {
        let b = unsafe { (*f).builtins };
        if !b.is_null() {
            return unsafe { PyRef::from_borrowed(b) };
        }
    }
    crate::builtins::builtins_dict()
}

pub fn current_frame_ptr() -> *mut Frame {
    // no attached state (e.g. PyTraceMalloc_Track called by an extension with the
    // GIL released): no frame. Attaching a state here leaked one per call.
    let ts = crate::threads::current_ptr();
    if ts.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*ts).frame.get() }
}

/// vectorcall entry for Python functions.
pub unsafe extern "C" fn function_vectorcall(
    func: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let all: &[*mut PyObject] = if nargs + nkw == 0 { &[] } else { std::slice::from_raw_parts(args, nargs + nkw) };
        into_c(call_function(&*func, all, nargs, if kwnames.is_null() { None } else { Some(&*kwnames) }))
    }
}

/// Call a Python function with vectorcall-style arguments.
pub fn call_function(func: &PyObject, args: &[*mut PyObject], nargs: usize, kwnames: Option<&PyObject>) -> PyResult {
    let f: &PyFunctionObject = funcobject::func(func);
    if f.func_code.is_null() {
        return Err(system_error("function object without code"));
    }
    let co = unsafe { &*f.func_code };
    let cobj = code::code(co).obj;
    let flags = cobj.co_flags as u32;
    if flags & (bc::co::GENERATOR | bc::co::COROUTINE | bc::co::ASYNC_GENERATOR) != 0 {
        // jit4: exact positional call of a generator function: arguments copied
        // straight into the generator's frame
        if kwnames.is_none()
            && nargs == cobj.co_argcount as usize
            && flags & (bc::co::VARARGS | bc::co::VARKEYWORDS) == 0
            && cobj.co_kwonlyargcount == 0
            && !code::code_data(co).eager_prologue
            // (module-level code with top-level await stores names: general path)
            && flags & bc::co::OPTIMIZED != 0
        {
            let mut frame = Frame::new_call(co, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
            for (i, &a) in args[..nargs].iter().enumerate() {
                unsafe { incref(a) };
                frame.slots[i] = a;
            }
            frame.make_owned();
            copy_free_vars(&mut frame, f);
            pep523::note_entry(co); // (capi2) creating a generator runs its frame to the start
            return crate::types::genobject::new_generator_for(frame, func, flags);
        }
        if kwnames.is_none() && nargs == 1 {
            if let Some(r) = new_genexpr(func, f, co, args[0], flags) {
                return r;
            }
        }
    } else if let Some(r) = jit::call_fast(func, args, nargs, kwnames) {
        return r; // jit workstream: compiled callee, direct entry
    } else if flags & bc::co::OPTIMIZED != 0 {
        // (perf4) any other argument shape: bind, then enter compiled code directly
        if let Some(r) = jit::call_bound(func, args, nargs, kwnames) {
            return r;
        }
    }
    // plain calls borrow code/func from the caller (perf-runtime, frame.rs `owns_fc`)
    let mut frame = Frame::new_call(co, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
    if flags & bc::co::OPTIMIZED == 0 && frame.locals.is_null() {
        // a function made from module-level code (FunctionType(compile(...)))
        // stores its names into its globals
        frame.locals = unsafe { (*f.func_globals).to_ref().into_raw() };
    }
    if let Err(e) = bind_arguments(func, &mut frame, args, nargs, kwnames) {
        frame::recycle(frame);
        return Err(e);
    }
    if flags & (bc::co::GENERATOR | bc::co::COROUTINE | bc::co::ASYNC_GENERATOR) != 0 {
        frame.make_owned();
        prepare_cells(&mut frame, f);
        if code::code_data(co).eager_prologue {
            // RETURN_GENERATOR is preceded by real work (a genexpr's `iter()`): run the
            // frame up to it now, so errors surface at call time.
            frame = run_generator_prologue(frame)?;
        } else {
            // the free variables are in place from creation on (CPython copies them before
            // RETURN_GENERATOR), so f_locals of a not yet started frame shows them; the
            // CopyFreeVars at the first resume stores the same cells again
            copy_free_vars(&mut frame, f);
            pep523::note_entry(co);
        }
        return crate::types::genobject::new_generator_for(frame, func, flags);
    }
    prepare_cells(&mut frame, f);
    run_frame(frame)
}

/// (perf4) `func(*args, **kw)` for a Python function (CALL_FUNCTION_EX): `args` is an
/// exact tuple, `kw` a non-empty exact dict or None. Arguments are bound straight
/// from the tuple and the dict (no argument vector, no kwnames tuple); a function
/// whose only parameter is `*args` receives the tuple itself. Anything unusual
/// (errors to report, non-str keys) goes through the general vectorcall path.
pub fn call_function_ex(func: &PyObject, self_: Option<&PyObject>, args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    let f: &PyFunctionObject = funcobject::func(func);
    if !f.func_code.is_null() {
        let co = unsafe { &*f.func_code };
        let cobj = code::code(co).obj;
        let flags = cobj.co_flags as u32;
        let plain = flags & (bc::co::GENERATOR | bc::co::COROUTINE | bc::co::ASYNC_GENERATOR) == 0 && flags & bc::co::OPTIMIZED != 0;
        if plain {
            let items = tuple::tuple_items(args);
            if kw.is_none() && self_.is_none() && cobj.co_argcount == 0 && cobj.co_kwonlyargcount == 0 && flags & bc::co::VARKEYWORDS == 0 && flags & bc::co::VARARGS != 0 {
                let mut frame = Frame::new_call(co, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
                frame.slots[0] = args.to_ref().into_raw();
                return jit::run_bound(func, frame);
            }
            let n = items.len() + self_.is_some() as usize;
            if n <= 16 {
                let mut buf: [*mut PyObject; 16] = [std::ptr::null_mut(); 16];
                let mut i = 0;
                if let Some(s) = self_ {
                    buf[0] = s.as_ptr();
                    i = 1;
                }
                for (k, it) in items.iter().enumerate() {
                    buf[i + k] = it.as_ptr();
                }
                match kw {
                    None => return call_function(func, &buf[..n], n, None),
                    Some(d) if dict::is_dict_exact(d) => {
                        let mut frame = Frame::new_call(co, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
                        match bind_kwdict(f, &mut frame, &buf[..n], d) {
                            Ok(true) => return jit::run_bound(func, frame),
                            Ok(false) => frame::recycle(frame),
                            Err(e) => {
                                frame::recycle(frame);
                                return Err(e);
                            }
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    }
    let mut v: Vec<&PyObject> = Vec::with_capacity(tuple::tuple_len(args) + 1);
    if let Some(s) = self_ {
        v.push(s);
    }
    v.extend_from_slice(tuple::tuple_items(args));
    crate::abstract_::call_with_kwargs_dict(func, &v, kw)
}

/// Bind positional `args` and the entries of exact dict `kw` into `frame` (see
/// `call_function_ex`). Ok(false): a case left to the general binder (which reports
/// the errors); the frame is then partly filled and must be discarded.
fn bind_kwdict(f: &PyFunctionObject, frame: &mut Frame, args: &[*mut PyObject], kw: &PyObject) -> PyResult<bool> {
    let cd = code::code(unsafe { &*frame.code });
    let co = cd.obj;
    let argcount = co.co_argcount as usize;
    let posonly = co.co_posonlyargcount as usize;
    let kwonly = co.co_kwonlyargcount as usize;
    let flags = co.co_flags as u32;
    let total_named = argcount + kwonly;
    let has_varargs = flags & bc::co::VARARGS != 0;
    let has_varkw = flags & bc::co::VARKEYWORDS != 0;
    let nargs = args.len();
    if nargs > argcount && !has_varargs {
        return Ok(false);
    }
    let slots = &mut frame.slots;
    for i in 0..nargs.min(argcount) {
        unsafe { incref(args[i]) };
        slots[i] = args[i];
    }
    if has_varargs {
        let extra = if nargs > argcount { unsafe { tuple::new_tuple_from_ptrs(&args[argcount..nargs]) } } else { tuple::empty_tuple() };
        slots[total_named] = extra.into_raw();
    }
    let kwnew = if has_varkw {
        let d = dict::new_dict_presized(dict::dict_len(kw));
        slots[total_named + has_varargs as usize] = d.as_ptr();
        Some(d.into_raw())
    } else {
        None
    };
    let names = &cd.d.localsplusnames;
    {
        // (no Python code runs below: exact str keys, str comparisons)
        let _g = dict::ob_lock(kw);
        let t = dict::table(kw);
        for e in t.entries().iter() {
            if e.key.is_null() {
                continue;
            }
            let key = unsafe { &*e.key };
            if !pystr::is_str_exact(key) {
                return Ok(false);
            }
            let mut found = None;
            for i in posonly..total_named {
                if names[i].is(key) {
                    found = Some(i);
                    break;
                }
            }
            if found.is_none() {
                for i in posonly..total_named {
                    if pystr::str_eq(&names[i], key) {
                        found = Some(i);
                        break;
                    }
                }
            }
            match found {
                Some(i) => {
                    if !slots[i].is_null() {
                        return Ok(false);
                    }
                    unsafe { incref(e.value) };
                    slots[i] = e.value;
                }
                None => match kwnew {
                    // keys of a dict are unique: no comparisons needed
                    Some(d) => dict::insert_unique(unsafe { &*d }, key, e.hash, unsafe { &*e.value }),
                    None => return Ok(false),
                },
            }
        }
    }
    if nargs < argcount {
        let defs = if f.func_defaults.is_null() { &[][..] } else { tuple::tuple_items(unsafe { &*f.func_defaults }) };
        let defs = &defs[defs.len().saturating_sub(argcount)..];
        let first_default = argcount - defs.len();
        for i in nargs..argcount {
            if slots[i].is_null() {
                if i < first_default {
                    return Ok(false);
                }
                slots[i] = defs[i - first_default].to_ref().into_raw();
            }
        }
    }
    for i in argcount..total_named {
        if slots[i].is_null() {
            let nm = &cd.d.localsplusnames[i];
            let d = if f.func_kwdefaults.is_null() { None } else { dict::get_item_str(unsafe { &*f.func_kwdefaults }, nm) };
            match d {
                Some(v) => slots[i] = v.into_raw(),
                None => return Ok(false),
            }
        }
    }
    Ok(true)
}

/// Number of free variables if the code's eager generator prologue is exactly a
/// generator expression's `iter(.0)` (optional CopyFreeVars, Resume/Nop, LoadFast 0,
/// GetIter, ReturnGenerator), cached per code object.
fn genexpr_prologue(cd: &code::CodeData) -> Option<u32> {
    use bc::Op;
    use std::sync::atomic::Ordering::Relaxed;
    let s = cd.caches.genexpr.load(Relaxed);
    if s != 0 {
        return if s >= 2 { Some(s - 2) } else { None };
    }
    let mut nfree = 0;
    let mut seen_load = false;
    let mut seen_iter = false;
    let mut ok = false;
    for (i, &w) in cd.code.iter().enumerate() {
        let (op, a) = bc::decode(w);
        match op {
            Op::Nop | Op::Resume if !seen_load => {}
            Op::CopyFreeVars if !seen_load && i == 0 => nfree = a,
            Op::LoadFast if !seen_load && a == 0 => seen_load = true,
            Op::GetIter if seen_load && !seen_iter => seen_iter = true,
            Op::ReturnGenerator if seen_iter => {
                ok = i + 1 == cd.gen_start as usize;
                break;
            }
            _ => break,
        }
    }
    cd.caches.genexpr.store(if ok { 2 + nfree } else { 1 }, Relaxed);
    if ok { Some(nfree) } else { None }
}

/// Create the generator of a generator expression without running its prologue in
/// the interpreter: `iter()` of an exact builtin container (which cannot fail or run
/// Python code) is done here. None: use the general path.
#[inline]
fn new_genexpr(func: &PyObject, f: &PyFunctionObject, co: &PyObject, arg: *mut PyObject, flags: u32) -> Option<PyResult> {
    use crate::types::{dict as d, list as l, range as r, set as s, str as st};
    let cd = code::code_data(co);
    if !cd.eager_prologue || flags & bc::co::GENERATOR == 0 {
        return None;
    }
    let cobj = code::code(co).obj;
    if cobj.co_argcount != 1 || cobj.co_kwonlyargcount != 0 || flags & (bc::co::VARARGS | bc::co::VARKEYWORDS) != 0 {
        return None;
    }
    let nfree = genexpr_prologue(cd)? as usize;
    if crate::modules::sys::tracing_active() {
        return None;
    }
    let a = unsafe { &*arg };
    let tp = a.type_ptr();
    let builtin = unsafe {
        [
            &raw mut l::PyList_Type,
            &raw mut tuple::PyTuple_Type,
            &raw mut r::PyRange_Type,
            &raw mut d::PyDict_Type,
            &raw mut s::PySet_Type,
            &raw mut s::PyFrozenSet_Type,
            &raw mut st::PyUnicode_Type,
            crate::types::genobject::gen_type(),
        ]
    };
    if !builtin.contains(&tp) {
        return None;
    }
    let it = match crate::abstract_::get_iter(a) {
        Ok(it) => it,
        Err(e) => return Some(Err(e)),
    };
    let mut frame = Frame::new_call(co, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
    unsafe { incref(arg) };
    frame.slots[0] = arg;
    if nfree > 0 {
        if f.func_closure.is_null() {
            frame::recycle(frame);
            return None;
        }
        let items = tuple::tuple_items(unsafe { &*f.func_closure });
        let base = frame.nlocalsplus - nfree;
        for (i, c) in items.iter().take(nfree).enumerate() {
            frame.slots[base + i] = c.to_ref().into_raw();
        }
    }
    let nlp = frame.nlocalsplus;
    frame.slots[nlp] = it.into_raw();
    frame.stack_top = nlp + 1;
    frame.lasti = cd.gen_start - 1;
    frame.next_instr = cd.gen_start;
    frame.make_owned();
    pep523::note_entry(co);
    Some(crate::types::genobject::new_generator_for(frame, func, flags))
}

/// Execute a fresh frame to completion on the current thread.
pub fn run_frame(mut frame: Box<Frame>) -> PyResult {
    let ts = crate::threads::current();
    enter_recursive_call(ts, "")?;
    let r = eval::eval_frame(ts, &mut frame, None);
    leave_recursive_call(ts);
    frame_finished(frame);
    match r {
        Ok(Exit::Return(v)) => Ok(v),
        Ok(Exit::Yield(_)) => Err(system_error("yield from a non-generator frame")),
        Err(e) => Err(e),
    }
}

/// Run a generator frame's eager prologue up to the internal `ReturnGenerator`; the frame
/// is then "just created" again (nothing is pushed on the first resume).
fn run_generator_prologue(mut frame: Box<Frame>) -> PyResult<Box<Frame>> {
    let ts = crate::threads::current();
    if let Err(e) = enter_recursive_call(ts, "") {
        frame_finished(frame);
        return Err(e);
    }
    let r = eval::eval_frame(ts, &mut frame, None);
    leave_recursive_call(ts);
    match r {
        Ok(Exit::Yield(_)) => {
            frame.state = frame::FrameState::Created;
            Ok(frame)
        }
        Ok(Exit::Return(_)) => {
            frame_finished(frame);
            Err(system_error("generator prologue returned"))
        }
        Err(e) => {
            frame_finished(frame);
            Err(e)
        }
    }
}

/// Is the activation's self-reference the only reference to frame object `fo`?
/// (Owned by this thread with a local count of 1 and no shared references.)
fn frame_obj_unshared(fo: *mut PyObject) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let o = unsafe { &*fo };
    o.ob_tid.load(Relaxed) == crate::object::thread_id() && o.ob_ref_local.load(Relaxed) == 1 && o.ob_ref_shared.load(Relaxed) == 0
}

/// Called when a frame completes: hand it to its frame object if one exists.
pub fn frame_finished(mut frame: Box<Frame>) {
    frame.state = frame::FrameState::Completed;
    frame.clear_stack();
    if !frame.frame_obj.is_null() && frame_obj_unshared(frame.frame_obj) {
        // (jit5) nothing but the activation's own self-reference keeps the frame
        // object: it dies with the activation. (Handing the frame over would
        // materialise the caller's frame object as f_back, which then repeats for
        // every caller up the stack, one allocation per finished call.)
        unsafe {
            let fo = std::mem::replace(&mut frame.frame_obj, std::ptr::null_mut());
            (*(fo as *mut crate::types::frameobject::PyFrameObject)).frame = std::ptr::null_mut();
            decref(fo);
        }
    }
    if !frame.frame_obj.is_null() {
        // the frame object outlives the activation: it keeps its caller as f_back
        // (tail3) not a generator's frame: its f_back is None once it stops running,
        // and a hidden link would keep whoever last resumed it alive
        let caller = crate::threads::current().frame.get();
        if !caller.is_null() && !std::ptr::eq(caller, &*frame) && frame.generator.is_null() {
            let back = eval::frame_object(unsafe { &mut *caller });
            unsafe { crate::types::frameobject::set_back(frame.frame_obj, back) };
        }
        frame.make_owned();
        unsafe { crate::types::frameobject::take_ownership(frame.frame_obj, frame) };
    } else {
        frame::recycle(frame);
    }
}

/// Copy closure cells into free slots. (Cells for cell variables are created by MakeCell.)
fn prepare_cells(frame: &mut Frame, f: &PyFunctionObject) {
    let _ = (frame, f);
}

fn copy_free_vars(frame: &mut Frame, f: &PyFunctionObject) {
    if f.func_closure.is_null() {
        return;
    }
    let n = code::code(frame.code()).nfreevars();
    let items = tuple::tuple_items(unsafe { &*f.func_closure });
    if n == 0 || items.len() < n || frame.nlocalsplus < n {
        return;
    }
    let base = frame.nlocalsplus - n;
    for i in 0..n {
        let old = std::mem::replace(&mut frame.slots[base + i], items[i].to_ref().into_raw());
        unsafe { xdecref(old) };
    }
}

/// Put arguments into localsplus according to CPython's binding rules.
pub fn bind_arguments(func: &PyObject, frame: &mut Frame, args: &[*mut PyObject], nargs: usize, kwnames: Option<&PyObject>) -> PyResult<()> {
    let f = funcobject::func(func);
    // (perf4) borrowed: the frame holds the code for the whole call
    let cd = code::code(unsafe { &*frame.code });
    let co = cd.obj;
    let argcount = co.co_argcount as usize;
    let posonly = co.co_posonlyargcount as usize;
    let kwonly = co.co_kwonlyargcount as usize;
    let flags = co.co_flags as u32;
    let total_named = argcount + kwonly;
    let has_varargs = flags & bc::co::VARARGS != 0;
    let has_varkw = flags & bc::co::VARKEYWORDS != 0;
    let slots = &mut frame.slots;

    // fast path: exact positional match, no kwargs
    if kwnames.is_none() && nargs == argcount && !has_varargs && !has_varkw && kwonly == 0 {
        for i in 0..nargs {
            unsafe { incref(args[i]) };
            slots[i] = args[i];
        }
        return Ok(());
    }
    // (perf4) positional call relying on trailing defaults
    if kwnames.is_none() && nargs < argcount && !has_varargs && !has_varkw && kwonly == 0 && !f.func_defaults.is_null() {
        let defs = tuple::tuple_items(unsafe { &*f.func_defaults });
        let ndef = defs.len().min(argcount);
        if nargs + ndef >= argcount {
            for i in 0..nargs {
                unsafe { incref(args[i]) };
                slots[i] = args[i];
            }
            let first_default = argcount - ndef;
            let defs = &defs[defs.len() - ndef..];
            for i in nargs..argcount {
                slots[i] = defs[i - first_default].to_ref().into_raw();
            }
            return Ok(());
        }
    }

    // (perf5) keywords that name parameters by identity (interned names), no *args /
    // **kwargs: bind directly; anything else (and every error) takes the general path
    if let Some(kn) = kwnames {
        if !has_varargs && !has_varkw && nargs <= argcount && bind_kw_fast(f, &cd.d.localsplusnames, slots, args, nargs, kn, posonly, argcount, total_named) {
            return Ok(());
        }
    }
    let qualname = || pystr::to_string_lossy(unsafe { &*f.func_qualname });
    let mut kwdict: Option<PyRef> = None;
    if has_varkw {
        let d = dict::new_dict();
        let idx = total_named + has_varargs as usize;
        slots[idx] = d.clone().into_raw();
        kwdict = Some(d);
    }
    let n = nargs.min(argcount);
    for i in 0..n {
        unsafe { incref(args[i]) };
        slots[i] = args[i];
    }
    if has_varargs {
        let extra = if nargs > argcount { unsafe { tuple::new_tuple_from_ptrs(&args[argcount..nargs]) } } else { tuple::empty_tuple() };
        slots[total_named] = extra.into_raw();
    }
    // keyword arguments
    let mut posonly_as_kw: Vec<String> = Vec::new();
    if let Some(kn) = kwnames {
        let names = &cd.d.localsplusnames;
        for (j, key) in tuple::tuple_items(kn).iter().enumerate() {
            if !pystr::is_str(key) {
                raise!(TypeError, "{}() keywords must be strings", qualname());
            }
            let value = args[nargs + j];
            let mut found = None;
            // identity first, then equality
            for i in posonly..total_named {
                if names[i].is(key) {
                    found = Some(i);
                    break;
                }
            }
            if found.is_none() {
                for i in posonly..total_named {
                    if pystr::str_eq(&names[i], key) {
                        found = Some(i);
                        break;
                    }
                }
            }
            match found {
                Some(i) => {
                    if !slots[i].is_null() {
                        raise!(TypeError, "{}() got multiple values for argument '{}'", qualname(), pystr::to_string_lossy(key));
                    }
                    unsafe { incref(value) };
                    slots[i] = value;
                }
                None => {
                    if let Some(d) = &kwdict {
                        if !pystr::is_str(key) {
                            raise!(TypeError, "{}() keywords must be strings", qualname());
                        }
                        dict::set_item(d, key, unsafe { &*value })?;
                        continue;
                    }
                    if names[..posonly].iter().any(|n| pystr::str_eq(n, key)) {
                        posonly_as_kw.push(pystr::to_string_lossy(key));
                        continue;
                    }
                    // a positional-only name passed by keyword anywhere in the call
                    // is reported in preference to the unknown keyword (grind-lib)
                    let po: Vec<String> = tuple::tuple_items(kn)
                        .iter()
                        .filter(|k| names[..posonly].iter().any(|n| pystr::str_eq(n, k)))
                        .map(|k| pystr::to_string_lossy(k))
                        .collect();
                    if !po.is_empty() {
                        raise!(
                            TypeError,
                            "{}() got some positional-only arguments passed as keyword arguments: '{}'",
                            qualname(),
                            po.join(", ")
                        );
                    }
                    let ks = pystr::to_string_lossy(key);
                    let suggestion = crate::modules::suggestions::suggest_keyword(&ks, &names[posonly..total_named]);
                    match suggestion {
                        Some(s) => raise!(
                            TypeError,
                            "{}() got an unexpected keyword argument '{}'. Did you mean '{}'?",
                            qualname(),
                            ks,
                            s
                        ),
                        None => raise!(TypeError, "{}() got an unexpected keyword argument '{}'", qualname(), ks),
                    }
                }
            }
        }
    }
    if !posonly_as_kw.is_empty() {
        raise!(
            TypeError,
            "{}() got some positional-only arguments passed as keyword arguments: '{}'",
            qualname(),
            posonly_as_kw.join(", ")
        );
    }
    // too many positional
    if nargs > argcount && !has_varargs {
        let defcount = if f.func_defaults.is_null() { 0 } else { tuple::tuple_len(unsafe { &*f.func_defaults }) }.min(argcount);
        let kwonly_given = (argcount..total_named).filter(|&i| !slots[i].is_null()).count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let takes = if defcount > 0 {
            format!("from {} to {} positional arguments", argcount - defcount.min(argcount), argcount)
        } else {
            format!("{} positional argument{}", argcount, plural(argcount))
        };
        if kwonly_given > 0 {
            raise!(
                TypeError,
                "{}() takes {} but {} positional argument{} (and {} keyword-only argument{}) were given",
                qualname(),
                takes,
                nargs,
                plural(nargs),
                kwonly_given,
                plural(kwonly_given)
            );
        }
        raise!(
            TypeError,
            "{}() takes {} but {} {} given",
            qualname(),
            takes,
            nargs,
            if nargs == 1 { "was" } else { "were" }
        );
    }
    // defaults for positional
    if nargs < argcount {
        let defs = if f.func_defaults.is_null() { &[][..] } else { tuple::tuple_items(unsafe { &*f.func_defaults }) };
        // more defaults than parameters: the surplus leading ones are ignored
        let defs = &defs[defs.len().saturating_sub(argcount)..];
        let first_default = argcount - defs.len();
        let mut missing: Vec<String> = Vec::new();
        for i in nargs..argcount {
            if slots[i].is_null() {
                if i >= first_default {
                    let d = defs[i - first_default];
                    slots[i] = d.to_ref().into_raw();
                } else {
                    missing.push(pystr::to_string_lossy(&cd.d.localsplusnames[i]));
                }
            }
        }
        if !missing.is_empty() {
            raise!(
                TypeError,
                "{}() missing {} required positional argument{}: {}",
                qualname(),
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                format_names(&missing)
            );
        }
    }
    // keyword-only defaults
    if kwonly > 0 {
        let mut missing: Vec<String> = Vec::new();
        for i in argcount..total_named {
            if slots[i].is_null() {
                let nm = &cd.d.localsplusnames[i];
                let d = if f.func_kwdefaults.is_null() { None } else { dict::get_item_str(unsafe { &*f.func_kwdefaults }, nm) };
                match d {
                    Some(v) => slots[i] = v.into_raw(),
                    None => missing.push(pystr::to_string_lossy(nm)),
                }
            }
        }
        if !missing.is_empty() {
            raise!(
                TypeError,
                "{}() missing {} required keyword-only argument{}: {}",
                qualname(),
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                format_names(&missing)
            );
        }
    }
    Ok(())
}

/// `bind_arguments` for a call with keywords that each name a distinct parameter
/// (not positional-only, not already given) by identity, defaults filling the rest.
/// False with the slots of the named parameters cleared again when anything else
/// applies (the general path then binds from scratch and reports errors).
#[allow(clippy::too_many_arguments)]
fn bind_kw_fast(
    f: &PyFunctionObject,
    names: &[PyRef],
    slots: &mut [*mut PyObject],
    args: &[*mut PyObject],
    nargs: usize,
    kn: &PyObject,
    posonly: usize,
    argcount: usize,
    total_named: usize,
) -> bool {
    let keys = tuple::tuple_items(kn);
    if names.len() < total_named || slots.len() < total_named || args.len() < nargs + keys.len() {
        return false;
    }
    for i in 0..nargs {
        unsafe { incref(args[i]) };
        slots[i] = args[i];
    }
    let undo = |slots: &mut [*mut PyObject]| {
        for s in slots[..total_named].iter_mut() {
            unsafe { xdecref(std::mem::replace(s, std::ptr::null_mut())) };
        }
        false
    };
    let lo = posonly.max(nargs);
    for (j, key) in keys.iter().enumerate() {
        let mut found = usize::MAX;
        for i in lo..total_named {
            if names[i].is(key) {
                found = i;
                break;
            }
        }
        if found == usize::MAX || !slots[found].is_null() {
            return undo(slots);
        }
        let v = args[nargs + j];
        unsafe { incref(v) };
        slots[found] = v;
    }
    if nargs + keys.len() < total_named {
        let defs = if f.func_defaults.is_null() { &[][..] } else { tuple::tuple_items(unsafe { &*f.func_defaults }) };
        let defs = &defs[defs.len().saturating_sub(argcount)..];
        let first_default = argcount - defs.len();
        for i in nargs..argcount {
            if slots[i].is_null() {
                if i < first_default {
                    return undo(slots);
                }
                slots[i] = defs[i - first_default].to_ref().into_raw();
            }
        }
        for i in argcount..total_named {
            if slots[i].is_null() {
                let d = if f.func_kwdefaults.is_null() { None } else { dict::get_item_str(unsafe { &*f.func_kwdefaults }, &names[i]) };
                match d {
                    Some(v) => slots[i] = v.into_raw(),
                    None => return undo(slots),
                }
            }
        }
    }
    true
}

fn format_names(v: &[String]) -> String {
    let q: Vec<String> = v.iter().map(|s| format!("'{}'", s)).collect();
    match q.len() {
        1 => q[0].clone(),
        2 => format!("{} and {}", q[0], q[1]),
        _ => format!("{}, and {}", q[..q.len() - 1].join(", "), q[q.len() - 1]),
    }
}

use crate::raise;

/// Execute a code object (module/exec/eval) with given globals and locals mapping.
pub fn eval_code(code: &PyObject, globals: &PyObject, locals: Option<&PyObject>) -> PyResult {
    let builtins = match dict::get_item_str(globals, crate::name!("__builtins__")) {
        Some(b) => {
            if crate::types::module::is_module(&b) {
                crate::types::module::module_dict(&b)
            } else {
                b
            }
        }
        None => {
            let b = current_builtins();
            dict::set_item(globals, crate::name!("__builtins__"), &b)?;
            b
        }
    };
    let frame = Frame::new(code, None, globals, &builtins, locals);
    run_or_wrap(frame)
}

/// Run a frame of module-level code; code compiled with top-level await (a coroutine
/// flag on non-function code) gives the coroutine to run instead, as CPython's eval does.
fn run_or_wrap(mut frame: Box<Frame>) -> PyResult {
    let co = frame.code().to_ref();
    let flags = code::code(&co).flags();
    if flags & (bc::co::GENERATOR | bc::co::COROUTINE | bc::co::ASYNC_GENERATOR) != 0 {
        frame.make_owned();
        return crate::types::genobject::new_generator_named(frame, &code::code_name(&co), &code::code_qualname(&co), flags);
    }
    run_frame(frame)
}

/// Like eval_code but with an explicit closure (exec of code with free vars).
pub fn eval_code_with_closure(code: &PyObject, globals: &PyObject, locals: Option<&PyObject>, closure: Option<&PyObject>) -> PyResult {
    match closure {
        None => eval_code(code, globals, locals),
        Some(c) => {
            let builtins = crate::builtins::builtins_dict();
            let mut frame = Frame::new(code, None, globals, &builtins, locals);
            let n = tuple::tuple_len(c);
            let nlp = frame.nlocalsplus;
            for (i, cell) in tuple::tuple_items(c).iter().enumerate() {
                frame.slots[nlp - n + i] = cell.to_ref().into_raw();
            }
            run_or_wrap(frame)
        }
    }
}
