//! User-defined SQL functions, aggregates, window functions, collations and the
//! authorizer / progress / trace hooks; Connection.backup.

use super::connection::{checked, conn};
use super::ffi::*;
use super::*;
use std::ffi::{CString, c_char, c_uint, c_void};

/// Context passed to SQLite as application data.
pub struct CbCtx {
    pub callable: PyRef,
}

unsafe extern "C" fn destroy_ctx(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        let g = crate::gil::ensure();
        drop(Box::from_raw(p as *mut CbCtx));
        crate::gil::release(g);
    }
}

fn ctx_callable(p: *mut c_void) -> PyRef {
    unsafe { (*(p as *mut CbCtx)).callable.clone() }
}

// ------------------------------------------------------------------ value conversion

pub fn value_to_py(v: *mut Value) -> PyResult {
    let t = sq!(sqlite3_value_type(v));
    Ok(match t {
        SQLITE_INTEGER => new_int(sq!(sqlite3_value_int64(v))),
        SQLITE_FLOAT => rpy_runtime::types::float::new_float(sq!(sqlite3_value_double(v))),
        SQLITE_TEXT => {
            let p = sq!(sqlite3_value_text(v));
            let n = sq!(sqlite3_value_bytes(v));
            if p.is_null() {
                if n == 0 {
                    return Ok(new_str(""));
                }
                unsafe { capi::PyErr_NoMemory() };
                return Err(PyErr);
            }
            from_c(unsafe { capi::PyUnicode_FromStringAndSize(p as *const c_char, n as isize) })?
        }
        SQLITE_BLOB => {
            let p = sq!(sqlite3_value_blob(v));
            let n = sq!(sqlite3_value_bytes(v));
            if p.is_null() {
                rpy_runtime::types::bytes::new_bytes(&[])
            } else {
                rpy_runtime::types::bytes::new_bytes(unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) })
            }
        }
        _ => none(),
    })
}

fn args_to_py(argc: c_int, argv: *mut *mut Value) -> PyResult<Vec<PyRef>> {
    let mut out = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        out.push(value_to_py(unsafe { *argv.add(i as usize) })?);
    }
    Ok(out)
}

/// Store a Python value as the function result.
fn set_result(ctx: *mut Ctx, r: &PyObject) -> PyResult<()> {
    if is_none(r) {
        sq!(sqlite3_result_null(ctx));
    } else if rpy_runtime::types::int::is_int(r) {
        match rpy_runtime::types::int::as_i64(r) {
            Some(v) => sq!(sqlite3_result_int64(ctx, v)),
            None => raise!(OverflowError, "Python int too large to convert to SQLite INTEGER"),
        }
    } else if rpy_runtime::types::float::is_float(r) {
        sq!(sqlite3_result_double(ctx, rpy_runtime::types::float::float_value(r)));
    } else if is_str(r) {
        let b = utf8_bytes(r)?;
        if b.len() > i32::MAX as usize {
            sq!(sqlite3_result_error_toobig(ctx));
            return Ok(());
        }
        sq!(sqlite3_result_text(ctx, b.as_ptr() as *const c_char, b.len() as c_int, SQLITE_TRANSIENT));
    } else if unsafe { capi::PyObject_CheckBuffer(r.as_ptr()) } != 0 {
        let mut view: Py_buffer = unsafe { std::mem::zeroed() };
        unsafe {
            if capi::PyObject_GetBuffer(r.as_ptr(), &mut view, 0) < 0 {
                return Err(PyErr);
            }
        }
        if view.len > i32::MAX as isize {
            sq!(sqlite3_result_error_toobig(ctx));
        } else {
            sq!(sqlite3_result_blob(ctx, view.buf, view.len as c_int, SQLITE_TRANSIENT));
        }
        unsafe { capi::PyBuffer_Release(&mut view) };
    } else {
        raise!(TypeError, "unsupported type for a user-defined function result: {}", tname(r));
    }
    Ok(())
}

