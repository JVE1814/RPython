//! (remote1) `_remote_debugging`: inspect another process (or our own) through its debug
//! table and `process_vm_readv`. Implements `RemoteUnwinder` and the module's helper
//! functions and struct-sequence result types.

use super::procmem::*;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

// PEP 393 string state bits (our ABI == CPython's).
const STR_KIND_SHIFT: u32 = 8;
const STR_COMPACT: u32 = 1 << 11;
const STR_ASCII: u32 = 1 << 12;
const ASCII_SIZE: u64 = 40; // sizeof(PyASCIIObject)
const COMPACT_SIZE: u64 = 56; // sizeof(PyCompactUnicodeObject)

const MAX_NAME: usize = 1024;
const MAX_LINETABLE: usize = 64 * 1024;
const MAX_FRAME_CHAIN: usize = 1024 + 512;
const MAX_TASK_WAITERS: usize = 1 << 14;

// ---------------------------------------------------------------- the parsed table

/// The debug table, copied out of the target and decoded (see offsets.rs for the
/// layout). All addresses are in the target's address space.
#[derive(Clone)]
pub struct Table {
    pub addr: u64,
    pub flags: u32,
    pub remote_exec: u64,
    pub remote_exec_byte: u64,
    pub vec_ptr: u64,
    pub vec_len: u64,
    pub thread_list: u64,
    pub thread_list_len: u64,
    pub thread_list_seq: u64,
    pub thread_list_cap: u64,
    pub main_thread: u64,
    pub gc_collector: u64,
    pub gc_stats: u64,
    pub gc_stat_slots: u64,
    pub gc_stat_words: u64,
    pub task_list_head: u64,
    pub set_dummy: u64,
    pub code_type: u64,
    pub gen_type: u64,
    pub coro_type: u64,
    pub asyncgen_type: u64,
    pub asend_type: u64,
    pub athrow_type: u64,
    pub none: u64,
    // thread offsets
    pub t_native: u64,
    pub t_interp: u64,
    pub t_eval_breaker: u64,
    pub t_exc_info: u64,
    pub t_current_exc: u64,
    pub t_frame: u64,
    pub t_mode: u64,
    pub t_alive: u64,
    pub t_exited: u64,
    // errstack
    pub e_value: u64,
    pub e_prev: u64,
    // frame offsets
    pub f_code: u64,
    pub f_prev: u64,
    pub f_lasti: u64,
    pub f_state: u64,
    pub f_slots: u64,
    pub f_stack_top: u64,
    pub f_nlocalsplus: u64,
    pub f_yield_from_send: u64,
    pub f_generator: u64,
    pub st_suspended: u8,
    // code offsets
    pub c_filename: u64,
    pub c_name: u64,
    pub c_qualname: u64,
    pub c_linetable: u64,
    pub c_firstlineno: u64,
    pub c_data: u64,
    pub c_meta: u64,
    pub c_pub_of_internal: u64,
    pub c_meta_co_code: u64,
    // object offsets
    pub o_ob_type: u64,
    pub o_var_size: u64,
    pub o_str_length: u64,
    pub o_str_state: u64,
    pub o_str_ascii_data: u64,
    pub o_str_compact_data: u64,
    pub o_str_data_ptr: u64,
    pub o_bytes_data: u64,
    pub o_tuple_items: u64,
    pub o_set_mask: u64,
    pub o_set_table: u64,
    pub o_set_entry_size: u64,
    pub o_gen_frame: u64,
    pub o_agen_gen: u64,
    pub o_type_name: u64,
    pub o_interp_id: u64,
    // asyncio (0 until _asyncio imported)
    pub a_task_name: u64,
    pub a_task_coro: u64,
    pub a_awaited_by: u64,
    pub a_fut_state: u64,
    pub a_await_code: u64,
    pub a_task_type: u64,
}

const SEC: &str = ".rpy_debug";

fn to_mem_err(e: MemError) -> PyErr {
    match e {
        MemError::NoProcess => err!(ProcessLookupError, "process is gone"),
        MemError::Permission(m) => err!(PermissionError, "{}", m),
        MemError::Fault(m) => err!(RuntimeError, "{}", m),
        MemError::Os(e) => err!(RuntimeError, "{}", e),
    }
}

