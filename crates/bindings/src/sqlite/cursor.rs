//! sqlite3.Cursor and sqlite3.Row

use super::connection::{self, checked, conn, statement};
use super::ffi::*;
use super::*;
use rpy_runtime::modules::fncommon::{self as fc, Trav};
use std::ffi::c_void;

pub struct Cur {
    pub connection: Option<PyRef>,
    pub initialized: bool,
    pub description: PyRef,
    pub converters: Vec<Option<PyRef>>,
    pub arraysize: isize,
    pub lastrowid: PyRef,
    pub rowcount: i64,
    pub row_factory: PyRef,
    pub statement: Option<PyRef>,
    pub closed: bool,
    pub locked: bool,
}

impl Default for Cur {
    fn default() -> Cur {
        Cur {
            connection: None,
            initialized: false,
            description: none(),
            converters: Vec::new(),
            arraysize: 1,
            lastrowid: none(),
            rowcount: -1,
            row_factory: none(),
            statement: None,
            closed: false,
            locked: false,
        }
    }
}

impl Trav for Cur {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        let mut refs: Vec<*mut PyObject> = vec![self.description.as_ptr(), self.lastrowid.as_ptr(), self.row_factory.as_ptr()];
        if let Some(c) = &self.connection {
            refs.push(c.as_ptr());
        }
        if let Some(s) = &self.statement {
            refs.push(s.as_ptr());
        }
        for c in self.converters.iter().flatten() {
            refs.push(c.as_ptr());
        }
        for r in refs {
            let x = v(r);
            if x != 0 {
                return x;
            }
        }
        0
    }
}

impl Drop for Cur {
    fn drop(&mut self) {
        if let Some(s) = self.statement.take() {
            reset_statement(&s);
        }
    }
}

pub static mut CURSOR_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ROW_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn cursor_type() -> *mut PyTypeObject {
    &raw mut CURSOR_TYPE
}
pub fn row_type() -> *mut PyTypeObject {
    &raw mut ROW_TYPE
}

pub fn cur(o: &PyObject) -> &'static mut Cur {
    unsafe { &mut *(fc::rs::<Cur>(o) as *mut Cur) }
}

pub fn reset_statement(s: &PyObject) {
    let stmt = statement(s);
    if stmt.in_use && !stmt.st.is_null() {
        let st = stmt.st;
        unsafe {
            let ts = capi::PyEval_SaveThread();
            (lib().sqlite3_reset.unwrap())(st);
            capi::PyEval_RestoreThread(ts);
        }
    }
    stmt.in_use = false;
}

fn step(st: *mut Stmt) -> c_int {
    unsafe {
        let ts = capi::PyEval_SaveThread();
        let rc = (lib().sqlite3_step.unwrap())(st);
        capi::PyEval_RestoreThread(ts);
        rc
    }
}

unsafe extern "C" fn cur_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, Cur::default()))
}

unsafe extern "C" fn cur_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = fc::targs(args);
        let r = (|| -> PyResult<()> {
            if fc::kw_len(kw) > 0 {
                raise!(TypeError, "Cursor() takes no keyword arguments");
            }
            if a.len() != 1 {
                raise!(TypeError, "Cursor() takes exactly 1 argument ({} given)", a.len());
            }
            if !connection::is_connection(a[0]) {
                raise!(TypeError, "Cursor() argument 1 must be sqlite3.Connection, not {}", tname(a[0]));
            }
            let c = cur(&*o);
            c.connection = Some(a[0].to_ref());
            c.arraysize = 1;
            c.rowcount = -1;
            c.description = none();
            c.lastrowid = none();
            c.row_factory = none();
            c.initialized = true;
            Ok(())
        })();
        int_into_c(r)
    }
}

fn check_cursor(o: &PyObject) -> PyResult<&'static mut Cur> {
    let c = cur(o);
    if !c.initialized {
        return Err(programming_error("Base Cursor.__init__ not called."));
    }
    if c.closed {
        return Err(programming_error("Cannot operate on a closed cursor."));
    }
    if c.locked {
        return Err(programming_error("Recursive use of cursors not allowed."));
    }
    checked(c.connection.as_ref().unwrap())?;
    Ok(c)
}

