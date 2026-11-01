//! `_tkinter`: the binding to Tcl/Tk 8.6 that the `tkinter` package is built on.
//!
//! libtcl8.6 and libtk8.6 are dlopen'ed on first import (DECISIONS D20, see dl.rs);
//! object layouts (`Tcl_Obj`, `Tcl_ObjType`, `Tcl_Event`) follow the public tcl.h.
//!
//! Design notes
//! * One `tkapp` object owns one Tcl interpreter.  Tcl interpreters may only be used
//!   from the thread that created them, so every operation requested from another
//!   Python thread is packaged as a closure, queued to the owner thread with
//!   `Tcl_ThreadQueueEvent` and run by that thread's event loop (which must be
//!   running `mainloop`, or the caller gets "main thread is not in main loop").
//! * Tcl calls run with the thread in `Blocked` mode (they may wait for events for an
//!   arbitrary time); callbacks into Python switch back to `InVm` for their duration.
//! * Strings: Tcl uses a modified UTF-8 (NUL is `C0 80`, characters outside the BMP
//!   may arrive as CESU-8 surrogate pairs).  Python strings are encoded the same way,
//!   with lone `U+DC80..U+DCFF` surrogates standing for raw bytes (surrogateescape);
//!   decoding maps undecodable bytes back to such surrogates.
//! * With `wantobjects` set, typed Tcl values become Python values (int, float, bool,
//!   bytes, tuples for lists); values of other Tcl types are wrapped as `Tcl_Obj`.
//!   Level 2 also converts the arguments passed to Python commands.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::object::*;
use crate::threads::ThreadMode;
use crate::types::{bool_, bytes, dict, exceptions, float, int, list, none, str as pystr, tuple, typeobject};
use crate::{dl_api, mdefs, raise};
use std::cell::Cell;
use std::ffi::{CString, c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

type Interp = *mut c_void;
type TclThreadId = *mut c_void;

#[repr(C)]
pub struct TclObjType {
    name: *const c_char,
    free_int_rep: *mut c_void,
    dup_int_rep: *mut c_void,
    update_string: *mut c_void,
    set_from_any: *mut c_void,
}

#[repr(C)]
pub struct TclObj {
    ref_count: c_int,
    bytes: *mut c_char,
    length: c_int,
    type_ptr: *const TclObjType,
    internal: [usize; 2],
}

#[repr(C)]
struct TclEvent {
    proc_: unsafe extern "C" fn(*mut TclEvent, c_int) -> c_int,
    next: *mut TclEvent,
}

type ObjCmdProc = unsafe extern "C" fn(*mut c_void, Interp, c_int, *const *mut TclObj) -> c_int;
type CmdDeleteProc = unsafe extern "C" fn(*mut c_void);
type FileProc = unsafe extern "C" fn(*mut c_void, c_int);
type TimerProc = unsafe extern "C" fn(*mut c_void);

const TCL_OK: c_int = 0;
const TCL_ERROR: c_int = 1;
const TCL_GLOBAL_ONLY: c_int = 1;
const TCL_LEAVE_ERR_MSG: c_int = 0x200;
const TCL_EVAL_GLOBAL: c_int = 0x020000;
const TCL_EVAL_DIRECT: c_int = 0x040000;
const TCL_QUEUE_TAIL: c_int = 0;
const TCL_DONT_WAIT: c_int = 1 << 1;
const TCL_WINDOW_EVENTS: c_int = 1 << 2;
const TCL_FILE_EVENTS: c_int = 1 << 3;
const TCL_TIMER_EVENTS: c_int = 1 << 4;
const TCL_IDLE_EVENTS: c_int = 1 << 5;
const TCL_ALL_EVENTS: c_int = !TCL_DONT_WAIT;
const TCL_READABLE: c_int = 1 << 1;
const TCL_WRITABLE: c_int = 1 << 2;
const TCL_EXCEPTION: c_int = 1 << 3;

dl_api! {
    pub struct TclApi, fn tcl_api, ["libtcl8.6.so", "libtcl8.6.so.0", "libtcl.so"] {
        fn Tcl_FindExecutable(argv0: *const c_char);
        fn Tcl_CreateInterp() -> Interp;
        fn Tcl_DeleteInterp(i: Interp);
        fn Tcl_InterpDeleted(i: Interp) -> c_int;
        fn Tcl_Init(i: Interp) -> c_int;
        fn Tcl_GetVersion(major: *mut c_int, minor: *mut c_int, patch: *mut c_int, kind: *mut c_int);
        fn Tcl_EvalEx(i: Interp, s: *const c_char, n: c_int, flags: c_int) -> c_int;
        fn Tcl_EvalObjv(i: Interp, objc: c_int, objv: *const *mut TclObj, flags: c_int) -> c_int;
        fn Tcl_EvalFile(i: Interp, name: *const c_char) -> c_int;
        fn Tcl_RecordAndEval(i: Interp, s: *const c_char, flags: c_int) -> c_int;
        fn Tcl_AddErrorInfo(i: Interp, msg: *const c_char);
        fn Tcl_GetObjResult(i: Interp) -> *mut TclObj;
        fn Tcl_SetObjResult(i: Interp, o: *mut TclObj);
        fn Tcl_ResetResult(i: Interp);
        fn Tcl_GetStringFromObj(o: *mut TclObj, len: *mut c_int) -> *mut c_char;
        fn Tcl_NewStringObj(s: *const c_char, n: c_int) -> *mut TclObj;
        fn Tcl_NewWideIntObj(v: i64) -> *mut TclObj;
        fn Tcl_NewBooleanObj(v: c_int) -> *mut TclObj;
        fn Tcl_NewDoubleObj(v: f64) -> *mut TclObj;
        fn Tcl_NewListObj(n: c_int, v: *const *mut TclObj) -> *mut TclObj;
        fn Tcl_NewByteArrayObj(b: *const u8, n: c_int) -> *mut TclObj;
        fn Tcl_NewBignumObj(mp: *mut c_void) -> *mut TclObj;
        fn Tcl_GetBignumFromObj(i: Interp, o: *mut TclObj, mp: *mut c_void) -> c_int;
        fn Tcl_GetWideIntFromObj(i: Interp, o: *mut TclObj, v: *mut i64) -> c_int;
        fn Tcl_GetDoubleFromObj(i: Interp, o: *mut TclObj, v: *mut f64) -> c_int;
        fn Tcl_GetBooleanFromObj(i: Interp, o: *mut TclObj, v: *mut c_int) -> c_int;
        fn Tcl_GetBoolean(i: Interp, s: *const c_char, v: *mut c_int) -> c_int;
        fn Tcl_SetSystemEncoding(i: Interp, name: *const c_char) -> c_int;
        fn Tcl_GetEncodingName(e: *mut c_void) -> *const c_char;
        fn Tcl_GetByteArrayFromObj(o: *mut TclObj, n: *mut c_int) -> *mut u8;
        fn Tcl_ListObjGetElements(i: Interp, o: *mut TclObj, n: *mut c_int, v: *mut *mut *mut TclObj) -> c_int;
        fn Tcl_GetObjType(name: *const c_char) -> *const TclObjType;
        fn Tcl_SetVar2Ex(i: Interp, n1: *const c_char, n2: *const c_char, v: *mut TclObj, flags: c_int) -> *mut TclObj;
        fn Tcl_GetVar2Ex(i: Interp, n1: *const c_char, n2: *const c_char, flags: c_int) -> *mut TclObj;
        fn Tcl_UnsetVar2(i: Interp, n1: *const c_char, n2: *const c_char, flags: c_int) -> c_int;
        fn Tcl_CreateObjCommand(i: Interp, name: *const c_char, p: ObjCmdProc, cd: *mut c_void, d: Option<CmdDeleteProc>) -> *mut c_void;
        fn Tcl_DeleteCommand(i: Interp, name: *const c_char) -> c_int;
        fn Tcl_CreateFileHandler(fd: c_int, mask: c_int, p: FileProc, cd: *mut c_void);
        fn Tcl_DeleteFileHandler(fd: c_int);
        fn Tcl_CreateTimerHandler(ms: c_int, p: TimerProc, cd: *mut c_void) -> *mut c_void;
        fn Tcl_DeleteTimerHandler(t: *mut c_void);
        fn Tcl_DoOneEvent(flags: c_int) -> c_int;
        fn Tcl_ExprString(i: Interp, s: *const c_char) -> c_int;
        fn Tcl_ExprLong(i: Interp, s: *const c_char, v: *mut c_long) -> c_int;
        fn Tcl_ExprDouble(i: Interp, s: *const c_char, v: *mut f64) -> c_int;
        fn Tcl_ExprBoolean(i: Interp, s: *const c_char, v: *mut c_int) -> c_int;
        fn Tcl_GetCurrentThread() -> TclThreadId;
        fn Tcl_ThreadQueueEvent(t: TclThreadId, ev: *mut TclEvent, pos: c_int);
        fn Tcl_ThreadAlert(t: TclThreadId);
        fn Tcl_Alloc(n: u32) -> *mut c_char;
        fn TclFreeObj(o: *mut TclObj);
        fn TclBN_mp_clear(mp: *mut c_void);
        fn Tcl_SetVar2(i: Interp, n1: *const c_char, n2: *const c_char, v: *const c_char, flags: c_int) -> *const c_char;
        fn Tcl_GetVar2(i: Interp, n1: *const c_char, n2: *const c_char, flags: c_int) -> *const c_char;
    }
}

dl_api! {
    pub struct TkApi, fn tk_api, ["libtk8.6.so", "libtk8.6.so.0", "libtk.so"] {
        fn Tk_Init(i: Interp) -> c_int;
        fn Tk_GetNumMainWindows() -> c_int;
        fn Tk_MainWindow(i: Interp) -> *mut c_void;
    }
}

fn api() -> &'static TclApi {
    tcl_api().ok().expect("Tcl library not loaded")
}

