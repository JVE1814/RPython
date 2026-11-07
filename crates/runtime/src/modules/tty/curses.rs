//! `_curses`: binding to ncursesw (libncursesw.so.6, dlopen'ed at import).
//!
//! Window objects wrap a `WINDOW*`; sub-windows keep their parent alive.  The
//! character arguments accept an int (a chtype, possibly with attributes), a
//! one-byte bytes object or a one-character str; non-ASCII characters of a str
//! are written as wide characters after checking that the window encoding can
//! represent them.  Text arguments accept bytes (written as is) or str (written
//! through the wide-character API).
//!
//! ncurses is not thread-safe; like the C module we rely on callers not using
//! one screen from several threads at once.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, dict, exceptions, int, none, str as pystr, tuple, typeobject};
use crate::{dl_api, mdefs, raise};
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_short, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

type W = *mut c_void;
pub const ERR: c_int = -1;

/// Opaque storage for an ncurses `cchar_t` (28 bytes on this ABI).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct CChar([u8; 64]);
impl CChar {
    fn zero() -> CChar {
        CChar([0; 64])
    }
}

#[repr(C)]
#[derive(Default)]
struct MEvent {
    id: c_short,
    x: c_int,
    y: c_int,
    z: c_int,
    bstate: u32,
}

dl_api! {
    pub struct Nc, fn nc_api, ["libncursesw.so.6", "libncursesw.so"] {
        fn initscr() -> W;
        fn endwin() -> c_int;
        fn isendwin() -> u8;
        fn newwin(l: c_int, c: c_int, y: c_int, x: c_int) -> W;
        fn newpad(l: c_int, c: c_int) -> W;
        fn delwin(w: W) -> c_int;
        fn subwin(w: W, l: c_int, c: c_int, y: c_int, x: c_int) -> W;
        fn derwin(w: W, l: c_int, c: c_int, y: c_int, x: c_int) -> W;
        fn subpad(w: W, l: c_int, c: c_int, y: c_int, x: c_int) -> W;
        fn mvwin(w: W, y: c_int, x: c_int) -> c_int;
        fn mvderwin(w: W, y: c_int, x: c_int) -> c_int;
        fn wmove(w: W, y: c_int, x: c_int) -> c_int;
        fn waddch(w: W, c: u32) -> c_int;
        fn wadd_wch(w: W, c: *const CChar) -> c_int;
        fn waddnstr(w: W, s: *const c_char, n: c_int) -> c_int;
        fn waddnwstr(w: W, s: *const i32, n: c_int) -> c_int;
        fn winsch(w: W, c: u32) -> c_int;
        fn wins_wch(w: W, c: *const CChar) -> c_int;
        fn winsnstr(w: W, s: *const c_char, n: c_int) -> c_int;
        fn wins_nwstr(w: W, s: *const i32, n: c_int) -> c_int;
        fn wechochar(w: W, c: u32) -> c_int;
        fn wecho_wchar(w: W, c: *const CChar) -> c_int;
        fn pechochar(w: W, c: u32) -> c_int;
        fn pecho_wchar(w: W, c: *const CChar) -> c_int;
        fn winch(w: W) -> u32;
        fn win_wch(w: W, c: *mut CChar) -> c_int;
        fn winnstr(w: W, s: *mut c_char, n: c_int) -> c_int;
        fn wgetch(w: W) -> c_int;
        fn wget_wch(w: W, c: *mut u32) -> c_int;
        fn wgetnstr(w: W, s: *mut c_char, n: c_int) -> c_int;
        fn wattr_get(w: W, a: *mut u32, p: *mut c_short, o: *mut c_void) -> c_int;
        fn wattr_set(w: W, a: u32, p: c_short, o: *mut c_void) -> c_int;
        fn wattron(w: W, a: c_int) -> c_int;
        fn wattroff(w: W, a: c_int) -> c_int;
        fn wattrset(w: W, a: c_int) -> c_int;
        fn wstandout(w: W) -> c_int;
        fn wstandend(w: W) -> c_int;
        fn wbkgd(w: W, c: u32) -> c_int;
        fn wbkgdset(w: W, c: u32);
        fn wbkgrnd(w: W, c: *const CChar) -> c_int;
        fn wbkgrndset(w: W, c: *const CChar);
        fn getbkgd(w: W) -> u32;
        fn wborder(w: W, a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32) -> c_int;
        fn whline(w: W, c: u32, n: c_int) -> c_int;
        fn wvline(w: W, c: u32, n: c_int) -> c_int;
        fn whline_set(w: W, c: *const CChar, n: c_int) -> c_int;
        fn wvline_set(w: W, c: *const CChar, n: c_int) -> c_int;
        fn wchgat(w: W, n: c_int, a: u32, p: c_short, o: *const c_void) -> c_int;
        fn wclear(w: W) -> c_int;
        fn werase(w: W) -> c_int;
        fn wclrtobot(w: W) -> c_int;
        fn wclrtoeol(w: W) -> c_int;
        fn wdelch(w: W) -> c_int;
        fn wdeleteln(w: W) -> c_int;
        fn winsertln(w: W) -> c_int;
        fn winsdelln(w: W, n: c_int) -> c_int;
        fn wscrl(w: W, n: c_int) -> c_int;
        fn scrollok(w: W, b: c_int) -> c_int;
        fn wsetscrreg(w: W, t: c_int, b: c_int) -> c_int;
        fn clearok(w: W, b: c_int) -> c_int;
        fn idcok(w: W, b: c_int);
        fn idlok(w: W, b: c_int) -> c_int;
        fn immedok(w: W, b: c_int);
        fn leaveok(w: W, b: c_int) -> c_int;
        fn keypad(w: W, b: c_int) -> c_int;
        fn nodelay(w: W, b: c_int) -> c_int;
        fn notimeout(w: W, b: c_int) -> c_int;
        fn wtimeout(w: W, d: c_int);
        fn syncok(w: W, b: c_int) -> c_int;
        fn wsyncup(w: W);
        fn wsyncdown(w: W);
        fn wcursyncup(w: W);
        fn wtouchln(w: W, y: c_int, n: c_int, c: c_int) -> c_int;
        fn is_wintouched(w: W) -> u8;
        fn is_linetouched(w: W, l: c_int) -> u8;
        fn wnoutrefresh(w: W) -> c_int;
        fn wrefresh(w: W) -> c_int;
        fn pnoutrefresh(w: W, a: c_int, b: c_int, c: c_int, d: c_int, e: c_int, f: c_int) -> c_int;
        fn prefresh(w: W, a: c_int, b: c_int, c: c_int, d: c_int, e: c_int, f: c_int) -> c_int;
        fn doupdate() -> c_int;
        fn wredrawln(w: W, b: c_int, n: c_int) -> c_int;
        fn wresize(w: W, l: c_int, c: c_int) -> c_int;
        fn wenclose(w: W, y: c_int, x: c_int) -> u8;
        fn copywin(s: W, d: W, a: c_int, b: c_int, c: c_int, e: c_int, f: c_int, g: c_int, o: c_int) -> c_int;
        fn overlay(s: W, d: W) -> c_int;
        fn overwrite(s: W, d: W) -> c_int;
        fn putwin(w: W, f: *mut libc::FILE) -> c_int;
        fn getwin(f: *mut libc::FILE) -> W;
        fn is_pad(w: W) -> u8;
        fn is_leaveok(w: W) -> u8;
        fn getcury(w: W) -> c_int;
        fn getcurx(w: W) -> c_int;
        fn getbegy(w: W) -> c_int;
        fn getbegx(w: W) -> c_int;
        fn getmaxy(w: W) -> c_int;
        fn getmaxx(w: W) -> c_int;
        fn getpary(w: W) -> c_int;
        fn getparx(w: W) -> c_int;
        fn setcchar(c: *mut CChar, w: *const i32, a: u32, p: c_short, o: *const c_void) -> c_int;
        fn getcchar(c: *const CChar, w: *mut i32, a: *mut u32, p: *mut c_short, o: *mut c_void) -> c_int;
        fn baudrate() -> c_int;
        fn beep() -> c_int;
        fn flash() -> c_int;
        fn can_change_color() -> u8;
        fn cbreak() -> c_int;
        fn nocbreak() -> c_int;
        fn echo() -> c_int;
        fn noecho() -> c_int;
        fn nl() -> c_int;
        fn nonl() -> c_int;
        fn raw() -> c_int;
        fn noraw() -> c_int;
        fn qiflush();
        fn noqiflush();
        fn intrflush(w: W, b: c_int) -> c_int;
        fn meta(w: W, b: c_int) -> c_int;
        fn halfdelay(t: c_int) -> c_int;
        fn curs_set(v: c_int) -> c_int;
        fn def_prog_mode() -> c_int;
        fn def_shell_mode() -> c_int;
        fn reset_prog_mode() -> c_int;
        fn reset_shell_mode() -> c_int;
        fn resetty() -> c_int;
        fn savetty() -> c_int;
        fn delay_output(ms: c_int) -> c_int;
        fn napms(ms: c_int) -> c_int;
        fn flushinp() -> c_int;
        fn erasechar() -> c_char;
        fn killchar() -> c_char;
        fn has_colors() -> u8;
        fn has_ic() -> u8;
        fn has_il() -> u8;
        fn has_key(k: c_int) -> c_int;
        fn start_color() -> c_int;
        fn use_default_colors() -> c_int;
        fn assume_default_colors(f: c_int, b: c_int) -> c_int;
        fn init_color(c: c_short, r: c_short, g: c_short, b: c_short) -> c_int;
        fn color_content(c: c_short, r: *mut c_short, g: *mut c_short, b: *mut c_short) -> c_int;
        fn init_pair(p: c_short, f: c_short, b: c_short) -> c_int;
        fn pair_content(p: c_short, f: *mut c_short, b: *mut c_short) -> c_int;
        fn keyname(k: c_int) -> *const c_char;
        fn longname() -> *const c_char;
        fn termname() -> *const c_char;
        fn termattrs() -> u32;
        fn filter();
        fn use_env(b: c_int);
        fn typeahead(fd: c_int) -> c_int;
        fn unctrl(c: u32) -> *const c_char;
        fn ungetch(c: c_int) -> c_int;
        fn unget_wch(c: i32) -> c_int;
        fn mousemask(m: u32, old: *mut u32) -> u32;
        fn mouseinterval(i: c_int) -> c_int;
        fn getmouse(e: *mut MEvent) -> c_int;
        fn ungetmouse(e: *mut MEvent) -> c_int;
        fn setupterm(t: *const c_char, fd: c_int, err: *mut c_int) -> c_int;
        fn tigetflag(n: *const c_char) -> c_int;
        fn tigetnum(n: *const c_char) -> c_int;
        fn tigetstr(n: *const c_char) -> *mut c_char;
        fn tparm(s: *const c_char, a: c_long, b: c_long, c: c_long, d: c_long, e: c_long, f: c_long, g: c_long, h: c_long, i: c_long) -> *mut c_char;
        fn putp(s: *const c_char) -> c_int;
        fn is_term_resized(l: c_int, c: c_int) -> u8;
        fn resize_term(l: c_int, c: c_int) -> c_int;
        fn resizeterm(l: c_int, c: c_int) -> c_int;
        fn get_escdelay() -> c_int;
        fn set_escdelay(d: c_int) -> c_int;
        fn set_tabsize(d: c_int) -> c_int;
        fn curses_version() -> *const c_char;
        ?fn init_extended_color(c: c_int, r: c_int, g: c_int, b: c_int) -> c_int;
        ?fn extended_color_content(c: c_int, r: *mut c_int, g: *mut c_int, b: *mut c_int) -> c_int;
        ?fn init_extended_pair(p: c_int, f: c_int, b: c_int) -> c_int;
        ?fn extended_pair_content(p: c_int, f: *mut c_int, b: *mut c_int) -> c_int;
        static stdscr: W;
        static newscr: W;
        static LINES: c_int;
        static COLS: c_int;
        static COLORS: c_int;
        static COLOR_PAIRS: c_int;
        static TABSIZE: c_int;
        static acs_map: u32;
    }
}

pub fn api() -> &'static Nc {
    nc_api().ok().expect("ncurses not loaded")
}

// ------------------------------------------------------------------ module state

static ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static MODULE_DICT: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static INITSCR_CALLED: AtomicBool = AtomicBool::new(false);
static SETUPTERM_CALLED: AtomicBool = AtomicBool::new(false);
static COLOR_STARTED: AtomicBool = AtomicBool::new(false);
static SCREEN_ENCODING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn error_type() -> *mut PyTypeObject {
    let p = ERROR.load(Ordering::Acquire);
    if p.is_null() { exceptions::RuntimeError() } else { p as *mut PyTypeObject }
}

pub fn cerror(msg: &str) -> PyErr {
    new_err(error_type(), msg)
}

fn ret_err(fname: &str) -> PyErr {
    cerror(&format!("{}() returned ERR", fname))
}

fn check(r: c_int, fname: &str) -> PyResult {
    if r == ERR { Err(ret_err(fname)) } else { Ok(none::none_ref()) }
}

fn need_initscr() -> PyResult<()> {
    if !INITSCR_CALLED.load(Ordering::Acquire) {
        return Err(cerror("must call initscr() first"));
    }
    Ok(())
}

fn need_setupterm() -> PyResult<()> {
    if !SETUPTERM_CALLED.load(Ordering::Acquire) && !INITSCR_CALLED.load(Ordering::Acquire) {
        return Err(cerror("must call (at least) setupterm() first"));
    }
    Ok(())
}

fn need_color() -> PyResult<()> {
    need_initscr()?;
    if !COLOR_STARTED.load(Ordering::Acquire) {
        return Err(cerror("must call start_color() first"));
    }
    Ok(())
}

/// C-API checks behind PyCursesSetupTermCalled / PyCursesInitialised /
/// PyCursesInitialisedColor: 1, or 0 with _curses.error set.
fn capi_check(f: fn() -> PyResult<()>) -> c_int {
    if f().is_ok() { 1 } else { 0 }
}
extern "C" fn capi_setupterm_called() -> c_int {
    capi_check(need_setupterm)
}
extern "C" fn capi_initialised() -> c_int {
    capi_check(need_initscr)
}
extern "C" fn capi_initialised_color() -> c_int {
    capi_check(need_color)
}
struct CapiTable([*mut std::ffi::c_void; 4]);
unsafe impl Sync for CapiTable {}
unsafe impl Send for CapiTable {}
fn capi_table() -> *const *mut std::ffi::c_void {
    static T: std::sync::OnceLock<CapiTable> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        CapiTable([
            window_type() as *mut std::ffi::c_void,
            capi_setupterm_called as *mut std::ffi::c_void,
            capi_initialised as *mut std::ffi::c_void,
            capi_initialised_color as *mut std::ffi::c_void,
        ])
    })
    .0
    .as_ptr()
}

fn ci(o: &PyObject) -> PyResult<c_int> {
    crate::modules::posix::path::c_int(o)
}

fn big(o: &PyObject) -> PyResult<i128> {
    if !ab::has_index(o) || crate::types::float::is_float(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    match crate::types::int::as_i128(&i) {
        Some(v) => Ok(v),
        None => Ok(if crate::types::int::sign(&i) < 0 { i128::MIN } else { i128::MAX }),
    }
}

fn short_arg(o: &PyObject) -> PyResult<c_int> {
    let v = big(o)?;
    if v > i16::MAX as i128 {
        raise!(OverflowError, "signed short integer is greater than maximum");
    }
    if v < i16::MIN as i128 {
        raise!(OverflowError, "signed short integer is less than minimum");
    }
    Ok(v as c_int)
}

fn flag(o: Option<&&PyObject>, default: bool) -> PyResult<bool> {
    match o {
        Some(v) => ab::is_true(v),
        None => Ok(default),
    }
}

fn bool_ref(b: bool) -> PyResult {
    Ok(bool_::new_bool(b))
}

fn int_ref(v: i64) -> PyResult {
    Ok(int::new_int(v))
}

fn cstr_bytes(p: *const c_char) -> PyResult {
    if p.is_null() {
        return Ok(bytes::new_bytes(b""));
    }
    Ok(bytes::new_bytes(unsafe { CStr::from_ptr(p) }.to_bytes()))
}

// ------------------------------------------------------------------ characters

pub enum Ch {
    /// plain chtype (may include attribute bits)
    C(u32),
    /// a wide character (code point)
    W(u32),
}

fn encode_check(s: &PyObject, enc: &str) -> PyResult<()> {
    crate::modules::codecs_registry::encode(s, enc, "strict").map(|_| ())
}

/// Convert a character argument (int / 1-byte bytes / 1-char str).
pub fn ch_arg(o: &PyObject, enc: &str) -> PyResult<Ch> {
    if int::is_int(o) {
        let v = big(o)?;
        if v < 0 || v > u32::MAX as i128 {
            raise!(OverflowError, "character code is out of range");
        }
        return Ok(Ch::C(v as u32));
    }
    if bytes::is_bytes(o) && bytes::bytes_len(o) == 1 {
        return Ok(Ch::C(bytes::bytes_data(o)[0] as u32));
    }
    if pystr::is_str(o) && pystr::str_len(o) == 1 {
        let c = crate::types::str::str_data(o).get(0);
        if c < 128 {
            return Ok(Ch::C(c));
        }
        encode_check(o, enc)?;
        return Ok(Ch::W(c));
    }
    if pystr::is_str(o) || bytes::is_bytes(o) {
        raise!(TypeError, "expect int or bytes or str of length 1, got a {} of length {}", tname(o), ab::length(o)?);
    }
    raise!(TypeError, "expect int or bytes or str of length 1, got {}", tname(o))
}

fn cchar(c: u32, attr: u32) -> PyResult<CChar> {
    let mut cc = CChar::zero();
    let wc = [c as i32, 0];
    let pair = ((attr & A_COLOR) >> 8) as c_short;
    let r = unsafe { (api().setcchar)(&mut cc, wc.as_ptr(), attr & !A_COLOR, pair, std::ptr::null()) };
    if r == ERR {
        return Err(cerror("setcchar() returned ERR"));
    }
    Ok(cc)
}

pub enum Txt {
    B(Vec<u8>),
    W(Vec<i32>),
}

fn txt_arg(o: &PyObject, enc: &str) -> PyResult<Txt> {
    if pystr::is_str(o) {
        let d = crate::types::str::str_data(o);
        let n = pystr::str_len(o);
        let mut v = Vec::with_capacity(n + 1);
        for i in 0..n {
            let c = d.get(i);
            if c == 0 {
                raise!(ValueError, "embedded null character");
            }
            v.push(c as i32);
        }
        if v.iter().any(|c| *c >= 128) {
            encode_check(o, enc)?;
        }
        v.push(0);
        return Ok(Txt::W(v));
    }
    if bytes::is_bytes(o) {
        let b = bytes::bytes_data(o);
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        let mut v = b.to_vec();
        v.push(0);
        return Ok(Txt::B(v));
    }
    raise!(TypeError, "expect bytes or str, got {}", tname(o))
}

// ------------------------------------------------------------------ attributes

pub const A_COLOR: u32 = 0xff00;
pub const A_CHARTEXT: u32 = 0xff;
pub const A_ATTRIBUTES: u32 = 0xffff_ff00;

const fn bit(n: u32) -> u32 {
    1u32 << (n + 8)
}

// ------------------------------------------------------------------ window objects

pub struct Win {
    pub win: W,
    parent: Option<PyRef>,
    owned: bool,
    pub encoding: String,
}

impl Trav for Win {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        crate::trav!(v; self.parent)
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        if self.owned && !self.win.is_null() {
            if let Ok(a) = nc_api() {
                unsafe {
                    if self.win != *a.stdscr {
                        (a.delwin)(self.win);
                    }
                }
            }
        }
        self.win = std::ptr::null_mut();
    }
}

#[unsafe(no_mangle)]
pub static mut RPY_CURSES_WINDOW_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn window_type() -> *mut PyTypeObject {
    &raw mut RPY_CURSES_WINDOW_TYPE
}

pub fn is_window(o: &PyObject) -> bool {
    unsafe { typeobject::is_subtype(o.type_ptr(), window_type()) }
}

pub fn win_of(o: &PyObject) -> &mut Win {
    fc::rs::<Win>(o)
}

fn default_encoding() -> String {
    if let Some(e) = SCREEN_ENCODING.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return e;
    }
    let cs = unsafe { libc::nl_langinfo(libc::CODESET) };
    let mut enc = if cs.is_null() { String::new() } else { unsafe { CStr::from_ptr(cs) }.to_string_lossy().into_owned() };
    if enc.is_empty() {
        enc = "utf-8".into();
    }
    // normalise through the codec registry (e.g. ANSI_X3.4-1968 -> ascii)
    let norm = (|| -> PyResult<String> {
        let codecs = crate::import::import_absolute("codecs")?;
        let info = ab::call_method_str(&codecs, "lookup", &[&pystr::new_str(&enc)])?;
        let n = ab::getattr_str(&info, "name")?;
        Ok(pystr::to_string_lossy(&n))
    })();
    match norm {
        Ok(n) => n,
        Err(_) => {
            clear_exc();
            "utf-8".into()
        }
    }
}

pub fn new_window(w: W, parent: Option<PyRef>, owned: bool, encoding: Option<String>) -> PyResult {
    if w.is_null() {
        return Err(cerror("curses function returned NULL"));
    }
    let encoding = match encoding {
        Some(e) => e,
        None => default_encoding(),
    };
    fc::robj_new(window_type(), Win { win: w, parent, owned, encoding })
}

/// Split `[y, x,] rest...`: y/x are present when more than `max_rest` args were given.
fn split_yx<'a>(fname: &str, args: &'a [&'a PyObject], min_rest: usize, max_rest: usize) -> PyResult<(Option<(c_int, c_int)>, &'a [&'a PyObject])> {
    let n = args.len();
    if n >= min_rest && n <= max_rest {
        return Ok((None, args));
    }
    if n >= min_rest + 2 && n <= max_rest + 2 {
        return Ok((Some((ci(args[0])?, ci(args[1])?)), &args[2..]));
    }
    let lo = min_rest;
    let hi = max_rest + 2;
    raise!(TypeError, "{} requires {} to {} arguments", fname, lo, hi)
}

fn do_move(win: W, yx: Option<(c_int, c_int)>, mvname: &str) -> PyResult<()> {
    if let Some((y, x)) = yx {
        if unsafe { (api().wmove)(win, y, x) } == ERR {
            return Err(ret_err(mvname));
        }
    }
    Ok(())
}

fn attr_arg(o: Option<&&PyObject>) -> PyResult<u32> {
    match o {
        None => Ok(0),
        Some(a) => {
            let v = big(a)?;
            if v < i64::MIN as i128 || v > u32::MAX as i128 {
                raise!(OverflowError, "attribute value is out of range");
            }
            Ok(v as u32)
        }
    }
}

/// Run `f` with the window rendition temporarily set to `attr` (if given).
fn with_attr<T>(win: W, attr: Option<u32>, f: impl FnOnce() -> T) -> T {
    let a = api();
    match attr {
        None => f(),
        Some(at) => unsafe {
            let mut old: u32 = 0;
            let mut pair: c_short = 0;
            (a.wattr_get)(win, &mut old, &mut pair, std::ptr::null_mut());
            (a.wattrset)(win, at as c_int);
            let r = f();
            (a.wattr_set)(win, old, pair, std::ptr::null_mut());
            r
        },
    }
}

macro_rules! wfn0 {
    ($name:ident, $cfn:ident, $label:expr) => {
        fn $name(o: &PyObject) -> PyResult {
            let w = win_of(o).win;
            check(unsafe { (api().$cfn)(w) }, $label)
        }
    };
}
macro_rules! wvoid0 {
    ($name:ident, $cfn:ident) => {
        fn $name(o: &PyObject) -> PyResult {
            let w = win_of(o).win;
            unsafe { (api().$cfn)(w) };
            Ok(none::none_ref())
        }
    };
}
macro_rules! wflag {
    ($name:ident, $cfn:ident, $label:expr) => {
        fn $name(o: &PyObject, v: &PyObject) -> PyResult {
            let w = win_of(o).win;
            let b = ab::is_true(v)? as c_int;
            check(unsafe { (api().$cfn)(w, b) as c_int }, $label)
        }
    };
}
macro_rules! wflag_void {
    ($name:ident, $cfn:ident) => {
        fn $name(o: &PyObject, v: &PyObject) -> PyResult {
            let w = win_of(o).win;
            let b = ab::is_true(v)? as c_int;
            unsafe { (api().$cfn)(w, b) };
            Ok(none::none_ref())
        }
    };
}

wfn0!(w_clear, wclear, "wclear");
wfn0!(w_erase, werase, "werase");
wfn0!(w_clrtobot, wclrtobot, "wclrtobot");
wfn0!(w_clrtoeol, wclrtoeol, "wclrtoeol");
wfn0!(w_deleteln, wdeleteln, "wdeleteln");
wfn0!(w_insertln, winsertln, "winsertln");
wfn0!(w_standend, wstandend, "wstandend");
wfn0!(w_standout, wstandout, "wstandout");
wvoid0!(w_cursyncup, wcursyncup);
wvoid0!(w_syncup, wsyncup);
wvoid0!(w_syncdown, wsyncdown);
wflag!(w_clearok, clearok, "clearok");
wflag!(w_idlok, idlok, "idlok");
wflag!(w_leaveok, leaveok, "leaveok");
wflag!(w_scrollok, scrollok, "scrollok");
wflag!(w_keypad, keypad, "keypad");
wflag!(w_nodelay, nodelay, "nodelay");
wflag!(w_notimeout, notimeout, "notimeout");
wflag!(w_syncok, syncok, "syncok");
wflag_void!(w_idcok, idcok);
wflag_void!(w_immedok, immedok);

fn w_redrawwin(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    check(unsafe { (a.wredrawln)(w, 0, (a.getmaxy)(w)) }, "redrawwin")
}

fn w_touchwin(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    check(unsafe { (a.wtouchln)(w, 0, (a.getmaxy)(w), 1) }, "touchwin")
}

fn w_untouchwin(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    check(unsafe { (a.wtouchln)(w, 0, (a.getmaxy)(w), 0) }, "untouchwin")
}

fn pair_of(a: c_int, b: c_int) -> PyResult {
    Ok(tuple::new_tuple(vec![int::new_int(a as i64), int::new_int(b as i64)]))
}

fn w_getyx(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    unsafe { pair_of((a.getcury)(w), (a.getcurx)(w)) }
}
fn w_getbegyx(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    unsafe { pair_of((a.getbegy)(w), (a.getbegx)(w)) }
}
fn w_getmaxyx(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    unsafe { pair_of((a.getmaxy)(w), (a.getmaxx)(w)) }
}
fn w_getparyx(o: &PyObject) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    unsafe { pair_of((a.getpary)(w), (a.getparx)(w)) }
}

