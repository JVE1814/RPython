//! The `_stat` accelerator module (constants and S_IS* helpers).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
use crate::types::{bool_, dict, int, module, str as pystr};

const S_IFMT: u64 = 0o170000;
const S_IFDIR: u64 = 0o040000;
const S_IFCHR: u64 = 0o020000;
const S_IFBLK: u64 = 0o060000;
const S_IFREG: u64 = 0o100000;
const S_IFIFO: u64 = 0o010000;
const S_IFLNK: u64 = 0o120000;
const S_IFSOCK: u64 = 0o140000;
const S_IFWHT: u64 = 0o160000;

fn mode_arg(o: &PyObject) -> PyResult<u64> {
    let i = ab::index(o)?;
    match int::as_i128(&i) {
        Some(v) if v >= 0 && v <= u32::MAX as i128 => Ok(v as u64),
        Some(v) if v < 0 => raise!(OverflowError, "mode out of range"),
        _ => raise!(OverflowError, "mode out of range"),
    }
}

macro_rules! isfn {
    ($name:ident, $fmt:expr) => {
        fn $name(_m: &PyObject, o: &PyObject) -> PyResult {
            let m = mode_arg(o)?;
            Ok(bool_::new_bool(m & S_IFMT == $fmt))
        }
    };
}
isfn!(s_isdir, S_IFDIR);
isfn!(s_ischr, S_IFCHR);
isfn!(s_isblk, S_IFBLK);
isfn!(s_isreg, S_IFREG);
isfn!(s_isfifo, S_IFIFO);
isfn!(s_islnk, S_IFLNK);
isfn!(s_issock, S_IFSOCK);

fn s_false(_m: &PyObject, o: &PyObject) -> PyResult {
    mode_arg(o)?;
    Ok(bool_::new_bool(false))
}

fn s_imode(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int((mode_arg(o)? & 0o7777) as i64))
}
fn s_ifmt(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int((mode_arg(o)? & S_IFMT) as i64))
}

pub fn filemode_str(m: u64) -> String {
    let mut s = String::with_capacity(10);
    s.push(match m & S_IFMT {
        S_IFLNK => 'l',
        S_IFSOCK => 's',
        S_IFREG => '-',
        S_IFBLK => 'b',
        S_IFDIR => 'd',
        S_IFCHR => 'c',
        S_IFIFO => 'p',
        _ => '?',
    });
    let bit = |b: u64, c: char| if m & b != 0 { c } else { '-' };
    s.push(bit(0o400, 'r'));
    s.push(bit(0o200, 'w'));
    s.push(match (m & 0o100 != 0, m & 0o4000 != 0) {
        (true, true) => 's',
        (false, true) => 'S',
        (true, false) => 'x',
        _ => '-',
    });
    s.push(bit(0o40, 'r'));
    s.push(bit(0o20, 'w'));
    s.push(match (m & 0o10 != 0, m & 0o2000 != 0) {
        (true, true) => 's',
        (false, true) => 'S',
        (true, false) => 'x',
        _ => '-',
    });
    s.push(bit(0o4, 'r'));
    s.push(bit(0o2, 'w'));
    s.push(match (m & 0o1 != 0, m & 0o1000 != 0) {
        (true, true) => 't',
        (false, true) => 'T',
        (true, false) => 'x',
        _ => '-',
    });
    s
}

fn filemode(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&filemode_str(mode_arg(o)?)))
}