#[inline]
unsafe fn incr(o: *mut TclObj) {
    unsafe { (*o).ref_count += 1 }
}

#[inline]
unsafe fn decr(o: *mut TclObj) {
    unsafe {
        (*o).ref_count -= 1;
        if (*o).ref_count <= 0 {
            (api().TclFreeObj)(o);
        }
    }
}

/// Owned Tcl object reference (increments on creation, decrements on drop).
struct TRef(*mut TclObj);
impl TRef {
    fn new(o: *mut TclObj) -> TRef {
        unsafe { incr(o) };
        TRef(o)
    }
}
impl Drop for TRef {
    fn drop(&mut self) {
        unsafe { decr(self.0) }
    }
}

// ------------------------------------------------------------------ module state

static TCL_ERROR_TYPE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
/// Exception raised by a Python callback while Tcl was dispatching (reported by
/// `mainloop`/`dooneevent`).  Owned reference or null.
static ERROR_IN_CMD: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static BUSY_WAIT_MS: AtomicI32 = AtomicI32::new(20);

struct KnownTypes {
    boolean: usize,
    boolean_string: usize,
    bytearray: usize,
    double: usize,
    int: usize,
    wide_int: usize,
    bignum: usize,
    list: usize,
    string: usize,
}
static TYPES: std::sync::OnceLock<KnownTypes> = std::sync::OnceLock::new();

fn known() -> &'static KnownTypes {
    TYPES.get_or_init(|| unsafe {
        let a = api();
        let g = |n: &std::ffi::CStr| (a.Tcl_GetObjType)(n.as_ptr()) as usize;
        let mut k = KnownTypes {
            boolean: g(c"boolean"),
            boolean_string: g(c"booleanString"),
            bytearray: g(c"bytearray"),
            double: g(c"double"),
            int: g(c"int"),
            wide_int: g(c"wideInt"),
            bignum: g(c"bignum"),
            list: g(c"list"),
            string: g(c"string"),
        };
        // Types that are not registered by name: learn them from sample values.
        let probe = |o: *mut TclObj| {
            incr(o);
            let t = (*o).type_ptr as usize;
            decr(o);
            t
        };
        if k.list == 0 {
            k.list = probe((a.Tcl_NewListObj)(0, std::ptr::null()));
        }
        if k.int == 0 {
            k.int = probe((a.Tcl_NewWideIntObj)(1));
        }
        if k.double == 0 {
            k.double = probe((a.Tcl_NewDoubleObj)(1.5));
        }
        if k.bytearray == 0 {
            k.bytearray = probe((a.Tcl_NewByteArrayObj)(b"x".as_ptr(), 1));
        }
        // values that acquire their type when parsed
        let parsed = |text: &[u8], how: u8| {
            let o = (a.Tcl_NewStringObj)(text.as_ptr() as *const c_char, text.len() as c_int);
            incr(o);
            let mut mp = [0u64; 8];
            let mut b: c_int = 0;
            let t = match how {
                0 => {
                    (a.Tcl_GetBooleanFromObj)(std::ptr::null_mut(), o, &mut b);
                    (*o).type_ptr as usize
                }
                _ => {
                    if (a.Tcl_GetBignumFromObj)(std::ptr::null_mut(), o, mp.as_mut_ptr() as *mut c_void) == TCL_OK {
                        let n = (a.Tcl_NewBignumObj)(mp.as_mut_ptr() as *mut c_void);
                        incr(n);
                        let t = (*n).type_ptr as usize;
                        decr(n);
                        t
                    } else {
                        0
                    }
                }
            };
            decr(o);
            t
        };
        if k.boolean_string == 0 {
            k.boolean_string = parsed(b"true", 0);
        }
        if k.bignum == 0 {
            k.bignum = parsed(b"123456789012345678901234567890", 1);
        }
        k
    })
}

fn tcl_error_type() -> *mut PyTypeObject {
    let p = TCL_ERROR_TYPE.load(Ordering::Acquire);
    if p.is_null() { exceptions::RuntimeError() } else { p as *mut PyTypeObject }
}

fn tcl_error(msg: PyRef) -> PyErr {
    set_err_obj(tcl_error_type(), msg)
}

fn tcl_error_str(msg: &str) -> PyErr {
    new_err(tcl_error_type(), msg)
}

// ------------------------------------------------------------------ string conversion