impl Table {
    fn load(p: &Process) -> PyResult<Table> {
        let (addr, size) = match find_section(p.pid, SEC).map_err(to_mem_err)? {
            Some(x) => x,
            None => raise!(RuntimeError, "could not find the debug section in the target process"),
        };
        if size < 64 {
            raise!(RuntimeError, "debug section is too small");
        }
        let buf = p.read_vec(addr, size as usize).map_err(to_mem_err)?;
        if &buf[0..8] != &super::offsets::MAGIC {
            raise!(RuntimeError, "the target process is not a {} process", "compatible Python");
        }
        let version = rd_u32(&buf, 8);
        if version != super::offsets::VERSION {
            raise!(RuntimeError, "unsupported debug table version {}", version);
        }
        let ready = rd_u32(&buf, 16);
        if ready == 0 {
            raise!(RuntimeError, "the target process has not finished initialising");
        }
        // field accessors by struct offset within the copied table
        macro_rules! u64at {
            ($off:expr) => {
                rd_u64(&buf, $off)
            };
        }
        // The offsets below mirror the DebugTable struct order; keep in sync with
        // offsets.rs. We locate fields by reading the sub-structs' absolute offsets
        // from the table's own declared field offsets is impractical, so we recompute
        // them from the live table definition.
        let t = &super::offsets::RPY_DEBUG_TABLE;
        let base = t as *const _ as usize;
        macro_rules! fo {
            ($field:expr) => {
                (&$field as *const _ as usize - base) as u64
            };
        }
        let table = Table {
            addr,
            flags: rd_u32(&buf, fo!(t.flags)),
            remote_exec: u64at!(fo!(t.remote_exec)),
            remote_exec_byte: u64at!(fo!(t.remote_exec_byte)),
            vec_ptr: u64at!(fo!(t.vec.ptr)),
            vec_len: u64at!(fo!(t.vec.len)),
            thread_list: u64at!(fo!(t.thread_list)),
            thread_list_len: u64at!(fo!(t.thread_list_len)),
            thread_list_seq: u64at!(fo!(t.thread_list_seq)),
            thread_list_cap: u64at!(fo!(t.thread_list_cap)),
            main_thread: u64at!(fo!(t.main_thread)),
            gc_collector: u64at!(fo!(t.gc_collector)),
            gc_stats: u64at!(fo!(t.gc_stats)),
            gc_stat_slots: u64at!(fo!(t.gc_stat_slots)),
            gc_stat_words: u64at!(fo!(t.gc_stat_words)),
            task_list_head: u64at!(fo!(t.task_list_head)),
            set_dummy: u64at!(fo!(t.set_dummy)),
            code_type: u64at!(fo!(t.code_type)),
            gen_type: u64at!(fo!(t.gen_type)),
            coro_type: u64at!(fo!(t.coro_type)),
            asyncgen_type: u64at!(fo!(t.asyncgen_type)),
            asend_type: u64at!(fo!(t.asend_type)),
            athrow_type: u64at!(fo!(t.athrow_type)),
            none: u64at!(fo!(t.none)),
            t_native: u64at!(fo!(t.thread.native_thread_id)),
            t_interp: u64at!(fo!(t.thread.interp)),
            t_eval_breaker: u64at!(fo!(t.thread.eval_breaker)),
            t_exc_info: u64at!(fo!(t.thread.exc_info)),
            t_current_exc: u64at!(fo!(t.thread.current_exception)),
            t_frame: u64at!(fo!(t.thread.frame)),
            t_mode: u64at!(fo!(t.thread.mode)),
            t_alive: u64at!(fo!(t.thread.alive)),
            t_exited: u64at!(fo!(t.thread.exited)),
            e_value: u64at!(fo!(t.errstack.exc_value)),
            e_prev: u64at!(fo!(t.errstack.previous_item)),
            f_code: u64at!(fo!(t.frame.code)),
            f_prev: u64at!(fo!(t.frame.prev)),
            f_lasti: u64at!(fo!(t.frame.lasti)),
            f_state: u64at!(fo!(t.frame.state)),
            f_slots: u64at!(fo!(t.frame.slots)),
            f_stack_top: u64at!(fo!(t.frame.stack_top)),
            f_nlocalsplus: u64at!(fo!(t.frame.nlocalsplus)),
            f_yield_from_send: u64at!(fo!(t.frame.yield_from_send)),
            f_generator: u64at!(fo!(t.frame.generator)),
            st_suspended: rd_u8(&buf, fo!(t.frame.state_suspended)),
            c_filename: u64at!(fo!(t.code.filename)),
            c_name: u64at!(fo!(t.code.name)),
            c_qualname: u64at!(fo!(t.code.qualname)),
            c_linetable: u64at!(fo!(t.code.linetable)),
            c_firstlineno: u64at!(fo!(t.code.firstlineno)),
            c_data: u64at!(fo!(t.code.data)),
            c_meta: u64at!(fo!(t.code.meta)),
            c_pub_of_internal: u64at!(fo!(t.code.data_pub_of_internal)),
            c_meta_co_code: u64at!(fo!(t.code.meta_co_code)),
            o_ob_type: u64at!(fo!(t.obj.ob_type)),
            o_var_size: u64at!(fo!(t.obj.var_size)),
            o_str_length: u64at!(fo!(t.obj.str_length)),
            o_str_state: u64at!(fo!(t.obj.str_state)),
            o_str_ascii_data: u64at!(fo!(t.obj.str_ascii_data)),
            o_str_compact_data: u64at!(fo!(t.obj.str_compact_data)),
            o_str_data_ptr: u64at!(fo!(t.obj.str_data_ptr)),
            o_bytes_data: u64at!(fo!(t.obj.bytes_data)),
            o_tuple_items: u64at!(fo!(t.obj.tuple_items)),
            o_set_mask: u64at!(fo!(t.obj.set_mask)),
            o_set_table: u64at!(fo!(t.obj.set_table)),
            o_set_entry_size: u64at!(fo!(t.obj.set_entry_size)),
            o_gen_frame: u64at!(fo!(t.obj.gen_frame)),
            o_agen_gen: u64at!(fo!(t.obj.agen_awaitable_gen)),
            o_type_name: u64at!(fo!(t.obj.type_name)),
            o_interp_id: u64at!(fo!(t.obj.interp_id)),
            a_task_name: u64at!(fo!(t.asyncio.task_name)),
            a_task_coro: u64at!(fo!(t.asyncio.task_coro)),
            a_awaited_by: u64at!(fo!(t.asyncio.fut_awaited_by)),
            a_fut_state: u64at!(fo!(t.asyncio.fut_state)),
            a_await_code: u64at!(fo!(t.asyncio.future_await_code)),
            a_task_type: u64at!(fo!(t.asyncio.task_type)),
        };
        Ok(table)
    }
}

