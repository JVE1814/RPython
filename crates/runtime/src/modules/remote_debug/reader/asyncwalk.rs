//! (remote1) Async task-tree walking for `get_all_awaited_by` / `get_async_stack_trace`.
//! Reads the target-side task registry (a linked list of `TaskNode`) and, for each task,
//! reconstructs its coroutine stack (leaf frame first) and the coroutines awaiting it.

use super::{rd_ss, rd_to_mem_err, rd_us, RdState};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{int, list, none};
use std::collections::HashSet;

const MAX_ASYNC_FRAMES: usize = 1024 + 512;
const MAX_TASK_WAITERS: usize = 1 << 14;
const MAX_CORO_CHAIN: usize = 1024 + 512;

// TaskNode layout (offsets.rs TaskNode): task, ts, prev, next, running (all u64-sized).
const TN_TASK: u64 = 0;
const TN_TS: u64 = 8;
const TN_NEXT: u64 = 24;
const TN_RUNNING: u64 = 32;

struct TaskRec {
    task: u64,
    ts: u64,
    native_id: u64,
    running: bool,
}

fn task_list(st: &RdState) -> PyResult<Vec<TaskRec>> {
    let t = &st.table;
    let mut head = st.proc.u64(t.task_list_head).map_err(rd_to_mem_err)?;
    let mut out = Vec::new();
    let mut guard = 0;
    while head != 0 {
        guard += 1;
        if guard > MAX_TASK_WAITERS {
            raise!(RuntimeError, "Too many task waiters while reading the task list");
        }
        let task = st.proc.u64(head + TN_TASK).map_err(rd_to_mem_err)?;
        let ts = st.proc.u64(head + TN_TS).map_err(rd_to_mem_err)?;
        let running = st.proc.u64(head + TN_RUNNING).map_err(rd_to_mem_err)? != 0;
        if task != 0 {
            let native_id = if ts != 0 { st.proc.u64(ts + t.t_native).map_err(rd_to_mem_err)? } else { 0 };
            out.push(TaskRec { task, ts, native_id, running });
        }
        head = st.proc.u64(head + TN_NEXT).map_err(rd_to_mem_err)?;
    }
    Ok(out)
}

fn ptr(st: &RdState, a: u64) -> PyResult<u64> {
    st.proc.u64(a).map_err(rd_to_mem_err)
}

/// The value the frame is delegating to via `yield from`/`await` (on its value stack
/// just below the top), when suspended in a Send loop.
fn frame_delegate(st: &RdState, frame: u64) -> PyResult<u64> {
    let t = &st.table;
    let yfs = st.proc.u32(frame + t.f_yield_from_send).map_err(rd_to_mem_err)? as i32;
    if yfs < 0 {
        return Ok(0);
    }
    let stack_top = ptr(st, frame + t.f_stack_top)? as usize;
    let nlp = ptr(st, frame + t.f_nlocalsplus)? as usize;
    if stack_top <= nlp {
        return Ok(0);
    }
    // slots is a Vec<*mut PyObject>; its data pointer is at frame + f_slots + vec.ptr
    let slots = ptr(st, frame + t.f_slots + t.vec_ptr)?;
    if slots == 0 {
        return Ok(0);
    }
    ptr(st, slots + ((stack_top - 1) as u64) * 8)
}

/// Does `obj` have one of the generator/coroutine/asend/athrow types (something whose
/// frame continues the chain)?
fn gen_like_frame(st: &RdState, obj: u64) -> PyResult<u64> {
    let t = &st.table;
    if obj == 0 {
        return Ok(0);
    }
    let tp = st.type_of(obj)?;
    if tp == t.coro_type || tp == t.gen_type || tp == t.asyncgen_type {
        return ptr(st, obj + t.o_gen_frame);
    }
    if tp == t.asend_type || tp == t.athrow_type {
        // an asend/athrow object wraps an async generator
        let agen = ptr(st, obj + t.o_agen_gen)?;
        if agen != 0 {
            return ptr(st, agen + t.o_gen_frame);
        }
    }
    Ok(0)
}