/// Encode a Python str in Tcl's internal (modified UTF-8) form.
fn encode_str(o: &PyObject) -> Vec<u8> {
    if let Some(s) = pystr::ascii_str(o) {
        if !s.as_bytes().contains(&0) {
            return s.as_bytes().to_vec();
        }
    }
    let n = pystr::str_len(o);
    let mut out = Vec::with_capacity(n + 8);
    let mut push = |c: u32, out: &mut Vec<u8>| {
        if c == 0 {
            out.extend_from_slice(&[0xC0, 0x80]);
        } else if c < 0x80 {
            out.push(c as u8);
        } else if (0xDC80..=0xDCFF).contains(&c) {
            // surrogateescape'd raw byte
            out.push((c - 0xDC00) as u8);
        } else if c < 0x800 {
            out.push(0xC0 | (c >> 6) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else if c < 0x10000 {
            out.push(0xE0 | (c >> 12) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else {
            // outside the BMP: a surrogate pair, each half in 3-byte form
            let v = c - 0x10000;
            for h in [0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)] {
                out.push(0xE0 | (h >> 12) as u8);
                out.push(0x80 | ((h >> 6) & 0x3F) as u8);
                out.push(0x80 | (h & 0x3F) as u8);
            }
        }
    };
    match pystr::str_data(o) {
        pystr::StrData::L1(s) => s.iter().for_each(|&c| push(c as u32, &mut out)),
        pystr::StrData::U2(s) => s.iter().for_each(|&c| push(c as u32, &mut out)),
        pystr::StrData::U4(s) => s.iter().for_each(|&c| push(c, &mut out)),
    }
    out
}

#[inline]
fn cont(b: &[u8], i: usize) -> Option<u32> {
    match b.get(i) {
        Some(&c) if c & 0xC0 == 0x80 => Some((c & 0x3F) as u32),
        _ => None,
    }
}

/// Decode one 3-byte surrogate encoding at `i` (ED A0..BF xx).
fn surrogate_at(b: &[u8], i: usize) -> Option<u32> {
    if b.get(i) == Some(&0xED) {
        if let (Some(&s), Some(c2)) = (b.get(i + 1), cont(b, i + 2)) {
            if (0xA0..=0xBF).contains(&s) {
                return Some(0xD000 | (((s & 0x3F) as u32) << 6) | c2);
            }
        }
    }
    None
}

/// Decode a Tcl string (modified UTF-8, possibly CESU-8 pairs) into a Python str.
pub fn decode_bytes(b: &[u8]) -> PyRef {
    if b.is_ascii() {
        return pystr::new_str(unsafe { std::str::from_utf8_unchecked(b) });
    }
    let mut cps: Vec<u32> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            cps.push(c as u32);
            i += 1;
            continue;
        }
        if c == 0xC0 && b.get(i + 1) == Some(&0x80) {
            cps.push(0);
            i += 2;
            continue;
        }
        if let Some(hi) = surrogate_at(b, i) {
            if hi < 0xDC00 {
                if let Some(lo) = surrogate_at(b, i + 3) {
                    if lo >= 0xDC00 {
                        cps.push(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                        i += 6;
                        continue;
                    }
                }
            }
        }
        let dec = match c {
            0xC2..=0xDF => cont(b, i + 1).map(|c1| ((((c & 0x1F) as u32) << 6) | c1, 2)),
            0xE0..=0xEF => {
                let lo_ok = match (c, b.get(i + 1)) {
                    (0xE0, Some(&s)) => (0xA0..=0xBF).contains(&s),
                    (0xED, Some(&s)) => (0x80..=0x9F).contains(&s),
                    (_, Some(&s)) => (0x80..=0xBF).contains(&s),
                    _ => false,
                };
                if lo_ok {
                    match (cont(b, i + 1), cont(b, i + 2)) {
                        (Some(c1), Some(c2)) => Some(((((c & 0x0F) as u32) << 12) | (c1 << 6) | c2, 3)),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            0xF0..=0xF4 => {
                let lo_ok = match (c, b.get(i + 1)) {
                    (0xF0, Some(&s)) => (0x90..=0xBF).contains(&s),
                    (0xF4, Some(&s)) => (0x80..=0x8F).contains(&s),
                    (_, Some(&s)) => (0x80..=0xBF).contains(&s),
                    _ => false,
                };
                if lo_ok {
                    match (cont(b, i + 1), cont(b, i + 2), cont(b, i + 3)) {
                        (Some(c1), Some(c2), Some(c3)) => Some(((((c & 0x07) as u32) << 18) | (c1 << 12) | (c2 << 6) | c3, 4)),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        match dec {
            Some((cp, n)) => {
                cps.push(cp);
                i += n;
            }
            None => {
                cps.push(0xDC00 + c as u32);
                i += 1;
            }
        }
    }
    pystr::from_codepoints(&cps)
}

unsafe fn obj_bytes<'a>(o: *mut TclObj) -> &'a [u8] {
    unsafe {
        let mut n: c_int = 0;
        let p = (api().Tcl_GetStringFromObj)(o, &mut n);
        if p.is_null() || n <= 0 { &[] } else { std::slice::from_raw_parts(p as *const u8, n as usize) }
    }
}

unsafe fn obj_str(o: *mut TclObj) -> PyRef {
    unsafe { decode_bytes(obj_bytes(o)) }
}

fn new_tcl_string(b: &[u8]) -> PyResult<*mut TclObj> {
    if b.len() > i32::MAX as usize {
        raise!(OverflowError, "string is too long");
    }
    Ok(unsafe { (api().Tcl_NewStringObj)(b.as_ptr() as *const c_char, b.len() as c_int) })
}

fn cstring(b: Vec<u8>) -> PyResult<CString> {
    if b.len() > i32::MAX as usize {
        raise!(OverflowError, "string is too long");
    }
    CString::new(b).map_err(|_| value_error("embedded null byte"))
}

/// A `str` argument (rejecting embedded NUL characters), Tcl-encoded.
fn str_arg(fname: &str, o: &PyObject) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument must be str, not {}", fname, typeobject::tname(o));
    }
    if str_has_nul(o) {
        raise!(ValueError, "embedded null character");
    }
    cstring(encode_str(o))
}

fn str_has_nul(o: &PyObject) -> bool {
    match pystr::str_data(o) {
        pystr::StrData::L1(s) => s.contains(&0),
        pystr::StrData::U2(s) => s.contains(&0),
        pystr::StrData::U4(s) => s.contains(&0),
    }
}

/// Variable names: str, bytes or Tcl_Obj.
fn varname_arg(o: &PyObject) -> PyResult<CString> {
    if pystr::is_str(o) {
        if str_has_nul(o) {
            raise!(ValueError, "embedded null character");
        }
        return cstring(encode_str(o));
    }
    if bytes::is_bytes(o) {
        let b = bytes::bytes_data(o);
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        return cstring(b.to_vec());
    }
    if is_tclobj(o) {
        return cstring(unsafe { obj_bytes(tclobj_value(o)) }.to_vec());
    }
    raise!(TypeError, "must be str, bytes or Tcl_Obj, not {}", typeobject::tname(o));
}

// ------------------------------------------------------------------ Python <-> Tcl values

/// Convert a Python value into a new (refcount 0) Tcl object.
fn as_obj(v: &PyObject) -> PyResult<*mut TclObj> {
    let a = api();
    unsafe {
        if pystr::is_str(v) {
            return new_tcl_string(&encode_str(v));
        }
        if bool_::is_bool(v) {
            return Ok((a.Tcl_NewBooleanObj)(ab::is_true(v)? as c_int));
        }
        if int::is_int(v) {
            if let Some(i) = int::as_i64(v) {
                return Ok((a.Tcl_NewWideIntObj)(i));
            }
            // big integer: let Tcl parse the decimal form into a bignum
            let mut digits = int::int_methods_mag_string(v, 10);
            if int::sign(v) < 0 {
                digits.insert(0, '-');
            }
            let s = pystr::new_str(&digits);
            let so = TRef::new(new_tcl_string(&encode_str(&s))?);
            let mut mp = [0u64; 8];
            if (a.Tcl_GetBignumFromObj)(std::ptr::null_mut(), so.0, mp.as_mut_ptr() as *mut c_void) != TCL_OK {
                raise!(OverflowError, "int too large to convert");
            }
            return Ok((a.Tcl_NewBignumObj)(mp.as_mut_ptr() as *mut c_void));
        }
        if float::is_float(v) {
            return Ok((a.Tcl_NewDoubleObj)(float::float_value(v)));
        }
        if bytes::is_bytes(v) || crate::types::bytearray::is_bytearray(v) {
            let d = bytes::bytes_like_data(v).unwrap_or_default();
            if d.len() > i32::MAX as usize {
                raise!(OverflowError, "bytes object is too long");
            }
            return Ok((a.Tcl_NewByteArrayObj)(d.as_ptr(), d.len() as c_int));
        }
        if tuple::is_tuple(v) || list::is_list(v) {
            // snapshot first: converting an item may run code that mutates the list
            let items: Vec<PyRef> = if tuple::is_tuple(v) { tuple::tuple_to_vec(v) } else { list::list_to_vec(v) };
            if items.len() > i32::MAX as usize {
                raise!(OverflowError, "list is too long");
            }
            let mut objs: Vec<TRef> = Vec::with_capacity(items.len());
            for it in &items {
                objs.push(TRef::new(fc::with_recursion(" while converting to a Tcl list", || as_obj(it))?));
            }
            let raw: Vec<*mut TclObj> = objs.iter().map(|t| t.0).collect();
            return Ok((a.Tcl_NewListObj)(raw.len() as c_int, raw.as_ptr()));
        }
        if is_tclobj(v) {
            return Ok(tclobj_value(v));
        }
        let s = ab::str(v)?;
        new_tcl_string(&encode_str(&s))
    }
}

/// Convert a Tcl object into a Python value (the `wantobjects` conversion).
fn from_obj(o: *mut TclObj) -> PyResult {
    let a = api();
    let k = known();
    unsafe {
        let t = (*o).type_ptr as usize;
        if t == 0 {
            return Ok(obj_str(o));
        }
        if t == k.boolean || t == k.boolean_string {
            let mut b: c_int = 0;
            if (a.Tcl_GetBooleanFromObj)(std::ptr::null_mut(), o, &mut b) == TCL_OK {
                return Ok(bool_::new_bool(b != 0));
            }
            return Ok(obj_str(o));
        }
        if t == k.bytearray {
            let mut n: c_int = 0;
            let p = (a.Tcl_GetByteArrayFromObj)(o, &mut n);
            let s = if p.is_null() || n <= 0 { &[][..] } else { std::slice::from_raw_parts(p, n as usize) };
            return Ok(bytes::new_bytes(s));
        }
        if t == k.double {
            // read the internal value directly: Tcl refuses to hand out NaN
            return Ok(float::new_float(f64::from_bits((*o).internal[0] as u64)));
        }
        if t == k.int || t == k.wide_int {
            let mut w: i64 = 0;
            if (a.Tcl_GetWideIntFromObj)(std::ptr::null_mut(), o, &mut w) == TCL_OK {
                return Ok(int::new_int(w));
            }
            return bignum_from_string(o);
        }
        if t == k.bignum {
            return bignum_from_string(o);
        }
        if t == k.list {
            let mut n: c_int = 0;
            let mut v: *mut *mut TclObj = std::ptr::null_mut();
            if (a.Tcl_ListObjGetElements)(std::ptr::null_mut(), o, &mut n, &mut v) != TCL_OK {
                return Ok(obj_str(o));
            }
            // keep the list alive while converting its elements
            let _keep = TRef::new(o);
            let elems: Vec<*mut TclObj> = (0..n as usize).map(|i| *v.add(i)).collect();
            let mut out = Vec::with_capacity(elems.len());
            for e in elems {
                out.push(fc::with_recursion(" while converting a Tcl list", || from_obj(e))?);
            }
            return Ok(tuple::new_tuple(out));
        }
        if t == k.string {
            return Ok(obj_str(o));
        }
        new_tclobj(o)
    }
}

unsafe fn bignum_from_string(o: *mut TclObj) -> PyResult {
    let s = unsafe { obj_bytes(o) };
    let txt = std::str::from_utf8(s).unwrap_or("").trim();
    let (neg, digits) = match txt.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, txt.strip_prefix('+').unwrap_or(txt)),
    };
    if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
        return crate::types::int_methods::from_decimal_str(neg, digits);
    }
    // not plain decimal (e.g. 0x..): let Python parse it with base 0
    ab::call(typeobject::type_obj(int::int_type()), &[&decode_bytes(s), &int::new_int(0)])
}

// ------------------------------------------------------------------ Tcl_Obj wrapper

struct TclObjW {
    value: *mut TclObj,
    string: Option<PyRef>,
}
impl Trav for TclObjW {}
impl Drop for TclObjW {
    fn drop(&mut self) {
        if !self.value.is_null() {
            unsafe { decr(self.value) };
        }
    }
}

#[unsafe(no_mangle)]
pub static mut RPY_TKINTER_TCLOBJ_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut RPY_TKINTER_TKAPP_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut RPY_TKINTER_TKTT_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn tclobj_type() -> *mut PyTypeObject {
    &raw mut RPY_TKINTER_TCLOBJ_TYPE
}
fn tkapp_type() -> *mut PyTypeObject {
    &raw mut RPY_TKINTER_TKAPP_TYPE
}
fn tktt_type() -> *mut PyTypeObject {
    &raw mut RPY_TKINTER_TKTT_TYPE
}

fn is_tclobj(o: &PyObject) -> bool {
    o.type_ptr() == tclobj_type()
}

fn tclobj_value(o: &PyObject) -> *mut TclObj {
    fc::rs::<TclObjW>(o).value
}

fn new_tclobj(o: *mut TclObj) -> PyResult {
    unsafe { incr(o) };
    fc::robj_new(tclobj_type(), TclObjW { value: o, string: None })
}

fn tclobj_string(o: &PyObject) -> PyRef {
    let w = fc::rs::<TclObjW>(o);
    if let Some(s) = &w.string {
        return s.clone();
    }
    let s = unsafe { obj_str(w.value) };
    w.string = Some(s.clone());
    s
}

fn tclobj_typename_s(o: &PyObject) -> String {
    let w = fc::rs::<TclObjW>(o);
    unsafe {
        let t = (*w.value).type_ptr;
        if t.is_null() || (*t).name.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr((*t).name).to_string_lossy().into_owned()
        }
    }
}

fn tclobj_get_string(o: &PyObject) -> PyResult {
    Ok(tclobj_string(o))
}
fn tclobj_get_typename(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&tclobj_typename_s(o)))
}

unsafe extern "C" fn tclobj_str(o: *mut PyObject) -> *mut PyObject {
    tclobj_string(unsafe { &*o }).into_raw()
}

unsafe extern "C" fn tclobj_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    let r = (|| -> PyResult {
        let s = ab::repr_string(&tclobj_string(o))?;
        Ok(pystr::new_str(&format!("<{} object: {}>", tclobj_typename_s(o), s)))
    })();
    into_c(r)
}

