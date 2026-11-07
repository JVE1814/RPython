//! (remote1) Target-side support for out-of-process readers: the asyncio task registry
//! that `_remote_debugging` walks, and the safe-point handler that runs a script queued
//! by `sys.remote_exec`.

use super::offsets::{RemoteExecArea, TaskNode, EVAL_REMOTE_EXEC, REMOTE_EXEC, RPY_DEBUG_TABLE};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::threads::ThreadState;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering::*};

// ------------------------------------------------------------------ task registry

/// Head of the list of registered tasks (read by debuggers through the debug table).
pub static TASK_HEAD: AtomicPtr<TaskNode> = AtomicPtr::new(std::ptr::null_mut());

struct Registry {
    /// task address -> node
    nodes: HashMap<usize, usize>,
    tail: usize,
    /// recycled nodes (never freed: a reader racing an unlink only sees stale data)
    free: Vec<usize>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

thread_local! {
    /// The task currently marked running on this thread.
    static RUNNING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let r = g.get_or_insert_with(|| Registry { nodes: HashMap::new(), tail: 0, free: Vec::new() });
    f(r)
}

/// Register `task` (borrowed; unregistered before it dies) as belonging to this thread.
pub fn link_task(task: *mut PyObject) {
    let ts = crate::threads::current_ptr();
    with_registry(|r| {
        if r.nodes.contains_key(&(task as usize)) {
            return;
        }
        let node = match r.free.pop() {
            Some(n) => n as *mut TaskNode,
            None => Box::into_raw(Box::new(TaskNode {
                task: std::ptr::null_mut(),
                ts: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                running: 0,
            })),
        };
        unsafe {
            (*node).task = task;
            (*node).ts = ts;
            (*node).running = 0;
            (*node).next = std::ptr::null_mut();
            (*node).prev = r.tail as *mut TaskNode;
            if r.tail != 0 {
                (*(r.tail as *mut TaskNode)).next = node;
            } else {
                TASK_HEAD.store(node, Release);
            }
        }
        r.tail = node as usize;
        r.nodes.insert(task as usize, node as usize);
    });
}

/// Remove `task` from the registry (no-op when it is not registered).
pub fn unlink_task(task: *mut PyObject) {
    with_registry(|r| {
        let Some(n) = r.nodes.remove(&(task as usize)) else { return };
        let node = n as *mut TaskNode;
        unsafe {
            let prev = (*node).prev;
            let next = (*node).next;
            if prev.is_null() {
                TASK_HEAD.store(next, Release);
            } else {
                (*prev).next = next;
            }
            if next.is_null() {
                r.tail = prev as usize;
            } else {
                (*next).prev = prev;
            }
            (*node).task = std::ptr::null_mut();
            (*node).running = 0;
        }
        r.free.push(n);
    });
    RUNNING.with(|c| {
        if c.get() == task as usize {
            c.set(0);
        }
    });
}

/// Mark `task` (or nothing, for NULL) as the task running a step on this thread.
pub fn set_running(task: *mut PyObject) {
    let prev = RUNNING.with(|c| c.replace(task as usize));
    if prev == task as usize {
        return;
    }
    with_registry(|r| unsafe {
        if prev != 0 {
            if let Some(&n) = r.nodes.get(&prev) {
                (*(n as *mut TaskNode)).running = 0;
            }
        }
        if !task.is_null() {
            if let Some(&n) = r.nodes.get(&(task as usize)) {
                (*(n as *mut TaskNode)).running = 1;
            }
        }
    });
}

/// A forked child keeps only the tasks of the thread that forked (the others' threads
/// are gone); called from the child.
pub fn after_fork_child() {
    RUNNING.with(|c| c.set(0));
}

fn slot_offset(descr: &PyObject) -> PyResult<u64> {
    if !descr.is_type(crate::types::descr::member_descr_type()) {
        crate::raise!(TypeError, "expected a member descriptor");
    }
    unsafe {
        let m = (*(descr.as_ptr() as *mut PyMemberDescrObject)).d_member;
        Ok((*m).offset as u64)
    }
}

// Python-callable hooks installed into `_asyncio` (module-level builtins).

fn py_link(_m: &PyObject, task: &PyObject) -> PyResult {
    link_task(task.as_ptr());
    Ok(crate::types::none::none_ref())
}
fn py_unlink(_m: &PyObject, task: &PyObject) -> PyResult {
    unlink_task(task.as_ptr());
    Ok(crate::types::none::none_ref())
}
fn py_running(_m: &PyObject, task: &PyObject) -> PyResult {
    let p = if task.is(crate::types::none::none()) { std::ptr::null_mut() } else { task.as_ptr() };
    set_running(p);
    Ok(crate::types::none::none_ref())
}
/// `_rd_layout(task_type, name_slot, coro_slot, awaited_by_slot, state_slot, await_code)`
fn py_layout(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_rd_layout", args, 6, 6)?;
    let a = &RPY_DEBUG_TABLE.asyncio;
    a.task_type.store(args[0].as_ptr() as u64, Relaxed);
    a.task_name.store(slot_offset(args[1])?, Relaxed);
    a.task_coro.store(slot_offset(args[2])?, Relaxed);
    a.fut_awaited_by.store(slot_offset(args[3])?, Relaxed);
    a.fut_state.store(slot_offset(args[4])?, Relaxed);
    a.future_await_code.store(args[5].as_ptr() as u64, Relaxed);
    Ok(crate::types::none::none_ref())
}

/// Add the registry hooks to `_asyncio`'s namespace (before its source runs).
pub fn install_asyncio_hooks(d: &PyObject) -> PyResult<()> {
    let methods = crate::mdefs![
        ("_rd_link", O, py_link),
        ("_rd_unlink", O, py_unlink),
        ("_rd_running", O, py_running),
        ("_rd_layout", FAST, py_layout),
    ];
    let (m, md) = crate::modules::fncommon::new_module("_asyncio._rd", methods, None)?;
    let _ = m;
    for name in ["_rd_link", "_rd_unlink", "_rd_running", "_rd_layout"] {
        if let Some(f) = crate::types::dict::get_item_str(&md, &crate::types::str::new_str(name)) {
            crate::types::dict::set_item_str(d, name, &f)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ remote exec

/// Safe-point handler for `EVAL_REMOTE_EXEC`: run the queued script, if any.
pub fn run_pending(ts: &ThreadState) -> PyResult<()> {
    ts.c.eval_breaker.fetch_and(!EVAL_REMOTE_EXEC, AcqRel);
    let area: &RemoteExecArea = &REMOTE_EXEC;
    if area.pending.swap(0, AcqRel) == 0 {
        return Ok(());
    }
    if !super::offsets::remote_exec_allowed() {
        return Ok(());
    }
    let raw: Vec<u8> = unsafe {
        let buf = &*area.path.get();
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf[..n].to_vec()
    };
    // a failing script must not disturb the code that was interrupted: its error is
    // reported as unraisable and the interrupted code continues
    let saved = crate::errors::take_exc();
    if run_script(&raw).is_err() {
        crate::modules::sys_full::write_unraisable(Some("Error executing debugger script"), None);
    }
    if let Some(s) = saved {
        crate::errors::set_exc(s);
    }
    Ok(())
}

fn run_script(raw: &[u8]) -> PyResult<()> {
    let path = crate::modules::posix::path::fs_decode(raw)?;
    crate::modules::sys_full::audit("cpython.remote_debugger_script", &[&path])?;
    let io = crate::import::import_absolute("io")?;
    let open = crate::abstract_::getattr_str(&io, "open_code")?;
    let f = crate::abstract_::call(&open, &[&path])?;
    let read = crate::abstract_::getattr_str(&f, "read")?;
    let src = crate::abstract_::call(&read, &[]);
    let close = crate::abstract_::getattr_str(&f, "close")?;
    let _ = crate::abstract_::call(&close, &[]);
    let src = src?;
    let builtins = crate::builtins::builtins_module();
    let compile = crate::abstract_::getattr_str(&builtins, "compile")?;
    let mode = crate::types::str::new_str("exec");
    let code = crate::abstract_::call(&compile, &[&src, &path, &mode])?;
    let g = crate::types::dict::new_dict();
    crate::types::dict::set_item_str(&g, "__name__", &crate::types::str::new_str("__remote_debugger__"))?;
    crate::types::dict::set_item_str(&g, "__builtins__", &builtins)?;
    crate::types::dict::set_item_str(&g, "__file__", &path)?;
    crate::interp::eval_code(&code, &g, Some(&g))?;
    Ok(())
}