/// Collect a coroutine's call stack (leaf first) by following delegation from `coro`'s
/// frame. For a running task we instead segment the live thread stack.
fn coro_call_stack(st: &RdState, task: &TaskRec, coro: u64) -> PyResult<Vec<PyRef>> {
    let t = &st.table;
    // the task's awaitable may be a coroutine/generator (frame directly) or an async
    // generator asend/athrow wrapper (frame via the wrapped async generator)
    let task_coro_frame = if coro != 0 { gen_like_frame(st, coro)? } else { 0 };

    // frames implementing `Future.__await__` are the machinery a coroutine uses to wait on
    // a future; they are not part of the reported coroutine stack
    let skip = |st: &RdState, frame: u64| -> PyResult<bool> {
        if t.a_await_code == 0 {
            return Ok(false);
        }
        Ok(ptr(st, frame + t.f_code)? == t.a_await_code)
    };

    if task.running && task.ts != 0 && task_coro_frame != 0 {
        // the whole chain is live on the thread stack: collect from the innermost frame
        // up to and including the task coroutine's own frame
        let mut top = ptr(st, task.ts + t.t_frame)?;
        let mut frames = Vec::new();
        let mut guard = 0;
        let mut reached = false;
        while top != 0 {
            guard += 1;
            if guard > MAX_ASYNC_FRAMES {
                raise!(RuntimeError, "Too many async stack frames while unwinding");
            }
            let this = top;
            if !skip(st, this)? {
                if let Some(fi) = super::read_frame_pub(st, this)? {
                    frames.push(fi);
                }
            }
            if this == task_coro_frame {
                reached = true;
                break;
            }
            top = ptr(st, this + t.f_prev)?;
        }
        if reached {
            return Ok(frames);
        }
        // fall through to delegation if the coro frame was not on this thread stack
    }

    // suspended: follow delegation, prepending deeper frames so the leaf ends up first
    let mut chain: Vec<PyRef> = Vec::new();
    let mut frame = task_coro_frame;
    let mut guard = 0;
    while frame != 0 {
        guard += 1;
        if guard > MAX_CORO_CHAIN {
            raise!(RuntimeError, "Too many coroutine frames while unwinding");
        }
        if !skip(st, frame)? {
            if let Some(fi) = super::read_frame_pub(st, frame)? {
                chain.insert(0, fi);
            }
        }
        let deleg = frame_delegate(st, frame)?;
        if deleg == 0 {
            break;
        }
        frame = gen_like_frame(st, deleg)?;
    }
    Ok(chain)
}

fn read_set_members(st: &RdState, set_obj: u64) -> PyResult<Vec<u64>> {
    let t = &st.table;
    if set_obj == 0 {
        return Ok(Vec::new());
    }
    let mask = st.proc.u64(set_obj + t.o_set_mask).map_err(rd_to_mem_err)? as usize;
    let table = st.proc.u64(set_obj + t.o_set_table).map_err(rd_to_mem_err)?;
    if table == 0 || mask == 0 || mask > (1 << 24) {
        return Ok(Vec::new());
    }
    let entry_size = t.o_set_entry_size;
    let mut out = Vec::new();
    for i in 0..=mask {
        let key = st.proc.u64(table + (i as u64) * entry_size).map_err(rd_to_mem_err)?;
        if key != 0 && key != t.set_dummy {
            out.push(key);
        }
    }
    Ok(out)
}

fn task_name(st: &RdState, task: u64) -> PyResult<PyRef> {
    let t = &st.table;
    if t.a_task_name == 0 {
        return Ok(none::none_ref());
    }
    let nm = ptr(st, task + t.a_task_name)?;
    if nm == 0 || nm == t.none {
        return Ok(none::none_ref());
    }
    Ok(crate::types::str::new_str(&st.read_str_pub(nm, 255)?))
}

