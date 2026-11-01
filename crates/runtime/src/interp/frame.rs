//! Interpreter frames (tier 0). A frame owns its code/globals/builtins/locals
//! references and a contiguous `localsplus + value stack` array of raw owned
//! pointers (NULL = unbound / null marker).

use crate::ffi::PyObject;
use crate::object::*;

pub struct Frame {
    pub code: *mut PyObject,
    pub func: *mut PyObject,
    pub globals: *mut PyObject,
    pub builtins: *mut PyObject,
    /// Locals mapping for module/class/exec frames (NULL for optimized functions).
    pub locals: *mut PyObject,
    pub prev: *mut Frame,
    /// Index of the instruction currently executing (or last executed).
    pub lasti: u32,
    /// Index of the next instruction to execute (saved on suspension).
    pub next_instr: u32,
    pub nlocalsplus: usize,
    /// localsplus followed by the value stack.
    pub slots: Vec<*mut PyObject>,
    pub stack_top: usize,
    /// Lazily created PyFrameObject (borrowed back-pointer; the frame object owns itself).
    pub frame_obj: *mut PyObject,
    /// Generator/coroutine owning this frame (borrowed), if any.
    pub generator: *mut PyObject,
    pub state: FrameState,
    /// Line of the last 'line' trace event (for sys.settrace).
    pub trace_line: i32,
    /// Per-frame f_trace callable (owned).
    pub f_trace: *mut PyObject,
    pub f_trace_lines: bool,
    pub f_trace_opcodes: bool,
    /// Index of the `Send` instruction if suspended inside a yield-from/await loop, else -1.
    pub yield_from_send: i32,
    /// Instruction index to use for the next exception-table lookup (set by Reraise), or -1.
    pub unwind_pc: i32,
    /// Trace event being delivered to this frame's trace function (0 = none; see
    /// interp::jump::EV_*), so that f_lineno assignments can be validated.
    pub trace_event: u8,
    /// Instruction to continue at after a trace function set f_lineno, or -1.
    pub jump_to: i32,
    /// globals/builtins are the function's (borrowed: the frame owns `func`, whose
    /// __globals__/__builtins__ cannot be rebound), so no refcount traffic per call.
    pub borrowed_gb: bool,
    /// `code` and `func` are owned references. False for plain call frames
    /// (`new_call`): the caller keeps the function alive for the duration of the call
    /// and a replaced `__code__` is retired, not freed (funcobject.rs). Frames that
    /// outlive their call (generators, frame objects) are switched to owned first
    /// (`make_owned`).
    pub owns_fc: bool,
}