// ---------------------------------------------------------------- struct-seq types

macro_rules! ss_type {
    ($st:ident, $getter:ident, $name:literal, [$($f:literal),*], $nseq:expr) => {
        static mut $st: PyTypeObject = PyTypeObject::ZERO;
        pub fn $getter() -> *mut PyTypeObject {
            &raw mut $st
        }
    };
}

ss_type!(FRAME_INFO, frame_info_type, "FrameInfo", ["filename", "location", "funcname", "opcode"], 4);
ss_type!(LOCATION_INFO, location_info_type, "LocationInfo", ["lineno", "end_lineno", "col_offset", "end_col_offset"], 4);
ss_type!(THREAD_INFO, thread_info_type, "ThreadInfo", ["thread_id", "status", "frame_info"], 3);
ss_type!(INTERP_INFO, interp_info_type, "InterpreterInfo", ["interpreter_id", "threads"], 2);
ss_type!(CORO_INFO, coro_info_type, "CoroInfo", ["call_stack", "task_name"], 2);
ss_type!(TASK_INFO, task_info_type, "TaskInfo", ["task_id", "task_name", "coroutine_stack", "awaited_by"], 4);
ss_type!(AWAITED_INFO, awaited_info_type, "AwaitedInfo", ["thread_id", "awaited_by"], 2);

fn init_types() -> PyResult<()> {
    unsafe {
        structseq::init_type(frame_info_type(), &Desc {
            name: "_remote_debugging.FrameInfo",
            doc: "A frame: (filename, location, funcname, opcode).",
            fields: &[Field::new("filename", "source file"), Field::new("location", "source location"), Field::new("funcname", "qualified name"), Field::new("opcode", "current opcode or None")],
            n_in_sequence: 4,
        })?;
        structseq::init_type(location_info_type(), &Desc {
            name: "_remote_debugging.LocationInfo",
            doc: "A source location: (lineno, end_lineno, col_offset, end_col_offset).",
            fields: &[Field::new("lineno", "first line"), Field::new("end_lineno", "last line"), Field::new("col_offset", "first column"), Field::new("end_col_offset", "last column")],
            n_in_sequence: 4,
        })?;
        structseq::init_type(thread_info_type(), &Desc {
            name: "_remote_debugging.ThreadInfo",
            doc: "A thread: (thread_id, status, frame_info).",
            fields: &[Field::new("thread_id", "native thread id"), Field::new("status", "status flags"), Field::new("frame_info", "frames, innermost first")],
            n_in_sequence: 3,
        })?;
        structseq::init_type(interp_info_type(), &Desc {
            name: "_remote_debugging.InterpreterInfo",
            doc: "An interpreter: (interpreter_id, threads).",
            fields: &[Field::new("interpreter_id", "interpreter id"), Field::new("threads", "its threads")],
            n_in_sequence: 2,
        })?;
        structseq::init_type(coro_info_type(), &Desc {
            name: "_remote_debugging.CoroInfo",
            doc: "A coroutine stack: (call_stack, task_name).",
            fields: &[Field::new("call_stack", "frames"), Field::new("task_name", "owning/awaiting task id")],
            n_in_sequence: 2,
        })?;
        structseq::init_type(task_info_type(), &Desc {
            name: "_remote_debugging.TaskInfo",
            doc: "A task: (task_id, task_name, coroutine_stack, awaited_by).",
            fields: &[Field::new("task_id", "task id"), Field::new("task_name", "task name"), Field::new("coroutine_stack", "the task's coroutine stack"), Field::new("awaited_by", "coroutines awaiting it")],
            n_in_sequence: 4,
        })?;
        structseq::init_type(awaited_info_type(), &Desc {
            name: "_remote_debugging.AwaitedInfo",
            doc: "Per-thread awaited-by info: (thread_id, awaited_by).",
            fields: &[Field::new("thread_id", "native thread id"), Field::new("awaited_by", "its tasks")],
            n_in_sequence: 2,
        })?;
    }
    Ok(())
}

fn ss(tp: *mut PyTypeObject, vals: Vec<PyRef>) -> PyRef {
    structseq::new_instance(tp, vals)
}

// ---------------------------------------------------------------- the unwinder object

#[repr(C)]
struct Unwinder {
    ob_base: PyObject,
    inner: *mut UnwinderState,
}

struct UnwinderState {
    pub(crate) proc: Process,
    pub(crate) table: Table,
    all_threads: bool,
    only_active: bool,
    cache_frames: bool,
    mode: i64,
    skip_non_matching: bool,
    opcodes: bool,
    stats_on: bool,
    stats: RefCell<Stats>,
    paused: std::cell::Cell<bool>,
    gc: bool,
    /// cached FrameInfo per (thread native id, frame address)
    frame_cache: RefCell<HashMap<(u64, u64), (u64, u32, PyRef)>>,
    /// cached (filename, qualname, linetable bytes, firstlineno, pub_of_internal) per code addr
    code_cache: RefCell<HashMap<u64, CodeCache>>,
}

struct CodeCache {
    filename: PyRef,
    funcname: PyRef,
    linetable: Vec<u8>,
    firstlineno: u32,
    pub_of_internal: Vec<u32>,
    co_code: Vec<u8>,
    /// (tail4) pointers the entry was built from: (filename, qualname, linetable,
    /// code data); a code object freed and another allocated at the same address
    /// is recognised by a change of these.
    stamp: (u64, u64, u64, u64),
}