/// Turn a pending Python exception into an SQLite function error.
fn function_error(ctx: *mut Ctx, callable: &PyObject, msg: &str) {
    let is_mem = rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::MemoryError());
    let is_overflow = rpy_runtime::errors::exc_matches(rpy_runtime::types::exceptions::OverflowError());
    report_callback_error(Some(callable));
    if is_mem {
        sq!(sqlite3_result_error_nomem(ctx));
    } else if is_overflow {
        sq!(sqlite3_result_error_toobig(ctx));
    } else {
        let m = cstring(msg);
        sq!(sqlite3_result_error(ctx, m.as_ptr(), -1));
    }
}

// ------------------------------------------------------------------ scalar functions

unsafe extern "C" fn func_callback(ctx: *mut Ctx, argc: c_int, argv: *mut *mut Value) {
    unsafe {
        let g = crate::gil::ensure();
        let callable = ctx_callable(sq!(sqlite3_user_data(ctx)));
        let r = args_to_py(argc, argv).and_then(|a| {
            let refs: Vec<&PyObject> = a.iter().map(|x| &**x).collect();
            call(&callable, &refs)
        });
        let ok = match r {
            Ok(v) => set_result(ctx, &v).is_ok(),
            Err(_) => false,
        };
        if !ok {
            function_error(ctx, &callable, "user-defined function raised exception");
        }
        crate::gil::release(g);
    }
}

fn check_narg(narg: i64) -> PyResult<()> {
    if !(-1..=127).contains(&narg) {
        return Err(programming_error(&format!("'narg' must be between -1 and 127, not {}", narg)));
    }
    Ok(())
}

fn no_kwargs(fname: &str, kw: Option<&PyObject>) -> PyResult<()> {
    if let Some(k) = kw {
        if tuple::tuple_len(k) > 0 {
            raise!(TypeError, "{}() got an unexpected keyword argument '{}'", fname, str_of(tuple::tuple_get(k, 0)));
        }
    }
    Ok(())
}

fn positional(fname: &str, args: &[&PyObject], kw: Option<&PyObject>, n: usize) -> PyResult<()> {
    let nkw = kw.map_or(0, |k| tuple::tuple_len(k));
    let npos = args.len() - nkw;
    if npos != n {
        raise!(
            TypeError,
            "{}() takes exactly {} positional argument{} ({} given)",
            fname,
            n,
            if n == 1 { "" } else { "s" },
            npos
        );
    }
    Ok(())
}

pub fn create_function(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("create_function", args, kw, 3)?;
    let mut deterministic = false;
    if let Some(k) = kw {
        for (i, name) in tuple::tuple_items(k).iter().enumerate() {
            if str_of(name) == "deterministic" {
                deterministic = rpy_runtime::abstract_::is_true(args[3 + i])?;
            } else {
                raise!(TypeError, "create_function() got an unexpected keyword argument '{}'", str_of(name));
            }
        }
    }
    let c = checked(o)?;
    if !is_str(args[0]) {
        raise!(TypeError, "create_function() argument 1 must be str, not {}", tname(args[0]));
    }
    let name = utf8_cstring(args[0])?;
    let narg = rpy_runtime::abstract_::as_i64(args[1])?;
    check_narg(narg)?;
    let func = args[2];
    let mut flags = SQLITE_UTF8;
    if deterministic {
        flags |= SQLITE_DETERMINISTIC;
    }
    let ctx = Box::into_raw(Box::new(CbCtx { callable: func.to_ref() })) as *mut c_void;
    let rc = sq!(sqlite3_create_function_v2(c.db, name.as_ptr(), narg as c_int, flags, ctx, Some(func_callback), None, None, Some(destroy_ctx)));
    if rc != SQLITE_OK {
        return Err(operational_error("Error creating function"));
    }
    Ok(none())
}

// ------------------------------------------------------------------ aggregates

