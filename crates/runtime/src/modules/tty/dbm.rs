//! `_gdbm` (GNU dbm) and `_dbm` (ndbm interface, provided by gdbm_compat) — thin
//! bindings to the system libraries, loaded at runtime (see dl.rs).
//!
//! Both modules expose one mapping-like database type; they share the Python-facing
//! logic and differ only in the backend calls, so the object holds a `Backend` enum.

use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, dict, exceptions, int, list, none, str as pystr, tuple, typeobject};
use crate::{dl_api, mdefs, raise};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

#[repr(C)]
#[derive(Clone, Copy)]
struct Datum {
    dptr: *mut c_char,
    dsize: c_int,
}

dl_api! {
    pub struct GdbmApi, fn gdbm_api, ["libgdbm.so.6", "libgdbm.so"] {
        fn gdbm_open(name: *const c_char, bs: c_int, flags: c_int, mode: c_int, fatal: *const c_void) -> *mut c_void;
        fn gdbm_close(f: *mut c_void) -> c_int;
        fn gdbm_store(f: *mut c_void, k: Datum, v: Datum, flag: c_int) -> c_int;
        fn gdbm_fetch(f: *mut c_void, k: Datum) -> Datum;
        fn gdbm_delete(f: *mut c_void, k: Datum) -> c_int;
        fn gdbm_firstkey(f: *mut c_void) -> Datum;
        fn gdbm_nextkey(f: *mut c_void, k: Datum) -> Datum;
        fn gdbm_reorganize(f: *mut c_void) -> c_int;
        fn gdbm_sync(f: *mut c_void) -> c_int;
        fn gdbm_exists(f: *mut c_void, k: Datum) -> c_int;
        fn gdbm_strerror(e: c_int) -> *const c_char;
        fn gdbm_errno_location() -> *mut c_int;
        ?fn gdbm_count(f: *mut c_void, n: *mut u64) -> c_int;
        ?fn gdbm_check_syserr(e: c_int) -> c_int;
        ?fn gdbm_last_errno(f: *mut c_void) -> c_int;
        ?fn gdbm_db_strerror(f: *mut c_void) -> *const c_char;
        ?fn gdbm_clear_error(f: *mut c_void);
        static gdbm_version_number: [c_int; 3];
    }
}

dl_api! {
    pub struct NdbmApi, fn ndbm_api, ["libgdbm_compat.so.4", "libgdbm_compat.so"] {
        fn dbm_open(name: *const c_char, flags: c_int, mode: c_int) -> *mut c_void;
        fn dbm_close(f: *mut c_void);
        fn dbm_fetch(f: *mut c_void, k: Datum) -> Datum;
        fn dbm_store(f: *mut c_void, k: Datum, v: Datum, flag: c_int) -> c_int;
        fn dbm_delete(f: *mut c_void, k: Datum) -> c_int;
        fn dbm_firstkey(f: *mut c_void) -> Datum;
        fn dbm_nextkey(f: *mut c_void) -> Datum;
        fn dbm_error(f: *mut c_void) -> c_int;
        fn dbm_clearerr(f: *mut c_void);
    }
}

const GDBM_READER: c_int = 0;
const GDBM_WRITER: c_int = 1;
const GDBM_WRCREAT: c_int = 2;
const GDBM_NEWDB: c_int = 3;
const GDBM_FAST: c_int = 0x10;
const GDBM_SYNC: c_int = 0x20;
const GDBM_NOLOCK: c_int = 0x40;
const GDBM_INSERT: c_int = 0;
const GDBM_REPLACE: c_int = 1;
const GDBM_ITEM_NOT_FOUND: c_int = 15;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Gnu,
    Ndbm,
}

struct Db {
    kind: Kind,
    handle: *mut c_void,
    /// cached number of keys (-1 = unknown)
    size: isize,
    readonly: bool,
}

impl Trav for Db {}

impl Drop for Db {
    fn drop(&mut self) {
        close_handle(self);
    }
}

