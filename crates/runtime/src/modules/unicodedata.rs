//! `unicodedata`: the Unicode character database (current version and the
//! `ucd_3_2_0` object) over the rpy-unicode tables, plus the 3.15 additions
//! (block, grapheme_cluster_break, indic_conjunct_break, extended_pictographic,
//! iter_graphemes / Segment).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::text_util as tu;
use crate::object::*;
use crate::types::str as pystr;
use crate::types::typeobject::tname;
use crate::types::{bool_, float, int, none};
use rpy_unicode::{Form, Lookup, Ucd};

#[repr(C)]
struct UcdObj {
    ob_base: PyObject,
    v320: bool,
}

#[repr(C)]
struct GraphemeIter {
    ob_base: PyObject,
    string: *mut PyObject,
    pos: usize,
    end: usize,
}

#[repr(C)]
struct SegmentObj {
    ob_base: PyObject,
    string: *mut PyObject,
    start: usize,
    end: usize,
}

static mut UCD_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut GITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut SEGMENT_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn ucd_type() -> *mut PyTypeObject {
    &raw mut UCD_TYPE
}

/// The database selected by `self` (the module = current, a UCD object = its version).
fn db(slf: &PyObject) -> Ucd {
    if slf.is_type(ucd_type()) && unsafe { (*(slf.as_ptr() as *const UcdObj)).v320 } {
        Ucd::V3_2_0
    } else {
        Ucd::Current
    }
}

fn char_arg(fname: &str, o: &PyObject) -> PyResult<u32> {
    char_arg_n(fname, "", o)
}

fn char_arg_n(fname: &str, argn: &str, o: &PyObject) -> PyResult<u32> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument{} must be a unicode character, not {}", fname, argn, tname(o));
    }
    let n = pystr::str_len(o);
    if n != 1 {
        raise!(TypeError, "{}() argument{} must be a unicode character, not a string of length {}", fname, argn, n);
    }
    Ok(pystr::str_data(o).get(0))
}

fn args_1_2<'a>(fname: &str, args: &[&'a PyObject]) -> PyResult<(u32, Option<&'a PyObject>)> {
    crate::args::check_pos(fname, args, 1, 2)?;
    Ok((char_arg_n(fname, " 1", args[0])?, args.get(1).copied()))
}

fn s(v: &str) -> PyRef {
    pystr::new_str(v)
}

// ------------------------------------------------------------------ properties

fn u_decimal(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    let (c, d) = args_1_2("decimal", args)?;
    match db(slf).decimal(c) {
        Some(v) => Ok(int::new_int(v as i64)),
        None => match d {
            Some(d) => Ok(d.to_ref()),
            None => raise!(ValueError, "not a decimal"),
        },
    }
}
fn u_digit(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    let (c, d) = args_1_2("digit", args)?;
    match db(slf).digit(c) {
        Some(v) => Ok(int::new_int(v as i64)),
        None => match d {
            Some(d) => Ok(d.to_ref()),
            None => raise!(ValueError, "not a digit"),
        },
    }
}
fn u_numeric(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    let (c, d) = args_1_2("numeric", args)?;
    match db(slf).numeric(c) {
        Some(v) => Ok(float::new_float(v)),
        None => match d {
            Some(d) => Ok(d.to_ref()),
            None => raise!(ValueError, "not a numeric character"),
        },
    }
}
fn u_category(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(db(slf).category(char_arg("category", o)?)))
}
fn u_bidirectional(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(db(slf).bidirectional(char_arg("bidirectional", o)?)))
}
fn u_combining(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(db(slf).combining(char_arg("combining", o)?) as i64))
}
fn u_mirrored(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(db(slf).mirrored(char_arg("mirrored", o)?) as i64))
}
fn u_east_asian_width(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(db(slf).east_asian_width(char_arg("east_asian_width", o)?)))
}
fn u_decomposition(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(&db(slf).decomposition(char_arg("decomposition", o)?)))
}
fn u_isxidstart(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(db(slf).isxidstart(char_arg("isxidstart", o)?)))
}
fn u_isxidcontinue(slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(db(slf).isxidcontinue(char_arg("isxidcontinue", o)?)))
}
fn u_block(_slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(rpy_unicode::block(char_arg("block", o)?)))
}
fn u_grapheme_cluster_break(_slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(rpy_unicode::grapheme_cluster_break(char_arg("grapheme_cluster_break", o)?)))
}
fn u_indic_conjunct_break(_slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(s(rpy_unicode::indic_conjunct_break(char_arg("indic_conjunct_break", o)?)))
}
fn u_extended_pictographic(_slf: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(rpy_unicode::extended_pictographic(char_arg("extended_pictographic", o)?)))
}

