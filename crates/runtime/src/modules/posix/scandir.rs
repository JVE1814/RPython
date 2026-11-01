//! os.scandir(): ScandirIterator and DirEntry.
use super::kwparse;
use super::path::*;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, bytes, int, none, str as pystr};
use std::ffi::{CString, c_int};

#[repr(C)]
pub struct ScandirIter {
    ob_base: PyObject,
    dir: *mut libc::DIR,
    /// Directory path as given (bytes, for joining), unless fd mode.
    prefix: *mut Vec<u8>,
    narrow: bool,
    fd: c_int, // -1 unless scanning an fd
}

#[repr(C)]
pub struct DirEntry {
    ob_base: PyObject,
    name: *mut PyObject,
    path: *mut PyObject,
    stat: *mut PyObject,
    lstat: *mut PyObject,
    /// Full path, encoded (for stat calls) or just the name in fd mode.
    cpath: *mut CString,
    d_type: u8,
    d_ino: u64,
    dir_fd: c_int,
}

pub static mut SCANDIR_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut DIRENTRY_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn it(o: &PyObject) -> &mut ScandirIter {
    unsafe { &mut *(o.as_ptr() as *mut ScandirIter) }
}
fn de(o: &PyObject) -> &mut DirEntry {
    unsafe { &mut *(o.as_ptr() as *mut DirEntry) }
}

fn close_iter(s: &mut ScandirIter) {
    if !s.dir.is_null() {
        unsafe {
            if s.fd >= 0 {
                libc::rewinddir(s.dir);
            }
            libc::closedir(s.dir);
        }
        s.dir = std::ptr::null_mut();
    }
}

pub fn os_scandir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("scandir", &["path"], 0, 0, 0, args, kw)?;
    let p = match path {
        None => PathArg::none(),
        Some(o) => path_converter(&PathSpec { func: "scandir", arg: "path", allow_fd: true, nullable: true }, o)?,
    };
    unsafe {
        let (dir, fd) = match p.fd {
            Some(fd) => {
                let fd2 = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0);
                if fd2 < 0 {
                    return Err(os_err_with(&p));
                }
                let d = libc::fdopendir(fd2);
                if d.is_null() {
                    let e = errno();
                    libc::close(fd2);
                    return Err(p.err(e));
                }
                (d, fd)
            }
            None => (libc::opendir(p.ptr()), -1),
        };
        if dir.is_null() {
            return Err(os_err_with(&p));
        }
        let op = crate::alloc::alloc_object(&raw mut SCANDIR_ITER_TYPE, std::mem::size_of::<ScandirIter>());
        let s = &mut *(op as *mut ScandirIter);
        s.dir = dir;
        s.prefix = Box::into_raw(Box::new(p.bytes().to_vec()));
        s.narrow = p.narrow;
        s.fd = fd;
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn iter_next(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &mut *(op as *mut ScandirIter);
        if s.dir.is_null() {
            return std::ptr::null_mut();
        }
        loop {
            *libc::__errno_location() = 0;
            let ent = libc::readdir64(s.dir);
            if ent.is_null() {
                let e = errno();
                close_iter(s);
                if e != 0 {
                    let pathobj = if s.narrow {
                        bytes::new_bytes(&*s.prefix)
                    } else {
                        fs_decode(&*s.prefix).unwrap_or_else(|_| none::none_ref())
                    };
                    let _ = crate::types::exceptions::os_error_from_errno(e, Some(&pathobj), None);
                }
                return std::ptr::null_mut();
            }
            let name = std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let r = make_entry(s, name, (*ent).d_type, (*ent).d_ino);
            return into_c(r);
        }
    }
}