fn close_handle(db: &mut Db) {
    if db.handle.is_null() {
        return;
    }
    unsafe {
        match db.kind {
            Kind::Gnu => {
                if let Ok(a) = gdbm_api() {
                    (a.gdbm_close)(db.handle);
                }
            }
            Kind::Ndbm => {
                if let Ok(a) = ndbm_api() {
                    (a.dbm_close)(db.handle);
                }
            }
        }
    }
    db.handle = std::ptr::null_mut();
}

#[unsafe(no_mangle)]
pub static mut RPY_GDBM_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut RPY_NDBM_TYPE: PyTypeObject = PyTypeObject::ZERO;

static GDBM_ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static NDBM_ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn error_type(kind: Kind) -> *mut PyTypeObject {
    let p = match kind {
        Kind::Gnu => GDBM_ERROR.load(Ordering::Acquire),
        Kind::Ndbm => NDBM_ERROR.load(Ordering::Acquire),
    };
    if p.is_null() { exceptions::OSError() } else { p as *mut PyTypeObject }
}

fn raise_error(kind: Kind, msg: &str) -> PyErr {
    new_err(error_type(kind), msg)
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Gnu => "gdbm",
        Kind::Ndbm => "dbm",
    }
}

fn st(o: &PyObject) -> &mut Db {
    fc::rs::<Db>(o)
}

fn check_open(o: &PyObject) -> PyResult<&mut Db> {
    let db = st(o);
    if db.handle.is_null() {
        return Err(raise_error(
            db.kind,
            match db.kind {
                Kind::Gnu => "GDBM object has already been closed",
                Kind::Ndbm => "DBM object has already been closed",
            },
        ));
    }
    Ok(db)
}

/// Key/value bytes from a str (UTF-8) or a bytes-like object.
fn data_arg(kind: Kind, o: &PyObject, what: &str) -> PyResult<Vec<u8>> {
    if pystr::is_str(o) {
        return Ok(pystr::as_utf8(o)?.as_bytes().to_vec());
    }
    if bytes::is_bytes(o) {
        return Ok(bytes::bytes_data(o).to_vec());
    }
    if crate::buffer::check_buffer(o) {
        return crate::buffer::with_buffer(o, |b| b.to_vec());
    }
    match what {
        "key" => raise!(TypeError, "{} key must be bytes or string, not {}", label(kind), tname(o)),
        "index" => raise!(TypeError, "{} mappings have bytes or string indices only", label(kind)),
        _ => raise!(TypeError, "{} mappings have bytes or string elements only", label(kind)),
    }
}

fn datum(b: &[u8]) -> Datum {
    Datum { dptr: b.as_ptr() as *mut c_char, dsize: b.len() as c_int }
}

unsafe fn datum_bytes(d: Datum) -> PyRef {
    if d.dptr.is_null() || d.dsize <= 0 {
        return bytes::new_bytes(b"");
    }
    bytes::new_bytes(unsafe { std::slice::from_raw_parts(d.dptr as *const u8, d.dsize as usize) })
}

fn gdbm_errno(a: &GdbmApi) -> c_int {
    unsafe { *(a.gdbm_errno_location)() }
}