fn u_name(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    let (c, d) = args_1_2("name", args)?;
    match db(slf).name(c) {
        Some(n) => Ok(s(&n)),
        None => match d {
            Some(d) => Ok(d.to_ref()),
            None => raise!(ValueError, "no such name"),
        },
    }
}

fn u_lookup(slf: &PyObject, o: &PyObject) -> PyResult {
    let name: String = if pystr::is_str(o) {
        pystr::to_string_lossy(o)
    } else if crate::types::bytes::is_bytes(o) {
        String::from_utf8_lossy(crate::types::bytes::bytes_data(o)).into_owned()
    } else {
        raise!(TypeError, "lookup() argument must be str, not {}", tname(o));
    };
    if name.len() > rpy_unicode::NAME_MAX_LEN {
        raise!(KeyError, "name too long");
    }
    match db(slf).lookup(&name) {
        Some(Lookup::Char(c)) => Ok(pystr::from_codepoints(&[c])),
        Some(Lookup::Sequence(seq)) => Ok(pystr::from_codepoints(seq)),
        None => {
            let msg = pystr::new_str(&format!("undefined character name '{}'", name));
            Err(set_err_obj(crate::types::exceptions::KeyError(), msg))
        }
    }
}

fn form_arg(o: &PyObject) -> PyResult<Form> {
    if !pystr::is_str(o) {
        raise!(TypeError, "normalize() argument 1 must be str, not {}", tname(o));
    }
    match Form::from_name(&pystr::to_string_lossy(o)) {
        Some(f) => Ok(f),
        None => raise!(ValueError, "invalid normalization form"),
    }
}

fn str_arg2<'a>(fname: &str, o: &'a PyObject) -> PyResult<&'a PyObject> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument 2 must be str, not {}", fname, tname(o));
    }
    Ok(o)
}

fn u_normalize(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("normalize", args, 2, 2)?;
    let form = form_arg(args[0])?;
    let o = str_arg2("normalize", args[1])?;
    let cps = pystr::str_data(o).to_vec();
    match db(slf).normalize_if_needed(form, &cps) {
        None => {
            if pystr::is_str_exact(o) {
                Ok(o.to_ref())
            } else {
                Ok(pystr::from_codepoints(&cps))
            }
        }
        Some(v) => Ok(pystr::from_codepoints(&v)),
    }
}

fn u_is_normalized(slf: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("is_normalized", args, 2, 2)?;
    let form = form_arg(args[0])?;
    let o = str_arg2("is_normalized", args[1])?;
    let cps = pystr::str_data(o).to_vec();
    Ok(bool_::new_bool(db(slf).is_normalized(form, &cps)))
}

// ------------------------------------------------------------------ graphemes

fn clamp_index(i: isize, len: usize) -> usize {
    let l = len as isize;
    let v = if i < 0 { i + l } else { i };
    v.clamp(0, l) as usize
}

