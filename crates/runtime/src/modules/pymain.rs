//! Running the `__main__` program: -c / -m / script / stdin / interactive REPL,
//! exit-status computation, PYTHONSTARTUP.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{dict, list, module, none, str as pystr};

pub enum RunMode {
    Command(String),
    Module(String),
    Script(String),
    /// Read the program from stdin (non-interactive).
    Stdin,
    /// Interactive loop.
    Repl,
}

pub struct MainOptions {
    pub mode: RunMode,
    /// -i / PYTHONINSPECT: enter the REPL after running the program.
    pub inspect: bool,
    /// -x: skip the first source line.
    pub skip_first_line: bool,
    pub quiet: bool,
    pub safe_path: bool,
    pub program_name: String,
}

/// Outcome of running a piece of code.
enum Outcome {
    Ok,
    Exit(i32),
}

fn main_module() -> PyResult {
    if let Some(m) = crate::import::get_loaded_module("__main__") {
        return Ok(m);
    }
    let m = module::new_module("__main__")?;
    let d = module::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    // like CPython, __main__ starts out with the built-in importer as its
    // loader (a script run later replaces it) (grind-lib)
    if let Some(bi) = crate::import::get_loaded_module("_frozen_importlib")
        .or_else(|| crate::import::get_loaded_module("importlib._bootstrap"))
        .and_then(|m| ab::getattr_str(&m, "BuiltinImporter").ok())
    {
        dict::set_item_str(&d, "__loader__", &bi)?;
    }
    clear_exc();
    dict::set_item_str(&d, "__annotations__", &dict::new_dict()).ok();
    dict::del_item(&d, &pystr::new_str("__annotations__")).ok();
    dict::set_item_str(&crate::modules::sys::modules_dict(), "__main__", &m)?;
    Ok(m)
}

/// Report the pending exception like CPython's main and compute the exit status.
fn handle_error() -> i32 {
    if !err_occurred() {
        return 1;
    }
    if exc_matches(crate::types::exceptions::SystemExit()) {
        let e = take_exc().unwrap();
        return crate::handle_system_exit(&e);
    }
    let is_kbd = exc_matches(crate::types::exceptions::KeyboardInterrupt());
    crate::print_exception_and_clear();
    if is_kbd {
        return -2; // caller re-raises SIGINT after finalisation
    }
    1
}