#[derive(Default)]
struct Stats {
    total_samples: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_partial_hits: u64,
    frames_from_cache: u64,
    frames_from_memory: u64,
}

static mut UNWINDER_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn unwinder_type() -> *mut PyTypeObject {
    &raw mut UNWINDER_TYPE
}

fn us(o: &PyObject) -> &UnwinderState {
    unsafe { &*(*(o.as_ptr() as *const Unwinder)).inner }
}

unsafe extern "C" fn unwinder_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let u = op as *mut Unwinder;
        if !(*u).inner.is_null() {
            if (*(*u).inner).paused.get() {
                libc::kill((*(*u).inner).proc.pid, libc::SIGCONT);
            }
            drop(Box::from_raw((*u).inner));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

unsafe extern "C" fn unwinder_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new(
                "RemoteUnwinder",
                &["pid", "all_threads", "only_active_thread", "debug", "cache_frames", "mode", "native", "skip_non_matching_threads", "opcodes", "stats", "gc"],
                0,
                1,
            )
            .kwonly(10);
            let parsed = crate::args::parse::<11>(&P, &refs, kws.as_deref())?;
            let pid = int::as_i64(parsed[0].unwrap()).ok_or_else(|| err!(TypeError, "pid must be an int"))? as i32;
            let getb = |i: usize, d: bool| parsed[i].map(|v| ab::is_true(v).unwrap_or(d)).unwrap_or(d);
            let mode = parsed[5].and_then(int::as_i64).unwrap_or(0);
            let proc = Process::new(pid);
            let table = Table::load(&proc)?;
            let state = UnwinderState {
                proc,
                table,
                all_threads: getb(1, false),
                only_active: getb(2, false),
                cache_frames: getb(4, false),
                mode,
                skip_non_matching: getb(7, true),
                opcodes: getb(8, false),
                stats_on: getb(9, false),
                stats: RefCell::new(Stats::default()),
                paused: std::cell::Cell::new(false),
                gc: getb(10, true),
                frame_cache: RefCell::new(HashMap::new()),
                code_cache: RefCell::new(HashMap::new()),
            };
            let op = ((*tp).tp_alloc.unwrap())(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            (*(op as *mut Unwinder)).inner = Box::into_raw(Box::new(state));
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

// --- reading primitives on the target

impl UnwinderState {
    fn ptr(&self, addr: u64) -> PyResult<u64> {
        self.proc.u64(addr).map_err(to_mem_err)
    }
    fn u32(&self, addr: u64) -> PyResult<u32> {
        self.proc.u32(addr).map_err(to_mem_err)
    }

    /// Read a PEP 393 string, truncated to `max` code points, as a Rust String.
    fn read_str(&self, obj: u64, max: usize) -> PyResult<String> {
        if obj == 0 {
            return Ok(String::new());
        }
        let t = &self.table;
        let length = self.proc.u64(obj + t.o_str_length).map_err(to_mem_err)? as usize;
        let state = self.u32(obj + t.o_str_state)?;
        let kind = ((state >> STR_KIND_SHIFT) & 7).max(1) as usize;
        let data_addr = if state & STR_COMPACT != 0 {
            if state & STR_ASCII != 0 {
                obj + t.o_str_ascii_data
            } else {
                obj + t.o_str_compact_data
            }
        } else {
            self.proc.u64(obj + t.o_str_data_ptr).map_err(to_mem_err)?
        };
        let n = length.min(max);
        let bytes = self.proc.read_vec(data_addr, n * kind).map_err(to_mem_err)?;
        let mut s = String::with_capacity(n);
        for i in 0..n {
            let cp = match kind {
                1 => bytes[i] as u32,
                2 => u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]) as u32,
                _ => u32::from_le_bytes([bytes[4 * i], bytes[4 * i + 1], bytes[4 * i + 2], bytes[4 * i + 3]]),
            };
            s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
        }
        Ok(s)
    }

    fn read_bytes(&self, obj: u64, max: usize) -> PyResult<Vec<u8>> {
        if obj == 0 {
            return Ok(Vec::new());
        }
        let t = &self.table;
        let size = self.proc.u64(obj + t.o_var_size).map_err(to_mem_err)? as usize;
        if size > max {
            return Ok(Vec::new()); // caller treats an oversized blob as "degraded"
        }
        self.proc.read_vec(obj + t.o_bytes_data, size).map_err(to_mem_err)
    }

    pub(crate) fn type_of(&self, obj: u64) -> PyResult<u64> {
        self.ptr(obj + self.table.o_ob_type)
    }

    /// `read_str` reachable from the async submodule.
    pub(crate) fn read_str_pub(&self, obj: u64, max: usize) -> PyResult<String> {
        self.read_str(obj, max)
    }
}

/// `read_frame` reachable from the async submodule.
pub(crate) fn read_frame_pub(st: &UnwinderState, frame_addr: u64) -> PyResult<Option<PyRef>> {
    read_frame(st, frame_addr)
}

// --- the code object

