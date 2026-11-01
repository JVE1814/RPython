//! `_locale`: POSIX locale support (setlocale, localeconv, strcoll, strxfrm,
//! nl_langinfo, getencoding, gettext bindings) over the C library.

use crate::errors::*;
use crate::ffi::*;
use crate::modules::text_util as tu;
use crate::object::*;
use crate::types::str as pystr;
use crate::types::typeobject::tname;
use crate::types::{dict, int, list, none};
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

type NlItem = c_int;

unsafe extern "C" {
    fn nl_langinfo(item: NlItem) -> *mut c_char;
    fn wcscoll(a: *const i32, b: *const i32) -> c_int;
    fn wcsxfrm(dst: *mut i32, src: *const i32, n: usize) -> usize;
    fn textdomain(d: *const c_char) -> *mut c_char;
    fn bindtextdomain(d: *const c_char, dir: *const c_char) -> *mut c_char;
    fn bind_textdomain_codeset(d: *const c_char, cs: *const c_char) -> *mut c_char;
    fn gettext(m: *const c_char) -> *mut c_char;
    fn dgettext(d: *const c_char, m: *const c_char) -> *mut c_char;
    fn dcgettext(d: *const c_char, m: *const c_char, c: c_int) -> *mut c_char;
}

// glibc nl_item values.
const LANGINFO: &[(&str, NlItem)] = &[
    ("CODESET", 14),
    ("D_T_FMT", 0x20028),
    ("D_FMT", 0x20029),
    ("T_FMT", 0x2002A),
    ("T_FMT_AMPM", 0x2002B),
    ("AM_STR", 0x20026),
    ("PM_STR", 0x20027),
    ("DAY_1", 0x20007),
    ("DAY_2", 0x20008),
    ("DAY_3", 0x20009),
    ("DAY_4", 0x2000A),
    ("DAY_5", 0x2000B),
    ("DAY_6", 0x2000C),
    ("DAY_7", 0x2000D),
    ("ABDAY_1", 0x20000),
    ("ABDAY_2", 0x20001),
    ("ABDAY_3", 0x20002),
    ("ABDAY_4", 0x20003),
    ("ABDAY_5", 0x20004),
    ("ABDAY_6", 0x20005),
    ("ABDAY_7", 0x20006),
    ("MON_1", 0x2001A),
    ("MON_2", 0x2001B),
    ("MON_3", 0x2001C),
    ("MON_4", 0x2001D),
    ("MON_5", 0x2001E),
    ("MON_6", 0x2001F),
    ("MON_7", 0x20020),
    ("MON_8", 0x20021),
    ("MON_9", 0x20022),
    ("MON_10", 0x20023),
    ("MON_11", 0x20024),
    ("MON_12", 0x20025),
    ("ABMON_1", 0x2000E),
    ("ABMON_2", 0x2000F),
    ("ABMON_3", 0x20010),
    ("ABMON_4", 0x20011),
    ("ABMON_5", 0x20012),
    ("ABMON_6", 0x20013),
    ("ABMON_7", 0x20014),
    ("ABMON_8", 0x20015),
    ("ABMON_9", 0x20016),
    ("ABMON_10", 0x20017),
    ("ABMON_11", 0x20018),
    ("ABMON_12", 0x20019),
    ("RADIXCHAR", 0x10000),
    ("THOUSEP", 0x10001),
    ("YESEXPR", 0x50000),
    ("NOEXPR", 0x50001),
    ("CRNCYSTR", 0x4000F),
    ("ERA", 0x2002C),
    ("ERA_D_FMT", 0x2002E),
    ("ERA_D_T_FMT", 0x20030),
    ("ERA_T_FMT", 0x20031),
    ("ALT_DIGITS", 0x2002F),
];
const ERA: NlItem = 0x2002C;
const ALT_DIGITS: NlItem = 0x2002F;

static ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn locale_error(msg: &str) -> PyErr {
    let p = ERROR.load(Ordering::Acquire);
    if p.is_null() {
        return err!(ValueError, "{}", msg);
    }
    set_err_obj(p as *mut PyTypeObject, pystr::new_str(msg))
}