fn borrows_globals(func: Option<&PyObject>, globals: &PyObject, builtins: &PyObject) -> bool {
    match func {
        Some(fo) if crate::types::funcobject::is_function(fo) => {
            let ff = crate::types::funcobject::func(fo);
            ff.func_globals == globals.as_ptr() && ff.func_builtins == builtins.as_ptr()
        }
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameState {
    Created,
    Executing,
    Suspended,
    Completed,
    Cleared,
}

impl Frame {
    /// Frame for a plain call of Python function `func` (code/globals/builtins are the
    /// function's): `code` and `func` are borrowed (see `owns_fc`). Recycled frames
    /// come from a per-thread pool.
    #[inline]
    pub fn new_call(code: &PyObject, func: &PyObject, globals: &PyObject, builtins: &PyObject) -> Box<Frame> {
        let cd = crate::types::code::code(code);
        let nlp = cd.obj.co_nlocalsplus as usize;
        let total = nlp + cd.obj.co_stacksize as usize + 1;
        unsafe { Self::new_call_raw(code.as_ptr(), func.as_ptr(), globals.as_ptr(), builtins.as_ptr(), nlp, total) }
    }

    /// `new_call` with the frame geometry precomputed (the JIT's call path caches
    /// `nlp`/`total` per code object). Locals are NULL; the value-stack part above
    /// `stack_top` is not cleared (every stack slot is written before it is read).
    ///
    /// Safety: `func` must stay alive for the call (the caller holds it; it holds
    /// code/globals/builtins); `nlp`/`total` must match `code`.
    #[inline]
    pub unsafe fn new_call_raw(code: *mut PyObject, func: *mut PyObject, globals: *mut PyObject, builtins: *mut PyObject, nlp: usize, total: usize) -> Box<Frame> {
        unsafe {
            match (*(&raw mut POOL)).pop() {
                Some(mut f) => {
                    if f.slots.capacity() < total {
                        f.slots.reserve(total - f.slots.len());
                    }
                    f.slots.set_len(total);
                    let s = f.slots.as_mut_ptr();
                    // (volatile: a few stores are cheaper than the memset call the
                    // compiler would otherwise emit)
                    for i in 0..nlp {
                        std::ptr::write_volatile(s.add(i), std::ptr::null_mut());
                    }
                    f.code = code;
                    f.func = func;
                    f.owns_fc = false;
                    f.borrowed_gb = true;
                    f.globals = globals;
                    f.builtins = builtins;
                    f.nlocalsplus = nlp;
                    f.stack_top = nlp;
                    // everything else was reset by `release` + `reset`
                    f
                }
                None => Self::new_call_slow(code, func, globals, builtins, nlp, total),
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn new_call_slow(code: *mut PyObject, func: *mut PyObject, globals: *mut PyObject, builtins: *mut PyObject, nlp: usize, total: usize) -> Box<Frame> {
        Box::new(Frame {
            code,
            func,
            globals,
            builtins,
            borrowed_gb: true,
            owns_fc: false,
            locals: std::ptr::null_mut(),
            prev: std::ptr::null_mut(),
            lasti: 0,
            next_instr: 0,
            nlocalsplus: nlp,
            slots: vec![std::ptr::null_mut(); total],
            stack_top: nlp,
            frame_obj: std::ptr::null_mut(),
            generator: std::ptr::null_mut(),
            state: FrameState::Created,
            trace_line: -1,
            f_trace: std::ptr::null_mut(),
            f_trace_lines: true,
            f_trace_opcodes: false,
            yield_from_send: -1,
            unwind_pc: -1,
            trace_event: 0,
            jump_to: -1,
        })
    }

    /// Take owned references to `code` and `func` (the frame is about to outlive the
    /// call that created it: generator, frame object).
    #[inline]
    pub fn make_owned(&mut self) {
        if !self.owns_fc {
            unsafe {
                if !self.code.is_null() {
                    incref(self.code);
                }
                if !self.func.is_null() {
                    incref(self.func);
                }
            }
            self.owns_fc = true;
        }
    }

    pub fn new(code: &PyObject, func: Option<&PyObject>, globals: &PyObject, builtins: &PyObject, locals: Option<&PyObject>) -> Box<Frame> {
        let cd = crate::types::code::code(code);
        let nlp = cd.obj.co_nlocalsplus as usize;
        let total = nlp + cd.obj.co_stacksize as usize + 1;
        if let Some(mut f) = unsafe { (*(&raw mut POOL)).pop() } {
            unsafe { f.slots.set_len(0) };
            f.slots.resize(total, std::ptr::null_mut());
            f.code = code.to_ref().into_raw();
            f.func = func.map_or(std::ptr::null_mut(), |f| f.to_ref().into_raw());
            f.owns_fc = true;
            f.borrowed_gb = borrows_globals(func, globals, builtins);
            f.globals = if f.borrowed_gb { globals.as_ptr() } else { globals.to_ref().into_raw() };
            f.builtins = if f.borrowed_gb { builtins.as_ptr() } else { builtins.to_ref().into_raw() };
            f.locals = locals.map_or(std::ptr::null_mut(), |l| l.to_ref().into_raw());
            f.prev = std::ptr::null_mut();
            f.lasti = 0;
            f.next_instr = 0;
            f.nlocalsplus = nlp;
            f.stack_top = nlp;
            f.frame_obj = std::ptr::null_mut();
            f.generator = std::ptr::null_mut();
            f.state = FrameState::Created;
            f.trace_line = -1;
            f.f_trace = std::ptr::null_mut();
            f.f_trace_lines = true;
            f.f_trace_opcodes = false;
            f.yield_from_send = -1;
            f.unwind_pc = -1;
            f.trace_event = 0;
            f.jump_to = -1;
            return f;
        }
        let slots = vec![std::ptr::null_mut::<PyObject>(); total];
        let borrowed_gb = borrows_globals(func, globals, builtins);
        Box::new(Frame {
            code: code.to_ref().into_raw(),
            func: func.map_or(std::ptr::null_mut(), |f| f.to_ref().into_raw()),
            globals: if borrowed_gb { globals.as_ptr() } else { globals.to_ref().into_raw() },
            builtins: if borrowed_gb { builtins.as_ptr() } else { builtins.to_ref().into_raw() },
            borrowed_gb,
            owns_fc: true,
            locals: locals.map_or(std::ptr::null_mut(), |l| l.to_ref().into_raw()),
            prev: std::ptr::null_mut(),
            lasti: 0,
            next_instr: 0,
            nlocalsplus: nlp,
            slots,
            stack_top: nlp,
            frame_obj: std::ptr::null_mut(),
            generator: std::ptr::null_mut(),
            state: FrameState::Created,
            trace_line: -1,
            f_trace: std::ptr::null_mut(),
            f_trace_lines: true,
            f_trace_opcodes: false,
            yield_from_send: -1,
            unwind_pc: -1,
            trace_event: 0,
            jump_to: -1,
        })
    }

    #[inline(always)]
    pub fn code(&self) -> &PyObject {
        unsafe { &*self.code }
    }
    #[inline(always)]
    pub fn globals(&self) -> &PyObject {
        unsafe { &*self.globals }
    }
    #[inline(always)]
    pub fn builtins(&self) -> &PyObject {
        unsafe { &*self.builtins }
    }

    /// Release the value stack (used when a frame finishes or is cleared).
    pub fn clear_stack(&mut self) {
        while self.stack_top > self.nlocalsplus {
            self.stack_top -= 1;
            let p = std::mem::replace(&mut self.slots[self.stack_top], std::ptr::null_mut());
            unsafe { xdecref(p) };
        }
    }

    /// Release locals and stack (frame.clear()).
    pub fn clear_locals(&mut self) {
        self.clear_stack();
        for i in 0..self.nlocalsplus {
            let p = std::mem::replace(&mut self.slots[i], std::ptr::null_mut());
            unsafe { xdecref(p) };
        }
    }

    /// Current line number.
    pub fn lineno(&self) -> i32 {
        crate::types::code::line_for_instr(self.code(), self.lasti as usize).map(|l| l as i32).unwrap_or(-1)
    }
}
impl Frame {
    /// Release all references held by the frame (idempotent).
    pub fn release(&mut self) {
        if self.code.is_null() {
            return;
        }
        if !self.frame_obj.is_null() {
            unsafe {
                let fo = self.frame_obj as *mut crate::types::frameobject::PyFrameObject;
                (*fo).frame = std::ptr::null_mut();
                self.frame_obj = std::ptr::null_mut();
                decref(fo as *mut PyObject);
            }
        }
        // locals and value stack in one pass (perf-runtime)
        unsafe {
            let p = self.slots.as_mut_ptr();
            let top = self.stack_top.max(self.nlocalsplus).min(self.slots.len());
            for i in 0..top {
                let o = *p.add(i);
                if !o.is_null() {
                    *p.add(i) = std::ptr::null_mut();
                    decref(o);
                }
            }
        }
        self.stack_top = self.nlocalsplus;
        unsafe {
            if self.owns_fc {
                xdecref(self.code);
                xdecref(self.func);
            }
            if !self.borrowed_gb {
                xdecref(self.globals);
                xdecref(self.builtins);
            }
            if !self.locals.is_null() {
                decref(self.locals);
            }
            if !self.f_trace.is_null() {
                decref(self.f_trace);
            }
        }
        self.code = std::ptr::null_mut();
        self.func = std::ptr::null_mut();
        self.globals = std::ptr::null_mut();
        self.builtins = std::ptr::null_mut();
        self.locals = std::ptr::null_mut();
        self.f_trace = std::ptr::null_mut();
    }

    /// Reset the bookkeeping fields of a released frame for reuse (`new_call` relies
    /// on this).
    #[inline]
    fn reset(&mut self) {
        self.prev = std::ptr::null_mut();
        self.lasti = 0;
        self.next_instr = 0;
        self.frame_obj = std::ptr::null_mut();
        self.generator = std::ptr::null_mut();
        self.state = FrameState::Created;
        self.trace_line = -1;
        self.f_trace_lines = true;
        self.f_trace_opcodes = false;
        self.yield_from_send = -1;
        self.unwind_pc = -1;
        self.trace_event = 0;
        self.jump_to = -1;
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.release();
    }
}

/// Per-thread pool of released frames (plain `#[thread_local]`: no lazy-init check
/// or RefCell on the call path). Drained by `drain_pool` when the thread exits.
#[thread_local]
static mut POOL: Vec<Box<Frame>> = Vec::new();
const POOL_MAX: usize = 128;

/// Return a finished frame to the per-thread pool.
#[inline]
pub fn recycle(mut f: Box<Frame>) {
    f.release();
    if f.slots.capacity() > 4096 {
        return;
    }
    unsafe {
        let pool = &mut *(&raw mut POOL);
        if pool.len() < POOL_MAX {
            f.reset();
            pool.push(f);
        }
    }
}

/// Free the calling thread's pooled frames (thread exit).
pub fn drain_pool() {
    unsafe {
        let pool = std::mem::take(&mut *(&raw mut POOL));
        drop(pool);
    }
}
