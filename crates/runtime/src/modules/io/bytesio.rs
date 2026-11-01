//! _io.BytesIO (with copy-on-write sharing of an initial bytes object and getbuffer()).
use super::*;
use crate::modules::bufview::BufView;
use crate::types::{bytes, list};
use std::ffi::{c_int, c_void};

pub enum Data {
    /// Shares the contents of an immutable bytes object until the first mutation.
    Shared(PyRef),
    Owned(Vec<u8>),
}

pub struct State {
    pub data: Data,
    pub pos: usize,
}

impl State {
    fn bytes(&self) -> &[u8] {
        match &self.data {
            Data::Shared(b) => bytes::bytes_data(b),
            Data::Owned(v) => v,
        }
    }
    fn len(&self) -> usize {
        self.bytes().len()
    }
    fn owned(&mut self) -> &mut Vec<u8> {
        if let Data::Shared(b) = &self.data {
            let v = bytes::bytes_data(b).to_vec();
            self.data = Data::Owned(v);
        }
        match &mut self.data {
            Data::Owned(v) => v,
            _ => unreachable!(),
        }
    }
}

#[repr(C)]
pub struct BytesIO {
    pub hdr: IoHeader,
    pub st: *mut State,
    pub exports: isize,
}

pub static mut BYTESIO_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut BYTESIOBUF_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn bytesio_type() -> *mut PyTypeObject {
    &raw mut BYTESIO_TYPE
}

#[inline]
fn bio(o: &PyObject) -> &mut BytesIO {
    unsafe { &mut *(o.as_ptr() as *mut BytesIO) }
}

/// State of an open BytesIO (ValueError if closed).
fn state(o: &PyObject) -> PyResult<&mut State> {
    let b = bio(o);
    if b.st.is_null() {
        return Err(err_closed());
    }
    Ok(unsafe { &mut *b.st })
}

fn check_exports(o: &PyObject) -> PyResult<()> {
    if bio(o).exports > 0 {
        raise!(BufferError, "Existing exports of data: object cannot be re-sized");
    }
    Ok(())
}

unsafe extern "C" fn bytesio_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = (*tp).tp_alloc.unwrap()(tp, 0);
        if !op.is_null() {
            let b = &mut *(op as *mut BytesIO);
            b.st = Box::into_raw(Box::new(State { data: Data::Owned(Vec::new()), pos: 0 }));
        }
        op
    }
}

fn set_initial(o: &PyObject, init: Option<&PyObject>) -> PyResult<()> {
    check_exports(o)?;
    let data = match init {
        None => Data::Owned(Vec::new()),
        Some(x) if x.is(none::none()) => Data::Owned(Vec::new()),
        Some(x) if crate::types::bytes::is_bytes(x) && x.is_type(crate::types::bytes::bytes_type()) => Data::Shared(x.to_ref()),
        Some(x) => Data::Owned(BufView::get(x, false)?.as_slice().to_vec()),
    };
    let b = bio(o);
    if b.st.is_null() {
        b.st = Box::into_raw(Box::new(State { data, pos: 0 }));
    } else {
        let s = unsafe { &mut *b.st };
        s.data = data;
        s.pos = 0;
    }
    Ok(())
}

unsafe extern "C" fn bytesio_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [init] = kwparse::<1>("BytesIO", &["initial_bytes"], 0, 0, &refs, kws.as_deref())?;
            set_initial(o, init)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn bytesio_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let b = &mut *(op as *mut BytesIO);
        if !b.st.is_null() {
            drop(Box::from_raw(b.st));
            b.st = std::ptr::null_mut();
        }
        dealloc_end(op);
    }
}
unsafe extern "C" fn bytesio_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe { visit_header(op, visit, arg) }
}
unsafe extern "C" fn bytesio_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        0
    }
}

fn b_getvalue(o: &PyObject) -> PyResult {
    let s = state(o)?;
    if let Data::Shared(b) = &s.data {
        return Ok(b.clone());
    }
    Ok(new_bytes(s.bytes()))
}

/// Read up to n bytes (n < 0: everything) from the current position.
fn read_n(s: &mut State, n: isize) -> &[u8] {
    let len = s.len();
    let start = s.pos.min(len);
    let avail = len - start;
    let n = if n < 0 { avail } else { (n as usize).min(avail) };
    s.pos = start + n;
    &s.bytes()[start..start + n]
}