/// Decode a C string from the current LC_CTYPE encoding.
fn decode_locale(b: &[u8]) -> PyResult {
    if let Ok(s) = std::str::from_utf8(b) {
        return Ok(pystr::new_str(s));
    }
    let cs = unsafe { CStr::from_ptr(nl_langinfo(14)) }.to_string_lossy().into_owned();
    let cs = if cs.is_empty() { "ascii".to_string() } else { cs };
    match crate::modules::_codecs::decode_text(b, None, &cs, "surrogateescape") {
        Ok(s) => Ok(s),
        Err(_) => {
            clear_exc();
            crate::modules::_codecs::decode_text(b, None, "latin-1", "strict")
        }
    }
}

fn cstr_bytes<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    unsafe { CStr::from_ptr(p) }.to_bytes()
}

fn c_arg(o: &PyObject, fname: &str) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument must be str, not {}", fname, tname(o));
    }
    let s = pystr::as_utf8(o)?;
    match CString::new(s) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null character"),
    }
}

fn opt_c_arg(o: Option<&PyObject>, fname: &str) -> PyResult<Option<CString>> {
    match o {
        None => Ok(None),
        Some(v) if none::is_none(v) => Ok(None),
        Some(v) => c_arg(v, fname).map(Some),
    }
}

fn m_setlocale(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setlocale", args, 1, 2)?;
    let cat = crate::abstract_::as_i64(args[0])? as c_int;
    let loc = opt_c_arg(args.get(1).copied(), "setlocale")?;
    if !(0..=12).contains(&cat) {
        return Err(locale_error("invalid locale category"));
    }
    let r = unsafe { libc::setlocale(cat, loc.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if r.is_null() {
        return Err(locale_error("unsupported locale setting"));
    }
    decode_locale(cstr_bytes(r))
}

fn grouping(p: *const c_char) -> PyRef {
    let b = cstr_bytes(p);
    let out = list::new_list_empty();
    if b.is_empty() {
        return out;
    }
    for &c in b {
        list::append(&out, int::new_int(c as i8 as i64));
        if c as i8 == i8::MAX {
            return out;
        }
    }
    list::append(&out, int::new_int(0));
    out
}

fn m_localeconv(_m: &PyObject) -> PyResult {
    let lc = unsafe { &*libc::localeconv() };
    let d = dict::new_dict();
    let s = |k: &str, p: *const c_char| -> PyResult<()> { dict::set_item_str(&d, k, &*decode_locale(cstr_bytes(p))?) };
    let n = |k: &str, v: c_char| dict::set_item_str(&d, k, &int::new_int(v as i8 as i64));
    s("int_curr_symbol", lc.int_curr_symbol)?;
    s("currency_symbol", lc.currency_symbol)?;
    s("mon_decimal_point", lc.mon_decimal_point)?;
    s("mon_thousands_sep", lc.mon_thousands_sep)?;
    dict::set_item_str(&d, "mon_grouping", &grouping(lc.mon_grouping))?;
    s("positive_sign", lc.positive_sign)?;
    s("negative_sign", lc.negative_sign)?;
    n("int_frac_digits", lc.int_frac_digits)?;
    n("frac_digits", lc.frac_digits)?;
    n("p_cs_precedes", lc.p_cs_precedes)?;
    n("p_sep_by_space", lc.p_sep_by_space)?;
    n("n_cs_precedes", lc.n_cs_precedes)?;
    n("n_sep_by_space", lc.n_sep_by_space)?;
    n("p_sign_posn", lc.p_sign_posn)?;
    n("n_sign_posn", lc.n_sign_posn)?;
    s("decimal_point", lc.decimal_point)?;
    s("thousands_sep", lc.thousands_sep)?;
    dict::set_item_str(&d, "grouping", &grouping(lc.grouping))?;
    Ok(d)
}

fn wide(o: &PyObject, fname: &str, n: usize) -> PyResult<Vec<i32>> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument {} must be str, not {}", fname, n, tname(o));
    }
    let mut v: Vec<i32> = Vec::with_capacity(pystr::str_len(o) + 1);
    for c in pystr::str_data(o).iter() {
        if c == 0 {
            raise!(ValueError, "embedded null character");
        }
        v.push(c as i32);
    }
    v.push(0);
    Ok(v)
}

