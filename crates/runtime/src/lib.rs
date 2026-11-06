//! rpy-runtime: the interpreter core (CPython 3.15 compatible object model and C ABI).
#![feature(c_variadic)]
#![allow(clippy::missing_safety_doc, clippy::new_without_default, static_mut_refs)]
#![allow(unused_unsafe)]
#![feature(thread_local)]

#[macro_use]
pub mod errors;
pub mod abstract_;
pub mod alloc;
pub mod args;
pub mod buffer;
pub mod builtins;
pub mod capi;
pub mod capi_ext;
pub mod compile;

pub mod ffi;
pub mod gc;
pub mod gc_support;
pub mod frozen;
pub mod hash;
pub mod import;
pub mod import_spec;
pub mod interp;
pub mod methods;
pub mod modules;
pub mod object;
pub mod qsbr;
pub mod shape;
pub mod threads;
pub mod fork;
pub mod tlab;
pub mod types;

use errors::*;
use object::PyRef;

#[derive(Default)]
pub struct Config {
    pub argv: Vec<String>,
    pub hash_seed: Option<u32>,
    pub optimize: i32,
    /// Full interpreter configuration (sys.flags, -X/-W options, ...).
    pub sys: modules::sys_full::RtConfig,
    /// sys.path fixed by an `<executable>._pth` file (no PYTHONPATH, no stdlib entry).
    pub pth_paths: Option<Vec<String>>,
}

/// Initialise the runtime: types, builtins, sys.
pub fn initialize(cfg: &Config) -> PyResult<()> {
    let mut rc = cfg.sys.clone();
    rc.optimize = rc.optimize.max(cfg.optimize);
    let ignore_env = rc.ignore_environment != 0;
    modules::sys::INT_MAX_STR_DIGITS.store(rc.int_max_str_digits, std::sync::atomic::Ordering::Relaxed);
    modules::sys::BYTES_WARNING.store(rc.bytes_warning, std::sync::atomic::Ordering::Relaxed);
    modules::sys::OPTIMIZE.store(rc.optimize, std::sync::atomic::Ordering::Relaxed);
    modules::sys_full::set_config(rc);
    hash::init_hash_secret(cfg.hash_seed);
    let _ = threads::current();
    threads::set_main_thread();
    types::int::init_small_ints();
    types::init_all()?;
    modules::register_all();
    modules::sys::init_bootstrap_types();
    let _b = builtins::init()?;
    let sys = modules::sys::create_sys_module(&cfg.argv)?;
    let md = modules::sys::modules_dict();
    types::dict::set_item_str(&md, "builtins", &builtins::builtins_module())?;
    let _ = sys;
    modules::signal::init_signals();
    // sys.path: script dir is added by the caller; then PYTHONPATH and the stdlib
    let path = modules::sys::get_sys_attr("path").unwrap();
    if let Some(pp) = &cfg.pth_paths {
        for p in pp {
            types::list::append(&path, types::str::new_str(p));
        }
    } else if !ignore_env {
        if let Ok(pp) = std::env::var("PYTHONPATH") {
            for p in pp.split(':').filter(|p| !p.is_empty()) {
                types::list::append(&path, types::str::new_str(p));
            }
        }
    }
    if cfg.pth_paths.is_none() {
        for entry in modules::sys_full::default_search_path() {
            types::list::append(&path, types::str::new_str(&entry));
        }
    }
    modules::sys_full::post_init()?;
    // real std streams (and builtins.open) must exist before site runs .pth files
    modules::sys::init_stdio();
    // -W options / PYTHONWARNINGS take effect through the warnings module's
    // filters, so it is imported right away when there are any
    if !modules::sys_full::config().warnoptions.is_empty() {
        if import::import_absolute("warnings").is_err() {
            errors::clear_exc();
        }
    }
    modules::sys_full::import_site()?;
    Ok(())
}

/// Interpreter shutdown: wait for non-daemon threads, run atexit handlers, flush streams.
pub fn finalize() -> bool {
    modules::sys_full::finalize()
}