/// The per-group aggregate instance lives in SQLite's aggregate context.
unsafe fn aggregate_instance(ctx: *mut Ctx, create: bool) -> Option<*mut *mut PyObject> {
    let p = sq!(sqlite3_aggregate_context(ctx, if create { std::mem::size_of::<*mut PyObject>() as c_int } else { 0 })) as *mut *mut PyObject;
    if p.is_null() { None } else { Some(p) }
}

unsafe fn get_or_create_instance(ctx: *mut Ctx, cls: &PyObject) -> Result<*mut PyObject, ()> {
    unsafe {
        let slot = match aggregate_instance(ctx, true) {
            Some(s) => s,
            None => return Err(()),
        };
        if (*slot).is_null() {
            match call(cls, &[]) {
                Ok(inst) => *slot = inst.into_raw(),
                Err(_) => {
                    function_error(ctx, cls, "user-defined aggregate's '__init__' method raised error");
                    return Err(());
                }
            }
        }
        Ok(*slot)
    }
}

unsafe fn call_method_of(ctx: *mut Ctx, cls: &PyObject, inst: *mut PyObject, meth: &str, args: &[PyRef], what: &str) -> Option<PyRef> {
    unsafe {
        let m = match getattr_opt(&*inst, meth) {
            Ok(Some(m)) => m,
            Ok(None) => {
                let _ = err!(AttributeError, "'{}' object has no attribute '{}'", tname(&*inst), meth);
                function_error(ctx, cls, &format!("user-defined {} '{}' method not defined", what, meth));
                return None;
            }
            Err(_) => {
                function_error(ctx, cls, &format!("user-defined {} '{}' method not defined", what, meth));
                return None;
            }
        };
        let refs: Vec<&PyObject> = args.iter().map(|x| &**x).collect();
        match call(&m, &refs) {
            Ok(r) => Some(r),
            Err(_) => {
                function_error(ctx, cls, &format!("user-defined {} '{}' method raised error", what, meth));
                None
            }
        }
    }
}

unsafe extern "C" fn agg_step(ctx: *mut Ctx, argc: c_int, argv: *mut *mut Value) {
    unsafe {
        let g = crate::gil::ensure();
        let cls = ctx_callable(sq!(sqlite3_user_data(ctx)));
        if let Ok(inst) = get_or_create_instance(ctx, &cls) {
            match args_to_py(argc, argv) {
                Ok(a) => {
                    let _ = call_method_of(ctx, &cls, inst, "step", &a, "aggregate's");
                }
                Err(_) => function_error(ctx, &cls, "user-defined aggregate's 'step' method raised error"),
            }
        }
        crate::gil::release(g);
    }
}

unsafe extern "C" fn agg_final(ctx: *mut Ctx) {
    unsafe {
        let g = crate::gil::ensure();
        let cls = ctx_callable(sq!(sqlite3_user_data(ctx)));
        if let Some(slot) = aggregate_instance(ctx, false) {
            let inst = *slot;
            if !inst.is_null() {
                *slot = std::ptr::null_mut();
                // keep a pending error from step (sqlite only reports the first one)
                let saved = rpy_runtime::errors::take_exc();
                if let Some(r) = call_method_of(ctx, &cls, inst, "finalize", &[], "aggregate's") {
                    if set_result(ctx, &r).is_err() {
                        function_error(ctx, &cls, "user-defined aggregate's 'finalize' method raised error");
                    }
                }
                if let Some(s) = saved {
                    rpy_runtime::errors::set_exc(s);
                }
                decref(inst);
            }
        }
        crate::gil::release(g);
    }
}