pub fn init() -> PyResult {
    let m = module::new_module("_stat")?;
    let d = module::module_dict(&m);
    let methods = crate::mdefs![
        ("S_ISDIR", O, s_isdir),
        ("S_ISCHR", O, s_ischr),
        ("S_ISBLK", O, s_isblk),
        ("S_ISREG", O, s_isreg),
        ("S_ISFIFO", O, s_isfifo),
        ("S_ISLNK", O, s_islnk),
        ("S_ISSOCK", O, s_issock),
        ("S_ISDOOR", O, s_false),
        ("S_ISPORT", O, s_false),
        ("S_ISWHT", O, s_false),
        ("S_IMODE", O, s_imode),
        ("S_IFMT", O, s_ifmt),
        ("filemode", O, filemode),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    let consts: &[(&str, i64)] = &[
        ("S_IFDIR", S_IFDIR as i64),
        ("S_IFCHR", S_IFCHR as i64),
        ("S_IFBLK", S_IFBLK as i64),
        ("S_IFREG", S_IFREG as i64),
        ("S_IFIFO", S_IFIFO as i64),
        ("S_IFLNK", S_IFLNK as i64),
        ("S_IFSOCK", S_IFSOCK as i64),
        ("S_IFDOOR", 0),
        ("S_IFPORT", 0),
        ("S_IFWHT", S_IFWHT as i64),
        ("S_ISUID", 0o4000),
        ("S_ISGID", 0o2000),
        ("S_ISVTX", 0o1000),
        ("S_ENFMT", 0o2000),
        ("S_IREAD", 0o400),
        ("S_IWRITE", 0o200),
        ("S_IEXEC", 0o100),
        ("S_IRWXU", 0o700),
        ("S_IRUSR", 0o400),
        ("S_IWUSR", 0o200),
        ("S_IXUSR", 0o100),
        ("S_IRWXG", 0o70),
        ("S_IRGRP", 0o40),
        ("S_IWGRP", 0o20),
        ("S_IXGRP", 0o10),
        ("S_IRWXO", 0o7),
        ("S_IROTH", 0o4),
        ("S_IWOTH", 0o2),
        ("S_IXOTH", 0o1),
        ("UF_SETTABLE", 0x0000ffff),
        ("UF_NODUMP", 0x00000001),
        ("UF_IMMUTABLE", 0x00000002),
        ("UF_APPEND", 0x00000004),
        ("UF_OPAQUE", 0x00000008),
        ("UF_NOUNLINK", 0x00000010),
        ("UF_COMPRESSED", 0x00000020),
        ("UF_TRACKED", 0x00000040),
        ("UF_DATAVAULT", 0x00000080),
        ("UF_HIDDEN", 0x00008000),
        ("SF_ARCHIVED", 0x00010000),
        ("SF_IMMUTABLE", 0x00020000),
        ("SF_APPEND", 0x00040000),
        ("SF_RESTRICTED", 0x00080000),
        ("SF_NOUNLINK", 0x00100000),
        ("SF_SNAPSHOT", 0x00200000),
        ("SF_FIRMLINK", 0x00800000),
        ("SF_DATALESS", 0x40000000),
        ("ST_MODE", 0),
        ("ST_INO", 1),
        ("ST_DEV", 2),
        ("ST_NLINK", 3),
        ("ST_UID", 4),
        ("ST_GID", 5),
        ("ST_SIZE", 6),
        ("ST_ATIME", 7),
        ("ST_MTIME", 8),
        ("ST_CTIME", 9),
        ("FILE_ATTRIBUTE_ARCHIVE", 32),
        ("FILE_ATTRIBUTE_COMPRESSED", 2048),
        ("FILE_ATTRIBUTE_DEVICE", 64),
        ("FILE_ATTRIBUTE_DIRECTORY", 16),
        ("FILE_ATTRIBUTE_ENCRYPTED", 16384),
        ("FILE_ATTRIBUTE_HIDDEN", 2),
        ("FILE_ATTRIBUTE_INTEGRITY_STREAM", 32768),
        ("FILE_ATTRIBUTE_NORMAL", 128),
        ("FILE_ATTRIBUTE_NOT_CONTENT_INDEXED", 8192),
        ("FILE_ATTRIBUTE_NO_SCRUB_DATA", 131072),
        ("FILE_ATTRIBUTE_OFFLINE", 4096),
        ("FILE_ATTRIBUTE_READONLY", 1),
        ("FILE_ATTRIBUTE_REPARSE_POINT", 1024),
        ("FILE_ATTRIBUTE_SPARSE_FILE", 512),
        ("FILE_ATTRIBUTE_SYSTEM", 4),
        ("FILE_ATTRIBUTE_TEMPORARY", 256),
        ("FILE_ATTRIBUTE_VIRTUAL", 65536),
    ];
    for (n, v) in consts {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    // the super-user flag masks are platform specific (macOS reserves the top two
    // bits for flags the kernel synthesises)
    let sf: &[(&str, i64)] = if cfg!(target_os = "macos") {
        &[("SF_SETTABLE", 0x3fff0000), ("SF_SUPPORTED", 0x009f0000), ("SF_SYNTHETIC", 0xc0000000)]
    } else {
        &[("SF_SETTABLE", 0xffff0000)]
    };
    for (n, v) in sf {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    Ok(m)
}