fn u_iter_graphemes(_slf: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("iter_graphemes", args, 1, 3)?;
    let o = args[0];
    if !pystr::is_str(o) {
        raise!(TypeError, "iter_graphemes() argument 1 must be str, not {}", tname(o));
    }
    let len = pystr::str_len(o);
    let start = match args.get(1) {
        Some(v) => clamp_index(ab::index_as_ssize(v, None)?, len),
        None => 0,
    };
    let end = match args.get(2) {
        Some(v) => clamp_index(ab::index_as_ssize(v, None)?, len),
        None => len,
    };
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut GITER_TYPE, std::mem::size_of::<GraphemeIter>());
        let g = &mut *(op as *mut GraphemeIter);
        g.string = o.to_ref().into_raw();
        g.pos = start.min(end);
        g.end = end;
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn giter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let g = &mut *(o as *mut GraphemeIter);
        if g.pos >= g.end {
            return std::ptr::null_mut();
        }
        let d = pystr::str_data(&*g.string);
        let start = g.pos;
        let next = rpy_unicode::next_grapheme_break_by(|i| d.get(i), start, g.end);
        g.pos = next;
        let op = crate::alloc::alloc_object(&raw mut SEGMENT_TYPE, std::mem::size_of::<SegmentObj>());
        let sg = &mut *(op as *mut SegmentObj);
        sg.string = (*g.string).to_ref().into_raw();
        sg.start = start;
        sg.end = next;
        op
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

unsafe extern "C" fn giter_dealloc(o: *mut PyObject) {
    unsafe {
        xdecref((*(o as *mut GraphemeIter)).string);
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn segment_dealloc(o: *mut PyObject) {
    unsafe {
        xdecref((*(o as *mut SegmentObj)).string);
        crate::alloc::free_object(o);
    }
}

fn seg(o: &PyObject) -> &SegmentObj {
    unsafe { &*(o.as_ptr() as *const SegmentObj) }
}

unsafe extern "C" fn segment_str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let sg = seg(&*o);
        tu::substr(&*sg.string, sg.start, sg.end).into_raw()
    }
}

unsafe extern "C" fn segment_repr(o: *mut PyObject) -> *mut PyObject {
    let sg = unsafe { seg(&*o) };
    pystr::new_str(&format!("<Segment {}:{}>", sg.start, sg.end)).into_raw()
}

fn segment_start(o: &PyObject) -> PyResult {
    Ok(int::new_int(seg(o).start as i64))
}
fn segment_end(o: &PyObject) -> PyResult {
    Ok(int::new_int(seg(o).end as i64))
}

// ------------------------------------------------------------------ module

fn ucd_unidata_version(o: &PyObject) -> PyResult {
    Ok(s(db(o).unidata_version()))
}