fn run_code_in_main(src: &[u8], filename: &str) -> Outcome {
    let r = (|| -> PyResult<()> {
        let m = main_module()?;
        let d = module::module_dict(&m);
        // (capi2) a script file may not contain NUL bytes (reported at the line holding the
        // first one, its text cut at the NUL)
        if filename != "<string>" {
            if let Some(pos) = src.iter().position(|&b| b == 0) {
                let start = src[..pos].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                let lineno = src[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
                let text = String::from_utf8_lossy(&src[start..pos]).into_owned();
                return Err(crate::compile::raise_syntax_error(
                    crate::types::exceptions::SyntaxError(),
                    "source code cannot contain null bytes",
                    filename,
                    lineno as i32,
                    -1,
                    Some(&text),
                    None,
                    None,
                ));
            }
        }
        // `-c` code is text already: a coding cookie in it is not obeyed
        let code = match std::str::from_utf8(src) {
            Ok(text) if filename == "<string>" => crate::compile::compile_str(text, filename, crate::compile::Mode::Exec, 0, -1)?,
            _ => {
                // (capi2) compile-time warnings of the main script belong to module __main__
                let main_name = pystr::new_str("__main__");
                let mut o = crate::compile::Opts::new(filename, crate::compile::Mode::Exec, 0, -1);
                o.module = Some(&main_name);
                crate::compile::compile_source(crate::compile::Source::Bytes(src), &o)?
            }
        };
        if filename == "<string>" {
            // `-c` code: tracebacks show its lines like those of a file
            register_source(&code.code, &String::from_utf8_lossy(src), "<string>");
        }
        crate::interp::eval_code(&code.code, &d, Some(&d))?;
        Ok(())
    })();
    match r {
        Ok(()) => Outcome::Ok,
        Err(_) => Outcome::Exit(handle_error()),
    }
}

/// The magic number importlib writes into .pyc headers.
fn pyc_magic() -> [u8; 4] {
    let token = crate::import::import_absolute("_imp")
        .and_then(|m| ab::getattr_str(&m, "pyc_magic_number_token"))
        .and_then(|t| ab::as_i64(&t));
    match token {
        Ok(t) => (t as u32).to_le_bytes(),
        Err(_) => {
            clear_exc();
            crate::modules::_imp::MAGIC.to_le_bytes()
        }
    }
}

/// Run a compiled script (the marshalled code after a .pyc header): the code
/// keeps the file name it was compiled with.
fn run_pyc_in_main(data: &[u8], path: &str) -> Outcome {
    let r = (|| -> PyResult<()> {
        let m = main_module()?;
        let d = module::module_dict(&m);
        dict::set_item_str(&d, "__file__", &pystr::new_str(path))?;
        dict::set_item_str(&d, "__cached__", none::none())?;
        if let Some(bs) = crate::import::get_loaded_module("_frozen_importlib_external") {
            let cls = ab::getattr_str(&bs, "SourcelessFileLoader")?;
            let l = ab::call(&cls, &[&pystr::new_str("__main__"), &pystr::new_str(path)])?;
            dict::set_item_str(&d, "__loader__", &l)?;
        }
        let code = crate::modules::marshal::loads_slice(data, true)?;
        if !crate::types::code::is_code(&code) {
            crate::raise!(RuntimeError, "Bad code object in .pyc file");
        }
        crate::interp::eval_code(&code, &d, Some(&d))?;
        Ok(())
    })();
    match r {
        Ok(()) => Outcome::Ok,
        Err(_) => Outcome::Exit(handle_error()),
    }
}

fn abspath(p: &str) -> String {
    let pb = std::path::Path::new(p);
    if pb.is_absolute() {
        return p.to_string();
    }
    match std::env::current_dir() {
        Ok(c) => c.join(pb).to_string_lossy().into_owned(),
        Err(_) => p.to_string(),
    }
}

/// A ZIP archive (possibly with data prepended, e.g. a launcher stub) has its
/// end-of-central-directory record within the last 64 KiB + 22 bytes.
fn looks_like_zip(path: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let Ok(len) = f.seek(SeekFrom::End(0)) else { return false };
    let start = len.saturating_sub(65535 + 22);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut tail = Vec::new();
    if f.read_to_end(&mut tail).is_err() {
        return false;
    }
    tail.windows(4).any(|w| w == b"PK\x05\x06")
}

/// A path that does not exist on disk but names a directory inside a zip archive
/// (`archive.zip/package/sub`): zipimport can import `__main__` from there.
fn inside_zip(path: &str) -> bool {
    let mut p = std::path::Path::new(path);
    while let Some(parent) = p.parent() {
        if parent.as_os_str().is_empty() {
            return false;
        }
        if let Ok(m) = std::fs::metadata(parent) {
            return m.is_file() && looks_like_zip(&parent.to_string_lossy());
        }
        p = parent;
    }
    false
}

fn insert_path0(p: &str) {
    if let Some(path) = crate::modules::sys::get_sys_attr("path") {
        list::insert(&path, 0, pystr::new_str(p));
    }
    // remembered for the "consider renaming" hints (sys.path may change later)
    crate::import::set_sys_path_0(p);
}

fn run_script(path: &str, opts: &MainOptions) -> Outcome {
    let meta = std::fs::metadata(path);
    // a directory or zip file with __main__.py: run it as a module
    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    let is_zip = !is_dir && (looks_like_zip(path) || (meta.is_err() && inside_zip(path)));
    if is_dir || is_zip {
        // The directory or archive is where __main__ comes from, so it is
        // put on sys.path even in safe-path / isolated mode.
        insert_path0(path);
        let r = (|| -> PyResult<()> {
            main_module()?;
            let runpy = crate::import::import_absolute("runpy")?;
            let f = ab::getattr_str(&runpy, "_run_module_as_main")?;
            ab::call(&f, &[&pystr::new_str("__main__"), crate::types::bool_::false_obj()])?;
            Ok(())
        })();
        return match r {
            Ok(()) => Outcome::Ok,
            Err(_) => Outcome::Exit(handle_error()),
        };
    }
    if !opts.safe_path {
        let real = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| abspath(path));
        let dir = std::path::Path::new(&real).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        insert_path0(&dir);
    }
    let mut src = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            crate::modules::sys::write_stderr(&format!(
                "{}: can't open file {}: [Errno {}] {}\n",
                opts.program_name,
                pyrepr_str(&abspath(path)),
                e.raw_os_error().unwrap_or(0),
                strerror(e.raw_os_error().unwrap_or(0))
            ));
            return Outcome::Exit(2);
        }
    };
    if opts.skip_first_line {
        match src.iter().position(|&b| b == b'\n') {
            Some(i) => {
                // keep the newline so line numbers stay correct
                src.drain(..i);
            }
            None => src.clear(),
        }
    }
    // the script path is made absolute (like CPython 3.9+): __file__ and code filenames
    let abs = abspath(path);
    let path = abs.as_str();
    if src.len() >= 16 && src[..4] == pyc_magic() {
        return run_pyc_in_main(&src[16..], path);
    }
    let r = (|| -> PyResult<()> {
        let m = main_module()?;
        let d = module::module_dict(&m);
        dict::set_item_str(&d, "__file__", &pystr::new_str(path))?;
        dict::set_item_str(&d, "__cached__", none::none())?;
        crate::import_spec::set_main_loader(&d, path)?;
        Ok(())
    })();
    if r.is_err() {
        return Outcome::Exit(handle_error());
    }
    run_code_in_main(&src, path)
}

