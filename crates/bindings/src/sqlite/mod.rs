//! `_sqlite3`: the DB-API 2.0 driver, binding the system libsqlite3.

pub mod blob;
pub mod callbacks;
pub mod connection;
pub mod cursor;
pub mod ffi;

use crate::capi::{self, Ob};
use crate::cs;
use crate::sq;
use crate::ctypes::{call, cstring, from_c, getattr, getattr_opt, is_none, is_str, new_int, new_str, none, str_of, tname};
use ffi::*;
use rpy_runtime::errors::*;
use rpy_runtime::ffi::*;
use rpy_runtime::object::*;
use rpy_runtime::types::{dict, tuple};
use rpy_runtime::{raise, err};
use std::ffi::{CStr, c_int};
use std::sync::atomic::{AtomicBool, Ordering};

// ------------------------------------------------------------------ module state

pub struct State {
    pub module: PyRef,
    pub warning: PyRef,
    pub error: PyRef,
    pub interface_error: PyRef,
    pub database_error: PyRef,
    pub data_error: PyRef,
    pub operational_error: PyRef,
    pub integrity_error: PyRef,
    pub internal_error: PyRef,
    pub programming_error: PyRef,
    pub not_supported_error: PyRef,
    pub adapters: PyRef,
    pub converters: PyRef,
    pub prepare_protocol: PyRef,
}
unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: std::sync::OnceLock<State> = std::sync::OnceLock::new();

pub fn st() -> &'static State {
    STATE.get().expect("_sqlite3 not initialised")
}

pub static CALLBACK_TRACEBACKS: AtomicBool = AtomicBool::new(false);
/// Set when an adapter is registered for int/float/str/bytes/bytearray.
pub static BASE_TYPE_ADAPTED: AtomicBool = AtomicBool::new(false);

pub const LEGACY_TRANSACTION_CONTROL: i64 = -1;
pub const PARSE_DECLTYPES: i32 = 1;
pub const PARSE_COLNAMES: i32 = 2;

pub fn raise_obj(exc: &PyObject, msg: &str) -> PyErr {
    set_err_type(exc, msg)
}

fn set_err_type(exc: &PyObject, msg: &str) -> PyErr {
    rpy_runtime::errors::new_err(exc.as_ptr() as *mut PyTypeObject, msg)
}

pub fn programming_error(msg: &str) -> PyErr {
    set_err_type(&st().programming_error, msg)
}
pub fn interface_error(msg: &str) -> PyErr {
    set_err_type(&st().interface_error, msg)
}
pub fn operational_error(msg: &str) -> PyErr {
    set_err_type(&st().operational_error, msg)
}
pub fn data_error(msg: &str) -> PyErr {
    set_err_type(&st().data_error, msg)
}

pub fn cstr_to_string(p: *const std::ffi::c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() } }
}

/// Name of a (possibly extended) SQLite result code.
pub fn error_name(code: c_int) -> Option<&'static str> {
    for &(n, v) in RESULT_CODES {
        if v as c_int == code {
            return Some(n);
        }
    }
    None
}

/// Raise the exception matching the error state of `db`.
pub fn set_db_error(db: *mut Db) -> PyErr {
    let primary = sq!(sqlite3_errcode(db));
    let extended = sq!(sqlite3_extended_errcode(db));
    let msg = cstr_to_string(sq!(sqlite3_errmsg(db)));
    raise_code(primary, extended, &msg)
}

