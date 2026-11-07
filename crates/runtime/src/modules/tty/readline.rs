//! The `readline` module: a binding to GNU readline (libreadline.so.8, dlopen'ed).
//!
//! Besides the Python-visible functions, importing the module installs
//! `PyOS_ReadlineFunctionPointer`, so `input()` on a terminal goes through
//! readline (line editing, history, completion).  The read loop uses readline's
//! callback interface plus select() so that signals (Ctrl-C) are delivered to
//! Python while waiting for input.


use crate::errors::*;
use crate::ffi::*;
use crate::modules::posix::path::{fs_decode, fs_encode_str};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{dict, exceptions, int, list, none, str as pystr};
use crate::{dl_api, mdefs, raise};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

type RlHook = unsafe extern "C" fn() -> c_int;
type CompEntry = unsafe extern "C" fn(*const c_char, c_int) -> *mut c_char;
type AttemptedCompletion = unsafe extern "C" fn(*const c_char, c_int, c_int) -> *mut *mut c_char;
type DisplayHook = unsafe extern "C" fn(*mut *mut c_char, c_int, c_int);
type VcpFunc = unsafe extern "C" fn(*mut c_char);
type CommandFunc = unsafe extern "C" fn(c_int, c_int) -> c_int;

#[repr(C)]
struct HistEntry {
    line: *mut c_char,
    timestamp: *mut c_char,
    data: *mut c_void,
}

#[repr(C)]
struct HistState {
    entries: *mut *mut HistEntry,
    offset: c_int,
    length: c_int,
    size: c_int,
    flags: c_int,
}

dl_api! {
    pub struct RlApi, fn rl_api, ["libreadline.so.8", "libreadline.so"] {
        fn rl_initialize() -> c_int;
        fn rl_parse_and_bind(s: *mut c_char) -> c_int;
        fn rl_read_init_file(f: *const c_char) -> c_int;
        fn rl_variable_bind(n: *const c_char, v: *const c_char) -> c_int;
        fn rl_bind_key(k: c_int, f: CommandFunc) -> c_int;
        fn rl_insert(a: c_int, b: c_int) -> c_int;
        fn rl_insert_text(s: *const c_char) -> c_int;
        fn rl_redisplay();
        fn rl_completion_matches(t: *const c_char, f: CompEntry) -> *mut *mut c_char;
        fn rl_callback_handler_install(p: *const c_char, h: VcpFunc);
        fn rl_callback_read_char();
        fn rl_callback_handler_remove();
        fn rl_free_line_state();
        fn rl_cleanup_after_signal();
        fn rl_resize_terminal();
        fn using_history();
        fn add_history(s: *const c_char);
        fn remove_history(i: c_int) -> *mut HistEntry;
        fn replace_history_entry(i: c_int, l: *const c_char, d: *mut c_void) -> *mut HistEntry;
        fn free_history_entry(e: *mut HistEntry) -> *mut c_void;
        fn history_get(i: c_int) -> *mut HistEntry;
        fn history_get_history_state() -> *mut HistState;
        fn clear_history();
        fn read_history(f: *const c_char) -> c_int;
        fn write_history(f: *const c_char) -> c_int;
        fn append_history(n: c_int, f: *const c_char) -> c_int;
        fn history_truncate_file(f: *const c_char, n: c_int) -> c_int;
        ?fn rl_callback_sigcleanup();
        static rl_readline_name: *const c_char;
        static rl_line_buffer: *mut c_char;
        static rl_point: c_int;
        static rl_end: c_int;
        static rl_instream: *mut libc::FILE;
        static rl_outstream: *mut libc::FILE;
        static rl_startup_hook: Option<RlHook>;
        static rl_pre_input_hook: Option<RlHook>;
        static rl_attempted_completion_function: Option<AttemptedCompletion>;
        static rl_completion_display_matches_hook: Option<DisplayHook>;
        static rl_completer_word_break_characters: *mut c_char;
        static rl_completion_append_character: c_int;
        static rl_completion_suppress_append: c_int;
        static rl_attempted_completion_over: c_int;
        static rl_completion_type: c_int;
        static rl_catch_signals: c_int;
        static rl_readline_version: c_int;
        static rl_library_version: *const c_char;
        static history_base: c_int;
        static history_length: c_int;
    }
}

