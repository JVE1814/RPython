//! The `_imp` module: low-level import support used by importlib (builtin and
//! extension module creation, the global import lock, frozen-module queries,
//! pyc helpers) plus a few private file-system helpers used by importlib before
//! a `posix` module is available.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, bytes, dict, float, int, list, none, str as pystr, tuple};
use std::sync::atomic::{AtomicI32, Ordering::*};

pub const EXT_SUFFIX: &str = ".cpython-315t-x86_64-linux-gnu.so";

// ------------------------------------------------------------------ import lock (reentrant)

static LOCK: crate::modules::_thread::RawLock = crate::modules::_thread::RawLock::new();
static LOCK_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOCK_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn me() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

pub fn acquire_import_lock() -> PyResult<()> {
    let m = me();
    if LOCK_COUNT.load(Acquire) > 0 && LOCK_OWNER.load(Acquire) == m {
        LOCK_COUNT.fetch_add(1, AcqRel);
        return Ok(());
    }
    loop {
        match LOCK.acquire_timed(None) {
            Ok(_) => break,
            Err(_) => {
                // a signal handler raised while waiting: keep waiting, report later
                clear_exc();
            }
        }
    }
    LOCK_OWNER.store(m, Release);
    LOCK_COUNT.store(1, Release);
    Ok(())
}

pub fn release_import_lock() -> PyResult<()> {
    if LOCK_COUNT.load(Acquire) == 0 || LOCK_OWNER.load(Acquire) != me() {
        raise!(RuntimeError, "not holding the import lock");
    }
    if LOCK_COUNT.fetch_sub(1, AcqRel) == 1 {
        LOCK_OWNER.store(0, Release);
        LOCK.release();
    }
    Ok(())
}

/// Reset the import lock in a forked child.
pub fn after_fork_child() {
    if LOCK_COUNT.load(Relaxed) > 0 && LOCK_OWNER.load(Relaxed) != me() {
        LOCK_COUNT.store(0, Relaxed);
        LOCK_OWNER.store(0, Relaxed);
        LOCK.state.store(0, Relaxed);
    }
}

fn imp_acquire_lock(_m: &PyObject) -> PyResult {
    acquire_import_lock()?;
    Ok(none::none_ref())
}
fn imp_release_lock(_m: &PyObject) -> PyResult {
    release_import_lock()?;
    Ok(none::none_ref())
}
fn imp_lock_held(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(LOCK_COUNT.load(Acquire) > 0))
}

// ------------------------------------------------------------------ builtins

fn name_arg(o: &PyObject) -> PyResult<String> {
    if !pystr::is_str(o) {
        raise!(TypeError, "name must be a str, not {}", crate::types::typeobject::tname(o));
    }
    Ok(pystr::to_string_lossy(o))
}

fn imp_is_builtin(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = name_arg(n)?;
    let is = crate::import::find_builtin(&n).is_some() || n == "sys" || n == "builtins";
    Ok(int::new_int(if is { 1 } else { 0 }))
}

fn imp_create_builtin(_m: &PyObject, spec: &PyObject) -> PyResult {
    let n = ab::getattr_str(spec, "name")?;
    if !pystr::is_str(&n) {
        raise!(TypeError, "name must be string, not {}", crate::types::typeobject::tname(&n));
    }
    let name = pystr::to_string_lossy(&n);
    match name.as_str() {
        "sys" => return Ok(crate::modules::sys::sys_module()),
        "builtins" => return Ok(crate::builtins::builtins_module()),
        _ => {}
    }
    if let Some(m) = crate::import::get_loaded_module(&name) {
        if crate::import::is_builtin_module_object(&m, &name) {
            return Ok(m);
        }
    }
    match crate::import::find_builtin(&name) {
        Some(init) => {
            let m = init()?;
            crate::import::note_builtin_module(&name, &m);
            Ok(m)
        }
        None => Err(crate::import::import_error_with(&format!("no built-in module named {}", name), Some(&n), None, None)),
    }
}
fn imp_exec_builtin(_m: &PyObject, _module: &PyObject) -> PyResult {
    Ok(int::new_int(0))
}

// ------------------------------------------------------------------ frozen (none)