pub fn create_aggregate(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("create_aggregate", args, kw, 3)?;
    no_kwargs("create_aggregate", kw)?;
    let c = checked(o)?;
    if !is_str(args[0]) {
        raise!(TypeError, "create_aggregate() argument 1 must be str, not {}", tname(args[0]));
    }
    let name = utf8_cstring(args[0])?;
    let narg = rpy_runtime::abstract_::as_i64(args[1])?;
    check_narg(narg)?;
    let ctx = Box::into_raw(Box::new(CbCtx { callable: args[2].to_ref() })) as *mut c_void;
    let rc = sq!(sqlite3_create_function_v2(c.db, name.as_ptr(), narg as c_int, SQLITE_UTF8, ctx, None, Some(agg_step), Some(agg_final), Some(destroy_ctx)));
    if rc != SQLITE_OK {
        return Err(operational_error("Error creating aggregate"));
    }
    Ok(none())
}

// ------------------------------------------------------------------ window functions

unsafe extern "C" fn win_step(ctx: *mut Ctx, argc: c_int, argv: *mut *mut Value) {
    unsafe { win_step_inverse(ctx, argc, argv, "step") }
}

unsafe extern "C" fn win_inverse(ctx: *mut Ctx, argc: c_int, argv: *mut *mut Value) {
    unsafe { win_step_inverse(ctx, argc, argv, "inverse") }
}

unsafe fn win_step_inverse(ctx: *mut Ctx, argc: c_int, argv: *mut *mut Value, meth: &str) {
    unsafe {
        let g = crate::gil::ensure();
        let cls = ctx_callable(sq!(sqlite3_user_data(ctx)));
        if let Ok(inst) = get_or_create_instance_win(ctx, &cls) {
            match args_to_py(argc, argv) {
                Ok(a) => {
                    let _ = call_method_of(ctx, &cls, inst, meth, &a, "aggregate's").map(|_| ());
                }
                Err(_) => function_error(ctx, &cls, &format!("'{}' method raised error", meth)),
            }
        }
        crate::gil::release(g);
    }
}

unsafe fn get_or_create_instance_win(ctx: *mut Ctx, cls: &PyObject) -> Result<*mut PyObject, ()> {
    unsafe {
        let slot = match aggregate_instance(ctx, true) {
            Some(s) => s,
            None => return Err(()),
        };
        if (*slot).is_null() {
            match call(cls, &[]) {
                Ok(inst) => *slot = inst.into_raw(),
                Err(_) => {
                    function_error(ctx, cls, "user-defined aggregate's '__init__' method raised error");
                    return Err(());
                }
            }
        }
        Ok(*slot)
    }
}

unsafe extern "C" fn win_value(ctx: *mut Ctx) {
    unsafe {
        let g = crate::gil::ensure();
        let cls = ctx_callable(sq!(sqlite3_user_data(ctx)));
        if let Ok(inst) = get_or_create_instance_win(ctx, &cls) {
            if let Some(r) = call_method_of(ctx, &cls, inst, "value", &[], "aggregate's") {
                if set_result(ctx, &r).is_err() {
                    function_error(ctx, &cls, "user-defined aggregate's 'value' method raised error");
                }
            }
        }
        crate::gil::release(g);
    }
}

pub fn create_window_function(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("create_window_function", args, kw, 3)?;
    no_kwargs("create_window_function", kw)?;
    let c = checked(o)?;
    if !is_str(args[0]) {
        raise!(TypeError, "create_window_function() argument 1 must be str, not {}", tname(args[0]));
    }
    let name = utf8_cstring(args[0])?;
    let narg = rpy_runtime::abstract_::as_i64(args[1])?;
    check_narg(narg)?;
    let rc = if is_none(args[2]) {
        sq!(sqlite3_create_window_function(c.db, name.as_ptr(), narg as c_int, SQLITE_UTF8, std::ptr::null_mut(), None, None, None, None, None))
    } else {
        let ctx = Box::into_raw(Box::new(CbCtx { callable: args[2].to_ref() })) as *mut c_void;
        sq!(sqlite3_create_window_function(
            c.db,
            name.as_ptr(),
            narg as c_int,
            SQLITE_UTF8 | SQLITE_DETERMINISTIC,
            ctx,
            Some(win_step),
            Some(agg_final),
            Some(win_value),
            Some(win_inverse),
            Some(destroy_ctx)
        ))
    };
    if rc != SQLITE_OK {
        return Err(set_db_error(c.db));
    }
    Ok(none())
}