fn m_strcoll(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("strcoll", args, 2, 2)?;
    let a = wide(args[0], "strcoll", 1)?;
    let b = wide(args[1], "strcoll", 2)?;
    Ok(int::new_int(unsafe { wcscoll(a.as_ptr(), b.as_ptr()) } as i64))
}

fn m_strxfrm(_m: &PyObject, s: &PyObject) -> PyResult {
    let src = wide(s, "strxfrm", 1)?;
    let n = unsafe { wcsxfrm(std::ptr::null_mut(), src.as_ptr(), 0) };
    let mut buf = vec![0i32; n + 1];
    let n2 = unsafe { wcsxfrm(buf.as_mut_ptr(), src.as_ptr(), n + 1) };
    buf.truncate(n2.min(n));
    let cps: Vec<u32> = buf.iter().map(|&c| c as u32).collect();
    Ok(pystr::from_codepoints(&cps))
}

fn m_nl_langinfo(_m: &PyObject, k: &PyObject) -> PyResult {
    let key = crate::abstract_::as_i64(k)? as NlItem;
    if !LANGINFO.iter().any(|(_, v)| *v == key) {
        raise!(ValueError, "unsupported langinfo constant");
    }
    let p = unsafe { nl_langinfo(key) };
    if key == ERA || key == ALT_DIGITS {
        // A sequence of NUL-terminated strings; join them with ';'.
        let mut parts: Vec<Vec<u8>> = Vec::new();
        let mut q = p as *const c_char;
        let max = if key == ALT_DIGITS { 100 } else { usize::MAX };
        unsafe {
            while !q.is_null() && *q != 0 && parts.len() < max {
                let b = CStr::from_ptr(q).to_bytes();
                parts.push(b.to_vec());
                q = q.add(b.len() + 1);
            }
        }
        let joined = parts.join(&b';');
        return decode_locale(&joined);
    }
    decode_locale(cstr_bytes(p))
}

fn m_getencoding(_m: &PyObject) -> PyResult {
    let cs = cstr_bytes(unsafe { nl_langinfo(14) });
    let cs = String::from_utf8_lossy(cs).into_owned();
    let utf8_mode = crate::modules::sys::get_sys_attr("flags")
        .and_then(|f| crate::abstract_::getattr_str(&f, "utf8_mode").ok())
        .and_then(|v| crate::types::int::as_i64(&v))
        .unwrap_or(0)
        != 0;
    if crate::errors::err_occurred() {
        clear_exc();
    }
    if cs.is_empty() || utf8_mode {
        return Ok(pystr::new_str("utf-8"));
    }
    Ok(pystr::new_str(&cs))
}

fn m_gettext(_m: &PyObject, msg: &PyObject) -> PyResult {
    let m = c_arg(msg, "gettext")?;
    decode_locale(cstr_bytes(unsafe { gettext(m.as_ptr()) }))
}

fn m_dgettext(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("dgettext", args, 2, 2)?;
    let d = opt_c_arg(Some(args[0]), "dgettext")?;
    let m = c_arg(args[1], "dgettext")?;
    decode_locale(cstr_bytes(unsafe { dgettext(d.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), m.as_ptr()) }))
}

fn m_dcgettext(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("dcgettext", args, 3, 3)?;
    let d = opt_c_arg(Some(args[0]), "dcgettext")?;
    let m = c_arg(args[1], "dcgettext")?;
    let c = crate::abstract_::as_i64(args[2])? as c_int;
    decode_locale(cstr_bytes(unsafe { dcgettext(d.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), m.as_ptr(), c) }))
}

