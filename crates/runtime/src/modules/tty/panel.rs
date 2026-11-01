//! `_curses_panel`: binding to the ncurses panel library (libpanelw.so.6).
//!
//! Every Python panel object wraps one `PANEL*`.  A registry maps the C
//! pointers back to their (borrowed) Python objects so that `above()`,
//! `below()`, `top_panel()` and `bottom_panel()` can return the same objects.

use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::modules::tty::curses;
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{bool_, dict, exceptions, none, typeobject};
use crate::{dl_api, mdefs, raise};
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

type P = *mut c_void;
type W = *mut c_void;

dl_api! {
    pub struct PanelApi, fn panel_api, ["libpanelw.so.6", "libpanelw.so"] {
        fn new_panel(w: W) -> P;
        fn del_panel(p: P) -> c_int;
        fn panel_above(p: P) -> P;
        fn panel_below(p: P) -> P;
        fn top_panel(p: P) -> c_int;
        fn bottom_panel(p: P) -> c_int;
        fn hide_panel(p: P) -> c_int;
        fn show_panel(p: P) -> c_int;
        fn panel_hidden(p: P) -> c_int;
        fn move_panel(p: P, y: c_int, x: c_int) -> c_int;
        fn replace_panel(p: P, w: W) -> c_int;
        fn update_panels();
    }
}

fn api() -> &'static PanelApi {
    panel_api().ok().expect("panel library not loaded")
}

struct Panel {
    pan: P,
    win: Option<PyRef>,
    userptr: Option<PyRef>,
}

impl Trav for Panel {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        crate::trav!(v; self.win, self.userptr)
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        // Take the panel off the stack before anything else can run (the
        // userptr's finalizer may look at the stack).
        if !self.pan.is_null() {
            unregister(self.pan);
            if let Ok(a) = panel_api() {
                unsafe { (a.del_panel)(self.pan) };
            }
            self.pan = std::ptr::null_mut();
        }
        let u = self.userptr.take();
        drop(u);
    }
}

/// (PANEL*, borrowed Python object) pairs, as usize for Send.
static REGISTRY: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn unregister(p: P) {
    let mut r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|(pp, _)| *pp != p as usize);
}

fn lookup(p: P) -> PyResult {
    if p.is_null() {
        return Ok(none::none_ref());
    }
    let r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    for (pp, obj) in r.iter() {
        if *pp == p as usize {
            return Ok(unsafe { &*(*obj as *const PyObject) }.to_ref());
        }
    }
    drop(r);
    Err(perror("panel_above: can't find Panel Object"))
}

fn perror(msg: &str) -> PyErr {
    let p = ERROR.load(Ordering::Acquire);
    new_err(if p.is_null() { exceptions::RuntimeError() } else { p as *mut PyTypeObject }, msg)
}

fn check(r: c_int, fname: &str) -> PyResult {
    if r == curses::ERR { Err(perror(&format!("{}() returned ERR", fname))) } else { Ok(none::none_ref()) }
}

#[unsafe(no_mangle)]
pub static mut RPY_CURSES_PANEL_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn panel_type() -> *mut PyTypeObject {
    &raw mut RPY_CURSES_PANEL_TYPE
}

fn st(o: &PyObject) -> &mut Panel {
    fc::rs::<Panel>(o)
}

fn p_above(o: &PyObject) -> PyResult {
    lookup(unsafe { (api().panel_above)(st(o).pan) })
}
fn p_below(o: &PyObject) -> PyResult {
    lookup(unsafe { (api().panel_below)(st(o).pan) })
}
fn p_bottom(o: &PyObject) -> PyResult {
    check(unsafe { (api().bottom_panel)(st(o).pan) }, "bottom")
}
fn p_top(o: &PyObject) -> PyResult {
    check(unsafe { (api().top_panel)(st(o).pan) }, "top")
}
fn p_hide(o: &PyObject) -> PyResult {
    check(unsafe { (api().hide_panel)(st(o).pan) }, "hide")
}
fn p_show(o: &PyObject) -> PyResult {
    check(unsafe { (api().show_panel)(st(o).pan) }, "show")
}
fn p_hidden(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(unsafe { (api().panel_hidden)(st(o).pan) } != 0))
}
fn p_move(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("move", args, 2, 2)?;
    let y = crate::modules::posix::path::c_int(args[0])?;
    let x = crate::modules::posix::path::c_int(args[1])?;
    check(unsafe { (api().move_panel)(st(o).pan, y, x) }, "move_panel")
}
fn p_replace(o: &PyObject, w: &PyObject) -> PyResult {
    if !curses::is_window(w) {
        raise!(TypeError, "replace() argument must be _curses.window, not {}", tname(w));
    }
    let win = curses::win_of(w).win;
    let p = st(o);
    check(unsafe { (api().replace_panel)(p.pan, win) }, "replace_panel")?;
    p.win = Some(w.to_ref());
    Ok(none::none_ref())
}
fn p_set_userptr(o: &PyObject, v: &PyObject) -> PyResult {
    let old = st(o).userptr.replace(v.to_ref());
    // released last: its finalizer may touch this panel again
    drop(old);
    Ok(none::none_ref())
}
fn p_userptr(o: &PyObject) -> PyResult {
    match &st(o).userptr {
        Some(u) => Ok(u.clone()),
        None => Err(perror("no userptr set")),
    }
}
fn p_window(o: &PyObject) -> PyResult {
    Ok(st(o).win.clone().unwrap_or_else(none::none_ref))
}

