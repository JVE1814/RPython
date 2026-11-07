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
    pub const YIELD_FROM_SEND: i32 = offset_of!(Frame, yield_from_send) as i32;
    pub const RESUME: i32 = offset_of!(Frame, resume) as i32;
    pub const T2_SITE: i32 = offset_of!(Frame, t2_site) as i32;
    pub const T2_SP: i32 = offset_of!(Frame, t2_sp) as i32;
    pub const FRAME_OBJ: i32 = offset_of!(Frame, frame_obj) as i32;
    pub const TP_TAG: i32 = offset_of!(PyTypeObject, tp_version_tag) as i32;
    pub const MANAGED_DICT: i32 = -(crate::alloc::MANAGED as i32);
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
    /// descriptors referenced by the code (see `Site`)
    pub sites: Vec<Box<Site>>,
    /// per-site memo cells referenced by the code
    pub cells: Vec<usize>,
}

/// Free memo cells made by the builder (`Box<AtomicUsize>` raw pointers).
pub fn free_cells(cells: &[usize]) {
    for &c in cells {
        unsafe { drop(Box::from_raw(c as *mut std::sync::atomic::AtomicUsize)) };
    }
}

/// Where the unboxed values of locals live while compiled code runs a template or a
/// call (stored in `Frame::t2_site` for its duration): frame introspection
/// (`f_locals`, `locals()`) boxes them into the frame slots first (`sync_frame`).
pub struct Site {
    pub entries: Vec<SiteEntry>,
}

#[derive(Clone, Copy, Debug)]
pub struct SiteEntry {
    pub slot: u32,
    pub float: bool,
    /// the local may be unbound: skip it when its frame slot is NULL
    pub maybe: bool,
    /// offset of the raw value from `Frame::t2_sp`; -1: no value (never assigned)
    pub off: i32,
}