fn gdbm_error_msg(a: &GdbmApi, handle: *mut c_void) -> String {
    unsafe {
        if !handle.is_null() {
            if let Some(f) = a.gdbm_db_strerror {
                let p = f(handle);
                if !p.is_null() {
                    return CStr::from_ptr(p).to_string_lossy().into_owned();
                }
            }
        }
        let p = (a.gdbm_strerror)(gdbm_errno(a));
        if p.is_null() { "unknown error".into() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

fn gdbm_raise(a: &GdbmApi, handle: *mut c_void) -> PyErr {
    let e = gdbm_errno(a);
    let sys = a.gdbm_check_syserr.map(|f| unsafe { f(e) } != 0).unwrap_or(false);
    if sys {
        let errno = crate::modules::posix::path::errno();
        if errno != 0 {
            return set_os_error(Kind::Gnu, errno, None);
        }
    }
    let msg = gdbm_error_msg(a, handle);
    if let Some(f) = a.gdbm_clear_error {
        if !handle.is_null() {
            unsafe { f(handle) };
        }
    }
    raise_error(Kind::Gnu, &msg)
}

/// error(errno, strerror(errno)[, filename])
fn set_os_error(kind: Kind, errno: i32, filename: Option<&PyObject>) -> PyErr {
    let msg = unsafe { CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy().into_owned();
    let mut args = vec![int::new_int(errno as i64), pystr::new_str(&msg)];
    if let Some(f) = filename {
        args.push(f.to_ref());
    }
    match exceptions::make_exception(error_type(kind), &args) {
        Ok(e) => set_exc(e),
        Err(e) => return e,
    }
    PyErr
}

// ------------------------------------------------------------------ backend ops

fn fetch(db: &Db, key: &[u8]) -> PyResult<Option<PyRef>> {
    unsafe {
        match db.kind {
            Kind::Gnu => {
                let a = gdbm_api()?;
                let d = (a.gdbm_fetch)(db.handle, datum(key));
                if d.dptr.is_null() {
                    return Ok(None);
                }
                let r = datum_bytes(d);
                libc::free(d.dptr as *mut c_void);
                Ok(Some(r))
            }
            Kind::Ndbm => {
                let a = ndbm_api()?;
                let d = (a.dbm_fetch)(db.handle, datum(key));
                if d.dptr.is_null() {
                    return Ok(None);
                }
                Ok(Some(datum_bytes(d)))
            }
        }
    }
}

fn exists(db: &Db, key: &[u8]) -> PyResult<bool> {
    unsafe {
        match db.kind {
            Kind::Gnu => {
                let a = gdbm_api()?;
                Ok((a.gdbm_exists)(db.handle, datum(key)) != 0)
            }
            Kind::Ndbm => Ok(fetch(db, key)?.is_some()),
        }
    }
}

fn store(db: &mut Db, key: &[u8], val: &[u8]) -> PyResult<()> {
    db.size = -1;
    unsafe {
        match db.kind {
            Kind::Gnu => {
                let a = gdbm_api()?;
                if (a.gdbm_store)(db.handle, datum(key), datum(val), GDBM_REPLACE) < 0 {
                    return Err(gdbm_raise(a, db.handle));
                }
            }
            Kind::Ndbm => {
                let a = ndbm_api()?;
                if (a.dbm_store)(db.handle, datum(key), datum(val), GDBM_REPLACE) < 0 {
                    (a.dbm_clearerr)(db.handle);
                    return Err(raise_error(Kind::Ndbm, "cannot add item to database"));
                }
            }
        }
    }
    Ok(())
}

fn delete(db: &mut Db, keyobj: &PyObject, key: &[u8]) -> PyResult<()> {
    db.size = -1;
    unsafe {
        match db.kind {
            Kind::Gnu => {
                let a = gdbm_api()?;
                if (a.gdbm_delete)(db.handle, datum(key)) < 0 {
                    if gdbm_errno(a) == GDBM_ITEM_NOT_FOUND {
                        if let Some(f) = a.gdbm_clear_error {
                            f(db.handle);
                        }
                        return Err(set_err_obj(exceptions::KeyError(), keyobj.to_ref()));
                    }
                    return Err(gdbm_raise(a, db.handle));
                }
            }
            Kind::Ndbm => {
                let a = ndbm_api()?;
                if (a.dbm_delete)(db.handle, datum(key)) < 0 {
                    if (a.dbm_error)(db.handle) != 0 {
                        (a.dbm_clearerr)(db.handle);
                        return Err(raise_error(Kind::Ndbm, "cannot delete item from database"));
                    }
                    return Err(set_err_obj(exceptions::KeyError(), keyobj.to_ref()));
                }
            }
        }
    }
    Ok(())
}

/// All keys, in database order.
fn all_keys(db: &Db) -> PyResult<Vec<PyRef>> {
    let mut out = Vec::new();
    unsafe {
        match db.kind {
            Kind::Gnu => {
                let a = gdbm_api()?;
                let mut k = (a.gdbm_firstkey)(db.handle);
                while !k.dptr.is_null() {
                    out.push(datum_bytes(k));
                    let n = (a.gdbm_nextkey)(db.handle, k);
                    libc::free(k.dptr as *mut c_void);
                    k = n;
                }
            }
            Kind::Ndbm => {
                let a = ndbm_api()?;
                let mut k = (a.dbm_firstkey)(db.handle);
                while !k.dptr.is_null() {
                    out.push(datum_bytes(k));
                    k = (a.dbm_nextkey)(db.handle);
                }
            }
        }
    }
    Ok(out)
}

fn count(db: &mut Db) -> PyResult<usize> {
    if db.size >= 0 {
        return Ok(db.size as usize);
    }
    let n = if db.kind == Kind::Gnu {
        let a = gdbm_api()?;
        match a.gdbm_count {
            Some(f) => {
                let mut n: u64 = 0;
                if unsafe { f(db.handle, &mut n) } < 0 {
                    return Err(gdbm_raise(a, db.handle));
                }
                n as usize
            }
            None => all_keys(db)?.len(),
        }
    } else {
        all_keys(db)?.len()
    };
    db.size = n as isize;
    Ok(n)
}

// ------------------------------------------------------------------ slots

unsafe extern "C" fn db_len(o: *mut PyObject) -> Py_ssize_t {
    let r = (|| -> PyResult<isize> {
        let db = check_open(unsafe { &*o })?;
        Ok(count(db)? as isize)
    })();
    r.unwrap_or(-1)
}

unsafe extern "C" fn db_bool(o: *mut PyObject) -> c_int {
    let r = (|| -> PyResult<bool> {
        let db = check_open(unsafe { &*o })?;
        if db.size >= 0 {
            return Ok(db.size > 0);
        }
        unsafe {
            match db.kind {
                Kind::Gnu => {
                    let a = gdbm_api()?;
                    let k = (a.gdbm_firstkey)(db.handle);
                    if k.dptr.is_null() {
                        return Ok(false);
                    }
                    libc::free(k.dptr as *mut c_void);
                    Ok(true)
                }
                Kind::Ndbm => {
                    let a = ndbm_api()?;
                    Ok(!(a.dbm_firstkey)(db.handle).dptr.is_null())
                }
            }
        }
    })();
    match r {
        Ok(b) => b as c_int,
        Err(_) => -1,
    }
}

unsafe extern "C" fn db_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    into_c((|| -> PyResult {
        let (o, k) = unsafe { (&*o, &*k) };
        let db = check_open(o)?;
        let key = data_arg(db.kind, k, "index")?;
        match fetch(db, &key)? {
            Some(v) => Ok(v),
            None => Err(set_err_obj(exceptions::KeyError(), k.to_ref())),
        }
    })())
}

unsafe extern "C" fn db_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    int_into_c((|| -> PyResult<()> {
        let (o, k) = unsafe { (&*o, &*k) };
        let db = check_open(o)?;
        let key = data_arg(db.kind, k, "index")?;
        if v.is_null() {
            delete(db, k, &key)
        } else {
            let val = data_arg(db.kind, unsafe { &*v }, "element")?;
            store(db, &key, &val)
        }
    })())
}

unsafe extern "C" fn db_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    match (|| -> PyResult<bool> {
        let (o, k) = unsafe { (&*o, &*k) };
        let db = check_open(o)?;
        let key = data_arg(db.kind, k, "key")?;
        exists(db, &key)
    })() {
        Ok(b) => b as c_int,
        Err(_) => -1,
    }
}

// ------------------------------------------------------------------ methods

fn m_close(o: &PyObject) -> PyResult {
    close_handle(st(o));
    Ok(none::none_ref())
}

fn m_keys(o: &PyObject) -> PyResult {
    let db = check_open(o)?;
    Ok(list::new_list(all_keys(db)?))
}

fn m_get(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", args, 1, 2)?;
    let db = check_open(o)?;
    let key = data_arg(db.kind, args[0], "key")?;
    match fetch(db, &key)? {
        Some(v) => Ok(v),
        None => Ok(args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref)),
    }
}

