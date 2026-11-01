//! Exported data symbols that are not owned by a runtime type module: placeholder
//! type objects (for types the runtime does not implement as C-level statics yet),
//! ctype tables, legacy configuration flags.
//!
//! NOTE for other workstreams: when you implement one of the placeholder types below
//! as a real static, export it under the CPython name (`#[unsafe(export_name = ...)]`)
//! and delete the placeholder here (the linker reports a duplicate symbol otherwise).

use super::*;
use std::ffi::c_uint;

macro_rules! placeholder_types {
    ($( ($sym:ident, $name:literal) ),* $(,)?) => {
        $(
            #[unsafe(no_mangle)]
            pub static mut $sym: PyTypeObject = PyTypeObject::ZERO;
        )*
        /// Give placeholders a name and make them ready (not instantiable).
        pub fn init_placeholders() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| unsafe {
                $(
                    {
                        let t = &mut *(&raw mut $sym);
                        if t.tp_flags & Py_TPFLAGS_READY == 0 {
                            t.tp_name = concat!($name, "\0").as_ptr() as *const c_char;
                            t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
                            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION;
                            let _ = super::typeobj::PyType_Ready(t);
                            clear_exc();
                        }
                    }
                )*
            });
        }
    };
}
placeholder_types!(
    (PyInstanceMethod_Type, "instancemethod"),
    (PyODictItems_Type, "odict_items"),
    (PyODictIter_Type, "odict_iterator"),
    (PyODictKeys_Type, "odict_keys"),
    (PyODictValues_Type, "odict_values"),
    (PyODict_Type, "collections.OrderedDict"),
    (PyPickleBuffer_Type, "pickle.PickleBuffer"),
    (PySentinel_Type, "sentinel"),
    (PyStdPrinter_Type, "stderrprinter"),
);

/// Called before the first extension module is loaded.
pub fn init() {
    init_placeholders();
    super::capsule::init_type();
}

#[repr(transparent)]
pub struct SyncPtr<T>(pub *const T);
unsafe impl<T> Sync for SyncPtr<T> {}

#[unsafe(no_mangle)]
pub static Py_hexdigits: SyncPtr<c_char> = SyncPtr(c"0123456789abcdef".as_ptr());

#[unsafe(no_mangle)]
pub static PyUnstable_ExecutableKinds: [SyncPtr<PyTypeObject>; 6] = [const { SyncPtr(std::ptr::null()) }; 6];

const fn ctype_table() -> [c_uint; 256] {
    let mut t = [0u32; 256];
    let mut c = 0;
    while c < 256 {
        let b = c as u8;
        let mut f = 0;
        if b.is_ascii_lowercase() {
            f |= 0x01;
        }
        if b.is_ascii_uppercase() {
            f |= 0x02;
        }
        if b.is_ascii_digit() {
            f |= 0x04;
        }
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == 0x0b || b == 0x0c {
            f |= 0x08;
        }
        if b.is_ascii_hexdigit() {
            f |= 0x10;
        }
        t[c] = f;
        c += 1;
    }
    t
}
#[unsafe(no_mangle)]
pub static _Py_ctype_table: [c_uint; 256] = ctype_table();
const fn case_table(upper: bool) -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let b = c as u8;
        t[c] = if upper { b.to_ascii_uppercase() } else { b.to_ascii_lowercase() };
        c += 1;
    }
    t
}
#[unsafe(no_mangle)]
pub static _Py_ctype_tolower: [u8; 256] = case_table(false);
#[unsafe(no_mangle)]
pub static _Py_ctype_toupper: [u8; 256] = case_table(true);
const fn ascii_ws() -> [u8; 128] {
    let mut t = [0u8; 128];
    // \t \n \v \f \r, 0x1c-0x1f, space
    let ws = [9u8, 10, 11, 12, 13, 28, 29, 30, 31, 32];
    let mut i = 0;
    while i < ws.len() {
        t[ws[i] as usize] = 1;
        i += 1;
    }
    t
}
#[unsafe(no_mangle)]
pub static _Py_ascii_whitespace: [u8; 128] = ascii_ws();

// legacy (deprecated) global configuration variables
macro_rules! int_flags {
    ($($n:ident = $v:expr),*) => { $( #[unsafe(no_mangle)] pub static mut $n: c_int = $v; )* };
}
int_flags!(
    Py_BytesWarningFlag = 0, Py_DebugFlag = 0, Py_DontWriteBytecodeFlag = 0, Py_FrozenFlag = 0, Py_HashRandomizationFlag = 1,
    Py_IgnoreEnvironmentFlag = 0, Py_InspectFlag = 0, Py_InteractiveFlag = 0, Py_IsolatedFlag = 0, Py_NoSiteFlag = 0,
    Py_NoUserSiteDirectory = 0, Py_OptimizeFlag = 0, Py_QuietFlag = 0, Py_UTF8Mode = 1, Py_UnbufferedStdioFlag = 0,
    Py_VerboseFlag = 0, Py_HasFileSystemDefaultEncoding = 1
);
#[unsafe(no_mangle)]
pub static Py_FileSystemDefaultEncoding: SyncPtr<c_char> = SyncPtr(c"utf-8".as_ptr());
#[unsafe(no_mangle)]
pub static Py_FileSystemDefaultEncodeErrors: SyncPtr<c_char> = SyncPtr(c"surrogateescape".as_ptr());
