//! Deprecated / stable-ABI-only entry points that are still exported by CPython
//! (used by older extension code and by the test helpers): _Py_Identifier helpers,
//! the private _PyUnicodeWriter, PyCFunction_Call, PyWeakref_GetObject, ...

use super::*;
use crate::abstract_ as ab;
use crate::types::{none, str as pystr};
use std::ffi::c_void;
use std::sync::Mutex;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_GetBuiltWithAssert() -> c_int {
    cfg!(debug_assertions) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_Call(f: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { super::abstract_::PyObject_Call(f, args, kw) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModuleNoBlock(name: *const c_char) -> *mut PyObject {
    unsafe { super::import::PyImport_ImportModule(name) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_GetObject(r: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mut out = std::ptr::null_mut();
        match super::funcs::PyWeakref_GetRef(r, &mut out) {
            1 => {
                // borrowed: the referent is kept alive elsewhere (legacy semantics)
                decref(out);
                out
            }
            0 => none::none().as_ptr(),
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_CallObjectWithKeywords(f: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if args.is_null() {
            let t = crate::types::tuple::empty_tuple();
            return super::abstract_::PyObject_Call(f, t.as_ptr(), kw);
        }
        super::abstract_::PyObject_Call(f, args, kw)
    }
}

// ---------------------------------------------------------------- _Py_Identifier

#[repr(C)]
pub struct PyIdentifier {
    pub string: *const c_char,
    pub index: Py_ssize_t,
    pub mutex: u8,
}
static IDENTIFIERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicode_FromId(id: *mut PyIdentifier) -> *mut PyObject {
    unsafe {
        let mut g = IDENTIFIERS.lock().unwrap();
        let idx = (*id).index;
        if idx >= 0 && (idx as usize) < g.len() {
            return g[idx as usize] as *mut PyObject;
        }
        let s = match cstr_obj((*id).string) {
            Ok(s) => pystr::intern(s),
            Err(_) => return std::ptr::null_mut(),
        };
        make_immortal(s.as_ptr());
        g.push(s.as_ptr() as usize);
        (*id).index = (g.len() - 1) as Py_ssize_t;
        s.into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GetAttrId(o: *mut PyObject, id: *mut PyIdentifier) -> *mut PyObject {
    unsafe {
        let n = _PyUnicode_FromId(id);
        if n.is_null() {
            return n;
        }
        super::object::PyObject_GetAttr(o, n)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_CallMethodId(o: *mut PyObject, id: *mut PyIdentifier, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        let n = _PyUnicode_FromId(id);
        if n.is_null() {
            return n;
        }
        let m = match ab::getattr(&*o, &*n) {
            Ok(m) => m,
            Err(_) => return std::ptr::null_mut(),
        };
        if fmt.is_null() || *fmt == 0 {
            return ret(ab::call(&m, &[]));
        }
        let a = super::varargs::build_value(fmt, &mut args);
        if a.is_null() {
            return a;
        }
        let a = PyRef::from_raw(a);
        if crate::types::tuple::is_tuple(&a) {
            ret(ab::call(&m, crate::types::tuple::tuple_items(&a)))
        } else {
            ret(ab::call(&m, &[&a]))
        }
    }
}

// ---------------------------------------------------------------- _PyUnicodeWriter

#[repr(C)]
pub struct PrivWriter {
    pub buffer: *mut PyObject,
    pub data: *mut c_void,
    pub kind: c_int,
    pub maxchar: u32,
    pub size: Py_ssize_t,
    pub pos: Py_ssize_t,
    pub min_length: Py_ssize_t,
    pub min_char: u32,
    pub overallocate: u8,
    pub readonly: u8,
}

fn kind_for(maxchar: u32) -> (c_int, u32) {
    if maxchar < 0x80 {
        (1, 0x7f)
    } else if maxchar < 0x100 {
        (1, 0xff)
    } else if maxchar < 0x10000 {
        (2, 0xffff)
    } else {
        (4, 0x10ffff)
    }
}
unsafe fn read_char(data: *const c_void, kind: c_int, i: usize) -> u32 {
    unsafe {
        match kind {
            1 => *(data as *const u8).add(i) as u32,
            2 => *(data as *const u16).add(i) as u32,
            _ => *(data as *const u32).add(i),
        }
    }
}
unsafe fn write_char(data: *mut c_void, kind: c_int, i: usize, c: u32) {
    unsafe {
        match kind {
            1 => *(data as *mut u8).add(i) = c as u8,
            2 => *(data as *mut u16).add(i) = c as u16,
            _ => *(data as *mut u32).add(i) = c,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Init(w: *mut PrivWriter) {
    unsafe {
        std::ptr::write_bytes(w, 0, 1);
        (*w).min_char = 127;
        (*w).kind = 1;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_PrepareInternal(w: *mut PrivWriter, length: Py_ssize_t, maxchar: u32) -> c_int {
    unsafe {
        let wr = &mut *w;
        let need = wr.pos + length;
        let maxc = maxchar.max(wr.min_char).max(if wr.buffer.is_null() { 0 } else { wr.maxchar });
        let (kind, kmax) = kind_for(maxc);
        if !wr.buffer.is_null() && wr.readonly == 0 && need <= wr.size && kmax <= wr.maxchar {
            return 0;
        }
        let mut cap = need.max(wr.min_length);
        if wr.overallocate != 0 && cap > wr.size {
            cap += cap / 4;
        }
        let (kind, kmax) = if !wr.buffer.is_null() && kind < wr.kind { (wr.kind, wr.maxchar) } else { (kind, kmax) };
        let nb = super::unicode::PyUnicode_New(cap, kmax);
        if nb.is_null() {
            return -1;
        }
        let ndata = pystr::str_data_ptr(&*nb) as *mut c_void;
        if !wr.buffer.is_null() {
            for i in 0..wr.pos as usize {
                write_char(ndata, kind, i, read_char(wr.data, wr.kind, i));
            }
            decref(wr.buffer);
        }
        wr.buffer = nb;
        wr.data = ndata;
        wr.kind = kind;
        wr.maxchar = kmax;
        wr.size = cap;
        wr.readonly = 0;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_PrepareKindInternal(w: *mut PrivWriter, kind: c_int) -> c_int {
    unsafe {
        let maxc = match kind {
            1 => 0xff,
            2 => 0xffff,
            _ => 0x10ffff,
        };
        _PyUnicodeWriter_PrepareInternal(w, 0.max((*w).size - (*w).pos).max(1), maxc)
    }
}
unsafe fn prepare(w: *mut PrivWriter, len: Py_ssize_t, maxc: u32) -> c_int {
    unsafe {
        let wr = &*w;
        if !wr.buffer.is_null() && wr.readonly == 0 && maxc <= wr.maxchar && len <= wr.size - wr.pos {
            return 0;
        }
        if len == 0 {
            return 0;
        }
        _PyUnicodeWriter_PrepareInternal(w, len, maxc)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteChar(w: *mut PrivWriter, c: u32) -> c_int {
    unsafe {
        if prepare(w, 1, c) < 0 {
            return -1;
        }
        let wr = &mut *w;
        write_char(wr.data, wr.kind, wr.pos as usize, c);
        wr.pos += 1;
        0
    }
}
unsafe fn write_chars(w: *mut PrivWriter, it: &[u32]) -> c_int {
    unsafe {
        let maxc = it.iter().copied().max().unwrap_or(0);
        if prepare(w, it.len() as Py_ssize_t, maxc) < 0 {
            return -1;
        }
        let wr = &mut *w;
        for (i, &c) in it.iter().enumerate() {
            write_char(wr.data, wr.kind, wr.pos as usize + i, c);
        }
        wr.pos += it.len() as Py_ssize_t;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteStr(w: *mut PrivWriter, s: *mut PyObject) -> c_int {
    unsafe {
        let v = pystr::str_data(&*s).to_vec();
        write_chars(w, &v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteSubstring(w: *mut PrivWriter, s: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t) -> c_int {
    unsafe {
        let d = pystr::str_data(&*s);
        let v: Vec<u32> = (start.max(0) as usize..(end.max(0) as usize).min(d.len())).map(|i| d.get(i)).collect();
        write_chars(w, &v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteASCIIString(w: *mut PrivWriter, s: *const c_char, len: Py_ssize_t) -> c_int {
    unsafe {
        let n = if len < 0 { libc::strlen(s) } else { len as usize };
        let v: Vec<u32> = std::slice::from_raw_parts(s as *const u8, n).iter().map(|&b| b as u32).collect();
        write_chars(w, &v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_WriteLatin1String(w: *mut PrivWriter, s: *const c_char, len: Py_ssize_t) -> c_int {
    unsafe { _PyUnicodeWriter_WriteASCIIString(w, s, len) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Finish(w: *mut PrivWriter) -> *mut PyObject {
    unsafe {
        let wr = &mut *w;
        let v: Vec<u32> = (0..wr.pos.max(0) as usize).map(|i| read_char(wr.data, wr.kind, i)).collect();
        let r = pystr::from_codepoints(&v);
        _PyUnicodeWriter_Dealloc(w);
        r.into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyUnicodeWriter_Dealloc(w: *mut PrivWriter) {
    unsafe {
        let wr = &mut *w;
        xdecref(wr.buffer);
        wr.buffer = std::ptr::null_mut();
        wr.data = std::ptr::null_mut();
        wr.size = 0;
        wr.pos = 0;
    }
}

// ---------------------------------------------------------------- legacy init configuration

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SetProgramName(_name: *const libc::wchar_t) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SetPythonHome(_home: *const libc::wchar_t) {}