/// A Python object stored in a global (one strong reference owned by the slot).
struct Slot(AtomicPtr<PyObject>);
impl Slot {
    const fn new() -> Slot {
        Slot(AtomicPtr::new(std::ptr::null_mut()))
    }
    fn get(&self) -> Option<PyRef> {
        let p = self.0.load(Ordering::Acquire);
        if p.is_null() { None } else { Some(unsafe { &*p }.to_ref()) }
    }
    fn set(&self, v: Option<PyRef>) {
        let new = v.map(|r| r.into_raw()).unwrap_or(std::ptr::null_mut());
        // `get` on another thread may have loaded the old pointer and not yet taken its
        // reference: release it through QSBR
        unsafe { crate::qsbr::swap_field(self.0.as_ptr(), new) };
    }
}

static COMPLETER: Slot = Slot::new();
static STARTUP_HOOK: Slot = Slot::new();
static PRE_INPUT_HOOK: Slot = Slot::new();
static DISPLAY_HOOK: Slot = Slot::new();
static BEGIDX: Slot = Slot::new();
static ENDIDX: Slot = Slot::new();
static HISTORY_LENGTH: AtomicI32 = AtomicI32::new(-1);
static AUTO_HISTORY: AtomicBool = AtomicBool::new(true);
/// Serialises all use of the (non thread-safe) readline library.
static RL_LOCK: Mutex<()> = Mutex::new(());

fn api() -> &'static RlApi {
    // only called after the module was imported successfully
    rl_api().ok().expect("readline not loaded")
}

fn cstring(o: &PyObject) -> PyResult<CString> {
    let b = fs_encode_str(o)?;
    match CString::new(b) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null character"),
    }
}

fn str_arg(fname: &str, o: &PyObject) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument must be str, not {}", fname, tname(o));
    }
    cstring(o)
}

fn decode(p: *const c_char) -> PyResult {
    if p.is_null() {
        return Ok(pystr::new_str(""));
    }
    fs_decode(unsafe { CStr::from_ptr(p) }.to_bytes())
}

/// Optional filename argument (None/absent -> NULL = readline's default file).
fn opt_path(o: Option<&PyObject>) -> PyResult<Option<CString>> {
    match o {
        None => Ok(None),
        Some(x) if x.is(none::none()) => Ok(None),
        Some(x) => {
            let p = crate::modules::posix::path::path_named("readline", "filename", x)?;
            Ok(Some(p.c))
        }
    }
}

/// The "open" audit event for a file readline is about to open (`default`
/// names the file used when no file name was given).
fn audit_open(arg: Option<&PyObject>, default: &str, mode: &str) -> PyResult<()> {
    let name = match arg {
        Some(x) if !x.is(none::none()) => x.to_ref(),
        _ => crate::types::str::new_str(default),
    };
    crate::modules::sys_full::audit("open", &[&name, &crate::types::str::new_str(mode), &crate::types::int::new_int(0)])
}

fn cptr(c: &Option<CString>) -> *const c_char {
    c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null())
}

fn errno_err(e: c_int) -> PyErr {
    exceptions::os_error_from_errno(e, None, None)
}