pub fn raise_code(primary: c_int, extended: c_int, msg: &str) -> PyErr {
    if primary == SQLITE_OK {
        return PyErr;
    }
    let s = st();
    let exc = match primary & 0xff {
        SQLITE_INTERNAL | SQLITE_NOTFOUND => &s.internal_error,
        SQLITE_NOMEM => {
            unsafe { capi::PyErr_NoMemory() };
            return PyErr;
        }
        SQLITE_ERROR | SQLITE_PERM | SQLITE_ABORT | SQLITE_BUSY | SQLITE_LOCKED | SQLITE_READONLY | SQLITE_INTERRUPT | SQLITE_IOERR
        | SQLITE_FULL | SQLITE_CANTOPEN | SQLITE_PROTOCOL | SQLITE_EMPTY | SQLITE_SCHEMA => &s.operational_error,
        SQLITE_CORRUPT => &s.database_error,
        SQLITE_TOOBIG => &s.data_error,
        SQLITE_CONSTRAINT | SQLITE_MISMATCH => &s.integrity_error,
        SQLITE_MISUSE | SQLITE_RANGE => &s.interface_error,
        _ => &s.database_error,
    };
    let e = match rpy_runtime::types::exceptions::make_exception(exc.as_ptr() as *mut PyTypeObject, &[new_str(msg)]) {
        Ok(e) => e,
        Err(_) => return PyErr,
    };
    let _ = rpy_runtime::abstract_::setattr(&e, &new_str("sqlite_errorcode"), &new_int(extended as i64));
    let name = error_name(extended).or_else(|| error_name(primary)).unwrap_or("unknown");
    let _ = rpy_runtime::abstract_::setattr(&e, &new_str("sqlite_errorname"), &new_str(name));
    rpy_runtime::errors::set_exc(e);
    PyErr
}

/// Report an exception raised inside a SQLite callback.
pub fn report_callback_error(callable: Option<&PyObject>) {
    if !err_occurred() {
        return;
    }
    if CALLBACK_TRACEBACKS.load(Ordering::Relaxed) {
        match callable {
            Some(c) => unsafe { capi::PyErr_FormatUnraisable(cs!("Exception ignored on sqlite3 callback %R"), c.as_ptr()) },
            None => unsafe { capi::PyErr_FormatUnraisable(cs!("Exception ignored on sqlite3 callback")) },
        }
    } else {
        rpy_runtime::errors::clear_exc();
    }
}

// ------------------------------------------------------------------ adaptation

pub fn is_exact(o: &PyObject, t: *mut PyTypeObject) -> bool {
    o.type_ptr() == t
}

/// `adapt(obj, proto=PrepareProtocol, alt=<missing>)`
pub fn adapt(obj: &PyObject, proto: &PyObject, alt: Option<&PyObject>) -> PyResult {
    let s = st();
    let key = tuple::new_tuple(vec![rpy_runtime::types::typeobject::type_ref(obj.type_ptr()), proto.to_ref()]);
    if let Some(adapter) = dict::get_item(&s.adapters, &key)? {
        return call(&adapter, &[obj]);
    }
    // proto.__adapt__(obj)
    if let Some(m) = getattr_opt(proto, "__adapt__")? {
        match call(&m, &[obj]) {
            Ok(r) if !is_none(&r) => return Ok(r),
            Ok(_) => {}
            Err(_) => {
                if !rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::TypeError()) {
                    return Err(PyErr);
                }
                rpy_runtime::errors::clear_exc();
            }
        }
    }
    // obj.__conform__(proto)
    if let Some(m) = getattr_opt(obj, "__conform__")? {
        match call(&m, &[proto]) {
            Ok(r) if !is_none(&r) => return Ok(r),
            Ok(_) => {}
            Err(_) => {
                if !rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::TypeError()) {
                    return Err(PyErr);
                }
                rpy_runtime::errors::clear_exc();
            }
        }
    }
    match alt {
        Some(a) => Ok(a.to_ref()),
        None => Err(programming_error("can't adapt")),
    }
}

fn m_adapt(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 3 {
        raise!(TypeError, "adapt expected 1 to 3 arguments, got {}", args.len());
    }
    let proto = if args.len() >= 2 { args[1].to_ref() } else { st().prepare_protocol.clone() };
    adapt(args[0], &proto, args.get(2).copied())
}