fn code_cache_get<'a>(st: &'a UnwinderState, code: u64) -> PyResult<()> {
    let t = &st.table;
    let fn_obj = st.ptr(code + t.c_filename)?;
    let qn_obj = st.ptr(code + t.c_qualname)?;
    let lt_obj = st.ptr(code + t.c_linetable)?;
    let data = st.ptr(code + t.c_data)?;
    let stamp = (fn_obj, qn_obj, lt_obj, data);
    if let Some(c) = st.code_cache.borrow().get(&code) {
        // still the same code object (and its translation has not appeared since)
        if c.stamp == stamp {
            return Ok(());
        }
    }
    let filename = st.read_str(fn_obj, MAX_NAME)?;
    let funcname = st.read_str(qn_obj, MAX_NAME)?;
    let firstlineno = st.u32(code + t.c_firstlineno)?;
    let linetable = st.read_bytes(lt_obj, MAX_LINETABLE)?;
    // pub_of_internal from CodeData (may be absent if the code never ran)
    let mut pub_of_internal = Vec::new();
    if data != 0 {
        let vp = st.ptr(data + t.c_pub_of_internal + t.vec_ptr)?;
        let vl = st.ptr(data + t.c_pub_of_internal + t.vec_len)? as usize;
        if vp != 0 && vl > 0 && vl < 1 << 24 {
            let raw = st.proc.read_vec(vp, vl * 4).map_err(to_mem_err)?;
            pub_of_internal = raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        }
    }
    let mut co_code = Vec::new();
    if st.opcodes {
        let meta = st.ptr(code + t.c_meta)?;
        if meta != 0 {
            let cc = st.ptr(meta + t.c_meta_co_code)?;
            co_code = st.read_bytes(cc, 1 << 24)?;
        }
    }
    st.code_cache.borrow_mut().insert(code, CodeCache {
        filename: pystr::new_str(&filename),
        funcname: pystr::new_str(&funcname),
        linetable,
        firstlineno,
        pub_of_internal,
        co_code,
        stamp,
    });
    Ok(())
}

/// Build a FrameInfo for a frame at `frame_addr`.
fn read_frame(st: &UnwinderState, frame_addr: u64) -> PyResult<Option<PyRef>> {
    let t = &st.table;
    let code = st.ptr(frame_addr + t.f_code)?;
    if code == 0 {
        return Ok(None);
    }
    // only our code objects
    let tp = st.type_of(code)?;
    if tp != t.code_type {
        return Ok(None);
    }
    let lasti = st.u32(frame_addr + t.f_lasti)? as usize;
    code_cache_get(st, code)?;
    let cc = st.code_cache.borrow();
    let c = cc.get(&code).unwrap();
    let pub_off = c.pub_of_internal.get(lasti).copied().unwrap_or(0) as usize;
    let location = if c.linetable.is_empty() {
        none::none_ref()
    } else {
        let positions = crate::interp::wc::decode_linetable(&c.linetable, c.firstlineno, pub_off + 1);
        match positions.get(pub_off) {
            Some(p) if p.lineno.is_some() => {
                let g = |o: Option<u32>| int::new_int(o.map(|x| x as i64).unwrap_or(0));
                let gc = |o: Option<u32>| int::new_int(o.map(|x| x as i64).unwrap_or(0));
                ss(location_info_type(), vec![
                    int::new_int(p.lineno.unwrap() as i64),
                    g(p.end_lineno),
                    gc(p.col),
                    gc(p.end_col),
                ])
            }
            _ => none::none_ref(),
        }
    };
    let opcode = if st.opcodes && pub_off * 2 < c.co_code.len() {
        int::new_int(c.co_code[pub_off * 2] as i64)
    } else {
        none::none_ref()
    };
    let fi = ss(frame_info_type(), vec![c.filename.clone(), location, c.funcname.clone(), opcode]);
    Ok(Some(fi))
}

/// Walk the Rust frame chain starting at `top` (a `*mut Frame`), innermost first.
///
/// With `cache_frames`, a parent frame (depth > 1) whose address, code object and
/// instruction offset are unchanged is reused as the same FrameInfo object (the profiler
/// relies on this identity); the innermost frame is always re-read so its line number is
/// current. A cached entry whose code/lasti no longer match (a frame-pool address reused
/// by a different call) is rebuilt.
fn walk_frames(st: &UnwinderState, native_id: u64, top: u64) -> PyResult<Option<Vec<PyRef>>> {
    let t = &st.table;
    let mut out = Vec::new();
    let mut addr = top;
    let mut depth = 0;
    while addr != 0 {
        depth += 1;
        if depth > MAX_FRAME_CHAIN {
            raise!(RuntimeError, "Too many stack frames while unwinding");
        }
        // (tail4) the target keeps running while its frames are read: a frame that
        // was released meanwhile (no code object any more) or a generator frame that
        // lost its caller link (the generator suspended) means a torn read, which
        // the caller retries
        let code = st.ptr(addr + t.f_code)?;
        if code == 0 || st.type_of(code)? != t.code_type {
            return Ok(None);
        }
        let generator = st.ptr(addr + t.f_generator)?;
        let lasti = st.u32(addr + t.f_lasti)?;
        let cacheable = st.cache_frames && depth > 1;
        let mut cached = None;
        if cacheable {
            if let Some((c, l, fi)) = st.frame_cache.borrow().get(&(native_id, addr)) {
                if *c == code && *l == lasti {
                    cached = Some(fi.clone());
                }
            }
        }
        match cached {
            Some(fi) => {
                if st.stats_on {
                    let mut s = st.stats.borrow_mut();
                    s.frames_from_cache += 1;
                    s.cache_hits += 1;
                }
                out.push(fi);
            }
            None => {
                let Some(fi) = read_frame(st, addr)? else { return Ok(None) };
                if st.stats_on {
                    let mut s = st.stats.borrow_mut();
                    s.frames_from_memory += 1;
                    s.cache_misses += 1;
                }
                if cacheable {
                    st.frame_cache.borrow_mut().insert((native_id, addr), (code, lasti, fi.clone()));
                }
                out.push(fi);
            }
        }
        let prev = st.ptr(addr + t.f_prev)?;
        if prev == 0 {
            // the outermost frame: never a generator's (generators always have a
            // caller), and still the same live frame
            if generator != 0 || st.ptr(addr + t.f_generator)? != 0 || st.ptr(addr + t.f_code)? != code {
                return Ok(None);
            }
        }
        addr = prev;
    }
    Ok(Some(out))
}

