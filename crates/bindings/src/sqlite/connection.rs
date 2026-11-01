//! sqlite3.Connection

use super::cursor;
use super::ffi::*;
use super::*;
use rpy_runtime::modules::fncommon::{self as fc, Trav};
use std::ffi::{CString, c_void};

pub struct Conn {
    pub db: *mut Db,
    pub initialized: bool,
    pub detect_types: i32,
    /// None: isolation_level None (no implicit transactions)
    pub isolation_level: Option<&'static str>,
    /// -1 legacy, 0 autocommit disabled, 1 autocommit enabled
    pub autocommit: i32,
    pub check_same_thread: bool,
    pub thread_ident: u64,
    pub cache: Vec<(Vec<u8>, PyRef)>,
    pub cache_size: usize,
    pub row_factory: PyRef,
    pub text_factory: PyRef,
    pub blobs: Vec<PyRef>,
    pub authorizer: Option<PyRef>,
    pub progress: Option<PyRef>,
    pub trace: Option<PyRef>,
    /// contexts replaced while possibly still in use by a running callback
    pub retired: Vec<PyRef>,
}

impl Default for Conn {
    fn default() -> Conn {
        Conn {
            db: std::ptr::null_mut(),
            initialized: false,
            detect_types: 0,
            isolation_level: Some(""),
            autocommit: -1,
            check_same_thread: true,
            thread_ident: 0,
            cache: Vec::new(),
            cache_size: 128,
            row_factory: none(),
            text_factory: rpy_runtime::types::typeobject::type_ref(rpy_runtime::types::str::str_type()),
            blobs: Vec::new(),
            authorizer: None,
            progress: None,
            trace: None,
            retired: Vec::new(),
        }
    }
}

impl Trav for Conn {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        let mut refs: Vec<&PyRef> = vec![&self.row_factory, &self.text_factory];
        for (_, s) in &self.cache {
            refs.push(s);
        }
        for o in [&self.authorizer, &self.progress, &self.trace] {
            if let Some(r) = o {
                refs.push(r);
            }
        }
        for r in refs.iter().chain(self.retired.iter().collect::<Vec<_>>().iter()) {
            let x = v(r.as_ptr());
            if x != 0 {
                return x;
            }
        }
        0
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.cache.clear();
        if !self.db.is_null() {
            let db = self.db;
            self.db = std::ptr::null_mut();
            sq!(sqlite3_close_v2(db));
        }
    }
}

pub static mut CONNECTION_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut STATEMENT_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn connection_type() -> *mut PyTypeObject {
    &raw mut CONNECTION_TYPE
}

pub fn conn(o: &PyObject) -> &'static mut Conn {
    unsafe { &mut *(fc::rs::<Conn>(o) as *mut Conn) }
}

pub fn is_connection(o: &PyObject) -> bool {
    crate::ctypes::subtype(o.type_ptr(), connection_type())
}

pub fn current_thread() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

pub fn check_thread(c: &Conn) -> PyResult<()> {
    if c.check_same_thread {
        let me = current_thread();
        if me != c.thread_ident {
            return Err(programming_error(&format!(
                "SQLite objects created in a thread can only be used in that same thread. The object was created in thread id {} and this is thread id {}.",
                c.thread_ident, me
            )));
        }
    }
    Ok(())
}

pub fn check_init(c: &Conn) -> PyResult<()> {
    if !c.initialized {
        return Err(programming_error("Base Connection.__init__ not called."));
    }
    Ok(())
}

pub fn check_open(c: &Conn) -> PyResult<()> {
    check_init(c)?;
    if c.db.is_null() {
        return Err(programming_error("Cannot operate on a closed database."));
    }
    Ok(())
}

/// Thread + state checks used by every operation.
pub fn checked(o: &PyObject) -> PyResult<&'static mut Conn> {
    let c = conn(o);
    check_init(c)?;
    check_thread(c)?;
    check_open(c)?;
    Ok(c)
}

// ------------------------------------------------------------------ statements

/// A prepared statement (sqlite3.Statement).
pub struct Statement {
    pub st: *mut Stmt,
    pub is_dml: bool,
    pub in_use: bool,
}

impl Trav for Statement {}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.st.is_null() {
            let s = self.st;
            self.st = std::ptr::null_mut();
            sq!(sqlite3_finalize(s));
        }
    }
}

pub fn statement(o: &PyObject) -> &'static mut Statement {
    unsafe { &mut *(fc::rs::<Statement>(o) as *mut Statement) }
}

