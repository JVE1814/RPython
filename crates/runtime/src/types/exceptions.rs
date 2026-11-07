//! Builtin exception hierarchy with CPython's object layouts.

#![allow(non_snake_case)]

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct BaseExc {
    pub ob_base: PyObject,
    pub dict: *mut PyObject,
    pub args: *mut PyObject,
    pub notes: *mut PyObject,
    pub traceback: *mut PyObject,
    pub context: *mut PyObject,
    pub cause: *mut PyObject,
    pub suppress_context: c_char,
}
#[repr(C)]
pub struct StopIterationExc {
    pub base: BaseExc,
    pub value: *mut PyObject,
}
#[repr(C)]
pub struct SystemExitExc {
    pub base: BaseExc,
    pub code: *mut PyObject,
}
#[repr(C)]
pub struct ImportErrorExc {
    pub base: BaseExc,
    pub msg: *mut PyObject,
    pub name: *mut PyObject,
    pub path: *mut PyObject,
    pub name_from: *mut PyObject,
}
#[repr(C)]
pub struct OSErrorExc {
    pub base: BaseExc,
    pub myerrno: *mut PyObject,
    pub strerror: *mut PyObject,
    pub filename: *mut PyObject,
    pub filename2: *mut PyObject,
    pub written: Py_ssize_t,
}
#[repr(C)]
pub struct SyntaxErrorExc {
    pub base: BaseExc,
    pub msg: *mut PyObject,
    pub filename: *mut PyObject,
    pub lineno: *mut PyObject,
    pub offset: *mut PyObject,
    pub end_lineno: *mut PyObject,
    pub end_offset: *mut PyObject,
    pub text: *mut PyObject,
    pub print_file_and_line: *mut PyObject,
    pub metadata: *mut PyObject,
}
#[repr(C)]
pub struct UnicodeErrorExc {
    pub base: BaseExc,
    pub encoding: *mut PyObject,
    pub object: *mut PyObject,
    pub start: Py_ssize_t,
    pub end: Py_ssize_t,
    pub reason: *mut PyObject,
}
#[repr(C)]
pub struct NameErrorExc {
    pub base: BaseExc,
    pub name: *mut PyObject,
}
#[repr(C)]
pub struct AttributeErrorExc {
    pub base: BaseExc,
    pub obj: *mut PyObject,
    pub name: *mut PyObject,
}
#[repr(C)]
pub struct ExcGroupExc {
    pub base: BaseExc,
    pub msg: *mut PyObject,
    pub excs: *mut PyObject,
    pub excs_str: *mut PyObject,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Base,
    StopIteration,
    SystemExit,
    ImportError,
    OSError,
    SyntaxError,
    UnicodeError,
    NameError,
    AttributeError,
    ExcGroup,
}

impl Layout {
    fn size(self) -> usize {
        match self {
            Layout::Base => std::mem::size_of::<BaseExc>(),
            Layout::StopIteration => std::mem::size_of::<StopIterationExc>(),
            Layout::SystemExit => std::mem::size_of::<SystemExitExc>(),
            Layout::ImportError => std::mem::size_of::<ImportErrorExc>(),
            Layout::OSError => std::mem::size_of::<OSErrorExc>(),
            Layout::SyntaxError => std::mem::size_of::<SyntaxErrorExc>(),
            Layout::UnicodeError => std::mem::size_of::<UnicodeErrorExc>(),
            Layout::NameError => std::mem::size_of::<NameErrorExc>(),
            Layout::AttributeError => std::mem::size_of::<AttributeErrorExc>(),
            Layout::ExcGroup => std::mem::size_of::<ExcGroupExc>(),
        }
    }
}

macro_rules! exc_table {
    ($( ($idx:ident, $sym:ident, $py:literal, $base:ident, $layout:ident, $doc:literal) ),* $(,)?) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy)]
        #[repr(usize)]
        enum Idx { $($idx,)* COUNT }
        const N: usize = Idx::COUNT as usize;
        static mut TYPES: [PyTypeObject; N] = [const { PyTypeObject::ZERO }; N];
        $(
            #[unsafe(no_mangle)]
            pub static mut $sym: *mut PyObject = std::ptr::null_mut();
            #[inline(always)]
            pub fn $idx() -> *mut PyTypeObject {
                unsafe { (&raw mut TYPES[Idx::$idx as usize]) as *mut PyTypeObject }
            }
        )*
        static TABLE: &[(Idx, &str, Option<Idx>, Layout, &str)] = &[
            $( (Idx::$idx, $py, exc_table!(@base $base), Layout::$layout, $doc), )*
        ];
        fn set_exported() {
            unsafe { $( $sym = $idx() as *mut PyObject; )* }
        }
    };
    (@base None) => { None };
    (@base $b:ident) => { Some(Idx::$b) };
}