// --- threads

struct ThreadRec {
    ts: u64,
    native_id: u64,
    interp_id: i64,
    frame: u64,
    mode: u8,
    status: i64,
}

fn thread_states(st: &UnwinderState) -> PyResult<Vec<u64>> {
    let t = &st.table;
    if !st.all_threads {
        let m = st.ptr(t.main_thread)?;
        return Ok(if m != 0 { vec![m] } else { vec![] });
    }
    // seq-locked mirror
    for _ in 0..100 {
        let s1 = st.proc.u64(t.thread_list_seq).map_err(to_mem_err)?;
        if s1 & 1 != 0 {
            continue;
        }
        let len = (st.proc.u64(t.thread_list_len).map_err(to_mem_err)? as usize).min(t.thread_list_cap as usize);
        let raw = st.proc.read_vec(t.thread_list, len * 8).map_err(to_mem_err)?;
        let s2 = st.proc.u64(t.thread_list_seq).map_err(to_mem_err)?;
        if s1 != s2 {
            continue;
        }
        let v: Vec<u64> = raw.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).filter(|&p| p != 0).collect();
        return Ok(v);
    }
    raise!(RuntimeError, "thread list kept changing while reading it")
}

fn thread_has_exception(st: &UnwinderState, ts: u64) -> PyResult<bool> {
    let t = &st.table;
    if st.ptr(ts + t.t_current_exc)? != 0 {
        return Ok(true);
    }
    let mut item = st.ptr(ts + t.t_exc_info)?;
    let mut guard = 0;
    while item != 0 && guard < 10_000 {
        guard += 1;
        let v = st.ptr(item + t.e_value)?;
        if v != 0 && v != t.none {
            return Ok(true);
        }
        item = st.ptr(item + t.e_prev)?;
    }
    Ok(false)
}

fn read_thread(st: &UnwinderState, ts: u64) -> PyResult<Option<ThreadRec>> {
    let t = &st.table;
    // exited states linger in the list
    let exited = st.proc.u8(ts + t.t_exited).map_err(to_mem_err)?;
    if exited != 0 {
        return Ok(None);
    }
    let native_id = st.ptr(ts + t.t_native)?;
    let interp = st.ptr(ts + t.t_interp)?;
    let interp_id = if interp == 0 { 0 } else { st.proc.u64(interp + t.o_interp_id).map_err(to_mem_err)? as i64 };
    let frame = st.ptr(ts + t.t_frame)?;
    let mode = st.proc.u8(ts + t.t_mode).map_err(to_mem_err)?;
    // status flags
    let mut status = 0i64;
    use crate::modules::remote_debug::*;
    if mode == 0 {
        status |= THREAD_STATUS_HAS_GIL;
    }
    match thread_state(st.proc.pid, native_id) {
        Some('R') => status |= THREAD_STATUS_ON_CPU,
        Some(_) => {}
        None => status |= THREAD_STATUS_UNKNOWN,
    }
    if thread_has_exception(st, ts)? {
        status |= THREAD_STATUS_HAS_EXCEPTION;
    }
    if ts == st.ptr(t.main_thread)? {
        status |= THREAD_STATUS_MAIN_THREAD;
    }
    Ok(Some(ThreadRec { ts, native_id, interp_id, frame, mode, status }))
}

fn mode_keeps(st: &UnwinderState, status: i64) -> bool {
    use crate::modules::remote_debug::*;
    if !st.skip_non_matching {
        return true;
    }
    match st.mode {
        1 => status & THREAD_STATUS_ON_CPU != 0,          // CPU
        2 => status & THREAD_STATUS_HAS_GIL != 0,          // GIL
        4 => status & THREAD_STATUS_HAS_EXCEPTION != 0,    // EXCEPTION
        _ => true,
    }
}