fn m_register_adapter(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "register_adapter expected 2 arguments, got {}", args.len());
    }
    let tp = args[0];
    unsafe {
        let t = tp.as_ptr() as *mut PyTypeObject;
        if t == rpy_runtime::types::int::int_type()
            || t == rpy_runtime::types::float::float_type()
            || t == rpy_runtime::types::str::str_type()
            || t == rpy_runtime::types::bytes::bytes_type()
            || t == &raw mut rpy_runtime::types::bytearray::PyByteArray_Type
        {
            BASE_TYPE_ADAPTED.store(true, Ordering::Relaxed);
        }
    }
    let key = tuple::new_tuple(vec![tp.to_ref(), st().prepare_protocol.clone()]);
    dict::set_item(&st().adapters, &key, args[1])?;
    Ok(none())
}

fn m_register_converter(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "register_converter expected 2 arguments, got {}", args.len());
    }
    if !is_str(args[0]) {
        raise!(TypeError, "register_converter() argument 1 must be str, not {}", tname(args[0]));
    }
    let upper = rpy_runtime::abstract_::call_method_str(args[0], "upper", &[])?;
    dict::set_item(&st().converters, &upper, args[1])?;
    Ok(none())
}

fn m_complete_statement(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("complete_statement", &["statement"], 0, 1);
    let [s] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let s = s.unwrap();
    if !is_str(s) {
        raise!(TypeError, "complete_statement() argument 'statement' must be str, not {}", tname(s));
    }
    let c = utf8_cstring(s)?;
    Ok(rpy_runtime::types::bool_::new_bool(sq!(sqlite3_complete(c.as_ptr())) != 0))
}

fn m_enable_callback_tracebacks(_m: &PyObject, v: &PyObject) -> PyResult {
    let b = rpy_runtime::abstract_::is_true(v)?;
    CALLBACK_TRACEBACKS.store(b, Ordering::Relaxed);
    Ok(none())
}

/// UTF-8 encoding of a str (UnicodeEncodeError for surrogates).
pub fn utf8_bytes(s: &PyObject) -> PyResult<Vec<u8>> {
    let mut n: isize = 0;
    let p = unsafe { capi::PyUnicode_AsUTF8AndSize(s.as_ptr(), &mut n) };
    if p.is_null() {
        return Err(PyErr);
    }
    Ok(unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) }.to_vec())
}

pub fn utf8_cstring(s: &PyObject) -> PyResult<std::ffi::CString> {
    let b = utf8_bytes(s)?;
    match std::ffi::CString::new(b) {
        Ok(c) => Ok(c),
        Err(_) => Err(programming_error("the query contains a null character")),
    }
}

// ------------------------------------------------------------------ PrepareProtocol

static mut PREPARE_PROTOCOL_TYPE: PyTypeObject = PyTypeObject::ZERO;

unsafe extern "C" fn pp_dealloc(o: *mut PyObject) {
    unsafe { rpy_runtime::types::typeobject::generic_free_and_type_decref(o, crate::ctypes::ob_type(o)) }
}

unsafe extern "C" fn pp_init(_o: *mut PyObject, _a: *mut PyObject, _k: *mut PyObject) -> c_int {
    0
}

// ------------------------------------------------------------------ constants

