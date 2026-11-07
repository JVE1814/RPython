//! C-variadic entry points that build Python values: Py_BuildValue & co,
//! PyObject_CallFunction/CallMethod(ObjArgs), PyTuple_Pack, PyOS_snprintf.

use super::*;
use crate::abstract_ as ab;
use crate::types::{bytes, complex, dict, float, int, list, none, str as pystr, tuple};
use std::ffi::{VaList, c_long, c_longlong, c_uint, c_ulong, c_ulonglong, c_void};

/// The Py_BuildValue engine.
pub(crate) struct Builder<'a, 'b> {
    fmt: &'a [u8],
    pos: usize,
    ap: &'a mut VaList<'b>,
    failed: bool,
}

impl<'a, 'b> Builder<'a, 'b> {
    fn peek(&self) -> u8 {
        *self.fmt.get(self.pos).unwrap_or(&0)
    }
    fn skip_sep(&mut self) {
        while matches!(self.peek(), b',' | b':' | b' ' | b'\t') {
            self.pos += 1;
        }
    }
    /// Count items until `end` at this nesting level.
    fn count(&self, end: u8) -> Result<usize, ()> {
        let mut level = 0;
        let mut n = 0;
        let mut i = self.pos;
        while i < self.fmt.len() {
            let c = self.fmt[i];
            if level == 0 && c == end {
                return Ok(n);
            }
            match c {
                b'(' | b'[' | b'{' => {
                    if level == 0 {
                        n += 1;
                    }
                    level += 1;
                }
                b')' | b']' | b'}' => {
                    if level == 0 {
                        return Err(());
                    }
                    level -= 1;
                }
                b'#' | b'&' | b',' | b':' | b' ' | b'\t' => {}
                _ => {
                    if level == 0 {
                        n += 1;
                    }
                }
            }
            i += 1;
        }
        if end == 0 { Ok(n) } else { Err(()) }
    }
    fn fail(&mut self) -> Option<PyRef> {
        self.failed = true;
        None
    }