fn strerror(e: i32) -> String {
    unsafe { std::ffi::CStr::from_ptr(libc::strerror(e)).to_string_lossy().into_owned() }
}

fn pyrepr_str(s: &str) -> String {
    match ab::repr_string(&pystr::new_str(s)) {
        Ok(r) => r,
        Err(_) => {
            clear_exc();
            format!("'{}'", s)
        }
    }
}

fn run_module(name: &str) -> Outcome {
    let r = (|| -> PyResult<()> {
        // like CPython, __main__ exists before runpy replaces it
        main_module()?;
        let runpy = crate::import::import_absolute("runpy")?;
        let f = ab::getattr_str(&runpy, "_run_module_as_main")?;
        ab::call(&f, &[&pystr::new_str(name)])?;
        Ok(())
    })();
    match r {
        Ok(()) => Outcome::Ok,
        Err(_) => Outcome::Exit(handle_error()),
    }
}

fn read_stdin_all() -> Vec<u8> {
    use std::io::Read;
    let mut v = Vec::new();
    let _ = std::io::stdin().lock().read_to_end(&mut v);
    v
}

fn run_startup_file() {
    let c = crate::modules::sys_full::config();
    if c.ignore_environment != 0 || c.isolated != 0 {
        return;
    }
    if let Ok(p) = std::env::var("PYTHONSTARTUP") {
        if p.is_empty() {
            return;
        }
        match std::fs::read(&p) {
            Ok(src) => {
                if let Outcome::Exit(_) = run_code_in_main(&src, &p) {}
            }
            Err(e) => {
                crate::modules::sys::write_stderr(&format!(
                    "Could not open PYTHONSTARTUP\nFileNotFoundError: [Errno {}] {}: {}\n",
                    e.raw_os_error().unwrap_or(0),
                    strerror(e.raw_os_error().unwrap_or(0)),
                    pyrepr_str(&p)
                ));
            }
        }
    }
}

fn get_prompt(name: &str, default: &str) -> String {
    match crate::modules::sys::get_sys_attr(name) {
        Some(p) => ab::str_string(&p).unwrap_or_else(|_| {
            clear_exc();
            default.to_string()
        }),
        None => default.to_string(),
    }
}

fn write_prompt(p: &str) {
    if let Some(e) = crate::modules::sys::get_sys_attr("stderr") {
        if !e.is(none::none()) {
            if ab::call_method_str(&e, "write", &[&pystr::new_str(p)]).is_ok() {
                let _ = ab::call_method_str(&e, "flush", &[]);
                clear_exc();
                return;
            }
            clear_exc();
        }
    }
    crate::modules::sys::write_fd(2, p.as_bytes());
}

fn is_incomplete_error() -> bool {
    if !exc_matches(crate::types::exceptions::SyntaxError()) {
        return false;
    }
    let e = take_exc().unwrap();
    let msg = ab::getattr_str(&e, "msg").ok().map(|m| ab::str_string(&m).unwrap_or_default()).unwrap_or_default();
    clear_exc();
    let inc = msg.contains("incomplete input") || msg.contains("unexpected EOF") || msg.contains("was never closed")
        || msg.contains("expected an indented block") || msg.contains("unterminated triple-quoted string");
    set_exc(e);
    inc
}

