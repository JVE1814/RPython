//! _io.StringIO: in-memory text stream (code point buffer).
use super::*;
use crate::types::list;
use std::ffi::{c_int, c_void};

pub const SEEN_CR: u8 = 1;
pub const SEEN_LF: u8 = 2;
pub const SEEN_CRLF: u8 = 4;

pub struct State {
    pub buf: Vec<u32>,
    pub pos: usize,
    /// The newline argument as given (None -> universal + translate).
    pub newline: Option<Vec<u32>>,
    pub readuniversal: bool,
    pub readtranslate: bool,
    pub writenl: Option<Vec<u32>>,
    pub seennl: u8,
    pub has_decoder: bool,
}

#[repr(C)]
pub struct StringIO {
    pub hdr: IoHeader,
    pub st: *mut State,
    pub closed: bool,
}

pub static mut STRINGIO_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn stringio_type() -> *mut PyTypeObject {
    &raw mut STRINGIO_TYPE
}

#[inline]
fn sio(o: &PyObject) -> &mut StringIO {
    unsafe { &mut *(o.as_ptr() as *mut StringIO) }
}

fn state(o: &PyObject) -> PyResult<&mut State> {
    let s = sio(o);
    if s.st.is_null() {
        raise!(ValueError, "I/O operation on uninitialized object");
    }
    if s.closed {
        raise!(ValueError, "I/O operation on closed file");
    }
    Ok(unsafe { &mut *s.st })
}

pub fn newlines_obj(seen: u8) -> PyRef {
    let s = |x: &str| pystr::new_str(x);
    match seen {
        0 => none::none_ref(),
        SEEN_CR => s("\r"),
        SEEN_LF => s("\n"),
        SEEN_CRLF => s("\r\n"),
        3 => tuple::new_tuple(vec![s("\r"), s("\n")]),
        5 => tuple::new_tuple(vec![s("\r"), s("\r\n")]),
        6 => tuple::new_tuple(vec![s("\n"), s("\r\n")]),
        _ => tuple::new_tuple(vec![s("\r"), s("\n"), s("\r\n")]),
    }
}

/// Universal newline translation of a complete chunk; records the kinds seen.
pub fn translate_newlines(input: &[u32], translate: bool, seen: &mut u8) -> Vec<u32> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        if c == '\r' as u32 {
            if i + 1 < input.len() && input[i + 1] == '\n' as u32 {
                *seen |= SEEN_CRLF;
                if translate {
                    out.push('\n' as u32);
                } else {
                    out.push(c);
                    out.push('\n' as u32);
                }
                i += 2;
                continue;
            }
            *seen |= SEEN_CR;
            out.push(if translate { '\n' as u32 } else { c });
        } else {
            if c == '\n' as u32 {
                *seen |= SEEN_LF;
            }
            out.push(c);
        }
        i += 1;
    }
    out
}

fn str_cps(s: &PyObject) -> Vec<u32> {
    pystr::str_data(s).iter().collect()
}

fn parse_newline(nl: Option<&PyObject>) -> PyResult<Option<Vec<u32>>> {
    match nl {
        None => Ok(Some(vec!['\n' as u32])),
        Some(x) if x.is(none::none()) => Ok(None),
        Some(x) => {
            if !pystr::is_str(x) {
                raise!(TypeError, "newline must be str or None, not {}", tname(x));
            }
            let s = pystr::to_string_lossy(x);
            if !matches!(s.as_str(), "" | "\n" | "\r" | "\r\n") {
                raise!(ValueError, "illegal newline value: {}", ab::repr_string(x)?);
            }
            Ok(Some(s.chars().map(|c| c as u32).collect()))
        }
    }
}

fn configure(newline: Option<Vec<u32>>) -> State {
    let readuniversal = newline.as_ref().map_or(true, |n| n.is_empty());
    let readtranslate = newline.is_none();
    let writenl = match &newline {
        Some(n) if !n.is_empty() && n[0] == '\r' as u32 => Some(n.clone()),
        _ => None,
    };
    State { buf: Vec::new(), pos: 0, newline, readuniversal, readtranslate, writenl, seennl: 0, has_decoder: readuniversal }
}