// ------------------------------------------------------------------ binding

fn need_adapt(o: &PyObject) -> bool {
    if BASE_TYPE_ADAPTED.load(Ordering::Relaxed) {
        return true;
    }
    unsafe {
        let t = o.type_ptr();
        !(t == rpy_runtime::types::int::int_type()
            || t == rpy_runtime::types::float::float_type()
            || t == rpy_runtime::types::str::str_type()
            || t == &raw mut rpy_runtime::types::bytearray::PyByteArray_Type)
    }
}

fn bind_one(st: *mut Stmt, pos: c_int, value: &PyObject) -> PyResult<()> {
    let adapted;
    let v: &PyObject = if !is_none(value) && need_adapt(value) {
        adapted = adapt(value, &st_proto(), Some(value))?;
        &adapted
    } else {
        value
    };
    let rc = if is_none(v) {
        sq!(sqlite3_bind_null(st, pos))
    } else if rpy_runtime::types::int::is_int(v) {
        let mut overflow = 0;
        let x = rpy_runtime::types::int::as_i64(v);
        let _ = &mut overflow;
        match x {
            Some(x) => sq!(sqlite3_bind_int64(st, pos, x)),
            None => raise!(OverflowError, "Python int too large to convert to SQLite INTEGER"),
        }
    } else if rpy_runtime::types::float::is_float(v) {
        sq!(sqlite3_bind_double(st, pos, rpy_runtime::types::float::float_value(v)))
    } else if is_str(v) {
        let b = utf8_bytes(v)?;
        if b.len() > i32::MAX as usize {
            raise!(OverflowError, "string longer than INT_MAX bytes");
        }
        sq!(sqlite3_bind_text(st, pos, b.as_ptr() as *const std::ffi::c_char, b.len() as c_int, SQLITE_TRANSIENT))
    } else if unsafe { capi::PyObject_CheckBuffer(v.as_ptr()) } != 0 {
        let mut view: Py_buffer = unsafe { std::mem::zeroed() };
        unsafe {
            if capi::PyObject_GetBuffer(v.as_ptr(), &mut view, 0) < 0 {
                return Err(PyErr);
            }
        }
        let r = if view.len > i32::MAX as isize {
            Err(err!(OverflowError, "BLOB longer than INT_MAX bytes"))
        } else {
            Ok(sq!(sqlite3_bind_blob(st, pos, view.buf as *const c_void, view.len as c_int, SQLITE_TRANSIENT)))
        };
        unsafe { capi::PyBuffer_Release(&mut view) };
        r?
    } else {
        return Err(programming_error(&format!("Error binding parameter {}: type '{}' is not supported", pos, tname(v))));
    };
    if rc != SQLITE_OK {
        if err_occurred() {
            return Err(PyErr);
        }
        return Err(programming_error(&format!("Error binding parameter {}: type '{}' is not supported", pos, tname(v))));
    }
    Ok(())
}

fn st_proto() -> PyRef {
    st().prepare_protocol.clone()
}

fn bind_parameters(st: *mut Stmt, params: &PyObject) -> PyResult<()> {
    let n = sq!(sqlite3_bind_parameter_count(st));
    let is_tuple_or_list = tuple::is_tuple(params) || rpy_runtime::types::list::is_list(params);
    let is_dict = dict::is_dict(params);
    if is_tuple_or_list || (!is_dict && unsafe { capi::PySequence_Check(params.as_ptr()) } != 0) {
        let len = unsafe { capi::PySequence_Size(params.as_ptr()) };
        if len < 0 {
            return Err(PyErr);
        }
        if len != n as isize {
            return Err(programming_error(&format!(
                "Incorrect number of bindings supplied. The current statement uses {}, and there are {} supplied.",
                n, len
            )));
        }
        for i in 1..=n {
            let name = sq!(sqlite3_bind_parameter_name(st, i));
            if !name.is_null() && unsafe { *name } as u8 != b'?' {
                let nm = cstr_to_string(name);
                return Err(programming_error(&format!(
                    "Binding {} ('{}') is a named parameter, but you supplied a sequence which requires nameless (qmark) placeholders.",
                    i,
                    &nm[1..]
                )));
            }
            let item = from_c(unsafe { capi::PySequence_GetItem(params.as_ptr(), (i - 1) as isize) })?;
            bind_one(st, i, &item)?;
        }
        return Ok(());
    }
    if is_dict {
        for i in 1..=n {
            let name = sq!(sqlite3_bind_parameter_name(st, i));
            if name.is_null() {
                return Err(programming_error(&format!(
                    "Binding {} has no name, but you supplied a dictionary (which has only names).",
                    i
                )));
            }
            let nm = cstr_to_string(name);
            let key = new_str(&nm[1..]);
            let item = match rpy_runtime::abstract_::getitem(params, &key) {
                Ok(v) => v,
                Err(_) => {
                    if rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::KeyError()) {
                        rpy_runtime::errors::clear_exc();
                        return Err(programming_error(&format!("You did not supply a value for binding parameter :{}.", &nm[1..])));
                    }
                    return Err(PyErr);
                }
            };
            bind_one(st, i, &item)?;
        }
        return Ok(());
    }
    Err(programming_error("parameters are of unsupported type"))
}

