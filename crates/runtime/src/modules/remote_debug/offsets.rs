//! (remote1) The debug table: what an out-of-process reader (`_remote_debugging`,
//! `sys.remote_exec`, profilers) needs to know to make sense of this runtime's memory.
//!
//! Our frames, thread states and code objects are Rust structures whose layout the
//! compiler chooses, so nothing outside this binary can know where a field lives. The
//! runtime therefore publishes one table, `RPY_DEBUG_TABLE`, in its own ELF section
//! (`.rpy_debug`). A reader opens the target's executable (`/proc/PID/exe`), finds the
//! section's link-time address in the section headers, adds the load bias seen in
//! `/proc/PID/maps` and copies the table out with `process_vm_readv`. Everything else is
//! reached from there:
//!
//! * addresses of the runtime's globals (the thread-state mirror kept by `qsbr`, the
//!   main thread state, the remote-exec mailbox, the asyncio task registry, the type
//!   objects a reader compares against, the set "dummy" key);
//! * field offsets of every structure a reader walks, computed with `offset_of!` so they
//!   follow the real definitions (`tests::table_matches_layouts` re-checks them);
//! * a few values only known at run time (how `Vec` lays out its pointer and length,
//!   whether remote debugging is allowed, the slot offsets of the asyncio task
//!   attributes), filled in by `init_table` / `target::asyncio_layout`.
//!
//! The table starts with a magic string, a format version and its own size; a reader
//! refuses a table whose version or size differs from what it understands.

use crate::ffi::*;
use crate::interp::frame::Frame;
use crate::threads::{CThreadState, ErrStackItem, ThreadState};
use crate::types::code::{CodeData, CodeMeta, PyCodeObject};
use crate::types::genobject::GenObject;
use std::mem::offset_of;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};

pub const MAGIC: [u8; 8] = *b"RPyDbgT\0";
/// Bumped whenever the layout of `DebugTable` changes.
pub const VERSION: u32 = 4;

/// Offsets of the pointer and length words inside a `Vec<T>` (std does not promise an
/// order, so `init_table` measures it).
#[repr(C)]
pub struct VecLayout {
    pub ptr: AtomicU64,
    pub len: AtomicU64,
}

#[repr(C)]
pub struct ThreadOffsets {
    pub size: u64,
    pub native_thread_id: u64,
    pub pthread_id: u64,
    pub interp: u64,
    pub eval_breaker: u64,
    pub exc_info: u64,
    pub current_exception: u64,
    /// `Cell<*mut Frame>`: innermost interpreter frame.
    pub frame: u64,
    /// `ThreadMode` byte: 0 running Python (attached), 1 in native code, 2 blocked.
    pub mode: u64,
    pub alive: u64,
    pub exited: u64,
}

#[repr(C)]
pub struct ErrStackOffsets {
    pub exc_value: u64,
    pub previous_item: u64,
}

#[repr(C)]
pub struct FrameOffsets {
    pub size: u64,
    pub code: u64,
    pub prev: u64,
    /// u32: internal instruction index currently executing.
    pub lasti: u64,
    pub next_instr: u64,
    /// u8, see `FrameState`.
    pub state: u64,
    /// `Vec<*mut PyObject>`: localsplus followed by the value stack.
    pub slots: u64,
    pub stack_top: u64,
    pub nlocalsplus: u64,
    /// i32: index of the Send instruction while suspended in `yield from`/`await`, -1.
    pub yield_from_send: u64,
    pub generator: u64,
    /// Values of the `state` byte.
    pub state_created: u8,
    pub state_executing: u8,
    pub state_suspended: u8,
    pub state_completed: u8,
    pub state_cleared: u8,
    pub _pad: [u8; 3],
}

#[repr(C)]
pub struct CodeOffsets {
    pub filename: u64,
    pub name: u64,
    pub qualname: u64,
    pub linetable: u64,
    pub firstlineno: u64,
    /// `AtomicPtr<CodeData>` (NULL until the code ran once).
    pub data: u64,
    pub meta: u64,
    /// In `CodeData`: `Vec<u32>` public code-unit offset per internal instruction.
    pub data_pub_of_internal: u64,
    /// In `CodeMeta`: the co_code bytes object.
    pub meta_co_code: u64,
}