// ------------------------------------------------------------------ collations

unsafe extern "C" fn collation_cb(app: *mut c_void, l1: c_int, s1: *const c_void, l2: c_int, s2: *const c_void) -> c_int {
    unsafe {
        let g = crate::gil::ensure();
        let callable = ctx_callable(app);
        let r = (|| -> PyResult<c_int> {
            let a = from_c(capi::PyUnicode_FromStringAndSize(s1 as *const c_char, l1 as isize))?;
            let b = from_c(capi::PyUnicode_FromStringAndSize(s2 as *const c_char, l2 as isize))?;
            let res = call(&callable, &[&a, &b])?;
            let v = match rpy_runtime::abstract_::as_i64(&res) {
                Ok(v) => v,
                Err(_) => return Err(PyErr),
            };
            Ok(if v > 0 {
                1
            } else if v < 0 {
                -1
            } else {
                0
            })
        })();
        let out = match r {
            Ok(v) => v,
            Err(_) => {
                report_callback_error(Some(&callable));
                0
            }
        };
        crate::gil::release(g);
        out
    }
}

pub fn create_collation(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("create_collation", args, kw, 2)?;
    no_kwargs("create_collation", kw)?;
    let c = checked(o)?;
    if !is_str(args[0]) {
        raise!(TypeError, "create_collation() argument 1 must be str, not {}", tname(args[0]));
    }
    let name = utf8_cstring(args[0])?;
    let callable = args[1];
    let rc = if is_none(callable) {
        sq!(sqlite3_create_collation_v2(c.db, name.as_ptr(), SQLITE_UTF8, std::ptr::null_mut(), None, None))
    } else {
        if !rpy_runtime::abstract_::is_callable(callable) {
            raise!(TypeError, "parameter must be callable");
        }
        let ctx = Box::into_raw(Box::new(CbCtx { callable: callable.to_ref() })) as *mut c_void;
        sq!(sqlite3_create_collation_v2(c.db, name.as_ptr(), SQLITE_UTF8, ctx, Some(collation_cb), Some(destroy_ctx)))
    };
    if rc != SQLITE_OK {
        return Err(set_db_error(c.db));
    }
    Ok(none())
}

// ------------------------------------------------------------------ authorizer / progress / trace

fn opt_str(p: *const c_char) -> PyRef {
    if p.is_null() { none() } else { new_str(&cstr_to_string(p)) }
}

unsafe extern "C" fn authorizer_cb(app: *mut c_void, action: c_int, a1: *const c_char, a2: *const c_char, a3: *const c_char, a4: *const c_char) -> c_int {
    unsafe {
        let g = crate::gil::ensure();
        let callable = (&*(app as *mut PyObject)).to_ref();
        let r = call(&callable, &[&new_int(action as i64), &opt_str(a1), &opt_str(a2), &opt_str(a3), &opt_str(a4)]);
        let rc = match r {
            Ok(v) => {
                if rpy_runtime::types::int::is_int(&v) {
                    let x = capi::PyLong_AsInt(v.as_ptr());
                    if x == -1 && err_occurred() {
                        report_callback_error(Some(&callable));
                        1
                    } else {
                        x
                    }
                } else {
                    1 // SQLITE_DENY
                }
            }
            Err(_) => {
                report_callback_error(Some(&callable));
                1
            }
        };
        crate::gil::release(g);
        rc
    }
}

unsafe extern "C" fn progress_cb(app: *mut c_void) -> c_int {
    unsafe {
        let g = crate::gil::ensure();
        let callable = (&*(app as *mut PyObject)).to_ref();
        let rc = match call(&callable, &[]) {
            Ok(v) => match rpy_runtime::abstract_::is_true(&v) {
                Ok(b) => b as c_int,
                Err(_) => {
                    report_callback_error(Some(&callable));
                    1
                }
            },
            Err(_) => {
                report_callback_error(Some(&callable));
                1
            }
        };
        crate::gil::release(g);
        rc
    }
}

