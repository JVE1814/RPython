//! CField: the descriptor for a structure/union field.

use super::cdata::*;
use super::*;
use rpy_runtime::modules::fncommon::{self as fc, Trav};

pub struct CField {
    pub name: PyRef,
    pub proto: PyRef,
    pub byte_offset: isize,
    pub byte_size: isize,
    pub index: isize,
    /// Some((bit_size, bit_offset)) for bit fields.
    pub bits: Option<(u32, u32)>,
    pub anonymous: bool,
    pub code: FieldCode,
}

impl Trav for CField {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        let r = v(self.name.as_ptr());
        if r != 0 {
            return r;
        }
        v(self.proto.as_ptr())
    }
}

pub static mut CFIELD_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn cfield_type() -> *mut PyTypeObject {
    &raw mut CFIELD_TYPE
}

pub fn is_cfield(o: &PyObject) -> bool {
    o.type_ptr() == cfield_type()
}

pub fn field(o: &PyObject) -> &'static mut CField {
    unsafe { &mut *(fc::rs::<CField>(o) as *mut CField) }
}

fn is_int_code(code: u8) -> bool {
    matches!(code, b'b' | b'B' | b'h' | b'H' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'?' | b'c' | b'v')
}

pub fn new_cfield(name: &PyObject, proto: &PyObject, byte_size: isize, byte_offset: isize, index: isize, bits: Option<(isize, isize)>) -> PyResult {
    if !is_str(name) {
        raise!(TypeError, "field name must be str, not {}", tname(name));
    }
    let info = match stginfo_of_type(proto) {
        Some(i) => i,
        None => raise!(TypeError, "type of field {} must be a C type", repr(name)?),
    };
    if byte_size != info.size {
        raise!(ValueError, "byte size of field {} ({}) does not match type size ({})", repr(name)?, byte_size, info.size);
    }
    if byte_offset < 0 {
        raise!(ValueError, "byte offset of field {} must not be negative", repr(name)?);
    }
    let bits = match bits {
        None => None,
        Some((bs, bo)) => {
            if !(info.kind == Kind::Simple && is_int_code(info.code)) {
                raise!(TypeError, "bit fields not allowed for type {}", type_name(proto.as_ptr() as *mut PyTypeObject));
            }
            if bs <= 0 || bs > byte_size * 8 {
                raise!(ValueError, "number of bits invalid for bit field {}", repr(name)?);
            }
            if bo < 0 {
                raise!(ValueError, "bit offset of field {} must not be negative", repr(name)?);
            }
            if bo + bs > byte_size * 8 {
                raise!(ValueError, "bit field {} overflows its type ({} + {} > {})", repr(name)?, bo, bs, byte_size * 8);
            }
            Some((bs as u32, bo as u32))
        }
    };
    // char / wchar arrays inside structures read as bytes / str
    let mut code = FieldCode::None;
    if info.kind == Kind::Array {
        if let Some(p) = &info.proto {
            if let Some(pi) = stginfo_of_type(p) {
                if pi.kind == Kind::Simple && !simple_instance(p.as_ptr() as *mut PyTypeObject) {
                    if pi.code == b'c' {
                        code = FieldCode::Code(b's');
                    } else if pi.code == b'u' {
                        code = FieldCode::Code(b'U');
                    }
                }
            }
        }
    }
    // keep the exact str (a subclass name is converted)
    let nm = if rpy_runtime::types::str::is_str_exact(name) { name.to_ref() } else { new_str(&str_of(name)) };
    let nm = rpy_runtime::types::str::intern(nm);
    fc::robj_new(
        cfield_type(),
        CField { name: nm, proto: proto.to_ref(), byte_offset, byte_size, index, bits, anonymous: false, code },
    )
}

unsafe extern "C" fn cfield_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = fc::VcArgs::from_c(args, kw);
        into_c(cfield_new_impl(&a))
    }
}

fn cfield_new_impl(a: &fc::VcArgs) -> PyResult {
    static P: rpy_runtime::args::Params =
        rpy_runtime::args::Params::new("CField", &["name", "type", "byte_size", "byte_offset", "index", "_internal_use", "bit_size", "bit_offset"], 0, 0)
            .kwonly(8);
    let [name, tp, byte_size, byte_offset, index, internal, bit_size, bit_offset] = a.parse::<8>(&P)?;
    let missing = |n: &str| -> PyResult { raise!(TypeError, "CField() missing required keyword-only argument: '{}'", n) };
    let name = match name {
        Some(n) => n,
        None => return missing("name"),
    };
    let tp = match tp {
        Some(n) => n,
        None => return missing("type"),
    };
    let byte_size = match byte_size {
        Some(n) => ssize(&n)?,
        None => return missing("byte_size"),
    };
    let byte_offset = match byte_offset {
        Some(n) => ssize(&n)?,
        None => return missing("byte_offset"),
    };
    let index = match index {
        Some(n) => ssize(&n)?,
        None => return missing("index"),
    };
    let internal = match internal {
        Some(n) => rpy_runtime::abstract_::is_true(&n)?,
        None => return missing("_internal_use"),
    };
    if !internal {
        raise!(TypeError, "CField is not intended to be used directly; use it only as a descriptor");
    }
    let bit_size = bit_size.filter(|x| !is_none(x));
    let bit_offset = bit_offset.filter(|x| !is_none(x));
    let bits = match (bit_size, bit_offset) {
        (None, None) => None,
        (Some(s), o) => Some((ssize(&s)?, match o {
            Some(o) => ssize(&o)?,
            None => 0,
        })),
        (None, Some(_)) => raise!(ValueError, "bit_offset requires bit_size"),
    };
    new_cfield(&name, &tp, byte_size, byte_offset, index, bits)
}

