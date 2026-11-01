//! Tier 2: the optimising compiler (design notes: notes/jit.md, "Tier 2").
//!
//! Pipeline: bytecode + tier-1 type profiles + inline-cache state -> typed IR by
//! abstract interpretation with speculation (build.rs) -> linear-scan register
//! allocation (regalloc.rs) -> x86-64 code (codegen.rs) with deoptimisation stubs that
//! rebuild the interpreter frame and resume in the interpreter.
//!
//! Units: the standard unit enters at pc 0 (function calls); an OSR unit enters at a
//! loop header (a long-running activation moves from tier 1 into tier 2 at a
//! backedge). Both are published per code object in `CodeCaches::opt` / `osr`.
//!
//! Invalidation (HotSpot "dependencies"): values of globals/builtins are folded into
//! the code as constants. The globals and builtins dicts are then watched; any write
//! to a watched dict marks the dependent code invalid (a byte checked at entries,
//! after every escaping instruction and at backedges, where the code deoptimises),
//! unpublishes it and republishes the tier-1 entry. Invalidated code stays mapped
//! until the code object dies (activations may still be running in it), so other
//! threads never execute freed code. Code that deoptimises too often is discarded the
//! same way.

pub mod build;
pub mod codegen;
pub mod ir;
pub mod regalloc;

use crate::ffi::*;
use crate::interp::cache::CodeCaches;
use crate::types::code::CodeData;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering::*};

/// Object/frame layout constants used by the generated code.
pub mod layout {
    use crate::ffi::*;
    use crate::interp::frame::Frame;
    use crate::threads::{CThreadState, ThreadState};
    use std::mem::offset_of;
    pub const OB_TID: i32 = offset_of!(PyObject, ob_tid) as i32;
    pub const OB_MUTEX: i32 = offset_of!(PyObject, ob_mutex) as i32;
    pub const GC_BITS: i32 = offset_of!(PyObject, ob_gc_bits) as i32;
    pub const OB_REF_LOCAL: i32 = offset_of!(PyObject, ob_ref_local) as i32;
    pub const OB_REF_SHARED: i32 = offset_of!(PyObject, ob_ref_shared) as i32;
    pub const OB_TYPE: i32 = offset_of!(PyObject, ob_type) as i32;
    pub const LV_TAG: i32 = offset_of!(PyLongObject, lv_tag) as i32;
    pub const LV_DIGIT: i32 = offset_of!(PyLongObject, ob_digit) as i32;
    pub const FVAL: i32 = offset_of!(PyFloatObject, ob_fval) as i32;
    pub const LIST_SIZE: i32 = offset_of!(PyVarObject, ob_size) as i32;
    pub const LIST_ITEMS: i32 = offset_of!(PyListObject, ob_item) as i32;
    pub const TUPLE_ITEMS: i32 = offset_of!(PyTupleObject, ob_item) as i32;
    pub const LASTI: i32 = offset_of!(Frame, lasti) as i32;
    pub const NEXT_INSTR: i32 = offset_of!(Frame, next_instr) as i32;
    pub const STACK_TOP: i32 = offset_of!(Frame, stack_top) as i32;
    pub const TS_EVAL_BREAKER: i32 = (offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker)) as i32;
    pub use crate::ffi::REF_SHARED_SHIFT;
}

/// Installed tier-2 code.
pub struct OptCode {
    pub addr: usize,
    pub len: usize,
    /// entry pc (code unit index) -> absolute address
    pub entries: Vec<(u32, usize)>,
    /// set when an assumption broke; checked by the code itself
    pub invalid: Box<AtomicU8>,
    pub deopts: Box<AtomicU32>,
}

impl OptCode {
    #[inline]
    pub fn entry(&self, pc: usize) -> Option<usize> {
        self.entries.iter().find(|e| e.0 as usize == pc).map(|e| e.1)
    }
}

// ------------------------------------------------------------------ configuration

static ENABLED: AtomicU8 = AtomicU8::new(2);
static CALLS: AtomicU32 = AtomicU32::new(1000);
static BACKEDGES: AtomicU32 = AtomicU32::new(10000);
static DEBUG: AtomicU8 = AtomicU8::new(0);
/// deoptimisations after which code is discarded
const DEOPT_LIMIT: u32 = 200;
/// compilations per code object
const MAX_TRIES: u8 = 4;