fn m_setdefault(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setdefault", args, 1, 2)?;
    let db = check_open(o)?;
    let key = data_arg(db.kind, args[0], "key")?;
    if let Some(v) = fetch(db, &key)? {
        return Ok(v);
    }
    let default = match args.get(1) {
        Some(d) => d.to_ref(),
        None => match db.kind {
            Kind::Ndbm => bytes::new_bytes(b""),
            Kind::Gnu => none::none_ref(),
        },
    };
    let val = data_arg(db.kind, &default, "element")?;
    store(db, &key, &val)?;
    Ok(default)
}

fn m_clear(o: &PyObject) -> PyResult {
    let db = check_open(o)?;
    for k in all_keys(db)? {
        let key = bytes::bytes_data(&k).to_vec();
        delete(db, &k, &key)?;
    }
    db.size = 0;
    Ok(none::none_ref())
}

fn m_firstkey(o: &PyObject) -> PyResult {
    let db = check_open(o)?;
    let a = gdbm_api()?;
    unsafe {
        let k = (a.gdbm_firstkey)(db.handle);
        if k.dptr.is_null() {
            return Ok(none::none_ref());
        }
        let r = datum_bytes(k);
        libc::free(k.dptr as *mut c_void);
        Ok(r)
    }
}

fn m_nextkey(o: &PyObject, key: &PyObject) -> PyResult {
    let db = check_open(o)?;
    let kb = data_arg(db.kind, key, "key")?;
    let a = gdbm_api()?;
    unsafe {
        let k = (a.gdbm_nextkey)(db.handle, datum(&kb));
        if k.dptr.is_null() {
            return Ok(none::none_ref());
        }
        let r = datum_bytes(k);
        libc::free(k.dptr as *mut c_void);
        Ok(r)
    }
}