    /// Build one value (None result after failure; keeps consuming args).
    unsafe fn one(&mut self) -> Option<PyRef> {
        unsafe {
            self.skip_sep();
            let c = self.peek();
            self.pos += 1;
            match c {
                b'(' => self.seq(b')', true),
                b'[' => self.seq(b']', false),
                b'{' => self.dict(),
                b'b' | b'B' | b'h' | b'i' => {
                    let v: c_int = self.ap.next_arg::<c_int>();
                    if self.failed { None } else { Some(int::new_int(v as i64)) }
                }
                b'H' => {
                    let v: c_int = self.ap.next_arg::<c_int>();
                    if self.failed { None } else { Some(int::new_int(v as u16 as i64)) }
                }
                b'I' => {
                    let v: c_uint = self.ap.next_arg::<c_uint>();
                    if self.failed { None } else { Some(int::new_int(v as i64)) }
                }
                b'n' => {
                    let v: isize = self.ap.next_arg::<isize>();
                    if self.failed { None } else { Some(int::new_int(v as i64)) }
                }
                b'l' => {
                    let v: c_long = self.ap.next_arg::<c_long>();
                    if self.failed { None } else { Some(int::new_int(v)) }
                }
                b'k' => {
                    let v: c_ulong = self.ap.next_arg::<c_ulong>();
                    if self.failed { None } else { Some(int::new_int_u128(v as u128)) }
                }
                b'L' => {
                    let v: c_longlong = self.ap.next_arg::<c_longlong>();
                    if self.failed { None } else { Some(int::new_int(v)) }
                }
                b'K' => {
                    let v: c_ulonglong = self.ap.next_arg::<c_ulonglong>();
                    if self.failed { None } else { Some(int::new_int_u128(v as u128)) }
                }
                b'p' => {
                    let v: c_int = self.ap.next_arg::<c_int>();
                    if self.failed { None } else { Some(crate::types::bool_::new_bool(v != 0)) }
                }
                b'd' | b'f' => {
                    let v: f64 = self.ap.next_arg::<f64>();
                    if self.failed { None } else { Some(float::new_float(v)) }
                }
                b'D' => {
                    let p: *const Py_complex = self.ap.next_arg::<*const Py_complex>();
                    if self.failed { None } else { Some(complex::new_complex((*p).real, (*p).imag)) }
                }
                b'c' => {
                    let v: c_int = self.ap.next_arg::<c_int>();
                    if self.failed { None } else { Some(bytes::new_bytes(&[v as u8])) }
                }
                b'C' => {
                    let v: c_int = self.ap.next_arg::<c_int>();
                    if self.failed {
                        return None;
                    }
                    if !(0..=0x10ffff).contains(&v) {
                        let _ = value_error("character U+... is not in range [U+0000; U+10ffff]");
                        return self.fail();
                    }
                    Some(pystr::from_codepoints(&[v as u32]))
                }
                b's' | b'z' | b'U' | b'y' => {
                    let p: *const c_char = self.ap.next_arg::<*const c_char>();
                    let mut n: isize = -1;
                    if self.peek() == b'#' {
                        self.pos += 1;
                        n = self.ap.next_arg::<isize>();
                    }
                    if self.failed {
                        return None;
                    }
                    if p.is_null() {
                        if c == b'y' {
                            let _ = value_error("NULL string with positive size with NULL passed to PyBytes_FromStringAndSize");
                            return self.fail();
                        }
                        return Some(none::none_ref());
                    }
                    let len = if n < 0 { libc::strlen(p) } else { n as usize };
                    let b = std::slice::from_raw_parts(p as *const u8, len);
                    if c == b'y' {
                        return Some(bytes::new_bytes(b));
                    }
                    match super::unicode::decode_utf8(b, None) {
                        Ok(s) => Some(s),
                        Err(_) => self.fail(),
                    }
                }
                b'u' => {
                    let p: *const libc::wchar_t = self.ap.next_arg::<*const libc::wchar_t>();
                    let mut n: isize = -1;
                    if self.peek() == b'#' {
                        self.pos += 1;
                        n = self.ap.next_arg::<isize>();
                    }
                    if self.failed {
                        return None;
                    }
                    if p.is_null() {
                        return Some(none::none_ref());
                    }
                    let r = super::unicode::PyUnicode_FromWideChar(p, n);
                    if r.is_null() { self.fail() } else { Some(PyRef::from_raw(r)) }
                }
                b'N' | b'S' | b'O' => {
                    if self.peek() == b'&' {
                        self.pos += 1;
                        let fp: *mut c_void = self.ap.next_arg::<*mut c_void>();
                        let arg: *mut c_void = self.ap.next_arg::<*mut c_void>();
                        if self.failed {
                            return None;
                        }
                        let f: unsafe extern "C" fn(*mut c_void) -> *mut PyObject = std::mem::transmute(fp);
                        let r = f(arg);
                        if r.is_null() {
                            return self.fail();
                        }
                        return Some(PyRef::from_raw(r));
                    }
                    let p: *mut PyObject = self.ap.next_arg::<*mut PyObject>();
                    if self.failed {
                        if c == b'N' {
                            xdecref(p);
                        }
                        return None;
                    }
                    if p.is_null() {
                        if !err_occurred() {
                            let _ = system_error("NULL object passed to Py_BuildValue");
                        }
                        return self.fail();
                    }
                    if c == b'N' { Some(PyRef::from_raw(p)) } else { Some(PyRef::from_borrowed(p)) }
                }
                _ => {
                    if !self.failed {
                        let _ = system_error("bad format char passed to Py_BuildValue");
                    }
                    self.fail()
                }
            }
        }
    }

    unsafe fn seq(&mut self, end: u8, is_tuple: bool) -> Option<PyRef> {
        unsafe {
            let mut items = Vec::new();
            loop {
                self.skip_sep();
                let c = self.peek();
                if c == end {
                    self.pos += 1;
                    break;
                }
                if c == 0 {
                    if !self.failed {
                        let _ = system_error("unmatched paren in format");
                    }
                    return self.fail();
                }
                match self.one() {
                    Some(v) => items.push(v),
                    None => {
                        self.failed = true;
                    }
                }
            }
            if self.failed {
                return None;
            }
            Some(if is_tuple { tuple::new_tuple(items) } else { list::new_list(items) })
        }
    }