/// Does `sql` contain anything but whitespace and comments?
fn has_more_sql(tail: &[u8]) -> bool {
    let mut i = 0;
    while i < tail.len() {
        let c = tail[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b';' => i += 1,
            b'-' if tail.get(i + 1) == Some(&b'-') => {
                while i < tail.len() && tail[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if tail.get(i + 1) == Some(&b'*') => {
                i += 2;
                loop {
                    if i >= tail.len() {
                        // unterminated comment at the end: ignored
                        return false;
                    }
                    if tail[i] == b'*' && tail.get(i + 1) == Some(&b'/') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            0 => return false,
            _ => return true,
        }
    }
    false
}

/// Is the statement an INSERT/UPDATE/DELETE/REPLACE (skipping comments)?
fn detect_dml(sql: &[u8]) -> bool {
    let mut i = 0;
    while i < sql.len() {
        match sql[i] {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => i += 1,
            b'-' if sql.get(i + 1) == Some(&b'-') => {
                while i < sql.len() && sql[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if sql.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < sql.len() && !(sql[i] == b'*' && sql.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => break,
        }
    }
    let rest = &sql[i.min(sql.len())..];
    let starts = |kw: &[u8]| rest.len() >= kw.len() && rest[..kw.len()].eq_ignore_ascii_case(kw);
    starts(b"insert") || starts(b"update") || starts(b"delete") || starts(b"replace")
}

pub fn prepare(c: &mut Conn, sql_obj: &PyObject) -> PyResult {
    if !is_str(sql_obj) {
        raise!(TypeError, "SQL query must be a string, not {}", tname(sql_obj));
    }
    let sql = utf8_bytes(sql_obj)?;
    if sql.contains(&0) {
        return Err(programming_error("the query contains a null character"));
    }
    let limit = sq!(sqlite3_limit(c.db, 1, -1)); // SQLITE_LIMIT_SQL_LENGTH
    if sql.len() as i64 > limit as i64 {
        return Err(data_error("query string is too large"));
    }
    let csql = CString::new(sql.clone()).unwrap();
    let mut st: *mut Stmt = std::ptr::null_mut();
    let mut tail: *const std::ffi::c_char = std::ptr::null();
    let db = c.db;
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_prepare_v2.unwrap())(db, csql.as_ptr(), (sql.len() + 1) as c_int, &mut st, &mut tail);
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_OK {
        if !st.is_null() {
            sq!(sqlite3_finalize(st));
        }
        if err_occurred() {
            return Err(PyErr);
        }
        return Err(set_db_error(db));
    }
    if !tail.is_null() {
        let off = tail as usize - csql.as_ptr() as usize;
        if has_more_sql(&sql[off.min(sql.len())..]) {
            if !st.is_null() {
                sq!(sqlite3_finalize(st));
            }
            return Err(programming_error("You can only execute one statement at a time."));
        }
    }
    let is_dml = detect_dml(&sql);
    fc::robj_new(&raw mut STATEMENT_TYPE, Statement { st, is_dml, in_use: false })
}

/// Cached statement for `sql` (LRU of `cached_statements` entries).
pub fn get_statement(c: &mut Conn, sql_obj: &PyObject) -> PyResult {
    if is_str(sql_obj) && c.cache_size > 0 {
        if let Ok(key) = utf8_bytes(sql_obj) {
            if let Some(pos) = c.cache.iter().position(|(k, _)| *k == key) {
                let e = c.cache.remove(pos);
                let r = e.1.clone();
                c.cache.push(e);
                return Ok(r);
            }
            let s = prepare(c, sql_obj)?;
            c.cache.push((key, s.clone()));
            if c.cache.len() > c.cache_size {
                c.cache.remove(0);
            }
            return Ok(s);
        }
        rpy_runtime::errors::clear_exc();
    }
    prepare(c, sql_obj)
}

/// Run a statement that returns no rows (BEGIN/COMMIT/ROLLBACK...).
pub fn exec_simple(c: &mut Conn, sql: &str) -> PyResult<()> {
    let s = prepare(c, &new_str(sql))?;
    let stmt = statement(&s);
    let db = c.db;
    let st = stmt.st;
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_step.unwrap())(st);
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_DONE && rc != SQLITE_ROW {
        if err_occurred() {
            return Err(PyErr);
        }
        return Err(set_db_error(db));
    }
    Ok(())
}

// ------------------------------------------------------------------ construction

unsafe extern "C" fn conn_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, Conn::default()))
}

const INIT_NAMES: &[&str] =
    &["database", "timeout", "detect_types", "isolation_level", "check_same_thread", "factory", "cached_statements", "uri", "autocommit"];

unsafe extern "C" fn conn_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = fc::VcArgs::from_c(args, kw);
        int_into_c(conn_init_impl(&*o, &a))
    }
}