fn make_entry(s: &ScandirIter, name: &[u8], d_type: u8, d_ino: u64) -> PyResult {
    let (nameobj, pathobj, cpath) = unsafe {
        if s.fd >= 0 {
            let n = if s.narrow { bytes::new_bytes(name) } else { fs_decode(name)? };
            (n.clone(), n, CString::new(name.to_vec()).unwrap())
        } else {
            let prefix = &*s.prefix;
            let mut full = prefix.clone();
            if !full.is_empty() && *full.last().unwrap() != b'/' {
                full.push(b'/');
            }
            full.extend_from_slice(name);
            let (n, p) = if s.narrow {
                (bytes::new_bytes(name), bytes::new_bytes(&full))
            } else {
                (fs_decode(name)?, fs_decode(&full)?)
            };
            (n, p, CString::new(full).unwrap())
        }
    };
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut DIRENTRY_TYPE, std::mem::size_of::<DirEntry>());
        let e = &mut *(op as *mut DirEntry);
        e.name = nameobj.into_raw();
        e.path = pathobj.into_raw();
        e.stat = std::ptr::null_mut();
        e.lstat = std::ptr::null_mut();
        e.cpath = Box::into_raw(Box::new(cpath));
        e.d_type = d_type;
        e.d_ino = d_ino;
        e.dir_fd = s.fd;
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn iter_dealloc(op: *mut PyObject) {
    unsafe {
        let s = &mut *(op as *mut ScandirIter);
        if !s.dir.is_null() {
            // ResourceWarning: unclosed scandir iterator
            let saved = take_exc();
            let msg = format!("unclosed scandir iterator {}", crate::abstract_::repr_string(&*op).unwrap_or_default());
            if crate::modules::warnings::warn_with_source(crate::types::exceptions::ResourceWarning(), &msg, 1, Some(&*op)).is_err() {
                crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing scandir iterator", Some(&*op));
            }
            if let Some(e) = saved {
                set_exc(e);
            }
            close_iter(s);
        }
        if !s.prefix.is_null() {
            drop(Box::from_raw(s.prefix));
        }
        crate::alloc::free_object(op);
    }
}

fn iter_close(o: &PyObject) -> PyResult {
    close_iter(it(o));
    Ok(none::none_ref())
}
fn iter_enter(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn iter_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    close_iter(it(o));
    Ok(none::none_ref())
}
fn iter_reduce(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", crate::types::typeobject::tname(o));
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

// ------------------------------------------------------------------ DirEntry

unsafe extern "C" fn entry_dealloc(op: *mut PyObject) {
    unsafe {
        let e = &mut *(op as *mut DirEntry);
        xdecref(e.name);
        xdecref(e.path);
        xdecref(e.stat);
        xdecref(e.lstat);
        if !e.cpath.is_null() {
            drop(Box::from_raw(e.cpath));
        }
        crate::alloc::free_object(op);
    }
}

fn get_name(o: &PyObject) -> PyResult {
    Ok(unsafe { (*de(o).name).to_ref() })
}
fn get_path(o: &PyObject) -> PyResult {
    Ok(unsafe { (*de(o).path).to_ref() })
}

fn entry_fetch_stat(e: &mut DirEntry, follow: bool) -> PyResult {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let cpath = unsafe { &*e.cpath };
    let r = unsafe {
        if e.dir_fd >= 0 {
            libc::fstatat(e.dir_fd, cpath.as_ptr(), &mut st, if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW })
        } else if follow {
            libc::stat(cpath.as_ptr(), &mut st)
        } else {
            libc::lstat(cpath.as_ptr(), &mut st)
        }
    };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(errno(), Some(unsafe { &*e.path }), None));
    }
    Ok(super::stat::stat_result_from(&st))
}

fn entry_lstat(e: &mut DirEntry) -> PyResult {
    if e.lstat.is_null() {
        let r = entry_fetch_stat(e, false)?;
        e.lstat = r.into_raw();
    }
    Ok(unsafe { (*e.lstat).to_ref() })
}

fn entry_is_symlink_impl(e: &mut DirEntry) -> PyResult<bool> {
    if e.d_type != libc::DT_UNKNOWN {
        return Ok(e.d_type == libc::DT_LNK);
    }
    let st = entry_lstat(e)?;
    Ok(st_mode(&st)? & libc::S_IFMT == libc::S_IFLNK)
}

fn st_mode(st: &PyObject) -> PyResult<u32> {
    Ok(crate::abstract_::as_i64(crate::types::structseq::get_item(st, 0))? as u32)
}

fn entry_stat_impl(e: &mut DirEntry, follow: bool) -> PyResult {
    if follow {
        if e.stat.is_null() {
            if entry_is_symlink_impl(e)? {
                let r = entry_fetch_stat(e, true)?;
                e.stat = r.into_raw();
            } else {
                let r = entry_lstat(e)?;
                e.stat = r.into_raw();
            }
        }
        Ok(unsafe { (*e.stat).to_ref() })
    } else {
        entry_lstat(e)
    }
}