/// Write code points at the current position, applying newline translation.
fn write_cps(s: &mut State, input: &[u32]) -> usize {
    let n = input.len();
    let mut data: Vec<u32>;
    let mut slice: &[u32] = input;
    if s.has_decoder {
        data = translate_newlines(input, s.readtranslate, &mut s.seennl);
        slice = &data;
    }
    let translated: Vec<u32>;
    if let Some(nl) = &s.writenl {
        if slice.contains(&('\n' as u32)) {
            let mut v = Vec::with_capacity(slice.len() + 8);
            for &c in slice {
                if c == '\n' as u32 {
                    v.extend_from_slice(nl);
                } else {
                    v.push(c);
                }
            }
            translated = v;
            slice = &translated;
        }
    }
    let pos = s.pos;
    if pos > s.buf.len() {
        s.buf.resize(pos, 0);
    }
    let end = pos + slice.len();
    if end > s.buf.len() {
        s.buf.resize(end, 0);
    }
    s.buf[pos..end].copy_from_slice(slice);
    s.pos = end;
    data = Vec::new();
    drop(data);
    n
}

unsafe extern "C" fn stringio_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe { (*tp).tp_alloc.unwrap()(tp, 0) }
}

fn init_impl(o: &PyObject, value: Option<&PyObject>, newline: Option<&PyObject>) -> PyResult<()> {
    let nl = parse_newline(newline)?;
    if let Some(v) = value {
        if !v.is(none::none()) && !pystr::is_str(v) {
            raise!(TypeError, "initial_value must be str or None, not {}", tname(v));
        }
    }
    let mut st = configure(nl);
    if let Some(v) = value.filter(|v| !v.is(none::none())) {
        let cps = str_cps(v);
        write_cps(&mut st, &cps);
        st.pos = 0;
    }
    let s = sio(o);
    if !s.st.is_null() {
        unsafe { drop(Box::from_raw(s.st)) };
    }
    s.st = Box::into_raw(Box::new(st));
    s.closed = false;
    Ok(())
}

unsafe extern "C" fn stringio_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [value, newline] = kwparse::<2>("StringIO", &["initial_value", "newline"], 0, 0, &refs, kws.as_deref())?;
            init_impl(o, value, newline)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn stringio_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let s = &mut *(op as *mut StringIO);
        if !s.st.is_null() {
            drop(Box::from_raw(s.st));
            s.st = std::ptr::null_mut();
        }
        dealloc_end(op);
    }
}
unsafe extern "C" fn stringio_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe { visit_header(op, visit, arg) }
}
unsafe extern "C" fn stringio_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        0
    }
}

fn s_getvalue(o: &PyObject) -> PyResult {
    let s = state(o)?;
    Ok(pystr::from_codepoints(&s.buf))
}

fn s_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    let s = state(o)?;
    let len = s.buf.len();
    let start = s.pos.min(len);
    let avail = len - start;
    let n = if n < 0 { avail } else { (n as usize).min(avail) };
    s.pos = start + n;
    Ok(pystr::from_codepoints(&s.buf[start..start + n]))
}

/// End index (exclusive) of the line starting at `start`.
pub fn find_line_end(d: &[u32], start: usize, limit: isize, universal: bool, translate: bool, nl: &[u32]) -> usize {
    let end = if limit < 0 { d.len() } else { (start + limit as usize).min(d.len()) };
    let seg = &d[start..end];
    let cr = '\r' as u32;
    let lf = '\n' as u32;
    if translate || (!universal && nl == [lf]) {
        return match seg.iter().position(|&c| c == lf) {
            Some(i) => start + i + 1,
            None => end,
        };
    }
    if universal {
        let mut i = 0;
        while i < seg.len() {
            let c = seg[i];
            if c == lf {
                return start + i + 1;
            }
            if c == cr {
                if i + 1 < seg.len() && seg[i + 1] == lf {
                    return start + i + 2;
                }
                // a '\r' at the limit boundary followed by '\n' outside: stop at '\r'
                return start + i + 1;
            }
            i += 1;
        }
        return end;
    }
    // explicit multi/single char terminator
    let k = nl.len();
    if seg.len() >= k {
        for i in 0..=seg.len() - k {
            if seg[i..i + k] == *nl {
                return start + i + k;
            }
        }
    }
    end
}

fn readline_impl(o: &PyObject, limit: isize) -> PyResult<PyRef> {
    let s = state(o)?;
    let len = s.buf.len();
    let start = s.pos.min(len);
    let nl = s.newline.clone().unwrap_or_default();
    let end = find_line_end(&s.buf, start, limit, s.readuniversal, s.readtranslate, &nl);
    s.pos = end;
    Ok(pystr::from_codepoints(&s.buf[start..end]))
}