fn m_reorganize(o: &PyObject) -> PyResult {
    let db = check_open(o)?;
    let a = gdbm_api()?;
    unsafe { *(a.gdbm_errno_location)() = 0 };
    if unsafe { (a.gdbm_reorganize)(db.handle) } < 0 {
        return Err(gdbm_raise(a, db.handle));
    }
    Ok(none::none_ref())
}

fn m_sync(o: &PyObject) -> PyResult {
    let db = check_open(o)?;
    let a = gdbm_api()?;
    unsafe { (a.gdbm_sync)(db.handle) };
    Ok(none::none_ref())
}

fn m_enter(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

fn m_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    close_handle(st(o));
    Ok(none::none_ref())
}

// The database handle is not thread-safe: every slot and method runs with the object lock.
crate::locked_slot!(l_db_len, db_len, () -> Py_ssize_t);
crate::locked_slot!(l_db_bool, db_bool, () -> c_int);
crate::locked_slot!(l_db_subscript, db_subscript, (k: *mut PyObject) -> *mut PyObject);
crate::locked_slot!(l_db_ass_subscript, db_ass_subscript, (k: *mut PyObject, v: *mut PyObject) -> c_int);
crate::locked_slot!(l_db_contains, db_contains, (k: *mut PyObject) -> c_int);

fn init_db_type(t: *mut PyTypeObject, name: &'static CStr, gnu: bool) -> PyResult<()> {
    unsafe {
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Db>(t, name, Py_TPFLAGS_DISALLOW_INSTANTIATION);
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_contains = Some(l_db_contains);
        (*t).tp_as_sequence = Box::into_raw(Box::new(sq));
        let mut mp: PyMappingMethods = std::mem::zeroed();
        mp.mp_length = Some(l_db_len);
        mp.mp_subscript = Some(l_db_subscript);
        mp.mp_ass_subscript = Some(l_db_ass_subscript);
        (*t).tp_as_mapping = Box::into_raw(Box::new(mp));
        let mut nb: PyNumberMethods = std::mem::zeroed();
        nb.nb_bool = Some(l_db_bool);
        (*t).tp_as_number = Box::into_raw(Box::new(nb));
        (*t).tp_methods = if gnu {
            mdefs![
                ("close", NOARGS, m_close, "close($self, /)\n--\n\nClose the database."),
                ("keys", NOARGS, m_keys, "keys($self, /)\n--\n\nGet a list of all keys in the database."),
                ("firstkey", NOARGS, m_firstkey, "firstkey($self, /)\n--\n\nReturn the starting key for the traversal."),
                ("nextkey", O, m_nextkey, "nextkey($self, key, /)\n--\n\nReturn the key that follows key in the traversal."),
                ("reorganize", NOARGS, m_reorganize, "reorganize($self, /)\n--\n\nReorganize the database to reclaim space."),
                ("sync", NOARGS, m_sync, "sync($self, /)\n--\n\nFlush the database to disk."),
                ("get", FAST, m_get, "get($self, key, default=None, /)\n--\n\nGet the value for key, or default."),
                ("setdefault", FAST, m_setdefault, "setdefault($self, key, default=None, /)\n--\n\nGet the value for key, inserting default if missing."),
                ("clear", NOARGS, m_clear, "clear($self, /)\n--\n\nRemove all items from the database."),
                ("__enter__", NOARGS, m_enter),
                ("__exit__", FAST, m_exit),
            ]
        } else {
            mdefs![
                ("close", NOARGS, m_close, "close($self, /)\n--\n\nClose the database."),
                ("keys", NOARGS, m_keys, "keys($self, /)\n--\n\nReturn a list of all keys in the database."),
                ("get", FAST, m_get, "get($self, key, default=None, /)\n--\n\nReturn the value for key if present, otherwise default."),
                ("setdefault", FAST, m_setdefault, "setdefault($self, key, default=b'', /)\n--\n\nReturn the value for key, storing default first if key is missing."),
                ("clear", NOARGS, m_clear, "clear($self, /)\n--\n\nRemove all items from the database."),
                ("__enter__", NOARGS, m_enter),
                ("__exit__", FAST, m_exit),
            ]
        };
        crate::methods::lock_self_methods(t);
        typeobject::type_ready(t)?;
    }
    Ok(())
}