/// Is the pending SyntaxError about a bracket or triple-quoted string that the
/// input so far leaves open?
fn still_open_error() -> bool {
    if !exc_matches(crate::types::exceptions::SyntaxError()) {
        return false;
    }
    let e = take_exc().unwrap();
    let msg = ab::getattr_str(&e, "msg").ok().map(|m| ab::str_string(&m).unwrap_or_default()).unwrap_or_default();
    clear_exc();
    set_exc(e);
    msg.contains("was never closed") || msg.contains("unterminated triple-quoted string")
}

/// Call sys.__interactivehook__ (installed by site: readline completion and history).
fn run_interactive_hook() {
    if let Some(h) = crate::modules::sys::get_sys_attr("__interactivehook__") {
        if ab::call(&h, &[]).is_err() {
            crate::modules::sys::write_stderr("Error in sys.__interactivehook__\n");
            crate::print_exception_and_clear();
        }
    }
}

/// Read one line through the PyOS_Readline hook (GNU readline when the
/// readline module was imported).  None: no hook installed.
fn readline_line(prompt: &str) -> Option<Result<String, ()>> {
    unsafe extern "C" {
        static mut stdin: *mut libc::FILE;
        static mut stdout: *mut libc::FILE;
    }
    if unsafe { (*(&raw const crate::capi::misc::PyOS_ReadlineFunctionPointer)).is_none() } {
        return None;
    }
    if let Some(o) = crate::modules::sys::get_sys_attr("stdout") {
        if ab::call_method_str(&o, "flush", &[]).is_err() {
            clear_exc();
        }
    }
    let mut p = prompt.as_bytes().to_vec();
    p.retain(|b| *b != 0);
    p.push(0);
    let r = unsafe { crate::capi::misc::PyOS_Readline(stdin, stdout, p.as_ptr() as *const std::ffi::c_char) };
    if r.is_null() {
        if !err_occurred() {
            set_exc_kbd();
        }
        return Some(Err(()));
    }
    let s = unsafe { std::ffi::CStr::from_ptr(r) }.to_string_lossy().into_owned();
    unsafe { crate::capi::mem::PyMem_RawFree(r as *mut std::ffi::c_void) };
    Some(Ok(s))
}

fn set_exc_kbd() {
    let _ = set_err_noargs(crate::types::exceptions::KeyboardInterrupt());
}

/// Make the source of an interactive statement available to tracebacks
/// (linecache keeps it keyed by the code objects).
fn register_interactive_source(code: &PyObject, src: &str) {
    register_source(code, src, "<stdin>")
}