fn b_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    let s = state(o)?;
    // whole-buffer read of a shared bytes object returns it unchanged
    if s.pos == 0 && (n < 0 || n as usize >= s.len()) {
        if let Data::Shared(b) = &s.data {
            let r = b.clone();
            s.pos = s.len();
            return Ok(r);
        }
    }
    Ok(new_bytes(read_n(s, n)))
}

fn b_readinto(o: &PyObject, b: &PyObject) -> PyResult {
    let mut view = BufView::get_msg(b, true, Some("readinto() argument"))?;
    let s = state(o)?;
    let len = s.len();
    let start = s.pos.min(len);
    let n = view.len().min(len - start);
    // the target may overlap our own buffer (getbuffer()[...]): use memmove
    unsafe {
        std::ptr::copy(s.bytes().as_ptr().add(start), view.as_mut_slice().as_mut_ptr(), n);
    }
    s.pos = start + n;
    Ok(int::new_int(n as i64))
}

fn line_end(d: &[u8], start: usize, limit: isize) -> usize {
    let end = if limit < 0 { d.len() } else { (start + limit as usize).min(d.len()) };
    match d[start..end].iter().position(|&c| c == b'\n') {
        Some(i) => start + i + 1,
        None => end,
    }
}

fn b_readline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readline", args, 0, 1)?;
    let limit = size_arg(args.first().copied())?;
    let s = state(o)?;
    let d = s.bytes();
    let start = s.pos.min(d.len());
    let end = line_end(d, start, limit);
    let r = new_bytes(&d[start..end]);
    s.pos = end;
    Ok(r)
}

fn b_readlines(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readlines", args, 0, 1)?;
    let hint = size_arg(args.first().copied())?;
    let s = state(o)?;
    let out = list::new_list_empty();
    let mut total = 0usize;
    loop {
        let d = s.bytes();
        let start = s.pos.min(d.len());
        if start >= d.len() {
            break;
        }
        let end = line_end(d, start, -1);
        list::append(&out, new_bytes(&d[start..end]));
        total += end - start;
        s.pos = end;
        if hint > 0 && total >= hint as usize {
            break;
        }
    }
    Ok(out)
}

fn write_bytes(o: &PyObject, data: &[u8]) -> PyResult<usize> {
    state(o)?;
    check_exports(o)?;
    let s = state(o)?;
    if data.is_empty() {
        return Ok(0);
    }
    let pos = s.pos;
    let v = s.owned();
    if pos > v.len() {
        v.resize(pos, 0);
    }
    let end = pos + data.len();
    if end > v.len() {
        v.resize(end, 0);
    }
    v[pos..end].copy_from_slice(data);
    s.pos = end;
    Ok(data.len())
}

fn b_write(o: &PyObject, b: &PyObject) -> PyResult {
    state(o)?;
    check_exports(o)?;
    let view = BufView::get(b, false)?;
    let n = write_bytes(o, view.as_slice())?;
    Ok(int::new_int(n as i64))
}

fn b_writelines(o: &PyObject, lines: &PyObject) -> PyResult {
    state(o)?;
    let it = ab::get_iter(lines)?;
    while let Some(line) = ab::iter_next(&it)? {
        b_write(o, &line)?;
    }
    Ok(none::none_ref())
}

fn b_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    let pos = ssize_arg(args[0])?;
    let whence = if args.len() > 1 { crate::modules::posix::path::c_int(args[1])? } else { 0 };
    let s = state(o)?;
    let newpos = match whence {
        0 => {
            if pos < 0 {
                raise!(ValueError, "negative seek value {}", pos);
            }
            pos
        }
        1 => (s.pos as isize).saturating_add(pos).max(0),
        2 => (s.len() as isize).saturating_add(pos).max(0),
        _ => raise!(ValueError, "invalid whence ({}, should be 0, 1 or 2)", whence),
    };
    s.pos = newpos as usize;
    Ok(int::new_int(newpos as i64))
}

fn b_tell(o: &PyObject) -> PyResult {
    Ok(int::new_int(state(o)?.pos as i64))
}

fn b_truncate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("truncate", args, 0, 1)?;
    state(o)?;
    let size = match args.first() {
        None => None,
        Some(x) if x.is(none::none()) => None,
        Some(x) => Some(ssize_arg(x)?),
    };
    let s = state(o)?;
    check_exports(o)?;
    let size = size.unwrap_or(s.pos as isize);
    if size < 0 {
        raise!(ValueError, "negative size value {}", size);
    }
    if (size as usize) < s.len() {
        s.owned().truncate(size as usize);
    }
    Ok(int::new_int(size as i64))
}