unsafe extern "C" fn tclobj_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    let (a, b) = unsafe { (&*a, &*b) };
    if !is_tclobj(a) || !is_tclobj(b) {
        return none::not_implemented().to_ref().into_raw();
    }
    let ord = if tclobj_value(a) == tclobj_value(b) {
        std::cmp::Ordering::Equal
    } else {
        pystr::str_cmp(&tclobj_string(a), &tclobj_string(b))
    };
    use std::cmp::Ordering::*;
    let r = match op {
        Py_LT => ord == Less,
        Py_LE => ord != Greater,
        Py_EQ => ord == Equal,
        Py_NE => ord != Equal,
        Py_GT => ord == Greater,
        _ => ord != Less,
    };
    bool_::new_bool(r).into_raw()
}

// ------------------------------------------------------------------ the tkapp object

struct App {
    interp: Interp,
    thread: TclThreadId,
    wantobjects: AtomicI32,
    dispatching: AtomicBool,
    quit: AtomicBool,
    tk_loaded: AtomicBool,
    trace: Option<PyRef>,
}
impl Trav for App {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        crate::trav!(v; self.trace)
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if !self.interp.is_null() {
            if let Ok(a) = tcl_api() {
                unsafe {
                    if self.thread == (a.Tcl_GetCurrentThread)() {
                        (a.Tcl_DeleteInterp)(self.interp);
                    }
                }
            }
            self.interp = std::ptr::null_mut();
        }
    }
}

fn app(o: &PyObject) -> &mut App {
    fc::rs::<App>(o)
}

/// Run `f` with the thread in Blocked mode (Tcl may wait for events).
fn tcl_blocking<R>(f: impl FnOnce() -> R) -> R {
    crate::threads::blocking(f)
}

/// Run Python code from inside a Tcl callback.
fn in_vm<R>(f: impl FnOnce() -> R) -> R {
    let ts = crate::threads::current();
    let prev = ts.mode.replace(ThreadMode::InVm);
    let r = f();
    ts.mode.set(prev);
    r
}

fn set_error_in_cmd(e: Option<PyRef>) {
    let new = e.map_or(std::ptr::null_mut(), |r| r.into_raw());
    let old = ERROR_IN_CMD.swap(new, Ordering::AcqRel);
    if !old.is_null() {
        unsafe { drop(PyRef::from_raw(old)) };
    }
}

fn take_error_in_cmd() -> Option<PyRef> {
    let p = ERROR_IN_CMD.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if p.is_null() { None } else { Some(unsafe { PyRef::from_raw(p) }) }
}

/// The interpreter result as a TclError.
fn app_error(app: &App) -> PyErr {
    let s = unsafe { obj_str((api().Tcl_GetObjResult)(app.interp)) };
    tcl_error(s)
}

fn result_str(app: &App) -> PyRef {
    unsafe { obj_str((api().Tcl_GetObjResult)(app.interp)) }
}

fn result_obj(app: &App) -> PyResult {
    let r = unsafe { (api().Tcl_GetObjResult)(app.interp) };
    if app.wantobjects.load(Ordering::Relaxed) != 0 { from_obj(r) } else { Ok(unsafe { obj_str(r) }) }
}

// ---- running work on the interpreter's thread

struct Job {
    work: Option<Box<dyn FnOnce() -> PyResult>>,
    result: Option<Result<PyRef, Option<PyRef>>>,
    done: bool,
}
struct JobCell {
    m: Mutex<Job>,
    cv: Condvar,
}
struct SendPtr<T>(*const T);
unsafe impl<T> Send for SendPtr<T> {}

#[repr(C)]
struct JobEvent {
    header: TclEvent,
    job: *const JobCell,
}

unsafe extern "C" fn job_event_proc(ev: *mut TclEvent, _flags: c_int) -> c_int {
    unsafe {
        let cell = &*(*(ev as *mut JobEvent)).job;
        let work = cell.m.lock().unwrap_or_else(|e| e.into_inner()).work.take();
        if let Some(w) = work {
            let r = in_vm(|| match w() {
                Ok(v) => Ok(v),
                Err(_) => Err(take_exc()),
            });
            let mut g = cell.m.lock().unwrap_or_else(|e| e.into_inner());
            g.result = Some(r);
            g.done = true;
            cell.cv.notify_all();
        }
    }
    1
}

fn on_interp_thread(app: &App) -> bool {
    unsafe { (api().Tcl_GetCurrentThread)() == app.thread }
}

/// Run `f` on the interpreter's thread (directly when already there).
fn run_in_interp(app: &App, f: Box<dyn FnOnce() -> PyResult>) -> PyResult {
    if on_interp_thread(app) {
        return f();
    }
    // The owner thread must be dispatching events; give it a moment to get there.
    if !app.dispatching.load(Ordering::Acquire) {
        let ok = tcl_blocking(|| {
            for _ in 0..10 {
                if app.dispatching.load(Ordering::Acquire) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            app.dispatching.load(Ordering::Acquire)
        });
        if !ok {
            raise!(RuntimeError, "main thread is not in main loop");
        }
    }
    let cell = Box::new(JobCell { m: Mutex::new(Job { work: Some(f), result: None, done: false }), cv: Condvar::new() });
    let a = api();
    unsafe {
        let ev = (a.Tcl_Alloc)(std::mem::size_of::<JobEvent>() as u32) as *mut JobEvent;
        std::ptr::write(ev, JobEvent { header: TclEvent { proc_: job_event_proc, next: std::ptr::null_mut() }, job: &*cell });
        (a.Tcl_ThreadQueueEvent)(app.thread, ev as *mut TclEvent, TCL_QUEUE_TAIL);
        (a.Tcl_ThreadAlert)(app.thread);
    }
    let cellp = SendPtr(&*cell as *const JobCell);
    let r = tcl_blocking(move || {
        let cell = unsafe { &*cellp.0 };
        let mut g = cell.m.lock().unwrap_or_else(|e| e.into_inner());
        while !g.done {
            g = cell.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.result.take()
    });
    drop(cell);
    match r {
        Some(Ok(v)) => Ok(v),
        Some(Err(Some(e))) => {
            set_exc(e);
            Err(PyErr)
        }
        _ => Err(system_error("Tcl call from another thread failed")),
    }
}

/// Borrow a Python object across the thread hop (the caller keeps it alive while waiting).
#[derive(Clone, Copy)]
struct Hold(*mut PyObject);
impl Hold {
    fn new(o: &PyObject) -> Hold {
        Hold(o.as_ptr())
    }
    fn get(&self) -> &PyObject {
        unsafe { &*self.0 }
    }
}

macro_rules! on_interp {
    ($app:expr, $body:expr) => {{
        let selfp = Hold::new($app);
        run_in_interp(app($app), Box::new(move || {
            let a: &App = app(selfp.get());
            $body(a)
        }))
    }};
}

// ---- methods

fn tk_call(o: &PyObject, args: &[&PyObject]) -> PyResult {
    // a single tuple argument stands for the whole argument list
    let mut owned: Vec<PyRef> = if args.len() == 1 && tuple::is_tuple(args[0]) {
        tuple::tuple_to_vec(args[0])
    } else {
        args.iter().map(|a| a.to_ref()).collect()
    };
    // A None argument ends the argument list (tkinter passes optional
    // trailing arguments as None).
    if let Some(n) = owned.iter().position(|v| none::is_none(v)) {
        owned.truncate(n);
    }
    if let Some(tr) = app(o).trace.clone() {
        ab::call(&tr, &[&tuple::new_tuple(owned.clone())])?;
    }
    on_interp!(o, move |a: &App| {
        let mut objs: Vec<TRef> = Vec::with_capacity(owned.len());
        for v in &owned {
            objs.push(TRef::new(as_obj(v)?));
        }
        let raw: Vec<*mut TclObj> = objs.iter().map(|t| t.0).collect();
        let interp = SendPtr(a.interp as *const c_void);
        let rawp = SendPtr(raw.as_ptr());
        let n = raw.len() as c_int;
        let rc = tcl_blocking(move || unsafe {
            (api().Tcl_EvalObjv)(interp.0 as Interp, n, rawp.0, TCL_EVAL_DIRECT | TCL_EVAL_GLOBAL)
        });
        drop(objs);
        if rc == TCL_ERROR {
            return Err(app_error(a));
        }
        result_obj(a)
    })
}

fn eval_script(a: &App, script: &CString, how: u8) -> c_int {
    let interp = SendPtr(a.interp as *const c_void);
    let sp = SendPtr(script.as_ptr());
    let n = script.as_bytes().len() as c_int;
    tcl_blocking(move || unsafe {
        let i = interp.0 as Interp;
        match how {
            0 => (api().Tcl_EvalEx)(i, sp.0, n, 0),
            1 => (api().Tcl_EvalFile)(i, sp.0),
            _ => (api().Tcl_RecordAndEval)(i, sp.0, 0),
        }
    })
}

fn tk_eval_common(o: &PyObject, arg: &PyObject, fname: &'static str, how: u8) -> PyResult {
    let s = str_arg(fname, arg)?;
    if let Some(tr) = app(o).trace.clone() {
        ab::call(&tr, &[&tuple::new_tuple(vec![pystr::new_str(fname), arg.to_ref()])])?;
    }
    on_interp!(o, move |a: &App| {
        if eval_script(a, &s, how) == TCL_ERROR {
            return Err(app_error(a));
        }
        Ok(result_str(a))
    })
}

fn tk_eval(o: &PyObject, arg: &PyObject) -> PyResult {
    tk_eval_common(o, arg, "eval", 0)
}
fn tk_evalfile(o: &PyObject, arg: &PyObject) -> PyResult {
    tk_eval_common(o, arg, "evalfile", 1)
}
fn tk_record(o: &PyObject, arg: &PyObject) -> PyResult {
    tk_eval_common(o, arg, "record", 2)
}

fn tk_adderrorinfo(o: &PyObject, arg: &PyObject) -> PyResult {
    let s = str_arg("adderrorinfo", arg)?;
    on_interp!(o, move |a: &App| {
        unsafe { (api().Tcl_AddErrorInfo)(a.interp, s.as_ptr()) };
        Ok(none::none_ref())
    })
}

fn set_var(o: &PyObject, args: &[&PyObject], flags: c_int, fname: &str) -> PyResult {
    let (n1, n2, val) = match args.len() {
        2 => (varname_arg(args[0])?, None, args[1].to_ref()),
        3 => (str_arg(fname, args[0])?, Some(str_arg(fname, args[1])?), args[2].to_ref()),
        n => raise!(TypeError, "{} expected 2 or 3 arguments, got {}", fname, n),
    };
    on_interp!(o, move |a: &App| {
        let v = TRef::new(as_obj(&val)?);
        let r = unsafe {
            (api().Tcl_SetVar2Ex)(a.interp, n1.as_ptr(), n2.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), v.0, flags | TCL_LEAVE_ERR_MSG)
        };
        if r.is_null() {
            return Err(app_error(a));
        }
        Ok(none::none_ref())
    })
}

fn get_var(o: &PyObject, args: &[&PyObject], flags: c_int, fname: &str) -> PyResult {
    let (n1, n2) = match args.len() {
        1 => (varname_arg(args[0])?, None),
        2 => (varname_arg(args[0])?, Some(str_arg(fname, args[1])?)),
        n => raise!(TypeError, "{} expected 1 or 2 arguments, got {}", fname, n),
    };
    on_interp!(o, move |a: &App| {
        let r = unsafe { (api().Tcl_GetVar2Ex)(a.interp, n1.as_ptr(), n2.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), flags | TCL_LEAVE_ERR_MSG) };
        if r.is_null() {
            return Err(app_error(a));
        }
        if a.wantobjects.load(Ordering::Relaxed) != 0 { from_obj(r) } else { Ok(unsafe { obj_str(r) }) }
    })
}