pub fn parse_autocommit(v: &PyObject) -> PyResult<i32> {
    if v.is(rpy_runtime::types::bool_::true_obj()) {
        return Ok(1);
    }
    if v.is(rpy_runtime::types::bool_::false_obj()) {
        return Ok(0);
    }
    if is_int(v) {
        if let Some(-1) = rpy_runtime::types::int::as_i64(v) {
            return Ok(-1);
        }
    }
    raise!(ValueError, "autocommit must be True, False, or sqlite3.LEGACY_TRANSACTION_CONTROL")
}

fn is_int(o: &PyObject) -> bool {
    rpy_runtime::types::int::is_int(o)
}

pub fn parse_isolation_level(v: &PyObject) -> PyResult<Option<&'static str>> {
    if is_none(v) {
        return Ok(None);
    }
    if !is_str(v) {
        raise!(TypeError, "isolation_level must be str or None, not {}", tname(v));
    }
    let s = str_of(v);
    let up = s.to_ascii_uppercase();
    match up.as_str() {
        "" => Ok(Some("")),
        "DEFERRED" => Ok(Some("DEFERRED")),
        "IMMEDIATE" => Ok(Some("IMMEDIATE")),
        "EXCLUSIVE" => Ok(Some("EXCLUSIVE")),
        _ => raise!(ValueError, "isolation_level string must be '', 'DEFERRED', 'IMMEDIATE', or 'EXCLUSIVE'"),
    }
}

fn database_path(db: &PyObject) -> PyResult<Vec<u8>> {
    let p = rpy_runtime::modules::posix::path::fspath(db)?;
    if crate::ctypes::is_bytes(&p) {
        return Ok(rpy_runtime::types::bytes::bytes_data(&p).to_vec());
    }
    let b = from_c(unsafe { capi::PyUnicode_EncodeFSDefault(p.as_ptr()) })?;
    Ok(rpy_runtime::types::bytes::bytes_data(&b).to_vec())
}