fn get_stack_trace(o: &PyObject) -> PyResult {
    let st = us(o);
    if !st.proc.alive() {
        raise!(ProcessLookupError, "process {} is not running", st.proc.pid);
    }
    if st.stats_on {
        st.stats.borrow_mut().total_samples += 1;
    }
    // the current (innermost) frame of each thread is re-read, parents may be cached:
    // drop only the innermost entry's cache would be complex, so invalidate on each
    // sample the entries whose frame address is the current top (handled by walking:
    // depth==1 is never served from cache)
    let states = thread_states(st)?;
    // group threads by interpreter
    let mut by_interp: Vec<(i64, Vec<PyRef>)> = Vec::new();
    for ts in states {
        let Some(rec) = read_thread(st, ts)? else { continue };
        if !mode_keeps(st, rec.status) {
            continue;
        }
        // (tail4) a generator frame whose caller link vanished while it was read
        // (the generator suspended mid-walk) gives a truncated stack: read again
        let mut frames = None;
        let mut top = rec.frame;
        for _ in 0..16 {
            if let Some(f) = walk_frames(st, rec.native_id, top)? {
                frames = Some(f);
                break;
            }
            top = st.ptr(ts + st.table.t_frame)?;
        }
        let Some(mut frames) = frames else {
            raise!(RuntimeError, "could not read a consistent stack of thread {}", rec.native_id);
        };
        // (tail4) a thread running the garbage collector gets a synthetic "<GC>" frame
        if st.gc && st.table.gc_collector != 0 && st.ptr(st.table.gc_collector)? == ts {
            let gc_frame = ss(frame_info_type(), vec![pystr::new_str("~"), none::none_ref(), pystr::new_str("<GC>"), none::none_ref()]);
            frames.insert(0, gc_frame);
        }
        let ti = ss(thread_info_type(), vec![int::new_int(rec.native_id as i64), int::new_int(rec.status), list::new_list(frames)]);
        match by_interp.iter_mut().find(|(id, _)| *id == rec.interp_id) {
            Some((_, v)) => v.push(ti),
            None => by_interp.push((rec.interp_id, vec![ti])),
        }
    }
    let interps: Vec<PyRef> = by_interp
        .into_iter()
        .map(|(id, threads)| ss(interp_info_type(), vec![int::new_int(id), list::new_list(threads)]))
        .collect();
    Ok(list::new_list(interps))
}

fn get_stats(o: &PyObject) -> PyResult {
    let st = us(o);
    if !st.stats_on {
        raise!(RuntimeError, "statistics were not enabled for this unwinder");
    }
    let s = st.stats.borrow();
    let d = dict::new_dict();
    let set = |k: &str, v: i64| dict::set_item_str(&d, k, &int::new_int(v));
    set("total_samples", s.total_samples as i64)?;
    set("frame_cache_hits", s.cache_hits as i64)?;
    set("frame_cache_misses", s.cache_misses as i64)?;
    set("frame_cache_partial_hits", s.cache_partial_hits as i64)?;
    set("frames_read_from_cache", s.frames_from_cache as i64)?;
    set("frames_read_from_memory", s.frames_from_memory as i64)?;
    let total = s.frames_from_cache + s.frames_from_memory;
    let rate = if total > 0 { s.frames_from_cache as f64 / total as f64 } else { 0.0 };
    dict::set_item_str(&d, "frame_cache_hit_rate", &crate::types::float::new_float(rate))?;
    for k in ["batched_read_attempts", "batched_read_successes", "batched_read_misses", "batched_read_segments_requested", "batched_read_segments_completed"] {
        set(k, 0)?;
    }
    dict::set_item_str(&d, "batched_read_success_rate", &crate::types::float::new_float(0.0))?;
    dict::set_item_str(&d, "batched_read_segment_completion_rate", &crate::types::float::new_float(0.0))?;
    Ok(d)
}

// --- stopping the target (tail4): job-control stop of the whole process, then wait
// until every thread reports the stopped state so no memory changes while it is read.

fn all_threads_stopped(pid: i32) -> bool {
    let Ok(rd) = std::fs::read_dir(format!("/proc/{}/task", pid)) else { return true };
    for e in rd.flatten() {
        let Ok(tid) = e.file_name().to_string_lossy().parse::<u64>() else { continue };
        match thread_state(pid, tid) {
            Some('T') | Some('t') | Some('Z') | Some('X') | None => {}
            Some(_) => return false,
        }
    }
    true
}