// ------------------------------------------------------------------ execution

fn column_name(st: *mut Stmt, i: c_int, detect: i32) -> PyResult {
    let p = sq!(sqlite3_column_name(st, i));
    if p.is_null() {
        unsafe { capi::PyErr_NoMemory() };
        return Err(PyErr);
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
    let mut end = bytes.len();
    if detect & PARSE_COLNAMES != 0 {
        if let Some(pos) = bytes.windows(2).position(|w| w == b" [") {
            end = pos;
        }
    }
    from_c(unsafe { capi::PyUnicode_FromStringAndSize(bytes.as_ptr() as *const std::ffi::c_char, end as isize) })
}

fn lookup_converter(name: &[u8]) -> PyResult<Option<PyRef>> {
    let s = String::from_utf8_lossy(name).to_uppercase();
    dict::get_item(&st().converters, &new_str(&s))
}

fn build_converters(c: &mut Cur, st: *mut Stmt, detect: i32) -> PyResult<()> {
    c.converters.clear();
    if detect == 0 {
        return Ok(());
    }
    let n = sq!(sqlite3_column_count(st));
    for i in 0..n {
        let mut conv = None;
        if detect & PARSE_COLNAMES != 0 {
            let p = sq!(sqlite3_column_name(st, i));
            if !p.is_null() {
                let b = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
                if let Some(l) = b.iter().position(|&x| x == b'[') {
                    if let Some(r) = b[l + 1..].iter().position(|&x| x == b']') {
                        conv = lookup_converter(&b[l + 1..l + 1 + r])?;
                    }
                }
            }
        }
        if conv.is_none() && detect & PARSE_DECLTYPES != 0 {
            let p = sq!(sqlite3_column_decltype(st, i));
            if !p.is_null() {
                let b = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
                let end = b.iter().position(|&x| x == b' ' || x == b'(').unwrap_or(b.len());
                conv = lookup_converter(&b[..end])?;
            }
        }
        c.converters.push(conv);
    }
    Ok(())
}

fn build_description(st: *mut Stmt, detect: i32) -> PyResult {
    let n = sq!(sqlite3_column_count(st));
    if n == 0 {
        return Ok(none());
    }
    let mut cols = Vec::with_capacity(n as usize);
    for i in 0..n {
        let name = column_name(st, i, detect)?;
        cols.push(tuple::new_tuple(vec![name, none(), none(), none(), none(), none(), none()]));
    }
    Ok(tuple::new_tuple(cols))
}

pub fn execute_impl(o: &PyObject, sql: &PyObject, params: Option<&PyObject>, multiple: bool) -> PyResult<()> {
    let c = check_cursor(o)?;
    if !is_str(sql) {
        raise!(TypeError, "execute() argument 1 must be str, not {}", tname(sql));
    }
    c.locked = true;
    let r = execute_locked(o, c, sql, params, multiple);
    c.locked = false;
    r
}

fn execute_locked(_o: &PyObject, c: &mut Cur, sql: &PyObject, params: Option<&PyObject>, multiple: bool) -> PyResult<()> {
    let conn_obj = c.connection.clone().unwrap();
    let cn = conn(&conn_obj);
    if let Some(s) = c.statement.take() {
        reset_statement(&s);
    }
    c.description = none();
    c.rowcount = -1;
    // parameter sets
    let param_iter: PyRef = if multiple {
        let p = params.unwrap();
        rpy_runtime::abstract_::get_iter(p)?
    } else {
        let p = match params {
            Some(p) => p.to_ref(),
            None => tuple::empty_tuple(),
        };
        rpy_runtime::abstract_::get_iter(&tuple::new_tuple(vec![p]))?
    };
    let mut stmt_obj = connection::get_statement(cn, sql)?;
    if statement(&stmt_obj).in_use {
        stmt_obj = connection::prepare(cn, sql)?;
    }
    let stp = statement(&stmt_obj).st;
    let is_dml = statement(&stmt_obj).is_dml;
    if stp.is_null() {
        // empty statement (only comments/whitespace)
        return Ok(());
    }
    sq!(sqlite3_reset(stp));
    if multiple && sq!(sqlite3_stmt_readonly(stp)) != 0 {
        return Err(programming_error("executemany() can only execute DML statements."));
    }
    // implicit transaction (legacy mode)
    if cn.autocommit == -1 && is_dml && sq!(sqlite3_get_autocommit(cn.db)) != 0 {
        if let Some(level) = cn.isolation_level {
            connection::exec_simple(cn, &format!("BEGIN {}", level))?;
        }
    }
    build_converters(c, stp, cn.detect_types)?;
    let db = cn.db;
    loop {
        let params = match rpy_runtime::abstract_::iter_next(&param_iter)? {
            Some(p) => p,
            None => break,
        };
        sq!(sqlite3_reset(stp));
        sq!(sqlite3_clear_bindings(stp));
        statement(&stmt_obj).in_use = true;
        if let Err(e) = bind_parameters(stp, &params) {
            reset_statement(&stmt_obj);
            return Err(e);
        }
        let rc = step(stp);
        if rc != SQLITE_DONE && rc != SQLITE_ROW {
            let pending = err_occurred();
            reset_statement(&stmt_obj);
            if pending {
                return Err(PyErr);
            }
            return Err(set_db_error(db));
        }
        if c.description.is(rpy_runtime::types::none::none()) {
            c.description = build_description(stp, cn.detect_types)?;
        }
        if is_dml {
            if c.rowcount < 0 {
                c.rowcount = 0;
            }
            c.rowcount += sq!(sqlite3_changes(db)) as i64;
        }
        if rc == SQLITE_ROW {
            if multiple {
                reset_statement(&stmt_obj);
                return Err(programming_error("executemany() can only execute DML statements."));
            }
            c.statement = Some(stmt_obj.clone());
            break;
        }
        reset_statement(&stmt_obj);
    }
    c.lastrowid = new_int(sq!(sqlite3_last_insert_rowid(db)));
    Ok(())
}

pub fn executescript_impl(o: &PyObject, script: &PyObject) -> PyResult<()> {
    let c = check_cursor(o)?;
    if !is_str(script) {
        raise!(ValueError, "script argument must be unicode.");
    }
    let sql = utf8_bytes(script)?;
    if sql.contains(&0) {
        return Err(programming_error("the query contains a null character"));
    }
    let conn_obj = c.connection.clone().unwrap();
    let cn = conn(&conn_obj);
    let limit = sq!(sqlite3_limit(cn.db, 1, -1));
    if sql.len() as i64 > limit as i64 {
        return Err(data_error("query string is too large"));
    }
    c.locked = true;
    let r = (|| -> PyResult<()> {
        if cn.autocommit == -1 && sq!(sqlite3_get_autocommit(cn.db)) == 0 {
            connection::exec_simple(cn, "COMMIT")?;
        }
        let csql = std::ffi::CString::new(sql.clone()).unwrap();
        let mut p = csql.as_ptr();
        let db = cn.db;
        loop {
            let mut st: *mut Stmt = std::ptr::null_mut();
            let mut tail: *const std::ffi::c_char = std::ptr::null();
            let rc = unsafe {
                let ts = capi::PyEval_SaveThread();
                let rc = (lib().sqlite3_prepare_v2.unwrap())(db, p, -1, &mut st, &mut tail);
                capi::PyEval_RestoreThread(ts);
                rc
            };
            if rc != SQLITE_OK {
                return Err(set_db_error(db));
            }
            if !st.is_null() {
                let mut rc = SQLITE_ROW;
                while rc == SQLITE_ROW {
                    rc = step(st);
                }
                let fin = sq!(sqlite3_finalize(st));
                if rc != SQLITE_DONE {
                    if err_occurred() {
                        return Err(PyErr);
                    }
                    let _ = fin;
                    return Err(set_db_error(db));
                }
            }
            if tail.is_null() || unsafe { *tail } == 0 {
                break;
            }
            p = tail;
        }
        Ok(())
    })();
    c.locked = false;
    r
}

// ------------------------------------------------------------------ fetching

fn column_value(c: &Cur, cn: &connection::Conn, st: *mut Stmt, i: c_int) -> PyResult {
    let ctype = sq!(sqlite3_column_type(st, i));
    if let Some(Some(conv)) = c.converters.get(i as usize) {
        if ctype != SQLITE_NULL {
            let p = sq!(sqlite3_column_blob(st, i));
            let n = sq!(sqlite3_column_bytes(st, i));
            let b = if p.is_null() {
                rpy_runtime::types::bytes::new_bytes(&[])
            } else {
                rpy_runtime::types::bytes::new_bytes(unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) })
            };
            return call(conv, &[&b]);
        }
    }
    Ok(match ctype {
        SQLITE_NULL => none(),
        SQLITE_INTEGER => new_int(sq!(sqlite3_column_int64(st, i))),
        SQLITE_FLOAT => rpy_runtime::types::float::new_float(sq!(sqlite3_column_double(st, i))),
        SQLITE_TEXT => {
            let p = sq!(sqlite3_column_text(st, i));
            let n = sq!(sqlite3_column_bytes(st, i));
            let data: &[u8] = if p.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(p, n as usize) } };
            text_value(cn, st, i, data)?
        }
        _ => {
            let p = sq!(sqlite3_column_blob(st, i));
            let n = sq!(sqlite3_column_bytes(st, i));
            if p.is_null() {
                rpy_runtime::types::bytes::new_bytes(&[])
            } else {
                rpy_runtime::types::bytes::new_bytes(unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) })
            }
        }
    })
}

