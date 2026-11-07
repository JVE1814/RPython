//! libsqlite3 entry points (loaded with dlopen). Declarations written from the
//! documented SQLite C interface.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_double, c_int, c_uint, c_void};

pub type Db = c_void;
pub type Stmt = c_void;
pub type Ctx = c_void;
pub type Value = c_void;
pub type Blob = c_void;
pub type Backup = c_void;

pub type XFunc = unsafe extern "C" fn(*mut Ctx, c_int, *mut *mut Value);
pub type XFinal = unsafe extern "C" fn(*mut Ctx);
pub type XDestroy = unsafe extern "C" fn(*mut c_void);
pub type XCompare = unsafe extern "C" fn(*mut c_void, c_int, *const c_void, c_int, *const c_void) -> c_int;
pub type XAuth = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, *const c_char, *const c_char, *const c_char) -> c_int;
pub type XProgress = unsafe extern "C" fn(*mut c_void) -> c_int;
pub type XTrace = unsafe extern "C" fn(c_uint, *mut c_void, *mut c_void, *mut c_void) -> c_int;

crate::dyn_lib!(LibSqlite, ["libsqlite3.so.0", "libsqlite3.so"], {
    fn sqlite3_libversion() -> *const c_char;
    fn sqlite3_libversion_number() -> c_int;
    fn sqlite3_threadsafe() -> c_int;
    fn sqlite3_complete(sql: *const c_char) -> c_int;
    fn sqlite3_keyword_count() -> c_int;
    fn sqlite3_keyword_name(i: c_int, name: *mut *const c_char, len: *mut c_int) -> c_int;
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut Db, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close_v2(db: *mut Db) -> c_int;
    fn sqlite3_errmsg(db: *mut Db) -> *const c_char;
    fn sqlite3_errcode(db: *mut Db) -> c_int;
    fn sqlite3_extended_errcode(db: *mut Db) -> c_int;
    fn sqlite3_errstr(code: c_int) -> *const c_char;
    fn sqlite3_extended_result_codes(db: *mut Db, onoff: c_int) -> c_int;
    fn sqlite3_busy_timeout(db: *mut Db, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(db: *mut Db, sql: *const c_char, n: c_int, stmt: *mut *mut Stmt, tail: *mut *const c_char) -> c_int;
    fn sqlite3_step(stmt: *mut Stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut Stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut Stmt) -> c_int;
    fn sqlite3_clear_bindings(stmt: *mut Stmt) -> c_int;
    fn sqlite3_stmt_readonly(stmt: *mut Stmt) -> c_int;
    fn sqlite3_stmt_busy(stmt: *mut Stmt) -> c_int;
    fn sqlite3_bind_parameter_count(stmt: *mut Stmt) -> c_int;
    fn sqlite3_bind_parameter_name(stmt: *mut Stmt, i: c_int) -> *const c_char;
    fn sqlite3_bind_null(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut Stmt, i: c_int, v: i64) -> c_int;
    fn sqlite3_bind_double(stmt: *mut Stmt, i: c_int, v: c_double) -> c_int;
    fn sqlite3_bind_text(stmt: *mut Stmt, i: c_int, v: *const c_char, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_blob(stmt: *mut Stmt, i: c_int, v: *const c_void, n: c_int, d: isize) -> c_int;
    fn sqlite3_column_count(stmt: *mut Stmt) -> c_int;
    fn sqlite3_data_count(stmt: *mut Stmt) -> c_int;
    fn sqlite3_column_type(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut Stmt, i: c_int) -> i64;
    fn sqlite3_column_double(stmt: *mut Stmt, i: c_int) -> c_double;
    fn sqlite3_column_text(stmt: *mut Stmt, i: c_int) -> *const u8;
    fn sqlite3_column_blob(stmt: *mut Stmt, i: c_int) -> *const c_void;
    fn sqlite3_column_bytes(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_column_name(stmt: *mut Stmt, i: c_int) -> *const c_char;
    fn sqlite3_column_decltype(stmt: *mut Stmt, i: c_int) -> *const c_char;
    fn sqlite3_changes(db: *mut Db) -> c_int;
    fn sqlite3_total_changes(db: *mut Db) -> c_int;
    fn sqlite3_last_insert_rowid(db: *mut Db) -> i64;
    fn sqlite3_get_autocommit(db: *mut Db) -> c_int;
    fn sqlite3_interrupt(db: *mut Db);
    fn sqlite3_create_function_v2(db: *mut Db, name: *const c_char, narg: c_int, enc: c_int, app: *mut c_void,
        xfunc: Option<XFunc>, xstep: Option<XFunc>, xfinal: Option<XFinal>, xdestroy: Option<XDestroy>) -> c_int;
    fn sqlite3_create_window_function(db: *mut Db, name: *const c_char, narg: c_int, enc: c_int, app: *mut c_void,
        xstep: Option<XFunc>, xfinal: Option<XFinal>, xvalue: Option<XFinal>, xinverse: Option<XFunc>, xdestroy: Option<XDestroy>) -> c_int;
    fn sqlite3_create_collation_v2(db: *mut Db, name: *const c_char, enc: c_int, app: *mut c_void, cmp: Option<XCompare>, d: Option<XDestroy>) -> c_int;
    fn sqlite3_set_authorizer(db: *mut Db, cb: Option<XAuth>, app: *mut c_void) -> c_int;
    fn sqlite3_progress_handler(db: *mut Db, n: c_int, cb: Option<XProgress>, app: *mut c_void);
    fn sqlite3_trace_v2(db: *mut Db, mask: c_uint, cb: Option<XTrace>, app: *mut c_void) -> c_int;
    fn sqlite3_expanded_sql(stmt: *mut Stmt) -> *mut c_char;
    fn sqlite3_sql(stmt: *mut Stmt) -> *const c_char;
    fn sqlite3_free(p: *mut c_void);
    fn sqlite3_malloc64(n: u64) -> *mut c_void;
    fn sqlite3_user_data(ctx: *mut Ctx) -> *mut c_void;
    fn sqlite3_aggregate_context(ctx: *mut Ctx, n: c_int) -> *mut c_void;
    fn sqlite3_value_type(v: *mut Value) -> c_int;
    fn sqlite3_value_int64(v: *mut Value) -> i64;
    fn sqlite3_value_double(v: *mut Value) -> c_double;
    fn sqlite3_value_text(v: *mut Value) -> *const u8;
    fn sqlite3_value_blob(v: *mut Value) -> *const c_void;
    fn sqlite3_value_bytes(v: *mut Value) -> c_int;
    fn sqlite3_result_null(ctx: *mut Ctx);
    fn sqlite3_result_int64(ctx: *mut Ctx, v: i64);
    fn sqlite3_result_double(ctx: *mut Ctx, v: c_double);
    fn sqlite3_result_text(ctx: *mut Ctx, v: *const c_char, n: c_int, d: isize);
    fn sqlite3_result_blob(ctx: *mut Ctx, v: *const c_void, n: c_int, d: isize);
    fn sqlite3_result_error(ctx: *mut Ctx, msg: *const c_char, n: c_int);
    fn sqlite3_result_error_nomem(ctx: *mut Ctx);
    fn sqlite3_result_error_toobig(ctx: *mut Ctx);
    fn sqlite3_backup_init(dst: *mut Db, dname: *const c_char, src: *mut Db, sname: *const c_char) -> *mut Backup;
    fn sqlite3_backup_step(b: *mut Backup, n: c_int) -> c_int;
    fn sqlite3_backup_finish(b: *mut Backup) -> c_int;
    fn sqlite3_backup_remaining(b: *mut Backup) -> c_int;
    fn sqlite3_backup_pagecount(b: *mut Backup) -> c_int;
    fn sqlite3_sleep(ms: c_int) -> c_int;
    fn sqlite3_serialize(db: *mut Db, schema: *const c_char, size: *mut i64, flags: c_uint) -> *mut u8;
    fn sqlite3_deserialize(db: *mut Db, schema: *const c_char, data: *mut u8, sz: i64, buf: i64, flags: c_uint) -> c_int;
    fn sqlite3_limit(db: *mut Db, id: c_int, v: c_int) -> c_int;
    fn sqlite3_enable_load_extension(db: *mut Db, onoff: c_int) -> c_int;
    fn sqlite3_load_extension(db: *mut Db, file: *const c_char, proc_: *const c_char, err: *mut *mut c_char) -> c_int;
    fn sqlite3_blob_open(db: *mut Db, dbname: *const c_char, table: *const c_char, column: *const c_char, row: i64, flags: c_int, blob: *mut *mut Blob) -> c_int;
    fn sqlite3_blob_close(b: *mut Blob) -> c_int;
    fn sqlite3_blob_bytes(b: *mut Blob) -> c_int;
    fn sqlite3_blob_read(b: *mut Blob, buf: *mut c_void, n: c_int, off: c_int) -> c_int;
    fn sqlite3_blob_write(b: *mut Blob, buf: *const c_void, n: c_int, off: c_int) -> c_int;
    fn sqlite3_db_filename(db: *mut Db, name: *const c_char) -> *const c_char;
    fn sqlite3_txn_state(db: *mut Db, schema: *const c_char) -> c_int;
});

/// `sqlite3_db_config` is variadic; it is resolved separately.
pub type DbConfigFn = unsafe extern "C" fn(*mut Db, c_int, ...) -> c_int;

pub fn db_config_fn() -> Option<DbConfigFn> {
    let l = LibSqlite::get().ok()?;
    let p = l.lib.sym("sqlite3_db_config");
    if p.is_null() { None } else { Some(unsafe { std::mem::transmute::<*mut c_void, DbConfigFn>(p) }) }
}

pub fn lib() -> &'static LibSqlite {
    LibSqlite::get().expect("libsqlite3 not available")
}

/// Call a loaded function (panics only if the symbol is missing from libsqlite3).
#[macro_export]
macro_rules! sq {
    ($f:ident ( $($a:expr),* $(,)? )) => {
        unsafe { ($crate::sqlite::ffi::lib().$f.expect(stringify!($f)))($($a),*) }
    };
}

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_INTERNAL: c_int = 2;
pub const SQLITE_PERM: c_int = 3;
pub const SQLITE_ABORT: c_int = 4;
pub const SQLITE_BUSY: c_int = 5;
pub const SQLITE_LOCKED: c_int = 6;
pub const SQLITE_NOMEM: c_int = 7;
pub const SQLITE_READONLY: c_int = 8;
pub const SQLITE_INTERRUPT: c_int = 9;
pub const SQLITE_IOERR: c_int = 10;
pub const SQLITE_CORRUPT: c_int = 11;
pub const SQLITE_NOTFOUND: c_int = 12;
pub const SQLITE_FULL: c_int = 13;
pub const SQLITE_CANTOPEN: c_int = 14;
pub const SQLITE_PROTOCOL: c_int = 15;
pub const SQLITE_EMPTY: c_int = 16;
pub const SQLITE_SCHEMA: c_int = 17;
pub const SQLITE_TOOBIG: c_int = 18;
pub const SQLITE_CONSTRAINT: c_int = 19;
pub const SQLITE_MISMATCH: c_int = 20;
pub const SQLITE_MISUSE: c_int = 21;
pub const SQLITE_RANGE: c_int = 25;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;

pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_BLOB: c_int = 4;
pub const SQLITE_NULL: c_int = 5;

pub const SQLITE_UTF8: c_int = 1;
pub const SQLITE_DETERMINISTIC: c_int = 0x800;
pub const SQLITE_TRANSIENT: isize = -1;

pub const SQLITE_OPEN_READONLY: c_int = 0x1;
pub const SQLITE_OPEN_READWRITE: c_int = 0x2;
pub const SQLITE_OPEN_CREATE: c_int = 0x4;
pub const SQLITE_OPEN_URI: c_int = 0x40;
pub const SQLITE_OPEN_NOMUTEX: c_int = 0x8000;
pub const SQLITE_OPEN_FULLMUTEX: c_int = 0x10000;

pub const SQLITE_TRACE_STMT: c_uint = 1;
pub const SQLITE_DESERIALIZE_FREEONCLOSE: c_uint = 1;
pub const SQLITE_DESERIALIZE_RESIZEABLE: c_uint = 2;
pub const SQLITE_SERIALIZE_NOCOPY: c_uint = 1;