#[cold]
fn init() -> bool {
    // tier 2 is opt-in for now (RPYTHON_JIT2=1 or a threshold): the spec smoke still
    // crashes with it enabled, see notes/jit.md
    let en = std::env::var("RPYTHON_JIT2").map(|v| v != "0").unwrap_or_else(|_| std::env::var("RPYTHON_JIT2_THRESHOLD").is_ok());
    if let Ok(t) = std::env::var("RPYTHON_JIT2_THRESHOLD") {
        if let Ok(n) = t.parse::<u32>() {
            CALLS.store(n.max(1), Relaxed);
            BACKEDGES.store((n * 10).max(1), Relaxed);
        }
    }
    if std::env::var("RPYTHON_JIT_DEBUG").map(|v| v != "0").unwrap_or(false) {
        DEBUG.store(1, Relaxed);
    }
    ENABLED.store(en as u8, Relaxed);
    en
}

#[inline]
pub fn enabled() -> bool {
    match ENABLED.load(Relaxed) {
        2 => init(),
        x => x != 0,
    }
}

pub fn call_threshold() -> i32 {
    enabled();
    CALLS.load(Relaxed) as i32
}
pub fn backedge_threshold() -> i32 {
    enabled();
    BACKEDGES.load(Relaxed) as i32
}

// ------------------------------------------------------------------ dependencies

/// (watched dict, dependent code caches)
static DEPS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Marker in a dict's `_ma_watcher_tag` (type pointers are aligned, so bit 0 is free).
pub const WATCH_TAG: u64 = 1;

fn watch(d: *mut PyObject, cc: &CodeCaches) {
    unsafe {
        let dp = d as *mut PyDictObject;
        let t = (*dp)._ma_watcher_tag;
        if t != 0 && t != WATCH_TAG {
            return; // a type dict: changes are covered by type tags
        }
        (*dp)._ma_watcher_tag = WATCH_TAG;
    }
    let mut g = DEPS.lock().unwrap_or_else(|e| e.into_inner());
    let k = (d as usize, cc as *const _ as usize);
    if !g.contains(&k) {
        g.push(k);
    }
}

/// A watched dict was written (called from cache::on_dict_write).
#[cold]
pub fn dict_written(d: &PyObject) {
    let dk = d.as_ptr() as usize;
    let victims: Vec<usize> = {
        let mut g = DEPS.lock().unwrap_or_else(|e| e.into_inner());
        let v: Vec<usize> = g.iter().filter(|e| e.0 == dk).map(|e| e.1).collect();
        g.retain(|e| e.0 != dk);
        v
    };
    for cc in victims {
        invalidate(unsafe { &*(cc as *const CodeCaches) });
    }
}

/// The code caches are being destroyed: forget their dependencies.
pub fn forget(cc: &CodeCaches) {
    let k = cc as *const _ as usize;
    let mut g = DEPS.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|e| e.1 != k);
}

/// Discard the tier-2 code of a code object (assumption broken or too many deopts).
pub fn invalidate(cc: &CodeCaches) {
    let mut retired = cc.t2_retired.lock().unwrap_or_else(|e| e.into_inner());
    for p in [&cc.opt, &cc.osr] {
        let old = p.swap(std::ptr::null_mut(), AcqRel);
        if !old.is_null() {
            unsafe { (*old).invalid.store(1, Release) };
            retired.push(old as usize);
        }
    }
    drop(retired);
    cc.t2_invalidations.fetch_add(1, Relaxed);
    // calls go back to tier 1 (or the interpreter)
    let j = cc.jit.load(Acquire);
    let t1 = if j.is_null() {
        0
    } else {
        let c = unsafe { &*j };
        match c.entries.first() {
            Some(&o) if o != u32::MAX && cc.fast_argc_ok.load(Relaxed) => c.addr + o as usize,
            _ => 0,
        }
    };
    cc.fast_entry.store(t1, Release);
    // allow a recompilation later
    cc.t2_calls.store(call_threshold(), Relaxed);
    cc.t2_backedge.store(backedge_threshold(), Relaxed);
}