fn s_readline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readline", args, 0, 1)?;
    let limit = size_arg(args.first().copied())?;
    readline_impl(o, limit)
}

fn s_readlines(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readlines", args, 0, 1)?;
    let hint = size_arg(args.first().copied())?;
    state(o)?;
    let out = list::new_list_empty();
    let mut total = 0usize;
    loop {
        let line = readline_impl(o, -1)?;
        let n = pystr::str_len(&line);
        if n == 0 {
            break;
        }
        list::append(&out, line);
        total += n;
        if hint > 0 && total >= hint as usize {
            break;
        }
    }
    Ok(out)
}

fn s_write(o: &PyObject, v: &PyObject) -> PyResult {
    if !pystr::is_str(v) {
        raise!(TypeError, "string argument expected, got '{}'", tname(v));
    }
    let s = state(o)?;
    let cps = str_cps(v);
    let n = if cps.is_empty() { 0 } else { write_cps(s, &cps) };
    Ok(int::new_int(n as i64))
}

fn s_writelines(o: &PyObject, lines: &PyObject) -> PyResult {
    state(o)?;
    let it = ab::get_iter(lines)?;
    while let Some(line) = ab::iter_next(&it)? {
        call1(o, "write", &line)?;
    }
    Ok(none::none_ref())
}

fn s_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    let pos = ssize_arg(args[0])?;
    let whence = if args.len() > 1 { crate::modules::posix::path::c_int(args[1])? } else { 0 };
    let s = state(o)?;
    match whence {
        0 => {
            if pos < 0 {
                raise!(ValueError, "Negative seek position {}", pos);
            }
            s.pos = pos as usize;
        }
        1 => {
            if pos != 0 {
                raise!(OSError, "Can't do nonzero cur-relative seeks");
            }
        }
        2 => {
            if pos != 0 {
                raise!(OSError, "Can't do nonzero end-relative seeks");
            }
            s.pos = s.buf.len();
        }
        _ => raise!(ValueError, "Invalid whence ({}, should be 0, 1 or 2)", whence),
    }
    Ok(int::new_int(s.pos as i64))
}

fn s_tell(o: &PyObject) -> PyResult {
    Ok(int::new_int(state(o)?.pos as i64))
}

fn s_truncate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("truncate", args, 0, 1)?;
    let size = match args.first() {
        None => None,
        Some(x) if x.is(none::none()) => None,
        Some(x) => Some(ssize_arg(x)?),
    };
    let s = state(o)?;
    let size = size.unwrap_or(s.pos as isize);
    if size < 0 {
        raise!(ValueError, "Negative size value {}", size);
    }
    if (size as usize) < s.buf.len() {
        s.buf.truncate(size as usize);
    }
    Ok(int::new_int(size as i64))
}

fn s_close(o: &PyObject) -> PyResult {
    let s = sio(o);
    s.closed = true;
    if !s.st.is_null() {
        let st = unsafe { &mut *s.st };
        st.buf = Vec::new();
    }
    hdr(o).ioflags |= F_CLOSED;
    Ok(none::none_ref())
}

fn s_closed(o: &PyObject) -> PyResult {
    let s = sio(o);
    if s.st.is_null() {
        raise!(ValueError, "I/O operation on uninitialized object");
    }
    Ok(bool_::new_bool(s.closed))
}
fn s_true(o: &PyObject) -> PyResult {
    state(o)?;
    Ok(bool_::new_bool(true))
}
fn s_line_buffering(o: &PyObject) -> PyResult {
    state(o)?;
    Ok(bool_::new_bool(false))
}
fn s_newlines(o: &PyObject) -> PyResult {
    let s = state(o)?;
    if !s.has_decoder {
        return Ok(none::none_ref());
    }
    Ok(newlines_obj(s.seennl))
}

fn s_getstate(o: &PyObject) -> PyResult {
    let v = s_getvalue(o)?;
    let s = state(o)?;
    let nl = match &s.newline {
        None => none::none_ref(),
        Some(n) => pystr::from_codepoints(n),
    };
    let d = match crate::types::typeobject::instance_dict_existing(o) {
        Some(d) => dict::copy(&d)?,
        None => none::none_ref(),
    };
    Ok(tuple::new_tuple(vec![v, nl, int::new_int(s.pos as i64), d]))
}