exc_table! {
    (BaseException, PyExc_BaseException, "BaseException", None, Base, "Common base class for all exceptions"),
    (BaseExceptionGroup, PyExc_BaseExceptionGroup, "BaseExceptionGroup", BaseException, ExcGroup, "A combination of multiple unrelated exceptions."),
    (GeneratorExit, PyExc_GeneratorExit, "GeneratorExit", BaseException, Base, "Request that a generator exit."),
    (KeyboardInterrupt, PyExc_KeyboardInterrupt, "KeyboardInterrupt", BaseException, Base, "Program interrupted by user."),
    (SystemExit, PyExc_SystemExit, "SystemExit", BaseException, SystemExit, "Request to exit from the interpreter."),
    (Exception, PyExc_Exception, "Exception", BaseException, Base, "Common base class for all non-exit exceptions."),
    (ArithmeticError, PyExc_ArithmeticError, "ArithmeticError", Exception, Base, "Base class for arithmetic errors."),
    (FloatingPointError, PyExc_FloatingPointError, "FloatingPointError", ArithmeticError, Base, "Floating-point operation failed."),
    (OverflowError, PyExc_OverflowError, "OverflowError", ArithmeticError, Base, "Result too large to be represented."),
    (ZeroDivisionError, PyExc_ZeroDivisionError, "ZeroDivisionError", ArithmeticError, Base, "Second argument to a division or modulo operation was zero."),
    (AssertionError, PyExc_AssertionError, "AssertionError", Exception, Base, "Assertion failed."),
    (AttributeError, PyExc_AttributeError, "AttributeError", Exception, AttributeError, "Attribute not found."),
    (BufferError, PyExc_BufferError, "BufferError", Exception, Base, "Buffer error."),
    (EOFError, PyExc_EOFError, "EOFError", Exception, Base, "Read beyond end of file."),
    (ExceptionGroup, PyExc_ExceptionGroup, "ExceptionGroup", BaseExceptionGroup, ExcGroup, ""),
    (ImportError, PyExc_ImportError, "ImportError", Exception, ImportError, "Import can't find module, or can't find name in module."),
    (ModuleNotFoundError, PyExc_ModuleNotFoundError, "ModuleNotFoundError", ImportError, ImportError, "Module not found."),
    (ImportCycleError, PyExc_ImportCycleError, "ImportCycleError", ImportError, ImportError, "Import produces a cycle."),
    (LookupError, PyExc_LookupError, "LookupError", Exception, Base, "Base class for lookup errors."),
    (IndexError, PyExc_IndexError, "IndexError", LookupError, Base, "Sequence index out of range."),
    (KeyError, PyExc_KeyError, "KeyError", LookupError, Base, "Mapping key not found."),
    (MemoryError, PyExc_MemoryError, "MemoryError", Exception, Base, "Out of memory."),
    (NameError, PyExc_NameError, "NameError", Exception, NameError, "Name not found globally."),
    (UnboundLocalError, PyExc_UnboundLocalError, "UnboundLocalError", NameError, NameError, "Local name referenced but not bound to a value."),
    (OSError, PyExc_OSError, "OSError", Exception, OSError, "Base class for I/O related errors."),
    (BlockingIOError, PyExc_BlockingIOError, "BlockingIOError", OSError, OSError, "I/O operation would block."),
    (ChildProcessError, PyExc_ChildProcessError, "ChildProcessError", OSError, OSError, "Child process error."),
    (ConnectionError, PyExc_ConnectionError, "ConnectionError", OSError, OSError, "Connection error."),
    (BrokenPipeError, PyExc_BrokenPipeError, "BrokenPipeError", ConnectionError, OSError, "Broken pipe."),
    (ConnectionAbortedError, PyExc_ConnectionAbortedError, "ConnectionAbortedError", ConnectionError, OSError, "Connection aborted."),
    (ConnectionRefusedError, PyExc_ConnectionRefusedError, "ConnectionRefusedError", ConnectionError, OSError, "Connection refused."),
    (ConnectionResetError, PyExc_ConnectionResetError, "ConnectionResetError", ConnectionError, OSError, "Connection reset."),
    (FileExistsError, PyExc_FileExistsError, "FileExistsError", OSError, OSError, "File already exists."),
    (FileNotFoundError, PyExc_FileNotFoundError, "FileNotFoundError", OSError, OSError, "File not found."),
    (InterruptedError, PyExc_InterruptedError, "InterruptedError", OSError, OSError, "Interrupted by signal."),
    (IsADirectoryError, PyExc_IsADirectoryError, "IsADirectoryError", OSError, OSError, "Operation doesn't work on directories."),
    (NotADirectoryError, PyExc_NotADirectoryError, "NotADirectoryError", OSError, OSError, "Operation only works on directories."),
    (PermissionError, PyExc_PermissionError, "PermissionError", OSError, OSError, "Not enough permissions."),
    (ProcessLookupError, PyExc_ProcessLookupError, "ProcessLookupError", OSError, OSError, "Process not found."),
    (TimeoutError, PyExc_TimeoutError, "TimeoutError", OSError, OSError, "Timeout expired."),
    (ReferenceError, PyExc_ReferenceError, "ReferenceError", Exception, Base, "Weak ref proxy used after referent went away."),
    (RuntimeError, PyExc_RuntimeError, "RuntimeError", Exception, Base, "Unspecified run-time error."),
    (NotImplementedError, PyExc_NotImplementedError, "NotImplementedError", RuntimeError, Base, "Method or function hasn't been implemented yet."),
    (PythonFinalizationError, PyExc_PythonFinalizationError, "PythonFinalizationError", RuntimeError, Base, "Operation blocked during Python finalization."),
    (RecursionError, PyExc_RecursionError, "RecursionError", RuntimeError, Base, "Recursion limit exceeded."),
    (StopAsyncIteration, PyExc_StopAsyncIteration, "StopAsyncIteration", Exception, Base, "Signal the end from iterator.__anext__()."),
    (StopIteration, PyExc_StopIteration, "StopIteration", Exception, StopIteration, "Signal the end from iterator.__next__()."),
    (SyntaxError, PyExc_SyntaxError, "SyntaxError", Exception, SyntaxError, "Invalid syntax."),
    (IndentationError, PyExc_IndentationError, "IndentationError", SyntaxError, SyntaxError, "Improper indentation."),
    (TabError, PyExc_TabError, "TabError", IndentationError, SyntaxError, "Improper mixture of spaces and tabs."),
    (IncompleteInputError, PyExc_IncompleteInputError, "_IncompleteInputError", SyntaxError, SyntaxError, "incomplete input."),
    (SystemError, PyExc_SystemError, "SystemError", Exception, Base, "Internal error in the Python interpreter.\n\nPlease report this to the Python maintainer, along with the traceback,\nthe Python version, and the hardware/OS platform and version."),
    (TypeError, PyExc_TypeError, "TypeError", Exception, Base, "Inappropriate argument type."),
    (ValueError, PyExc_ValueError, "ValueError", Exception, Base, "Inappropriate argument value (of correct type)."),
    (UnicodeError, PyExc_UnicodeError, "UnicodeError", ValueError, Base, "Unicode related error."),
    (UnicodeDecodeError, PyExc_UnicodeDecodeError, "UnicodeDecodeError", UnicodeError, UnicodeError, "Unicode decoding error."),
    (UnicodeEncodeError, PyExc_UnicodeEncodeError, "UnicodeEncodeError", UnicodeError, UnicodeError, "Unicode encoding error."),
    (UnicodeTranslateError, PyExc_UnicodeTranslateError, "UnicodeTranslateError", UnicodeError, UnicodeError, "Unicode translation error."),
    (Warning, PyExc_Warning, "Warning", Exception, Base, "Base class for warning categories."),
    (BytesWarning, PyExc_BytesWarning, "BytesWarning", Warning, Base, "Base class for warnings about bytes and buffer related problems, mostly\nrelated to conversion from str or comparing to str."),
    (DeprecationWarning, PyExc_DeprecationWarning, "DeprecationWarning", Warning, Base, "Base class for warnings about deprecated features."),
    (EncodingWarning, PyExc_EncodingWarning, "EncodingWarning", Warning, Base, "Base class for warnings about encodings."),
    (FutureWarning, PyExc_FutureWarning, "FutureWarning", Warning, Base, "Base class for warnings about constructs that will change semantically\nin the future."),
    (ImportWarning, PyExc_ImportWarning, "ImportWarning", Warning, Base, "Base class for warnings about probable mistakes in module imports"),
    (PendingDeprecationWarning, PyExc_PendingDeprecationWarning, "PendingDeprecationWarning", Warning, Base, "Base class for warnings about features which will be deprecated\nin the future."),
    (ResourceWarning, PyExc_ResourceWarning, "ResourceWarning", Warning, Base, "Base class for warnings about resource usage."),
    (RuntimeWarning, PyExc_RuntimeWarning, "RuntimeWarning", Warning, Base, "Base class for warnings about dubious runtime behavior."),
    (SyntaxWarning, PyExc_SyntaxWarning, "SyntaxWarning", Warning, Base, "Base class for warnings about dubious syntax."),
    (UnicodeWarning, PyExc_UnicodeWarning, "UnicodeWarning", Warning, Base, "Base class for warnings about Unicode related problems, mostly\nrelated to conversion problems."),
    (UserWarning, PyExc_UserWarning, "UserWarning", Warning, Base, "Base class for warnings generated by user code."),
}

#[unsafe(no_mangle)]
pub static mut PyExc_EnvironmentError: *mut PyObject = std::ptr::null_mut();
#[unsafe(no_mangle)]
pub static mut PyExc_IOError: *mut PyObject = std::ptr::null_mut();

#[inline]
pub fn exc(o: &PyObject) -> &mut BaseExc {
    unsafe { &mut *(o.as_ptr() as *mut BaseExc) }
}
#[inline]
pub fn is_exception_instance(o: &PyObject) -> bool {
    o.ty().tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS != 0
}
#[inline]
pub fn is_exception_class(o: &PyObject) -> bool {
    is_type(o) && unsafe { (*as_type(o)).tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS != 0 }
}