pub fn free_retired(cc: &CodeCaches) {
    let v = std::mem::take(&mut *cc.t2_retired.lock().unwrap_or_else(|e| e.into_inner()));
    for p in v {
        free_opt(p as *mut OptCode);
    }
    for p in [&cc.opt, &cc.osr] {
        let old = p.swap(std::ptr::null_mut(), AcqRel);
        if !old.is_null() {
            free_opt(old);
        }
    }
}

fn free_opt(p: *mut OptCode) {
    unsafe {
        let b = Box::from_raw(p);
        super::mem::free(b.addr, b.len);
    }
}

// ------------------------------------------------------------------ compilation

/// Compile the tier-2 unit of `code` entered at `entry_pc` (0: function entry, else
/// an OSR loop header). Returns the entry address on success.
pub fn compile(code: &PyObject, globals: *mut PyObject, builtins: *mut PyObject, entry_pc: usize) -> Option<usize> {
    if !enabled() || crate::modules::sys::tracing_active() {
        return None;
    }
    let cd = crate::types::code::code_data(code);
    let cc = &cd.caches;
    let co = crate::types::code::code(code);
    let flags = co.flags();
    use rpy_compiler::bytecode::co as cof;
    if flags & (cof::GENERATOR | cof::COROUTINE | cof::ASYNC_GENERATOR | cof::ITERABLE_COROUTINE) != 0 || flags & cof::OPTIMIZED == 0 {
        cc.t2_tries.store(MAX_TRIES, Relaxed);
        return None;
    }
    if cc.t2_tries.load(Relaxed) >= MAX_TRIES || globals.is_null() || builtins.is_null() {
        return None;
    }
    if cc.t2_busy.compare_exchange(0, 1, Acquire, Relaxed).is_err() {
        return None;
    }
    let r = compile_locked(code, cd, globals, builtins, entry_pc, flags);
    cc.t2_busy.store(0, Release);
    r
}