fn b_close(o: &PyObject) -> PyResult {
    check_exports(o)?;
    let b = bio(o);
    if !b.st.is_null() {
        unsafe { drop(Box::from_raw(b.st)) };
        b.st = std::ptr::null_mut();
    }
    hdr(o).ioflags |= F_CLOSED;
    Ok(none::none_ref())
}

fn b_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(bio(o).st.is_null()))
}
fn b_true(o: &PyObject) -> PyResult {
    state(o)?;
    Ok(bool_::new_bool(true))
}
fn b_false(o: &PyObject) -> PyResult {
    state(o)?;
    Ok(bool_::new_bool(false))
}
fn b_flush(o: &PyObject) -> PyResult {
    state(o)?;
    Ok(none::none_ref())
}

fn b_getstate(o: &PyObject) -> PyResult {
    let v = b_getvalue(o)?;
    let s = state(o)?;
    let d = match crate::types::typeobject::instance_dict_existing(o) {
        Some(d) => dict::copy(&d)?,
        None => none::none_ref(),
    };
    Ok(tuple::new_tuple(vec![v, int::new_int(s.pos as i64), d]))
}

fn b_setstate(o: &PyObject, st: &PyObject) -> PyResult {
    if !tuple::is_tuple(st) || tuple::tuple_len(st) < 3 {
        raise!(TypeError, "{}.__setstate__ argument should be 3-tuple, got {}", crate::types::typeobject::type_name(o.ty()), tname(st));
    }
    state(o)?;
    check_exports(o)?;
    let items = tuple::tuple_items(st);
    let data = BufView::get(items[0], false)?.as_slice().to_vec();
    let posobj = items[1];
    if !int::is_int(posobj) {
        raise!(TypeError, "second item of state must be an integer, not {}", tname(posobj));
    }
    let pos = ab::index_as_ssize(posobj, Some(crate::types::exceptions::OverflowError()))?;
    if pos < 0 {
        raise!(ValueError, "position value cannot be negative");
    }
    let d = items[2];
    if !d.is(none::none()) && !dict::is_dict(d) {
        raise!(TypeError, "third item of state should be a dict, got a {}", tname(d));
    }
    let s = state(o)?;
    s.data = Data::Owned(data);
    s.pos = pos as usize;
    if dict::is_dict(d) {
        if let Some(own) = crate::types::typeobject::instance_dict(o) {
            dict::merge_dict(&own, d, true)?;
        }
    }
    Ok(none::none_ref())
}

fn b_sizeof(o: &PyObject) -> PyResult {
    let mut n = unsafe { (*o.type_ptr()).tp_basicsize } as usize;
    if let Ok(s) = state(o) {
        if let Data::Owned(v) = &s.data {
            if !v.is_empty() || v.capacity() > 0 {
                n += std::mem::offset_of!(PyBytesObject, ob_sval) + v.capacity() + 1;
            }
        } else if let Data::Shared(b) = &s.data {
            n += std::mem::offset_of!(PyBytesObject, ob_sval) + bytes::bytes_data(b).len() + 1;
        }
    } else {
        clear_exc();
    }
    Ok(int::new_int(n as i64))
}

unsafe extern "C" fn b_iternext(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        if !o.is_type(bytesio_type()) {
            // subclass may override readline
            let r = call0(o, "readline");
            return match r {
                Ok(l) if ab::object_len_or_zero(&l) > 0 => l.into_raw(),
                Ok(_) => std::ptr::null_mut(),
                Err(_) => std::ptr::null_mut(),
            };
        }
        let s = match state(o) {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };
        let d = s.bytes();
        let start = s.pos.min(d.len());
        if start >= d.len() {
            return std::ptr::null_mut();
        }
        let end = line_end(d, start, -1);
        let r = new_bytes(&d[start..end]);
        s.pos = end;
        r.into_raw()
    }
}
unsafe extern "C" fn b_iter(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        into_c(state(o).map(|_| o.to_ref()))
    }
}

// ---------------------------------------------------------------- getbuffer()

#[repr(C)]
struct BytesIOBuffer {
    ob_base: PyObject,
    source: *mut PyObject,
}