/// When attribute lookup on *o* failed with an AttributeError that does not
/// say which attribute (e.g. a `__getattr__` hook raising a bare
/// AttributeError), record *name* and *o* on it, so that tracebacks can
/// still suggest similar names.
pub fn fill_attribute_error_context(o: &PyObject, name: &PyObject) {
    let p = current_exc_ptr();
    if p.is_null() {
        return;
    }
    let exc = unsafe { &*p };
    if !isinstance_exact_or_sub(exc, AttributeError()) {
        return;
    }
    let e = unsafe { &mut *(p as *mut AttributeErrorExc) };
    let unset = |f: *mut PyObject| f.is_null() || unsafe { (*f).is(none::none()) };
    if !unset(e.name) {
        return;
    }
    set_field(&mut e.name, Some(name));
    if unset(e.obj) {
        set_field(&mut e.obj, Some(o));
    }
}

/// Replace an exception field. Other threads may be reading it (getters take a new
/// reference without a lock), so the old value is released through QSBR.
fn set_field(slot: &mut *mut PyObject, v: Option<&PyObject>) {
    let new = v.map_or(std::ptr::null_mut(), |v| v.to_ref().into_raw());
    unsafe { crate::qsbr::swap_field(slot as *mut *mut PyObject, new) };
}
fn get_field(p: *mut PyObject) -> PyRef {
    unsafe { if p.is_null() { none::none_ref() } else { PyRef::from_borrowed(p) } }
}
fn get_field_opt(p: *mut PyObject) -> Option<PyRef> {
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}

/// Instantiate `tp(*args)` (fast path for builtin exception types).
pub fn make_exception(tp: *mut PyTypeObject, args: &[PyRef]) -> PyResult {
    unsafe {
        // (perf4) exception types with BaseException's own __new__/__init__ (KeyError,
        // IndexError, ...) under the plain `type` metaclass: what calling the type
        // would do, without the call machinery and argument vectors
        if (*tp).tp_new == Some(base_new as unsafe extern "C" fn(_, _, _) -> _)
            && (*tp).tp_init == Some(base_init as unsafe extern "C" fn(_, _, _) -> _)
            && (*(tp as *mut PyObject)).type_ptr() == &raw mut crate::types::typeobject::PyType_Type
        {
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            exc(&*op).args = match args.len() {
                0 => tuple::empty_tuple().into_raw(),
                _ => tuple::new_tuple(args.to_vec()).into_raw(),
            };
            return Ok(PyRef::from_raw(op));
        }
    }
    let refs: Vec<&PyObject> = args.iter().map(|a| &**a).collect();
    ab::call(type_obj(tp), &refs)
}

// ---------------------------------------------------------------- BaseException

unsafe extern "C" fn base_new(tp: *mut PyTypeObject, args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return op;
        }
        let e = exc(&*op);
        e.args = if args.is_null() { tuple::empty_tuple().into_raw() } else { (*args).to_ref().into_raw() };
        op
    }
}

unsafe extern "C" fn base_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        if !kw.is_null() && dict::dict_len(&*kw) > 0 {
            let _ = err!(TypeError, "{}() takes no keyword arguments", type_name((*op).ty()));
            return -1;
        }
        set_field(&mut exc(&*op).args, Some(&*args));
        0
    }
}

pub unsafe extern "C" fn base_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        base_clear(op);
        let tp = (*op).type_ptr();
        generic_free_and_type_decref(op, tp);
    }
}

unsafe extern "C" fn base_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let e = exc(&*op);
        for slot in [&mut e.dict, &mut e.args, &mut e.notes, &mut e.traceback, &mut e.context, &mut e.cause] {
            set_field(slot, None);
        }
        // subclass extra object fields
        let layout = layout_of((*op).type_ptr());
        let n = (layout.size() - std::mem::size_of::<BaseExc>()) / 8;
        let extra = (op as *mut u8).add(std::mem::size_of::<BaseExc>()) as *mut *mut PyObject;
        for i in 0..n {
            if layout == Layout::OSError && i == 4 {
                continue; // `written` is not an object
            }
            if layout == Layout::UnicodeError && (i == 2 || i == 3) {
                continue; // start/end
            }
            let p = extra.add(i);
            let old = *p;
            *p = std::ptr::null_mut();
            xdecref(old);
        }
        0
    }
}

unsafe extern "C" fn base_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let e = exc(&*op);
        let v = visit.unwrap();
        for p in [e.dict, e.args, e.notes, e.traceback, e.context, e.cause] {
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        let layout = layout_of((*op).type_ptr());
        let n = (layout.size() - std::mem::size_of::<BaseExc>()) / 8;
        let extra = (op as *mut u8).add(std::mem::size_of::<BaseExc>()) as *mut *mut PyObject;
        for i in 0..n {
            if (layout == Layout::OSError && i == 4) || (layout == Layout::UnicodeError && (i == 2 || i == 3)) {
                continue;
            }
            let p = *extra.add(i);
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

fn layout_of(tp: *mut PyTypeObject) -> Layout {
    // walk to the nearest builtin exception type
    let mut t = tp;
    unsafe {
        while !t.is_null() {
            let base_ptr = (&raw mut TYPES) as *mut PyTypeObject;
            if t >= base_ptr && t < base_ptr.add(N) {
                let i = t.offset_from(base_ptr) as usize;
                return TABLE[i].3;
            }
            t = (*t).tp_base;
        }
    }
    Layout::Base
}

pub fn exc_args(o: &PyObject) -> PyRef {
    let a = exc(o).args;
    if a.is_null() { tuple::empty_tuple() } else { unsafe { PyRef::from_borrowed(a) } }
}

unsafe extern "C" fn base_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = exc_args(&*op);
        match tuple::tuple_len(&a) {
            0 => pystr::new_str("").into_raw(),
            1 => into_c(ab::str(tuple::tuple_get(&a, 0))),
            _ => into_c(ab::str(&a)),
        }
    }
}

unsafe extern "C" fn base_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let a = exc_args(o);
        let nm = type_name(o.ty());
        let r = if tuple::tuple_len(&a) == 1 {
            ab::repr(tuple::tuple_get(&a, 0)).map(|r| format!("{}({})", nm, pystr::to_string_lossy(&r)))
        } else {
            ab::repr(&a).map(|r| {
                let s = pystr::to_string_lossy(&r);
                if tuple::tuple_len(&a) == 0 { format!("{}()", nm) } else { format!("{}{}", nm, s) }
            })
        };
        into_c(r.map(|s| pystr::new_str(&s)))
    }
}