pub static RESULT_CODES: &[(&str, i64)] = &[
    ("SQLITE_OK", 0), ("SQLITE_ERROR", 1), ("SQLITE_INTERNAL", 2), ("SQLITE_PERM", 3), ("SQLITE_ABORT", 4),
    ("SQLITE_BUSY", 5), ("SQLITE_LOCKED", 6), ("SQLITE_NOMEM", 7), ("SQLITE_READONLY", 8), ("SQLITE_INTERRUPT", 9),
    ("SQLITE_IOERR", 10), ("SQLITE_CORRUPT", 11), ("SQLITE_NOTFOUND", 12), ("SQLITE_FULL", 13), ("SQLITE_CANTOPEN", 14),
    ("SQLITE_PROTOCOL", 15), ("SQLITE_EMPTY", 16), ("SQLITE_SCHEMA", 17), ("SQLITE_TOOBIG", 18), ("SQLITE_CONSTRAINT", 19),
    ("SQLITE_MISMATCH", 20), ("SQLITE_MISUSE", 21), ("SQLITE_NOLFS", 22), ("SQLITE_AUTH", 23), ("SQLITE_FORMAT", 24),
    ("SQLITE_RANGE", 25), ("SQLITE_NOTADB", 26), ("SQLITE_NOTICE", 27), ("SQLITE_WARNING", 28), ("SQLITE_ROW", 100),
    ("SQLITE_DONE", 101),
    ("SQLITE_ERROR_MISSING_COLLSEQ", 1 | 1 << 8), ("SQLITE_ERROR_RETRY", 1 | 2 << 8), ("SQLITE_ERROR_SNAPSHOT", 1 | 3 << 8),
    ("SQLITE_IOERR_READ", 10 | 1 << 8), ("SQLITE_IOERR_SHORT_READ", 10 | 2 << 8), ("SQLITE_IOERR_WRITE", 10 | 3 << 8),
    ("SQLITE_IOERR_FSYNC", 10 | 4 << 8), ("SQLITE_IOERR_DIR_FSYNC", 10 | 5 << 8), ("SQLITE_IOERR_TRUNCATE", 10 | 6 << 8),
    ("SQLITE_IOERR_FSTAT", 10 | 7 << 8), ("SQLITE_IOERR_UNLOCK", 10 | 8 << 8), ("SQLITE_IOERR_RDLOCK", 10 | 9 << 8),
    ("SQLITE_IOERR_DELETE", 10 | 10 << 8), ("SQLITE_IOERR_BLOCKED", 10 | 11 << 8), ("SQLITE_IOERR_NOMEM", 10 | 12 << 8),
    ("SQLITE_IOERR_ACCESS", 10 | 13 << 8), ("SQLITE_IOERR_CHECKRESERVEDLOCK", 10 | 14 << 8), ("SQLITE_IOERR_LOCK", 10 | 15 << 8),
    ("SQLITE_IOERR_CLOSE", 10 | 16 << 8), ("SQLITE_IOERR_DIR_CLOSE", 10 | 17 << 8), ("SQLITE_IOERR_SHMOPEN", 10 | 18 << 8),
    ("SQLITE_IOERR_SHMSIZE", 10 | 19 << 8), ("SQLITE_IOERR_SHMLOCK", 10 | 20 << 8), ("SQLITE_IOERR_SHMMAP", 10 | 21 << 8),
    ("SQLITE_IOERR_SEEK", 10 | 22 << 8), ("SQLITE_IOERR_DELETE_NOENT", 10 | 23 << 8), ("SQLITE_IOERR_MMAP", 10 | 24 << 8),
    ("SQLITE_IOERR_GETTEMPPATH", 10 | 25 << 8), ("SQLITE_IOERR_CONVPATH", 10 | 26 << 8), ("SQLITE_IOERR_VNODE", 10 | 27 << 8),
    ("SQLITE_IOERR_AUTH", 10 | 28 << 8), ("SQLITE_IOERR_BEGIN_ATOMIC", 10 | 29 << 8), ("SQLITE_IOERR_COMMIT_ATOMIC", 10 | 30 << 8),
    ("SQLITE_IOERR_ROLLBACK_ATOMIC", 10 | 31 << 8), ("SQLITE_IOERR_DATA", 10 | 32 << 8), ("SQLITE_IOERR_CORRUPTFS", 10 | 33 << 8),
    ("SQLITE_LOCKED_SHAREDCACHE", 6 | 1 << 8), ("SQLITE_LOCKED_VTAB", 6 | 2 << 8),
    ("SQLITE_BUSY_RECOVERY", 5 | 1 << 8), ("SQLITE_BUSY_SNAPSHOT", 5 | 2 << 8), ("SQLITE_BUSY_TIMEOUT", 5 | 3 << 8),
    ("SQLITE_CANTOPEN_NOTEMPDIR", 14 | 1 << 8), ("SQLITE_CANTOPEN_ISDIR", 14 | 2 << 8), ("SQLITE_CANTOPEN_FULLPATH", 14 | 3 << 8),
    ("SQLITE_CANTOPEN_CONVPATH", 14 | 4 << 8), ("SQLITE_CANTOPEN_DIRTYWAL", 14 | 5 << 8), ("SQLITE_CANTOPEN_SYMLINK", 14 | 6 << 8),
    ("SQLITE_CORRUPT_VTAB", 11 | 1 << 8), ("SQLITE_CORRUPT_SEQUENCE", 11 | 2 << 8), ("SQLITE_CORRUPT_INDEX", 11 | 3 << 8),
    ("SQLITE_READONLY_RECOVERY", 8 | 1 << 8), ("SQLITE_READONLY_CANTLOCK", 8 | 2 << 8), ("SQLITE_READONLY_ROLLBACK", 8 | 3 << 8),
    ("SQLITE_READONLY_DBMOVED", 8 | 4 << 8), ("SQLITE_READONLY_CANTINIT", 8 | 5 << 8), ("SQLITE_READONLY_DIRECTORY", 8 | 6 << 8),
    ("SQLITE_ABORT_ROLLBACK", 4 | 2 << 8),
    ("SQLITE_CONSTRAINT_CHECK", 19 | 1 << 8), ("SQLITE_CONSTRAINT_COMMITHOOK", 19 | 2 << 8), ("SQLITE_CONSTRAINT_FOREIGNKEY", 19 | 3 << 8),
    ("SQLITE_CONSTRAINT_FUNCTION", 19 | 4 << 8), ("SQLITE_CONSTRAINT_NOTNULL", 19 | 5 << 8), ("SQLITE_CONSTRAINT_PRIMARYKEY", 19 | 6 << 8),
    ("SQLITE_CONSTRAINT_TRIGGER", 19 | 7 << 8), ("SQLITE_CONSTRAINT_UNIQUE", 19 | 8 << 8), ("SQLITE_CONSTRAINT_VTAB", 19 | 9 << 8),
    ("SQLITE_CONSTRAINT_ROWID", 19 | 10 << 8), ("SQLITE_CONSTRAINT_PINNED", 19 | 11 << 8), ("SQLITE_CONSTRAINT_DATATYPE", 19 | 12 << 8),
    ("SQLITE_NOTICE_RECOVER_WAL", 27 | 1 << 8), ("SQLITE_NOTICE_RECOVER_ROLLBACK", 27 | 2 << 8),
    ("SQLITE_WARNING_AUTOINDEX", 28 | 1 << 8),
    ("SQLITE_AUTH_USER", 23 | 1 << 8),
    ("SQLITE_OK_LOAD_PERMANENTLY", 1 << 8), ("SQLITE_OK_SYMLINK", 2 << 8),
];