fn two_ints(fname: &str, args: &[&PyObject]) -> PyResult<(c_int, c_int)> {
    crate::args::check_pos(fname, args, 2, 2)?;
    Ok((ci(args[0])?, ci(args[1])?))
}

fn w_move(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (y, x) = two_ints("move", args)?;
    check(unsafe { (api().wmove)(win_of(o).win, y, x) }, "wmove")
}
fn w_mvwin(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (y, x) = two_ints("mvwin", args)?;
    check(unsafe { (api().mvwin)(win_of(o).win, y, x) }, "mvwin")
}
fn w_mvderwin(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (y, x) = two_ints("mvderwin", args)?;
    check(unsafe { (api().mvderwin)(win_of(o).win, y, x) }, "mvderwin")
}
fn w_resize(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (l, c) = two_ints("resize", args)?;
    check(unsafe { (api().wresize)(win_of(o).win, l, c) }, "wresize")
}
fn w_setscrreg(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (t, b) = two_ints("setscrreg", args)?;
    check(unsafe { (api().wsetscrreg)(win_of(o).win, t, b) }, "wsetscrreg")
}
fn w_redrawln(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (b, n) = two_ints("redrawln", args)?;
    check(unsafe { (api().wredrawln)(win_of(o).win, b, n) }, "wredrawln")
}
fn w_enclose(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (y, x) = two_ints("enclose", args)?;
    bool_ref(unsafe { (api().wenclose)(win_of(o).win, y, x) } != 0)
}

fn w_timeout(o: &PyObject, d: &PyObject) -> PyResult {
    let d = ci(d)?;
    unsafe { (api().wtimeout)(win_of(o).win, d) };
    Ok(none::none_ref())
}

fn w_insdelln(o: &PyObject, n: &PyObject) -> PyResult {
    let n = ci(n)?;
    check(unsafe { (api().winsdelln)(win_of(o).win, n) }, "winsdelln")
}

fn w_scroll(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("scroll", args, 0, 1)?;
    let n = match args.first() {
        Some(v) => ci(v)?,
        None => 1,
    };
    check(unsafe { (api().wscrl)(win_of(o).win, n) }, "scroll")
}

fn attr_long(v: &PyObject) -> PyResult<c_int> {
    let x = big(v)?;
    Ok(x as i64 as c_int)
}

fn w_attron(o: &PyObject, v: &PyObject) -> PyResult {
    let a = attr_long(v)?;
    check(unsafe { (api().wattron)(win_of(o).win, a) }, "wattron")
}
fn w_attroff(o: &PyObject, v: &PyObject) -> PyResult {
    let a = attr_long(v)?;
    check(unsafe { (api().wattroff)(win_of(o).win, a) }, "wattroff")
}
fn w_attrset(o: &PyObject, v: &PyObject) -> PyResult {
    let a = attr_long(v)?;
    check(unsafe { (api().wattrset)(win_of(o).win, a) }, "wattrset")
}

fn w_is_wintouched(o: &PyObject) -> PyResult {
    bool_ref(unsafe { (api().is_wintouched)(win_of(o).win) } != 0)
}

fn w_is_linetouched(o: &PyObject, l: &PyObject) -> PyResult {
    let l = ci(l)?;
    let w = win_of(o).win;
    if l < 0 || l >= unsafe { (api().getmaxy)(w) } {
        return Err(cerror("is_linetouched: line number outside of boundaries"));
    }
    bool_ref(unsafe { (api().is_linetouched)(w, l) } != 0)
}

fn w_touchline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("touchline", args, 2, 3)?;
    let s = ci(args[0])?;
    let n = ci(args[1])?;
    let changed = flag(args.get(2), true)?;
    check(unsafe { (api().wtouchln)(win_of(o).win, s, n, changed as c_int) }, "touchline")
}

// ---- output

fn put_ch(w: &Win, ch: Ch, attr: u32, wide_fn: unsafe extern "C" fn(W, *const CChar) -> c_int, narrow: unsafe extern "C" fn(W, u32) -> c_int) -> c_int {
    match ch {
        Ch::C(c) => unsafe { narrow(w.win, c | attr) },
        Ch::W(c) => match cchar(c, attr) {
            Ok(cc) => unsafe { wide_fn(w.win, &cc) },
            Err(_) => {
                clear_exc();
                ERR
            }
        },
    }
}

fn w_addch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, rest) = split_yx("addch", args, 1, 2)?;
    let w = win_of(o);
    let ch = ch_arg(rest[0], &w.encoding)?;
    let attr = attr_arg(rest.get(1))?;
    do_move(w.win, yx, "mvwaddch")?;
    let a = api();
    check(put_ch(w, ch, attr, a.wadd_wch, a.waddch), if yx.is_some() { "mvwaddch" } else { "waddch" })
}

fn w_insch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, rest) = split_yx("insch", args, 1, 2)?;
    let w = win_of(o);
    let ch = ch_arg(rest[0], &w.encoding)?;
    let attr = attr_arg(rest.get(1))?;
    do_move(w.win, yx, "mvwinsch")?;
    let a = api();
    check(put_ch(w, ch, attr, a.wins_wch, a.winsch), if yx.is_some() { "mvwinsch" } else { "winsch" })
}

fn w_echochar(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("echochar", args, 1, 2)?;
    let w = win_of(o);
    let ch = ch_arg(args[0], &w.encoding)?;
    let attr = attr_arg(args.get(1))?;
    let a = api();
    let pad = unsafe { (a.is_pad)(w.win) } != 0;
    if pad {
        check(put_ch(w, ch, attr, a.pecho_wchar, a.pechochar), "pechochar")
    } else {
        check(put_ch(w, ch, attr, a.wecho_wchar, a.wechochar), "wechochar")
    }
}

fn put_str(fname: &str, o: &PyObject, args: &[&PyObject], with_n: bool, insert: bool) -> PyResult {
    let (min, max) = if with_n { (2, 3) } else { (1, 2) };
    let (yx, rest) = split_yx(fname, args, min, max)?;
    let w = win_of(o);
    let t = txt_arg(rest[0], &w.encoding)?;
    let n = if with_n { ci(rest[1])? } else { -1 };
    let attr_o = rest.get(if with_n { 2 } else { 1 });
    let attr = match attr_o {
        Some(_) => Some(attr_arg(attr_o)?),
        None => None,
    };
    let a = api();
    let win = w.win;
    let r = with_attr(win, attr, || unsafe {
        if let Some((y, x)) = yx {
            if (a.wmove)(win, y, x) == ERR {
                return ERR;
            }
        }
        match (&t, insert) {
            (Txt::B(b), false) => (a.waddnstr)(win, b.as_ptr() as *const c_char, n),
            (Txt::W(v), false) => (a.waddnwstr)(win, v.as_ptr(), n),
            (Txt::B(b), true) => (a.winsnstr)(win, b.as_ptr() as *const c_char, n),
            (Txt::W(v), true) => (a.wins_nwstr)(win, v.as_ptr(), n),
        }
    });
    let label = match (insert, with_n, yx.is_some()) {
        (false, false, false) => "addstr",
        (false, false, true) => "mvwaddstr",
        (false, true, false) => "addnstr",
        (false, true, true) => "mvwaddnstr",
        (true, false, false) => "insstr",
        (true, false, true) => "mvwinsstr",
        (true, true, false) => "insnstr",
        (true, true, true) => "mvwinsnstr",
    };
    check(r, label)
}

fn w_addstr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    put_str("addstr", o, args, false, false)
}
fn w_addnstr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    put_str("addnstr", o, args, true, false)
}
fn w_insstr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    put_str("insstr", o, args, false, true)
}
fn w_insnstr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    put_str("insnstr", o, args, true, true)
}

fn line_ch(o: &PyObject, w: &Win) -> PyResult<Ch> {
    ch_arg(o, &w.encoding)
}

fn draw_line(o: &PyObject, args: &[&PyObject], vertical: bool) -> PyResult {
    let fname = if vertical { "vline" } else { "hline" };
    let (yx, rest) = split_yx(fname, args, 2, 3)?;
    let w = win_of(o);
    let ch = line_ch(rest[0], w)?;
    let n = ci(rest[1])?;
    let attr = attr_arg(rest.get(2))?;
    do_move(w.win, yx, if vertical { "mvwvline" } else { "mvwhline" })?;
    let a = api();
    let r = unsafe {
        match ch {
            Ch::C(c) => {
                if vertical { (a.wvline)(w.win, c | attr, n) } else { (a.whline)(w.win, c | attr, n) }
            }
            Ch::W(c) => {
                let cc = cchar(c, attr)?;
                if vertical { (a.wvline_set)(w.win, &cc, n) } else { (a.whline_set)(w.win, &cc, n) }
            }
        }
    };
    check(r, fname)
}

fn w_hline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    draw_line(o, args, false)
}
fn w_vline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    draw_line(o, args, true)
}

/// Border characters: wide characters are not supported by wborder, so a
/// non-ASCII str is mapped to its single byte in the window encoding.
fn border_ch(o: &PyObject, w: &Win) -> PyResult<u32> {
    match ch_arg(o, &w.encoding)? {
        Ch::C(c) => Ok(c),
        Ch::W(c) => {
            let b = crate::modules::codecs_registry::encode(o, &w.encoding, "strict")?;
            let d = bytes::bytes_data(&b);
            if d.len() == 1 { Ok(d[0] as u32) } else { Ok(c) }
        }
    }
}

fn w_border(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("border", args, 0, 8)?;
    let w = win_of(o);
    let mut ch = [0u32; 8];
    for (i, a) in args.iter().enumerate() {
        ch[i] = border_ch(a, w)?;
    }
    unsafe { (api().wborder)(w.win, ch[0], ch[1], ch[2], ch[3], ch[4], ch[5], ch[6], ch[7]) };
    Ok(none::none_ref())
}