fn unset_var(o: &PyObject, args: &[&PyObject], flags: c_int, fname: &str) -> PyResult {
    let (n1, n2) = match args.len() {
        1 => (str_arg(fname, args[0])?, None),
        2 => (str_arg(fname, args[0])?, Some(str_arg(fname, args[1])?)),
        n => raise!(TypeError, "{} expected 1 or 2 arguments, got {}", fname, n),
    };
    on_interp!(o, move |a: &App| {
        let r = unsafe { (api().Tcl_UnsetVar2)(a.interp, n1.as_ptr(), n2.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), flags | TCL_LEAVE_ERR_MSG) };
        if r == TCL_ERROR {
            return Err(app_error(a));
        }
        Ok(none::none_ref())
    })
}

fn tk_setvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    set_var(o, args, 0, "setvar")
}
fn tk_globalsetvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    set_var(o, args, TCL_GLOBAL_ONLY, "globalsetvar")
}
fn tk_getvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    get_var(o, args, 0, "getvar")
}
fn tk_globalgetvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    get_var(o, args, TCL_GLOBAL_ONLY, "globalgetvar")
}
fn tk_unsetvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    unset_var(o, args, 0, "unsetvar")
}
fn tk_globalunsetvar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    unset_var(o, args, TCL_GLOBAL_ONLY, "globalunsetvar")
}

/// Argument of getint/getdouble/getboolean as a Tcl object (str or Tcl_Obj).
fn conv_arg(fname: &str, v: &PyObject) -> PyResult<TRef> {
    if is_tclobj(v) {
        return Ok(TRef::new(tclobj_value(v)));
    }
    let s = str_arg(fname, v)?;
    Ok(TRef::new(new_tcl_string(s.as_bytes())?))
}

fn tk_getint(o: &PyObject, v: &PyObject) -> PyResult {
    if int::is_int(v) {
        return Ok(v.to_ref());
    }
    let arg = conv_arg("getint", v)?;
    let argp = SendPtr(arg.0 as *const TclObj);
    on_interp!(o, move |a: &App| unsafe {
        let x = argp.0 as *mut TclObj;
        // (Tcl_GetWideIntFromObj would silently wrap values in 2**63..2**64)
        let mut mp = [0u64; 8];
        if (api().Tcl_GetBignumFromObj)(a.interp, x, mp.as_mut_ptr() as *mut c_void) != TCL_OK {
            return Err(app_error(a));
        }
        let b = TRef::new((api().Tcl_NewBignumObj)(mp.as_mut_ptr() as *mut c_void));
        let k = known();
        let t = (*b.0).type_ptr as usize;
        if t != 0 && (t == k.int || t == k.wide_int) {
            let mut w: i64 = 0;
            if (api().Tcl_GetWideIntFromObj)(std::ptr::null_mut(), b.0, &mut w) == TCL_OK {
                return Ok(int::new_int(w));
            }
        }
        bignum_from_string(b.0)
    })
}

fn tk_getdouble(o: &PyObject, v: &PyObject) -> PyResult {
    if float::is_float(v) {
        return Ok(v.to_ref());
    }
    if int::is_int(v) {
        return Ok(float::new_float(int::to_f64(v)?));
    }
    let arg = conv_arg("getdouble", v)?;
    let argp = SendPtr(arg.0 as *const TclObj);
    on_interp!(o, move |a: &App| unsafe {
        let mut d = 0.0;
        if (api().Tcl_GetDoubleFromObj)(a.interp, argp.0 as *mut TclObj, &mut d) != TCL_OK {
            return Err(app_error(a));
        }
        Ok(float::new_float(d))
    })
}

fn tk_getboolean(o: &PyObject, v: &PyObject) -> PyResult {
    if int::is_int(v) {
        return Ok(bool_::new_bool(ab::is_true(v)?));
    }
    // Strings must be boolean words or 0/1 (not any number, as for objects).
    let text = if is_tclobj(v) { None } else { Some(str_arg("getboolean", v)?) };
    let arg = conv_arg("getboolean", v)?;
    let argp = SendPtr(arg.0 as *const TclObj);
    on_interp!(o, move |a: &App| unsafe {
        let mut b: c_int = 0;
        let rc = match &text {
            Some(s) => (api().Tcl_GetBoolean)(a.interp, s.as_ptr(), &mut b),
            None => (api().Tcl_GetBooleanFromObj)(a.interp, argp.0 as *mut TclObj, &mut b),
        };
        if rc != TCL_OK {
            return Err(app_error(a));
        }
        Ok(bool_::new_bool(b != 0))
    })
}

fn expr_common(o: &PyObject, v: &PyObject, fname: &'static str, kind: u8) -> PyResult {
    let s = str_arg(fname, v)?;
    on_interp!(o, move |a: &App| unsafe {
        let ap = api();
        let interp = SendPtr(a.interp as *const c_void);
        let sp = SendPtr(s.as_ptr());
        match kind {
            0 => {
                if tcl_blocking(move || (ap.Tcl_ExprString)(interp.0 as Interp, sp.0)) == TCL_ERROR {
                    return Err(app_error(a));
                }
                Ok(result_str(a))
            }
            1 => {
                let mut r: c_long = 0;
                let rp = SendPtr(&mut r as *mut c_long as *const c_long);
                if tcl_blocking(move || (ap.Tcl_ExprLong)(interp.0 as Interp, sp.0, rp.0 as *mut c_long)) == TCL_ERROR {
                    return Err(app_error(a));
                }
                Ok(int::new_int(r as i64))
            }
            2 => {
                let mut r: f64 = 0.0;
                let rp = SendPtr(&mut r as *mut f64 as *const f64);
                if tcl_blocking(move || (ap.Tcl_ExprDouble)(interp.0 as Interp, sp.0, rp.0 as *mut f64)) == TCL_ERROR {
                    return Err(app_error(a));
                }
                Ok(float::new_float(r))
            }
            _ => {
                let mut r: c_int = 0;
                let rp = SendPtr(&mut r as *mut c_int as *const c_int);
                if tcl_blocking(move || (ap.Tcl_ExprBoolean)(interp.0 as Interp, sp.0, rp.0 as *mut c_int)) == TCL_ERROR {
                    return Err(app_error(a));
                }
                Ok(int::new_int((r != 0) as i64))
            }
        }
    })
}