static OTHER_CONSTANTS: &[(&str, i64)] = &[
    // authorizer return values and action codes
    ("SQLITE_DENY", 1), ("SQLITE_IGNORE", 2),
    ("SQLITE_CREATE_INDEX", 1), ("SQLITE_CREATE_TABLE", 2), ("SQLITE_CREATE_TEMP_INDEX", 3), ("SQLITE_CREATE_TEMP_TABLE", 4),
    ("SQLITE_CREATE_TEMP_TRIGGER", 5), ("SQLITE_CREATE_TEMP_VIEW", 6), ("SQLITE_CREATE_TRIGGER", 7), ("SQLITE_CREATE_VIEW", 8),
    ("SQLITE_DELETE", 9), ("SQLITE_DROP_INDEX", 10), ("SQLITE_DROP_TABLE", 11), ("SQLITE_DROP_TEMP_INDEX", 12),
    ("SQLITE_DROP_TEMP_TABLE", 13), ("SQLITE_DROP_TEMP_TRIGGER", 14), ("SQLITE_DROP_TEMP_VIEW", 15), ("SQLITE_DROP_TRIGGER", 16),
    ("SQLITE_DROP_VIEW", 17), ("SQLITE_INSERT", 18), ("SQLITE_PRAGMA", 19), ("SQLITE_READ", 20), ("SQLITE_SELECT", 21),
    ("SQLITE_TRANSACTION", 22), ("SQLITE_UPDATE", 23), ("SQLITE_ATTACH", 24), ("SQLITE_DETACH", 25), ("SQLITE_ALTER_TABLE", 26),
    ("SQLITE_REINDEX", 27), ("SQLITE_ANALYZE", 28), ("SQLITE_CREATE_VTABLE", 29), ("SQLITE_DROP_VTABLE", 30),
    ("SQLITE_FUNCTION", 31), ("SQLITE_SAVEPOINT", 32), ("SQLITE_RECURSIVE", 33),
    // limits
    ("SQLITE_LIMIT_LENGTH", 0), ("SQLITE_LIMIT_SQL_LENGTH", 1), ("SQLITE_LIMIT_COLUMN", 2), ("SQLITE_LIMIT_EXPR_DEPTH", 3),
    ("SQLITE_LIMIT_COMPOUND_SELECT", 4), ("SQLITE_LIMIT_VDBE_OP", 5), ("SQLITE_LIMIT_FUNCTION_ARG", 6), ("SQLITE_LIMIT_ATTACHED", 7),
    ("SQLITE_LIMIT_LIKE_PATTERN_LENGTH", 8), ("SQLITE_LIMIT_VARIABLE_NUMBER", 9), ("SQLITE_LIMIT_TRIGGER_DEPTH", 10),
    ("SQLITE_LIMIT_WORKER_THREADS", 11),
    // db config
    ("SQLITE_DBCONFIG_ENABLE_FKEY", 1002), ("SQLITE_DBCONFIG_ENABLE_TRIGGER", 1003), ("SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER", 1004),
    ("SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION", 1005), ("SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE", 1006), ("SQLITE_DBCONFIG_ENABLE_QPSG", 1007),
    ("SQLITE_DBCONFIG_TRIGGER_EQP", 1008), ("SQLITE_DBCONFIG_RESET_DATABASE", 1009), ("SQLITE_DBCONFIG_DEFENSIVE", 1010),
    ("SQLITE_DBCONFIG_WRITABLE_SCHEMA", 1011), ("SQLITE_DBCONFIG_LEGACY_ALTER_TABLE", 1012), ("SQLITE_DBCONFIG_DQS_DML", 1013),
    ("SQLITE_DBCONFIG_DQS_DDL", 1014), ("SQLITE_DBCONFIG_ENABLE_VIEW", 1015), ("SQLITE_DBCONFIG_LEGACY_FILE_FORMAT", 1016),
    ("SQLITE_DBCONFIG_TRUSTED_SCHEMA", 1017),
    ("PARSE_DECLTYPES", 1), ("PARSE_COLNAMES", 2),
    ("LEGACY_TRANSACTION_CONTROL", -1),
];