fn w_box(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() == 1 || args.len() > 2 {
        raise!(TypeError, "box requires 0 or 2 arguments");
    }
    let w = win_of(o);
    let (v, h) = if args.len() == 2 { (border_ch(args[0], w)?, border_ch(args[1], w)?) } else { (0, 0) };
    unsafe { (api().wborder)(w.win, v, v, h, h, 0, 0, 0, 0) };
    Ok(none::none_ref())
}

fn w_chgat(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let w = win_of(o);
    let (yx, n, attr) = match args.len() {
        1 => (None, -1, attr_arg(Some(&args[0]))?),
        2 => (None, ci(args[0])?, attr_arg(Some(&args[1]))?),
        3 => (Some((ci(args[0])?, ci(args[1])?)), -1, attr_arg(Some(&args[2]))?),
        4 => (Some((ci(args[0])?, ci(args[1])?)), ci(args[2])?, attr_arg(Some(&args[3]))?),
        _ => raise!(TypeError, "chgat requires 1 to 4 arguments"),
    };
    let a = api();
    do_move(w.win, yx, "mvwchgat")?;
    let color = ((attr & A_COLOR) >> 8) as c_short;
    let r = unsafe { (a.wchgat)(w.win, n, attr, color, std::ptr::null()) };
    if r != ERR {
        let y = unsafe { (a.getcury)(w.win) };
        unsafe { (a.wtouchln)(w.win, y, 1, 1) };
    }
    check(r, "chgat")
}

fn w_delch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("delch", args, 0, 0)?;
    let w = win_of(o);
    do_move(w.win, yx, "mvwdelch")?;
    check(unsafe { (api().wdelch)(w.win) }, "wdelch")
}

fn w_bkgd(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("bkgd", args, 1, 2)?;
    let w = win_of(o);
    let attr = attr_arg(args.get(1))?;
    let a = api();
    let r = match ch_arg(args[0], &w.encoding)? {
        Ch::C(c) => unsafe { (a.wbkgd)(w.win, c | attr) },
        Ch::W(_) => unsafe { (a.wbkgd)(w.win, border_ch(args[0], w)? | attr) },
    };
    check(r, "bkgd")
}

fn w_bkgdset(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("bkgdset", args, 1, 2)?;
    let w = win_of(o);
    let attr = attr_arg(args.get(1))?;
    let c = match ch_arg(args[0], &w.encoding)? {
        Ch::C(c) => c,
        Ch::W(_) => border_ch(args[0], w)?,
    };
    unsafe { (api().wbkgdset)(w.win, c | attr) };
    Ok(none::none_ref())
}

fn w_getbkgd(o: &PyObject) -> PyResult {
    int_ref(unsafe { (api().getbkgd)(win_of(o).win) } as i64)
}

// ---- input from the window contents

fn w_inch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("inch", args, 0, 0)?;
    let w = win_of(o);
    do_move(w.win, yx, "mvwinch")?;
    let v = unsafe { (api().winch)(w.win) };
    if v == u32::MAX {
        return Err(ret_err("inch"));
    }
    int_ref(v as i64)
}

fn w_in_wch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("in_wch", args, 0, 0)?;
    let w = win_of(o);
    do_move(w.win, yx, "mvwin_wch")?;
    let a = api();
    let mut cc = CChar::zero();
    if unsafe { (a.win_wch)(w.win, &mut cc) } == ERR {
        return Err(ret_err("in_wch"));
    }
    let mut wc = [0i32; 8];
    let mut attr: u32 = 0;
    let mut pair: c_short = 0;
    unsafe { (a.getcchar)(&cc, wc.as_mut_ptr(), &mut attr, &mut pair, std::ptr::null_mut()) };
    let cps: Vec<u32> = wc.iter().take_while(|c| **c != 0).map(|c| *c as u32).collect();
    Ok(pystr::from_codepoints(&cps))
}

fn w_instr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, rest) = split_yx("instr", args, 0, 1)?;
    let w = win_of(o);
    let n = match rest.first() {
        Some(v) => ci(v)?,
        None => 1023,
    };
    if n < 0 {
        raise!(ValueError, "'n' must be nonnegative");
    }
    let n = n.min(1023);
    do_move(w.win, yx, "mvwinnstr")?;
    let mut buf = vec![0u8; n as usize + 1];
    let r = unsafe { (api().winnstr)(w.win, buf.as_mut_ptr() as *mut c_char, n) };
    if r == ERR {
        return Ok(bytes::new_bytes(b""));
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(bytes::new_bytes(&buf[..len]))
}

// ---- keyboard input

fn read_key(w: W) -> c_int {
    let a = api();
    crate::threads::blocking(|| unsafe { (a.wgetch)(w) })
}

fn w_getch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("getch", args, 0, 0)?;
    let w = win_of(o).win;
    do_move(w, yx, "mvwgetch")?;
    let r = read_key(w);
    int_ref(r as i64)
}

fn w_getkey(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("getkey", args, 0, 0)?;
    let w = win_of(o).win;
    do_move(w, yx, "mvwgetch")?;
    let r = read_key(w);
    if r == ERR {
        crate::modules::signal::check_signals()?;
        return Err(cerror("no input"));
    }
    if r <= 255 {
        return Ok(pystr::from_codepoints(&[r as u32]));
    }
    let p = unsafe { (api().keyname)(r) };
    if p.is_null() {
        return Ok(pystr::new_str(""));
    }
    crate::modules::posix::path::fs_decode(unsafe { CStr::from_ptr(p) }.to_bytes())
}

fn w_get_wch(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, _) = split_yx("get_wch", args, 0, 0)?;
    let w = win_of(o).win;
    do_move(w, yx, "mvwget_wch")?;
    let a = api();
    let mut c: u32 = 0;
    let r = crate::threads::blocking(|| unsafe { (a.wget_wch)(w, &mut c) });
    if r == ERR {
        crate::modules::signal::check_signals()?;
        return Err(cerror("no input"));
    }
    if r == 0o400 {
        return int_ref(c as i64);
    }
    Ok(pystr::from_codepoints(&[c]))
}

fn w_getstr(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (yx, rest) = split_yx("getstr", args, 0, 1)?;
    let w = win_of(o).win;
    let n = match rest.first() {
        Some(v) => ci(v)?,
        None => 1023,
    };
    if n < 0 {
        raise!(ValueError, "'n' must be nonnegative");
    }
    let n = n.min(1023);
    do_move(w, yx, "mvwgetnstr")?;
    let mut buf = vec![0u8; n as usize + 1];
    let a = api();
    let r = crate::threads::blocking(|| unsafe { (a.wgetnstr)(w, buf.as_mut_ptr() as *mut c_char, n) });
    if r == ERR {
        return Ok(bytes::new_bytes(b""));
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(bytes::new_bytes(&buf[..len]))
}

// ---- refresh

fn six_ints(args: &[&PyObject]) -> PyResult<[c_int; 6]> {
    let mut v = [0; 6];
    for i in 0..6 {
        v[i] = ci(args[i])?;
    }
    Ok(v)
}

fn refresh_impl(o: &PyObject, args: &[&PyObject], out: bool) -> PyResult {
    let w = win_of(o).win;
    let a = api();
    let pad = unsafe { (a.is_pad)(w) } != 0;
    if pad {
        if args.len() != 6 {
            raise!(TypeError, "{}() requires 6 arguments", if out { "noutrefresh" } else { "refresh" });
        }
        let v = six_ints(args)?;
        let r = unsafe { if out { (a.pnoutrefresh)(w, v[0], v[1], v[2], v[3], v[4], v[5]) } else { (a.prefresh)(w, v[0], v[1], v[2], v[3], v[4], v[5]) } };
        return check(r, if out { "pnoutrefresh" } else { "prefresh" });
    }
    if !args.is_empty() {
        raise!(TypeError, "{}() takes no arguments (6 given)", if out { "noutrefresh" } else { "refresh" });
    }
    let r = unsafe { if out { (a.wnoutrefresh)(w) } else { (a.wrefresh)(w) } };
    check(r, if out { "wnoutrefresh" } else { "wrefresh" })
}

fn w_refresh(o: &PyObject, args: &[&PyObject]) -> PyResult {
    refresh_impl(o, args, false)
}
fn w_noutrefresh(o: &PyObject, args: &[&PyObject]) -> PyResult {
    refresh_impl(o, args, true)
}

// ---- sub-windows

fn sub_impl(o: &PyObject, args: &[&PyObject], derived: bool) -> PyResult {
    let fname = if derived { "derwin" } else { "subwin" };
    let (l, c, y, x) = match args.len() {
        2 => (0, 0, ci(args[0])?, ci(args[1])?),
        4 => (ci(args[0])?, ci(args[1])?, ci(args[2])?, ci(args[3])?),
        _ => raise!(TypeError, "{} requires 2 or 4 arguments", fname),
    };
    let w = win_of(o);
    let a = api();
    let pad = unsafe { (a.is_pad)(w.win) } != 0;
    let nw = unsafe {
        if derived {
            (a.derwin)(w.win, l, c, y, x)
        } else if pad {
            (a.subpad)(w.win, l, c, y, x)
        } else {
            (a.subwin)(w.win, l, c, y, x)
        }
    };
    if nw.is_null() {
        return Err(cerror(&format!("curses function returned NULL")));
    }
    let enc = w.encoding.clone();
    new_window(nw, Some(o.to_ref()), true, Some(enc))
}

fn w_subwin(o: &PyObject, args: &[&PyObject]) -> PyResult {
    sub_impl(o, args, false)
}
fn w_derwin(o: &PyObject, args: &[&PyObject]) -> PyResult {
    sub_impl(o, args, true)
}
fn w_subpad(o: &PyObject, args: &[&PyObject]) -> PyResult {
    sub_impl(o, args, false)
}

fn copy_impl(o: &PyObject, args: &[&PyObject], is_overlay: bool) -> PyResult {
    let fname = if is_overlay { "overlay" } else { "overwrite" };
    if args.len() != 1 && args.len() != 7 {
        raise!(TypeError, "{} requires one or seven arguments", fname);
    }
    if !is_window(args[0]) {
        raise!(TypeError, "{}() argument 1 must be _curses.window, not {}", fname, tname(args[0]));
    }
    let src = win_of(o).win;
    let dst = win_of(args[0]).win;
    let a = api();
    let r = unsafe {
        if args.len() == 7 {
            let mut v = [0; 6];
            for i in 0..6 {
                v[i] = ci(args[i + 1])?;
            }
            (a.copywin)(src, dst, v[0], v[1], v[2], v[3], v[4], v[5], is_overlay as c_int)
        } else if is_overlay {
            (a.overlay)(src, dst)
        } else {
            (a.overwrite)(src, dst)
        }
    };
    check(r, if args.len() == 7 { "copywin" } else { fname })
}

fn w_overlay(o: &PyObject, args: &[&PyObject]) -> PyResult {
    copy_impl(o, args, true)
}
fn w_overwrite(o: &PyObject, args: &[&PyObject]) -> PyResult {
    copy_impl(o, args, false)
}

fn w_putwin(o: &PyObject, file: &PyObject) -> PyResult {
    let w = win_of(o).win;
    unsafe {
        let f = libc::tmpfile();
        if f.is_null() {
            return Err(crate::modules::posix::path::os_err());
        }
        let r = (api().putwin)(w, f);
        if r == ERR {
            libc::fclose(f);
            return Err(ret_err("putwin"));
        }
        libc::fflush(f);
        libc::rewind(f);
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), f);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
        }
        libc::fclose(f);
        ab::call_method_str(file, "write", &[&bytes::new_bytes(&data)])?;
    }
    Ok(none::none_ref())
}

fn w_encoding_get(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&win_of(o).encoding))
}

fn w_encoding_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let Some(v) = v else { raise!(TypeError, "can't delete encoding attribute") };
    if !pystr::is_str(v) {
        raise!(TypeError, "setting encoding to a non-string");
    }
    win_of(o).encoding = pystr::as_utf8(v)?.to_string();
    Ok(())
}