/// Print an uncaught exception via sys.excepthook (or the built-in printer).
pub fn print_exception_and_clear() {
    let e = match take_exc() {
        Some(e) => e,
        None => return,
    };
    if let Some(hook) = modules::sys::get_sys_attr("excepthook") {
        let t = types::typeobject::type_ref(e.type_ptr());
        let tb = types::exceptions::exc_traceback(&e).unwrap_or_else(types::none::none_ref);
        if modules::sys_full::audit("sys.excepthook", &[&hook, &t, &e, &tb]).is_err() {
            // a failing audit hook does not stop the report
            errors::clear_exc();
        }
        if abstract_::call(&hook, &[&t, &e, &tb]).is_ok() {
            return;
        }
        // a broken hook: report its own failure, then the original error
        if let Some(hook_err) = take_exc() {
            modules::sys::write_stderr("Error in sys.excepthook:\n");
            modules::traceback_print::print_exception(&hook_err);
            modules::sys::write_stderr("\nOriginal exception was:\n");
        }
    }
    modules::traceback_print::print_exception(&e);
}

/// Handle SystemExit: returns the process exit code.
pub fn handle_system_exit(e: &PyRef) -> i32 {
    let code = unsafe { &*(e.as_ptr() as *const types::exceptions::SystemExitExc) }.code;
    if code.is_null() || unsafe { &*code }.is(types::none::none()) {
        return 0;
    }
    let c = unsafe { &*code };
    if types::int::is_int(c) {
        // out of the C int range: -1, as CPython's failed conversion gives
        return types::int::as_i64(c).and_then(|v| i32::try_from(v).ok()).unwrap_or(-1);
    }
    // printed through sys.stderr (its error handler deals with surrogates)
    let printed = match modules::sys::get_sys_attr("stderr") {
        Some(err) if !err.is(types::none::none()) => (|| -> errors::PyResult<()> {
            let s = abstract_::str(c)?;
            abstract_::call_method_str(&err, "write", &[&s])?;
            abstract_::call_method_str(&err, "write", &[&types::str::new_str("\n")])?;
            let _ = abstract_::call_method_str(&err, "flush", &[]);
            Ok(())
        })()
        .is_ok(),
        _ => false,
    };
    if !printed {
        errors::clear_exc();
        let s = abstract_::str_string(c).unwrap_or_default();
        modules::sys::write_stderr(&format!("{}\n", s));
    }
    errors::clear_exc();
    1
}

/// Run a script file as __main__; returns the exit code.
pub fn run_file(path: &str) -> i32 {
    let r = (|| -> PyResult<()> {
        let src = match std::fs::read(path) {
            Ok(s) => s,
            Err(e) => {
                modules::sys::write_stderr(&format!(
                    "{}: can't open file {:?}: [Errno {}] {}\n",
                    std::env::args().next().unwrap_or_default(),
                    path,
                    e.raw_os_error().unwrap_or(0),
                    e
                ));
                return Err(PyErr);
            }
        };
        let main = types::module::new_module("__main__")?;
        let d = types::module::module_dict(&main);
        types::dict::set_item_str(&d, "__file__", &types::str::new_str(path))?;
        types::dict::set_item_str(&d, "__builtins__", &builtins::builtins_module())?;
        types::dict::set_item_str(&modules::sys::modules_dict(), "__main__", &main)?;
        let code = compile::compile_bytes(&src, path, compile::Mode::Exec, 0, -1)?;
        interp::eval_code(&code.code, &d, Some(&d))?;
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(_) => {
            if !err_occurred() {
                return 2;
            }
            if exc_matches(types::exceptions::SystemExit()) {
                let e = take_exc().unwrap();
                return handle_system_exit(&e);
            }
            print_exception_and_clear();
            1
        }
    }
}

/// Run a string of code (-c).
pub fn run_string(src: &str) -> i32 {
    let r = (|| -> PyResult<()> {
        let main = types::module::new_module("__main__")?;
        let d = types::module::module_dict(&main);
        types::dict::set_item_str(&d, "__builtins__", &builtins::builtins_module())?;
        types::dict::set_item_str(&modules::sys::modules_dict(), "__main__", &main)?;
        let code = compile::compile_str(src, "<string>", compile::Mode::Exec, 0, -1)?;
        interp::eval_code(&code.code, &d, Some(&d))?;
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(_) => {
            if exc_matches(types::exceptions::SystemExit()) {
                let e = take_exc().unwrap();
                return handle_system_exit(&e);
            }
            print_exception_and_clear();
            1
        }
    }
}

/// Run module as script (-m).
pub fn run_module(name: &str) -> i32 {
    let r = (|| -> PyResult<()> {
        let runpy = import::import_absolute("runpy")?;
        let f = abstract_::getattr_str(&runpy, "_run_module_as_main")?;
        abstract_::call(&f, &[&types::str::new_str(name)])?;
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(_) => {
            if exc_matches(types::exceptions::SystemExit()) {
                let e = take_exc().unwrap();
                return handle_system_exit(&e);
            }
            print_exception_and_clear();
            1
        }
    }
}