fn none() -> PyResult {
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ module functions

fn parse_and_bind(_m: &PyObject, s: &PyObject) -> PyResult {
    let c = str_arg("parse_and_bind", s)?;
    let a = api();
    let _g = RL_LOCK.lock();
    // rl_parse_and_bind modifies its argument: hand it a private copy
    let copy = unsafe { libc::strdup(c.as_ptr()) };
    if copy.is_null() {
        return Err(memory_error());
    }
    unsafe {
        (a.rl_parse_and_bind)(copy);
        libc::free(copy as *mut c_void);
    }
    none()
}

fn read_init_file(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read_init_file", args, 0, 1)?;
    let p = opt_path(args.first().copied())?;
    audit_open(args.first().copied(), "<readline_init_file>", "r")?;
    let r = unsafe { (api().rl_read_init_file)(cptr(&p)) };
    if r != 0 {
        return Err(errno_err(r));
    }
    none()
}

fn read_history_file(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read_history_file", args, 0, 1)?;
    let p = opt_path(args.first().copied())?;
    audit_open(args.first().copied(), "~/.history", "r")?;
    let _g = RL_LOCK.lock();
    let r = unsafe { (api().read_history)(cptr(&p)) };
    if r != 0 {
        return Err(errno_err(r));
    }
    none()
}

fn write_history_file(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("write_history_file", args, 0, 1)?;
    let p = opt_path(args.first().copied())?;
    audit_open(args.first().copied(), "~/.history", "w")?;
    let a = api();
    let _g = RL_LOCK.lock();
    let mut r = unsafe { (a.write_history)(cptr(&p)) };
    let n = HISTORY_LENGTH.load(Ordering::Relaxed);
    if r == 0 && n >= 0 {
        r = unsafe { (a.history_truncate_file)(cptr(&p), n) };
    }
    if r != 0 {
        return Err(errno_err(r));
    }
    none()
}

fn append_history_file(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("append_history_file", args, 1, 2)?;
    let n = crate::modules::posix::path::c_int(args[0])?;
    if n < 0 {
        raise!(ValueError, "nelements must be non-negative");
    }
    let p = opt_path(args.get(1).copied())?;
    audit_open(args.get(1).copied(), "~/.history", "a")?;
    let a = api();
    let _g = RL_LOCK.lock();
    let mut r = unsafe { (a.append_history)(n, cptr(&p)) };
    let len = HISTORY_LENGTH.load(Ordering::Relaxed);
    if r == 0 && len >= 0 {
        r = unsafe { (a.history_truncate_file)(cptr(&p), len) };
    }
    if r != 0 {
        return Err(errno_err(r));
    }
    none()
}

fn set_history_length(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = crate::modules::posix::path::c_int(n)?;
    HISTORY_LENGTH.store(n, Ordering::Relaxed);
    none()
}

fn get_history_length(_m: &PyObject) -> PyResult {
    Ok(int::new_int(HISTORY_LENGTH.load(Ordering::Relaxed) as i64))
}

fn hook_arg(fname: &str, args: &[&PyObject]) -> PyResult<Option<PyRef>> {
    crate::args::check_pos(fname, args, 0, 1)?;
    match args.first() {
        None => Ok(None),
        Some(f) if f.is(none::none()) => Ok(None),
        Some(f) => {
            if !crate::abstract_::is_callable(f) {
                raise!(TypeError, "{} must be callable", fname.trim_start_matches("set_"));
            }
            Ok(Some(f.to_ref()))
        }
    }
}

fn set_completion_display_matches_hook(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let h = hook_arg("set_completion_display_matches_hook", args)?;
    let on = h.is_some();
    DISPLAY_HOOK.set(h);
    unsafe {
        *api().rl_completion_display_matches_hook = if on { Some(on_display_matches) } else { None };
    }
    none()
}

fn set_startup_hook(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    STARTUP_HOOK.set(hook_arg("set_startup_hook", args)?);
    none()
}

fn set_pre_input_hook(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    PRE_INPUT_HOOK.set(hook_arg("set_pre_input_hook", args)?);
    none()
}

fn get_pre_input_hook(_m: &PyObject) -> PyResult {
    Ok(PRE_INPUT_HOOK.get().unwrap_or_else(none::none_ref))
}

fn set_completer(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    COMPLETER.set(hook_arg("set_completer", args)?);
    none()
}

fn get_completer(_m: &PyObject) -> PyResult {
    Ok(COMPLETER.get().unwrap_or_else(none::none_ref))
}

fn get_completion_type(_m: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { *api().rl_completion_type } as i64))
}

