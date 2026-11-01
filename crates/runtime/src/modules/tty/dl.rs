//! Runtime loading of the system C libraries the "tty" modules bind (DECISIONS D20).
//!
//! We dlopen the shared objects lazily instead of linking them, so building the
//! interpreter never needs the -dev packages and a missing library just makes the
//! corresponding module raise ImportError (exactly what happens with a module that
//! was not built).
//!
//! `dl_api!` declares a struct of function pointers (and data-symbol addresses) that
//! is resolved once, on first use, and cached for the life of the process.

use crate::errors::*;
use std::ffi::{CStr, CString, c_void};

pub struct Lib(pub *mut c_void);
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

impl Lib {
    /// Open the first library in `names` that loads (RTLD_GLOBAL so dependent
    /// libraries such as libpanelw -> libncursesw resolve against each other).
    pub fn open(names: &[&str]) -> Result<Lib, String> {
        let mut last = String::new();
        for n in names {
            let c = CString::new(*n).unwrap();
            let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if !h.is_null() {
                return Ok(Lib(h));
            }
            let e = unsafe { libc::dlerror() };
            last = if e.is_null() { format!("cannot load {}", n) } else { unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned() };
        }
        Err(last)
    }

    pub fn sym(&self, name: &str) -> *mut c_void {
        let c = CString::new(name).unwrap();
        unsafe { libc::dlsym(self.0, c.as_ptr()) }
    }
}

pub fn import_error(msg: &str) -> PyErr {
    new_err(crate::types::exceptions::ImportError(), msg)
}

/// Declare a lazily loaded C API.
///
/// ```ignore
/// dl_api! {
///     pub struct Gdbm, fn gdbm_api, ["libgdbm.so.6", "libgdbm.so"] {
///         fn gdbm_close(f: *mut c_void);
///         ?fn gdbm_count(f: *mut c_void, n: *mut u64) -> c_int;   // optional symbol
///         static gdbm_version_number: [c_int; 3];                    // data symbol (pointer)
///     }
/// }
/// ```
/// Required functions abort loading when missing; `?fn` entries become `Option<fn>`.
#[macro_export]
macro_rules! dl_api {
    (
        $vis:vis struct $S:ident, fn $getter:ident, [$($lib:expr),+ $(,)?] {
            $( fn $f:ident ( $($a:ident : $t:ty),* $(,)? ) $(-> $r:ty)? ; )*
            $( ?fn $of:ident ( $($oa:ident : $ot:ty),* $(,)? ) $(-> $or:ty)? ; )*
            $( static $v:ident : $vt:ty ; )*
        }
    ) => {
        #[allow(non_snake_case, dead_code)]
        $vis struct $S {
            pub lib: $crate::modules::tty::dl::Lib,
            $( pub $f: unsafe extern "C" fn($($t),*) $(-> $r)?, )*
            $( pub $of: Option<unsafe extern "C" fn($($ot),*) $(-> $or)?>, )*
            $( pub $v: *mut $vt, )*
        }
        unsafe impl Send for $S {}
        unsafe impl Sync for $S {}

        #[allow(dead_code, non_snake_case)]
        $vis fn $getter() -> $crate::errors::PyResult<&'static $S> {
            static CELL: std::sync::OnceLock<Result<$S, String>> = std::sync::OnceLock::new();
            let r = CELL.get_or_init(|| unsafe {
                const NAMES: &[&str] = &[$($lib),+];
                let lib = $crate::modules::tty::dl::Lib::open(NAMES)?;
                $(
                    let p = lib.sym(stringify!($f));
                    if p.is_null() {
                        return Err(format!("{}: undefined symbol: {}", NAMES[0], stringify!($f)));
                    }
                    let $f: unsafe extern "C" fn($($t),*) $(-> $r)? = std::mem::transmute(p);
                )*
                $(
                    let p = lib.sym(stringify!($of));
                    let $of: Option<unsafe extern "C" fn($($ot),*) $(-> $or)?> =
                        if p.is_null() { None } else { Some(std::mem::transmute(p)) };
                )*
                $(
                    let $v = lib.sym(stringify!($v)) as *mut $vt;
                )*
                Ok($S { lib, $($f,)* $($of,)* $($v,)* })
            });
            match r {
                Ok(s) => Ok(s),
                Err(e) => Err($crate::modules::tty::dl::import_error(e)),
            }
        }
    };
}