fn get_args(o: &PyObject) -> PyResult {
    Ok(exc_args(o))
}
fn set_args(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "args may not be deleted"),
    };
    let t = ab::sequence_tuple(v)?;
    set_field(&mut exc(o).args, Some(&t));
    Ok(())
}
fn get_tb(o: &PyObject) -> PyResult {
    Ok(get_field(exc(o).traceback))
}
fn set_tb(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "__traceback__ may not be deleted"),
        Some(v) if v.is(none::none()) => set_field(&mut exc(o).traceback, None),
        Some(v) if crate::types::traceback::is_traceback(v) => set_field(&mut exc(o).traceback, Some(v)),
        Some(_) => raise!(TypeError, "__traceback__ must be a traceback or None"),
    }
    Ok(())
}
fn get_context(o: &PyObject) -> PyResult {
    Ok(get_field(exc(o).context))
}
fn set_context(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "__context__ may not be deleted"),
        Some(v) if v.is(none::none()) => set_field(&mut exc(o).context, None),
        Some(v) if is_exception_instance(v) => set_field(&mut exc(o).context, Some(v)),
        Some(_) => raise!(TypeError, "exception context must be None or derive from BaseException"),
    }
    Ok(())
}
fn get_cause(o: &PyObject) -> PyResult {
    Ok(get_field(exc(o).cause))
}
fn set_cause(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "__cause__ may not be deleted"),
        Some(v) if v.is(none::none()) => set_field(&mut exc(o).cause, None),
        Some(v) if is_exception_instance(v) => set_field(&mut exc(o).cause, Some(v)),
        Some(_) => raise!(TypeError, "exception cause must be None or derive from BaseException"),
    }
    exc(o).suppress_context = 1;
    Ok(())
}
fn get_dict(o: &PyObject) -> PyResult {
    let e = exc(o);
    let slot = unsafe { std::sync::atomic::AtomicPtr::from_ptr(&raw mut e.dict) };
    if slot.load(std::sync::atomic::Ordering::Acquire).is_null() {
        let d = dict::new_dict().into_raw();
        if slot.compare_exchange(std::ptr::null_mut(), d, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
            unsafe { decref(d) }; // another thread created it first
        }
    }
    Ok(get_field(slot.load(std::sync::atomic::Ordering::Acquire)))
}
fn set_dict(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if dict::is_dict(v) => {
            set_field(&mut exc(o).dict, Some(v));
            Ok(())
        }
        None => raise!(TypeError, "__dict__ may not be deleted"),
        _ => raise!(TypeError, "__dict__ must be a dictionary"),
    }
}

fn with_traceback(o: &PyObject, tb: &PyObject) -> PyResult {
    set_tb(o, Some(tb))?;
    Ok(o.to_ref())
}

fn add_note(o: &PyObject, note: &PyObject) -> PyResult {
    if !pystr::is_str(note) {
        raise!(TypeError, "note must be a str, not '{}'", tname(note));
    }
    let notes = match ab::getattr_opt(o, name!("__notes__"))? {
        Some(n) => n,
        None => {
            let l = list::new_list_empty();
            ab::setattr(o, name!("__notes__"), &l)?;
            l
        }
    };
    if !list::is_list(&notes) {
        raise!(TypeError, "Cannot add note: __notes__ is not a list");
    }
    list::append(&notes, note.to_ref());
    Ok(none::none_ref())
}

fn base_reduce(o: &PyObject) -> PyResult {
    let t = type_ref(o.type_ptr());
    let a = exc_args(o);
    let d = exc(o).dict;
    if d.is_null() {
        Ok(tuple::new_tuple(vec![t, a]))
    } else {
        Ok(tuple::new_tuple(vec![t, a, get_field(d)]))
    }
}

fn base_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if state.is(none::none()) {
        return Ok(none::none_ref());
    }
    if !dict::is_dict(state) {
        raise!(TypeError, "state is not a dictionary");
    }
    for (k, v) in dict::items_vec(state) {
        ab::setattr(o, &k, &v)?;
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- subclass specifics

fn stopiter_init(o: &PyObject, args: &PyObject) -> PyResult<()> {
    let v = if tuple::tuple_len(args) > 0 { tuple::tuple_get(args, 0).to_ref() } else { none::none_ref() };
    unsafe { set_field(&mut (*(o.as_ptr() as *mut StopIterationExc)).value, Some(&v)) };
    Ok(())
}

pub fn stop_iteration_value(o: &PyObject) -> PyRef {
    unsafe { get_field((*(o.as_ptr() as *mut StopIterationExc)).value) }
}

fn sysexit_init(o: &PyObject, args: &PyObject) -> PyResult<()> {
    let v = match tuple::tuple_len(args) {
        0 => none::none_ref(),
        1 => tuple::tuple_get(args, 0).to_ref(),
        _ => args.to_ref(),
    };
    unsafe { set_field(&mut (*(o.as_ptr() as *mut SystemExitExc)).code, Some(&v)) };
    Ok(())
}

fn importerror_init(o: &PyObject, args: &PyObject, kw: Option<&PyObject>) -> PyResult<()> {
    let e = unsafe { &mut *(o.as_ptr() as *mut ImportErrorExc) };
    let items = kw.map(dict::items_vec).unwrap_or_default();
    if items.len() > 3 {
        raise!(TypeError, "{}() takes at most 3 keyword arguments ({} given)", type_name(o.ty()), items.len());
    }
    for (k, _) in &items {
        let ks = pystr::to_string_lossy(k);
        if !matches!(ks.as_str(), "name" | "path" | "name_from") {
            return Err(crate::args::unexpected_keyword(&type_name(o.ty()), k, &["name", "path", "name_from"]));
        }
    }
    // (re-)initialisation resets what is not given
    set_field(&mut e.name, None);
    set_field(&mut e.path, None);
    set_field(&mut e.name_from, None);
    set_field(&mut e.msg, None);
    for (k, v) in &items {
        match pystr::to_string_lossy(k).as_str() {
            "name" => set_field(&mut e.name, Some(v)),
            "path" => set_field(&mut e.path, Some(v)),
            _ => set_field(&mut e.name_from, Some(v)),
        }
    }
    if tuple::tuple_len(args) == 1 {
        set_field(&mut e.msg, Some(tuple::tuple_get(args, 0)));
    }
    Ok(())
}

/// `ImportError('msg', name=..., path=...)`: the positional arguments, then the
/// name and path that are set.
unsafe extern "C" fn importerror_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let e = &*(op as *const ImportErrorExc);
        let r = (|| -> PyResult<PyRef> {
            let mut parts: Vec<String> = Vec::new();
            for a in tuple::tuple_items(&exc_args(o)) {
                parts.push(ab::repr_string(a)?);
            }
            for (label, p) in [("name", e.name), ("path", e.path)] {
                if !p.is_null() && !(*p).is(crate::types::none::none()) {
                    parts.push(format!("{}={}", label, ab::repr_string(&*p)?));
                }
            }
            Ok(pystr::new_str(&format!("{}({})", type_name(o.ty()), parts.join(", "))))
        })();
        into_c(r)
    }
}

fn syntaxerror_init(o: &PyObject, args: &PyObject) -> PyResult<()> {
    let e = unsafe { &mut *(o.as_ptr() as *mut SyntaxErrorExc) };
    let n = tuple::tuple_len(args);
    if n >= 1 {
        set_field(&mut e.msg, Some(tuple::tuple_get(args, 0)));
    }
    if n == 2 {
        let info = ab::sequence_tuple(tuple::tuple_get(args, 1))?;
        let m = tuple::tuple_len(&info);
        if m < 4 {
            raise!(TypeError, "function takes at least 4 arguments ({} given)", m);
        }
        if m > 6 {
            raise!(TypeError, "function takes at most 6 arguments ({} given)", m);
        }
        if m == 5 {
            raise!(TypeError, "end_offset must be provided when end_lineno is provided");
        }
        let it = tuple::tuple_items(&info);
        set_field(&mut e.filename, Some(it[0]));
        set_field(&mut e.lineno, Some(it[1]));
        set_field(&mut e.offset, Some(it[2]));
        set_field(&mut e.text, Some(it[3]));
        // a shorter tuple on re-initialisation clears the end position
        set_field(&mut e.end_lineno, if m > 4 { Some(it[4]) } else { None });
        set_field(&mut e.end_offset, None);
        if m > 5 {
            set_field(&mut e.end_offset, Some(it[5]));
        }
    }
    Ok(())
}