fn init_window_type() -> PyResult<()> {
    unsafe {
        let t = window_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Win>(t, c"_curses.window", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_methods = mdefs![
            ("addch", FAST, w_addch, "addch([y, x,] ch, [attr=_curses.A_NORMAL])\nPaint the character ch at (y, x) with attributes attr,\noverwriting any character previously painted at that location.\nBy default, the character position and attributes are the\ncurrent settings for the window object."),
            ("addnstr", FAST, w_addnstr, "addnstr([y, x,] str, n, [attr])\nPaint at most n characters of the string str at (y, x) with attributes attr."),
            ("addstr", FAST, w_addstr, "addstr([y, x,] str, [attr])\nPaint the string str at (y, x) with attributes attr."),
            ("attroff", O, w_attroff, "attroff($self, attr, /)\n--\n\nRemove attribute attr from the \"background\" set."),
            ("attron", O, w_attron, "attron($self, attr, /)\n--\n\nAdd attribute attr from the \"background\" set."),
            ("attrset", O, w_attrset, "attrset($self, attr, /)\n--\n\nSet the \"background\" set of attributes."),
            ("bkgd", FAST, w_bkgd, "bkgd($self, ch, attr=_curses.A_NORMAL, /)\n--\n\nSet the background property of the window."),
            ("bkgdset", FAST, w_bkgdset, "bkgdset($self, ch, attr=_curses.A_NORMAL, /)\n--\n\nSet the window's background."),
            ("border", FAST, w_border, "border($self, ls=_curses.ACS_VLINE, rs=_curses.ACS_VLINE,\n       ts=_curses.ACS_HLINE, bs=_curses.ACS_HLINE,\n       tl=_curses.ACS_ULCORNER, tr=_curses.ACS_URCORNER,\n       bl=_curses.ACS_LLCORNER, br=_curses.ACS_LRCORNER, /)\n--\n\nDraw a border around the edges of the window."),
            ("box", FAST, w_box, "box([verch=0, horch=0])\nDraw a border around the edges of the window."),
            ("chgat", FAST, w_chgat, "chgat([y, x,] [n=-1,] attr)\nSet the attributes of characters."),
            ("clear", NOARGS, w_clear),
            ("clearok", O, w_clearok),
            ("clrtobot", NOARGS, w_clrtobot),
            ("clrtoeol", NOARGS, w_clrtoeol),
            ("cursyncup", NOARGS, w_cursyncup),
            ("delch", FAST, w_delch, "delch([y, x])\nDelete any character at (y, x)."),
            ("deleteln", NOARGS, w_deleteln),
            ("derwin", FAST, w_derwin, "derwin([nlines=0, ncols=0,] begin_y, begin_x)\nCreate a sub-window (window-relative coordinates)."),
            ("echochar", FAST, w_echochar, "echochar($self, ch, attr=_curses.A_NORMAL, /)\n--\n\nAdd character ch with attribute attr, and refresh."),
            ("enclose", FAST, w_enclose, "enclose($self, y, x, /)\n--\n\nReturn True if the screen-relative coordinates are enclosed by the window."),
            ("erase", NOARGS, w_erase),
            ("getbegyx", NOARGS, w_getbegyx),
            ("getbkgd", NOARGS, w_getbkgd, "getbkgd($self, /)\n--\n\nReturn the window's current background character/attribute pair."),
            ("getch", FAST, w_getch, "getch([y, x])\nGet a character code from terminal keyboard."),
            ("getkey", FAST, w_getkey, "getkey([y, x])\nGet a character (string) from terminal keyboard."),
            ("get_wch", FAST, w_get_wch, "get_wch([y, x])\nGet a wide character from terminal keyboard."),
            ("getmaxyx", NOARGS, w_getmaxyx),
            ("getparyx", NOARGS, w_getparyx),
            ("getstr", FAST, w_getstr, "getstr([[y, x,] n=1023])\nRead a string from the user, with primitive line editing capacity."),
            ("getyx", NOARGS, w_getyx),
            ("hline", FAST, w_hline, "hline([y, x,] ch, n, [attr=_curses.A_NORMAL])\nDisplay a horizontal line."),
            ("idcok", O, w_idcok),
            ("idlok", O, w_idlok),
            ("immedok", O, w_immedok),
            ("inch", FAST, w_inch, "inch([y, x])\nReturn the character at the given position in the window."),
            ("in_wch", FAST, w_in_wch, "in_wch([y, x])\nReturn the wide character at the given position in the window."),
            ("insch", FAST, w_insch, "insch([y, x,] ch, [attr=_curses.A_NORMAL])\nInsert a character before the current or specified position."),
            ("insdelln", O, w_insdelln),
            ("insertln", NOARGS, w_insertln),
            ("insnstr", FAST, w_insnstr, "insnstr([y, x,] str, n, [attr])\nInsert at most n characters of the string."),
            ("insstr", FAST, w_insstr, "insstr([y, x,] str, [attr])\nInsert the string before the current or specified position."),
            ("instr", FAST, w_instr, "instr([y, x,] n=2047)\nReturn a string of characters, extracted from the window."),
            ("is_linetouched", O, w_is_linetouched, "is_linetouched($self, line, /)\n--\n\nReturn True if the specified line was modified."),
            ("is_wintouched", NOARGS, w_is_wintouched),
            ("keypad", O, w_keypad),
            ("leaveok", O, w_leaveok),
            ("move", FAST, w_move),
            ("mvderwin", FAST, w_mvderwin),
            ("mvwin", FAST, w_mvwin),
            ("nodelay", O, w_nodelay),
            ("notimeout", O, w_notimeout),
            ("noutrefresh", FAST, w_noutrefresh, "noutrefresh([pminrow, pmincol, sminrow, smincol, smaxrow, smaxcol])\nMark for refresh but wait."),
            ("overlay", FAST, w_overlay, "overlay(destwin, [sminrow, smincol, dminrow, dmincol, dmaxrow, dmaxcol])\nOverlay the window on top of destwin."),
            ("overwrite", FAST, w_overwrite, "overwrite(destwin, [sminrow, smincol, dminrow, dmincol, dmaxrow, dmaxcol])\nOverwrite the window on top of destwin."),
            ("putwin", O, w_putwin, "putwin($self, file, /)\n--\n\nWrite all data associated with the window into the provided file object."),
            ("redrawln", FAST, w_redrawln),
            ("redrawwin", NOARGS, w_redrawwin),
            ("refresh", FAST, w_refresh, "refresh([pminrow, pmincol, sminrow, smincol, smaxrow, smaxcol])\nUpdate the display immediately."),
            ("resize", FAST, w_resize),
            ("scroll", FAST, w_scroll, "scroll([lines=1])\nScroll the screen or scrolling region."),
            ("scrollok", O, w_scrollok),
            ("setscrreg", FAST, w_setscrreg),
            ("standend", NOARGS, w_standend),
            ("standout", NOARGS, w_standout),
            ("subpad", FAST, w_subpad, "subpad([nlines=0, ncols=0,] begin_y, begin_x)\nCreate a sub-pad."),
            ("subwin", FAST, w_subwin, "subwin([nlines=0, ncols=0,] begin_y, begin_x)\nCreate a sub-window (screen-relative coordinates)."),
            ("syncdown", NOARGS, w_syncdown),
            ("syncok", O, w_syncok),
            ("syncup", NOARGS, w_syncup),
            ("timeout", O, w_timeout),
            ("touchline", FAST, w_touchline, "touchline(start, count, [changed=True])\nPretend count lines have been changed, starting with line start."),
            ("touchwin", NOARGS, w_touchwin),
            ("untouchwin", NOARGS, w_untouchwin),
            ("vline", FAST, w_vline, "vline([y, x,] ch, n, [attr=_curses.A_NORMAL])\nDisplay a vertical line."),
        ];
        (*t).tp_getset = crate::getset_table![("encoding", w_encoding_get, w_encoding_set; "the typecode character used to create the array")];
        typeobject::type_ready(t)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ module functions

macro_rules! nfn0 {
    ($name:ident, $cfn:ident, $label:expr) => {
        fn $name(_m: &PyObject) -> PyResult {
            need_initscr()?;
            check(unsafe { (api().$cfn)() }, $label)
        }
    };
}

nfn0!(m_beep, beep, "beep");
nfn0!(m_flash, flash, "flash");
nfn0!(m_def_prog_mode, def_prog_mode, "def_prog_mode");
nfn0!(m_def_shell_mode, def_shell_mode, "def_shell_mode");
nfn0!(m_reset_prog_mode, reset_prog_mode, "reset_prog_mode");
nfn0!(m_reset_shell_mode, reset_shell_mode, "reset_shell_mode");
nfn0!(m_resetty, resetty, "resetty");
nfn0!(m_savetty, savetty, "savetty");
nfn0!(m_nocbreak, nocbreak, "nocbreak");
nfn0!(m_noecho, noecho, "noecho");
nfn0!(m_nonl, nonl, "nonl");
nfn0!(m_noraw, noraw, "noraw");
nfn0!(m_flushinp, flushinp, "flushinp");
nfn0!(m_use_default_colors, use_default_colors, "use_default_colors");

fn m_endwin(_m: &PyObject) -> PyResult {
    need_initscr()?;
    check(unsafe { (api().endwin)() }, "endwin")
}

fn m_doupdate(_m: &PyObject) -> PyResult {
    need_initscr()?;
    check(unsafe { (api().doupdate)() }, "doupdate")
}

fn m_isendwin(_m: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().isendwin)() } != 0)
}

fn toggle(args: &[&PyObject], fname: &str, on: unsafe extern "C" fn() -> c_int, off: unsafe extern "C" fn() -> c_int) -> PyResult {
    crate::args::check_pos(fname, args, 0, 1)?;
    need_initscr()?;
    let f = flag(args.first(), true)?;
    let r = unsafe { if f { on() } else { off() } };
    if r == ERR {
        return Err(ret_err(&if f { fname.to_string() } else { format!("no{}", fname) }));
    }
    Ok(none::none_ref())
}

fn m_cbreak(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let a = api();
    toggle(args, "cbreak", a.cbreak, a.nocbreak)
}
fn m_echo(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let a = api();
    toggle(args, "echo", a.echo, a.noecho)
}
fn m_nl(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let a = api();
    toggle(args, "nl", a.nl, a.nonl)
}
fn m_raw(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let a = api();
    toggle(args, "raw", a.raw, a.noraw)
}

fn m_qiflush(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("qiflush", args, 0, 1)?;
    need_initscr()?;
    let a = api();
    if flag(args.first(), true)? {
        unsafe { (a.qiflush)() }
    } else {
        unsafe { (a.noqiflush)() }
    }
    Ok(none::none_ref())
}

fn m_noqiflush(_m: &PyObject) -> PyResult {
    need_initscr()?;
    unsafe { (api().noqiflush)() };
    Ok(none::none_ref())
}

fn m_intrflush(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let b = ab::is_true(v)?;
    check(unsafe { (api().intrflush)(std::ptr::null_mut(), b as c_int) }, "intrflush")
}

fn m_meta(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let b = ab::is_true(v)?;
    check(unsafe { (api().meta)(*api().stdscr, b as c_int) }, "meta")
}

fn m_halfdelay(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let t = big(v)?;
    if !(0..=255).contains(&t) {
        raise!(OverflowError, "unsigned byte integer is out of range");
    }
    check(unsafe { (api().halfdelay)(t as c_int) }, "halfdelay")
}

fn m_curs_set(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let r = unsafe { (api().curs_set)(ci(v)?) };
    if r == ERR {
        return Err(ret_err("curs_set"));
    }
    int_ref(r as i64)
}

fn m_delay_output(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    check(unsafe { (api().delay_output)(ci(v)?) }, "delay_output")
}

fn m_napms(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let ms = ci(v)?;
    let a = api();
    int_ref(crate::threads::blocking(|| unsafe { (a.napms)(ms) }) as i64)
}

fn m_baudrate(_m: &PyObject) -> PyResult {
    need_initscr()?;
    int_ref(unsafe { (api().baudrate)() } as i64)
}

fn m_erasechar(_m: &PyObject) -> PyResult {
    need_initscr()?;
    let c = unsafe { (api().erasechar)() } as u8;
    Ok(bytes::new_bytes(&[c]))
}

fn m_killchar(_m: &PyObject) -> PyResult {
    need_initscr()?;
    let c = unsafe { (api().killchar)() } as u8;
    Ok(bytes::new_bytes(&[c]))
}

fn m_has_colors(_m: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().has_colors)() } != 0)
}
fn m_can_change_color(_m: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().can_change_color)() } != 0)
}
fn m_has_ic(_m: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().has_ic)() } != 0)
}
fn m_has_il(_m: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().has_il)() } != 0)
}
fn m_has_key(_m: &PyObject, k: &PyObject) -> PyResult {
    need_initscr()?;
    bool_ref(unsafe { (api().has_key)(ci(k)?) } != 0)
}
fn m_has_extended_color_support(_m: &PyObject) -> PyResult {
    bool_ref(api().extended_color_content.is_some())
}