fn imp_is_frozen(_m: &PyObject, n: &PyObject) -> PyResult {
    name_arg(n)?;
    Ok(bool_::new_bool(false))
}
fn imp_find_frozen(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("find_frozen", &["name", "withdata"], 1, 1).kwonly(1);
    let [n, _w] = crate::args::parse::<2>(&P, args, kw)?;
    name_arg(n.unwrap())?;
    Ok(none::none_ref())
}
fn no_frozen(n: &PyObject) -> PyErr {
    let s = ab::repr_string(n).unwrap_or_default();
    crate::import::import_error_with(&format!("No such frozen object named {}", s), Some(n), None, None)
}
fn imp_get_frozen_object(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_frozen_object", args, 1, 2)?;
    name_arg(args[0])?;
    Err(no_frozen(args[0]))
}
fn imp_init_frozen(_m: &PyObject, n: &PyObject) -> PyResult {
    name_arg(n)?;
    Ok(none::none_ref())
}
fn imp_is_frozen_package(_m: &PyObject, n: &PyObject) -> PyResult {
    name_arg(n)?;
    Err(no_frozen(n))
}
fn imp_frozen_module_names(_m: &PyObject) -> PyResult {
    Ok(tuple::empty_tuple())
}
static FROZEN_OVERRIDE: AtomicI32 = AtomicI32::new(0);
fn imp_override_frozen(_m: &PyObject, v: &PyObject) -> PyResult {
    let n = ab::as_i64(v)? as i32;
    FROZEN_OVERRIDE.store(n, Relaxed);
    Ok(none::none_ref())
}
static MULTI_INTERP_OVERRIDE: AtomicI32 = AtomicI32::new(-1);
fn imp_override_multi_interp(_m: &PyObject, v: &PyObject) -> PyResult {
    let n = ab::as_i64(v)? as i32;
    Ok(int::new_int(MULTI_INTERP_OVERRIDE.swap(n, AcqRel) as i64))
}

// ------------------------------------------------------------------ extensions

pub fn extension_suffixes() -> Vec<&'static str> {
    vec![EXT_SUFFIX, ".abi3.so", ".so"]
}
fn imp_extension_suffixes(_m: &PyObject) -> PyResult {
    Ok(list::new_list(extension_suffixes().into_iter().map(pystr::new_str).collect()))
}
fn imp_create_dynamic(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("create_dynamic", args, 1, 2)?;
    let spec = args[0];
    let n = ab::getattr_str(spec, "name")?;
    let name = pystr::to_string_lossy(&n);
    if let Some(m) = crate::import::get_loaded_module(&name) {
        if crate::import::is_builtin_module_object(&m, &name) {
            return Ok(m);
        }
    }
    let origin = ab::getattr_str(spec, "origin")?;
    let path = crate::modules::os_fspath::fsdecode(&origin)?;
    crate::modules::sys_full::audit("import", &[&n, &origin, none::none(), none::none(), none::none()])?;
    crate::capi_ext::load_extension(&name, std::path::Path::new(&path))
}
fn imp_exec_dynamic(_m: &PyObject, _module: &PyObject) -> PyResult {
    Ok(int::new_int(0))
}

// ------------------------------------------------------------------ pyc helpers

fn imp_fix_co_filename(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_fix_co_filename", args, 2, 2)?;
    if !crate::types::code::is_code(args[0]) {
        raise!(TypeError, "_fix_co_filename() argument 1 must be code, not {}", crate::types::typeobject::tname(args[0]));
    }
    if !pystr::is_str(args[1]) {
        raise!(TypeError, "_fix_co_filename() argument 2 must be str, not {}", crate::types::typeobject::tname(args[1]));
    }
    Ok(none::none_ref())
}

fn imp_source_hash(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("source_hash", &["key", "source"], 0, 2).kwonly(0);
    let [k, s] = crate::args::parse::<2>(&P, args, kw)?;
    let key = ab::as_i128_index(k.unwrap())? as u64;
    let data = match crate::buffer::buffer_bytes(s.unwrap()) {
        Some(d) => d,
        None => raise!(TypeError, "a bytes-like object is required, not {}", crate::types::typeobject::tname(s.unwrap())),
    };
    let h = crate::hash::siphash13(key, 0, &data);
    Ok(bytes::new_bytes(&h.to_le_bytes()))
}

// ------------------------------------------------------------------ private fs helpers