fn oserror_errno_class(errno: i64) -> Option<*mut PyTypeObject> {
    use libc::*;
    let e = errno as i32;
    Some(match e {
        EAGAIN | EALREADY | EINPROGRESS => BlockingIOError(),
        ECHILD => ChildProcessError(),
        EPIPE | ESHUTDOWN => BrokenPipeError(),
        ECONNABORTED => ConnectionAbortedError(),
        ECONNREFUSED => ConnectionRefusedError(),
        ECONNRESET => ConnectionResetError(),
        EEXIST => FileExistsError(),
        ENOENT => FileNotFoundError(),
        EISDIR => IsADirectoryError(),
        ENOTDIR => NotADirectoryError(),
        EINTR => InterruptedError(),
        EACCES | EPERM | ENOTCAPABLE_OR_ZERO => PermissionError(),
        ESRCH => ProcessLookupError(),
        ETIMEDOUT => TimeoutError(),
        _ => return None,
    })
}
const ENOTCAPABLE_OR_ZERO: i32 = -9999;

unsafe extern "C" fn oserror_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mut tp = tp;
        let a = tuple::tuple_items(&*args);
        if tp == OSError() && a.len() >= 2 {
            if let Some(n) = if int::is_int(a[0]) { int::as_i64(a[0]) } else { None } {
                if let Some(sub) = oserror_errno_class(n) {
                    tp = sub;
                }
            }
        }
        let op = base_new(tp, args, kw);
        if op.is_null() {
            return op;
        }
        let e = &mut *(op as *mut OSErrorExc);
        e.written = -1;
        // only fill fields here when __init__ is the default one (so subclasses that
        // override __init__ still get errno/strerror from the arguments)
        if (*tp).tp_init == OSError().as_ref().and_then(|t| t.tp_init) || true {
            if oserror_fill(&*op, &*args).is_err() {
                decref(op);
                return std::ptr::null_mut();
            }
        }
        op
    }
}

fn oserror_fill(o: &PyObject, args: &PyObject) -> PyResult<()> {
    let e = unsafe { &mut *(o.as_ptr() as *mut OSErrorExc) };
    let a = tuple::tuple_items(args);
    if a.len() >= 2 && a.len() <= 5 {
        set_field(&mut e.myerrno, Some(a[0]));
        set_field(&mut e.strerror, Some(a[1]));
        if a.len() >= 3 && !a[2].is(none::none()) {
            let third = a[2];
            let n = third.ty().tp_as_number;
            let numeric = !n.is_null()
                && unsafe { (*n).nb_index.is_some() || (*n).nb_int.is_some() || (*n).nb_float.is_some() };
            if o.is_type(BlockingIOError()) && numeric {
                // BlockingIOError(errno, strerror, characters_written): args kept whole
                e.written = ab::index_as_ssize(third, Some(ValueError()))?;
            } else {
                set_field(&mut e.filename, Some(third));
                if a.len() >= 5 && !a[4].is(none::none()) {
                    set_field(&mut e.filename2, Some(a[4]));
                }
                // args is reduced to (errno, strerror)
                let t = tuple::new_tuple(vec![a[0].to_ref(), a[1].to_ref()]);
                set_field(&mut exc(o).args, Some(&t));
            }
        }
    }
    Ok(())
}

unsafe extern "C" fn oserror_init(_op: *mut PyObject, _args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        if !kw.is_null() && dict::dict_len(&*kw) > 0 {
            let _ = err!(TypeError, "{}() takes no keyword arguments", type_name((*_op).ty()));
            return -1;
        }
        0
    }
}

unsafe extern "C" fn oserror_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let e = &*(op as *mut OSErrorExc);
        let r = (|| -> PyResult {
            let s = |p: *mut PyObject| -> PyResult<String> { Ok(pystr::to_string_lossy(&*ab::str(&get_field(p))?)) };
            let r = |p: *mut PyObject| -> PyResult<String> { Ok(pystr::to_string_lossy(&*ab::repr(&get_field(p))?)) };
            if !e.filename.is_null() {
                if !e.filename2.is_null() {
                    return Ok(pystr::new_str(&format!(
                        "[Errno {}] {}: {} -> {}",
                        s(e.myerrno)?,
                        s(e.strerror)?,
                        r(e.filename)?,
                        r(e.filename2)?
                    )));
                }
                return Ok(pystr::new_str(&format!("[Errno {}] {}: {}", s(e.myerrno)?, s(e.strerror)?, r(e.filename)?)));
            }
            if !e.myerrno.is_null() && !e.strerror.is_null() {
                return Ok(pystr::new_str(&format!("[Errno {}] {}", s(e.myerrno)?, s(e.strerror)?)));
            }
            Ok(PyRef::from_raw(base_str(o.as_ptr())))
        })();
        into_c(r)
    }
}

fn oserror_reduce(o: &PyObject) -> PyResult {
    let e = unsafe { &*(o.as_ptr() as *mut OSErrorExc) };
    let mut args = exc_args(o);
    if tuple::tuple_len(&args) == 2 && !e.filename.is_null() {
        let mut v = tuple::tuple_to_vec(&args);
        v.push(get_field(e.filename));
        if !e.filename2.is_null() {
            v.push(none::none_ref());
            v.push(get_field(e.filename2));
        }
        args = tuple::new_tuple(v);
    }
    let t = type_ref(o.type_ptr());
    let d = exc(o).dict;
    if d.is_null() { Ok(tuple::new_tuple(vec![t, args])) } else { Ok(tuple::new_tuple(vec![t, args, get_field(d)])) }
}

fn oserror_get_written(o: &PyObject) -> PyResult {
    let e = unsafe { &*(o.as_ptr() as *mut OSErrorExc) };
    if e.written == -1 {
        raise!(AttributeError, "characters_written");
    }
    Ok(int::new_int(e.written as i64))
}
fn oserror_set_written(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let e = unsafe { &mut *(o.as_ptr() as *mut OSErrorExc) };
    match v {
        None => {
            if e.written == -1 {
                raise!(AttributeError, "characters_written");
            }
            e.written = -1;
        }
        Some(v) => e.written = ab::index_as_ssize(v, Some(ValueError()))?,
    }
    Ok(())
}

unsafe extern "C" fn keyerror_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = exc_args(&*op);
        if tuple::tuple_len(&a) == 1 {
            return into_c(ab::repr(tuple::tuple_get(&a, 0)));
        }
        base_str(op)
    }
}

unsafe extern "C" fn importerror_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let e = &*(op as *mut ImportErrorExc);
        // only an exact str message is used as is (like CPython); anything
        // else goes through BaseException.__str__
        if !e.msg.is_null() && crate::types::str::is_str_exact(&*e.msg) {
            return (*e.msg).to_ref().into_raw();
        }
        base_str(op)
    }
}

fn importerror_reduce(o: &PyObject) -> PyResult {
    let e = unsafe { &*(o.as_ptr() as *mut ImportErrorExc) };
    let t = type_ref(o.type_ptr());
    let args = exc_args(o);
    let mut state: Option<PyRef> = if exc(o).dict.is_null() { None } else { Some(dict::copy(&get_field(exc(o).dict))?) };
    for (k, p) in [("name", e.name), ("path", e.path), ("name_from", e.name_from)] {
        if !p.is_null() && !unsafe { &*p }.is(none::none()) {
            let d = state.get_or_insert_with(dict::new_dict);
            dict::set_item_str(d, k, unsafe { &*p })?;
        }
    }
    match state {
        Some(s) => Ok(tuple::new_tuple(vec![t, args, s])),
        None => Ok(tuple::new_tuple(vec![t, args])),
    }
}