fn m_termattrs(_m: &PyObject) -> PyResult {
    need_initscr()?;
    int_ref(unsafe { (api().termattrs)() } as i64)
}
fn m_termname(_m: &PyObject) -> PyResult {
    need_initscr()?;
    cstr_bytes(unsafe { (api().termname)() })
}
fn m_longname(_m: &PyObject) -> PyResult {
    need_initscr()?;
    cstr_bytes(unsafe { (api().longname)() })
}

fn m_filter(_m: &PyObject) -> PyResult {
    unsafe { (api().filter)() };
    Ok(none::none_ref())
}

fn m_use_env(_m: &PyObject, v: &PyObject) -> PyResult {
    let b = ab::is_true(v)?;
    unsafe { (api().use_env)(b as c_int) };
    Ok(none::none_ref())
}

fn m_typeahead(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    check(unsafe { (api().typeahead)(ci(v)?) }, "typeahead")
}

fn m_get_escdelay(_m: &PyObject) -> PyResult {
    int_ref(unsafe { (api().get_escdelay)() } as i64)
}
fn m_set_escdelay(_m: &PyObject, v: &PyObject) -> PyResult {
    let d = ci(v)?;
    if d <= 0 {
        raise!(ValueError, "ms must be > 0");
    }
    check(unsafe { (api().set_escdelay)(d) }, "set_escdelay")
}
fn m_get_tabsize(_m: &PyObject) -> PyResult {
    int_ref(unsafe { *api().TABSIZE } as i64)
}
fn m_set_tabsize(_m: &PyObject, v: &PyObject) -> PyResult {
    let d = ci(v)?;
    if d <= 0 {
        raise!(ValueError, "size must be > 0");
    }
    check(unsafe { (api().set_tabsize)(d) }, "set_tabsize")
}

fn m_getsyx(_m: &PyObject) -> PyResult {
    need_initscr()?;
    let a = api();
    unsafe {
        let ns = *a.newscr;
        if ns.is_null() || (a.is_leaveok)(ns) != 0 {
            return pair_of(-1, -1);
        }
        pair_of((a.getcury)(ns), (a.getcurx)(ns))
    }
}

fn m_setsyx(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (y, x) = two_ints("setsyx", args)?;
    need_initscr()?;
    let a = api();
    unsafe {
        let ns = *a.newscr;
        if ns.is_null() {
            return Ok(none::none_ref());
        }
        if y == -1 && x == -1 {
            (a.leaveok)(ns, 1);
        } else {
            (a.leaveok)(ns, 0);
            (a.wmove)(ns, y, x);
        }
    }
    Ok(none::none_ref())
}

fn m_keyname(_m: &PyObject, k: &PyObject) -> PyResult {
    let k = ci(k)?;
    if k < 0 {
        raise!(ValueError, "invalid key number");
    }
    cstr_bytes(unsafe { (api().keyname)(k) })
}

fn m_unctrl(_m: &PyObject, c: &PyObject) -> PyResult {
    let enc = default_encoding();
    let v = match ch_arg(c, &enc)? {
        Ch::C(v) => v,
        Ch::W(_) => {
            let b = crate::modules::codecs_registry::encode(c, &enc, "strict")?;
            let d = bytes::bytes_data(&b);
            if d.len() != 1 {
                raise!(OverflowError, "byte doesn't fit in chtype");
            }
            d[0] as u32
        }
    };
    cstr_bytes(unsafe { (api().unctrl)(v) })
}

fn m_ungetch(_m: &PyObject, c: &PyObject) -> PyResult {
    need_initscr()?;
    let enc = default_encoding();
    let v = match ch_arg(c, &enc)? {
        Ch::C(v) => v,
        Ch::W(v) => v,
    };
    check(unsafe { (api().ungetch)(v as c_int) }, "ungetch")
}

fn m_unget_wch(_m: &PyObject, c: &PyObject) -> PyResult {
    need_initscr()?;
    let v: u32 = if pystr::is_str(c) {
        if pystr::str_len(c) != 1 {
            raise!(TypeError, "expect str of length 1 or int, got a str of length {}", pystr::str_len(c));
        }
        crate::types::str::str_data(c).get(0)
    } else if int::is_int(c) {
        let v = big(c)?;
        if v < 0 || v > 0x10ffff {
            raise!(OverflowError, "character code is out of range");
        }
        v as u32
    } else {
        raise!(TypeError, "expect str of length 1 or int, got {}", tname(c));
    };
    check(unsafe { (api().unget_wch)(v as i32) }, "unget_wch")
}

fn m_initscr(_m: &PyObject) -> PyResult {
    let a = api();
    if INITSCR_CALLED.load(Ordering::Acquire) {
        unsafe { (a.wrefresh)(*a.stdscr) };
        return new_window(unsafe { *a.stdscr }, None, false, None);
    }
    let w = unsafe { (a.initscr)() };
    if w.is_null() {
        return Err(cerror("initscr() returned NULL"));
    }
    INITSCR_CALLED.store(true, Ordering::Release);
    SETUPTERM_CALLED.store(true, Ordering::Release);
    let enc = default_encoding();
    *SCREEN_ENCODING.lock().unwrap_or_else(|e| e.into_inner()) = Some(enc.clone());
    // ACS_* constants are only meaningful after initscr() filled acs_map
    let d = module_dict()?;
    for (name, ch) in ACS_NAMES {
        let v = unsafe { *a.acs_map.add(*ch as usize) };
        dict::set_item_str(&d, name, &int::new_int(v as i64))?;
    }
    set_lines_cols(&d)?;
    new_window(w, None, false, Some(enc))
}

fn module_dict() -> PyResult<PyRef> {
    let p = MODULE_DICT.load(Ordering::Acquire);
    if p.is_null() {
        return Err(cerror("_curses module not initialised"));
    }
    Ok(unsafe { &*p }.to_ref())
}

fn set_lines_cols(d: &PyObject) -> PyResult<()> {
    let a = api();
    let (l, c) = unsafe { (*a.LINES, *a.COLS) };
    dict::set_item_str(d, "LINES", &int::new_int(l as i64))?;
    dict::set_item_str(d, "COLS", &int::new_int(c as i64))?;
    // the pure-Python curses package mirrors these names
    let sys_modules = crate::modules::sys::get_sys_attr("modules");
    if let Some(mods) = sys_modules {
        if let Ok(Some(cm)) = ab::getitem(&mods, &pystr::new_str("curses")).map(Some).or_else(|_| {
            clear_exc();
            Ok::<_, PyErr>(None)
        }) {
            ab::setattr(&cm, &pystr::new_str("LINES"), &int::new_int(l as i64))?;
            ab::setattr(&cm, &pystr::new_str("COLS"), &int::new_int(c as i64))?;
        }
    }
    Ok(())
}

fn m_update_lines_cols(_m: &PyObject) -> PyResult {
    let d = module_dict()?;
    set_lines_cols(&d)?;
    Ok(none::none_ref())
}

fn resize_impl(args: &[&PyObject], fname: &str, f: unsafe extern "C" fn(c_int, c_int) -> c_int) -> PyResult {
    crate::args::check_pos(fname, args, 2, 2)?;
    need_initscr()?;
    let l = short_arg(args[0])?;
    let c = short_arg(args[1])?;
    let r = unsafe { f(l, c) };
    if r == ERR {
        return Err(ret_err(fname));
    }
    let d = module_dict()?;
    set_lines_cols(&d)?;
    Ok(none::none_ref())
}

fn m_resizeterm(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    resize_impl(args, "resizeterm", api().resizeterm)
}
fn m_resize_term(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    resize_impl(args, "resize_term", api().resize_term)
}
fn m_is_term_resized(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (l, c) = two_ints("is_term_resized", args)?;
    need_initscr()?;
    bool_ref(unsafe { (api().is_term_resized)(l, c) } != 0)
}

fn m_newwin(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    need_initscr()?;
    let (l, c, y, x) = match args.len() {
        2 => (ci(args[0])?, ci(args[1])?, 0, 0),
        4 => (ci(args[0])?, ci(args[1])?, ci(args[2])?, ci(args[3])?),
        _ => raise!(TypeError, "newwin requires 2 or 4 arguments"),
    };
    let w = unsafe { (api().newwin)(l, c, y, x) };
    if w.is_null() {
        return Err(cerror("curses function returned NULL"));
    }
    new_window(w, None, true, None)
}

fn m_newpad(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (l, c) = two_ints("newpad", args)?;
    need_initscr()?;
    let w = unsafe { (api().newpad)(l, c) };
    if w.is_null() {
        return Err(cerror("curses function returned NULL"));
    }
    new_window(w, None, true, None)
}

fn m_getwin(_m: &PyObject, file: &PyObject) -> PyResult {
    need_initscr()?;
    let data = ab::call_method_str(file, "read", &[])?;
    if !bytes::is_bytes(&data) {
        raise!(TypeError, "f.read() returned {} instead of bytes", tname(&data));
    }
    let d = bytes::bytes_data(&data).to_vec();
    unsafe {
        let f = libc::tmpfile();
        if f.is_null() {
            return Err(crate::modules::posix::path::os_err());
        }
        libc::fwrite(d.as_ptr() as *const c_void, 1, d.len(), f);
        libc::fflush(f);
        libc::rewind(f);
        let w = (api().getwin)(f);
        libc::fclose(f);
        if w.is_null() {
            return Err(cerror("getwin() returned NULL"));
        }
        new_window(w, None, true, None)
    }
}

fn m_setupterm(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("setupterm", &["term", "fd"], 0, 0);
    let [term, fd] = crate::args::parse::<2>(&P, args, kw)?;
    let term: Option<CString> = match term {
        Some(t) if !t.is(none::none()) => {
            if !pystr::is_str(t) {
                raise!(TypeError, "setupterm() argument 'term' must be str or None, not {}", tname(t));
            }
            let s = pystr::as_utf8(t)?;
            Some(CString::new(s).map_err(|_| crate::err!(ValueError, "embedded null character"))?)
        }
        _ => None,
    };
    let mut fd = match fd {
        Some(f) => ci(f)?,
        None => -1,
    };
    if fd == -1 {
        let out = crate::modules::sys::get_sys_attr("stdout").ok_or_else(|| crate::err!(RuntimeError, "lost sys.stdout"))?;
        let r = ab::call_method_str(&out, "fileno", &[])?;
        fd = ci(&r)?;
    }
    if !SETUPTERM_CALLED.load(Ordering::Acquire) {
        let mut err: c_int = 0;
        let tp = term.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        if unsafe { (api().setupterm)(tp, fd, &mut err) } == ERR {
            let msg = if err == 0 { "setupterm: could not find terminal" } else { "setupterm: could not find terminfo database" };
            return Err(cerror(msg));
        }
        SETUPTERM_CALLED.store(true, Ordering::Release);
    }
    Ok(none::none_ref())
}

fn capname(o: &PyObject, fname: &str) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument must be str, not {}", fname, tname(o));
    }
    CString::new(pystr::as_utf8(o)?).map_err(|_| crate::err!(ValueError, "embedded null character"))
}

fn m_tigetflag(_m: &PyObject, c: &PyObject) -> PyResult {
    need_setupterm()?;
    let c = capname(c, "tigetflag")?;
    int_ref(unsafe { (api().tigetflag)(c.as_ptr()) } as i64)
}
fn m_tigetnum(_m: &PyObject, c: &PyObject) -> PyResult {
    need_setupterm()?;
    let c = capname(c, "tigetnum")?;
    int_ref(unsafe { (api().tigetnum)(c.as_ptr()) } as i64)
}
fn m_tigetstr(_m: &PyObject, c: &PyObject) -> PyResult {
    need_setupterm()?;
    let c = capname(c, "tigetstr")?;
    let p = unsafe { (api().tigetstr)(c.as_ptr()) };
    if p.is_null() || p as isize == -1 {
        return Ok(none::none_ref());
    }
    cstr_bytes(p)
}

