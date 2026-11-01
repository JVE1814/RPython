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
    pub slots: Box<[*mut PyObject]>,
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
        let slots = vec![std::ptr::null_mut::<PyObject>(); total].into_boxed_slice();
        Box::new(Frame {
            code: code.to_ref().into_raw(),
            func: func.map_or(std::ptr::null_mut(), |f| f.to_ref().into_raw()),
            globals: globals.to_ref().into_raw(),
            builtins: builtins.to_ref().into_raw(),
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

impl Drop for Frame {
    fn drop(&mut self) {
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
            xdecref(self.globals);
            xdecref(self.builtins);
            xdecref(self.locals);
            xdecref(self.f_trace);
        }
    }
}
