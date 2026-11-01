//! Runtime loading of system shared libraries (dlopen/dlsym).

use std::ffi::{CStr, CString, c_void};

pub struct Lib {
    pub handle: *mut c_void,
}
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

impl Lib {
    /// Open the first library of `names` that loads.
    pub fn open(names: &[&str]) -> Result<Lib, String> {
        let mut last = String::new();
        for n in names {
            let c = CString::new(*n).unwrap();
            let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if !h.is_null() {
                return Ok(Lib { handle: h });
            }
            last = dlerror_string();
        }
        Err(last)
    }

    pub fn sym(&self, name: &str) -> *mut c_void {
        let c = CString::new(name).unwrap();
        unsafe { libc::dlsym(self.handle, c.as_ptr()) }
    }

    /// Look up a symbol and transmute it to a function pointer type.
    pub unsafe fn func<T: Copy>(&self, name: &str) -> Option<T> {
        let p = self.sym(name);
        if p.is_null() {
            None
        } else {
            debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
            Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
        }
    }
}

pub fn dlerror_string() -> String {
    unsafe {
        let e = libc::dlerror();
        if e.is_null() { String::new() } else { CStr::from_ptr(e).to_string_lossy().into_owned() }
    }
}

/// Declare a table of dynamically loaded functions.
/// `dyn_lib!(Name, ["libfoo.so.1"], { fn foo(a: i32) -> i32; ... })` creates a struct
/// with one `Option<fn>` field per function and a `Name::get()` accessor that loads the
/// library once (returns Err(message) if it cannot be loaded).
#[macro_export]
macro_rules! dyn_lib {
    ($name:ident, [$($lib:expr),* $(,)?], { $( fn $f:ident ( $($an:ident : $at:ty),* $(,)? ) $(-> $rt:ty)? ; )* }) => {
        #[allow(non_snake_case, dead_code)]
        pub struct $name {
            pub lib: $crate::dl::Lib,
            $( pub $f: Option<unsafe extern "C" fn($($at),*) $(-> $rt)?>, )*
        }
        impl $name {
            pub fn get() -> Result<&'static $name, String> {
                static CELL: std::sync::OnceLock<Result<$name, String>> = std::sync::OnceLock::new();
                CELL.get_or_init(|| {
                    let lib = $crate::dl::Lib::open(&[$($lib),*])?;
                    unsafe {
                        Ok($name {
                            $( $f: lib.func(stringify!($f)), )*
                            lib,
                        })
                    }
                }).as_ref().map_err(|e| e.clone())
            }
        }
    };
}