fn conn_init_impl(o: &PyObject, a: &fc::VcArgs) -> PyResult<()> {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("Connection", INIT_NAMES, 0, 1).kwonly(1);
    let [database, timeout, detect_types, isolation_level, check_same_thread, _factory, cached_statements, uri, autocommit] = a.parse::<9>(&P)?;
    let database = database.unwrap();
    let path = database_path(&database)?;
    let timeout = match timeout {
        Some(t) => rpy_runtime::abstract_::float_value(&t)?,
        None => 5.0,
    };
    let detect_types = match detect_types {
        Some(d) => {
            let v = unsafe { capi::PyLong_AsInt(d.as_ptr()) };
            if v == -1 && err_occurred() {
                return Err(PyErr);
            }
            v
        }
        None => 0,
    };
    let isolation = match &isolation_level {
        Some(l) => parse_isolation_level(l)?,
        None => Some(""),
    };
    let check_same_thread = match check_same_thread {
        Some(c) => rpy_runtime::abstract_::is_true(&c)?,
        None => true,
    };
    let cache_size = match cached_statements {
        Some(c) => {
            let v = rpy_runtime::abstract_::as_i64(&c)?;
            if v < 0 {
                raise!(ValueError, "cached_statements must be a positive integer");
            }
            v as usize
        }
        None => 128,
    };
    let uri = match uri {
        Some(u) => rpy_runtime::abstract_::is_true(&u)?,
        None => false,
    };
    let autocommit = match autocommit {
        Some(v) => parse_autocommit(&v)?,
        None => -1,
    };
    unsafe {
        let p = std::ffi::CString::new(path.clone()).map_err(|_| err!(ValueError, "embedded null byte"))?;
        let _ = p;
        if capi::PySys_Audit(cs!("sqlite3.connect"), cs!("O"), database.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let c = conn(o);
    // re-initialisation: drop the previous database (statements keep it alive)
    if c.initialized {
        close_blobs(c);
        c.cache.clear();
        if !c.db.is_null() {
            let old = c.db;
            c.db = std::ptr::null_mut();
            sq!(sqlite3_close_v2(old));
        }
        c.initialized = false;
    }
    let cpath = match CString::new(path) {
        Ok(p) => p,
        Err(_) => raise!(ValueError, "embedded null character in path"),
    };
    let mut flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX;
    if uri {
        flags |= SQLITE_OPEN_URI;
    }
    let mut db: *mut Db = std::ptr::null_mut();
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_open_v2.unwrap())(cpath.as_ptr(), &mut db, flags, std::ptr::null());
        if rc == SQLITE_OK {
            (lib().sqlite3_busy_timeout.unwrap())(db, (timeout * 1000.0) as c_int);
        }
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if db.is_null() {
        unsafe { capi::PyErr_NoMemory() };
        return Err(PyErr);
    }
    if rc != SQLITE_OK {
        let e = set_db_error(db);
        sq!(sqlite3_close_v2(db));
        return Err(e);
    }
    c.db = db;
    c.detect_types = detect_types;
    c.isolation_level = isolation;
    c.autocommit = autocommit;
    c.check_same_thread = check_same_thread;
    c.thread_ident = current_thread();
    c.cache_size = cache_size;
    c.cache.clear();
    c.row_factory = none();
    c.text_factory = rpy_runtime::types::typeobject::type_ref(rpy_runtime::types::str::str_type());
    c.initialized = true;
    if autocommit == 0 {
        exec_simple(c, "BEGIN")?;
    }
    Ok(())
}

/// `sqlite3.connect(database, *, timeout, ..., factory=Connection, ...)`
pub fn module_connect(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = rpy_runtime::args::split(args, kw);
    if pos.len() > 1 {
        raise!(TypeError, "connect() takes at most 1 positional arguments ({} given)", pos.len());
    }
    let mut factory: Option<&PyObject> = None;
    let mut rest_vals: Vec<&PyObject> = pos.to_vec();
    let mut names: Vec<PyRef> = Vec::new();
    for (k, v) in &kws {
        if str_of(k) == "factory" {
            factory = Some(v);
        } else {
            names.push(k.to_ref());
            rest_vals.push(v);
        }
    }
    let audit_db = if pos.is_empty() { kws.iter().find(|(k, _)| str_of(k) == "database").map(|x| x.1) } else { Some(pos[0]) };
    let _ = audit_db;
    let f = match factory {
        Some(f) => f.to_ref(),
        None => rpy_runtime::types::typeobject::type_ref(connection_type()),
    };
    let kwn = if names.is_empty() { None } else { Some(tuple::new_tuple(names)) };
    let r = rpy_runtime::abstract_::call_kw(&f, &rest_vals, kwn.as_deref())?;
    Ok(r)
}

// ------------------------------------------------------------------ dealloc

pub fn close_blobs(c: &mut Conn) {
    let blobs = std::mem::take(&mut c.blobs);
    for w in blobs {
        if let Ok(Some(b)) = weak_get(&w) {
            super::blob::close_blob(&b);
        }
    }
}

pub fn weak_get(w: &PyObject) -> PyResult<Option<PyRef>> {
    let r = call(w, &[])?;
    Ok(if is_none(&r) { None } else { Some(r) })
}

unsafe extern "C" fn conn_dealloc(o: *mut PyObject) {
    unsafe {
        conn_finalize(o);
        fc::robj_dealloc::<Conn>(o);
    }
}

unsafe extern "C" fn conn_finalize(o: *mut PyObject) {
    unsafe {
        let c = conn(&*o);
        if c.initialized && !c.db.is_null() {
            let exc = rpy_runtime::errors::take_exc();
            let msg = format!("unclosed database in {}", crate::ctypes::repr(&*o).unwrap_or_default());
            let cm = cstring(&msg);
            if capi::PyErr_WarnEx(capi::exc::ResourceWarning(), cm.as_ptr(), 1) < 0 {
                capi::PyErr_WriteUnraisable(o);
            }
            if let Some(e) = exc {
                rpy_runtime::errors::set_exc(e);
            }
            c.cache.clear();
            let db = c.db;
            c.db = std::ptr::null_mut();
            sq!(sqlite3_close_v2(db));
        }
    }
}

// ------------------------------------------------------------------ methods

fn m_cursor(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("cursor", &["factory"], 0, 0);
    let [factory] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let c = checked(o)?;
    let f = match factory {
        Some(f) => f.to_ref(),
        None => rpy_runtime::types::typeobject::type_ref(cursor::cursor_type()),
    };
    let cur = call(&f, &[o])?;
    if !crate::ctypes::subtype(cur.type_ptr(), cursor::cursor_type()) {
        raise!(TypeError, "factory must return a cursor, not {}", tname(&cur));
    }
    if !is_none(&c.row_factory) {
        cursor::cur(&cur).row_factory = c.row_factory.clone();
    }
    Ok(cur)
}

fn new_cursor(o: &PyObject) -> PyResult {
    m_cursor(o, &[], None)
}

fn m_execute(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "execute expected 1 or 2 arguments, got {}", args.len());
    }
    let cur = new_cursor(o)?;
    cursor::execute_impl(&cur, args[0], args.get(1).copied(), false)?;
    Ok(cur)
}