fn compile_locked(code: &PyObject, cd: &CodeData, globals: *mut PyObject, builtins: *mut PyObject, entry_pc: usize, flags: u32) -> Option<usize> {
    let cc = &cd.caches;
    let slot = if entry_pc == 0 { &cc.opt } else { &cc.osr };
    if let Some(p) = unsafe { slot.load(Acquire).as_ref() } {
        if let Some(e) = p.entry(entry_pc) {
            return Some(e);
        }
    }
    cc.t2_tries.fetch_add(1, Relaxed);
    let dbg = DEBUG.load(Relaxed) != 0;
    let name = || crate::types::str::to_string_lossy(&cd.co_qualname);
    let tables = cc.tables(&cd.code);
    let an = match super::analysis::analyze(&cd.code, &cd.exception_table) {
        Ok(a) => a,
        Err(e) => {
            if dbg {
                eprintln!("[jit] tier2 analysis failed for {}: {}", name(), e);
            }
            return None;
        }
    };
    let entry = an.index_of.get(entry_pc).copied().unwrap_or(u32::MAX) as usize;
    if entry >= an.instrs.len() || an.depth[entry] < 0 {
        return None;
    }
    let co = crate::types::code::code(code).obj;
    let mut nargs = co.co_argcount as usize + co.co_kwonlyargcount as usize;
    use rpy_compiler::bytecode::co as cof;
    if flags & cof::VARARGS != 0 {
        nargs += 1;
    }
    if flags & cof::VARKEYWORDS != 0 {
        nargs += 1;
    }
    let invalid = Box::new(AtomicU8::new(0));
    let deopts = Box::new(AtomicU32::new(0));
    let unit = build::Unit {
        cd,
        an: &an,
        tables,
        nlp: co.co_nlocalsplus as usize,
        entry,
        nargs,
        globals,
        builtins,
        invalid_flag: &*invalid as *const AtomicU8 as usize,
        flags,
        fold_globals: cc.t2_invalidations.load(Relaxed) < 3,
    };
    let built = match build::build(&unit) {
        Ok(b) => b,
        Err(e) => {
            if dbg {
                eprintln!("[jit] tier2 build failed for {}: {}", name(), e);
            }
            return None;
        }
    };
    let alloc = regalloc::allocate(&built.func);
    if std::env::var("RPYTHON_JIT2_IR").is_ok_and(|v| v == name() || v == "*") {
        eprintln!("[jit] tier2 IR {} @{} reps {:?}", name(), entry_pc, built.reps);
        for &b in &built.func.order {
            let blk = &built.func.blocks[b as usize];
            eprintln!("  b{} (instr {}):", b, blk.instr as i32);
            for ins in &blk.insts {
                eprintln!("    {:?}", ins);
            }
            eprintln!("    -> {:?}", blk.term);
        }
        for (k, s) in built.func.snaps.iter().enumerate() {
            eprintln!("  snap{} {:?}", k, s);
        }
        for (v, l) in alloc.loc.iter().enumerate() {
            eprintln!("  v{} {:?} {:?}", v, built.func.classes[v], l);
        }
    }
    let out = match codegen::codegen(&unit, &built, &alloc, &*deopts as *const AtomicU32 as usize) {
        Ok(o) => o,
        Err(e) => {
            if dbg {
                eprintln!("[jit] tier2 codegen failed for {}: {}", name(), e);
            }
            return None;
        }
    };
    let asm = &out.asm;
    let addr = super::mem::install(&asm.buf, |buf, base| {
        let _ = asm.patch_abs(buf, base);
    })?;
    if let Ok(dir) = std::env::var("RPYTHON_JIT_DUMP") {
        let _ = std::fs::write(format!("{}/{}.t2.{:x}.bin", dir, name(), addr), &asm.buf);
    }
    let entries: Vec<(u32, usize)> = out.entries.iter().map(|&(pc, off)| (pc, addr + off as usize)).collect();
    let target = entries.iter().find(|e| e.0 as usize == entry_pc).map(|e| e.1)?;
    if dbg {
        eprintln!(
            "[jit] tier2 {} @{} ({} instrs -> {} bytes, {} blocks, {} vregs, frame {})",
            name(),
            entry_pc,
            an.instrs.len(),
            asm.buf.len(),
            built.func.blocks.len(),
            built.func.classes.len(),
            alloc.frame
        );
    }
    if built.has_deps && unit.fold_globals {
        watch(globals, cc);
        watch(builtins, cc);
    }
    let oc = Box::into_raw(Box::new(OptCode { addr, len: asm.buf.len(), entries, invalid, deopts }));
    let old = slot.swap(oc, AcqRel);
    if !old.is_null() {
        unsafe { (*old).invalid.store(1, Release) };
        cc.t2_retired.lock().unwrap_or_else(|e| e.into_inner()).push(old as usize);
    }
    if entry_pc == 0 && cc.fast_argc_ok.load(Relaxed) {
        cc.fast_entry.store(target, Release);
    }
    Some(target)
}

/// Tier-1 backedge counter expired at the Jump to `header` (code unit index of the
/// loop header). Returns the address of a tier-2 OSR entry to jump to, or 0.
pub unsafe extern "C" fn h_backedge(f: *mut crate::interp::frame::Frame, header: u64) -> usize {
    unsafe {
        let f = &mut *f;
        let cd = crate::types::code::code_data(f.code());
        let cc = &cd.caches;
        cc.t2_backedge.store(i32::MAX, Relaxed);
        if !enabled() {
            return 0;
        }
        let code = f.code().to_ref();
        // standard unit for later calls, OSR unit for this activation
        if cc.opt.load(Acquire).is_null() && cd.invocation_count.load(Relaxed) > 1 {
            compile(&code, f.globals, f.builtins, 0);
        }
        compile(&code, f.globals, f.builtins, header as usize).unwrap_or(0)
    }
}

/// Status DEOPT came back from code entered at `pc`: count it against the tier-2
/// code and discard that code if it keeps failing.
#[inline]
pub fn note_deopt(cc: &CodeCaches) {
    for p in [&cc.opt, &cc.osr] {
        let o = p.load(Acquire);
        if !o.is_null() {
            let n = unsafe { (*o).deopts.load(Relaxed) };
            if n > DEOPT_LIMIT {
                if DEBUG.load(Relaxed) != 0 {
                    eprintln!("[jit] tier2 code discarded after {} deopts", n);
                }
                invalidate(cc);
                return;
            }
        }
    }
}