fn get_begidx(_m: &PyObject) -> PyResult {
    Ok(BEGIDX.get().unwrap_or_else(|| int::new_int(0)))
}

fn get_endidx(_m: &PyObject) -> PyResult {
    Ok(ENDIDX.get().unwrap_or_else(|| int::new_int(0)))
}

fn set_completer_delims(_m: &PyObject, s: &PyObject) -> PyResult {
    let c = str_arg("set_completer_delims", s)?;
    let a = api();
    let _g = RL_LOCK.lock();
    unsafe {
        let new = libc::strdup(c.as_ptr());
        if new.is_null() {
            return Err(memory_error());
        }
        // The default value is a static string inside libreadline; only free
        // what we allocated ourselves.
        let old = *a.rl_completer_word_break_characters;
        *a.rl_completer_word_break_characters = new;
        let prev = OWNED_DELIMS.swap(new, Ordering::AcqRel);
        if !prev.is_null() && prev == old {
            libc::free(prev as *mut c_void);
        }
    }
    none()
}

static OWNED_DELIMS: AtomicPtr<c_char> = AtomicPtr::new(std::ptr::null_mut());

fn get_completer_delims(_m: &PyObject) -> PyResult {
    // under the lock: a concurrent set_completer_delims frees the previous string
    let _g = RL_LOCK.lock();
    decode(unsafe { *api().rl_completer_word_break_characters })
}

fn add_history(_m: &PyObject, s: &PyObject) -> PyResult {
    let c = str_arg("add_history", s)?;
    let _g = RL_LOCK.lock();
    unsafe { (api().add_history)(c.as_ptr()) };
    none()
}

fn history_len(a: &RlApi) -> c_int {
    unsafe {
        let st = (a.history_get_history_state)();
        if st.is_null() {
            return *a.history_length;
        }
        let n = (*st).length;
        libc::free(st as *mut c_void);
        n
    }
}

fn remove_history_item(_m: &PyObject, pos: &PyObject) -> PyResult {
    let n = crate::modules::posix::path::c_int(pos)?;
    let a = api();
    let _g = RL_LOCK.lock();
    if n < 0 {
        raise!(ValueError, "History index cannot be negative");
    }
    let e = unsafe { (a.remove_history)(n) };
    if e.is_null() {
        raise!(ValueError, "No history item at position {}", n);
    }
    unsafe { free_entry(a, e) };
    none()
}

unsafe fn free_entry(a: &RlApi, e: *mut HistEntry) {
    unsafe {
        let data = (a.free_history_entry)(e);
        if !data.is_null() {
            libc::free(data);
        }
    }
}

fn replace_history_item(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("replace_history_item", args, 2, 2)?;
    let n = crate::modules::posix::path::c_int(args[0])?;
    let line = str_arg("replace_history_item", args[1])?;
    let a = api();
    let _g = RL_LOCK.lock();
    if n < 0 {
        raise!(ValueError, "History index cannot be negative");
    }
    let old = unsafe { (a.replace_history_entry)(n, line.as_ptr(), std::ptr::null_mut()) };
    if old.is_null() {
        raise!(ValueError, "No history item at position {}", n);
    }
    unsafe { free_entry(a, old) };
    none()
}

fn get_history_item(_m: &PyObject, idx: &PyObject) -> PyResult {
    let n = crate::modules::posix::path::c_int(idx)?;
    let a = api();
    let _g = RL_LOCK.lock();
    let e = unsafe { (a.history_get)(n) };
    if e.is_null() {
        return none();
    }
    decode(unsafe { (*e).line })
}

fn get_current_history_length(_m: &PyObject) -> PyResult {
    let _g = RL_LOCK.lock();
    Ok(int::new_int(history_len(api()) as i64))
}

fn clear_history(_m: &PyObject) -> PyResult {
    let _g = RL_LOCK.lock();
    unsafe { (api().clear_history)() };
    none()
}