#[repr(C)]
pub struct ObjectOffsets {
    pub ob_type: u64,
    pub var_size: u64,
    /// str (PEP 393): `length`, `state` word, inline data of compact ASCII / compact
    /// strings, data pointer of the other ones.
    pub str_length: u64,
    pub str_state: u64,
    pub str_ascii_data: u64,
    pub str_compact_data: u64,
    pub str_data_ptr: u64,
    /// int: tag word and first digit (30-bit digits).
    pub long_tag: u64,
    pub long_digits: u64,
    pub bytes_data: u64,
    pub tuple_items: u64,
    /// set: mask, table pointer, entry size and key offset.
    pub set_mask: u64,
    pub set_table: u64,
    pub set_entry_size: u64,
    /// generators/coroutines: their frame.
    pub gen_frame: u64,
    /// async generator `asend()`/`athrow()` awaitables: the generator.
    pub agen_awaitable_gen: u64,
    /// `PyTypeObject.tp_name` / `tp_flags`
    pub type_name: u64,
    pub type_flags: u64,
    /// subinterpreters: `Interp.id` (a NULL `interp` pointer is the main interpreter, id 0)
    pub interp_id: u64,
}

/// Where the asyncio task attributes live inside a Task object (slot offsets of the
/// `_asyncio.Task` class, 0 until `_asyncio` was imported), and the code object of
/// `Future.__await__` (a coroutine awaiting a future delegates to a generator running
/// it; a reader stops there instead of reporting that generator's frame).
#[repr(C)]
pub struct AsyncioOffsets {
    pub task_name: AtomicU64,
    pub task_coro: AtomicU64,
    pub fut_awaited_by: AtomicU64,
    pub fut_state: AtomicU64,
    pub future_await_code: AtomicU64,
    pub task_type: AtomicU64,
}

/// Fixed-size mailbox for `sys.remote_exec`: a debugger writes the script path
/// (NUL-terminated), then sets `pending`, then raises `EVAL_REMOTE_EXEC` in the main
/// thread's eval breaker.
#[repr(C)]
pub struct RemoteExecArea {
    pub pending: AtomicU32,
    pub _pad: u32,
    pub path: std::cell::UnsafeCell<[u8; REMOTE_PATH_MAX]>,
}
pub const REMOTE_PATH_MAX: usize = 4096;
unsafe impl Sync for RemoteExecArea {}

/// One registered asyncio task (see target.rs): doubly linked, nodes are recycled and
/// never returned to the allocator so that a racing reader only sees stale data.
#[repr(C)]
pub struct TaskNode {
    pub task: *mut PyObject,
    /// thread state of the thread the task was created on
    pub ts: *mut ThreadState,
    pub prev: *mut TaskNode,
    pub next: *mut TaskNode,
    /// 1 while the task is running a step
    pub running: u64,
}

