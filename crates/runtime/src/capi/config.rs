//! PEP 741 runtime configuration access: PyConfig_Get / GetInt / Names / Set.
//!
//! The runtime's configuration lives where Python code sees it: `sys` attributes
//! (argv, path, prefixes, ...), `sys.flags` and a few runtime settings. Every option
//! name maps to one of those sources; reading consults the live value (so code that
//! replaces e.g. `sys.argv` is observed), writing validates the value's type and
//! updates the source. Changing a flag installs a fresh `sys.flags` object and the
//! matching legacy `Py_*Flag` global.

use super::*;
use crate::abstract_ as ab;
use crate::types::{bool_, dict, int, list, none, str as pystr};
use std::sync::atomic::{AtomicI32, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Bool,
    Int,
    UInt,
    Str,
    OptStr,
    StrList,
    Dict,
}

#[derive(Clone, Copy)]
enum Src {
    /// a `sys` attribute that PyConfig_Set may replace
    Sys(&'static str),
    /// a `sys` attribute that is read-only through PyConfig_Set
    SysRo(&'static str),
    /// a `sys.flags` field: (field, negated, settable)
    Flag(&'static str, bool, bool),
    /// computed by `fixed_value`, read-only
    Fixed,
    /// computed by `fixed_value`, settable by a dedicated branch of `set`
    Special,
}

use Src::*;
use Ty::*;

const OPTIONS: &[(&str, Ty, Src)] = &[
    ("allocator", Int, Fixed),
    ("argv", StrList, Sys("argv")),
    ("base_exec_prefix", OptStr, Sys("base_exec_prefix")),
    ("base_executable", OptStr, Sys("_base_executable")),
    ("base_prefix", OptStr, Sys("base_prefix")),
    ("buffered_stdio", Bool, Fixed),
    ("bytes_warning", UInt, Flag("bytes_warning", false, true)),
    ("check_hash_pycs_mode", Str, Fixed),
    ("code_debug_ranges", Bool, Fixed),
    ("configure_c_stdio", Bool, Fixed),
    ("coerce_c_locale", Bool, Fixed),
    ("coerce_c_locale_warn", Bool, Fixed),
    ("configure_locale", Bool, Fixed),
    ("context_aware_warnings", Int, Flag("context_aware_warnings", false, false)),
    ("cpu_count", Int, Special),
    ("dev_mode", Bool, Flag("dev_mode", false, false)),
    ("dump_refs", Bool, Fixed),
    ("dump_refs_file", OptStr, Fixed),
    ("enable_gil", Int, Fixed),
    ("exec_prefix", OptStr, Sys("exec_prefix")),
    ("executable", OptStr, Sys("executable")),
    ("faulthandler", Bool, Fixed),
    ("filesystem_encoding", Str, Fixed),
    ("filesystem_errors", Str, Fixed),
    ("hash_seed", Int, Fixed),
    ("home", OptStr, Fixed),
    ("import_time", Int, Fixed),
    ("inspect", Bool, Flag("inspect", false, true)),
    ("install_signal_handlers", Bool, Fixed),
    ("int_max_str_digits", UInt, Flag("int_max_str_digits", false, true)),
    ("interactive", Bool, Flag("interactive", false, true)),
    ("isolated", Bool, Flag("isolated", false, false)),
    ("lazy_imports", Int, Fixed),
    ("malloc_stats", Bool, Fixed),
    ("module_search_paths", StrList, Sys("path")),
    ("optimization_level", UInt, Flag("optimize", false, true)),
    ("orig_argv", StrList, SysRo("orig_argv")),
    ("parse_argv", Bool, Fixed),
    ("parser_debug", Bool, Flag("debug", false, true)),
    ("pathconfig_warnings", Bool, Fixed),
    ("perf_profiling", Int, Fixed),
    ("platlibdir", Str, Sys("platlibdir")),
    ("prefix", OptStr, Sys("prefix")),
    ("program_name", Str, Fixed),
    ("pycache_prefix", OptStr, Sys("pycache_prefix")),
    ("pymalloc_hugepages", Bool, Fixed),
    ("quiet", Bool, Flag("quiet", false, true)),
    ("remote_debug", Int, Fixed),
    ("run_command", OptStr, Fixed),
    ("run_filename", OptStr, Fixed),
    ("run_module", OptStr, Fixed),
    ("safe_path", Bool, Flag("safe_path", false, false)),
    ("show_ref_count", Bool, Fixed),
    ("site_import", Bool, Flag("no_site", true, false)),
    ("skip_source_first_line", Bool, Fixed),
    ("stdio_encoding", Str, Fixed),
    ("stdio_errors", Str, Fixed),
    ("stdlib_dir", OptStr, Sys("_stdlib_dir")),
    ("thread_inherit_context", Int, Flag("thread_inherit_context", false, false)),
    ("tlbc_enabled", Int, Fixed),
    ("tracemalloc", Int, Fixed),
    ("use_environment", Bool, Flag("ignore_environment", true, true)),
    ("use_frozen_modules", Bool, Fixed),
    ("use_hash_seed", Bool, Fixed),
    ("user_site_directory", Bool, Flag("no_user_site", true, false)),
    ("utf8_mode", Bool, Flag("utf8_mode", false, false)),
    ("verbose", UInt, Flag("verbose", false, true)),
    ("warn_default_encoding", Bool, Flag("warn_default_encoding", false, false)),
    ("warnoptions", StrList, Sys("warnoptions")),
    ("write_bytecode", Bool, Flag("dont_write_bytecode", true, true)),
    ("xoptions", Dict, Sys("_xoptions")),
];

/// os.cpu_count() override set through PyConfig_Set("cpu_count") (i32::MIN: none).
static CPU_COUNT: AtomicI32 = AtomicI32::new(i32::MIN);

/// The cpu_count configuration: what os.cpu_count() reports (-1: the real count).
pub fn cpu_count_config() -> i32 {
    let v = CPU_COUNT.load(Ordering::Relaxed);
    if v != i32::MIN { v } else { crate::modules::sys_full::config().cpu_count }
}

fn find(name: &str) -> PyResult<(Ty, Src)> {
    OPTIONS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|&(_, t, s)| (t, s))
        .ok_or_else(|| value_error(&format!("unknown config option name: {}", name)))
}

fn sys_attr(a: &str) -> PyResult {
    crate::modules::sys::get_sys_attr(a).ok_or_else(|| system_error(&format!("lost sys.{}", a)))
}

fn flag(field: &str) -> PyResult {
    ab::getattr_str(&*sys_attr("flags")?, field)
}

fn env_hash_seed() -> Option<i64> {
    if crate::modules::sys_full::config().ignore_environment != 0 {
        return None;
    }
    std::env::var("PYTHONHASHSEED").ok().and_then(|v| v.trim().parse::<i64>().ok())
}

fn fixed_value(name: &str) -> PyResult {
    let c = crate::modules::sys_full::config();
    let b = |v: bool| Ok(bool_::new_bool(v));
    let i = |v: i64| Ok(int::new_int(v));
    let s = |v: &str| Ok(pystr::new_str(v));
    match name {
        "allocator" | "perf_profiling" | "remote_debug" | "tlbc_enabled" => i(0),
        "buffered_stdio" => b(!c.unbuffered),
        "check_hash_pycs_mode" => s(&c.check_hash_based_pycs),
        "code_debug_ranges" => {
            let xo = sys_attr("_xoptions")?;
            let off = dict::get_item_rs(&xo, "no_debug_ranges").is_some()
                || (c.ignore_environment == 0 && std::env::var_os("PYTHONNODEBUGRANGES").is_some_and(|v| !v.is_empty()));
            b(!off)
        }
        "configure_c_stdio" | "configure_locale" | "install_signal_handlers" | "parse_argv" | "pathconfig_warnings" | "use_frozen_modules" => {
            b(true)
        }
        "coerce_c_locale" | "coerce_c_locale_warn" | "dump_refs" | "malloc_stats" | "pymalloc_hugepages" | "show_ref_count" | "skip_source_first_line" => {
            b(false)
        }
        "cpu_count" => i(cpu_count_config() as i64),
        "dump_refs_file" | "home" | "run_command" | "run_filename" | "run_module" => Ok(none::none_ref()),
        "enable_gil" => i(c.gil as i64),
        "faulthandler" => {
            if let Some(m) = crate::import::get_loaded_module("faulthandler") {
                if let Ok(f) = ab::getattr_str(&m, "is_enabled") {
                    return b(ab::is_true(&*ab::call(&f, &[])?)?);
                }
            }
            b(c.faulthandler)
        }
        "filesystem_encoding" => s("utf-8"),
        "filesystem_errors" => s("surrogateescape"),
        "hash_seed" => i(if c.hash_randomization == 0 { 0 } else { env_hash_seed().unwrap_or(0) }),
        "use_hash_seed" => b(c.hash_randomization == 0 || env_hash_seed().is_some()),
        "import_time" => i(c.import_time as i64),
        "lazy_imports" => i(c.lazy_imports as i64),
        "program_name" => {
            let argv0 = c.orig_argv.first().cloned().unwrap_or_else(|| "python3".into());
            s(&argv0)
        }
        "stdio_encoding" => s("utf-8"),
        "stdio_errors" => s("strict"),
        "tracemalloc" => i(c.tracemalloc as i64),
        _ => Err(value_error(&format!("unknown config option name: {}", name))),
    }
}

pub(crate) fn config_get(name: &str) -> PyResult {
    get(name)
}

fn get(name: &str) -> PyResult {
    let (ty, src) = find(name)?;
    match src {
        Sys(a) | SysRo(a) => sys_attr(a),
        Flag(field, negate, _) => {
            // write_bytecode follows sys.dont_write_bytecode, which code may replace
            let v = if field == "dont_write_bytecode" { sys_attr("dont_write_bytecode")? } else { flag(field)? };
            match ty {
                Bool => {
                    let t = ab::is_true(&v)?;
                    Ok(bool_::new_bool(t != negate))
                }
                _ => {
                    if v.is(none::none()) {
                        return Ok(int::new_int(-1));
                    }
                    ab::index(&v)
                }
            }
        }
        Fixed | Special => fixed_value(name),
    }
}

/// The legacy global variable shadowing a settable flag option.
fn legacy_global(name: &str) -> Option<*mut c_int> {
    use super::data::*;
    unsafe {
        Some(match name {
            "bytes_warning" => &raw mut Py_BytesWarningFlag,
            "inspect" => &raw mut Py_InspectFlag,
            "interactive" => &raw mut Py_InteractiveFlag,
            "optimization_level" => &raw mut Py_OptimizeFlag,
            "parser_debug" => &raw mut Py_DebugFlag,
            "quiet" => &raw mut Py_QuietFlag,
            "use_environment" => &raw mut Py_IgnoreEnvironmentFlag,
            "verbose" => &raw mut Py_VerboseFlag,
            "write_bytecode" => &raw mut Py_DontWriteBytecodeFlag,
            _ => return None,
        })
    }
}

fn check_type(name: &str, ty: Ty, v: &PyObject) -> PyResult<PyRef> {
    let bad = |what: &str| type_error(&format!("{} must be {}, not {}", name, what, crate::types::typeobject::tname(v)));
    match ty {
        Str => {
            if !pystr::is_str(v) {
                return Err(bad("str"));
            }
            Ok(v.to_ref())
        }
        OptStr => {
            if !pystr::is_str(v) && !v.is(none::none()) {
                return Err(bad("str or None"));
            }
            Ok(v.to_ref())
        }
        StrList => {
            if !list::is_list(v) {
                return Err(bad("list"));
            }
            let items = list::list_items(v);
            if items.iter().any(|x| !pystr::is_str(x)) {
                return Err(type_error(&format!("{} items must be str", name)));
            }
            // the object itself becomes the sys attribute (no copy)
            Ok(v.to_ref())
        }
        Dict => {
            if !dict::is_dict(v) {
                return Err(bad("dict"));
            }
            for (k, x) in dict::items_vec(v) {
                if !pystr::is_str(&k) {
                    return Err(type_error(&format!("{} keys must be str", name)));
                }
                if !pystr::is_str(&x) && !bool_::is_bool(&x) {
                    return Err(type_error(&format!("{} values must be str or bool", name)));
                }
            }
            Ok(v.to_ref())
        }
        Bool | Int | UInt => {
            if !int::is_int(v) {
                return Err(bad("int"));
            }
            Ok(v.to_ref())
        }
    }
}

fn set(name: &str, v: &PyObject) -> PyResult<()> {
    let (ty, src) = find(name)?;
    let read_only = || value_error(&format!("cannot set read-only option {:?}", name));
    match src {
        Sys(a) => {
            let v = check_type(name, ty, v)?;
            crate::modules::sys::set_sys_attr(a, &v)
        }
        SysRo(_) | Fixed | Flag(_, _, false) => Err(read_only()),
        Special => {
            // cpu_count
            check_type(name, Int, v)?;
            let n = int::as_i64(v).ok_or_else(|| overflow_error("cpu_count out of range"))?;
            if n < -1 || n > i32::MAX as i64 || n == 0 {
                return Err(value_error("cpu_count must be -1 or a positive number"));
            }
            CPU_COUNT.store(n as i32, Ordering::Relaxed);
            Ok(())
        }
        Flag(field, negate, true) => {
            check_type(name, ty, v)?;
            let raw = int::as_i64(v).ok_or_else(|| overflow_error(&format!("{} out of range", name)))?;
            if raw < 0 && ty != Int {
                return Err(value_error(&format!("{} must be >= 0", name)));
            }
            let raw = raw.min(i32::MAX as i64);
            let stored: i64 = match ty {
                Bool => {
                    let b = raw != 0;
                    (b != negate) as i64
                }
                _ => raw,
            };
            if field == "int_max_str_digits" {
                if stored != 0 && stored < 4300 {
                    return Err(value_error("int_max_str_digits: must be 0 or >= 4300"));
                }
                let sys = crate::import::import_absolute("sys")?;
                ab::call(&*ab::getattr_str(&sys, "set_int_max_str_digits")?, &[&int::new_int(stored)])?;
            }
            crate::modules::sys_full::replace_flag(field, &int::new_int(stored))?;
            if field == "dont_write_bytecode" {
                crate::modules::sys::set_sys_attr("dont_write_bytecode", &bool_::new_bool(stored != 0))?;
            }
            if let Some(g) = legacy_global(name) {
                // the legacy variable keeps the value as given (negated options store
                // the negation of its truth value)
                let gv = if negate { stored } else { raw };
                unsafe { *g = gv as c_int };
            }
            Ok(())
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_Get(name: *const c_char) -> *mut PyObject {
    unsafe { ret(get(&cs(name))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_GetInt(name: *const c_char, value: *mut c_int) -> c_int {
    unsafe {
        let r = (|| -> PyResult<c_int> {
            let n = cs(name);
            let v = get(&n)?;
            if !int::is_int(&v) {
                return Err(type_error(&format!("config option {} is not an int", n)));
            }
            let x = int::as_i64(&v).ok_or_else(|| overflow_error(&format!("config option {} value does not fit in a C int", n)))?;
            c_int::try_from(x).map_err(|_| overflow_error(&format!("config option {} value does not fit in a C int", n)))
        })();
        match r {
            Ok(v) => {
                *value = v;
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_Names() -> *mut PyObject {
    ret(crate::types::set::new_frozenset_from_vec(OPTIONS.iter().map(|(n, _, _)| pystr::new_str(n)).collect()))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_Set(name: *const c_char, value: *mut PyObject) -> c_int {
    unsafe {
        if value.is_null() {
            let _ = bad_internal();
            return -1;
        }
        ret_i(set(&cs(name), &*value))
    }
}

/// The legacy global configuration variables (for `_testinternalcapi.get_configs`).
pub fn legacy_globals() -> Vec<(&'static str, i32)> {
    use super::data::*;
    unsafe {
        vec![
            ("Py_BytesWarningFlag", Py_BytesWarningFlag),
            ("Py_DebugFlag", Py_DebugFlag),
            ("Py_DontWriteBytecodeFlag", Py_DontWriteBytecodeFlag),
            ("Py_FrozenFlag", Py_FrozenFlag),
            ("Py_HashRandomizationFlag", Py_HashRandomizationFlag),
            ("Py_IgnoreEnvironmentFlag", Py_IgnoreEnvironmentFlag),
            ("Py_InspectFlag", Py_InspectFlag),
            ("Py_InteractiveFlag", Py_InteractiveFlag),
            ("Py_IsolatedFlag", Py_IsolatedFlag),
            ("Py_NoSiteFlag", Py_NoSiteFlag),
            ("Py_NoUserSiteDirectory", Py_NoUserSiteDirectory),
            ("Py_OptimizeFlag", Py_OptimizeFlag),
            ("Py_QuietFlag", Py_QuietFlag),
            ("Py_UTF8Mode", Py_UTF8Mode),
            ("Py_UnbufferedStdioFlag", Py_UnbufferedStdioFlag),
            ("Py_VerboseFlag", Py_VerboseFlag),
        ]
    }
}

/// Copy the startup configuration into the legacy global variables.
pub fn init_legacy_globals() {
    use super::data::*;
    let c = crate::modules::sys_full::config();
    unsafe {
        Py_BytesWarningFlag = c.bytes_warning;
        Py_DebugFlag = c.debug;
        Py_DontWriteBytecodeFlag = c.dont_write_bytecode;
        Py_HashRandomizationFlag = c.hash_randomization;
        Py_IgnoreEnvironmentFlag = c.ignore_environment;
        Py_InspectFlag = c.inspect;
        Py_InteractiveFlag = c.interactive;
        Py_IsolatedFlag = c.isolated;
        Py_NoSiteFlag = c.no_site;
        Py_NoUserSiteDirectory = c.no_user_site;
        Py_OptimizeFlag = c.optimize;
        Py_QuietFlag = c.quiet;
        Py_UTF8Mode = c.utf8_mode;
        Py_UnbufferedStdioFlag = c.unbuffered as i32;
        Py_VerboseFlag = c.verbose;
    }
}