unsafe extern "C" fn syntaxerror_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let e = &*(op as *mut SyntaxErrorExc);
        let msg = match ab::str(&get_field(e.msg)) {
            Ok(m) => pystr::to_string_lossy(&m),
            Err(_) => return std::ptr::null_mut(),
        };
        let filename = if !e.filename.is_null() && pystr::is_str(&*e.filename) {
            let f = pystr::to_string_lossy(&*e.filename);
            Some(match f.rfind('/') {
                Some(i) => f[i + 1..].to_string(),
                None => f,
            })
        } else {
            None
        };
        let lineno = if !e.lineno.is_null() && int::is_int(&*e.lineno) { int::as_i64(&*e.lineno) } else { None };
        let s = match (filename, lineno) {
            (Some(f), Some(l)) => format!("{} ({}, line {})", msg, f, l),
            (Some(f), None) => format!("{} ({})", msg, f),
            (None, Some(l)) => format!("{} (line {})", msg, l),
            (None, None) => msg,
        };
        pystr::new_str(&s).into_raw()
    }
}

fn unicode_char_repr(c: u32) -> String {
    if c <= 0xff {
        format!("\\x{:02x}", c)
    } else if c <= 0xffff {
        format!("\\u{:04x}", c)
    } else {
        format!("\\U{:08x}", c)
    }
}

unsafe extern "C" fn unicode_error_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let e = &*(op as *mut UnicodeErrorExc);
        if e.object.is_null() {
            return pystr::new_str("").into_raw();
        }
        let r = (|| -> PyResult<String> {
            // str() of reason/encoding may run code that mutates the exception: compute
            // them first, then re-validate the object
            let reason = pystr::to_string_lossy(&*ab::str(&get_field(e.reason))?);
            let is_decode = o.is_type(UnicodeDecodeError()) || is_subtype(o.type_ptr(), UnicodeDecodeError());
            let is_enc = o.is_type(UnicodeEncodeError()) || is_subtype(o.type_ptr(), UnicodeEncodeError());
            let enc_s = if is_decode || is_enc { Some(pystr::to_string_lossy(&*ab::str(&get_field(e.encoding))?)) } else { None };
            let e = &*(op as *mut UnicodeErrorExc);
            let obj_ref = match get_field_opt(e.object) {
                Some(x) if (is_decode && crate::types::bytes::bytes_like_data(&x).is_some()) || (!is_decode && pystr::is_str(&x)) => x,
                _ => crate::raise!(TypeError, "{} attribute must be {}", "object", if is_decode { "bytes" } else { "unicode" }),
            };
            let obj = &*obj_ref;
            let get_field = |p: *mut PyObject| -> PyRef {
                if p == e.encoding && enc_s.is_some() { pystr::new_str(enc_s.as_ref().unwrap()) } else { get_field(p) }
            };
            if is_decode {
                let enc = pystr::to_string_lossy(&*ab::str(&get_field(e.encoding))?);
                let bytes = crate::types::bytes::bytes_like_data(obj).unwrap_or_default();
                if e.start >= 0 && (e.start as usize) < bytes.len() && e.end == e.start + 1 {
                    return Ok(format!(
                        "'{}' codec can't decode byte 0x{:02x} in position {}: {}",
                        enc, bytes[e.start as usize], e.start, reason
                    ));
                }
                return Ok(format!(
                    "'{}' codec can't decode bytes in position {}-{}: {}",
                    enc,
                    e.start,
                    e.end - 1,
                    reason
                ));
            }
            let is_encode = o.is_type(UnicodeEncodeError()) || is_subtype(o.type_ptr(), UnicodeEncodeError());
            let len = if pystr::is_str(obj) { pystr::str_len(obj) as isize } else { 0 };
            if e.start >= 0 && e.start < len && e.end == e.start + 1 {
                let c = pystr::str_data(obj).get(e.start as usize);
                if is_encode {
                    let enc = pystr::to_string_lossy(&*ab::str(&get_field(e.encoding))?);
                    return Ok(format!(
                        "'{}' codec can't encode character '{}' in position {}: {}",
                        enc,
                        unicode_char_repr(c),
                        e.start,
                        reason
                    ));
                }
                return Ok(format!("can't translate character '{}' in position {}: {}", unicode_char_repr(c), e.start, reason));
            }
            if is_encode {
                let enc = pystr::to_string_lossy(&*ab::str(&get_field(e.encoding))?);
                return Ok(format!(
                    "'{}' codec can't encode characters in position {}-{}: {}",
                    enc,
                    e.start,
                    e.end - 1,
                    reason
                ));
            }
            Ok(format!("can't translate characters in position {}-{}: {}", e.start, e.end - 1, reason))
        })();
        into_c(r.map(|s| pystr::new_str(&s)))
    }
}

fn unicode_error_init(o: &PyObject, args: &PyObject) -> PyResult<()> {
    let e = unsafe { &mut *(o.as_ptr() as *mut UnicodeErrorExc) };
    let a = tuple::tuple_items(args);
    let is_translate = o.is_type(UnicodeTranslateError()) || is_subtype(o.type_ptr(), UnicodeTranslateError());
    let is_decode = o.is_type(UnicodeDecodeError()) || is_subtype(o.type_ptr(), UnicodeDecodeError());
    let (need, nm) = if is_translate { (4, "UnicodeTranslateError") } else if is_decode { (5, "UnicodeDecodeError") } else { (5, "UnicodeEncodeError") };
    if a.len() != need {
        raise!(TypeError, "function takes exactly {} arguments ({} given)", need, a.len());
    }
    let mut i = 0;
    if !is_translate {
        if !pystr::is_str(a[0]) {
            raise!(TypeError, "argument 1 must be str, not {}", tname(a[0]));
        }
        set_field(&mut e.encoding, Some(a[0]));
        i = 1;
    }
    let obj = a[i];
    if is_decode {
        // accept any bytes-like object; store as bytes
        match crate::types::bytes::bytes_like_data(obj) {
            Some(d) => {
                let b = crate::types::bytes::new_bytes(&d);
                set_field(&mut e.object, Some(&b));
            }
            None => raise!(TypeError, "argument {} must be bytes-like object, not {}", i + 1, tname(obj)),
        }
    } else {
        if !pystr::is_str(obj) {
            raise!(TypeError, "argument {} must be str, not {}", i + 1, tname(obj));
        }
        set_field(&mut e.object, Some(obj));
    }
    e.start = ab::index_as_ssize(a[i + 1], None)?;
    e.end = ab::index_as_ssize(a[i + 2], None)?;
    if !pystr::is_str(a[i + 3]) {
        raise!(TypeError, "argument {} must be str, not {}", i + 4, tname(a[i + 3]));
    }
    set_field(&mut e.reason, Some(a[i + 3]));
    let _ = nm;
    Ok(())
}

fn uerr(o: &PyObject) -> &mut UnicodeErrorExc {
    unsafe { &mut *(o.as_ptr() as *mut UnicodeErrorExc) }
}
/// The attributes hold the raw values (only the C API getters clamp them).
fn ue_get_start(o: &PyObject) -> PyResult {
    Ok(int::new_int(uerr(o).start as i64))
}
fn ue_set_start(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => uerr(o).start = ab::index_as_ssize(v, None)?,
        None => raise!(TypeError, "can't delete attribute"),
    }
    Ok(())
}
fn ue_get_end(o: &PyObject) -> PyResult {
    Ok(int::new_int(uerr(o).end as i64))
}
fn ue_set_end(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => uerr(o).end = ab::index_as_ssize(v, None)?,
        None => raise!(TypeError, "can't delete attribute"),
    }
    Ok(())
}

