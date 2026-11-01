//! sqlite3.Blob: incremental BLOB I/O.

use super::connection::{self, checked, conn};
use super::ffi::*;
use super::*;
use rpy_runtime::modules::fncommon::{self as fc, Trav};
use std::ffi::{CString, c_void};

pub struct BlobObj {
    pub connection: Option<PyRef>,
    pub blob: *mut Blob,
    pub offset: i64,
}

impl Trav for BlobObj {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        match &self.connection {
            Some(c) => v(c.as_ptr()),
            None => 0,
        }
    }
}

impl Drop for BlobObj {
    fn drop(&mut self) {
        if !self.blob.is_null() {
            let b = self.blob;
            self.blob = std::ptr::null_mut();
            sq!(sqlite3_blob_close(b));
        }
    }
}

pub static mut BLOB_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn blob_type() -> *mut PyTypeObject {
    &raw mut BLOB_TYPE
}

fn bl(o: &PyObject) -> &'static mut BlobObj {
    unsafe { &mut *(fc::rs::<BlobObj>(o) as *mut BlobObj) }
}

pub fn close_blob(o: &PyObject) {
    let b = bl(o);
    if !b.blob.is_null() {
        let p = b.blob;
        b.blob = std::ptr::null_mut();
        unsafe {
            let ts = capi::PyEval_SaveThread();
            (lib().sqlite3_blob_close.unwrap())(p);
            capi::PyEval_RestoreThread(ts);
        }
    }
}

fn check(o: &PyObject) -> PyResult<&'static mut BlobObj> {
    let b = bl(o);
    if let Some(c) = &b.connection {
        checked(c)?;
    }
    if b.blob.is_null() {
        return Err(programming_error("Cannot operate on a closed blob."));
    }
    Ok(b)
}

fn blob_error(b: &BlobObj) -> PyErr {
    let c = conn(b.connection.as_ref().unwrap());
    set_db_error(c.db)
}

fn length(b: &BlobObj) -> i64 {
    sq!(sqlite3_blob_bytes(b.blob)) as i64
}

fn read_at(b: &BlobObj, offset: i64, n: i64) -> PyResult<Vec<u8>> {
    let mut buf = vec![0u8; n.max(0) as usize];
    if n == 0 {
        return Ok(buf);
    }
    let blob = b.blob;
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_blob_read.unwrap())(blob, buf.as_mut_ptr() as *mut c_void, n as c_int, offset as c_int);
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_OK {
        return Err(blob_error(b));
    }
    Ok(buf)
}

fn write_at(b: &BlobObj, offset: i64, data: &[u8]) -> PyResult<()> {
    if data.is_empty() {
        return Ok(());
    }
    let blob = b.blob;
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_blob_write.unwrap())(blob, data.as_ptr() as *const c_void, data.len() as c_int, offset as c_int);
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_OK {
        return Err(blob_error(b));
    }
    Ok(())
}

fn buffer_bytes(o: &PyObject) -> PyResult<Vec<u8>> {
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(o.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
        let v = std::slice::from_raw_parts(view.buf as *const u8, view.len as usize).to_vec();
        capi::PyBuffer_Release(&mut view);
        Ok(v)
    }
}

// ------------------------------------------------------------------ methods

pub fn blobopen(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("blobopen", &["table", "column", "row", "readonly", "name"], 3, 3).kwonly(2);
    let [table, column, row, readonly, name] = rpy_runtime::args::parse::<5>(&P, args, kw)?;
    let c = checked(o)?;
    let table = utf8_cstring(table.unwrap())?;
    let column = utf8_cstring(column.unwrap())?;
    let row = rpy_runtime::abstract_::as_i64(row.unwrap())?;
    let readonly = match readonly {
        Some(r) => rpy_runtime::abstract_::is_true(r)?,
        None => false,
    };
    let name = match name {
        Some(n) => utf8_cstring(n)?,
        None => CString::new("main").unwrap(),
    };
    let mut blob: *mut Blob = std::ptr::null_mut();
    let db = c.db;
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_blob_open.unwrap())(db, name.as_ptr(), table.as_ptr(), column.as_ptr(), row, (!readonly) as c_int, &mut blob);
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_OK {
        return Err(set_db_error(db));
    }
    let obj = fc::robj_new(blob_type(), BlobObj { connection: Some(o.to_ref()), blob, offset: 0 })?;
    // the connection closes its blobs; it keeps weak references to them
    let w = from_c(unsafe { capi::PyWeakref_NewRef(obj.as_ptr(), std::ptr::null_mut()) })?;
    c.blobs.retain(|w| matches!(connection::weak_get(w), Ok(Some(_))));
    c.blobs.push(w);
    Ok(obj)
}

