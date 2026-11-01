//! Bindings to terminal / database / GUI system libraries (workstream "tty"):
//! `_gdbm`, `_dbm`, `readline`, `_curses`, `_curses_panel`, `_tkinter`.
//! All libraries are dlopen'ed on first import (see dl.rs, DECISIONS D20).

pub mod dl;
pub mod dbm;
pub mod input;
pub mod readline;
pub mod curses;
pub mod panel;

pub fn register() {
    crate::import::register_builtin("_gdbm", dbm::init_gdbm);
    crate::import::register_builtin("_dbm", dbm::init_ndbm);
    crate::import::register_builtin("readline", readline::init);
    crate::import::register_builtin("_curses", curses::init);
    crate::import::register_builtin("_curses_panel", panel::init);
}