fn text_value(cn: &connection::Conn, st: *mut Stmt, i: c_int, data: &[u8]) -> PyResult {
    let tf = &cn.text_factory;
    unsafe {
        if tf.is(rpy_runtime::types::typeobject::type_obj(rpy_runtime::types::str::str_type())) {
            let r = capi::PyUnicode_Decode(data.as_ptr() as *const std::ffi::c_char, data.len() as isize, cs!("utf-8"), cs!("strict"));
            if r.is_null() {
                if rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::UnicodeDecodeError()) {
                    rpy_runtime::errors::clear_exc();
                    let col = cstr_to_string(sq!(sqlite3_column_name(st, i)));
                    let text = String::from_utf8_lossy(data);
                    return Err(operational_error(&format!("Could not decode to UTF-8 column '{}' with text '{}'", col, text)));
                }
                return Err(PyErr);
            }
            return Ok(PyRef::from_raw(r));
        }
        if tf.is(rpy_runtime::types::typeobject::type_obj(rpy_runtime::types::bytes::bytes_type())) {
            return Ok(rpy_runtime::types::bytes::new_bytes(data));
        }
        if tf.is(rpy_runtime::types::typeobject::type_obj(&raw mut rpy_runtime::types::bytearray::PyByteArray_Type)) {
            let b = rpy_runtime::types::bytes::new_bytes(data);
            return call(tf, &[&b]);
        }
    }
    let b = rpy_runtime::types::bytes::new_bytes(data);
    call(tf, &[&b])
}