fn pause_threads(o: &PyObject) -> PyResult {
    let st = us(o);
    if st.paused.get() {
        return Ok(bool_::new_bool(false));
    }
    let pid = st.proc.pid;
    if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
        let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    st.paused.set(true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !all_threads_stopped(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
    Ok(bool_::new_bool(true))
}

fn resume_threads(o: &PyObject) -> PyResult {
    let st = us(o);
    if !st.paused.get() {
        return Ok(bool_::new_bool(false));
    }
    st.paused.set(false);
    unsafe { libc::kill(st.proc.pid, libc::SIGCONT) };
    Ok(bool_::new_bool(true))
}

// --- async (best effort)

mod asyncwalk;
mod gcstats; // tail4

// --- module-level functions

fn is_python_process(_m: &PyObject, arg: &PyObject) -> PyResult {
    let pid = int::as_i64(arg).ok_or_else(|| err!(TypeError, "pid must be an int"))? as i32;
    let p = Process::new(pid);
    let ok = matches!(find_section(pid, SEC), Ok(Some(_)));
    let _ = p;
    Ok(bool_::new_bool(ok))
}

fn get_child_pids(_m: &PyObject, arg: &PyObject) -> PyResult {
    let pid = int::as_i64(arg).ok_or_else(|| err!(TypeError, "pid must be an int"))? as i32;
    let kids: Vec<PyRef> = child_pids(pid, true).into_iter().map(|p| int::new_int(p as i64)).collect();
    Ok(list::new_list(kids))
}

fn zstd_available(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(true))
}

/// `sys.remote_exec` targeting another process: write the script path into its mailbox
/// and set the remote-exec eval-breaker bit of its main thread.
pub fn do_remote_exec(pid: i32, path: &[u8]) -> PyResult<()> {
    let p = Process::new(pid);
    let table = Table::load(&p)?;
    if table.flags & 1 == 0 {
        raise!(RuntimeError, "Remote debugging is not enabled in the remote process");
    }
    if table.remote_exec == 0 {
        raise!(RuntimeError, "the remote process does not support remote exec");
    }
    if path.len() + 1 > super::offsets::REMOTE_PATH_MAX {
        raise!(ValueError, "script path is too long");
    }
    // path then NUL
    let mut buf = path.to_vec();
    buf.push(0);
    // RemoteExecArea: pending(u32), _pad(u32), path[...]
    p.write(table.remote_exec + 8, &buf).map_err(to_mem_err)?;
    p.write(table.remote_exec, &1u32.to_le_bytes()).map_err(to_mem_err)?;
    // set the eval-breaker byte of the target's main thread
    let main = p.u64(table.main_thread).map_err(to_mem_err)?;
    if main != 0 {
        let byte_addr = main + table.remote_exec_byte;
        let cur = p.u8(byte_addr).map_err(to_mem_err)?;
        p.write(byte_addr, &[cur | 1]).map_err(to_mem_err)?;
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    unsafe {
        let t = &mut *unwinder_type();
        t.tp_name = crate::methods::cstr_static("_remote_debugging.RemoteUnwinder");
        t.tp_basicsize = std::mem::size_of::<Unwinder>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_new = Some(unwinder_new);
        t.tp_dealloc = Some(unwinder_dealloc);
        t.tp_traverse = Some(empty_traverse);
        t.tp_methods = crate::mdefs![
            ("get_stack_trace", NOARGS, get_stack_trace, "get_stack_trace($self, /)\n--\n\nStack traces of the target's threads."),
            ("get_all_awaited_by", NOARGS, asyncwalk::get_all_awaited_by, "get_all_awaited_by($self, /)\n--\n\nAll asyncio tasks and what awaits them."),
            ("get_async_stack_trace", NOARGS, asyncwalk::get_async_stack_trace, "get_async_stack_trace($self, /)\n--\n\nThe async stack trace of the running tasks."),
            ("get_stats", NOARGS, get_stats, "get_stats($self, /)\n--\n\nSampling statistics (stats=True only)."),
            ("pause_threads", NOARGS, pause_threads, "pause_threads($self, /)\n--\n\nStop every thread of the target; False if already paused."),
            ("resume_threads", NOARGS, resume_threads, "resume_threads($self, /)\n--\n\nLet the target's threads run again; False if not paused."),
        ];
        crate::types::typeobject::type_ready(unwinder_type())?;
    }
    let methods = crate::mdefs![
        ("is_python_process", O, is_python_process, "is_python_process($module, pid, /)\n--\n\nIs the process a compatible Python process?"),
        ("get_child_pids", O, get_child_pids, "get_child_pids($module, pid, /)\n--\n\nDescendant process ids of pid."),
        ("zstd_available", NOARGS, zstd_available, "zstd_available($module, /)\n--\n\nWhether zstd compression is available."),
        ("get_gc_stats", FASTKW, gcstats::module_get_gc_stats, "get_gc_stats($module, /, pid, *, all_interpreters=False)\n--\n\nGarbage collection statistics of process pid."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("_remote_debugging", methods, Some("Inspect another Python process's interpreter state."))?;
    dict::set_item_str(&d, "RemoteUnwinder", &crate::types::typeobject::type_ref(unwinder_type()))?;
    dict::set_item_str(&d, "FrameInfo", &crate::types::typeobject::type_ref(frame_info_type()))?;
    dict::set_item_str(&d, "LocationInfo", &crate::types::typeobject::type_ref(location_info_type()))?;
    dict::set_item_str(&d, "ThreadInfo", &crate::types::typeobject::type_ref(thread_info_type()))?;
    dict::set_item_str(&d, "InterpreterInfo", &crate::types::typeobject::type_ref(interp_info_type()))?;
    dict::set_item_str(&d, "CoroInfo", &crate::types::typeobject::type_ref(coro_info_type()))?;
    dict::set_item_str(&d, "TaskInfo", &crate::types::typeobject::type_ref(task_info_type()))?;
    dict::set_item_str(&d, "AwaitedInfo", &crate::types::typeobject::type_ref(awaited_info_type()))?;
    super::binfmt::init(&d)?; // tail4
    gcstats::init(&d)?; // tail4
    dict::set_item_str(&d, "PROCESS_VM_READV_SUPPORTED", &bool_::new_bool(true))?;
    for (k, v) in [
        ("THREAD_STATUS_HAS_GIL", super::THREAD_STATUS_HAS_GIL),
        ("THREAD_STATUS_ON_CPU", super::THREAD_STATUS_ON_CPU),
        ("THREAD_STATUS_UNKNOWN", super::THREAD_STATUS_UNKNOWN),
        ("THREAD_STATUS_GIL_REQUESTED", super::THREAD_STATUS_GIL_REQUESTED),
        ("THREAD_STATUS_HAS_EXCEPTION", super::THREAD_STATUS_HAS_EXCEPTION),
        ("THREAD_STATUS_MAIN_THREAD", super::THREAD_STATUS_MAIN_THREAD),
    ] {
        dict::set_item_str(&d, k, &int::new_int(v))?;
    }
    Ok(m)
}

unsafe extern "C" fn empty_traverse(_op: *mut PyObject, _visit: visitproc, _arg: *mut c_void) -> std::ffi::c_int {
    0
}

// re-exports for the async submodule
pub(crate) type RdState = UnwinderState;
pub(crate) fn rd_us(o: &PyObject) -> &UnwinderState {
    us(o)
}
pub(crate) fn rd_ss(tp: *mut PyTypeObject, v: Vec<PyRef>) -> PyRef {
    ss(tp, v)
}
pub(crate) fn rd_to_mem_err(e: MemError) -> PyErr {
    to_mem_err(e)
}