pub fn is_config_op(op: i64) -> bool {
    (1002..=1017).contains(&op)
}

// ------------------------------------------------------------------ init

fn new_exc(name: &str, base: &PyObject) -> PyResult {
    let c = cstring(name);
    from_c(unsafe { capi::PyErr_NewException(c.as_ptr(), base.as_ptr(), std::ptr::null_mut()) })
}

pub fn init() -> PyResult {
    if let Err(e) = ffi::LibSqlite::get() {
        raise!(ImportError, "libsqlite3 could not be loaded: {}", e);
    }
    let methods = rpy_runtime::mdefs![
        ("adapt", FAST, m_adapt),
        ("register_adapter", FAST, m_register_adapter),
        ("register_converter", FAST, m_register_converter),
        ("complete_statement", FASTKW, m_complete_statement),
        ("enable_callback_tracebacks", O, m_enable_callback_tracebacks),
        ("connect", FASTKW, connection::module_connect),
    ];
    let (m, d) = rpy_runtime::modules::fncommon::new_module("_sqlite3", methods, None)?;
    let exception = unsafe { PyRef::from_borrowed(rpy_runtime::types::exceptions::Exception() as *mut PyObject) };
    let warning = new_exc("sqlite3.Warning", &exception)?;
    let error = new_exc("sqlite3.Error", &exception)?;
    let interface_error = new_exc("sqlite3.InterfaceError", &error)?;
    let database_error = new_exc("sqlite3.DatabaseError", &error)?;
    let data_error = new_exc("sqlite3.DataError", &database_error)?;
    let operational_error = new_exc("sqlite3.OperationalError", &database_error)?;
    let integrity_error = new_exc("sqlite3.IntegrityError", &database_error)?;
    let internal_error = new_exc("sqlite3.InternalError", &database_error)?;
    let programming_error = new_exc("sqlite3.ProgrammingError", &database_error)?;
    let not_supported_error = new_exc("sqlite3.NotSupportedError", &database_error)?;
    for (n, v) in [
        ("Warning", &warning),
        ("Error", &error),
        ("InterfaceError", &interface_error),
        ("DatabaseError", &database_error),
        ("DataError", &data_error),
        ("OperationalError", &operational_error),
        ("IntegrityError", &integrity_error),
        ("InternalError", &internal_error),
        ("ProgrammingError", &programming_error),
        ("NotSupportedError", &not_supported_error),
    ] {
        dict::set_item_str(&d, n, v)?;
    }
    // PrepareProtocol
    unsafe {
        let t = &raw mut PREPARE_PROTOCOL_TYPE;
        rpy_runtime::modules::fncommon::init_type(t, c"sqlite3.PrepareProtocol", std::mem::size_of::<PyObject>(), Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(rpy_runtime::types::object_type::generic_new);
        (*t).tp_init = Some(pp_init);
        (*t).tp_dealloc = Some(pp_dealloc);
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    let pp = rpy_runtime::types::typeobject::type_ref(&raw mut PREPARE_PROTOCOL_TYPE);
    dict::set_item_str(&d, "PrepareProtocol", &pp)?;
    let adapters = dict::new_dict();
    let converters = dict::new_dict();
    dict::set_item_str(&d, "adapters", &adapters)?;
    dict::set_item_str(&d, "converters", &converters)?;
    let _ = STATE.set(State {
        module: m.clone(),
        warning,
        error,
        interface_error,
        database_error,
        data_error,
        operational_error,
        integrity_error,
        internal_error,
        programming_error,
        not_supported_error,
        adapters,
        converters,
        prepare_protocol: pp,
    });
    connection::init_type()?;
    cursor::init_types()?;
    blob::init_type()?;
    let ty = |t: *mut PyTypeObject| rpy_runtime::types::typeobject::type_ref(t);
    dict::set_item_str(&d, "Connection", &ty(connection::connection_type()))?;
    dict::set_item_str(&d, "Cursor", &ty(cursor::cursor_type()))?;
    dict::set_item_str(&d, "Row", &ty(cursor::row_type()))?;
    dict::set_item_str(&d, "Blob", &ty(blob::blob_type()))?;
    for &(n, v) in RESULT_CODES.iter().chain(OTHER_CONSTANTS.iter()) {
        dict::set_item_str(&d, n, &new_int(v))?;
    }
    let ver = cstr_to_string(sq!(sqlite3_libversion()));
    dict::set_item_str(&d, "sqlite_version", &new_str(&ver))?;
    let ts = match sq!(sqlite3_threadsafe()) {
        0 => 0,
        2 => 1,
        _ => 3,
    };
    dict::set_item_str(&d, "threadsafety", &new_int(ts))?;
    Ok(m)
}

pub fn _unused(_: Ob, _: &str) {}
pub fn _unused2() -> String {
    str_of(&none())
}