/// Next row (None at the end).
fn next_row(o: &PyObject) -> PyResult<Option<PyRef>> {
    let c = check_cursor(o)?;
    let stmt_obj = match &c.statement {
        Some(s) => s.clone(),
        None => return Ok(None),
    };
    let stmt = statement(&stmt_obj);
    if !stmt.in_use || stmt.st.is_null() {
        c.statement = None;
        return Ok(None);
    }
    let st = stmt.st;
    let conn_obj = c.connection.clone().unwrap();
    let cn = conn(&conn_obj);
    let n = sq!(sqlite3_data_count(st));
    let mut vals = Vec::with_capacity(n as usize);
    for i in 0..n {
        vals.push(column_value(c, cn, st, i)?);
    }
    let row = tuple::new_tuple(vals);
    // advance
    let rc = step(st);
    if rc == SQLITE_DONE {
        reset_statement(&stmt_obj);
        c.statement = None;
    } else if rc != SQLITE_ROW {
        let pending = err_occurred();
        reset_statement(&stmt_obj);
        c.statement = None;
        if pending {
            return Err(PyErr);
        }
        return Err(set_db_error(cn.db));
    }
    if !is_none(&c.row_factory) {
        return call(&c.row_factory.clone(), &[o, &row]).map(Some);
    }
    Ok(Some(row))
}