/// Build and set a UnicodeEncodeError.
pub fn unicode_encode_error(encoding: &str, obj: &PyObject, start: usize, end: usize, reason: &str) -> PyErr {
    let args = [
        pystr::new_str(encoding),
        obj.to_ref(),
        int::new_int(start as i64),
        int::new_int(end as i64),
        pystr::new_str(reason),
    ];
    match make_exception(UnicodeEncodeError(), &args) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}
pub fn unicode_decode_error(encoding: &str, data: &[u8], start: usize, end: usize, reason: &str) -> PyErr {
    let args = [
        pystr::new_str(encoding),
        crate::types::bytes::new_bytes(data),
        int::new_int(start as i64),
        int::new_int(end as i64),
        pystr::new_str(reason),
    ];
    match make_exception(UnicodeDecodeError(), &args) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

/// AttributeError with `name` and `obj` set (used for suggestions).
pub fn attribute_error_with(msg: &str, obj: &PyObject, nm: &PyObject) -> PyErr {
    match make_exception(AttributeError(), &[pystr::new_str(msg)]) {
        Ok(e) => {
            unsafe {
                let ae = &mut *(e.as_ptr() as *mut AttributeErrorExc);
                set_field(&mut ae.name, Some(nm));
                set_field(&mut ae.obj, Some(obj));
            }
            set_exc(e);
        }
        Err(_) => {}
    }
    PyErr
}

pub fn name_error_with(tp: *mut PyTypeObject, msg: &str, nm: &PyObject) -> PyErr {
    match make_exception(tp, &[pystr::new_str(msg)]) {
        Ok(e) => {
            unsafe {
                let ne = &mut *(e.as_ptr() as *mut NameErrorExc);
                set_field(&mut ne.name, Some(nm));
            }
            set_exc(e);
        }
        Err(_) => {}
    }
    PyErr
}

/// OSError from errno (picks the subclass), optional filename(s).
pub fn os_error_from_errno(errno: i32, filename: Option<&PyObject>, filename2: Option<&PyObject>) -> PyErr {
    let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)).to_string_lossy().into_owned() };
    let mut args = vec![int::new_int(errno as i64), pystr::new_str(&msg)];
    if let Some(f) = filename {
        args.push(f.to_ref());
        if let Some(f2) = filename2 {
            args.push(none::none_ref());
            args.push(f2.to_ref());
        }
    }
    match make_exception(OSError(), &args) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

pub fn last_os_error() -> PyErr {
    os_error_from_errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(0), None, None)
}

/// Generic init for the special subclasses: validates kwargs then fills fields.
unsafe extern "C" fn special_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let tp = o.type_ptr();
        let layout = layout_of(tp);
        let kwd = if kw.is_null() || dict::dict_len(&*kw) == 0 { None } else { Some(&*kw) };
        if layout != Layout::ImportError && layout != Layout::NameError && layout != Layout::AttributeError {
            if kwd.is_some() {
                let _ = err!(TypeError, "{}() takes no keyword arguments", type_name(o.ty()));
                return -1;
            }
        }
        set_field(&mut exc(o).args, Some(&*args));
        let r = match layout {
            Layout::StopIteration => stopiter_init(o, &*args),
            Layout::SystemExit => sysexit_init(o, &*args),
            Layout::ImportError => importerror_init(o, &*args, kwd),
            Layout::SyntaxError => syntaxerror_init(o, &*args),
            Layout::UnicodeError => {
                if tp == UnicodeError() {
                    Ok(())
                } else {
                    unicode_error_init(o, &*args)
                }
            }
            Layout::NameError => (|| {
                if let Some(kw) = kwd {
                    for (k, v) in dict::items_vec(kw) {
                        if pystr::str_eq_ascii(&k, "name") {
                            set_field(&mut (*(op as *mut NameErrorExc)).name, Some(&v));
                        } else {
                            raise!(TypeError, "'{}' is an invalid keyword argument for {}()", pystr::to_string_lossy(&k), type_name(o.ty()));
                        }
                    }
                }
                Ok(())
            })(),
            Layout::AttributeError => (|| {
                if let Some(kw) = kwd {
                    for (k, v) in dict::items_vec(kw) {
                        let ks = pystr::to_string_lossy(&k);
                        let e = &mut *(op as *mut AttributeErrorExc);
                        match ks.as_str() {
                            "name" => set_field(&mut e.name, Some(&v)),
                            "obj" => set_field(&mut e.obj, Some(&v)),
                            _ => raise!(TypeError, "'{}' is an invalid keyword argument for {}()", ks, type_name(o.ty())),
                        }
                    }
                }
                Ok(())
            })(),
            _ => Ok(()),
        };
        int_into_c(r)
    }
}

fn attributeerror_getstate(o: &PyObject) -> PyResult {
    let e = unsafe { &*(o.as_ptr() as *mut AttributeErrorExc) };
    let d = if exc(o).dict.is_null() { dict::new_dict() } else { dict::copy(&get_field(exc(o).dict))? };
    if !e.name.is_null() {
        dict::set_item_str(&d, "name", unsafe { &*e.name })?;
    }
    dict::set_item_str(&d, "args", &exc_args(o))?;
    Ok(d)
}
fn attributeerror_reduce(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), exc_args(o), attributeerror_getstate(o)?]))
}

// ---------------------------------------------------------------- setup

unsafe extern "C" fn incomplete_dummy(_o: *mut PyObject) {}