fn tk_exprstring(o: &PyObject, v: &PyObject) -> PyResult {
    expr_common(o, v, "exprstring", 0)
}
fn tk_exprlong(o: &PyObject, v: &PyObject) -> PyResult {
    expr_common(o, v, "exprlong", 1)
}
fn tk_exprdouble(o: &PyObject, v: &PyObject) -> PyResult {
    expr_common(o, v, "exprdouble", 2)
}
fn tk_exprboolean(o: &PyObject, v: &PyObject) -> PyResult {
    expr_common(o, v, "exprboolean", 3)
}

fn tk_splitlist(o: &PyObject, v: &PyObject) -> PyResult {
    if tuple::is_tuple(v) {
        return Ok(v.to_ref());
    }
    if list::is_list(v) {
        return Ok(tuple::new_tuple(list::list_to_vec(v)));
    }
    let src: TRef = if is_tclobj(v) {
        TRef::new(tclobj_value(v))
    } else if pystr::is_str(v) {
        if str_has_nul(v) {
            raise!(ValueError, "embedded null character");
        }
        TRef::new(new_tcl_string(&encode_str(v))?)
    } else if bytes::is_bytes(v) {
        let b = bytes::bytes_data(v);
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        TRef::new(new_tcl_string(b)?)
    } else {
        raise!(TypeError, "splitlist() argument must be str, bytes or Tcl_Obj, not {}", typeobject::tname(v));
    };
    let srcp = SendPtr(src.0 as *const TclObj);
    on_interp!(o, move |a: &App| unsafe {
        let mut n: c_int = 0;
        let mut items: *mut *mut TclObj = std::ptr::null_mut();
        let x = srcp.0 as *mut TclObj;
        if (api().Tcl_ListObjGetElements)(a.interp, x, &mut n, &mut items) != TCL_OK {
            return Err(app_error(a));
        }
        let elems: Vec<*mut TclObj> = (0..n as usize).map(|i| *items.add(i)).collect();
        let _keep = TRef::new(x);
        let want = a.wantobjects.load(Ordering::Relaxed) != 0;
        let mut out = Vec::with_capacity(elems.len());
        for e in elems {
            out.push(if want { from_obj(e)? } else { obj_str(e) });
        }
        Ok(tuple::new_tuple(out))
    })
}

// ---- commands implemented in Python

struct CmdData {
    // Borrowed: the interpreter (and with it this command) dies with the app.
    app: *mut PyObject,
    func: PyRef,
}

unsafe extern "C" fn python_cmd(cd: *mut c_void, _interp: Interp, objc: c_int, objv: *const *mut TclObj) -> c_int {
    unsafe {
        let data = &*(cd as *const CmdData);
        in_vm(|| {
            let app_ref = (*data.app).to_ref();
            let a = app(&app_ref);
            let want2 = a.wantobjects.load(Ordering::Relaxed) >= 2;
            let r = (|| -> PyResult {
                let mut args = Vec::with_capacity(objc.max(1) as usize - 1);
                for i in 1..objc as usize {
                    let x = *objv.add(i);
                    args.push(if want2 { from_obj(x)? } else { obj_str(x) });
                }
                let refs: Vec<&PyObject> = args.iter().map(|r| &**r).collect();
                let res = ab::call(&data.func, &refs)?;
                let t = as_obj(&res)?;
                (api().Tcl_SetObjResult)(a.interp, t);
                Ok(none::none_ref())
            })();
            match r {
                Ok(_) => TCL_OK,
                Err(_) => {
                    let e = take_exc();
                    if let Some(e) = &e {
                        if let Ok(s) = ab::str(e) {
                            if let Ok(t) = new_tcl_string(&encode_str(&s)) {
                                (api().Tcl_SetObjResult)(a.interp, t);
                            }
                        }
                    }
                    set_error_in_cmd(e);
                    TCL_ERROR
                }
            }
        })
    }
}

unsafe extern "C" fn python_cmd_delete(cd: *mut c_void) {
    unsafe {
        in_vm(|| drop(Box::from_raw(cd as *mut CmdData)));
    }
}

fn tk_createcommand(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("createcommand", args, 2, 2)?;
    let name = str_arg("createcommand", args[0])?;
    let func = args[1].to_ref();
    if !ab::is_callable(&func) {
        raise!(TypeError, "command not callable");
    }
    let me = Hold::new(o);
    on_interp!(o, move |a: &App| unsafe {
        let data = Box::into_raw(Box::new(CmdData { app: me.0, func }));
        let r = (api().Tcl_CreateObjCommand)(a.interp, name.as_ptr(), python_cmd, data as *mut c_void, Some(python_cmd_delete));
        if r.is_null() {
            drop(Box::from_raw(data));
            return Err(tcl_error_str("can't create Tcl command"));
        }
        Ok(none::none_ref())
    })
}

fn tk_deletecommand(o: &PyObject, v: &PyObject) -> PyResult {
    let name = str_arg("deletecommand", v)?;
    on_interp!(o, move |a: &App| unsafe {
        if (api().Tcl_DeleteCommand)(a.interp, name.as_ptr()) == -1 {
            return Err(tcl_error_str("can't delete Tcl command"));
        }
        Ok(none::none_ref())
    })
}

// ---- file handlers

struct FileData {
    func: PyRef,
    file: PyRef,
}
static FILE_HANDLERS: Mutex<Vec<(c_int, usize)>> = Mutex::new(Vec::new());

unsafe extern "C" fn file_proc(cd: *mut c_void, mask: c_int) {
    unsafe {
        let d = &*(cd as *const FileData);
        in_vm(|| {
            let func = d.func.clone();
            let file = d.file.clone();
            if ab::call(&func, &[&file, &int::new_int(mask as i64)]).is_err() {
                set_error_in_cmd(take_exc());
            }
        });
    }
}

fn remove_file_handler(fd: c_int) {
    let old: Vec<usize> = {
        let mut g = FILE_HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
        let olds = g.iter().filter(|(f, _)| *f == fd).map(|(_, p)| *p).collect();
        g.retain(|(f, _)| *f != fd);
        olds
    };
    for p in old {
        unsafe { drop(Box::from_raw(p as *mut FileData)) };
    }
}

fn tk_createfilehandler(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("createfilehandler", args, 3, 3)?;
    let fd = crate::modules::posix::path::as_file_descriptor(args[0])?;
    let mask = crate::modules::posix::path::c_int(args[1])?;
    if !ab::is_callable(args[2]) {
        raise!(TypeError, "bad argument list");
    }
    let file = args[0].to_ref();
    let func = args[2].to_ref();
    on_interp!(o, move |_a: &App| unsafe {
        remove_file_handler(fd);
        let p = Box::into_raw(Box::new(FileData { func, file }));
        FILE_HANDLERS.lock().unwrap_or_else(|e| e.into_inner()).push((fd, p as usize));
        (api().Tcl_CreateFileHandler)(fd, mask, file_proc, p as *mut c_void);
        Ok(none::none_ref())
    })
}

fn tk_deletefilehandler(o: &PyObject, v: &PyObject) -> PyResult {
    let fd = crate::modules::posix::path::as_file_descriptor(v)?;
    on_interp!(o, move |_a: &App| unsafe {
        (api().Tcl_DeleteFileHandler)(fd);
        remove_file_handler(fd);
        Ok(none::none_ref())
    })
}

// ---- timer handlers

struct Timer {
    token: *mut c_void,
    func: Option<PyRef>,
}
impl Trav for Timer {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        crate::trav!(v; self.func)
    }
}

unsafe extern "C" fn timer_proc(cd: *mut c_void) {
    unsafe {
        in_vm(|| {
            // the token holds a reference to itself while it is pending
            let tok = PyRef::from_raw(cd as *mut PyObject);
            let t = fc::rs::<Timer>(&tok);
            t.token = std::ptr::null_mut();
            if let Some(f) = t.func.take() {
                if ab::call0(&f).is_err() {
                    set_error_in_cmd(take_exc());
                }
            }
            drop(tok);
        });
    }
}

fn tk_createtimerhandler(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("createtimerhandler", args, 2, 2)?;
    let ms = crate::modules::posix::path::c_int(args[0])?;
    if !ab::is_callable(args[1]) {
        raise!(TypeError, "bad argument list");
    }
    let func = args[1].to_ref();
    on_interp!(o, move |_a: &App| unsafe {
        let tok = fc::robj_new(tktt_type(), Timer { token: std::ptr::null_mut(), func: Some(func) })?;
        let selfref = tok.clone().into_raw();
        fc::rs::<Timer>(&tok).token = (api().Tcl_CreateTimerHandler)(ms.max(0), timer_proc, selfref as *mut c_void);
        Ok(tok)
    })
}

fn tt_delete(o: &PyObject) -> PyResult {
    let t = fc::rs::<Timer>(o);
    if !t.token.is_null() {
        unsafe { (api().Tcl_DeleteTimerHandler)(t.token) };
        t.token = std::ptr::null_mut();
        t.func = None;
        // drop the self-reference held for the pending handler
        unsafe { drop(PyRef::from_raw(o.as_ptr())) };
    }
    Ok(none::none_ref())
}