fn insert_text(_m: &PyObject, s: &PyObject) -> PyResult {
    let c = str_arg("insert_text", s)?;
    unsafe { (api().rl_insert_text)(c.as_ptr()) };
    none()
}

fn redisplay(_m: &PyObject) -> PyResult {
    unsafe { (api().rl_redisplay)() };
    none()
}

fn get_line_buffer(_m: &PyObject) -> PyResult {
    decode(unsafe { *api().rl_line_buffer })
}

fn set_auto_history(_m: &PyObject, v: &PyObject) -> PyResult {
    AUTO_HISTORY.store(crate::abstract_::is_true(v)?, Ordering::Relaxed);
    none()
}

// ------------------------------------------------------------------ callbacks from readline

/// Report an exception raised by a Python hook (readline cannot propagate it).
fn report_hook_error() {
    if let Some(e) = take_exc() {
        let r = (|| -> PyResult<()> {
            let sys = crate::import::import_absolute("sys")?;
            let hook = crate::abstract_::getattr_str(&sys, "excepthook")?;
            let tp = crate::types::typeobject::type_ref(e.type_ptr());
            let tb = crate::abstract_::getattr_str(&e, "__traceback__")?;
            crate::abstract_::call(&hook, &[&tp, &e, &tb])?;
            Ok(())
        })();
        if r.is_err() {
            clear_exc();
        }
    }
}

fn call_hook(slot: &Slot) -> c_int {
    let Some(f) = slot.get() else { return 0 };
    match crate::abstract_::call(&f, &[]) {
        Ok(r) => {
            if r.is(none::none()) {
                0
            } else {
                match crate::types::int::as_i64(&r) {
                    Some(v) => v as c_int,
                    None => 0,
                }
            }
        }
        Err(_) => {
            report_hook_error();
            -1
        }
    }
}

unsafe extern "C" fn on_startup() -> c_int {
    call_hook(&STARTUP_HOOK)
}

unsafe extern "C" fn on_pre_input() -> c_int {
    call_hook(&PRE_INPUT_HOOK)
}

unsafe extern "C" fn on_display_matches(matches: *mut *mut c_char, num: c_int, max_len: c_int) {
    let Some(f) = DISPLAY_HOOK.get() else { return };
    let r = (|| -> PyResult<()> {
        let sub = decode(unsafe { *matches })?;
        let mut items = Vec::with_capacity(num as usize);
        for i in 1..=num as usize {
            items.push(decode(unsafe { *matches.add(i) })?);
        }
        let l = list::new_list(items);
        crate::abstract_::call(&f, &[&sub, &l, &int::new_int(max_len as i64)])?;
        Ok(())
    })();
    if r.is_err() {
        report_hook_error();
    }
}

unsafe extern "C" fn on_completion(text: *const c_char, state: c_int) -> *mut c_char {
    let Some(f) = COMPLETER.get() else { return std::ptr::null_mut() };
    let r = (|| -> PyResult<*mut c_char> {
        let t = decode(text)?;
        let res = crate::abstract_::call(&f, &[&t, &int::new_int(state as i64)])?;
        if res.is(none::none()) {
            return Ok(std::ptr::null_mut());
        }
        if !pystr::is_str(&res) {
            raise!(TypeError, "completer must return a str or None, not {}", tname(&res));
        }
        let c = cstring(&res)?;
        Ok(unsafe { libc::strdup(c.as_ptr()) })
    })();
    match r {
        Ok(p) => p,
        Err(_) => {
            report_hook_error();
            std::ptr::null_mut()
        }
    }
}

/// Number of characters in the first `nbytes` bytes of the line buffer.
fn char_offset(buf: *const c_char, nbytes: c_int) -> i64 {
    if buf.is_null() || nbytes <= 0 {
        return 0;
    }
    let bytes = unsafe { CStr::from_ptr(buf) }.to_bytes();
    let n = (nbytes as usize).min(bytes.len());
    match fs_decode(&bytes[..n]) {
        Ok(s) => pystr::str_len(&s) as i64,
        Err(_) => {
            clear_exc();
            n as i64
        }
    }
}

