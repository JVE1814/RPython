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
    /// globals/builtins are the function's (borrowed: the frame owns `func`, whose
    /// __globals__/__builtins__ cannot be rebound), so no refcount traffic per call.
    pub borrowed_gb: bool,
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
    pub fn new(code: &PyObject, func: Option<&PyObject>, globals: &PyObject, builtins: &PyObject, locals: Option<&PyObject>) -> Box<Frame> {
        let cd = crate::types::code::code(code);
        let nlp = cd.obj.co_nlocalsplus as usize;
        let total = nlp + cd.obj.co_stacksize as usize + 1;
        if let Some(mut f) = pool_pop() {
            f.slots.clear();
            f.slots.resize(total, std::ptr::null_mut());
            f.code = code.to_ref().into_raw();
            f.func = func.map_or(std::ptr::null_mut(), |f| f.to_ref().into_raw());
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
        self.clear_locals();
        unsafe {
            xdecref(self.code);
            xdecref(self.func);
            if !self.borrowed_gb {
                xdecref(self.globals);
                xdecref(self.builtins);
            }
            xdecref(self.locals);
            xdecref(self.f_trace);
        }
        self.code = std::ptr::null_mut();
        self.func = std::ptr::null_mut();
        self.globals = std::ptr::null_mut();
        self.builtins = std::ptr::null_mut();
        self.locals = std::ptr::null_mut();
        self.f_trace = std::ptr::null_mut();
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.release();
    }
}

// ------------------------------------------------------------------ frame pool
//
// Finished frames are kept in a per-thread pool (`#[thread_local]`: direct fs-relative
// access, no lazy-init check, no RefCell) so a call reuses the Box and the slot
// storage. A thread_local! guard frees the pooled frames at thread exit.

const POOL_MAX: usize = 64;

struct Pool {
    n: usize,
    registered: bool,
    dead: bool,
    items: [*mut Frame; POOL_MAX],
}

#[thread_local]
static mut FPOOL: Pool = Pool { n: 0, registered: false, dead: false, items: [std::ptr::null_mut(); POOL_MAX] };

struct PoolGuard;
impl Drop for PoolGuard {
    fn drop(&mut self) {
        unsafe {
            let p = &mut *(&raw mut FPOOL);
            p.dead = true;
            while p.n > 0 {
                p.n -= 1;
                drop(Box::from_raw(p.items[p.n]));
            }
        }
    }
}

thread_local! {
    static PGUARD: PoolGuard = const { PoolGuard };
}

#[inline(always)]
fn pool_pop() -> Option<Box<Frame>> {
    unsafe {
        let p = &mut *(&raw mut FPOOL);
        if p.n == 0 {
            return None;
        }
        p.n -= 1;
        Some(Box::from_raw(p.items[p.n]))
    }
}

#[cold]
fn pool_register(p: &mut Pool) -> bool {
    if PGUARD.try_with(|_| ()).is_err() {
        p.dead = true;
        return false;
    }
    p.registered = true;
    true
}

/// Return a finished frame to the per-thread pool.
#[inline]
pub fn recycle(mut f: Box<Frame>) {
    f.release();
    if f.slots.capacity() > 4096 {
        return;
    }
    unsafe {
        let p = &mut *(&raw mut FPOOL);
        if p.dead || p.n >= POOL_MAX || (!p.registered && !pool_register(p)) {
            return;
        }
        p.items[p.n] = Box::into_raw(f);
        p.n += 1;
    }
}

/// Release a completed call frame made by `alloc_call` (no frame object, no locals
/// mapping) and return it to the pool.
#[inline]
pub fn recycle_call(mut f: Box<Frame>) {
    unsafe {
        let s = f.slots.as_mut_ptr();
        for i in 0..f.stack_top {
            let p = *s.add(i);
            if !p.is_null() {
                *s.add(i) = std::ptr::null_mut();
                decref(p);
            }
        }
        f.stack_top = f.nlocalsplus;
        decref(f.code);
        decref(f.func);
        f.code = std::ptr::null_mut();
        f.func = std::ptr::null_mut();
        f.globals = std::ptr::null_mut();
        f.builtins = std::ptr::null_mut();
        if !f.f_trace.is_null() {
            xdecref(std::mem::replace(&mut f.f_trace, std::ptr::null_mut()));
        }
        if !f.locals.is_null() {
            xdecref(std::mem::replace(&mut f.locals, std::ptr::null_mut()));
        }
        if f.slots.capacity() > 4096 {
            return;
        }
        let p = &mut *(&raw mut FPOOL);
        if p.dead || p.n >= POOL_MAX || (!p.registered && !pool_register(p)) {
            return;
        }
        p.items[p.n] = Box::into_raw(f);
        p.n += 1;
    }
}

/// Frame for a call of an optimized Python function (jit fast call path).
/// Takes ownership of one reference to `func`; increfs `code`. Locals are NULL (the
/// caller stores the arguments); globals/builtins are the function's (borrowed).
#[inline]
pub unsafe fn alloc_call(code: *mut PyObject, func: *mut PyObject, globals: *mut PyObject, builtins: *mut PyObject, nlp: usize, total: usize) -> Box<Frame> {
    unsafe {
        incref(code);
        let mut f = match pool_pop() {
            Some(f) => f,
            None => return alloc_call_slow(code, func, globals, builtins, nlp, total),
        };
        if f.slots.capacity() < total {
            f.slots.reserve(total - f.slots.len());
        }
        // the value-stack part is written before it is read (stack_top tracks it)
        f.slots.set_len(total);
        let s = f.slots.as_mut_ptr();
        // (volatile: a call to memset costs more than these few stores)
        for i in 0..nlp {
            std::ptr::write_volatile(s.add(i), std::ptr::null_mut());
        }
        f.code = code;
        f.func = func;
        f.borrowed_gb = true;
        f.globals = globals;
        f.builtins = builtins;
        f.locals = std::ptr::null_mut();
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
        f
    }
}

#[cold]
unsafe fn alloc_call_slow(code: *mut PyObject, func: *mut PyObject, globals: *mut PyObject, builtins: *mut PyObject, nlp: usize, total: usize) -> Box<Frame> {
    Box::new(Frame {
        code,
        func,
        globals,
        builtins,
        borrowed_gb: true,
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
    })
}