unsafe extern "C" fn buf_getbuffer(op: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let src = &*(*(op as *mut BytesIOBuffer)).source;
        let s = match state(src) {
            Ok(s) => s,
            Err(_) => return -1,
        };
        let v = s.owned();
        if v.capacity() == 0 {
            v.reserve(1);
        }
        let b = bio(src);
        b.exports += 1;
        (*view).buf = v.as_mut_ptr() as *mut c_void;
        (*view).obj = (*op).to_ref().into_raw();
        (*view).len = v.len() as isize;
        (*view).itemsize = 1;
        (*view).readonly = 0;
        (*view).ndim = 1;
        (*view).format = if flags & crate::modules::bufview::PYBUF_FORMAT != 0 { c"B".as_ptr() as *mut _ } else { std::ptr::null_mut() };
        (*view).shape = if flags & crate::modules::bufview::PYBUF_ND == crate::modules::bufview::PYBUF_ND { &raw mut (*view).len } else { std::ptr::null_mut() };
        (*view).strides = if flags & crate::modules::bufview::PYBUF_STRIDES == crate::modules::bufview::PYBUF_STRIDES { &raw mut (*view).itemsize } else { std::ptr::null_mut() };
        (*view).suboffsets = std::ptr::null_mut();
        (*view).internal = std::ptr::null_mut();
        0
    }
}
unsafe extern "C" fn buf_releasebuffer(op: *mut PyObject, _view: *mut Py_buffer) {
    unsafe {
        let src = &*(*(op as *mut BytesIOBuffer)).source;
        bio(src).exports -= 1;
    }
}
unsafe extern "C" fn buf_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        xdecref((*(op as *mut BytesIOBuffer)).source);
        crate::alloc::free_object(op);
    }
}
unsafe extern "C" fn buf_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe { visit_ptr((*(op as *mut BytesIOBuffer)).source, visit, arg) }
}
static mut BUF_PROCS: PyBufferProcs = PyBufferProcs { bf_getbuffer: Some(buf_getbuffer), bf_releasebuffer: Some(buf_releasebuffer) };

fn b_getbuffer(o: &PyObject) -> PyResult {
    state(o)?;
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut BYTESIOBUF_TYPE, std::mem::size_of::<BytesIOBuffer>());
        (*(op as *mut BytesIOBuffer)).source = o.to_ref().into_raw();
        crate::gc::track(op);
        let holder = PyRef::from_raw(op);
        let mv = crate::import::import_absolute("builtins").and_then(|b| ab::getattr_str(&b, "memoryview"))?;
        ab::call(&mv, &[&holder])
    }
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if BYTESIO_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup_type(
            bytesio_type(),
            c"_io.BytesIO",
            std::mem::size_of::<BytesIO>(),
            super::iobase::bufferediobase_type(),
            "BytesIO(initial_bytes=b'')\n--\n\nBuffered I/O implementation using an in-memory bytes buffer.",
        );
        let t = &mut *bytesio_type();
        t.tp_new = Some(bytesio_new);
        t.tp_init = Some(bytesio_init);
        t.tp_dealloc = Some(bytesio_dealloc);
        t.tp_traverse = Some(bytesio_traverse);
        t.tp_clear = Some(bytesio_clear);
        t.tp_iter = Some(b_iter);
        t.tp_iternext = Some(b_iternext);
        t.tp_methods = crate::mdefs![
            ("getvalue", NOARGS, b_getvalue),
            ("getbuffer", NOARGS, b_getbuffer),
            ("read", FAST, b_read),
            ("read1", FAST, b_read),
            ("readinto", O, b_readinto),
            ("readline", FAST, b_readline),
            ("readlines", FAST, b_readlines),
            ("write", O, b_write),
            ("writelines", O, b_writelines),
            ("seek", FAST, b_seek),
            ("tell", NOARGS, b_tell),
            ("truncate", FAST, b_truncate),
            ("close", NOARGS, b_close),
            ("readable", NOARGS, b_true),
            ("writable", NOARGS, b_true),
            ("seekable", NOARGS, b_true),
            ("isatty", NOARGS, b_false),
            ("flush", NOARGS, b_flush),
            ("__getstate__", NOARGS, b_getstate),
            ("__setstate__", O, b_setstate),
            ("__sizeof__", NOARGS, b_sizeof),
        ];
        t.tp_getset = crate::getset_table![("closed", b_closed)];
        crate::types::typeobject::type_ready(bytesio_type())?;

        let t = &mut *(&raw mut BYTESIOBUF_TYPE);
        t.tp_name = c"_io._BytesIOBuffer".as_ptr();
        t.tp_basicsize = std::mem::size_of::<BytesIOBuffer>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(buf_dealloc);
        t.tp_traverse = Some(buf_traverse);
        t.tp_as_buffer = &raw mut BUF_PROCS;
        crate::types::typeobject::type_ready(&raw mut BYTESIOBUF_TYPE)?;
    }
    Ok(())
}