fn m_executemany(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "executemany expected 2 arguments, got {}", args.len());
    }
    let cur = new_cursor(o)?;
    cursor::execute_impl(&cur, args[0], Some(args[1]), true)?;
    Ok(cur)
}

fn m_executescript(o: &PyObject, script: &PyObject) -> PyResult {
    let cur = new_cursor(o)?;
    cursor::executescript_impl(&cur, script)?;
    Ok(cur)
}

pub fn commit_impl(o: &PyObject) -> PyResult<()> {
    let c = checked(o)?;
    if c.autocommit == -1 {
        if sq!(sqlite3_get_autocommit(c.db)) == 0 {
            exec_simple(c, "COMMIT")?;
        }
    } else if c.autocommit == 0 {
        exec_simple(c, "COMMIT")?;
        exec_simple(c, "BEGIN")?;
    }
    Ok(())
}

pub fn rollback_impl(o: &PyObject) -> PyResult<()> {
    let c = checked(o)?;
    if c.autocommit == -1 {
        if sq!(sqlite3_get_autocommit(c.db)) == 0 {
            exec_simple(c, "ROLLBACK")?;
        }
    } else if c.autocommit == 0 {
        exec_simple(c, "ROLLBACK")?;
        exec_simple(c, "BEGIN")?;
    }
    Ok(())
}

fn m_commit(o: &PyObject) -> PyResult {
    commit_impl(o)?;
    Ok(none())
}

fn m_rollback(o: &PyObject) -> PyResult {
    rollback_impl(o)?;
    Ok(none())
}

fn m_close(o: &PyObject) -> PyResult {
    let c = conn(o);
    check_init(c)?;
    check_thread(c)?;
    close_blobs(c);
    if !c.db.is_null() {
        if c.autocommit == 0 && sq!(sqlite3_get_autocommit(c.db)) == 0 {
            exec_simple(c, "ROLLBACK")?;
        }
        c.cache.clear();
        let db = c.db;
        c.db = std::ptr::null_mut();
        let rc = sq!(sqlite3_close_v2(db));
        if rc != SQLITE_OK {
            return Err(raise_code(rc, rc, "unable to close database"));
        }
    }
    Ok(none())
}

fn m_interrupt(o: &PyObject) -> PyResult {
    let c = conn(o);
    check_open(c)?;
    sq!(sqlite3_interrupt(c.db));
    Ok(none())
}

fn m_enter(o: &PyObject) -> PyResult {
    checked(o)?;
    Ok(o.to_ref())
}

fn m_exit(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 3 {
        raise!(TypeError, "__exit__ expected 3 arguments, got {}", args.len());
    }
    let ok = is_none(args[0]) && is_none(args[1]) && is_none(args[2]);
    if ok {
        if commit_impl(o).is_err() {
            let e = rpy_runtime::errors::take_exc();
            let _ = rollback_impl(o);
            rpy_runtime::errors::clear_exc();
            if let Some(e) = e {
                rpy_runtime::errors::set_exc(e);
            }
            return Err(PyErr);
        }
    } else {
        rollback_impl(o)?;
    }
    Ok(rpy_runtime::types::bool_::new_bool(false))
}

fn m_iterdump(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("iterdump", &["filter"], 0, 0).kwonly(1);
    let [filter] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    check_init(conn(o))?;
    let dump = rpy_runtime::import::import_absolute("sqlite3.dump")?;
    let f = getattr(&dump, "_iterdump")?;
    let filt = filter.map(|x| x.to_ref()).unwrap_or_else(none);
    let kwn = tuple::new_tuple(vec![new_str("filter")]);
    rpy_runtime::abstract_::call_kw(&f, &[o, &filt], Some(&kwn))
}

