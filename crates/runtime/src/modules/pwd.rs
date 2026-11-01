//! The `pwd` and `grp` modules (password / group database).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::posix::path::{fs_decode, fs_encode_str};
use crate::object::*;
use crate::types::structseq::{self, Desc, Field};
use crate::types::typeobject::{tname, type_ref};
use crate::types::{dict, int, list, none, str as pystr};
use crate::{mdefs, raise};
use std::ffi::{CStr, c_char};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

static PASSWD_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static GROUP_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
/// getpwent()/getgrent() iterate global state.
static ENT_LOCK: Mutex<()> = Mutex::new(());

fn cstr_obj(p: *const c_char) -> PyResult {
    if p.is_null() {
        return Ok(none::none_ref());
    }
    fs_decode(unsafe { CStr::from_ptr(p) }.to_bytes())
}

fn id_obj(v: u32) -> PyRef {
    crate::modules::posix::path::id_result(v)
}

/// Encode a str argument with the filesystem encoding; rejects embedded NULs.
fn name_arg(fname: &str, o: &PyObject) -> PyResult<std::ffi::CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument must be str, not {}", fname, tname(o));
    }
    let b = fs_encode_str(o)?;
    if b.contains(&0) {
        raise!(ValueError, "embedded null character");
    }
    Ok(std::ffi::CString::new(b).unwrap())
}

/// uid_t/gid_t conversion as _Py_Uid_Converter does.
fn id_arg(o: &PyObject, what: &str) -> PyResult<u32> {
    if crate::types::float::is_float(o) || !ab::has_index(o) {
        raise!(TypeError, "{} should be integer, not {}", what, tname(o));
    }
    let i = ab::index(o)?;
    match crate::modules::devtools::int_as_i128(&i) {
        Some(-1) => Ok(u32::MAX),
        Some(v) if (0..u32::MAX as i128).contains(&v) => Ok(v as u32),
        Some(v) if v < 0 => raise!(OverflowError, "{} is less than minimum", what),
        _ => raise!(OverflowError, "{} is greater than maximum", what),
    }
}

// ------------------------------------------------------------------ pwd

unsafe fn mkpwent(p: *const libc::passwd) -> PyResult {
    unsafe {
        let p = &*p;
        let vals = vec![
            cstr_obj(p.pw_name)?,
            cstr_obj(p.pw_passwd)?,
            id_obj(p.pw_uid),
            id_obj(p.pw_gid),
            cstr_obj(p.pw_gecos)?,
            cstr_obj(p.pw_dir)?,
            cstr_obj(p.pw_shell)?,
        ];
        Ok(structseq::new_instance(PASSWD_TYPE.load(Ordering::Acquire), vals))
    }
}

fn bufsize(name: libc::c_int) -> usize {
    let n = unsafe { libc::sysconf(name) };
    if n <= 0 { 1024 } else { n as usize }
}

/// Call a reentrant getpw*_r/getgr*_r style function, growing the buffer on ERANGE.
fn with_r<T>(initial: usize, mut f: impl FnMut(&mut T, &mut [c_char], &mut *mut T) -> libc::c_int, conv: impl FnOnce(*const T) -> PyResult) -> PyResult<Option<PyRef>> {
    let mut size = initial;
    loop {
        let mut ent: T = unsafe { std::mem::zeroed() };
        let mut buf = vec![0 as c_char; size];
        let mut result: *mut T = std::ptr::null_mut();
        let rc = crate::threads::blocking(|| f(&mut ent, &mut buf, &mut result));
        if rc == libc::ERANGE && size < (1 << 26) {
            size *= 2;
            continue;
        }
        if result.is_null() {
            return Ok(None);
        }
        return conv(result).map(Some);
    }
}

fn pwd_getpwuid(_m: &PyObject, uidobj: &PyObject) -> PyResult {
    let uid = match id_arg(uidobj, "uid") {
        Ok(u) => u,
        Err(e) => {
            if exc_matches(crate::types::exceptions::OverflowError()) {
                clear_exc();
                raise!(KeyError, "getpwuid(): uid not found");
            }
            return Err(e);
        }
    };
    let r = with_r::<libc::passwd>(
        bufsize(libc::_SC_GETPW_R_SIZE_MAX),
        |ent, buf, res| unsafe { libc::getpwuid_r(uid, ent, buf.as_mut_ptr(), buf.len(), res) },
        |p| unsafe { mkpwent(p) },
    )?;
    match r {
        Some(v) => Ok(v),
        None => {
            let rep = ab::repr_string(uidobj)?;
            Err(key_error_msg(&format!("getpwuid(): uid not found: {}", rep)))
        }
    }
}