fn follow_arg(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<bool> {
    let [f] = kwparse::<1>(fname, &["follow_symlinks"], 0, 0, 1, args, kw)?;
    truthy(f, true)
}

fn entry_stat(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let follow = follow_arg("stat", args, kw)?;
    entry_stat_impl(de(o), follow)
}

fn test_mode(e: &mut DirEntry, follow: bool, mode_bits: u32) -> PyResult<bool> {
    let need_stat = e.d_type == libc::DT_UNKNOWN || (follow && e.d_type == libc::DT_LNK);
    if need_stat {
        match entry_stat_impl(e, follow) {
            Ok(st) => Ok(st_mode(&st)? & libc::S_IFMT == mode_bits),
            Err(err) => {
                if exc_matches(crate::types::exceptions::FileNotFoundError()) {
                    clear_exc();
                    Ok(false)
                } else {
                    Err(err)
                }
            }
        }
    } else if mode_bits == libc::S_IFDIR {
        Ok(e.d_type == libc::DT_DIR)
    } else {
        Ok(e.d_type == libc::DT_REG)
    }
}

fn entry_is_dir(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let follow = follow_arg("is_dir", args, kw)?;
    Ok(bool_::new_bool(test_mode(de(o), follow, libc::S_IFDIR)?))
}
fn entry_is_file(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let follow = follow_arg("is_file", args, kw)?;
    Ok(bool_::new_bool(test_mode(de(o), follow, libc::S_IFREG)?))
}
fn entry_is_symlink(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(entry_is_symlink_impl(de(o))?))
}
fn entry_is_junction(_o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}
fn entry_inode(o: &PyObject) -> PyResult {
    Ok(int::new_int_u128(de(o).d_ino as u128))
}
fn entry_fspath(o: &PyObject) -> PyResult {
    get_path(o)
}
unsafe extern "C" fn entry_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let n = crate::abstract_::repr_string(&*de(&*op).name)?;
            Ok(pystr::new_str(&format!("<DirEntry {}>", n)))
        })();
        into_c(r)
    }
}
fn entry_reduce(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", crate::types::typeobject::tname(o));
}

pub fn class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    let types = crate::import::import_absolute("types")?;
    let ga = crate::abstract_::getattr_str(&types, "GenericAlias")?;
    crate::abstract_::call(&ga, &[cls, item])
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = &mut *(&raw mut SCANDIR_ITER_TYPE);
        if t.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        t.tp_name = c"posix.ScandirIterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ScandirIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(iter_dealloc);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(iter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("close", NOARGS, iter_close),
            ("__enter__", NOARGS, iter_enter),
            ("__exit__", FAST, iter_exit, "__exit__($self, /, *args)\n--\n\nClose the iterator when leaving a with block."),
            ("__reduce__", NOARGS, iter_reduce),
        ];
        crate::types::typeobject::type_ready(&raw mut SCANDIR_ITER_TYPE)?;

        let t = &mut *(&raw mut DIRENTRY_TYPE);
        t.tp_name = c"posix.DirEntry".as_ptr();
        t.tp_basicsize = std::mem::size_of::<DirEntry>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(entry_dealloc);
        t.tp_repr = Some(entry_repr);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_getset = crate::getset_table![("name", get_name; "the entry's base filename, relative to scandir() \"path\" argument"), ("path", get_path; "the entry's full path name; equivalent to os.path.join(scandir_path, entry.name)")];
        t.tp_methods = crate::mdefs![
            ("stat", FASTKW, entry_stat, "stat($self, /, *, follow_symlinks=True)\n--\n\nReturn the stat_result of the entry, cached after the first call."),
            ("is_dir", FASTKW, entry_is_dir, "is_dir($self, /, *, follow_symlinks=True)\n--\n\nReturn True if the entry is a directory."),
            ("is_file", FASTKW, entry_is_file, "is_file($self, /, *, follow_symlinks=True)\n--\n\nReturn True if the entry is a regular file."),
            ("is_symlink", NOARGS, entry_is_symlink),
            ("is_junction", NOARGS, entry_is_junction),
            ("inode", NOARGS, entry_inode),
            ("__fspath__", NOARGS, entry_fspath),
            ("__reduce__", NOARGS, entry_reduce),
            ("__class_getitem__", O, class_getitem; METH_CLASS),
        ];
        crate::types::typeobject::type_ready(&raw mut DIRENTRY_TYPE)?;
    }
    Ok(())
}