fn m_getlimit(o: &PyObject, cat: &PyObject) -> PyResult {
    let c = checked(o)?;
    let category = unsafe { capi::PyLong_AsInt(cat.as_ptr()) };
    if category == -1 && err_occurred() {
        return Err(PyErr);
    }
    let r = sq!(sqlite3_limit(c.db, category, -1));
    if r < 0 {
        return Err(programming_error("'category' is out of bounds"));
    }
    Ok(new_int(r as i64))
}

fn m_setlimit(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "setlimit expected 2 arguments, got {}", args.len());
    }
    let c = checked(o)?;
    let category = unsafe { capi::PyLong_AsInt(args[0].as_ptr()) };
    if category == -1 && err_occurred() {
        return Err(PyErr);
    }
    let limit = unsafe { capi::PyLong_AsInt(args[1].as_ptr()) };
    if limit == -1 && err_occurred() {
        return Err(PyErr);
    }
    let r = sq!(sqlite3_limit(c.db, category, limit));
    if r < 0 {
        return Err(programming_error("'category' is out of bounds"));
    }
    Ok(new_int(r as i64))
}

fn m_getconfig(o: &PyObject, op: &PyObject) -> PyResult {
    let c = checked(o)?;
    let opv = rpy_runtime::abstract_::as_i64(op)?;
    if !is_config_op(opv) {
        raise!(ValueError, "unknown config 'op': {}", opv);
    }
    let f = match ffi::db_config_fn() {
        Some(f) => f,
        None => return Err(operational_error("sqlite3_db_config is not available")),
    };
    let mut cur: c_int = 0;
    let rc = unsafe { f(c.db, opv as c_int, -1 as c_int, &mut cur as *mut c_int) };
    if rc != SQLITE_OK {
        return Err(set_db_error(c.db));
    }
    Ok(rpy_runtime::types::bool_::new_bool(cur != 0))
}

fn m_setconfig(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "setconfig expected 1 or 2 arguments, got {}", args.len());
    }
    let c = checked(o)?;
    let opv = rpy_runtime::abstract_::as_i64(args[0])?;
    if !is_config_op(opv) {
        raise!(ValueError, "unknown config 'op': {}", opv);
    }
    let enable = if args.len() == 2 { rpy_runtime::abstract_::is_true(args[1])? } else { true };
    let f = match ffi::db_config_fn() {
        Some(f) => f,
        None => return Err(operational_error("sqlite3_db_config is not available")),
    };
    let mut cur: c_int = 0;
    let rc = unsafe { f(c.db, opv as c_int, enable as c_int, &mut cur as *mut c_int) };
    if rc != SQLITE_OK {
        return Err(set_db_error(c.db));
    }
    Ok(rpy_runtime::types::bool_::new_bool(cur != 0))
}

fn m_serialize(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("serialize", &["name"], 0, 0).kwonly(1);
    let [name] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let c = checked(o)?;
    let name = match name {
        Some(n) => utf8_cstring(n)?,
        None => CString::new("main").unwrap(),
    };
    let mut size: i64 = 0;
    let data = unsafe {
        let ts = capi::PyEval_SaveThread();
        let d = (lib().sqlite3_serialize.unwrap())(c.db, name.as_ptr(), &mut size, 0);
        capi::PyEval_RestoreThread(ts);
        d
    };
    if data.is_null() {
        return Err(operational_error(&format!("unable to serialize '{}'", name.to_string_lossy())));
    }
    let b = rpy_runtime::types::bytes::new_bytes(unsafe { std::slice::from_raw_parts(data, size as usize) });
    sq!(sqlite3_free(data as *mut c_void));
    Ok(b)
}

fn m_deserialize(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("deserialize", &["data", "name"], 1, 1).kwonly(1);
    let [data, name] = rpy_runtime::args::parse::<2>(&P, args, kw)?;
    let c = checked(o)?;
    let data = data.unwrap();
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(data.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let bytes = unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) }.to_vec();
    unsafe { capi::PyBuffer_Release(&mut view) };
    let name = match name {
        Some(n) => utf8_cstring(n)?,
        None => CString::new("main").unwrap(),
    };
    let size = bytes.len();
    let buf = sq!(sqlite3_malloc64(size.max(1) as u64)) as *mut u8;
    if buf.is_null() {
        unsafe { capi::PyErr_NoMemory() };
        return Err(PyErr);
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, size) };
    let rc = unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_deserialize.unwrap())(
            c.db,
            name.as_ptr(),
            buf,
            size as i64,
            size as i64,
            SQLITE_DESERIALIZE_FREEONCLOSE | SQLITE_DESERIALIZE_RESIZEABLE,
        );
        capi::PyEval_RestoreThread(ts);
        rc
    };
    if rc != SQLITE_OK {
        return Err(set_db_error(c.db));
    }
    Ok(none())
}