#[repr(C)]
pub struct DebugTable {
    pub magic: [u8; 8],
    pub version: u32,
    pub size: u32,
    /// 1 once `init_table` ran (the run-time values below are valid).
    pub ready: AtomicU32,
    /// bit 0: remote debugging allowed (`sys.remote_exec` targets, `-X disable-remote-debug`)
    pub flags: AtomicU32,
    pub vec: VecLayout,
    /// The thread-state mirror of qsbr.rs: `thread_list_cap` slots, the live count and
    /// a sequence word that is odd while the mirror is rewritten.
    pub thread_list: *const u8,
    pub thread_list_len: *const u8,
    pub thread_list_seq: *const u8,
    pub thread_list_cap: u64,
    /// `AtomicPtr<ThreadState>` of the main thread.
    pub main_thread: *const u8,
    /// (tail4) `AtomicUsize`: the thread state running a garbage collection, or 0.
    pub gc_collector: *const u8,
    /// Eval-breaker bit a debugger sets for `sys.remote_exec`; it lives alone in its
    /// byte (`remote_exec_byte` inside the word) so a one-byte write cannot clobber
    /// the bits the runtime updates atomically.
    pub eval_remote_exec_bit: u64,
    pub remote_exec_byte: u64,
    pub remote_exec: *const RemoteExecArea,
    /// `*mut TaskNode` head of the asyncio task list.
    pub task_list_head: *const u8,
    pub set_dummy: *const u8,
    pub code_type: *const u8,
    pub gen_type: *const u8,
    pub coro_type: *const u8,
    pub asyncgen_type: *const u8,
    pub asend_type: *const u8,
    pub athrow_type: *const u8,
    pub str_type: *const u8,
    pub long_type: *const u8,
    pub none: *const u8,
    pub thread: ThreadOffsets,
    pub errstack: ErrStackOffsets,
    pub frame: FrameOffsets,
    pub code: CodeOffsets,
    pub obj: ObjectOffsets,
    pub asyncio: AsyncioOffsets,
    pub task_node_size: u64,
}
unsafe impl Sync for DebugTable {}

/// Eval-breaker bit for a pending `sys.remote_exec` script (the top byte of the word is
/// used by nothing else).
pub const EVAL_REMOTE_EXEC: usize = 1 << 56;

pub static REMOTE_EXEC: RemoteExecArea =
    RemoteExecArea { pending: AtomicU32::new(0), _pad: 0, path: std::cell::UnsafeCell::new([0; REMOTE_PATH_MAX]) };

const fn off(v: usize) -> u64 {
    v as u64
}