    unsafe fn dict(&mut self) -> Option<PyRef> {
        unsafe {
            let d = dict::new_dict();
            loop {
                self.skip_sep();
                let c = self.peek();
                if c == b'}' {
                    self.pos += 1;
                    break;
                }
                if c == 0 {
                    if !self.failed {
                        let _ = system_error("unmatched paren in format");
                    }
                    return self.fail();
                }
                let k = self.one();
                let v = self.one();
                if let (Some(k), Some(v)) = (k, v) {
                    if !self.failed && dict::set_item(&d, &k, &v).is_err() {
                        self.failed = true;
                    }
                } else {
                    self.failed = true;
                }
            }
            if self.failed { None } else { Some(d) }
        }
    }
}

/// Py_VaBuildValue semantics: 0 items -> None, 1 item -> it, n items -> tuple.
pub(crate) unsafe fn build_value(fmt: *const c_char, ap: &mut VaList) -> *mut PyObject {
    unsafe {
        if fmt.is_null() {
            return none::none_ref().into_raw();
        }
        let f = CStr::from_ptr(fmt).to_bytes();
        let mut b = Builder { fmt: f, pos: 0, ap, failed: false };
        let n = match b.count(0) {
            Ok(n) => n,
            Err(_) => {
                let _ = system_error("unmatched paren in format");
                return std::ptr::null_mut();
            }
        };
        if n == 0 {
            return none::none_ref().into_raw();
        }
        if n == 1 {
            return ret_opt(b.one().filter(|_| !b.failed));
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            match b.one() {
                Some(v) => items.push(v),
                None => b.failed = true,
            }
        }
        if b.failed {
            return std::ptr::null_mut();
        }
        tuple::new_tuple(items).into_raw()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_BuildValue(fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe { build_value(fmt, &mut args) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_BuildValue_SizeT(fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe { build_value(fmt, &mut args) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_VaBuildValue(fmt: *const c_char, mut ap: VaList) -> *mut PyObject {
    unsafe { build_value(fmt, &mut ap) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_VaBuildValue_SizeT(fmt: *const c_char, mut ap: VaList) -> *mut PyObject {
    unsafe { build_value(fmt, &mut ap) }
}

unsafe fn call_built(callable: &PyObject, args: *mut PyObject) -> *mut PyObject {
    unsafe {
        if args.is_null() {
            return std::ptr::null_mut();
        }
        let a = PyRef::from_raw(args);
        if tuple::is_tuple(&a) {
            ret(ab::call(callable, tuple::tuple_items(&a)))
        } else {
            ret(ab::call(callable, &[&a]))
        }
    }
}
unsafe fn has_format(fmt: *const c_char) -> bool {
    unsafe { !fmt.is_null() && *fmt != 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFunction(callable: *mut PyObject, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        if callable.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if !has_format(fmt) {
            return ret(ab::call(&*callable, &[]));
        }
        call_built(&*callable, build_value(fmt, &mut args))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallFunction_SizeT(callable: *mut PyObject, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        if !has_format(fmt) {
            return ret(ab::call(&*callable, &[]));
        }
        call_built(&*callable, build_value(fmt, &mut args))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_CallFunction(callable: *mut PyObject, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        let a = build_value(fmt, &mut args);
        if a.is_null() {
            return a;
        }
        let a = PyRef::from_raw(a);
        let t = if tuple::is_tuple(&a) { a } else { tuple::new_tuple(vec![a]) };
        ret(ab::call(&*callable, tuple::tuple_items(&t)))
    }
}
unsafe fn method_of(o: *mut PyObject, name: *const c_char) -> PyResult {
    unsafe {
        if o.is_null() || name.is_null() {
            return Err(bad_internal());
        }
        let n = cstr_obj(name)?;
        ab::getattr(&*o, &n)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallMethod(o: *mut PyObject, name: *const c_char, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        let m = match method_of(o, name) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        if !has_format(fmt) {
            return ret(ab::call(&m, &[]));
        }
        call_built(&m, build_value(fmt, &mut args))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallMethod_SizeT(o: *mut PyObject, name: *const c_char, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        let m = match method_of(o, name) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        if !has_format(fmt) {
            return ret(ab::call(&m, &[]));
        }
        call_built(&m, build_value(fmt, &mut args))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_CallMethod(o: *mut PyObject, name: *const c_char, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        let m = match method_of(o, name) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        let a = build_value(fmt, &mut args);
        if a.is_null() {
            return a;
        }
        let a = PyRef::from_raw(a);
        let t = if tuple::is_tuple(&a) { a } else { tuple::new_tuple(vec![a]) };
        ret(ab::call(&m, tuple::tuple_items(&t)))
    }
}
unsafe fn collect_objargs(ap: &mut VaList) -> Vec<*mut PyObject> {
    unsafe {
        let mut v = Vec::new();
        loop {
            let p: *mut PyObject = ap.next_arg::<*mut PyObject>();
            if p.is_null() {
                break;
            }
            v.push(p);
        }
        v
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFunctionObjArgs(callable: *mut PyObject, mut args: ...) -> *mut PyObject {
    unsafe {
        let v = collect_objargs(&mut args);
        if callable.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ab::vectorcall_raw(&*callable, v.as_ptr(), v.len(), std::ptr::null_mut())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallMethodObjArgs(o: *mut PyObject, name: *mut PyObject, mut args: ...) -> *mut PyObject {
    unsafe {
        let v = collect_objargs(&mut args);
        if o.is_null() || name.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let m = match ab::getattr(&*o, &*name) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        ab::vectorcall_raw(&m, v.as_ptr(), v.len(), std::ptr::null_mut())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_Pack(n: Py_ssize_t, mut args: ...) -> *mut PyObject {
    unsafe {
        // (hang1) PyTuple_New validates the size (SystemError / MemoryError)
        // before any argument is read
        let t = super::containers::PyTuple_New(n);
        if t.is_null() || n == 0 {
            return t;
        }
        let base = tuple::item_ptr(t);
        for i in 0..n as usize {
            let p: *mut PyObject = args.next_arg::<*mut PyObject>();
            incref(p);
            *base.add(i) = p;
        }
        tuple::track_if_needed(t);
        t
    }
}

// ---------------------------------------------------------------- C printf

unsafe extern "C" {
    fn vsnprintf(s: *mut c_char, n: usize, fmt: *const c_char, ap: VaList) -> c_int;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_vsnprintf(s: *mut c_char, size: usize, fmt: *const c_char, ap: VaList) -> c_int {
    unsafe {
        if size == 0 {
            return 0;
        }
        let r = vsnprintf(s, size, fmt, ap);
        *s.add(size - 1) = 0;
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_snprintf(s: *mut c_char, size: usize, fmt: *const c_char, args: ...) -> c_int {
    unsafe { PyOS_vsnprintf(s, size, fmt, args) }
}

// ---------------------------------------------------------------- sys writes

unsafe fn sys_write(name: &str, s: &PyObject) {
    let saved = take_exc();
    let r = (|| -> PyResult<()> {
        let f = crate::modules::sys::get_sys_attr(name).ok_or(PyErr)?;
        if none::is_none(&f) {
            return Ok(());
        }
        ab::call_method_str(&f, "write", &[s])?;
        Ok(())
    })();
    if r.is_err() {
        clear_exc();
        let t = pystr::to_string_lossy(s);
        crate::modules::sys::write_fd(if name == "stdout" { 1 } else { 2 }, t.as_bytes());
    }
    if let Some(e) = saved {
        set_exc(e);
    }
}
unsafe fn c_vformat(fmt: *const c_char, ap: VaList) -> String {
    unsafe {
        let mut buf = vec![0u8; 1001];
        let full = vsnprintf(buf.as_mut_ptr() as *mut c_char, 1001, fmt, ap);
        let n = buf.iter().position(|&b| b == 0).unwrap_or(1000);
        let mut s = String::from_utf8_lossy(&buf[..n]).into_owned();
        // (capi2) longer output is cut at 1000 bytes and marked
        if full > 1000 {
            s.push_str("... truncated");
        }
        s
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_WriteStdout(fmt: *const c_char, args: ...) {
    unsafe {
        let s = c_vformat(fmt, args);
        sys_write("stdout", &pystr::new_str(&s));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_WriteStderr(fmt: *const c_char, args: ...) {
    unsafe {
        let s = c_vformat(fmt, args);
        sys_write("stderr", &pystr::new_str(&s));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_FormatStdout(fmt: *const c_char, mut args: ...) {
    unsafe {
        if let Ok(s) = super::printf::format_v(fmt, &mut args) {
            sys_write("stdout", &s);
        } else {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_FormatStderr(fmt: *const c_char, mut args: ...) {
    unsafe {
        if let Ok(s) = super::printf::format_v(fmt, &mut args) {
            sys_write("stderr", &s);
        } else {
            clear_exc();
        }
    }
}