fn key_error_msg(msg: &str) -> PyErr {
    crate::errors::new_err(crate::types::exceptions::KeyError(), msg)
}

fn pwd_getpwnam(_m: &PyObject, name: &PyObject) -> PyResult {
    let c = name_arg("getpwnam", name)?;
    let r = with_r::<libc::passwd>(
        bufsize(libc::_SC_GETPW_R_SIZE_MAX),
        |ent, buf, res| unsafe { libc::getpwnam_r(c.as_ptr(), ent, buf.as_mut_ptr(), buf.len(), res) },
        |p| unsafe { mkpwent(p) },
    )?;
    match r {
        Some(v) => Ok(v),
        None => {
            let rep = ab::repr_string(name)?;
            Err(key_error_msg(&format!("getpwnam(): name not found: {}", rep)))
        }
    }
}

fn pwd_getpwall(_m: &PyObject) -> PyResult {
    let _g = ENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = list::new_list_empty();
    unsafe {
        libc::setpwent();
        loop {
            let p = libc::getpwent();
            if p.is_null() {
                break;
            }
            match mkpwent(p) {
                Ok(v) => list::append(&out, v),
                Err(e) => {
                    libc::endpwent();
                    return Err(e);
                }
            }
        }
        libc::endpwent();
    }
    Ok(out)
}

static PASSWD_FIELDS: &[Field] = &[
    Field::new("pw_name", "user name"),
    Field::new("pw_passwd", "password"),
    Field::new("pw_uid", "user id"),
    Field::new("pw_gid", "group id"),
    Field::new("pw_gecos", "real name"),
    Field::new("pw_dir", "home directory"),
    Field::new("pw_shell", "shell program"),
];