fn m_tparm(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tparm", args, 1, 10)?;
    need_setupterm()?;
    if !bytes::is_bytes(args[0]) {
        raise!(TypeError, "tparm() argument 1 must be bytes, not {}", tname(args[0]));
    }
    let s = CString::new(bytes::bytes_data(args[0])).map_err(|_| crate::err!(ValueError, "embedded null byte"))?;
    let mut p = [0 as c_long; 9];
    for (i, a) in args[1..].iter().enumerate() {
        p[i] = crate::modules::posix::path::c_long(a)? as c_long;
    }
    let r = unsafe { (api().tparm)(s.as_ptr(), p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8]) };
    if r.is_null() {
        return Err(cerror("tparm() returned NULL"));
    }
    cstr_bytes(r)
}

fn m_putp(_m: &PyObject, s: &PyObject) -> PyResult {
    if !bytes::is_bytes(s) {
        raise!(TypeError, "putp() argument must be bytes, not {}", tname(s));
    }
    let c = CString::new(bytes::bytes_data(s)).map_err(|_| crate::err!(ValueError, "embedded null byte"))?;
    check(unsafe { (api().putp)(c.as_ptr()) }, "putp")
}

// ---- colors

fn color_arg(o: &PyObject) -> PyResult<c_int> {
    let v = big(o)?;
    if v < 0 {
        raise!(ValueError, "Color number is less than 0.");
    }
    let max = unsafe { *api().COLORS } as i128;
    if v >= max {
        raise!(ValueError, "Color number is greater than COLORS-1 ({}).", max - 1);
    }
    Ok(v as c_int)
}

/// fg/bg color for init_pair/assume_default_colors: -1 means the default color.
fn color_arg_default(o: &PyObject) -> PyResult<c_int> {
    let v = big(o)?;
    if v == -1 {
        return Ok(-1);
    }
    if v < -1 {
        raise!(ValueError, "Color number is less than -1.");
    }
    color_arg(o)
}

fn pair_arg(o: &PyObject) -> PyResult<c_int> {
    let v = big(o)?;
    if v < 0 {
        raise!(ValueError, "Color pair is less than 0.");
    }
    if v > i32::MAX as i128 {
        let max = unsafe { *api().COLOR_PAIRS } as i128;
        raise!(ValueError, "Color pair is greater than COLOR_PAIRS-1 ({}).", max - 1);
    }
    Ok(v as c_int)
}

fn pair_range_error(p: c_int) -> PyErr {
    let max = unsafe { *api().COLOR_PAIRS };
    if p >= max {
        crate::err!(ValueError, "Color pair is greater than COLOR_PAIRS-1 ({}).", max - 1)
    } else {
        ret_err("init_pair")
    }
}

fn m_start_color(_m: &PyObject) -> PyResult {
    need_initscr()?;
    let a = api();
    if unsafe { (a.start_color)() } == ERR {
        return Err(ret_err("start_color"));
    }
    COLOR_STARTED.store(true, Ordering::Release);
    let d = module_dict()?;
    dict::set_item_str(&d, "COLORS", &int::new_int(unsafe { *a.COLORS } as i64))?;
    dict::set_item_str(&d, "COLOR_PAIRS", &int::new_int(unsafe { *a.COLOR_PAIRS } as i64))?;
    Ok(none::none_ref())
}

fn m_color_content(_m: &PyObject, c: &PyObject) -> PyResult {
    need_color()?;
    let c = color_arg(c)?;
    let a = api();
    let (mut r, mut g, mut b) = (0, 0, 0);
    let rc = unsafe {
        match a.extended_color_content {
            Some(f) => f(c, &mut r, &mut g, &mut b),
            None => {
                let (mut r2, mut g2, mut b2) = (0i16, 0i16, 0i16);
                let x = (a.color_content)(c as c_short, &mut r2, &mut g2, &mut b2);
                r = r2 as c_int;
                g = g2 as c_int;
                b = b2 as c_int;
                x
            }
        }
    };
    if rc == ERR {
        return Err(cerror(&format!("Argument 1 was out of range. Check value of COLORS.")));
    }
    Ok(tuple::new_tuple(vec![int::new_int(r as i64), int::new_int(g as i64), int::new_int(b as i64)]))
}

fn component(o: &PyObject) -> PyResult<c_int> {
    let v = big(o)?;
    if !(0..=1000).contains(&v) {
        raise!(ValueError, "Color component is {} than {}.", if v < 0 { "less" } else { "greater" }, if v < 0 { 0 } else { 1000 });
    }
    Ok(v as c_int)
}

fn m_init_color(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("init_color", args, 4, 4)?;
    need_color()?;
    let c = color_arg(args[0])?;
    let (r, g, b) = (component(args[1])?, component(args[2])?, component(args[3])?);
    let a = api();
    let rc = unsafe {
        match a.init_extended_color {
            Some(f) => f(c, r, g, b),
            None => (a.init_color)(c as c_short, r as c_short, g as c_short, b as c_short),
        }
    };
    check(rc, "init_color")
}

fn m_init_pair(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("init_pair", args, 3, 3)?;
    need_color()?;
    let p = pair_arg(args[0])?;
    let f = color_arg_default(args[1])?;
    let b = color_arg_default(args[2])?;
    let a = api();
    let rc = unsafe {
        match a.init_extended_pair {
            Some(fun) => fun(p, f, b),
            None => (a.init_pair)(p as c_short, f as c_short, b as c_short),
        }
    };
    if rc == ERR {
        return Err(pair_range_error(p));
    }
    Ok(none::none_ref())
}

fn m_pair_content(_m: &PyObject, p: &PyObject) -> PyResult {
    need_color()?;
    let p = pair_arg(p)?;
    let a = api();
    let (mut f, mut b) = (0, 0);
    let rc = unsafe {
        match a.extended_pair_content {
            Some(fun) => fun(p, &mut f, &mut b),
            None => {
                let (mut f2, mut b2) = (0i16, 0i16);
                let x = (a.pair_content)(p as c_short, &mut f2, &mut b2);
                f = f2 as c_int;
                b = b2 as c_int;
                x
            }
        }
    };
    if rc == ERR {
        let max = unsafe { *a.COLOR_PAIRS };
        if p >= max {
            raise!(ValueError, "Color pair is greater than COLOR_PAIRS-1 ({}).", max - 1);
        }
        return Err(cerror("Argument 1 was out of range. (0..COLOR_PAIRS-1)"));
    }
    pair_of(f, b)
}

fn m_color_pair(_m: &PyObject, p: &PyObject) -> PyResult {
    need_color()?;
    let v = big(p)?;
    int_ref(((v as i64) << 8) & A_COLOR as i64)
}

fn m_pair_number(_m: &PyObject, a: &PyObject) -> PyResult {
    need_color()?;
    let v = big(a)?;
    int_ref(((v as i64) & A_COLOR as i64) >> 8)
}

fn m_assume_default_colors(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("assume_default_colors", args, 2, 2)?;
    need_color()?;
    let f = color_arg_default(args[0])?;
    let b = color_arg_default(args[1])?;
    check(unsafe { (api().assume_default_colors)(f, b) }, "assume_default_colors")
}

// ---- mouse

fn m_mousemask(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    let mask = big(v)? as u32;
    let mut old: u32 = 0;
    let avail = unsafe { (api().mousemask)(mask, &mut old) };
    pair_of_u(avail, old)
}

fn pair_of_u(a: u32, b: u32) -> PyResult {
    Ok(tuple::new_tuple(vec![int::new_int(a as i64), int::new_int(b as i64)]))
}

fn m_mouseinterval(_m: &PyObject, v: &PyObject) -> PyResult {
    need_initscr()?;
    int_ref(unsafe { (api().mouseinterval)(ci(v)?) } as i64)
}

fn m_getmouse(_m: &PyObject) -> PyResult {
    need_initscr()?;
    let mut e = MEvent::default();
    if unsafe { (api().getmouse)(&mut e) } == ERR {
        return Err(ret_err("getmouse"));
    }
    Ok(tuple::new_tuple(vec![
        int::new_int(e.id as i64),
        int::new_int(e.x as i64),
        int::new_int(e.y as i64),
        int::new_int(e.z as i64),
        int::new_int(e.bstate as i64),
    ]))
}

fn m_ungetmouse(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ungetmouse", args, 5, 5)?;
    need_initscr()?;
    let mut e = MEvent { id: short_arg(args[0])? as c_short, x: ci(args[1])?, y: ci(args[2])?, z: ci(args[3])?, bstate: big(args[4])? as u32 };
    check(unsafe { (api().ungetmouse)(&mut e) }, "ungetmouse")
}

// ------------------------------------------------------------------ constants

const ACS_NAMES: &[(&str, u8)] = &[
    ("ACS_ULCORNER", b'l'),
    ("ACS_LLCORNER", b'm'),
    ("ACS_URCORNER", b'k'),
    ("ACS_LRCORNER", b'j'),
    ("ACS_LTEE", b't'),
    ("ACS_RTEE", b'u'),
    ("ACS_BTEE", b'v'),
    ("ACS_TTEE", b'w'),
    ("ACS_HLINE", b'q'),
    ("ACS_VLINE", b'x'),
    ("ACS_PLUS", b'n'),
    ("ACS_S1", b'o'),
    ("ACS_S9", b's'),
    ("ACS_DIAMOND", b'`'),
    ("ACS_CKBOARD", b'a'),
    ("ACS_DEGREE", b'f'),
    ("ACS_PLMINUS", b'g'),
    ("ACS_BULLET", b'~'),
    ("ACS_LARROW", b','),
    ("ACS_RARROW", b'+'),
    ("ACS_DARROW", b'.'),
    ("ACS_UARROW", b'-'),
    ("ACS_BOARD", b'h'),
    ("ACS_LANTERN", b'i'),
    ("ACS_BLOCK", b'0'),
    ("ACS_S3", b'p'),
    ("ACS_S7", b'r'),
    ("ACS_LEQUAL", b'y'),
    ("ACS_GEQUAL", b'z'),
    ("ACS_PI", b'{'),
    ("ACS_NEQUAL", b'|'),
    ("ACS_STERLING", b'}'),
    ("ACS_BSSB", b'l'),
    ("ACS_SSBB", b'm'),
    ("ACS_BBSS", b'k'),
    ("ACS_SBBS", b'j'),
    ("ACS_SBSS", b'u'),
    ("ACS_SSSB", b't'),
    ("ACS_SSBS", b'v'),
    ("ACS_BSSS", b'w'),
    ("ACS_BSBS", b'q'),
    ("ACS_SBSB", b'x'),
    ("ACS_SSSS", b'n'),
];

const KEYS: &[(&str, i64)] = &[
    ("KEY_MIN", 0o401), ("KEY_BREAK", 0o401), ("KEY_SRESET", 0o530), ("KEY_RESET", 0o531),
    ("KEY_DOWN", 0o402), ("KEY_UP", 0o403), ("KEY_LEFT", 0o404), ("KEY_RIGHT", 0o405),
    ("KEY_HOME", 0o406), ("KEY_BACKSPACE", 0o407), ("KEY_F0", 0o410), ("KEY_DL", 0o510),
    ("KEY_IL", 0o511), ("KEY_DC", 0o512), ("KEY_IC", 0o513), ("KEY_EIC", 0o514),
    ("KEY_CLEAR", 0o515), ("KEY_EOS", 0o516), ("KEY_EOL", 0o517), ("KEY_SF", 0o520),
    ("KEY_SR", 0o521), ("KEY_NPAGE", 0o522), ("KEY_PPAGE", 0o523), ("KEY_STAB", 0o524),
    ("KEY_CTAB", 0o525), ("KEY_CATAB", 0o526), ("KEY_ENTER", 0o527), ("KEY_PRINT", 0o532),
    ("KEY_LL", 0o533), ("KEY_A1", 0o534), ("KEY_A3", 0o535), ("KEY_B2", 0o536),
    ("KEY_C1", 0o537), ("KEY_C3", 0o540), ("KEY_BTAB", 0o541), ("KEY_BEG", 0o542),
    ("KEY_CANCEL", 0o543), ("KEY_CLOSE", 0o544), ("KEY_COMMAND", 0o545), ("KEY_COPY", 0o546),
    ("KEY_CREATE", 0o547), ("KEY_END", 0o550), ("KEY_EXIT", 0o551), ("KEY_FIND", 0o552),
    ("KEY_HELP", 0o553), ("KEY_MARK", 0o554), ("KEY_MESSAGE", 0o555), ("KEY_MOVE", 0o556),
    ("KEY_NEXT", 0o557), ("KEY_OPEN", 0o560), ("KEY_OPTIONS", 0o561), ("KEY_PREVIOUS", 0o562),
    ("KEY_REDO", 0o563), ("KEY_REFERENCE", 0o564), ("KEY_REFRESH", 0o565), ("KEY_REPLACE", 0o566),
    ("KEY_RESTART", 0o567), ("KEY_RESUME", 0o570), ("KEY_SAVE", 0o571), ("KEY_SBEG", 0o572),
    ("KEY_SCANCEL", 0o573), ("KEY_SCOMMAND", 0o574), ("KEY_SCOPY", 0o575), ("KEY_SCREATE", 0o576),
    ("KEY_SDC", 0o577), ("KEY_SDL", 0o600), ("KEY_SELECT", 0o601), ("KEY_SEND", 0o602),
    ("KEY_SEOL", 0o603), ("KEY_SEXIT", 0o604), ("KEY_SFIND", 0o605), ("KEY_SHELP", 0o606),
    ("KEY_SHOME", 0o607), ("KEY_SIC", 0o610), ("KEY_SLEFT", 0o611), ("KEY_SMESSAGE", 0o612),
    ("KEY_SMOVE", 0o613), ("KEY_SNEXT", 0o614), ("KEY_SOPTIONS", 0o615), ("KEY_SPREVIOUS", 0o616),
    ("KEY_SPRINT", 0o617), ("KEY_SREDO", 0o620), ("KEY_SREPLACE", 0o621), ("KEY_SRIGHT", 0o622),
    ("KEY_SRSUME", 0o623), ("KEY_SSAVE", 0o624), ("KEY_SSUSPEND", 0o625), ("KEY_SUNDO", 0o626),
    ("KEY_SUSPEND", 0o627), ("KEY_UNDO", 0o630), ("KEY_MOUSE", 0o631), ("KEY_RESIZE", 0o632),
    ("KEY_MAX", 0o777),
];