fn methods() -> *mut PyMethodDef {
    crate::mdefs![
        ("decimal", FAST, u_decimal, "Converts a Unicode character into its equivalent decimal value.\n\nReturns the decimal value assigned to the character chr as integer.\nIf no such value is defined, default is returned, or, if not given,\nValueError is raised."),
        ("digit", FAST, u_digit, "Converts a Unicode character into its equivalent digit value.\n\nReturns the digit value assigned to the character chr as integer.\nIf no such value is defined, default is returned, or, if not given,\nValueError is raised."),
        ("numeric", FAST, u_numeric, "Converts a Unicode character into its equivalent numeric value.\n\nReturns the numeric value assigned to the character chr as float.\nIf no such value is defined, default is returned, or, if not given,\nValueError is raised."),
        ("category", O, u_category, "Returns the general category assigned to the character chr as string."),
        ("bidirectional", O, u_bidirectional, "Returns the bidirectional class assigned to the character chr as string.\n\nIf no such value is defined, an empty string is returned."),
        ("combining", O, u_combining, "Returns the canonical combining class assigned to the character chr as integer.\n\nReturns 0 if no combining class is defined."),
        ("mirrored", O, u_mirrored, "Returns the mirrored property assigned to the character chr as integer.\n\nReturns 1 if the character has been identified as a \"mirrored\"\ncharacter in bidirectional text, 0 otherwise."),
        ("east_asian_width", O, u_east_asian_width, "Returns the east asian width assigned to the character chr as string."),
        ("decomposition", O, u_decomposition, "Returns the character decomposition mapping assigned to the character chr as string.\n\nAn empty string is returned in case no such mapping is defined."),
        ("isxidstart", O, u_isxidstart, "Return True if the character has the XID_Start property, else False."),
        ("isxidcontinue", O, u_isxidcontinue, "Return True if the character has the XID_Continue property, else False."),
        ("block", O, u_block, "Return the Unicode block name of the character chr ('No_Block' if none)."),
        ("grapheme_cluster_break", O, u_grapheme_cluster_break, "Return the Grapheme_Cluster_Break property of the character chr."),
        ("indic_conjunct_break", O, u_indic_conjunct_break, "Return the Indic_Conjunct_Break property of the character chr."),
        ("extended_pictographic", O, u_extended_pictographic, "Return True if the character has the Extended_Pictographic property."),
        ("name", FAST, u_name, "Returns the name assigned to the character chr as a string.\n\nIf no name is defined, default is returned, or, if not given,\nValueError is raised."),
        ("lookup", O, u_lookup, "Look up character by name.\n\nIf a character with the given name is found, return the\ncorresponding character.  If not found, KeyError is raised."),
        ("normalize", FAST, u_normalize, "Return the normal form 'form' for the Unicode string unistr.\n\nValid values for form are 'NFC', 'NFKC', 'NFD', and 'NFKD'."),
        ("is_normalized", FAST, u_is_normalized, "Return whether the Unicode string unistr is in the normal form 'form'.\n\nValid values for form are 'NFC', 'NFKC', 'NFD', and 'NFKD'."),
        ("iter_graphemes", FAST, u_iter_graphemes, "Return an iterator over the extended grapheme clusters of unistr[start:end]."),
    ]
}

fn init_types() -> PyResult<()> {
    unsafe {
        let t = &mut *ucd_type();
        if !t.tp_name.is_null() {
            return Ok(());
        }
        t.tp_name = c"unicodedata.UCD".as_ptr();
        t.tp_basicsize = std::mem::size_of::<UcdObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_methods = methods();
        t.tp_getset = crate::getset_table![("unidata_version", ucd_unidata_version)];

        let t = &mut *(&raw mut GITER_TYPE);
        t.tp_name = c"unicodedata.GraphemeBreakIterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<GraphemeIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION;
        t.tp_dealloc = Some(giter_dealloc);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(giter_next);

        let t = &mut *(&raw mut SEGMENT_TYPE);
        t.tp_name = c"unicodedata.Segment".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SegmentObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION;
        t.tp_dealloc = Some(segment_dealloc);
        t.tp_str = Some(segment_str);
        t.tp_repr = Some(segment_repr);
        t.tp_getset = crate::getset_table![("start", segment_start), ("end", segment_end)];
        for t in [ucd_type(), &raw mut GITER_TYPE, &raw mut SEGMENT_TYPE] {
            tu::ready_type(t)?;
        }
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("unicodedata")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, methods(), Some(&m))?;
    tu::dict_set(&d, "unidata_version", &s(rpy_unicode::UNIDATA_VERSION))?;
    tu::dict_set(&d, "UCD", crate::types::typeobject::type_obj(ucd_type()))?;
    tu::dict_set(&d, "Segment", crate::types::typeobject::type_obj(&raw mut SEGMENT_TYPE))?;
    let old = unsafe {
        let op = crate::alloc::alloc_object(ucd_type(), std::mem::size_of::<UcdObj>());
        (*(op as *mut UcdObj)).v320 = true;
        PyRef::from_raw(op)
    };
    tu::dict_set(&d, "ucd_3_2_0", &old)?;
    tu::dict_set(&d, "__doc__", &s("This module provides access to the Unicode Character Database which\ndefines character properties for all Unicode characters. The data in\nthis database is based on the UnicodeData.txt file version\n17.0.0 which is publicly available from ftp://ftp.unicode.org/.\n\nThe module uses the same names and symbols as defined by the\nUnicodeData File Format 17.0.0."))?;
    let _ = none::none;
    Ok(m)
}