fn s_setstate(o: &PyObject, st: &PyObject) -> PyResult {
    if !tuple::is_tuple(st) || tuple::tuple_len(st) < 4 {
        raise!(TypeError, "{}.__setstate__ argument should be 4-tuple, got {}", crate::types::typeobject::type_name(o.ty()), tname(st));
    }
    // like __init__, this may run on an object created by __new__ alone
    // (unpickling), so an uninitialized object is fine here
    if sio(o).closed {
        raise!(ValueError, "I/O operation on closed file");
    }
    let items = tuple::tuple_items(st);
    let value = items[0];
    if !pystr::is_str(value) {
        raise!(TypeError, "initial_value must be str, not {}", tname(value));
    }
    let nl = parse_newline(Some(items[1]))?;
    let posobj = items[2];
    if !int::is_int(posobj) {
        raise!(TypeError, "third item of state must be an integer, got {}", tname(posobj));
    }
    let pos = ab::index_as_ssize(posobj, Some(crate::types::exceptions::OverflowError()))?;
    if pos < 0 {
        raise!(ValueError, "position value cannot be negative");
    }
    let d = items[3];
    if !d.is(none::none()) && !dict::is_dict(d) {
        raise!(TypeError, "fourth item of state should be a dict, got a {}", tname(d));
    }
    let mut new = configure(nl);
    new.buf = str_cps(value);
    new.pos = pos as usize;
    let s = sio(o);
    if !s.st.is_null() {
        unsafe { drop(Box::from_raw(s.st)) };
    }
    s.st = Box::into_raw(Box::new(new));
    if dict::is_dict(d) {
        if let Some(own) = crate::types::typeobject::instance_dict(o) {
            dict::merge_dict(&own, d, true)?;
        }
    }
    Ok(none::none_ref())
}

fn s_detach(_o: &PyObject) -> PyResult {
    Err(unsupported("detach"))
}

unsafe extern "C" fn s_iternext(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = if o.is_type(stringio_type()) {
            readline_impl(o, -1)
        } else {
            match call0(o, "readline") {
                Ok(l) => {
                    if !pystr::is_str(&l) {
                        let _ = err!(OSError, "readline() should have returned a str object, not '{}'", tname(&l));
                        return std::ptr::null_mut();
                    }
                    Ok(l)
                }
                Err(e) => Err(e),
            }
        };
        match r {
            Ok(l) if pystr::str_len(&l) > 0 => l.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn s_iter(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        into_c(state(o).map(|_| o.to_ref()))
    }
}

unsafe extern "C" fn stringio_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let name = crate::types::typeobject::type_fqname(o.type_ptr());
        let name = if name == "_io.StringIO" { "_io.StringIO".to_string() } else { name };
        into_c(Ok(pystr::new_str(&format!("<{} object at {:#x}>", name, op as usize))))
    }
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if STRINGIO_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup_type(
            stringio_type(),
            c"_io.StringIO",
            std::mem::size_of::<StringIO>(),
            super::iobase::textiobase_type(),
            "StringIO(initial_value='', newline='\\n')\n--\n\nText I/O implementation using an in-memory buffer.\n\nThe initial_value argument sets the value of object.  The newline\nargument is like the one of TextIOWrapper's constructor.",
        );
        let t = &mut *stringio_type();
        t.tp_new = Some(stringio_new);
        t.tp_init = Some(stringio_init);
        t.tp_dealloc = Some(stringio_dealloc);
        t.tp_traverse = Some(stringio_traverse);
        t.tp_clear = Some(stringio_clear);
        t.tp_iter = Some(s_iter);
        t.tp_iternext = Some(s_iternext);
        let _ = stringio_repr;
        t.tp_methods = crate::mdefs![
            ("getvalue", NOARGS, s_getvalue),
            ("read", FAST, s_read),
            ("readline", FAST, s_readline),
            ("readlines", FAST, s_readlines),
            ("write", O, s_write),
            ("writelines", O, s_writelines),
            ("seek", FAST, s_seek),
            ("tell", NOARGS, s_tell),
            ("truncate", FAST, s_truncate),
            ("close", NOARGS, s_close),
            ("readable", NOARGS, s_true),
            ("writable", NOARGS, s_true),
            ("seekable", NOARGS, s_true),
            ("detach", NOARGS, s_detach),
            ("__getstate__", NOARGS, s_getstate),
            ("__setstate__", O, s_setstate),
        ];
        t.tp_getset = crate::getset_table![("closed", s_closed), ("line_buffering", s_line_buffering), ("newlines", s_newlines)];
        crate::types::typeobject::type_ready(stringio_type())?;
    }
    Ok(())
}