fn m_new_panel(_m: &PyObject, w: &PyObject) -> PyResult {
    if !curses::is_window(w) {
        raise!(TypeError, "new_panel() argument must be _curses.window, not {}", tname(w));
    }
    let pan = unsafe { (api().new_panel)(curses::win_of(w).win) };
    if pan.is_null() {
        return Err(perror("curses function returned NULL"));
    }
    let obj = fc::robj_new(panel_type(), Panel { pan, win: Some(w.to_ref()), userptr: None })?;
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).push((pan as usize, obj.as_ptr() as usize));
    Ok(obj)
}

fn m_top_panel(_m: &PyObject) -> PyResult {
    lookup(unsafe { (api().panel_below)(std::ptr::null_mut()) })
}
fn m_bottom_panel(_m: &PyObject) -> PyResult {
    lookup(unsafe { (api().panel_above)(std::ptr::null_mut()) })
}
fn m_update_panels(_m: &PyObject) -> PyResult {
    unsafe { (api().update_panels)() };
    Ok(none::none_ref())
}

pub fn init() -> PyResult {
    panel_api()?;
    // the window type lives in _curses
    crate::import::import_absolute("_curses")?;
    unsafe {
        let t = panel_type();
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_robj_type::<Panel>(t, c"_curses_panel.panel", Py_TPFLAGS_DISALLOW_INSTANTIATION);
            (*t).tp_methods = mdefs![
                ("above", NOARGS, p_above, "above($self, /)\n--\n\nReturn the panel above the current panel."),
                ("below", NOARGS, p_below, "below($self, /)\n--\n\nReturn the panel below the current panel."),
                ("bottom", NOARGS, p_bottom, "bottom($self, /)\n--\n\nPush the panel to the bottom of the stack."),
                ("hidden", NOARGS, p_hidden, "hidden($self, /)\n--\n\nReturn True if the panel is hidden (not visible), False otherwise."),
                ("hide", NOARGS, p_hide, "hide($self, /)\n--\n\nHide the panel."),
                ("move", FAST, p_move, "move($self, y, x, /)\n--\n\nMove the panel to the screen coordinates (y, x)."),
                ("replace", O, p_replace, "replace($self, win, /)\n--\n\nChange the window associated with the panel to the window win."),
                ("set_userptr", O, p_set_userptr, "set_userptr($self, obj, /)\n--\n\nSet the panel's user pointer to obj."),
                ("show", NOARGS, p_show, "show($self, /)\n--\n\nDisplay the panel (which might have been hidden)."),
                ("top", NOARGS, p_top, "top($self, /)\n--\n\nPush panel to the top of the stack."),
                ("userptr", NOARGS, p_userptr, "userptr($self, /)\n--\n\nReturn the user pointer for the panel."),
                ("window", NOARGS, p_window, "window($self, /)\n--\n\nReturn the window object associated with the panel."),
            ];
            typeobject::type_ready(t)?;
        }
    }
    let (m, d) = fc::new_module(
        "_curses_panel",
        mdefs![
            ("bottom_panel", NOARGS, m_bottom_panel, "bottom_panel($module, /)\n--\n\nReturn the bottom panel in the panel stack."),
            ("new_panel", O, m_new_panel, "new_panel($module, win, /)\n--\n\nReturn a panel object, associating it with the given window win."),
            ("top_panel", NOARGS, m_top_panel, "top_panel($module, /)\n--\n\nReturn the top panel in the panel stack."),
            ("update_panels", NOARGS, m_update_panels, "update_panels($module, /)\n--\n\nUpdates the virtual screen after changes in the panel stack."),
        ],
        None,
    )?;
    if ERROR.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("_curses_panel", "error", exceptions::Exception())?;
        ERROR.store(e.into_raw(), Ordering::Release);
    }
    dict::set_item_str(&d, "error", unsafe { &*ERROR.load(Ordering::Acquire) })?;
    dict::set_item_str(&d, "panel", typeobject::type_obj(panel_type()))?;
    dict::set_item_str(&d, "version", &crate::types::bytes::new_bytes(b"2.1"))?;
    dict::set_item_str(&d, "__version__", &crate::types::bytes::new_bytes(b"2.1"))?;
    Ok(m)
}