pub fn init_pwd() -> PyResult {
    if PASSWD_TYPE.load(Ordering::Acquire).is_null() {
        let t = structseq::new_type(&Desc {
            name: "pwd.struct_passwd",
            doc: "pwd.struct_passwd: Results from getpw*() routines.\n\nThis object may be accessed either as a tuple of\n  (pw_name,pw_passwd,pw_uid,pw_gid,pw_gecos,pw_dir,pw_shell)\nor via the object attributes as named in the above tuple.",
            fields: PASSWD_FIELDS,
            n_in_sequence: 7,
        })?;
        PASSWD_TYPE.store(t, Ordering::Release);
    }
    let methods = mdefs![
        ("getpwuid", O, pwd_getpwuid, "getpwuid($module, uidobj, /)\n--\n\nReturn the password database entry for the given numeric user ID.\n\nSee `help(pwd)` for more on password database entries."),
        ("getpwnam", O, pwd_getpwnam, "getpwnam($module, name, /)\n--\n\nReturn the password database entry for the given user name.\n\nSee `help(pwd)` for more on password database entries."),
        ("getpwall", NOARGS, pwd_getpwall, "getpwall($module, /)\n--\n\nReturn a list of all available password database entries, in arbitrary order.\n\nSee help(pwd) for more on password database entries."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("pwd", methods, Some("This module provides access to the Unix password database.\nIt is available on all Unix versions.\n\nPassword database entries are reported as 7-tuples containing the following\nitems from the password database (see `<pwd.h>'), in order:\npw_name, pw_passwd, pw_uid, pw_gid, pw_gecos, pw_dir, pw_shell.\nThe uid and gid items are integers, all others are strings. An\nexception is raised if the entry asked for cannot be found."))?;
    dict::set_item_str(&d, "struct_passwd", &type_ref(PASSWD_TYPE.load(Ordering::Acquire)))?;
    Ok(m)
}

// ------------------------------------------------------------------ grp

unsafe fn mkgrent(p: *const libc::group) -> PyResult {
    unsafe {
        let g = &*p;
        let mem = list::new_list_empty();
        if !g.gr_mem.is_null() {
            let mut q = g.gr_mem;
            while !(*q).is_null() {
                list::append(&mem, cstr_obj(*q)?);
                q = q.add(1);
            }
        }
        let vals = vec![cstr_obj(g.gr_name)?, cstr_obj(g.gr_passwd)?, id_obj(g.gr_gid), mem];
        Ok(structseq::new_instance(GROUP_TYPE.load(Ordering::Acquire), vals))
    }
}

fn grp_getgrgid(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("getgrgid", &["id"], 0, 1);
    let [id] = crate::args::parse::<1>(&P, args, kw)?;
    let idobj = id.unwrap();
    let gid = id_arg(idobj, "gid")?;
    let r = with_r::<libc::group>(
        bufsize(libc::_SC_GETGR_R_SIZE_MAX),
        |ent, buf, res| unsafe { libc::getgrgid_r(gid, ent, buf.as_mut_ptr(), buf.len(), res) },
        |p| unsafe { mkgrent(p) },
    )?;
    match r {
        Some(v) => Ok(v),
        None => {
            let rep = ab::repr_string(idobj)?;
            Err(key_error_msg(&format!("getgrgid(): gid not found: {}", rep)))
        }
    }
}

fn grp_getgrnam(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("getgrnam", &["name"], 0, 1);
    let [name] = crate::args::parse::<1>(&P, args, kw)?;
    let name = name.unwrap();
    let c = name_arg("getgrnam", name)?;
    let r = with_r::<libc::group>(
        bufsize(libc::_SC_GETGR_R_SIZE_MAX),
        |ent, buf, res| unsafe { libc::getgrnam_r(c.as_ptr(), ent, buf.as_mut_ptr(), buf.len(), res) },
        |p| unsafe { mkgrent(p) },
    )?;
    match r {
        Some(v) => Ok(v),
        None => {
            let rep = ab::repr_string(name)?;
            Err(key_error_msg(&format!("getgrnam(): name not found: {}", rep)))
        }
    }
}

fn grp_getgrall(_m: &PyObject) -> PyResult {
    let _g = ENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = list::new_list_empty();
    unsafe {
        libc::setgrent();
        loop {
            let p = libc::getgrent();
            if p.is_null() {
                break;
            }
            match mkgrent(p) {
                Ok(v) => list::append(&out, v),
                Err(e) => {
                    libc::endgrent();
                    return Err(e);
                }
            }
        }
        libc::endgrent();
    }
    Ok(out)
}

static GROUP_FIELDS: &[Field] = &[
    Field::new("gr_name", "group name"),
    Field::new("gr_passwd", "password"),
    Field::new("gr_gid", "group id"),
    Field::new("gr_mem", "group members"),
];

pub fn init_grp() -> PyResult {
    if GROUP_TYPE.load(Ordering::Acquire).is_null() {
        let t = structseq::new_type(&Desc {
            name: "grp.struct_group",
            doc: "grp.struct_group: Results from getgr*() routines.\n\nThis object may be accessed either as a tuple of\n  (gr_name,gr_passwd,gr_gid,gr_mem)\nor via the object attributes as named in the above tuple.\n",
            fields: GROUP_FIELDS,
            n_in_sequence: 4,
        })?;
        GROUP_TYPE.store(t, Ordering::Release);
    }
    let methods = mdefs![
        ("getgrgid", FASTKW, grp_getgrgid, "getgrgid($module, /, id)\n--\n\nReturn the group database entry for the given numeric group ID.\n\nIf id is not valid, raise KeyError."),
        ("getgrnam", FASTKW, grp_getgrnam, "getgrnam($module, /, name)\n--\n\nReturn the group database entry for the given group name.\n\nIf name is not valid, raise KeyError."),
        ("getgrall", NOARGS, grp_getgrall, "getgrall($module, /)\n--\n\nReturn a list of all available group entries, in arbitrary order.\n\nAn entry whose name starts with '+' or '-' represents an instruction\nto use YP/NIS and may not be accessible via getgrnam or getgrgid."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("grp", methods, Some("Access to the Unix group database.\n\nGroup entries are reported as 4-tuples containing the following fields\nfrom the group database, in order:\n\n  gr_name   - name of the group\n  gr_passwd - group password (encrypted); often empty\n  gr_gid    - numeric ID of the group\n  gr_mem    - list of members\n\nThe gid is an integer, name and password are strings.  (Note that most\nusers are not explicitly listed as members of the groups they are in\naccording to the password database.  Check both databases to get\ncomplete membership information.)"))?;
    dict::set_item_str(&d, "struct_group", &type_ref(GROUP_TYPE.load(Ordering::Acquire)))?;
    Ok(m)
}