fn mouse_mask(b: u32, m: u32) -> i64 {
    ((m as u64) << ((b - 1) * 5)) as i64
}

static NCURSES_VERSION_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn ncurses_version_obj() -> PyResult {
    use crate::types::structseq::{self, Desc, Field};
    static FIELDS: &[Field] = &[Field::new("major", "Major release number"), Field::new("minor", "Minor release number"), Field::new("patch", "Patch release number")];
    let mut t = NCURSES_VERSION_TYPE.load(Ordering::Acquire);
    if t.is_null() {
        t = structseq::new_type(&Desc { name: "curses.ncurses_version", doc: "ncurses version\n\nThe version of the ncurses library as a named tuple (major, minor, patch).", fields: FIELDS, n_in_sequence: 3 })?;
        structseq::disallow_instantiation(t);
        NCURSES_VERSION_TYPE.store(t, Ordering::Release);
    }
    // "ncurses 6.4.20240113"
    let v = unsafe { CStr::from_ptr((api().curses_version)()) }.to_string_lossy().into_owned();
    let nums: Vec<i64> = v.rsplit(' ').next().unwrap_or("").split('.').map(|p| p.parse::<i64>().unwrap_or(0)).collect();
    let get = |i: usize| int::new_int(*nums.get(i).unwrap_or(&0));
    Ok(structseq::new_instance(t, vec![get(0), get(1), get(2)]))
}

pub fn init() -> PyResult {
    let a = nc_api()?;
    init_window_type()?;
    let methods = mdefs![
        ("assume_default_colors", FAST, m_assume_default_colors, "assume_default_colors($module, fg, bg, /)\n--\n\nAllow use of default values for colors on terminals supporting this feature."),
        ("baudrate", NOARGS, m_baudrate, "baudrate($module, /)\n--\n\nReturn the output speed of the terminal in bits per second."),
        ("beep", NOARGS, m_beep, "beep($module, /)\n--\n\nEmit a short attention sound."),
        ("can_change_color", NOARGS, m_can_change_color, "can_change_color($module, /)\n--\n\nReturn True if the programmer can change the colors displayed by the terminal."),
        ("cbreak", FAST, m_cbreak, "cbreak($module, flag=True, /)\n--\n\nEnter cbreak mode."),
        ("color_content", O, m_color_content, "color_content($module, color_number, /)\n--\n\nReturn the red, green, and blue (RGB) components of the specified color."),
        ("color_pair", O, m_color_pair, "color_pair($module, pair_number, /)\n--\n\nReturn the attribute value for displaying text in the specified color."),
        ("curs_set", O, m_curs_set, "curs_set($module, visibility, /)\n--\n\nSet the cursor state."),
        ("def_prog_mode", NOARGS, m_def_prog_mode),
        ("def_shell_mode", NOARGS, m_def_shell_mode),
        ("delay_output", O, m_delay_output),
        ("doupdate", NOARGS, m_doupdate),
        ("echo", FAST, m_echo, "echo($module, flag=True, /)\n--\n\nEnter echo mode."),
        ("endwin", NOARGS, m_endwin),
        ("erasechar", NOARGS, m_erasechar),
        ("filter", NOARGS, m_filter),
        ("flash", NOARGS, m_flash),
        ("flushinp", NOARGS, m_flushinp),
        ("get_escdelay", NOARGS, m_get_escdelay),
        ("set_escdelay", O, m_set_escdelay),
        ("get_tabsize", NOARGS, m_get_tabsize),
        ("set_tabsize", O, m_set_tabsize),
        ("getmouse", NOARGS, m_getmouse),
        ("getsyx", NOARGS, m_getsyx),
        ("getwin", O, m_getwin),
        ("halfdelay", O, m_halfdelay),
        ("has_colors", NOARGS, m_has_colors),
        ("has_extended_color_support", NOARGS, m_has_extended_color_support),
        ("has_ic", NOARGS, m_has_ic),
        ("has_il", NOARGS, m_has_il),
        ("has_key", O, m_has_key),
        ("init_color", FAST, m_init_color),
        ("init_pair", FAST, m_init_pair),
        ("initscr", NOARGS, m_initscr),
        ("intrflush", O, m_intrflush),
        ("is_term_resized", FAST, m_is_term_resized),
        ("isendwin", NOARGS, m_isendwin),
        ("keyname", O, m_keyname),
        ("killchar", NOARGS, m_killchar),
        ("longname", NOARGS, m_longname),
        ("meta", O, m_meta),
        ("mouseinterval", O, m_mouseinterval),
        ("mousemask", O, m_mousemask),
        ("napms", O, m_napms),
        ("newpad", FAST, m_newpad),
        ("newwin", FAST, m_newwin, "newwin(nlines, ncols, [begin_y=0, begin_x=0])\nReturn a new window."),
        ("nl", FAST, m_nl),
        ("nocbreak", NOARGS, m_nocbreak),
        ("noecho", NOARGS, m_noecho),
        ("nonl", NOARGS, m_nonl),
        ("noqiflush", NOARGS, m_noqiflush),
        ("noraw", NOARGS, m_noraw),
        ("pair_content", O, m_pair_content),
        ("pair_number", O, m_pair_number),
        ("putp", O, m_putp),
        ("qiflush", FAST, m_qiflush),
        ("raw", FAST, m_raw),
        ("reset_prog_mode", NOARGS, m_reset_prog_mode),
        ("reset_shell_mode", NOARGS, m_reset_shell_mode),
        ("resetty", NOARGS, m_resetty),
        ("resize_term", FAST, m_resize_term),
        ("resizeterm", FAST, m_resizeterm),
        ("savetty", NOARGS, m_savetty),
        ("setsyx", FAST, m_setsyx),
        ("setupterm", FASTKW, m_setupterm, "setupterm($module, /, term=None, fd=-1)\n--\n\nInitialize the terminal."),
        ("start_color", NOARGS, m_start_color),
        ("termattrs", NOARGS, m_termattrs),
        ("termname", NOARGS, m_termname),
        ("tigetflag", O, m_tigetflag),
        ("tigetnum", O, m_tigetnum),
        ("tigetstr", O, m_tigetstr),
        ("tparm", FAST, m_tparm),
        ("typeahead", O, m_typeahead),
        ("unctrl", O, m_unctrl),
        ("unget_wch", O, m_unget_wch),
        ("ungetch", O, m_ungetch),
        ("ungetmouse", FAST, m_ungetmouse),
        ("update_lines_cols", NOARGS, m_update_lines_cols),
        ("use_default_colors", NOARGS, m_use_default_colors),
        ("use_env", O, m_use_env),
    ];
    let (m, d) = fc::new_module("_curses", methods, None)?;
    MODULE_DICT.store(d.clone().into_raw(), Ordering::Release);
    if ERROR.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("_curses", "error", exceptions::Exception())?;
        ERROR.store(e.into_raw(), Ordering::Release);
    }
    let set = |n: &str, v: i64| dict::set_item_str(&d, n, &int::new_int(v));
    dict::set_item_str(&d, "error", unsafe { &*ERROR.load(Ordering::Acquire) })?;
    dict::set_item_str(&d, "window", typeobject::type_obj(window_type()))?;
    // the py_curses.h C API: the window type and the three "initialised" checks (capi2)
    {
        let cap = unsafe { crate::capi::capsule::PyCapsule_New(capi_table() as *mut std::ffi::c_void, c"_curses._C_API".as_ptr(), None) };
        let cap = unsafe { crate::errors::from_c(cap) }?;
        dict::set_item_str(&d, "_C_API", &cap)?;
    }
    dict::set_item_str(&d, "version", &bytes::new_bytes(b"2.2"))?;
    dict::set_item_str(&d, "__version__", &bytes::new_bytes(b"2.2"))?;
    let ver = ncurses_version_obj()?;
    dict::set_item_str(&d, "ncurses_version", &ver)?;
    set("ERR", -1)?;
    set("OK", 0)?;
    let attrs: &[(&str, u32)] = &[
        ("A_ATTRIBUTES", A_ATTRIBUTES),
        ("A_NORMAL", 0),
        ("A_STANDOUT", bit(8)),
        ("A_UNDERLINE", bit(9)),
        ("A_REVERSE", bit(10)),
        ("A_BLINK", bit(11)),
        ("A_DIM", bit(12)),
        ("A_BOLD", bit(13)),
        ("A_ALTCHARSET", bit(14)),
        ("A_INVIS", bit(15)),
        ("A_PROTECT", bit(16)),
        ("A_CHARTEXT", A_CHARTEXT),
        ("A_COLOR", A_COLOR),
        ("A_HORIZONTAL", bit(17)),
        ("A_LEFT", bit(18)),
        ("A_LOW", bit(19)),
        ("A_RIGHT", bit(20)),
        ("A_TOP", bit(21)),
        ("A_VERTICAL", bit(22)),
        ("A_ITALIC", bit(23)),
    ];
    for (n, v) in attrs {
        set(n, *v as i64)?;
    }
    for (i, n) in ["COLOR_BLACK", "COLOR_RED", "COLOR_GREEN", "COLOR_YELLOW", "COLOR_BLUE", "COLOR_MAGENTA", "COLOR_CYAN", "COLOR_WHITE"].iter().enumerate() {
        set(n, i as i64)?;
    }
    for b in 1..=5u32 {
        set(&format!("BUTTON{}_RELEASED", b), mouse_mask(b, 0o1))?;
        set(&format!("BUTTON{}_PRESSED", b), mouse_mask(b, 0o2))?;
        set(&format!("BUTTON{}_CLICKED", b), mouse_mask(b, 0o4))?;
        set(&format!("BUTTON{}_DOUBLE_CLICKED", b), mouse_mask(b, 0o10))?;
        set(&format!("BUTTON{}_TRIPLE_CLICKED", b), mouse_mask(b, 0o20))?;
    }
    set("BUTTON_CTRL", mouse_mask(6, 0o1))?;
    set("BUTTON_SHIFT", mouse_mask(6, 0o2))?;
    set("BUTTON_ALT", mouse_mask(6, 0o4))?;
    set("REPORT_MOUSE_POSITION", mouse_mask(6, 0o10))?;
    set("ALL_MOUSE_EVENTS", mouse_mask(6, 0o10) - 1)?;
    for (n, v) in KEYS {
        set(n, *v)?;
    }
    for i in 1..64 {
        set(&format!("KEY_F{}", i), 0o410 + i)?;
    }
    let _ = a;
    Ok(m)
}