fn check_instance(inst: &PyObject) -> PyResult<()> {
    if !is_cdata(inst) {
        raise!(TypeError, "not a ctype instance");
    }
    Ok(())
}

unsafe extern "C" fn cfield_descr_get(d: *mut PyObject, inst: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if inst.is_null() || is_none(&*inst) {
            return (&*d).to_ref().into_raw();
        }
        let f = field(&*d);
        let o = &*inst;
        if check_instance(o).is_err() {
            return std::ptr::null_mut();
        }
        let c = cd(o);
        into_c(cdata_get(&f.proto, f.code, Some(o), f.index, f.byte_size, f.bits, c.b_ptr.offset(f.byte_offset)))
    }
}

unsafe extern "C" fn cfield_descr_set(d: *mut PyObject, inst: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let f = field(&*d);
        let o = &*inst;
        let r = (|| -> PyResult<()> {
            check_instance(o)?;
            if v.is_null() {
                raise!(TypeError, "can't delete attribute");
            }
            let c = cd(o);
            cdata_set(o, &f.proto, f.code, &*v, f.index, f.byte_size, f.bits, c.b_ptr.offset(f.byte_offset))
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn cfield_repr(d: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = field(&*d);
        let tn = type_name(f.proto.as_ptr() as *mut PyTypeObject);
        let s = match f.bits {
            Some((bs, bo)) => format!("<{} {} type={}, ofs={}, bit_size={}, bit_offset={}>", "CField", repr(&f.name).unwrap_or_default(), tn, f.byte_offset, bs, bo),
            None => format!("<{} {} type={}, ofs={}, size={}>", "CField", repr(&f.name).unwrap_or_default(), tn, f.byte_offset, f.byte_size),
        };
        new_str(&s).into_raw()
    }
}

fn g_name(o: &PyObject) -> PyResult {
    Ok(field(o).name.clone())
}
fn g_type(o: &PyObject) -> PyResult {
    Ok(field(o).proto.clone())
}
fn g_offset(o: &PyObject) -> PyResult {
    Ok(new_int(field(o).byte_offset as i64))
}
fn g_byte_size(o: &PyObject) -> PyResult {
    Ok(new_int(field(o).byte_size as i64))
}
fn g_size(o: &PyObject) -> PyResult {
    let f = field(o);
    Ok(match f.bits {
        Some((bs, bo)) => new_int(((bs as i64) << 16) | bo as i64),
        None => new_int(f.byte_size as i64),
    })
}
fn g_bit_size(o: &PyObject) -> PyResult {
    let f = field(o);
    Ok(match f.bits {
        Some((bs, _)) => new_int(bs as i64),
        None => rpy_runtime::types::int::new_int_i128(f.byte_size as i128 * 8),
    })
}
fn g_bit_offset(o: &PyObject) -> PyResult {
    Ok(new_int(field(o).bits.map_or(0, |b| b.1) as i64))
}
fn g_is_bitfield(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(field(o).bits.is_some()))
}
fn g_is_anonymous(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(field(o).anonymous))
}

fn cfield_get_method(d: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "__get__ expected 1 or 2 arguments, got {}", args.len());
    }
    let inst = args[0];
    if is_none(inst) {
        return Ok(d.to_ref());
    }
    from_c(unsafe { cfield_descr_get(d.as_ptr(), inst.as_ptr(), std::ptr::null_mut()) })
}

pub fn init_type() -> PyResult<()> {
    unsafe {
        let t = cfield_type();
        fc::init_robj_type::<CField>(t, c"_ctypes.CField", 0);
        (*t).tp_new = Some(cfield_new);
        (*t).tp_descr_get = Some(cfield_descr_get);
        (*t).tp_descr_set = Some(cfield_descr_set);
        (*t).tp_repr = Some(cfield_repr);
        (*t).tp_doc = c"Structure/Union member".as_ptr();
        let _ = cfield_get_method;
        (*t).tp_getset = rpy_runtime::getset_table![
            ("name", g_name; "name of this field"),
            ("type", g_type; "type of this field"),
            ("offset", g_offset; "offset in bytes of this field (same as byte_offset)"),
            ("byte_offset", g_offset; "offset in bytes of this field"),
            ("byte_size", g_byte_size; "size of this field in bytes"),
            ("size", g_size; "size in bytes of this field; for bit fields, the bit size and offset packed together"),
            ("bit_size", g_bit_size; "size of this field in bits"),
            ("bit_offset", g_bit_offset; "additional offset in bits (relative to byte_offset)"),
            ("is_bitfield", g_is_bitfield; "true if this is a bitfield"),
            ("is_anonymous", g_is_anonymous; "true if this field is anonymous"),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