#[unsafe(link_section = ".rpy_debug")]
#[unsafe(no_mangle)]
#[used]
pub static RPY_DEBUG_TABLE: DebugTable = DebugTable {
    magic: MAGIC,
    version: VERSION,
    size: std::mem::size_of::<DebugTable>() as u32,
    ready: AtomicU32::new(0),
    flags: AtomicU32::new(0),
    vec: VecLayout { ptr: AtomicU64::new(0), len: AtomicU64::new(0) },
    thread_list: &crate::qsbr::LIST as *const _ as *const u8,
    thread_list_len: &crate::qsbr::LIST_LEN as *const _ as *const u8,
    thread_list_seq: &crate::qsbr::LIST_VER as *const _ as *const u8,
    thread_list_cap: crate::qsbr::CAP as u64,
    main_thread: &crate::threads::MAIN_THREAD_STATE as *const _ as *const u8,
    gc_collector: &crate::gc::GC_COLLECTOR_TS as *const _ as *const u8,
    eval_remote_exec_bit: EVAL_REMOTE_EXEC as u64,
    remote_exec_byte: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker) + 7),
    remote_exec: &REMOTE_EXEC,
    task_list_head: &super::target::TASK_HEAD as *const _ as *const u8,
    set_dummy: &crate::types::set::DUMMY_CELL as *const u64 as *const u8,
    code_type: &raw const crate::types::code::PyCode_Type as *const u8,
    gen_type: &raw const crate::types::genobject::PyGen_Type as *const u8,
    coro_type: &raw const crate::types::genobject::PyCoro_Type as *const u8,
    asyncgen_type: &raw const crate::types::genobject::PyAsyncGen_Type as *const u8,
    asend_type: &raw const crate::types::asyncgen::ASEND_TYPE as *const u8,
    athrow_type: &raw const crate::types::asyncgen::ATHROW_TYPE as *const u8,
    str_type: &raw const crate::types::str::PyUnicode_Type as *const u8,
    long_type: &raw const crate::types::int::PyLong_Type as *const u8,
    none: &raw const crate::types::none::_Py_NoneStruct as *const u8,
    thread: ThreadOffsets {
        size: off(std::mem::size_of::<ThreadState>()),
        native_thread_id: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, native_thread_id)),
        pthread_id: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, thread_id)),
        interp: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, interp)),
        eval_breaker: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker)),
        exc_info: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, exc_info)),
        current_exception: off(offset_of!(ThreadState, c) + offset_of!(CThreadState, current_exception)),
        frame: off(offset_of!(ThreadState, frame)),
        mode: off(offset_of!(ThreadState, mode)),
        alive: off(offset_of!(ThreadState, alive)),
        exited: off(offset_of!(ThreadState, exited)),
    },
    errstack: ErrStackOffsets {
        exc_value: off(offset_of!(ErrStackItem, exc_value)),
        previous_item: off(offset_of!(ErrStackItem, previous_item)),
    },
    frame: FrameOffsets {
        size: off(std::mem::size_of::<Frame>()),
        code: off(offset_of!(Frame, code)),
        prev: off(offset_of!(Frame, prev)),
        lasti: off(offset_of!(Frame, lasti)),
        next_instr: off(offset_of!(Frame, next_instr)),
        state: off(offset_of!(Frame, state)),
        slots: off(offset_of!(Frame, slots)),
        stack_top: off(offset_of!(Frame, stack_top)),
        nlocalsplus: off(offset_of!(Frame, nlocalsplus)),
        yield_from_send: off(offset_of!(Frame, yield_from_send)),
        generator: off(offset_of!(Frame, generator)),
        state_created: crate::interp::frame::FrameState::Created as u8,
        state_executing: crate::interp::frame::FrameState::Executing as u8,
        state_suspended: crate::interp::frame::FrameState::Suspended as u8,
        state_completed: crate::interp::frame::FrameState::Completed as u8,
        state_cleared: crate::interp::frame::FrameState::Cleared as u8,
        _pad: [0; 3],
    },
    code: CodeOffsets {
        filename: off(offset_of!(PyCodeObject, co_filename)),
        name: off(offset_of!(PyCodeObject, co_name)),
        qualname: off(offset_of!(PyCodeObject, co_qualname)),
        linetable: off(offset_of!(PyCodeObject, co_linetable)),
        firstlineno: off(offset_of!(PyCodeObject, co_firstlineno)),
        data: off(offset_of!(PyCodeObject, data)),
        meta: off(offset_of!(PyCodeObject, meta)),
        data_pub_of_internal: off(offset_of!(CodeData, pub_of_internal)),
        meta_co_code: off(offset_of!(CodeMeta, co_code)),
    },
    obj: ObjectOffsets {
        ob_type: off(offset_of!(PyObject, ob_type)),
        var_size: off(offset_of!(PyVarObject, ob_size)),
        str_length: off(offset_of!(PyASCIIObject, length)),
        str_state: off(offset_of!(PyASCIIObject, state)),
        str_ascii_data: off(std::mem::size_of::<PyASCIIObject>()),
        str_compact_data: off(std::mem::size_of::<PyCompactUnicodeObject>()),
        str_data_ptr: off(offset_of!(PyUnicodeObject, data)),
        long_tag: off(offset_of!(PyLongObject, lv_tag)),
        long_digits: off(offset_of!(PyLongObject, ob_digit)),
        bytes_data: off(offset_of!(PyBytesObject, ob_sval)),
        tuple_items: off(offset_of!(PyTupleObject, ob_item)),
        set_mask: off(offset_of!(crate::types::set::PySetObject, mask)),
        set_table: off(offset_of!(crate::types::set::PySetObject, table)),
        set_entry_size: off(std::mem::size_of::<crate::types::set::SetEntry>()),
        gen_frame: off(offset_of!(GenObject, frame)),
        agen_awaitable_gen: off(crate::types::asyncgen::AWAITABLE_GEN_OFFSET),
        type_name: off(offset_of!(PyTypeObject, tp_name)),
        type_flags: off(offset_of!(PyTypeObject, tp_flags)),
        interp_id: off(offset_of!(crate::subinterp::Interp, id)),
    },
    asyncio: AsyncioOffsets {
        task_name: AtomicU64::new(0),
        task_coro: AtomicU64::new(0),
        fut_awaited_by: AtomicU64::new(0),
        fut_state: AtomicU64::new(0),
        future_await_code: AtomicU64::new(0),
        task_type: AtomicU64::new(0),
    },
    task_node_size: off(std::mem::size_of::<TaskNode>()),
};