/// Refresh the frame slots of unboxed locals of `f` if compiled code is suspended
/// in a call with stale slots. Afterwards the compiled code deoptimises when the
/// call returns (writes to the slots, e.g. through `f_locals`, must be seen).
pub fn sync_frame(f: *mut crate::interp::frame::Frame) {
    let f = unsafe { &mut *f };
    let s = f.t2_site;
    if s <= 1 {
        return;
    }
    let site = unsafe { &*(s as *const Site) };
    for e in &site.entries {
        if e.off < 0 {
            continue;
        }
        let i = e.slot as usize;
        if e.maybe && f.slots[i].is_null() {
            continue;
        }
        let raw = unsafe { *((f.t2_sp as isize + e.off as isize) as *const u64) };
        let o = unsafe {
            if e.float { crate::interp::jit::helpers::h_new_float(f64::from_bits(raw)) } else { crate::interp::jit::helpers::h_new_int(raw as i64) }
        };
        if o.is_null() {
            crate::errors::clear_exc();
            continue;
        }
        let old = std::mem::replace(&mut f.slots[i], o);
        unsafe { crate::object::xdecref(old) };
    }
    f.t2_site = 1;
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
/// RPYTHON_JIT_DEBUG level (3: also log inlining decisions).
pub fn debug_level() -> u8 {
    DEBUG.load(Relaxed)
}
/// deoptimisations after which code is discarded
const DEOPT_LIMIT: u32 = 64;
/// compilations per code object
const MAX_TRIES: u8 = 4;

#[cold]
fn init() -> bool {
    // on by default; RPYTHON_JIT2=0 disables tier 2 (RPYTHON_JIT=0 disables both tiers)
    let en = std::env::var("RPYTHON_JIT2").map(|v| v != "0").unwrap_or(true);
    if let Ok(t) = std::env::var("RPYTHON_JIT2_THRESHOLD") {
        if let Ok(n) = t.parse::<u32>() {
            CALLS.store(n.max(1), Relaxed);
            BACKEDGES.store((n * 10).max(1), Relaxed);
        }
    }
    if let Some(n) = std::env::var("RPYTHON_JIT_DEBUG").ok().and_then(|v| v.parse::<u8>().ok()) {
        DEBUG.store(n, Relaxed);
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

/// Tier 2 compiles generator/coroutine code (RPYTHON_JIT2_GEN=0 disables it).
fn gen_enabled() -> bool {
    static GEN: AtomicU8 = AtomicU8::new(2);
    match GEN.load(Relaxed) {
        2 => {
            let en = std::env::var("RPYTHON_JIT2_GEN").map(|v| v != "0").unwrap_or(true);
            GEN.store(en as u8, Relaxed);
            en
        }
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

/// Dependencies of tier-2 code on dict contents: watched dict -> [(key, dependent
/// code caches)]. The key is an exact str (borrowed: the dependent code object's
/// `co_names` keeps it alive, and `forget` runs before the code object dies). A
/// dependency means "the lookup of `key` in this dict gives the value the code was
/// compiled with": any write, insertion or deletion of that key, or a mutation that
/// may touch any key (clear, merge), invalidates the dependent code. Writes of other
/// keys do not (no invalidation storms from module-level loops assigning globals).
type DepMap = std::collections::HashMap<usize, Vec<(usize, usize)>>;
static DEPS: Mutex<Option<DepMap>> = Mutex::new(None);

/// Marker in a dict's `_ma_watcher_tag` (type pointers are aligned, so bit 0 is free).
pub const WATCH_TAG: u64 = 1;

fn deps() -> std::sync::MutexGuard<'static, Option<DepMap>> {
    DEPS.lock().unwrap_or_else(|e| e.into_inner())
}

fn watch(d: *mut PyObject, key: *mut PyObject, cc: &CodeCaches) {
    let mut g = deps();
    unsafe {
        let dp = d as *mut PyDictObject;
        let t = (*dp)._ma_watcher_tag;
        if t != 0 && t != WATCH_TAG {
            return; // a type dict: changes are covered by type tags
        }
        (*dp)._ma_watcher_tag = WATCH_TAG;
    }
    let v = g.get_or_insert_with(Default::default).entry(d as usize).or_default();
    let k = (key as usize, cc as *const _ as usize);
    if !v.contains(&k) {
        v.push(k);
    }
}

/// Same key? Keys are exact strs; writers may use another (non-interned) str object.
fn same_key(dep: usize, key: *mut PyObject) -> bool {
    if dep == key as usize {
        return true;
    }
    unsafe {
        let k = &*key;
        crate::types::str::is_str_exact(k) && crate::types::str::str_eq(&*(dep as *mut PyObject), k)
    }
}

/// (dict, key hash) pairs whose folding was invalidated once: such a global is
/// rebound while the program runs (go's `TIMESTAMP`), so folding it again would only
/// cause more invalidations and recompilations. Builders read them through the
/// generic path instead. A hash collision merely costs a fold.
static UNSTABLE: Mutex<Option<std::collections::HashSet<(usize, isize)>>> = Mutex::new(None);

fn key_hash(key: *mut PyObject) -> Option<isize> {
    unsafe {
        let k = &*key;
        if crate::types::str::is_str_exact(k) { Some(crate::types::str::str_hash(k)) } else { None }
    }
}

/// Has a fold of `key` in dict `d` been invalidated before?
pub fn unstable_key(d: usize, key: *mut PyObject) -> bool {
    let Some(h) = key_hash(key) else { return true };
    let g = UNSTABLE.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().is_some_and(|s| s.contains(&(d, h)))
}

/// A watched dict was written (called from cache::on_dict_write); `key` NULL: any key
/// may have changed.
#[cold]
pub fn dict_written(d: &PyObject, key: *mut PyObject) {
    let dk = d.as_ptr() as usize;
    let victims: Vec<usize> = {
        let mut g = deps();
        let Some(m) = g.as_mut() else { return };
        let Some(v) = m.get_mut(&dk) else { return };
        let mut out = Vec::new();
        if !key.is_null() && v.iter().any(|&(k, _)| same_key(k, key)) {
            if let Some(h) = key_hash(key) {
                UNSTABLE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert((dk, h));
            }
        }
        v.retain(|&(k, cc)| {
            if key.is_null() || same_key(k, key) {
                out.push(cc);
                false
            } else {
                true
            }
        });
        if v.is_empty() {
            m.remove(&dk);
            unsafe { (*(d.as_ptr() as *mut PyDictObject))._ma_watcher_tag = 0 };
        }
        out
    };
    for cc in victims {
        invalidate(unsafe { &*(cc as *const CodeCaches) });
    }
}

/// Do the folded lookups still give the values the code was compiled with?
/// (dict, key, expected value or 0 for "absent")
fn deps_current(deps: &[(usize, usize, usize)]) -> bool {
    deps.iter().all(|&(d, k, v)| unsafe {
        let got = crate::types::dict::get_item_str(&*(d as *mut PyObject), &*(k as *mut PyObject));
        got.map_or(0, |o| o.as_ptr() as usize) == v
    })
}

/// The code caches are being destroyed: forget their dependencies.
pub fn forget(cc: &CodeCaches) {
    if cc.t2_tries.load(Relaxed) == 0 {
        return; // never compiled by tier 2: no dependencies
    }
    let k = cc as *const _ as usize;
    let mut g = deps();
    if let Some(m) = g.as_mut() {
        m.retain(|_, v| {
            v.retain(|e| e.1 != k);
            !v.is_empty()
        });
    }
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
    cc.t2_prof.store(1, Relaxed);
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
        free_cells(&b.cells);
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
    // generator code: the standard unit only (with an entry after every yield)
    let is_gen = flags & (cof::GENERATOR | cof::COROUTINE | cof::ASYNC_GENERATOR | cof::ITERABLE_COROUTINE) != 0;
    if is_gen && entry_pc != 0 {
        return None;
    }
    if !gen_enabled() && is_gen || flags & cof::OPTIMIZED == 0 {
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
        if entry_pc == 0 {
            // the standard unit exists (a generator whose eager prologue keeps pc 0
            // out of the unit's entries): nothing to recompile
            return None;
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
    let mut built = match build::build(&unit) {
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
            free_cells(&built.cells);
            return None;
        }
    };
    // register the dependencies before publishing, then re-check that the folded
    // values are still current (a write between the build and the registration
    // would otherwise go unnoticed)
    for &(d, k, _) in &built.deps {
        watch(d as *mut PyObject, k as *mut PyObject, cc);
    }
    if !deps_current(&built.deps) {
        if dbg {
            eprintln!("[jit] tier2 {}: a folded global changed during compilation", name());
        }
        free_cells(&built.cells);
        return None;
    }
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
    let len = asm.buf.len();
    let oc = Box::into_raw(Box::new(OptCode { addr, len, entries, invalid, deopts, sites: out.sites, cells: std::mem::take(&mut built.cells) }));
    let old = slot.swap(oc, AcqRel);
    if !old.is_null() {
        unsafe { (*old).invalid.store(1, Release) };
        cc.t2_retired.lock().unwrap_or_else(|e| e.into_inner()).push(old as usize);
    }
    if entry_pc == 0 && cc.fast_argc_ok.load(Relaxed) {
        cc.fast_entry.store(target, Release);
    }
    cc.t2_prof.store(0, Relaxed);
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
        // standard unit for later calls (a function with a hot loop may be called
        // only a few times), OSR unit for this activation
        if cc.opt.load(Acquire).is_null() {
            compile(&code, f.globals, f.builtins, 0);
        }
        if !f.generator.is_null() || crate::types::code::code(&code).flags() & rpy_compiler::bytecode::co::GENERATOR != 0 {
            // generator: no OSR units; the activation moves to tier 2 at its next
            // suspension (tier-1 YieldValue defers to installed tier-2 code)
            return 0;
        }
        compile(&code, f.globals, f.builtins, header as usize).unwrap_or(0)
    }
}

/// Status DEOPT came back from code entered at `pc`: count it against the tier-2
/// code and discard that code if it keeps failing.
#[inline]
pub fn note_deopt(cc: &CodeCaches, f: &crate::interp::frame::Frame) {
    if DEBUG.load(Relaxed) >= 2 {
        let cd = crate::types::code::code_data(f.code());
        eprintln!("[jit] tier2 deopt {} -> pc {}", crate::types::str::to_string_lossy(&cd.co_qualname), f.next_instr);
    }
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