/// (perf4) Sources recorded for linecache before it was imported: (owned code object,
/// source, name). `python -c` used to import linecache at every start only to register
/// its source; linecache takes these when it is imported (`_imp._rpy_linecache_pending`).
static PENDING_SOURCES: std::sync::Mutex<Vec<(usize, String, &'static str)>> = std::sync::Mutex::new(Vec::new());

/// Take the pending sources as a list of (code, source, name) tuples.
pub fn take_pending_sources() -> PyRef {
    let v = std::mem::take(&mut *PENDING_SOURCES.lock().unwrap_or_else(|e| e.into_inner()));
    let items: Vec<PyRef> = v
        .into_iter()
        .map(|(c, s, n)| crate::types::tuple::new_tuple(vec![unsafe { PyRef::from_raw(c as *mut PyObject) }, pystr::new_str(&s), pystr::new_str(n)]))
        .collect();
    crate::types::list::new_list(items)
}

fn register_source(code: &PyObject, src: &str, name: &str) {
    if name == "<string>" && crate::import::get_loaded_module("linecache").is_none() {
        PENDING_SOURCES.lock().unwrap_or_else(|e| e.into_inner()).push((code.to_ref().into_raw() as usize, src.to_string(), "<string>"));
        return;
    }
    let r = (|| -> PyResult<()> {
        let lc = crate::import::import_absolute("linecache")?;
        let f = ab::getattr_str(&lc, "_register_code")?;
        ab::call(&f, &[code, &pystr::new_str(src), &pystr::new_str(name)])?;
        Ok(())
    })();
    if r.is_err() {
        clear_exc();
    }
}

/// Run the pure-Python REPL (_pyrepl) when stdin/stdout are a terminal.
/// Returns None when it is not available (the caller falls back to `repl`).
fn fancy_repl(quiet: bool) -> Option<i32> {
    let cfg = crate::modules::sys_full::config();
    let basic = cfg.ignore_environment == 0 && std::env::var("PYTHON_BASIC_REPL").map(|v| !v.is_empty()).unwrap_or(false);
    if basic || unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return None;
    }
    let r = (|| -> PyResult<()> {
        // __main__ must exist before _pyrepl (rlcompleter imports it)
        let main = main_module()?;
        let m = crate::import::import_absolute("_pyrepl.main")?;
        let usable = ab::getattr_str(&m, "CAN_USE_PYREPL").and_then(|v| ab::is_true(&v)).unwrap_or(false);
        if !usable {
            clear_exc();
            if let Ok(reason) = ab::getattr_str(&m, "FAIL_REASON") {
                let reason = pystr::to_string_lossy(&reason);
                if !reason.is_empty() {
                    crate::modules::sys::write_stderr(&format!("{}\n", reason));
                }
            }
            clear_exc();
            return Err(crate::modules::tty::dl::import_error("pyrepl cannot be used"));
        }
        let f = ab::getattr_str(&m, "interactive_console")?;
        if !quiet {
            let v = crate::modules::sys::get_sys_attr("version").map(|v| pystr::to_string_lossy(&v)).unwrap_or_default();
            crate::modules::sys::write_stderr(&format!(
                "Python {} on linux\nType \"help\", \"copyright\", \"credits\" or \"license\" for more information.\n",
                v
            ));
        }
        run_interactive_hook();
        let startup = if cfg.ignore_environment == 0 && cfg.isolated == 0 { std::env::var("PYTHONSTARTUP").ok().filter(|s| !s.is_empty()) } else { None };
        let st = match startup {
            Some(s) => pystr::new_str(&s),
            None => none::none_ref(),
        };
        let kw = dict::new_dict();
        dict::set_item_str(&kw, "pythonstartup", &st)?;
        crate::modules::fncommon::call_kwdict(&f, &[&main, &crate::types::bool_::new_bool(quiet)], Some(&kw))?;
        Ok(())
    })();
    match r {
        Ok(()) => Some(0),
        Err(_) => {
            if exc_matches(crate::types::exceptions::SystemExit()) {
                let e = take_exc().unwrap();
                return Some(crate::handle_system_exit(&e));
            }
            if exc_matches(crate::types::exceptions::ImportError()) {
                clear_exc();
                return None;
            }
            crate::print_exception_and_clear();
            crate::modules::sys::write_stderr("warning: can't use pyrepl, falling back to the basic REPL\n");
            None
        }
    }
}