unsafe extern "C" fn tt_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    let r = (|| -> PyResult {
        let f = match &fc::rs::<Timer>(o).func {
            Some(f) => ab::repr_string(f)?,
            None => "None".to_string(),
        };
        Ok(pystr::new_str(&format!("<tktimertoken at {:p}, func={}>", o.as_ptr(), f)))
    })();
    into_c(r)
}

// ---- event loop

fn num_main_windows(a: &App) -> c_int {
    if !a.tk_loaded.load(Ordering::Relaxed) {
        return 0;
    }
    match tk_api() {
        Ok(t) => unsafe { (t.Tk_GetNumMainWindows)() },
        Err(_) => {
            clear_exc();
            0
        }
    }
}

unsafe extern "C" fn wake_timer(_cd: *mut c_void) {}

fn tk_mainloop(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("mainloop", args, 0, 1)?;
    let threshold = if args.is_empty() { 0 } else { crate::modules::posix::path::c_int(args[0])? };
    let a = app(o);
    a.quit.store(false, Ordering::Relaxed);
    a.dispatching.store(true, Ordering::Release);
    let ap = api();
    let mut res = Ok(());
    while num_main_windows(a) > threshold && !a.quit.load(Ordering::Relaxed) && ERROR_IN_CMD.load(Ordering::Acquire).is_null() {
        // a short timer bounds the wait so that signals are noticed promptly
        let ms = BUSY_WAIT_MS.load(Ordering::Relaxed).max(1) * 5;
        let tok = SendPtr(unsafe { (ap.Tcl_CreateTimerHandler)(ms, wake_timer, std::ptr::null_mut()) } as *const c_void);
        tcl_blocking(|| unsafe { (ap.Tcl_DoOneEvent)(0) });
        unsafe { (ap.Tcl_DeleteTimerHandler)(tok.0 as *mut c_void) };
        if let Err(e) = crate::modules::signal::check_signals() {
            res = Err(e);
            break;
        }
    }
    a.dispatching.store(false, Ordering::Release);
    a.quit.store(false, Ordering::Relaxed);
    res?;
    if let Some(e) = take_error_in_cmd() {
        set_exc(e);
        return Err(PyErr);
    }
    Ok(none::none_ref())
}

fn tk_dooneevent(_o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("dooneevent", args, 0, 1)?;
    let flags = if args.is_empty() { 0 } else { crate::modules::posix::path::c_int(args[0])? };
    let r = tcl_blocking(|| unsafe { (api().Tcl_DoOneEvent)(flags) });
    if let Some(e) = take_error_in_cmd() {
        set_exc(e);
        return Err(PyErr);
    }
    Ok(int::new_int(r as i64))
}

fn tk_quit(o: &PyObject) -> PyResult {
    app(o).quit.store(true, Ordering::Relaxed);
    Ok(none::none_ref())
}

fn tk_interpaddr(o: &PyObject) -> PyResult {
    Ok(int::new_int(app(o).interp as usize as i64))
}

fn tk_willdispatch(o: &PyObject) -> PyResult {
    app(o).dispatching.store(true, Ordering::Release);
    Ok(none::none_ref())
}

fn load_tk(a: &App) -> PyResult<()> {
    if a.tk_loaded.load(Ordering::Relaxed) {
        return Ok(());
    }
    let tk = tk_api()?;
    unsafe {
        let probe = c"info exists tk_version";
        if (api().Tcl_EvalEx)(a.interp, probe.as_ptr(), -1, 0) == TCL_OK && result_str(a).to_string_lossy_s() == "1" {
            a.tk_loaded.store(true, Ordering::Relaxed);
            return Ok(());
        }
        let interp = SendPtr(a.interp as *const c_void);
        if tcl_blocking(move || (tk.Tk_Init)(interp.0 as Interp)) != TCL_OK {
            return Err(app_error(a));
        }
    }
    a.tk_loaded.store(true, Ordering::Relaxed);
    Ok(())
}

trait LossyS {
    fn to_string_lossy_s(&self) -> String;
}
impl LossyS for PyRef {
    fn to_string_lossy_s(&self) -> String {
        pystr::to_string_lossy(self)
    }
}

fn tk_loadtk(o: &PyObject) -> PyResult {
    on_interp!(o, move |a: &App| {
        load_tk(a)?;
        Ok(none::none_ref())
    })
}

fn tk_wantobjects(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("wantobjects", args, 0, 1)?;
    let a = app(o);
    if args.is_empty() {
        return Ok(int::new_int(a.wantobjects.load(Ordering::Relaxed) as i64));
    }
    let v = crate::modules::posix::path::c_int(args[0])?;
    a.wantobjects.store(v, Ordering::Relaxed);
    Ok(none::none_ref())
}

fn tk_settrace(o: &PyObject, v: &PyObject) -> PyResult {
    app(o).trace = if none::is_none(v) { None } else { Some(v.to_ref()) };
    Ok(none::none_ref())
}

fn tk_gettrace(o: &PyObject) -> PyResult {
    Ok(app(o).trace.clone().unwrap_or_else(none::none_ref))
}

// ------------------------------------------------------------------ module functions

fn opt_bool(o: Option<&PyObject>, d: bool) -> PyResult<bool> {
    match o {
        Some(v) => ab::is_true(v),
        None => Ok(d),
    }
}

fn opt_str(fname: &str, o: Option<&PyObject>) -> PyResult<Option<CString>> {
    match o {
        Some(v) if !none::is_none(v) => Ok(Some(str_arg(fname, v)?)),
        _ => Ok(None),
    }
}

fn m_create(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("create", args, 0, 8)?;
    let g = |i: usize| args.get(i).copied();
    let screen = opt_str("create", g(0))?;
    let _base = opt_str("create", g(1))?;
    let class_name = match g(2) {
        Some(v) => {
            // Tcl 8 cannot title-case a first letter outside the BMP.
            if pystr::is_str(v) && pystr::str_len(v) > 0 {
                if let pystr::StrData::U4(d) = pystr::str_data(v) {
                    if d[0] > 0xFFFF {
                        raise!(ValueError, "className must not start with a character outside the BMP");
                    }
                }
            }
            str_arg("create", v)?
        }
        None => CString::new("Tk").unwrap(),
    };
    let interactive = opt_bool(g(3), false)?;
    let wantobjects = match g(4) {
        Some(v) => crate::modules::posix::path::c_int(v)?,
        None => 0,
    };
    let want_tk = opt_bool(g(5), true)?;
    let sync = opt_bool(g(6), false)?;
    let use_ = opt_str("create", g(7))?;
    let a = api();
    unsafe {
        let interp = (a.Tcl_CreateInterp)();
        if interp.is_null() {
            raise!(RuntimeError, "Tcl_CreateInterp failed");
        }
        let obj = fc::robj_new(
            tkapp_type(),
            App {
                interp,
                thread: (a.Tcl_GetCurrentThread)(),
                wantobjects: AtomicI32::new(wantobjects),
                dispatching: AtomicBool::new(false),
                quit: AtomicBool::new(false),
                tk_loaded: AtomicBool::new(false),
                trace: None,
            },
        )?;
        let ap = app(&obj);
        let set_global = |n: &std::ffi::CStr, v: &std::ffi::CStr| {
            (a.Tcl_SetVar2)(interp, n.as_ptr(), std::ptr::null(), v.as_ptr(), TCL_GLOBAL_ONLY);
        };
        set_global(c"tcl_interactive", if interactive { c"1" } else { c"0" });
        let argv0 = CString::new(class_name.to_bytes().to_ascii_lowercase()).unwrap();
        set_global(c"argv0", &argv0);
        if !want_tk {
            set_global(c"_tkinter_skip_tk_init", c"1");
        }
        let mut argv = String::new();
        if sync {
            argv.push_str("-sync");
        }
        if let Some(u) = &use_ {
            if !argv.is_empty() {
                argv.push(' ');
            }
            argv.push_str("-use ");
            // quote the window id as a Tcl list element
            let l = TRef::new((a.Tcl_NewListObj)(0, std::ptr::null()));
            let _ = l;
            argv.push_str(&u.to_string_lossy());
        }
        if !argv.is_empty() {
            let c = CString::new(argv).unwrap();
            set_global(c"argv", &c);
        }
        if let Some(s) = &screen {
            (a.Tcl_SetVar2)(interp, c"env".as_ptr(), c"DISPLAY".as_ptr(), s.as_ptr(), TCL_GLOBAL_ONLY);
        }
        let ip = SendPtr(interp as *const c_void);
        if tcl_blocking(move || (a.Tcl_Init)(ip.0 as Interp)) != TCL_OK {
            return Err(app_error(ap));
        }
        if want_tk {
            load_tk(ap)?;
        }
        Ok(obj)
    }
}

fn flatten_into(item: &PyObject, out: &mut Vec<PyRef>, depth: usize) -> PyResult<()> {
    if depth > 1000 {
        raise!(ValueError, "nesting too deep in _flatten");
    }
    let items: Vec<PyRef> = if tuple::is_tuple(item) { tuple::tuple_to_vec(item) } else { list::list_to_vec(item) };
    for it in items {
        if tuple::is_tuple(&it) || list::is_list(&it) {
            flatten_into(&it, out, depth + 1)?;
        } else if !none::is_none(&it) {
            out.push(it);
        }
    }
    Ok(())
}