fn m_close(o: &PyObject) -> PyResult {
    let b = bl(o);
    if let Some(c) = &b.connection {
        checked(c)?;
    }
    close_blob(o);
    Ok(none())
}

fn m_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let b = check(o)?;
    let len = length(b);
    let mut n = if !args.is_empty() { unsafe { capi::PyLong_AsInt(args[0].as_ptr()) as i64 } } else { -1 };
    if n == -1 && err_occurred() {
        return Err(PyErr);
    }
    let max = len - b.offset;
    if n < 0 || n > max {
        n = max;
    }
    let data = read_at(b, b.offset, n)?;
    b.offset += n;
    Ok(rpy_runtime::types::bytes::new_bytes(&data))
}

fn m_write(o: &PyObject, data: &PyObject) -> PyResult {
    let b = check(o)?;
    let d = buffer_bytes(data)?;
    if d.len() as i64 > length(b) - b.offset {
        raise!(ValueError, "data longer than blob length");
    }
    write_at(b, b.offset, &d)?;
    b.offset += d.len() as i64;
    Ok(none())
}

fn m_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "seek expected 1 or 2 arguments, got {}", args.len());
    }
    let b = check(o)?;
    let offset = unsafe { capi::PyLong_AsInt(args[0].as_ptr()) } as i64;
    if offset == -1 && err_occurred() {
        return Err(PyErr);
    }
    let origin = if args.len() == 2 { unsafe { capi::PyLong_AsInt(args[1].as_ptr()) } } else { 0 };
    if origin == -1 && err_occurred() {
        return Err(PyErr);
    }
    let len = length(b);
    let base = match origin {
        0 => 0,
        1 => b.offset,
        2 => len,
        _ => raise!(ValueError, "'origin' should be os.SEEK_SET, os.SEEK_CUR, or os.SEEK_END"),
    };
    let target = base + offset;
    if (origin != 0) && target > i32::MAX as i64 {
        raise!(OverflowError, "seek offset results in overflow");
    }
    if target < 0 || target > len {
        raise!(ValueError, "offset out of blob range");
    }
    b.offset = target;
    Ok(none())
}

fn m_tell(o: &PyObject) -> PyResult {
    let b = check(o)?;
    Ok(new_int(b.offset))
}

fn m_enter(o: &PyObject) -> PyResult {
    check(o)?;
    Ok(o.to_ref())
}

fn m_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    check(o)?;
    close_blob(o);
    Ok(rpy_runtime::types::bool_::new_bool(false))
}

unsafe extern "C" fn blob_length(o: *mut PyObject) -> isize {
    match check(unsafe { &*o }) {
        Ok(b) => length(b) as isize,
        Err(_) => -1,
    }
}

fn index_of(key: &PyObject, len: i64) -> PyResult<i64> {
    let i = rpy_runtime::abstract_::index_as_ssize(key, Some(rpy_runtime::types::exceptions::IndexError()))? as i64;
    let j = if i < 0 { i + len } else { i };
    if j < 0 || j >= len {
        raise!(IndexError, "Blob index out of range");
    }
    Ok(j)
}

fn is_slice(o: &PyObject) -> bool {
    unsafe { o.type_ptr() == &raw mut rpy_runtime::types::slice::PySlice_Type }
}