fn m_enable_load_extension(o: &PyObject, v: &PyObject) -> PyResult {
    let c = checked(o)?;
    let on = rpy_runtime::abstract_::is_true(v)?;
    unsafe {
        if capi::PySys_Audit(cs!("sqlite3.enable_load_extension"), cs!("OO"), o.as_ptr(), if on { rpy_runtime::types::bool_::true_obj() } else { rpy_runtime::types::bool_::false_obj() }.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let rc = sq!(sqlite3_enable_load_extension(c.db, on as c_int));
    if rc != SQLITE_OK {
        return Err(operational_error("Error enabling load extension"));
    }
    Ok(none())
}

fn m_load_extension(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("load_extension", &["name", "entrypoint"], 1, 1).kwonly(1);
    let [name, entry] = rpy_runtime::args::parse::<2>(&P, args, kw)?;
    let c = checked(o)?;
    let name = name.unwrap();
    unsafe {
        if capi::PySys_Audit(cs!("sqlite3.load_extension"), cs!("OO"), o.as_ptr(), name.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let path = CString::new(database_path(name)?).map_err(|_| err!(ValueError, "embedded null byte"))?;
    let ep = match entry {
        Some(e) if !is_none(e) => Some(utf8_cstring(e)?),
        _ => None,
    };
    let mut errmsg: *mut std::ffi::c_char = std::ptr::null_mut();
    let rc = sq!(sqlite3_load_extension(c.db, path.as_ptr(), ep.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()), &mut errmsg));
    if rc != SQLITE_OK {
        let m = cstr_to_string(errmsg);
        if !errmsg.is_null() {
            sq!(sqlite3_free(errmsg as *mut c_void));
        }
        return Err(operational_error(&m));
    }
    Ok(none())
}

fn m_call(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    if kw.map_or(false, |k| tuple::tuple_len(k) > 0) {
        raise!(TypeError, "Connection() takes no keyword arguments");
    }
    let c = checked(o)?;
    if args.len() != 1 {
        raise!(TypeError, "Connection() takes exactly 1 argument ({} given)", args.len());
    }
    prepare(c, args[0])
}

fn m_call_fast(o: &PyObject, args: &[&PyObject]) -> PyResult {
    m_call(o, args, None)
}

unsafe extern "C" fn conn_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = fc::targs(args);
        if fc::kw_len(kw) > 0 {
            let _ = err!(TypeError, "Connection() takes no keyword arguments");
            return std::ptr::null_mut();
        }
        into_c(m_call(&*o, a, None))
    }
}

// ------------------------------------------------------------------ attributes

fn g_isolation_level(o: &PyObject) -> PyResult {
    let c = conn(o);
    check_init(c)?;
    Ok(match c.isolation_level {
        Some(s) => new_str(s),
        None => none(),
    })
}

fn s_isolation_level(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let c = conn(o);
    check_init(c)?;
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let lvl = parse_isolation_level(v)?;
    if lvl.is_none() && c.autocommit == -1 && !c.db.is_null() {
        // switching to autocommit commits any pending transaction
        if sq!(sqlite3_get_autocommit(c.db)) == 0 {
            exec_simple(c, "COMMIT")?;
        }
    }
    c.isolation_level = lvl;
    Ok(())
}

fn g_autocommit(o: &PyObject) -> PyResult {
    let c = conn(o);
    check_init(c)?;
    Ok(match c.autocommit {
        1 => rpy_runtime::types::bool_::new_bool(true),
        0 => rpy_runtime::types::bool_::new_bool(false),
        _ => new_int(-1),
    })
}

fn s_autocommit(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let c = checked(o)?;
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete autocommit attribute"),
    };
    let mode = parse_autocommit(v)?;
    c.autocommit = mode;
    if mode == 1 {
        if sq!(sqlite3_get_autocommit(c.db)) == 0 {
            exec_simple(c, "COMMIT")?;
        }
    } else if mode == 0 && sq!(sqlite3_get_autocommit(c.db)) != 0 {
        exec_simple(c, "BEGIN")?;
    }
    Ok(())
}

fn g_total_changes(o: &PyObject) -> PyResult {
    let c = checked(o)?;
    Ok(new_int(sq!(sqlite3_total_changes(c.db)) as i64))
}

fn g_in_transaction(o: &PyObject) -> PyResult {
    let c = checked(o)?;
    Ok(rpy_runtime::types::bool_::new_bool(sq!(sqlite3_get_autocommit(c.db)) == 0))
}