fn path_arg(o: &PyObject) -> PyResult<String> {
    crate::modules::os_fspath::fsdecode(o)
}
fn os_err(e: std::io::Error, p: &str) -> PyErr {
    crate::types::exceptions::os_error_from_errno(e.raw_os_error().unwrap_or(libc::EIO), Some(&pystr::new_str(p)), None)
}
fn imp_rpy_stat(_m: &PyObject, p: &PyObject) -> PyResult {
    use std::os::unix::fs::MetadataExt;
    let path = path_arg(p)?;
    let md = std::fs::metadata(&path).map_err(|e| os_err(e, &path))?;
    let mtime = md.mtime() as f64 + md.mtime_nsec() as f64 * 1e-9;
    Ok(tuple::new_tuple(vec![int::new_int(md.mode() as i64), int::new_int(md.size() as i64), float::new_float(mtime)]))
}
fn imp_rpy_listdir(_m: &PyObject, p: &PyObject) -> PyResult {
    let path = path_arg(p)?;
    let rd = std::fs::read_dir(&path).map_err(|e| os_err(e, &path))?;
    let mut v = Vec::new();
    for ent in rd.flatten() {
        use std::os::unix::ffi::OsStrExt;
        v.push(crate::modules::_thread::decode_surrogateescape(ent.file_name().as_bytes()));
    }
    Ok(list::new_list(v))
}
fn imp_rpy_getcwd(_m: &PyObject) -> PyResult {
    let d = std::env::current_dir().map_err(|e| os_err(e, "."))?;
    use std::os::unix::ffi::OsStrExt;
    Ok(crate::modules::_thread::decode_surrogateescape(d.as_os_str().as_bytes()))
}
fn imp_rpy_read_file(_m: &PyObject, p: &PyObject) -> PyResult {
    let path = path_arg(p)?;
    let data = crate::threads::blocking(|| std::fs::read(&path)).map_err(|e| os_err(e, &path))?;
    Ok(bytes::new_bytes(&data))
}

/// pyc magic: 3690..3699 (the top of the 3.15 range, 2900+50*15, unlikely to collide
/// with CPython's own) selected by a hash of the compiler sources (stale caches written
/// by an older compiler are never reused), then "\r\n".
pub const MAGIC: u32 = (3690 + rpy_compiler::BYTECODE_HASH % 10) | ((b'\r' as u32) << 16) | ((b'\n' as u32) << 24);

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_imp")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("acquire_lock", NOARGS, imp_acquire_lock),
            ("release_lock", NOARGS, imp_release_lock),
            ("lock_held", NOARGS, imp_lock_held),
            ("is_builtin", O, imp_is_builtin),
            ("create_builtin", O, imp_create_builtin),
            ("exec_builtin", O, imp_exec_builtin),
            ("is_frozen", O, imp_is_frozen),
            ("find_frozen", FASTKW, imp_find_frozen),
            ("get_frozen_object", FAST, imp_get_frozen_object),
            ("init_frozen", O, imp_init_frozen),
            ("is_frozen_package", O, imp_is_frozen_package),
            ("_frozen_module_names", NOARGS, imp_frozen_module_names),
            ("_override_frozen_modules_for_tests", O, imp_override_frozen),
            ("_override_multi_interp_extensions_check", O, imp_override_multi_interp),
            ("extension_suffixes", NOARGS, imp_extension_suffixes),
            ("create_dynamic", FAST, imp_create_dynamic),
            ("exec_dynamic", O, imp_exec_dynamic),
            ("_fix_co_filename", FAST, imp_fix_co_filename),
            ("source_hash", FASTKW, imp_source_hash),
            ("_rpy_stat", O, imp_rpy_stat),
            ("_rpy_listdir", O, imp_rpy_listdir),
            ("_rpy_getcwd", NOARGS, imp_rpy_getcwd),
            ("_rpy_read_file", O, imp_rpy_read_file),
        ],
        Some(&m),
    )?;
    let chk = crate::modules::sys_full::config().check_hash_based_pycs.clone();
    dict::set_item_str(&d, "check_hash_based_pycs", &pystr::new_str(&chk))?;
    dict::set_item_str(&d, "pyc_magic_number_token", &int::new_int(if crate::types::code::public_mode() { (MAGIC + 50) as i64 } else { MAGIC as i64 }))?;
    Ok(m)
}