fn m_textdomain(_m: &PyObject, d: &PyObject) -> PyResult {
    let d = opt_c_arg(Some(d), "textdomain")?;
    let r = unsafe { textdomain(d.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if r.is_null() {
        return Err(crate::types::exceptions::last_os_error());
    }
    decode_locale(cstr_bytes(r))
}

fn m_bindtextdomain(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("bindtextdomain", args, 2, 2)?;
    let d = c_arg(args[0], "bindtextdomain")?;
    if d.as_bytes().is_empty() {
        raise!(ValueError, "empty string");
    }
    let dir = if none::is_none(args[1]) {
        None
    } else {
        let p = crate::abstract_::call(&*tu::import_attr("os", "fsencode")?, &[args[1]])?;
        Some(CString::new(crate::types::bytes::bytes_data(&p).to_vec()).map_err(|_| err!(ValueError, "embedded null byte"))?)
    };
    let r = unsafe { bindtextdomain(d.as_ptr(), dir.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if r.is_null() {
        return Err(crate::types::exceptions::last_os_error());
    }
    decode_locale(cstr_bytes(r))
}

fn m_bind_textdomain_codeset(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("bind_textdomain_codeset", args, 2, 2)?;
    let d = c_arg(args[0], "bind_textdomain_codeset")?;
    let cs = opt_c_arg(Some(args[1]), "bind_textdomain_codeset")?;
    let r = unsafe { bind_textdomain_codeset(d.as_ptr(), cs.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if r.is_null() {
        return Ok(none::none_ref());
    }
    decode_locale(cstr_bytes(r))
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_locale")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("setlocale", FAST, m_setlocale, "Activates/queries locale processing."),
            ("localeconv", NOARGS, m_localeconv, "Returns numeric and monetary locale-specific parameters."),
            ("strcoll", FAST, m_strcoll, "Compares two strings according to the locale."),
            ("strxfrm", O, m_strxfrm, "Return a string that can be used as a key for locale-aware comparisons."),
            ("nl_langinfo", O, m_nl_langinfo, "Return the value for the locale information associated with key."),
            ("getencoding", NOARGS, m_getencoding, "Get the current locale encoding."),
            ("gettext", O, m_gettext, "Return translation of msg."),
            ("dgettext", FAST, m_dgettext, "Return translation of msg in domain."),
            ("dcgettext", FAST, m_dcgettext, "Return translation of msg in domain and category."),
            ("textdomain", O, m_textdomain, "Set the C library's textdmain to domain, returning the new domain."),
            ("bindtextdomain", FAST, m_bindtextdomain, "Bind the C library's domain to dir."),
            ("bind_textdomain_codeset", FAST, m_bind_textdomain_codeset, "Bind the C library's domain to codeset."),
        ],
        Some(&m),
    )?;
    let cats: &[(&str, c_int)] = &[
        ("LC_CTYPE", libc::LC_CTYPE),
        ("LC_NUMERIC", libc::LC_NUMERIC),
        ("LC_TIME", libc::LC_TIME),
        ("LC_COLLATE", libc::LC_COLLATE),
        ("LC_MONETARY", libc::LC_MONETARY),
        ("LC_MESSAGES", libc::LC_MESSAGES),
        ("LC_ALL", libc::LC_ALL),
    ];
    for (k, v) in cats {
        tu::dict_set(&d, k, &int::new_int(*v as i64))?;
    }
    for (k, v) in LANGINFO {
        tu::dict_set(&d, k, &int::new_int(*v as i64))?;
    }
    tu::dict_set(&d, "CHAR_MAX", &int::new_int(127))?;
    let base = crate::types::typeobject::type_ref(crate::types::exceptions::Exception());
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str("locale"))?;
    let err = crate::abstract_::call(
        crate::types::typeobject::type_obj(crate::types::typeobject::type_type()),
        &[&pystr::new_str("Error"), &crate::types::tuple::new_tuple(vec![base]), &ns],
    )?;
    ERROR.store(err.clone().into_raw(), Ordering::Release);
    tu::dict_set(&d, "Error", &err)?;
    Ok(m)
}