unsafe extern "C" fn cur_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match next_row(&*o) {
            Ok(Some(r)) => r.into_raw(),
            Ok(None) => std::ptr::null_mut(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" fn cur_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (&*o).to_ref().into_raw() }
}

fn m_execute(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "execute expected 1 or 2 arguments, got {}", args.len());
    }
    execute_impl(o, args[0], args.get(1).copied(), false)?;
    Ok(o.to_ref())
}

fn m_executemany(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "executemany expected 2 arguments, got {}", args.len());
    }
    execute_impl(o, args[0], Some(args[1]), true)?;
    Ok(o.to_ref())
}

fn m_executescript(o: &PyObject, s: &PyObject) -> PyResult {
    executescript_impl(o, s)?;
    Ok(o.to_ref())
}

fn m_fetchone(o: &PyObject) -> PyResult {
    Ok(next_row(o)?.unwrap_or_else(none))
}

fn m_fetchmany(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("fetchmany", &["size"], 0, 0);
    let [size] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let n = match size {
        Some(s) => {
            let v = unsafe { capi::PyLong_AsInt(s.as_ptr()) };
            if v == -1 && err_occurred() {
                return Err(PyErr);
            }
            if v < 0 {
                raise!(ValueError, "fetchmany() size must be non-negative");
            }
            v as isize
        }
        None => cur(o).arraysize,
    };
    let l = rpy_runtime::types::list::new_list(Vec::new());
    let mut k = 0;
    while k < n {
        match next_row(o)? {
            Some(r) => rpy_runtime::types::list::append(&l, r),
            None => break,
        }
        k += 1;
    }
    Ok(l)
}

fn m_fetchall(o: &PyObject) -> PyResult {
    let l = rpy_runtime::types::list::new_list(Vec::new());
    while let Some(r) = next_row(o)? {
        rpy_runtime::types::list::append(&l, r);
    }
    Ok(l)
}

fn m_close(o: &PyObject) -> PyResult {
    let c = cur(o);
    if !c.initialized {
        return Err(programming_error("Base Cursor.__init__ not called."));
    }
    if c.locked {
        return Err(programming_error("Recursive use of cursors not allowed."));
    }
    let cn = conn(c.connection.as_ref().unwrap());
    connection::check_thread(cn)?;
    if let Some(s) = c.statement.take() {
        reset_statement(&s);
    }
    c.closed = true;
    Ok(none())
}

fn m_setinputsizes(_o: &PyObject, _a: &PyObject) -> PyResult {
    Ok(none())
}

fn m_setoutputsize(_o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "setoutputsize expected 1 or 2 arguments, got {}", args.len());
    }
    Ok(none())
}