unsafe extern "C" fn flex_complete(text: *const c_char, start: c_int, end: c_int) -> *mut *mut c_char {
    let a = api();
    unsafe {
        *a.rl_completion_append_character = 0;
        *a.rl_completion_suppress_append = 0;
        // never fall back to readline's filename completion
        *a.rl_attempted_completion_over = 1;
        let buf = *a.rl_line_buffer;
        BEGIDX.set(Some(int::new_int(char_offset(buf, start))));
        ENDIDX.set(Some(int::new_int(char_offset(buf, end))));
        (a.rl_completion_matches)(text, on_completion)
    }
}

// ------------------------------------------------------------------ the input() hook

struct ReadState {
    done: bool,
    line: *mut c_char,
}
static mut READ_STATE: ReadState = ReadState { done: false, line: std::ptr::null_mut() };

unsafe extern "C" fn line_handler(line: *mut c_char) {
    unsafe {
        let st = &mut *(&raw mut READ_STATE);
        st.line = line;
        st.done = true;
        (api().rl_callback_handler_remove)();
    }
}

unsafe fn raw_copy(bytes: &[u8], newline: bool) -> *mut c_char {
    unsafe {
        let n = bytes.len() + newline as usize;
        let p = crate::capi::mem::PyMem_RawMalloc(n + 1) as *mut u8;
        if p.is_null() {
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        if newline {
            *p.add(bytes.len()) = b'\n';
        }
        *p.add(n) = 0;
        p as *mut c_char
    }
}

/// PyOS_ReadlineFunctionPointer implementation.  Returns a PyMem_RawMalloc'd
/// line including the trailing newline, "" at EOF, or NULL with an exception
/// set (e.g. KeyboardInterrupt).
unsafe extern "C" fn call_readline(inp: *mut libc::FILE, out: *mut libc::FILE, prompt: *const c_char) -> *mut c_char {
    let a = api();
    let _g = crate::threads::blocking(|| RL_LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    unsafe {
        *a.rl_instream = inp;
        *a.rl_outstream = out;
        let st = &mut *(&raw mut READ_STATE);
        st.done = false;
        st.line = std::ptr::null_mut();
        (a.rl_callback_handler_install)(prompt, line_handler);
        let fd = libc::fileno(inp);
        while !st.done {
            let ready = crate::threads::blocking(|| {
                let mut fds: libc::fd_set = std::mem::zeroed();
                libc::FD_ZERO(&mut fds);
                libc::FD_SET(fd, &mut fds);
                let hook = crate::capi::misc::PyOS_InputHook;
                let mut tv = libc::timeval { tv_sec: 0, tv_usec: 100_000 };
                let tvp = if hook.is_some() { &mut tv as *mut libc::timeval } else { std::ptr::null_mut() };
                let r = libc::select(fd + 1, &mut fds, std::ptr::null_mut(), std::ptr::null_mut(), tvp);
                (r, *libc::__errno_location())
            });
            match ready {
                (r, _) if r > 0 => (a.rl_callback_read_char)(),
                (0, _) => {
                    if let Some(h) = crate::capi::misc::PyOS_InputHook {
                        h();
                    }
                }
                (_, e) if e == libc::EINTR => {
                    if crate::modules::signal::check_signals().is_err() {
                        // abandon the line: restore the terminal and report
                        (a.rl_free_line_state)();
                        if let Some(f) = a.rl_callback_sigcleanup {
                            f();
                        }
                        (a.rl_cleanup_after_signal)();
                        (a.rl_callback_handler_remove)();
                        libc::fputs(c"\n".as_ptr(), out);
                        libc::fflush(out);
                        return std::ptr::null_mut();
                    }
                }
                _ => {
                    // select() failure (e.g. bad fd): treat as EOF
                    (a.rl_callback_handler_remove)();
                    break;
                }
            }
        }
        let line = st.line;
        st.line = std::ptr::null_mut();
        if line.is_null() {
            // EOF
            return raw_copy(b"", false);
        }
        let bytes = CStr::from_ptr(line).to_bytes();
        if !bytes.is_empty() && AUTO_HISTORY.load(Ordering::Relaxed) {
            let n = history_len(a);
            let last: &[u8] = if n > 0 {
                let e = (a.history_get)(*a.history_base + n - 1);
                if e.is_null() || (*e).line.is_null() { b"" } else { CStr::from_ptr((*e).line).to_bytes() }
            } else {
                b""
            };
            if last != bytes {
                (a.add_history)(line);
            }
        }
        let r = raw_copy(bytes, true);
        libc::free(line as *mut c_void);
        r
    }
}

// ------------------------------------------------------------------ module init

fn setup(a: &RlApi) {
    unsafe {
        (a.using_history)();
        *a.rl_readline_name = c"python".as_ptr();
        // Tab inserts a tab until someone (e.g. site/rlcompleter) binds completion.
        (a.rl_bind_key)(b'\t' as c_int, a.rl_insert);
        *a.rl_startup_hook = Some(on_startup);
        *a.rl_pre_input_hook = Some(on_pre_input);
        *a.rl_attempted_completion_function = Some(flex_complete);
        // We handle SIGINT ourselves (see call_readline).
        *a.rl_catch_signals = 0;
        let delims = libc::strdup(c" \t\n`~!@#$%^&*()-=+[{]}\\|;:'\",<>/?".as_ptr());
        *a.rl_completer_word_break_characters = delims;
        OWNED_DELIMS.store(delims, Ordering::Release);
        // Avoid emitting the "meta key" escape sequence when output is not a terminal.
        if libc::isatty(1) == 0 {
            (a.rl_variable_bind)(c"enable-meta-key".as_ptr(), c"off".as_ptr());
        }
        (a.rl_initialize)();
        if libc::isatty(1) == 0 {
            (a.rl_variable_bind)(c"enable-meta-key".as_ptr(), c"off".as_ptr());
        }
        // The line-at-a-time REPL handles pasted text itself; bracketed-paste
        // mode would wrap every prompt in terminal mode switches.
        (a.rl_variable_bind)(c"enable-bracketed-paste".as_ptr(), c"off".as_ptr());
    }
}

pub fn init() -> PyResult {
    let a = rl_api()?;
    static SETUP: std::sync::Once = std::sync::Once::new();
    SETUP.call_once(|| setup(a));
    unsafe {
        crate::capi::misc::PyOS_ReadlineFunctionPointer = Some(call_readline);
    }
    let methods = mdefs![
        ("parse_and_bind", O, parse_and_bind, "parse_and_bind($module, string, /)\n--\n\nExecute the init line provided in the string argument."),
        ("read_init_file", FAST, read_init_file, "read_init_file($module, filename=None, /)\n--\n\nExecute a readline initialization file.\n\nThe default filename is the last filename used."),
        ("read_history_file", FAST, read_history_file, "read_history_file($module, filename=None, /)\n--\n\nLoad a readline history file.\n\nThe default filename is ~/.history."),
        ("write_history_file", FAST, write_history_file, "write_history_file($module, filename=None, /)\n--\n\nSave a readline history file.\n\nThe default filename is ~/.history."),
        ("append_history_file", FAST, append_history_file, "append_history_file($module, nelements, filename=None, /)\n--\n\nAppend the last nelements items of the history list to file.\n\nThe default filename is ~/.history."),
        ("set_history_length", O, set_history_length, "set_history_length($module, length, /)\n--\n\nSet the maximal number of lines which will be written to the history file.\n\nA negative length is used to inhibit history truncation."),
        ("get_history_length", NOARGS, get_history_length, "get_history_length($module, /)\n--\n\nReturn the maximum number of lines that will be written to the history file."),
        ("set_completion_display_matches_hook", FAST, set_completion_display_matches_hook, "set_completion_display_matches_hook($module, function=None, /)\n--\n\nSet or remove the completion display function.\n\nThe function is called as\n  function(substitution, [matches], longest_match_length)\nonce each time matches need to be displayed."),
        ("set_startup_hook", FAST, set_startup_hook, "set_startup_hook($module, function=None, /)\n--\n\nSet or remove the function invoked by the rl_startup_hook callback.\n\nThe function is called with no arguments just\nbefore readline prints the first prompt."),
        ("set_pre_input_hook", FAST, set_pre_input_hook, "set_pre_input_hook($module, function=None, /)\n--\n\nSet or remove the function invoked by the rl_pre_input_hook callback.\n\nThe function is called with no arguments after the first prompt\nhas been printed and just before readline starts reading input\ncharacters."),
        ("get_pre_input_hook", NOARGS, get_pre_input_hook, "get_pre_input_hook($module, /)\n--\n\nGet the current pre-input hook function."),
        ("set_completer", FAST, set_completer, "set_completer($module, function=None, /)\n--\n\nSet or remove the completer function.\n\nThe function is called as function(text, state),\nfor state in 0, 1, 2, ..., until it returns a non-string.\nIt should return the next possible completion starting with 'text'."),
        ("get_completer", NOARGS, get_completer, "get_completer($module, /)\n--\n\nGet the current completer function."),
        ("get_completion_type", NOARGS, get_completion_type, "get_completion_type($module, /)\n--\n\nGet the type of completion being attempted."),
        ("get_begidx", NOARGS, get_begidx, "get_begidx($module, /)\n--\n\nGet the beginning index of the completion scope."),
        ("get_endidx", NOARGS, get_endidx, "get_endidx($module, /)\n--\n\nGet the ending index of the completion scope."),
        ("set_completer_delims", O, set_completer_delims, "set_completer_delims($module, string, /)\n--\n\nSet the word delimiters for completion."),
        ("get_completer_delims", NOARGS, get_completer_delims, "get_completer_delims($module, /)\n--\n\nGet the word delimiters for completion."),
        ("add_history", O, add_history, "add_history($module, string, /)\n--\n\nAdd an item to the history buffer."),
        ("remove_history_item", O, remove_history_item, "remove_history_item($module, pos, /)\n--\n\nRemove history item given by its zero-based position."),
        ("replace_history_item", FAST, replace_history_item, "replace_history_item($module, pos, line, /)\n--\n\nReplaces history item given by its position with contents of line.\n\npos is zero-based."),
        ("get_history_item", O, get_history_item, "get_history_item($module, index, /)\n--\n\nReturn the current contents of history item at one-based index."),
        ("get_current_history_length", NOARGS, get_current_history_length, "get_current_history_length($module, /)\n--\n\nReturn the current (not the maximum) length of history."),
        ("clear_history", NOARGS, clear_history, "clear_history($module, /)\n--\n\nClear the current readline history."),
        ("insert_text", O, insert_text, "insert_text($module, string, /)\n--\n\nInsert text into the line buffer at the cursor position."),
        ("redisplay", NOARGS, redisplay, "redisplay($module, /)\n--\n\nChange what's displayed on the screen to reflect contents of the line buffer."),
        ("get_line_buffer", NOARGS, get_line_buffer, "get_line_buffer($module, /)\n--\n\nReturn the current contents of the line buffer."),
        ("set_auto_history", O, set_auto_history, "set_auto_history($module, enabled, /)\n--\n\nEnables or disables automatic history."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("readline", methods, Some("Importing this module enables command line editing using GNU readline."))?;
    let version = unsafe { *a.rl_readline_version } as i64;
    dict::set_item_str(&d, "_READLINE_VERSION", &int::new_int(version))?;
    dict::set_item_str(&d, "_READLINE_RUNTIME_VERSION", &int::new_int(version))?;
    let lib = unsafe { *a.rl_library_version };
    let libv = decode(lib)?;
    dict::set_item_str(&d, "_READLINE_LIBRARY_VERSION", &libv)?;
    dict::set_item_str(&d, "backend", &pystr::new_str("readline"))?;
    Ok(m)
}