unsafe extern "C" fn trace_cb(_ty: c_uint, app: *mut c_void, stmt: *mut c_void, _x: *mut c_void) -> c_int {
    unsafe {
        let g = crate::gil::ensure();
        let callable = (&*(app as *mut PyObject)).to_ref();
        let expanded = sq!(sqlite3_expanded_sql(stmt));
        let text = if expanded.is_null() {
            let db_limit_err = data_error("Expanded SQL string exceeds the maximum string length");
            let _ = db_limit_err;
            report_callback_error(Some(&callable));
            cstr_to_string(sq!(sqlite3_sql(stmt)))
        } else {
            let s = cstr_to_string(expanded);
            sq!(sqlite3_free(expanded as *mut c_void));
            s
        };
        if call(&callable, &[&new_str(&text)]).is_err() {
            report_callback_error(Some(&callable));
        }
        crate::gil::release(g);
        0
    }
}

fn retire(c: &mut super::connection::Conn, old: Option<PyRef>) {
    if let Some(o) = old {
        c.retired.push(o);
    }
}

pub fn set_authorizer(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("set_authorizer", args, kw, 1)?;
    no_kwargs("set_authorizer", kw)?;
    let c = checked(o)?;
    let cb = args[0];
    if is_none(cb) {
        sq!(sqlite3_set_authorizer(c.db, None, std::ptr::null_mut()));
        let old = c.authorizer.take();
        retire(c, old);
    } else {
        if !rpy_runtime::abstract_::is_callable(cb) {
            raise!(TypeError, "parameter must be callable");
        }
        let keep = cb.to_ref();
        let rc = sq!(sqlite3_set_authorizer(c.db, Some(authorizer_cb), keep.as_ptr() as *mut c_void));
        if rc != SQLITE_OK {
            return Err(operational_error("Error setting authorizer callback"));
        }
        let old = c.authorizer.replace(keep);
        retire(c, old);
    }
    Ok(none())
}

pub fn set_progress_handler(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let nkw = kw.map_or(0, |k| tuple::tuple_len(k));
    let npos = args.len() - nkw;
    if npos < 1 {
        raise!(TypeError, "set_progress_handler() takes at least 1 positional argument ({} given)", npos);
    }
    let mut n: Option<&PyObject> = if npos >= 2 { Some(args[1]) } else { None };
    if let Some(k) = kw {
        for (i, name) in tuple::tuple_items(k).iter().enumerate() {
            if str_of(name) == "n" {
                n = Some(args[npos + i]);
            } else {
                raise!(TypeError, "set_progress_handler() got an unexpected keyword argument '{}'", str_of(name));
            }
        }
    }
    let n = match n {
        Some(n) => {
            let v = unsafe { capi::PyLong_AsInt(n.as_ptr()) };
            if v == -1 && err_occurred() {
                return Err(PyErr);
            }
            v
        }
        None => raise!(TypeError, "set_progress_handler() missing required argument 'n'"),
    };
    let c = checked(o)?;
    let cb = args[0];
    if is_none(cb) {
        sq!(sqlite3_progress_handler(c.db, 0, None, std::ptr::null_mut()));
        let old = c.progress.take();
        retire(c, old);
    } else {
        if !rpy_runtime::abstract_::is_callable(cb) {
            raise!(TypeError, "parameter must be callable");
        }
        let keep = cb.to_ref();
        sq!(sqlite3_progress_handler(c.db, n, Some(progress_cb), keep.as_ptr() as *mut c_void));
        let old = c.progress.replace(keep);
        retire(c, old);
    }
    Ok(none())
}