/// A task counts for the async view only while it is pending (CPython drops finished and
/// cancelled tasks from the reported set).
fn task_pending(st: &RdState, task: u64) -> PyResult<bool> {
    let t = &st.table;
    if t.a_fut_state == 0 {
        return Ok(true);
    }
    let s = ptr(st, task + t.a_fut_state)?;
    if s == 0 || s == t.none {
        return Ok(true);
    }
    Ok(st.read_str_pub(s, 16)? == "PENDING")
}

fn task_coro(st: &RdState, task: u64) -> PyResult<u64> {
    let t = &st.table;
    if t.a_task_coro == 0 {
        return Ok(0);
    }
    let c = ptr(st, task + t.a_task_coro)?;
    if c == t.none { Ok(0) } else { Ok(c) }
}

/// Build the TaskInfo for one task.
fn task_info(st: &RdState, recs: &[TaskRec], task: u64, depth: usize) -> PyResult<PyRef> {
    let t = &st.table;
    // find the record for this task (for running / thread)
    let default = TaskRec { task, ts: 0, native_id: 0, running: false };
    let rec = recs.iter().find(|r| r.task == task).unwrap_or(&default);
    let name = task_name(st, task)?;
    let coro = task_coro(st, task)?;
    let call_stack = coro_call_stack(st, rec, coro)?;
    let coro_info = rd_ss(super::coro_info_type(), vec![list::new_list(call_stack), int::new_int(task as i64)]);
    let coroutine_stack = list::new_list(vec![coro_info]);

    // awaited_by: the waiters in this task's _fut_awaited_by set
    let mut awaited = Vec::new();
    if t.a_awaited_by != 0 && depth < 64 {
        let set_obj = ptr(st, task + t.a_awaited_by)?;
        let waiters = read_set_members(st, set_obj)?;
        for w in waiters {
            // the waiter is itself a task/future; its own coroutine stack leads to the await
            let wrec_default = TaskRec { task: w, ts: 0, native_id: 0, running: false };
            let wrec = recs.iter().find(|r| r.task == w).unwrap_or(&wrec_default);
            let wcoro = task_coro(st, w)?;
            let wstack = coro_call_stack(st, wrec, wcoro)?;
            let ci = rd_ss(super::coro_info_type(), vec![list::new_list(wstack), int::new_int(w as i64)]);
            awaited.push(ci);
        }
    }
    Ok(rd_ss(super::task_info_type(), vec![int::new_int(task as i64), name, coroutine_stack, list::new_list(awaited)]))
}

pub(crate) fn get_all_awaited_by(o: &PyObject) -> PyResult {
    let st = rd_us(o);
    if !st.proc.alive() {
        raise!(ProcessLookupError, "process {} is not running", st.proc.pid);
    }
    let recs = task_list(st)?;
    // group tasks by their thread's native id
    let mut by_thread: Vec<(u64, Vec<PyRef>)> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    for r in &recs {
        if !seen.insert(r.task) {
            continue;
        }
        if !task_pending(st, r.task)? {
            continue;
        }
        let ti = task_info(st, &recs, r.task, 0)?;
        match by_thread.iter_mut().find(|(tid, _)| *tid == r.native_id) {
            Some((_, v)) => v.push(ti),
            None => by_thread.push((r.native_id, vec![ti])),
        }
    }
    let mut out: Vec<PyRef> = by_thread
        .into_iter()
        .map(|(tid, tasks)| rd_ss(super::awaited_info_type(), vec![int::new_int(tid as i64), list::new_list(tasks)]))
        .collect();
    // the per-interpreter fallback list (tasks not attached to a thread): empty here
    out.push(rd_ss(super::awaited_info_type(), vec![int::new_int(0), list::new_list(vec![])]));
    Ok(list::new_list(out))
}

pub(crate) fn get_async_stack_trace(o: &PyObject) -> PyResult {
    // Same shape as get_all_awaited_by for our purposes: tests read
    // stack_trace[0].awaited_by.
    get_all_awaited_by(o)
}
