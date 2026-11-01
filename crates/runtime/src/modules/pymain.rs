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
        let code = crate::compile::compile_bytes(src, filename, crate::compile::Mode::Exec, 0, -1)?;
        crate::interp::eval_code(&code.code, &d, Some(&d))?;
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

fn insert_path0(p: &str) {
    if let Some(path) = crate::modules::sys::get_sys_attr("path") {
        list::insert(&path, 0, pystr::new_str(p));
    }
}

fn run_script(path: &str, opts: &MainOptions) -> Outcome {
    let meta = std::fs::metadata(path);
    // a directory or zip file with __main__.py: run it as a module
    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    let is_zip = !is_dir && looks_like_zip(path);
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

/// Simple interactive loop (fallback REPL).
fn repl(quiet: bool) -> i32 {
    let tty = unsafe { libc::isatty(0) } != 0;
    if !quiet && tty {
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
    run_startup_file();
    let stdin = std::io::stdin();
    let mut buf = String::new();
    loop {
        let prompt = if buf.is_empty() { get_prompt("ps1", ">>> ") } else { get_prompt("ps2", "... ") };
        if tty || true {
            write_prompt(&prompt);
        }
        let mut line = String::new();
        let n = crate::threads::blocking(|| stdin.read_line(&mut line));
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
        match n {
            Ok(0) | Err(_) => {
                if tty {
                    crate::modules::sys::write_stderr("\n");
                }
                break;
            }
            Ok(_) => {}
        }
        buf.push_str(&line);
        let src = buf.clone();
        let blank = line.trim().is_empty();
        let r = crate::compile::compile_str(&src, "<stdin>", crate::compile::Mode::Single, 0, -1);
        match r {
            Ok(code) => {
                // multi-line compound statements continue until a blank line
                let first = src.lines().next().unwrap_or("");
                if src.lines().count() >= 1 && first.trim_end().ends_with(':') && !blank {
                    continue;
                }
                buf.clear();
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
            }
            Err(_) => {
                if is_incomplete_error() && !blank {
                    clear_exc();
                    continue;
                }
                buf.clear();
                crate::print_exception_and_clear();
            }
        }
    }
    0
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