fn g_connection(o: &PyObject) -> PyResult {
    Ok(cur(o).connection.clone().unwrap_or_else(none))
}
fn g_description(o: &PyObject) -> PyResult {
    Ok(cur(o).description.clone())
}
fn g_lastrowid(o: &PyObject) -> PyResult {
    Ok(cur(o).lastrowid.clone())
}
fn g_rowcount(o: &PyObject) -> PyResult {
    Ok(new_int(cur(o).rowcount))
}
fn g_arraysize(o: &PyObject) -> PyResult {
    Ok(new_int(cur(o).arraysize as i64))
}
fn s_arraysize(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let n = unsafe { capi::PyLong_AsInt(v.as_ptr()) };
    if n == -1 && err_occurred() {
        return Err(PyErr);
    }
    cur(o).arraysize = n as isize;
    Ok(())
}
fn g_row_factory(o: &PyObject) -> PyResult {
    Ok(cur(o).row_factory.clone())
}
fn s_row_factory(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => {
            cur(o).row_factory = v.to_ref();
            Ok(())
        }
        None => raise!(AttributeError, "cannot delete attribute"),
    }
}

// ------------------------------------------------------------------ Row

pub struct Row {
    pub data: PyRef,
    pub description: PyRef,
}

impl Trav for Row {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        let r = v(self.data.as_ptr());
        if r != 0 {
            return r;
        }
        v(self.description.as_ptr())
    }
}

fn row(o: &PyObject) -> &'static mut Row {
    unsafe { &mut *(fc::rs::<Row>(o) as *mut Row) }
}

unsafe extern "C" fn row_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = fc::targs(args);
        let r = (|| -> PyResult {
            if fc::kw_len(kw) > 0 {
                raise!(TypeError, "Row() takes no keyword arguments");
            }
            if a.len() != 2 {
                raise!(TypeError, "Row() takes exactly 2 arguments ({} given)", a.len());
            }
            if !crate::ctypes::subtype(a[0].type_ptr(), cursor_type()) {
                raise!(TypeError, "Row() argument 1 must be sqlite3.Cursor, not {}", tname(a[0]));
            }
            if !tuple::is_tuple(a[1]) {
                raise!(TypeError, "Row() argument 2 must be tuple, not {}", tname(a[1]));
            }
            let desc = cur(a[0]).description.clone();
            fc::robj_new(tp, Row { data: a[1].to_ref(), description: desc })
        })();
        into_c(r)
    }
}

fn row_names(o: &PyObject) -> Vec<PyRef> {
    let d = &row(o).description;
    if is_none(d) || !tuple::is_tuple(d) {
        return Vec::new();
    }
    tuple::tuple_items(d).iter().map(|c| tuple::tuple_get(c, 0).to_ref()).collect()
}

fn row_subscript_impl(o: &PyObject, key: &PyObject) -> PyResult {
    let data = row(o).data.clone();
    let n = tuple::tuple_len(&data) as isize;
    if rpy_runtime::types::int::is_int(key) {
        let mut i = match rpy_runtime::types::int::as_i64(key) {
            Some(i) => i as isize,
            None => raise!(IndexError, "Index must be int or string"),
        };
        if i < 0 {
            i += n;
        }
        if i < 0 || i >= n {
            raise!(IndexError, "tuple index out of range");
        }
        return Ok(tuple::tuple_get(&data, i as usize).to_ref());
    }
    if is_str(key) {
        let k = str_of(key);
        for (idx, nm) in row_names(o).iter().enumerate() {
            if str_of(nm).eq_ignore_ascii_case(&k) && str_of(nm).len() == k.len() {
                return Ok(tuple::tuple_get(&data, idx).to_ref());
            }
        }
        raise!(IndexError, "No item with that key");
    }
    if unsafe { key.type_ptr() == &raw mut rpy_runtime::types::slice::PySlice_Type } {
        return rpy_runtime::abstract_::getitem(&data, key);
    }
    raise!(IndexError, "Index must be int or string")
}

unsafe extern "C" fn row_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(row_subscript_impl(&*o, &*k)) }
}

unsafe extern "C" fn row_item(o: *mut PyObject, i: isize) -> *mut PyObject {
    unsafe {
        let data = &row(&*o).data;
        let n = tuple::tuple_len(data) as isize;
        if i < 0 || i >= n {
            let _ = err!(IndexError, "tuple index out of range");
            return std::ptr::null_mut();
        }
        tuple::tuple_get(data, i as usize).to_ref().into_raw()
    }
}

unsafe extern "C" fn row_length(o: *mut PyObject) -> isize {
    unsafe { tuple::tuple_len(&row(&*o).data) as isize }
}