fn slice_info(key: &PyObject, len: i64) -> PyResult<(i64, i64, i64)> {
    let (mut start, mut stop, mut step) = (0isize, 0isize, 0isize);
    unsafe {
        if capi::PySlice_Unpack(key.as_ptr(), &mut start, &mut stop, &mut step) < 0 {
            return Err(PyErr);
        }
        let n = capi::PySlice_AdjustIndices(len as isize, &mut start, &mut stop, step);
        Ok((start as i64, step as i64, n as i64))
    }
}

fn subscript(o: &PyObject, key: &PyObject) -> PyResult {
    let b = check(o)?;
    let len = length(b);
    if rpy_runtime::abstract_::has_index(key) {
        let i = index_of(key, len)?;
        let d = read_at(b, i, 1)?;
        return Ok(new_int(d[0] as i64));
    }
    if is_slice(key) {
        let (start, step, n) = slice_info(key, len)?;
        if n <= 0 {
            return Ok(rpy_runtime::types::bytes::new_bytes(&[]));
        }
        if step == 1 {
            let d = read_at(b, start, n)?;
            return Ok(rpy_runtime::types::bytes::new_bytes(&d));
        }
        let all = read_at(b, 0, len)?;
        let mut out = Vec::with_capacity(n as usize);
        let mut cur = start;
        for _ in 0..n {
            out.push(all[cur as usize]);
            cur += step;
        }
        return Ok(rpy_runtime::types::bytes::new_bytes(&out));
    }
    raise!(TypeError, "Blob indices must be integers")
}

fn ass_subscript(o: &PyObject, key: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let b = check(o)?;
    let len = length(b);
    if rpy_runtime::abstract_::has_index(key) {
        let v = match v {
            Some(v) => v,
            None => raise!(TypeError, "Blob doesn't support item deletion"),
        };
        let i = index_of(key, len)?;
        let byte = rpy_runtime::abstract_::index(v)?;
        let val = match rpy_runtime::types::int::as_i64(&byte) {
            Some(x) if (0..256).contains(&x) => x as u8,
            _ => raise!(ValueError, "byte must be in range(0, 256)"),
        };
        return write_at(b, i, &[val]);
    }
    if is_slice(key) {
        let v = match v {
            Some(v) => v,
            None => raise!(TypeError, "Blob doesn't support slice deletion"),
        };
        let (start, step, n) = slice_info(key, len)?;
        let data = buffer_bytes(v)?;
        if data.len() as i64 != n.max(0) {
            raise!(IndexError, "Blob slice assignment is wrong size");
        }
        if n <= 0 {
            return Ok(());
        }
        if step == 1 {
            return write_at(b, start, &data);
        }
        let mut all = read_at(b, 0, len)?;
        let mut cur = start;
        for &x in &data {
            all[cur as usize] = x;
            cur += step;
        }
        return write_at(b, 0, &all);
    }
    raise!(TypeError, "Blob indices must be integers")
}

unsafe extern "C" fn blob_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript(&*o, &*k)) }
}

unsafe extern "C" fn blob_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_subscript(&*o, &*k, if v.is_null() { None } else { Some(&*v) })) }
}

static mut BLOB_AS_MAPPING: PyMappingMethods = unsafe { std::mem::zeroed() };

pub fn init_type() -> PyResult<()> {
    unsafe {
        let t = blob_type();
        fc::init_robj_type::<BlobObj>(t, c"sqlite3.Blob", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        BLOB_AS_MAPPING.mp_length = Some(blob_length);
        BLOB_AS_MAPPING.mp_subscript = Some(blob_subscript);
        BLOB_AS_MAPPING.mp_ass_subscript = Some(blob_ass_subscript);
        (*t).tp_as_mapping = &raw mut BLOB_AS_MAPPING;
        (*t).tp_methods = rpy_runtime::mdefs![
            ("read", FAST, m_read, "Read data at the current offset position."),
            ("write", O, m_write, "Write data at the current offset."),
            ("seek", FAST, m_seek, "Set the current access position to offset."),
            ("tell", NOARGS, m_tell, "Return the current access position for the blob."),
            ("close", NOARGS, m_close, "Close the blob."),
            ("__enter__", NOARGS, m_enter, "Blob context manager enter."),
            ("__exit__", FAST, m_exit, "Blob context manager exit."),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