/// Simple interactive loop (fallback REPL).
fn repl(quiet: bool) -> i32 {
    if let Some(code) = fancy_repl(quiet) {
        return code;
    }
    let tty = unsafe { libc::isatty(0) } != 0;
    // (the plain REPL runs for a terminal or with -i: CPython prints the banner in both cases)
    if !quiet {
        let v = crate::modules::sys::get_sys_attr("version").map(|v| pystr::to_string_lossy(&v)).unwrap_or_default();
        crate::modules::sys::write_stderr(&format!(
            "Python {} on linux\nType \"help\", \"copyright\", \"credits\" or \"license\" for more information.\n",
            v
        ));
    }
    if let Some(sd) = Some(crate::modules::sys::sys_dict()) {
        if dict::get_item_rs(&sd, "ps1").is_none() {
            let _ = dict::set_item_str(&sd, "ps1", &pystr::new_str(">>> "));
        }
        if dict::get_item_rs(&sd, "ps2").is_none() {
            let _ = dict::set_item_str(&sd, "ps2", &pystr::new_str("... "));
        }
    }
    // __main__ must exist before the hook runs (rlcompleter imports it)
    if main_module().is_err() {
        clear_exc();
    }
    run_startup_file();
    run_interactive_hook();
    let use_readline = tty && unsafe { libc::isatty(1) } != 0;
    let stdin = std::io::stdin();
    let mut nomem = 0u32;
    let mut buf = String::new();
    // (tail3) `from __future__ import ...` typed at the prompt stays in effect for later statements
    let mut future_flags = 0u32;
    loop {
        // what the last statement printed is visible before the next prompt (pipes are buffered)
        for name in ["stderr", "stdout"] {
            if let Some(s) = crate::modules::sys::get_sys_attr(name).filter(|s| !s.is(none::none())) {
                if ab::call_method_str(&s, "flush", &[]).is_err() {
                    clear_exc();
                }
            }
        }
        // (tail2) reading and compiling the next statement needs parser memory; a
        // failure-injecting allocator hook that refuses it fails the statement before
        // any input is consumed, and the session gives up after a run of such failures
        if crate::capi::mem::hooked_alloc_refuses(4096) {
            let _ = crate::errors::memory_error();
            nomem += 1;
            if nomem > 16 {
                clear_exc();
                return 1;
            }
            crate::print_exception_and_clear();
            continue;
        }
        nomem = 0;
        let prompt = if buf.is_empty() { get_prompt("ps1", ">>> ") } else { get_prompt("ps2", "... ") };
        let mut line = String::new();
        let n = match if use_readline { readline_line(&prompt) } else { None } {
            Some(Ok(l)) => {
                line = l;
                Ok(line.len())
            }
            Some(Err(())) => {
                // KeyboardInterrupt (or another exception) while editing the line
                if exc_matches(crate::types::exceptions::KeyboardInterrupt()) {
                    clear_exc();
                    crate::modules::sys::write_stderr("\nKeyboardInterrupt\n");
                } else {
                    crate::print_exception_and_clear();
                }
                buf.clear();
                continue;
            }
            None => {
                write_prompt(&prompt);
                crate::threads::blocking(|| stdin.read_line(&mut line))
            }
        };
        if crate::modules::signal::signals_pending() {
            if crate::modules::signal::check_signals().is_err() {
                if exc_matches(crate::types::exceptions::KeyboardInterrupt()) {
                    clear_exc();
                    crate::modules::sys::write_stderr("\nKeyboardInterrupt\n");
                    buf.clear();
                    continue;
                }
                crate::print_exception_and_clear();
                continue;
            }
        }
        let at_eof = matches!(n, Ok(0) | Err(_));
        match n {
            Ok(0) | Err(_) => {
                if buf.is_empty() {
                    if tty {
                        crate::modules::sys::write_stderr("\n");
                    }
                    break;
                }
                // end of input finishes a pending statement as a blank line would
                line.clear();
            }
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            // last line before EOF: finish the prompt line like an echoed newline would
            write_prompt("\n");
        }
        buf.push_str(&line);
        let src = buf.clone();
        let blank = line.trim().is_empty();
        let r = crate::compile::compile_str(&src, "<stdin>", crate::compile::Mode::Single, future_flags, -1);
        match r {
            Ok(code) => {
                // multi-line compound statements continue until a blank line
                let first = src.lines().next().unwrap_or("");
                if src.lines().count() >= 1 && (first.trim_end().ends_with(':') || starts_compound(first)) && !blank {
                    continue;
                }
                buf.clear();
                register_interactive_source(&code.code, &src);
                future_flags |= code.future_flags & rpy_compiler::cf::PYCF_MASK;
                let res = (|| -> PyResult<()> {
                    let m = main_module()?;
                    let d = module::module_dict(&m);
                    crate::interp::eval_code(&code.code, &d, Some(&d))?;
                    Ok(())
                })();
                if res.is_err() {
                    if exc_matches(crate::types::exceptions::SystemExit()) {
                        let e = take_exc().unwrap();
                        return crate::handle_system_exit(&e);
                    }
                    crate::print_exception_and_clear();
                }
                // (hang1) each statement's output is flushed before the next
                // prompt, also when stdout is a pipe (block-buffered)
                crate::modules::sys_full::flush_std("stdout");
                crate::modules::sys_full::flush_std("stderr");
            }
            Err(_) => {
                // a compound statement is complete only after a blank line (its error, too)
                let first = src.lines().next().unwrap_or("");
                // inside an open bracket or triple-quoted string a blank line is just
                // part of the statement (only the end of input stops it there)
                let open = still_open_error();
                if ((is_incomplete_error() || starts_compound(first)) && !blank) || (open && !at_eof) {
                    clear_exc();
                    continue;
                }
                buf.clear();
                fill_syntax_error_text(&src);
                crate::print_exception_and_clear();
            }
        }
    }
    0
}

/// Does this line start a compound statement (which the prompt keeps open until a blank
/// line, even when its body is on the same line)?
fn starts_compound(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with('@') {
        return true;
    }
    let word: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let rest = t[word.len()..].trim_start();
    match word.as_str() {
        "def" | "class" | "if" | "for" | "while" | "with" | "try" => !rest.starts_with('=') && !rest.starts_with('.'),
        "async" => rest.starts_with("def") || rest.starts_with("for") || rest.starts_with("with"),
        _ => false,
    }
}