unsafe extern "C" fn row_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(rpy_runtime::abstract_::get_iter(&row(&*o).data)) }
}

unsafe extern "C" fn row_hash(o: *mut PyObject) -> isize {
    unsafe {
        let r = row(&*o);
        let a = match rpy_runtime::abstract_::hash(&r.description) {
            Ok(h) => h,
            Err(_) => return -1,
        };
        let b = match rpy_runtime::abstract_::hash(&r.data) {
            Ok(h) => h,
            Err(_) => return -1,
        };
        let h = a ^ b;
        if h == -1 { -2 } else { h }
    }
}

unsafe extern "C" fn row_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if op != 2 && op != 3 {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        }
        if !crate::ctypes::subtype((*b).type_ptr(), row_type()) {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        }
        let ra = row(&*a);
        let rb = row(&*b);
        let r = (|| -> PyResult<bool> {
            let d = rpy_runtime::abstract_::rich_eq(&ra.description, &rb.description)?;
            if !d {
                return Ok(false);
            }
            rpy_runtime::abstract_::rich_eq(&ra.data, &rb.data)
        })();
        match r {
            Ok(eq) => rpy_runtime::types::bool_::new_bool(if op == 2 { eq } else { !eq }).into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

fn row_keys(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::list::new_list(row_names(o)))
}

static mut ROW_AS_SEQUENCE: PySequenceMethods = unsafe { std::mem::zeroed() };
static mut ROW_AS_MAPPING: PyMappingMethods = unsafe { std::mem::zeroed() };

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = cursor_type();
        fc::init_robj_type::<Cur>(t, c"sqlite3.Cursor", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(cur_new);
        (*t).tp_init = Some(cur_init);
        (*t).tp_iter = Some(cur_iter);
        (*t).tp_iternext = Some(cur_iternext);
        (*t).tp_doc = c"SQLite database cursor class.".as_ptr();
        (*t).tp_methods = rpy_runtime::mdefs![
            ("execute", FAST, m_execute, "Executes an SQL statement."),
            ("executemany", FAST, m_executemany, "Repeatedly executes an SQL statement."),
            ("executescript", O, m_executescript, "Executes multiple SQL statements at once."),
            ("fetchone", NOARGS, m_fetchone, "Fetches one row from the resultset."),
            ("fetchmany", FASTKW, m_fetchmany, "Fetches several rows from the resultset."),
            ("fetchall", NOARGS, m_fetchall, "Fetches all rows from the resultset."),
            ("close", NOARGS, m_close, "Closes the cursor."),
            ("setinputsizes", O, m_setinputsizes, "Required by DB-API. Does nothing in sqlite3."),
            ("setoutputsize", FAST, m_setoutputsize, "Required by DB-API. Does nothing in sqlite3."),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("connection", g_connection),
            ("description", g_description),
            ("lastrowid", g_lastrowid),
            ("rowcount", g_rowcount),
            ("arraysize", g_arraysize, s_arraysize),
            ("row_factory", g_row_factory, s_row_factory),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;

        let r = row_type();
        fc::init_robj_type::<Row>(r, c"sqlite3.Row", Py_TPFLAGS_BASETYPE);
        (*r).tp_new = Some(row_new);
        (*r).tp_hash = Some(row_hash);
        (*r).tp_richcompare = Some(row_richcompare);
        (*r).tp_iter = Some(row_iter);
        ROW_AS_SEQUENCE.sq_length = Some(row_length);
        ROW_AS_SEQUENCE.sq_item = Some(row_item);
        ROW_AS_MAPPING.mp_length = Some(row_length);
        ROW_AS_MAPPING.mp_subscript = Some(row_subscript);
        (*r).tp_as_sequence = &raw mut ROW_AS_SEQUENCE;
        (*r).tp_as_mapping = &raw mut ROW_AS_MAPPING;
        (*r).tp_methods = rpy_runtime::mdefs![("keys", NOARGS, row_keys, "Returns the keys of the row.")];
        rpy_runtime::types::typeobject::type_ready(r)?;
    }
    Ok(())
}

pub fn _unused(_: *mut c_void) {}
