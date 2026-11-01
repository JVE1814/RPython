//! Minimal libffi binding (loaded with dlopen). Declarations written from the
//! documented libffi ABI (ffi.h of libffi 3.4, x86-64 / aarch64 SysV).

use std::ffi::{c_int, c_uint, c_ushort, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiType {
    pub size: usize,
    pub alignment: c_ushort,
    pub type_: c_ushort,
    pub elements: *mut *mut FfiType,
}
unsafe impl Send for FfiType {}
unsafe impl Sync for FfiType {}

impl FfiType {
    pub const fn zero() -> FfiType {
        FfiType { size: 0, alignment: 0, type_: 0, elements: std::ptr::null_mut() }
    }
}

pub const FFI_TYPE_VOID: u16 = 0;
pub const FFI_TYPE_INT: u16 = 1;
pub const FFI_TYPE_FLOAT: u16 = 2;
pub const FFI_TYPE_DOUBLE: u16 = 3;
pub const FFI_TYPE_LONGDOUBLE: u16 = 4;
pub const FFI_TYPE_UINT8: u16 = 5;
pub const FFI_TYPE_SINT8: u16 = 6;
pub const FFI_TYPE_UINT16: u16 = 7;
pub const FFI_TYPE_SINT16: u16 = 8;
pub const FFI_TYPE_UINT32: u16 = 9;
pub const FFI_TYPE_SINT32: u16 = 10;
pub const FFI_TYPE_UINT64: u16 = 11;
pub const FFI_TYPE_SINT64: u16 = 12;
pub const FFI_TYPE_STRUCT: u16 = 13;
pub const FFI_TYPE_POINTER: u16 = 14;
pub const FFI_TYPE_COMPLEX: u16 = 15;

#[cfg(target_arch = "x86_64")]
pub const FFI_DEFAULT_ABI: c_int = 2; // FFI_UNIX64
#[cfg(target_arch = "aarch64")]
pub const FFI_DEFAULT_ABI: c_int = 1; // FFI_SYSV

/// ffi_cif (plus room for target-specific extra fields).
#[repr(C)]
pub struct FfiCif {
    pub abi: c_int,
    pub nargs: c_uint,
    pub arg_types: *mut *mut FfiType,
    pub rtype: *mut FfiType,
    pub bytes: c_uint,
    pub flags: c_uint,
    pub _extra: [u64; 4],
}

impl FfiCif {
    pub fn new() -> FfiCif {
        unsafe { std::mem::zeroed() }
    }
}

pub type ClosureFun = unsafe extern "C" fn(cif: *mut FfiCif, ret: *mut c_void, args: *mut *mut c_void, user: *mut c_void);

crate::dyn_lib!(LibFfi, ["libffi.so.8", "libffi.so.7", "libffi.so"], {
    fn ffi_prep_cif(cif: *mut FfiCif, abi: c_int, nargs: c_uint, rtype: *mut FfiType, atypes: *mut *mut FfiType) -> c_int;
    fn ffi_prep_cif_var(cif: *mut FfiCif, abi: c_int, nfixed: c_uint, ntotal: c_uint, rtype: *mut FfiType, atypes: *mut *mut FfiType) -> c_int;
    fn ffi_call(cif: *mut FfiCif, f: *mut c_void, rvalue: *mut c_void, avalue: *mut *mut c_void);
    fn ffi_closure_alloc(size: usize, code: *mut *mut c_void) -> *mut c_void;
    fn ffi_closure_free(c: *mut c_void);
    fn ffi_prep_closure_loc(closure: *mut c_void, cif: *mut FfiCif, fun: ClosureFun, user: *mut c_void, codeloc: *mut c_void) -> c_int;
    fn ffi_get_struct_offsets(abi: c_int, st: *mut FfiType, offsets: *mut usize) -> c_int;
});

pub fn lib() -> Result<&'static LibFfi, String> {
    LibFfi::get()
}

/// Address of a builtin `ffi_type_*` object exported by libffi.
pub fn builtin_type(name: &str) -> *mut FfiType {
    match lib() {
        Ok(l) => l.lib.sym(name) as *mut FfiType,
        Err(_) => std::ptr::null_mut(),
    }
}

/// Lazily resolved builtin types.
pub struct Types {
    pub void: *mut FfiType,
    pub uint8: *mut FfiType,
    pub sint8: *mut FfiType,
    pub uint16: *mut FfiType,
    pub sint16: *mut FfiType,
    pub uint32: *mut FfiType,
    pub sint32: *mut FfiType,
    pub uint64: *mut FfiType,
    pub sint64: *mut FfiType,
    pub float: *mut FfiType,
    pub double: *mut FfiType,
    pub longdouble: *mut FfiType,
    pub pointer: *mut FfiType,
    pub complex_float: *mut FfiType,
    pub complex_double: *mut FfiType,
    pub complex_longdouble: *mut FfiType,
}
unsafe impl Send for Types {}
unsafe impl Sync for Types {}

// Local fallbacks used when libffi is not available (sizes/alignments only).
static mut FALLBACK: [FfiType; 16] = [FfiType::zero(); 16];

pub fn types() -> &'static Types {
    static T: std::sync::OnceLock<Types> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let ok = lib().is_ok();
        let mk = |i: usize, name: &str, size: usize, align: u16, t: u16| -> *mut FfiType {
            if ok {
                let p = builtin_type(name);
                if !p.is_null() {
                    return p;
                }
            }
            unsafe {
                let p = &raw mut FALLBACK[i];
                (*p).size = size;
                (*p).alignment = align;
                (*p).type_ = t;
                p
            }
        };
        Types {
            void: mk(0, "ffi_type_void", 1, 1, FFI_TYPE_VOID),
            uint8: mk(1, "ffi_type_uint8", 1, 1, FFI_TYPE_UINT8),
            sint8: mk(2, "ffi_type_sint8", 1, 1, FFI_TYPE_SINT8),
            uint16: mk(3, "ffi_type_uint16", 2, 2, FFI_TYPE_UINT16),
            sint16: mk(4, "ffi_type_sint16", 2, 2, FFI_TYPE_SINT16),
            uint32: mk(5, "ffi_type_uint32", 4, 4, FFI_TYPE_UINT32),
            sint32: mk(6, "ffi_type_sint32", 4, 4, FFI_TYPE_SINT32),
            uint64: mk(7, "ffi_type_uint64", 8, 8, FFI_TYPE_UINT64),
            sint64: mk(8, "ffi_type_sint64", 8, 8, FFI_TYPE_SINT64),
            float: mk(9, "ffi_type_float", 4, 4, FFI_TYPE_FLOAT),
            double: mk(10, "ffi_type_double", 8, 8, FFI_TYPE_DOUBLE),
            longdouble: mk(11, "ffi_type_longdouble", 16, 16, FFI_TYPE_LONGDOUBLE),
            pointer: mk(12, "ffi_type_pointer", 8, 8, FFI_TYPE_POINTER),
            complex_float: mk(13, "ffi_type_complex_float", 8, 4, FFI_TYPE_COMPLEX),
            complex_double: mk(14, "ffi_type_complex_double", 16, 8, FFI_TYPE_COMPLEX),
            complex_longdouble: mk(15, "ffi_type_complex_longdouble", 32, 16, FFI_TYPE_COMPLEX),
        }
    })
}