fn g_row_factory(o: &PyObject) -> PyResult {
    Ok(conn(o).row_factory.clone())
}
fn s_row_factory(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => {
            conn(o).row_factory = v.to_ref();
            Ok(())
        }
        None => raise!(AttributeError, "cannot delete attribute"),
    }
}
fn g_text_factory(o: &PyObject) -> PyResult {
    Ok(conn(o).text_factory.clone())
}
fn s_text_factory(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => {
            conn(o).text_factory = v.to_ref();
            Ok(())
        }
        None => raise!(AttributeError, "cannot delete attribute"),
    }
}

macro_rules! exc_getter {
    ($name:ident, $field:ident) => {
        fn $name(_o: &PyObject) -> PyResult {
            Ok(st().$field.clone())
        }
    };
}
exc_getter!(g_warning, warning);
exc_getter!(g_error, error);
exc_getter!(g_interface_error, interface_error);
exc_getter!(g_database_error, database_error);
exc_getter!(g_data_error, data_error);
exc_getter!(g_operational_error, operational_error);
exc_getter!(g_integrity_error, integrity_error);
exc_getter!(g_internal_error, internal_error);
exc_getter!(g_programming_error, programming_error);
exc_getter!(g_not_supported_error, not_supported_error);

pub fn init_type() -> PyResult<()> {
    unsafe {
        let t = connection_type();
        fc::init_robj_type::<Conn>(t, c"sqlite3.Connection", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(conn_new);
        (*t).tp_init = Some(conn_init);
        (*t).tp_call = Some(conn_call);
        (*t).tp_finalize = Some(conn_finalize);
        (*t).tp_dealloc = Some(conn_dealloc);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("cursor", FASTKW, m_cursor),
            ("close", NOARGS, m_close),
            ("commit", NOARGS, m_commit),
            ("rollback", NOARGS, m_rollback),
            ("execute", FAST, m_execute),
            ("executemany", FAST, m_executemany),
            ("executescript", O, m_executescript),
            ("create_function", FASTKW, callbacks::create_function),
            ("create_aggregate", FASTKW, callbacks::create_aggregate),
            ("create_window_function", FASTKW, callbacks::create_window_function),
            ("create_collation", FASTKW, callbacks::create_collation),
            ("set_authorizer", FASTKW, callbacks::set_authorizer),
            ("set_progress_handler", FASTKW, callbacks::set_progress_handler),
            ("set_trace_callback", FASTKW, callbacks::set_trace_callback),
            ("enable_load_extension", O, m_enable_load_extension),
            ("load_extension", FASTKW, m_load_extension),
            ("interrupt", NOARGS, m_interrupt),
            ("iterdump", FASTKW, m_iterdump),
            ("backup", FASTKW, callbacks::backup),
            ("blobopen", FASTKW, super::blob::blobopen),
            ("getlimit", O, m_getlimit),
            ("setlimit", FAST, m_setlimit),
            ("getconfig", O, m_getconfig),
            ("setconfig", FAST, m_setconfig),
            ("serialize", FASTKW, m_serialize),
            ("deserialize", FASTKW, m_deserialize),
            ("__call__", FAST, m_call_fast, "__call__($self, sql, /)\n--\n\nCall self as a function."),
            ("__enter__", NOARGS, m_enter),
            ("__exit__", FAST, m_exit),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("isolation_level", g_isolation_level, s_isolation_level),
            ("autocommit", g_autocommit, s_autocommit),
            ("total_changes", g_total_changes),
            ("in_transaction", g_in_transaction),
            ("row_factory", g_row_factory, s_row_factory),
            ("text_factory", g_text_factory, s_text_factory),
            ("Warning", g_warning),
            ("Error", g_error),
            ("InterfaceError", g_interface_error),
            ("DatabaseError", g_database_error),
            ("DataError", g_data_error),
            ("OperationalError", g_operational_error),
            ("IntegrityError", g_integrity_error),
            ("InternalError", g_internal_error),
            ("ProgrammingError", g_programming_error),
            ("NotSupportedError", g_not_supported_error),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
        // the callable signature
        let d = &*(*t).tp_dict;
        let _ = d;

        let s = &raw mut STATEMENT_TYPE;
        fc::init_robj_type::<Statement>(s, c"sqlite3.Statement", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        rpy_runtime::types::typeobject::type_ready(s)?;
    }
    Ok(())
}