fn new_db(tp: *mut PyTypeObject, kind: Kind, handle: *mut c_void, readonly: bool) -> PyResult {
    fc::robj_new(tp, Db { kind, handle, size: -1, readonly })
}

// ------------------------------------------------------------------ open()

fn path_bytes(fname: &str, o: &PyObject) -> PyResult<(PyRef, std::ffi::CString)> {
    let p = crate::modules::posix::path::path_named(fname, "filename", o)?;
    // keep the original (decoded) filename for error messages
    let shown = crate::modules::posix::path::fspath(o)?;
    Ok((shown, p.c))
}

fn gdbm_open(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("open", args, 1, 3)?;
    let a = gdbm_api()?;
    let (shown, cpath) = path_bytes("open", args[0])?;
    let flags = match args.get(1) {
        Some(f) => {
            if !pystr::is_str(f) {
                raise!(TypeError, "open() argument 2 must be str, not {}", tname(f));
            }
            pystr::as_utf8(f)?.to_string()
        }
        None => "r".to_string(),
    };
    let mode = match args.get(2) {
        Some(m) => crate::modules::posix::path::c_int(m)?,
        None => 0o666,
    };
    let mut chars = flags.chars();
    let mut iflags = match chars.next() {
        Some('r') => GDBM_READER,
        Some('w') => GDBM_WRITER,
        Some('c') => GDBM_WRCREAT,
        Some('n') => GDBM_NEWDB,
        _ => return Err(raise_error(Kind::Gnu, "First flag must be one of 'r', 'w', 'c' or 'n'")),
    };
    for c in chars {
        match c {
            'f' => iflags |= GDBM_FAST,
            's' => iflags |= GDBM_SYNC,
            'u' => iflags |= GDBM_NOLOCK,
            _ => return Err(raise_error(Kind::Gnu, &format!("Flag '{}' is not supported.", c))),
        }
    }
    unsafe { *(a.gdbm_errno_location)() = 0 };
    let h = crate::threads::blocking(|| unsafe { (a.gdbm_open)(cpath.as_ptr(), 0, iflags, mode, std::ptr::null()) });
    if h.is_null() {
        let e = gdbm_errno(a);
        let sys = a.gdbm_check_syserr.map(|f| unsafe { f(e) } != 0).unwrap_or(false);
        let errno = crate::modules::posix::path::errno();
        if sys && errno != 0 {
            return Err(set_os_error(Kind::Gnu, errno, Some(&shown)));
        }
        return Err(raise_error(Kind::Gnu, &gdbm_error_msg(a, std::ptr::null_mut())));
    }
    new_db(&raw mut RPY_GDBM_TYPE, Kind::Gnu, h, iflags & 7 == GDBM_READER)
}