/// Measure `Vec`'s field order: a vector with distinct capacity and length shows which
/// word holds what.
fn probe_vec_layout() -> (u64, u64) {
    let mut v: Vec<u32> = Vec::with_capacity(7);
    v.push(1);
    v.push(2);
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(&v) };
    let ptr = v.as_ptr() as usize;
    let mut p = 0;
    let mut l = 0;
    for (i, w) in words.iter().enumerate() {
        if *w == ptr {
            p = i;
        } else if *w == 2 {
            l = i;
        }
    }
    ((p * 8) as u64, (l * 8) as u64)
}

/// Fill in the run-time part of the table (called once during runtime start-up).
pub fn init_table() {
    let t = &RPY_DEBUG_TABLE;
    let (p, l) = probe_vec_layout();
    t.vec.ptr.store(p, Relaxed);
    t.vec.len.store(l, Relaxed);
    let enabled = crate::modules::sys_full::remote_debug_enabled();
    t.flags.store(if enabled { 1 } else { 0 }, Relaxed);
    t.ready.store(1, Release);
}

/// Is `sys.remote_exec` allowed to target this process?
pub fn remote_exec_allowed() -> bool {
    RPY_DEBUG_TABLE.flags.load(Relaxed) & 1 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_layouts() {
        let t = &RPY_DEBUG_TABLE;
        assert_eq!(t.magic, MAGIC);
        assert_eq!(t.size as usize, std::mem::size_of::<DebugTable>());
        // a frame: the offsets the reader uses land on the fields they name
        let n = std::ptr::null_mut();
        let f = unsafe { Frame::new_call_raw(n, n, n, n, 0, 1) };
        let base = &*f as *const Frame as usize;
        assert_eq!(&raw const f.code as usize - base, t.frame.code as usize);
        assert_eq!(&raw const f.prev as usize - base, t.frame.prev as usize);
        assert_eq!(&raw const f.lasti as usize - base, t.frame.lasti as usize);
        assert_eq!(&raw const f.state as usize - base, t.frame.state as usize);
        assert_eq!(&raw const f.slots as usize - base, t.frame.slots as usize);
        assert_eq!(&raw const f.stack_top as usize - base, t.frame.stack_top as usize);
        assert_eq!(&raw const f.yield_from_send as usize - base, t.frame.yield_from_send as usize);
        assert_eq!(std::mem::size_of_val(&f.lasti), 4);
        assert_eq!(std::mem::size_of_val(&f.state), 1);
        assert_eq!(std::mem::size_of_val(&f.yield_from_send), 4);
        // the measured Vec layout reads a real vector back
        let (p, l) = probe_vec_layout();
        let v: Vec<u32> = vec![5, 6, 7];
        let words: [usize; 3] = unsafe { std::mem::transmute_copy(&v) };
        assert_eq!(words[p as usize / 8], v.as_ptr() as usize);
        assert_eq!(words[l as usize / 8], 3);
        // PEP 393 strings and ints: data offsets follow the real struct sizes
        assert_eq!(t.obj.str_ascii_data, std::mem::size_of::<PyASCIIObject>() as u64);
        assert_eq!(t.obj.str_compact_data, std::mem::size_of::<PyCompactUnicodeObject>() as u64);
        assert_eq!(t.obj.long_tag, std::mem::offset_of!(PyLongObject, lv_tag) as u64);
        // the eval-breaker byte reserved for remote exec is the word's top byte
        assert_eq!(EVAL_REMOTE_EXEC >> ((t.remote_exec_byte - t.thread.eval_breaker) * 8), 1);
        // thread-state fields have the widths the reader assumes
        let ts_size = std::mem::size_of::<ThreadState>();
        assert!(t.thread.mode < ts_size as u64 && t.thread.frame < ts_size as u64);
        assert_eq!(std::mem::size_of::<crate::threads::ModeCell>(), 1);
    }
}