pub fn set_trace_callback(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    positional("set_trace_callback", args, kw, 1)?;
    no_kwargs("set_trace_callback", kw)?;
    let c = checked(o)?;
    let cb = args[0];
    if is_none(cb) {
        sq!(sqlite3_trace_v2(c.db, SQLITE_TRACE_STMT, None, std::ptr::null_mut()));
        let old = c.trace.take();
        retire(c, old);
    } else {
        if !rpy_runtime::abstract_::is_callable(cb) {
            raise!(TypeError, "parameter must be callable");
        }
        let keep = cb.to_ref();
        sq!(sqlite3_trace_v2(c.db, SQLITE_TRACE_STMT, Some(trace_cb), keep.as_ptr() as *mut c_void));
        let old = c.trace.replace(keep);
        retire(c, old);
    }
    Ok(none())
}

// ------------------------------------------------------------------ backup

pub fn backup(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("backup", &["target", "pages", "progress", "name", "sleep"], 1, 1).kwonly(4);
    let [target, pages, progress, name, sleep] = rpy_runtime::args::parse::<5>(&P, args, kw)?;
    let c = checked(o)?;
    let target = target.unwrap();
    if !super::connection::is_connection(target) {
        raise!(TypeError, "backup() argument 'target' must be sqlite3.Connection, not {}", tname(target));
    }
    if target.is(o) {
        raise!(ValueError, "target cannot be the same connection instance");
    }
    let tc = checked(target)?;
    let mut pages = match pages {
        Some(p) => {
            let v = unsafe { capi::PyLong_AsInt(p.as_ptr()) };
            if v == -1 && err_occurred() {
                return Err(PyErr);
            }
            v
        }
        None => -1,
    };
    if pages == 0 {
        pages = -1;
    }
    let progress = progress.filter(|p| !is_none(p));
    if let Some(p) = progress {
        if !rpy_runtime::abstract_::is_callable(p) {
            raise!(TypeError, "progress argument must be a callable");
        }
    }
    let name = match name {
        Some(n) => {
            if !is_str(n) {
                raise!(TypeError, "backup() argument 'name' must be str, not {}", tname(n));
            }
            utf8_cstring(n)?
        }
        None => CString::new("main").unwrap(),
    };
    let sleep_s = match sleep {
        Some(s) => rpy_runtime::abstract_::float_value(s)?,
        None => 0.25,
    };
    if sleep_s < 0.0 {
        raise!(ValueError, "sleep must be greater-than or equal to zero");
    }
    let sleep_ms = (sleep_s * 1000.0) as c_int;
    if sq!(sqlite3_get_autocommit(tc.db)) == 0 {
        return Err(operational_error("target is in transaction"));
    }
    let main = CString::new("main").unwrap();
    let bck = sq!(sqlite3_backup_init(tc.db, main.as_ptr(), c.db, name.as_ptr()));
    if bck.is_null() {
        return Err(set_db_error(tc.db));
    }
    let mut rc;
    loop {
        rc = unsafe {
            let ts = crate::gil::save();
            let r = (lib().sqlite3_backup_step.unwrap())(bck, pages);
            crate::gil::restore(ts);
            r
        };
        if let Some(p) = progress {
            let remaining = sq!(sqlite3_backup_remaining(bck));
            let count = sq!(sqlite3_backup_pagecount(bck));
            if call(p, &[&new_int(rc as i64), &new_int(remaining as i64), &new_int(count as i64)]).is_err() {
                sq!(sqlite3_backup_finish(bck));
                return Err(PyErr);
            }
        }
        if rc == SQLITE_BUSY || rc == SQLITE_LOCKED {
            unsafe {
                let ts = crate::gil::save();
                (lib().sqlite3_sleep.unwrap())(sleep_ms);
                crate::gil::restore(ts);
            }
        }
        if !(rc == SQLITE_OK || rc == SQLITE_BUSY || rc == SQLITE_LOCKED) {
            break;
        }
    }
    let frc = sq!(sqlite3_backup_finish(bck));
    if frc != SQLITE_OK {
        return Err(set_db_error(tc.db));
    }
    let _ = conn;
    Ok(none())
}