fn ndbm_open(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("open", args, 1, 3)?;
    let a = ndbm_api()?;
    let (shown, cpath) = path_bytes("open", args[0])?;
    let flags = match args.get(1) {
        Some(f) => {
            if !pystr::is_str(f) {
                raise!(TypeError, "open() argument 2 must be str, not {}", tname(f));
            }
            pystr::as_utf8(f)?.to_string()
        }
        None => "r".to_string(),
    };
    let mode = match args.get(2) {
        Some(m) => crate::modules::posix::path::c_int(m)?,
        None => 0o666,
    };
    let iflags = match flags.as_str() {
        "r" => libc::O_RDONLY,
        "w" => libc::O_RDWR,
        "rw" => libc::O_RDWR | libc::O_CREAT,
        "c" => libc::O_RDWR | libc::O_CREAT,
        "n" => libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
        _ => return Err(raise_error(Kind::Ndbm, "arg 2 to open should be 'r', 'w', 'c', or 'n'")),
    };
    unsafe { *libc::__errno_location() = 0 };
    let h = crate::threads::blocking(|| unsafe { (a.dbm_open)(cpath.as_ptr(), iflags, mode) });
    if h.is_null() {
        let mut errno = crate::modules::posix::path::errno();
        if errno == 0 {
            errno = libc::ENOENT;
        }
        return Err(set_os_error(Kind::Ndbm, errno, Some(&shown)));
    }
    new_db(&raw mut RPY_NDBM_TYPE, Kind::Ndbm, h, iflags == libc::O_RDONLY)
}

pub fn init_gdbm() -> PyResult {
    let a = gdbm_api()?;
    init_db_type(&raw mut RPY_GDBM_TYPE, c"_gdbm.gdbm", true)?;
    let (m, d) = fc::new_module(
        "_gdbm",
        mdefs![("open", FAST, gdbm_open, "open($module, filename, flags='r', mode=0o666, /)\n--\n\nOpen a dbm database and return a dbm object.\n\nThe flags argument starts with 'r' (read-only, the default), 'w' (read-write),\n'c' (read-write, create if missing) or 'n' (always create a new, empty\ndatabase); it may be followed by 'f' (fast mode), 's' (synchronized mode) and\n'u' (no locking).  mode is the Unix permission used when a file is created.")],
        Some("Interface to the GNU dbm library."),
    )?;
    if GDBM_ERROR.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("_gdbm", "error", exceptions::OSError())?;
        GDBM_ERROR.store(e.into_raw(), Ordering::Release);
    }
    dict::set_item_str(&d, "error", unsafe { &*GDBM_ERROR.load(Ordering::Acquire) })?;
    dict::set_item_str(&d, "open_flags", &pystr::new_str("rwcnfsu"))?;
    if !a.gdbm_version_number.is_null() {
        let v = unsafe { *a.gdbm_version_number };
        let t = tuple::new_tuple(v.iter().map(|x| int::new_int(*x as i64)).collect());
        dict::set_item_str(&d, "_GDBM_VERSION", &t)?;
    }
    Ok(m)
}

pub fn init_ndbm() -> PyResult {
    ndbm_api()?;
    init_db_type(&raw mut RPY_NDBM_TYPE, c"_dbm.dbm", false)?;
    let (m, d) = fc::new_module(
        "_dbm",
        mdefs![("open", FAST, ndbm_open, "open($module, filename, flags='r', mode=0o666, /)\n--\n\nReturn a database object.\n\n  filename\n    The filename to open.\n  flags\n    How to open the file.  \"r\" for reading, \"w\" for writing, etc.\n  mode\n    If creating a new file, the mode bits for the new file\n    (e.g. os.O_RDWR).")],
        None,
    )?;
    if NDBM_ERROR.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("_dbm", "error", exceptions::OSError())?;
        NDBM_ERROR.store(e.into_raw(), Ordering::Release);
    }
    dict::set_item_str(&d, "error", unsafe { &*NDBM_ERROR.load(Ordering::Acquire) })?;
    dict::set_item_str(&d, "library", &pystr::new_str("GNU gdbm"))?;
    Ok(m)
}
