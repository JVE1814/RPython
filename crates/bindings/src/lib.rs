//! Binding modules over system C libraries (DECISIONS D20): `_ctypes` (libffi),
//! `_sqlite3` (libsqlite3) and `_ssl` (OpenSSL 3). The libraries are dlopen'ed at
//! runtime; the modules themselves are written from scratch.
#![allow(clippy::missing_safety_doc, static_mut_refs, unused_unsafe, dead_code)]

/// NUL-terminated C string literal.
#[macro_export]
macro_rules! cs {
    ($s:literal) => {
        concat!($s, "\0").as_ptr() as *const std::ffi::c_char
    };
}

pub mod capi;
pub mod ctypes;
pub mod dl;
pub mod sqlite;

/// Register the binding modules as builtin modules (call before runtime initialisation).
pub fn register() {
    rpy_runtime::import::register_builtin("_ctypes", ctypes::module::init);
    rpy_runtime::import::register_builtin("_sqlite3", sqlite::init);
}
