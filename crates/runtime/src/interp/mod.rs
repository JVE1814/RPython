//! Tier-0 interpreter: function calls, argument binding, recursion control.

pub mod cache;
pub mod eval;
pub mod frame;

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
fn stack_pointer() -> usize {
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
        return None;
    }
    unsafe { Some(PyRef::from_borrowed((*f).globals)) }
}

pub fn current_frame_ptr() -> *mut Frame {
    crate::threads::current().frame.get()
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
    let co = unsafe { &*f.func_code };
    let mut frame = Frame::new(co, Some(func), unsafe { &*f.func_globals }, unsafe { &*f.func_builtins }, None);
    bind_arguments(func, &mut frame, args, nargs, kwnames)?;
    let flags = code::code(co).flags();
    if flags & (bc::co::GENERATOR | bc::co::COROUTINE | bc::co::ASYNC_GENERATOR) != 0 {
        prepare_cells(&mut frame, f);
        return crate::types::genobject::new_generator_for(frame, func, flags);
    }
    prepare_cells(&mut frame, f);
    run_frame(frame)
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

/// Called when a frame completes: hand it to its frame object if one exists.
pub fn frame_finished(mut frame: Box<Frame>) {
    frame.state = frame::FrameState::Completed;
    frame.clear_stack();
    if !frame.frame_obj.is_null() {
        unsafe { crate::types::frameobject::take_ownership(frame.frame_obj, frame) };
    } else {
        frame::recycle(frame);
    }
}

/// Copy closure cells into free slots. (Cells for cell variables are created by MakeCell.)
fn prepare_cells(frame: &mut Frame, f: &PyFunctionObject) {
    let _ = (frame, f);
}

/// Put arguments into localsplus according to CPython's binding rules.
pub fn bind_arguments(func: &PyObject, frame: &mut Frame, args: &[*mut PyObject], nargs: usize, kwnames: Option<&PyObject>) -> PyResult<()> {
    let f = funcobject::func(func);
    let code_ref = frame.code().to_ref();
    let cd = code::code(&code_ref);
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
                    let ks = pystr::to_string_lossy(key);
                    let suggestion = crate::modules::suggestions::suggest_keyword(&ks, &names[..total_named]);
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
        let defcount = if f.func_defaults.is_null() { 0 } else { tuple::tuple_len(unsafe { &*f.func_defaults }) };
        let kwonly_given = (argcount..total_named).filter(|&i| !slots[i].is_null()).count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let takes = if defcount > 0 {
            format!("from {} to {} positional arguments", argcount - defcount, argcount)
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
            let b = crate::builtins::builtins_module();
            dict::set_item(globals, crate::name!("__builtins__"), &b)?;
            crate::builtins::builtins_dict()
        }
    };
    let frame = Frame::new(code, None, globals, &builtins, locals);
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
            run_frame(frame)
        }
    }
}