fn m_flatten(_m: &PyObject, item: &PyObject) -> PyResult {
    if !tuple::is_tuple(item) && !list::is_list(item) {
        raise!(TypeError, "argument must be sequence");
    }
    let mut out = Vec::new();
    flatten_into(item, &mut out, 0)?;
    Ok(tuple::new_tuple(out))
}

fn m_setbusywaitinterval(_m: &PyObject, v: &PyObject) -> PyResult {
    let n = crate::modules::posix::path::c_int(v)?;
    if n < 0 {
        raise!(ValueError, "busywaitinterval must be >= 0");
    }
    BUSY_WAIT_MS.store(n, Ordering::Relaxed);
    Ok(none::none_ref())
}

fn m_getbusywaitinterval(_m: &PyObject) -> PyResult {
    Ok(int::new_int(BUSY_WAIT_MS.load(Ordering::Relaxed) as i64))
}

static INITIALISED: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static _UNUSED: Cell<u8> = const { Cell::new(0) };
}

pub fn init() -> PyResult {
    let a = tcl_api()?;
    if INITIALISED.swap(1, Ordering::AcqRel) == 0 {
        let exe = crate::import::import_absolute("sys")
            .and_then(|s| ab::getattr_str(&s, "executable"))
            .ok()
            .filter(|e| pystr::is_str(e))
            .map(|e| pystr::to_string_lossy(&e))
            .unwrap_or_default();
        clear_exc();
        let exe = CString::new(exe).unwrap_or_default();
        unsafe { (a.Tcl_FindExecutable)(exe.as_ptr()) };
        // Keep Tcl's idea of file names and system strings in line with
        // Python's: a C locale makes Tcl choose iso8859-1, while Python then
        // uses UTF-8 for the file system.
        let fsenc = crate::import::import_absolute("sys")
            .and_then(|s| ab::call_method_str(&s, "getfilesystemencoding", &[]))
            .ok()
            .map(|e| pystr::to_string_lossy(&e))
            .unwrap_or_default();
        clear_exc();
        if fsenc == "utf-8" {
            unsafe { (a.Tcl_SetSystemEncoding)(std::ptr::null_mut(), c"utf-8".as_ptr()) };
        }
    }
    unsafe {
        let t = tclobj_type();
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_robj_type::<TclObjW>(t, c"_tkinter.Tcl_Obj", Py_TPFLAGS_DISALLOW_INSTANTIATION);
            (*t).tp_str = Some(tclobj_str);
            (*t).tp_repr = Some(tclobj_repr);
            (*t).tp_richcompare = Some(tclobj_richcompare);
            (*t).tp_hash = Some(crate::types::slots::hash_not_implemented);
            (*t).tp_getset = crate::getset_table![
                ("string", tclobj_get_string; "the string representation of this object, either as str or bytes"),
                ("typename", tclobj_get_typename; "name of the Tcl type"),
            ];
            typeobject::type_ready(t)?;
        }
        let t = tktt_type();
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_robj_type::<Timer>(t, c"_tkinter.tktimertoken", Py_TPFLAGS_DISALLOW_INSTANTIATION);
            (*t).tp_repr = Some(tt_repr);
            (*t).tp_methods = mdefs![("deletetimerhandler", NOARGS, tt_delete, "deletetimerhandler($self, /)\n--\n\n")];
            typeobject::type_ready(t)?;
        }
        let t = tkapp_type();
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_robj_type::<App>(t, c"_tkinter.tkapp", Py_TPFLAGS_DISALLOW_INSTANTIATION);
            (*t).tp_methods = mdefs![
                ("willdispatch", NOARGS, tk_willdispatch, "willdispatch($self, /)\n--\n\n"),
                ("wantobjects", FAST, tk_wantobjects, ""),
                ("call", FAST, tk_call, ""),
                ("eval", O, tk_eval, "eval($self, script, /)\n--\n\n"),
                ("evalfile", O, tk_evalfile, "evalfile($self, fileName, /)\n--\n\n"),
                ("record", O, tk_record, "record($self, script, /)\n--\n\n"),
                ("adderrorinfo", O, tk_adderrorinfo, "adderrorinfo($self, msg, /)\n--\n\n"),
                ("setvar", FAST, tk_setvar, ""),
                ("globalsetvar", FAST, tk_globalsetvar, ""),
                ("getvar", FAST, tk_getvar, ""),
                ("globalgetvar", FAST, tk_globalgetvar, ""),
                ("unsetvar", FAST, tk_unsetvar, ""),
                ("globalunsetvar", FAST, tk_globalunsetvar, ""),
                ("getint", O, tk_getint, "getint($self, arg, /)\n--\n\n"),
                ("getdouble", O, tk_getdouble, "getdouble($self, arg, /)\n--\n\n"),
                ("getboolean", O, tk_getboolean, "getboolean($self, arg, /)\n--\n\n"),
                ("exprstring", O, tk_exprstring, "exprstring($self, s, /)\n--\n\n"),
                ("exprlong", O, tk_exprlong, "exprlong($self, s, /)\n--\n\n"),
                ("exprdouble", O, tk_exprdouble, "exprdouble($self, s, /)\n--\n\n"),
                ("exprboolean", O, tk_exprboolean, "exprboolean($self, s, /)\n--\n\n"),
                ("splitlist", O, tk_splitlist, "splitlist($self, arg, /)\n--\n\n"),
                ("createcommand", FAST, tk_createcommand, "createcommand($self, name, func, /)\n--\n\n"),
                ("deletecommand", O, tk_deletecommand, "deletecommand($self, name, /)\n--\n\n"),
                ("createfilehandler", FAST, tk_createfilehandler, "createfilehandler($self, file, mask, func, /)\n--\n\n"),
                ("deletefilehandler", O, tk_deletefilehandler, "deletefilehandler($self, file, /)\n--\n\n"),
                ("createtimerhandler", FAST, tk_createtimerhandler, "createtimerhandler($self, milliseconds, func, /)\n--\n\n"),
                ("mainloop", FAST, tk_mainloop, "mainloop($self, threshold=0, /)\n--\n\n"),
                ("dooneevent", FAST, tk_dooneevent, "dooneevent($self, flags=0, /)\n--\n\n"),
                ("quit", NOARGS, tk_quit, "quit($self, /)\n--\n\n"),
                ("interpaddr", NOARGS, tk_interpaddr, "interpaddr($self, /)\n--\n\n"),
                ("loadtk", NOARGS, tk_loadtk, "loadtk($self, /)\n--\n\n"),
                ("settrace", O, tk_settrace, "settrace($self, func, /)\n--\n\nSet the tracing function."),
                ("gettrace", NOARGS, tk_gettrace, "gettrace($self, /)\n--\n\nGet the tracing function."),
            ];
            typeobject::type_ready(t)?;
        }
    }
    let (m, d) = fc::new_module(
        "_tkinter",
        mdefs![
            ("create", FAST, m_create, "create($module, screenName=None, baseName='', className='Tk',\n       interactive=False, wantobjects=False, wantTk=True, sync=False,\n       use=None, /)\n--\n\n"),
            ("_flatten", O, m_flatten, "_flatten($module, item, /)\n--\n\n"),
            ("setbusywaitinterval", O, m_setbusywaitinterval, "setbusywaitinterval($module, new_val, /)\n--\n\nSet the busy-wait interval in milliseconds between successive calls to Tcl_DoOneEvent in a threaded Python interpreter."),
            ("getbusywaitinterval", NOARGS, m_getbusywaitinterval, "getbusywaitinterval($module, /)\n--\n\nReturn the current busy-wait interval between successive calls to Tcl_DoOneEvent in a threaded Python interpreter."),
        ],
        None,
    )?;
    if TCL_ERROR_TYPE.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("_tkinter", "TclError", exceptions::Exception())?;
        TCL_ERROR_TYPE.store(e.into_raw(), Ordering::Release);
    }
    dict::set_item_str(&d, "TclError", unsafe { &*TCL_ERROR_TYPE.load(Ordering::Acquire) })?;
    for (n, v) in [
        ("READABLE", TCL_READABLE),
        ("WRITABLE", TCL_WRITABLE),
        ("EXCEPTION", TCL_EXCEPTION),
        ("WINDOW_EVENTS", TCL_WINDOW_EVENTS),
        ("FILE_EVENTS", TCL_FILE_EVENTS),
        ("TIMER_EVENTS", TCL_TIMER_EVENTS),
        ("IDLE_EVENTS", TCL_IDLE_EVENTS),
        ("ALL_EVENTS", TCL_ALL_EVENTS),
        ("DONT_WAIT", TCL_DONT_WAIT),
    ] {
        dict::set_item_str(&d, n, &int::new_int(v as i64))?;
    }
    let (mut maj, mut min, mut pat, mut kind) = (0, 0, 0, 0);
    unsafe { (a.Tcl_GetVersion)(&mut maj, &mut min, &mut pat, &mut kind) };
    let ver = pystr::new_str(&format!("{}.{}", maj, min));
    dict::set_item_str(&d, "TCL_VERSION", &ver)?;
    dict::set_item_str(&d, "TK_VERSION", &ver)?;
    dict::set_item_str(&d, "TkappType", typeobject::type_obj(tkapp_type()))?;
    dict::set_item_str(&d, "Tcl_Obj", typeobject::type_obj(tclobj_type()))?;
    dict::set_item_str(&d, "TkttType", typeobject::type_obj(tktt_type()))?;
    Ok(m)
}