pub fn init_types() {
    unsafe {
        let base_getset = crate::getset_table![
            ("args", get_args, set_args),
            ("__traceback__", get_tb, set_tb),
            ("__context__", get_context, set_context; "exception context"),
            ("__cause__", get_cause, set_cause; "exception cause"),
            ("__dict__", get_dict, set_dict),
        ];
        let base_members = crate::member_table![(
            "__suppress_context__",
            Py_T_BOOL,
            std::mem::offset_of!(BaseExc, suppress_context),
            0
        )];
        let base_methods = crate::mdefs![
            ("with_traceback", O, with_traceback, "Set self.__traceback__ to tb and return self."),
            ("add_note", O, add_note, "Add a note to the exception"),
            ("__reduce__", NOARGS, base_reduce),
            ("__setstate__", O, base_setstate),
        ];
        let _ = incomplete_dummy;
        for &(idx, pyname, base, layout, doc) in TABLE.iter() {
            let tp = (&raw mut TYPES[idx as usize]) as *mut PyTypeObject;
            let t = &mut *tp;
            t.tp_name = crate::methods::cstr_static(pyname);
            t.tp_basicsize = layout.size() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASE_EXC_SUBCLASS;
            if !doc.is_empty() {
                t.tp_doc = crate::methods::cstr_static(doc);
            }
            t.tp_dealloc = Some(base_dealloc);
            t.tp_traverse = Some(base_traverse);
            t.tp_clear = Some(base_clear);
            t.tp_dictoffset = std::mem::offset_of!(BaseExc, dict) as isize;
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            match base {
                None => {
                    t.tp_new = Some(base_new);
                    t.tp_init = Some(base_init);
                    t.tp_str = Some(base_str);
                    t.tp_repr = Some(base_repr);
                    t.tp_getattro = Some(object_generic_getattro);
                    t.tp_setattro = Some(object_generic_setattro);
                    t.tp_getset = base_getset;
                    t.tp_members = base_members;
                    t.tp_methods = base_methods;
                }
                Some(b) => {
                    t.tp_base = (&raw mut TYPES[b as usize]) as *mut PyTypeObject;
                }
            }
            match layout {
                Layout::StopIteration => {
                    t.tp_init = Some(special_init);
                    t.tp_members = crate::member_table![(
                        "value",
                        Py_T_OBJECT,
                        std::mem::offset_of!(StopIterationExc, value),
                        0;
                        "generator return value"
                    )];
                }
                Layout::SystemExit => {
                    t.tp_init = Some(special_init);
                    t.tp_members = crate::member_table![("code", Py_T_OBJECT, std::mem::offset_of!(SystemExitExc, code), 0; "exception code")];
                }
                Layout::ImportError if idx as usize == Idx::ImportError as usize => {
                    t.tp_init = Some(special_init);
                    t.tp_str = Some(importerror_str);
                    t.tp_repr = Some(importerror_repr);
                    t.tp_members = crate::member_table![
                        ("msg", Py_T_OBJECT, std::mem::offset_of!(ImportErrorExc, msg), 0; "exception message"),
                        ("name", Py_T_OBJECT, std::mem::offset_of!(ImportErrorExc, name), 0; "module name"),
                        ("path", Py_T_OBJECT, std::mem::offset_of!(ImportErrorExc, path), 0; "module path"),
                        ("name_from", Py_T_OBJECT, std::mem::offset_of!(ImportErrorExc, name_from), 0; "name imported from module"),
                    ];
                    t.tp_methods = crate::mdefs![("__reduce__", NOARGS, importerror_reduce)];
                }
                Layout::OSError if idx as usize == Idx::OSError as usize => {
                    t.tp_new = Some(oserror_new);
                    t.tp_init = Some(oserror_init);
                    t.tp_str = Some(oserror_str);
                    t.tp_members = crate::member_table![
                        ("errno", Py_T_OBJECT, std::mem::offset_of!(OSErrorExc, myerrno), 0; "POSIX exception code"),
                        ("strerror", Py_T_OBJECT, std::mem::offset_of!(OSErrorExc, strerror), 0; "exception strerror"),
                        ("filename", Py_T_OBJECT, std::mem::offset_of!(OSErrorExc, filename), 0; "exception filename"),
                        ("filename2", Py_T_OBJECT, std::mem::offset_of!(OSErrorExc, filename2), 0; "second exception filename"),
                    ];
                    t.tp_getset = crate::getset_table![("characters_written", oserror_get_written, oserror_set_written)];
                    t.tp_methods = crate::mdefs![("__reduce__", NOARGS, oserror_reduce)];
                }
                Layout::SyntaxError if idx as usize == Idx::SyntaxError as usize => {
                    t.tp_init = Some(special_init);
                    t.tp_str = Some(syntaxerror_str);
                    t.tp_members = crate::member_table![
                        ("msg", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, msg), 0; "exception msg"),
                        ("filename", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, filename), 0; "exception filename"),
                        ("lineno", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, lineno), 0; "exception lineno"),
                        ("offset", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, offset), 0; "exception offset"),
                        ("text", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, text), 0; "exception text"),
                        ("end_lineno", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, end_lineno), 0; "exception end lineno"),
                        ("end_offset", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, end_offset), 0; "exception end offset"),
                        ("print_file_and_line", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, print_file_and_line), 0; "exception print_file_and_line"),
                        ("_metadata", Py_T_OBJECT, std::mem::offset_of!(SyntaxErrorExc, metadata), 0; "exception private metadata"),
                    ];
                }
                Layout::UnicodeError if idx as usize != Idx::UnicodeError as usize => {
                    t.tp_init = Some(special_init);
                    t.tp_str = Some(unicode_error_str);
                    t.tp_members = crate::member_table![
                        ("encoding", Py_T_OBJECT, std::mem::offset_of!(UnicodeErrorExc, encoding), 0; "exception encoding"),
                        ("object", Py_T_OBJECT, std::mem::offset_of!(UnicodeErrorExc, object), 0; "exception object"),
                        ("reason", Py_T_OBJECT, std::mem::offset_of!(UnicodeErrorExc, reason), 0; "exception reason"),
                    ];
                    t.tp_getset = crate::getset_table![("start", ue_get_start, ue_set_start), ("end", ue_get_end, ue_set_end)];
                }
                Layout::NameError if idx as usize == Idx::NameError as usize => {
                    t.tp_init = Some(special_init);
                    t.tp_members = crate::member_table![("name", Py_T_OBJECT, std::mem::offset_of!(NameErrorExc, name), 0; "name")];
                }
                Layout::AttributeError => {
                    t.tp_init = Some(special_init);
                    t.tp_members = crate::member_table![
                        ("name", Py_T_OBJECT, std::mem::offset_of!(AttributeErrorExc, name), 0; "attribute name"),
                        ("obj", Py_T_OBJECT, std::mem::offset_of!(AttributeErrorExc, obj), 0; "object"),
                    ];
                    t.tp_methods = crate::mdefs![
                        ("__getstate__", NOARGS, attributeerror_getstate),
                        ("__reduce__", NOARGS, attributeerror_reduce),
                    ];
                }
                Layout::ExcGroup => {
                    crate::types::exceptiongroup::setup_type(tp, idx as usize == Idx::BaseExceptionGroup as usize);
                }
                _ => {}
            }
        }
        (*KeyError()).tp_str = Some(keyerror_str);
        set_exported();
        PyExc_EnvironmentError = OSError() as *mut PyObject;
        PyExc_IOError = OSError() as *mut PyObject;
    }
}

/// Ready all exception types (after `type` is ready) in table order.
pub fn ready_all() -> PyResult<()> {
    for &(idx, ..) in TABLE.iter() {
        let tp = unsafe { (&raw mut TYPES[idx as usize]) as *mut PyTypeObject };
        type_ready(tp)?;
        // every builtin exception class has its own __new__ (as in CPython, where each one
        // sets tp_new); extensions look for it in the class dict (msgspec rejects
        // `class S(Struct, Exception)` that way)
        unsafe {
            let d = &*(*tp).tp_dict;
            if (*tp).tp_new.is_some() && dict::get_item_str(d, crate::name!("__new__")).is_none() {
                dict::set_item(d, crate::name!("__new__"), &crate::types::descr::new_tp_new_wrapper(tp))?;
            }
        }
    }
    Ok(())
}

/// (python name, type) for every builtin exception, for the builtins module.
pub fn all_exception_types() -> Vec<(&'static str, *mut PyTypeObject)> {
    TABLE
        .iter()
        .map(|&(idx, name, ..)| (name, unsafe { (&raw mut TYPES[idx as usize]) as *mut PyTypeObject }))
        .collect()
}

pub fn set_context_and_cause(e: &PyObject, context: Option<&PyObject>) {
    set_field(&mut exc(e).context, context);
}
pub fn exc_traceback(e: &PyObject) -> Option<PyRef> {
    let p = exc(e).traceback;
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}
pub fn set_exc_traceback(e: &PyObject, tb: Option<&PyObject>) {
    set_field(&mut exc(e).traceback, tb);
}
pub fn exc_context(e: &PyObject) -> Option<PyRef> {
    let p = exc(e).context;
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}
pub fn set_exc_cause(e: &PyObject, cause: Option<&PyObject>) {
    set_field(&mut exc(e).cause, cause);
    exc(e).suppress_context = 1;
}
pub fn set_name_error_name(e: &PyObject, nm: &PyObject) {
    unsafe { set_field(&mut (*(e.as_ptr() as *mut NameErrorExc)).name, Some(nm)) }
}