/// A SyntaxError found after parsing (symtable, code generation) has no source text for
/// a string; at the prompt the statement just typed is that text.
fn fill_syntax_error_text(src: &str) {
    let Some(e) = take_exc() else { return };
    let _ = (|| -> PyResult<()> {
        if !ab::isinstance(&e, &crate::types::typeobject::type_ref(crate::types::exceptions::SyntaxError()))? {
            return Ok(());
        }
        if !ab::getattr_str(&e, "text")?.is(crate::types::none::none()) {
            return Ok(());
        }
        let ln = ab::getattr_str(&e, "lineno")?;
        if !crate::types::int::is_int(&ln) {
            return Ok(());
        }
        let n = ab::as_i64(&ln)?;
        if let Some(line) = src.split_inclusive('\n').nth((n - 1).max(0) as usize).filter(|_| n >= 1) {
            ab::setattr(&e, crate::name!("text"), &pystr::new_str(line))?;
        }
        Ok(())
    })();
    clear_exc();
    set_exc(e);
}

fn outcome_code(o: Outcome) -> i32 {
    match o {
        Outcome::Ok => 0,
        Outcome::Exit(c) => c,
    }
}

/// The `-c` argument with the indentation shared by all non-blank lines
/// removed (blank lines become empty), so indented snippets run as-is.
fn dedent_command(c: &str) -> String {
    let is_ws = |ch: char| ch == ' ' || ch == '\t';
    let mut common: Option<&str> = None;
    for line in c.split('\n') {
        if line.chars().all(|ch| is_ws(ch) || ch == '\r') {
            continue;
        }
        let indent = &line[..line.len() - line.trim_start_matches(is_ws).len()];
        common = Some(match common {
            None => indent,
            Some(prev) => {
                let n = prev.bytes().zip(indent.bytes()).take_while(|(a, b)| a == b).count();
                &prev[..n]
            }
        });
    }
    let strip = common.unwrap_or("").len();
    if strip == 0 && !c.split('\n').any(|l| !l.is_empty() && l.chars().all(|ch| is_ws(ch) || ch == '\r')) {
        return c.to_string();
    }
    c.split('\n')
        .map(|line| {
            if line.chars().all(|ch| is_ws(ch) || ch == '\r') {
                ""
            } else {
                &line[strip..]
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the main program and finalise. Returns the process exit status
/// (or terminates the process itself for KeyboardInterrupt).
pub fn run_main(opts: MainOptions) -> i32 {
    let cfg = crate::modules::sys_full::config();
    if cfg.verbose == 0 {
        // nothing
    }
    // -W / PYTHONWARNINGS options are applied (and invalid ones reported) by
    // the warnings module, so it is imported up front when there are any
    // (grind-lib)
    if !cfg.warnoptions.is_empty() && crate::import::import_absolute("warnings").is_err() {
        crate::print_exception_and_clear();
    }
    let mut code = match &opts.mode {
        RunMode::Command(c) => {
            if !opts.safe_path {
                insert_path0("");
            }
            let mut src = dedent_command(c).into_bytes();
            src.push(b'\n');
            outcome_code(run_code_in_main(&src, "<string>"))
        }
        RunMode::Module(m) => {
            if !opts.safe_path {
                let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                insert_path0(&cwd);
            }
            outcome_code(run_module(m))
        }
        RunMode::Script(s) => outcome_code(run_script(s, &opts)),
        RunMode::Stdin => {
            if !opts.safe_path {
                insert_path0("");
            }
            let src = read_stdin_all();
            outcome_code(run_code_in_main(&src, "<stdin>"))
        }
        RunMode::Repl => {
            if !opts.safe_path {
                insert_path0("");
            }
            repl(opts.quiet)
        }
    };
    let inspect_env = cfg.ignore_environment == 0 && std::env::var("PYTHONINSPECT").map(|v| !v.is_empty()).unwrap_or(false);
    if (opts.inspect || inspect_env) && !matches!(opts.mode, RunMode::Repl) && code != -2 {
        code = repl(true);
    }
    let flushed = crate::finalize();
    if code == -2 {
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::kill(libc::getpid(), libc::SIGINT);
        }
        return 128 + libc::SIGINT;
    }
    if !flushed && code == 0 {
        return 120;
    }
    code
}

#[allow(dead_code)]
fn _unused(_: &PyObject, _: PyRef) {}
