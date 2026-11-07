//! The `termios` module (Linux).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::posix::path::{as_file_descriptor, c_int, errno};
use crate::object::*;
use crate::types::{bytes, dict, int, list, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::sync::atomic::{AtomicPtr, Ordering};

static ERROR: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter

const NCCS: usize = 32;
const ICANON: libc::tcflag_t = 0o2;
const VTIME: usize = 5;
const VMIN: usize = 6;

/// Raise termios.error(errno, strerror).
fn term_err() -> PyErr {
    let e = errno();
    let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(e)).to_string_lossy().into_owned() };
    let cls = ERROR.load(Ordering::Acquire);
    if cls.is_null() {
        return crate::types::exceptions::os_error_from_errno(e, None, None);
    }
    match ab::call(unsafe { &*cls }, &[&int::new_int(e as i64), &pystr::new_str(&msg)]) {
        Ok(exc) => {
            set_exc(exc);
            PyErr
        }
        Err(e) => e,
    }
}

fn long_arg(o: &PyObject) -> PyResult<i64> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

fn getattr_raw(fd: i32) -> PyResult<libc::termios> {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    let r = crate::threads::blocking(|| unsafe { libc::tcgetattr(fd, &mut t) });
    if r == -1 {
        return Err(term_err());
    }
    Ok(t)
}

fn t_tcgetattr(_m: &PyObject, fdo: &PyObject) -> PyResult {
    let fd = as_file_descriptor(fdo)?;
    let t = getattr_raw(fd)?;
    let ispeed = unsafe { libc::cfgetispeed(&t) };
    let ospeed = unsafe { libc::cfgetospeed(&t) };
    let mut cc = Vec::with_capacity(NCCS);
    for i in 0..NCCS {
        let c = t.c_cc[i];
        if t.c_lflag & ICANON == 0 && (i == VMIN || i == VTIME) {
            cc.push(int::new_int(c as i64));
        } else {
            cc.push(bytes::new_bytes(&[c]));
        }
    }
    Ok(list::new_list(vec![
        int::new_int(t.c_iflag as i64),
        int::new_int(t.c_oflag as i64),
        int::new_int(t.c_cflag as i64),
        int::new_int(t.c_lflag as i64),
        int::new_int(ispeed as i64),
        int::new_int(ospeed as i64),
        list::new_list(cc),
    ]))
}

fn t_tcsetattr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tcsetattr", args, 3, 3)?;
    let fd = as_file_descriptor(args[0])?;
    let when = c_int(args[1])?;
    let term = args[2];
    if !list::is_list(term) || list::list_len(term) != 7 {
        raise!(TypeError, "tcsetattr, arg 3: must be 7 element list");
    }
    let mut t = getattr_raw(fd)?;
    let items = list::list_to_vec(term);
    let flag = |o: &PyObject| -> PyResult<libc::tcflag_t> {
        let v = long_arg(o)?;
        Ok(v as libc::tcflag_t)
    };
    t.c_iflag = flag(&items[0])?;
    t.c_oflag = flag(&items[1])?;
    t.c_cflag = flag(&items[2])?;
    t.c_lflag = flag(&items[3])?;
    let ispeed = long_arg(&items[4])?;
    let ospeed = long_arg(&items[5])?;
    let cc = &items[6];
    if !list::is_list(cc) || list::list_len(cc) != NCCS {
        raise!(TypeError, "tcsetattr: attributes[6] must be {} element list", NCCS);
    }
    for (i, v) in list::list_to_vec(cc).iter().enumerate() {
        if bytes::is_bytes(v) && bytes::bytes_len(v) == 1 {
            t.c_cc[i] = bytes::bytes_data(v)[0];
        } else if int::is_int(v) {
            t.c_cc[i] = long_arg(v)? as libc::cc_t;
        } else {
            raise!(TypeError, "tcsetattr: elements of attributes must be characters or integers");
        }
    }
    unsafe {
        if libc::cfsetispeed(&mut t, ispeed as libc::speed_t) == -1 {
            return Err(term_err());
        }
        if libc::cfsetospeed(&mut t, ospeed as libc::speed_t) == -1 {
            return Err(term_err());
        }
    }
    let r = crate::threads::blocking(|| unsafe { libc::tcsetattr(fd, when, &t) });
    if r == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

fn two_int(fname: &str, args: &[&PyObject]) -> PyResult<(i32, i32)> {
    crate::args::check_pos(fname, args, 2, 2)?;
    let fd = as_file_descriptor(args[0])?;
    let v = c_int(args[1])?;
    Ok((fd, v))
}

fn t_tcsendbreak(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (fd, d) = two_int("tcsendbreak", args)?;
    if crate::threads::blocking(|| unsafe { libc::tcsendbreak(fd, d) }) == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

fn t_tcdrain(_m: &PyObject, fdo: &PyObject) -> PyResult {
    let fd = as_file_descriptor(fdo)?;
    if crate::threads::blocking(|| unsafe { libc::tcdrain(fd) }) == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

fn t_tcflush(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (fd, q) = two_int("tcflush", args)?;
    if crate::threads::blocking(|| unsafe { libc::tcflush(fd, q) }) == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

fn t_tcflow(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (fd, a) = two_int("tcflow", args)?;
    if crate::threads::blocking(|| unsafe { libc::tcflow(fd, a) }) == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

fn t_tcgetwinsize(_m: &PyObject, fdo: &PyObject) -> PyResult {
    let fd = as_file_descriptor(fdo)?;
    let mut w: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut w) } == -1 {
        return Err(term_err());
    }
    Ok(tuple::new_tuple(vec![int::new_int(w.ws_row as i64), int::new_int(w.ws_col as i64)]))
}

fn t_tcsetwinsize(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tcsetwinsize", args, 2, 2)?;
    let fd = as_file_descriptor(args[0])?;
    let ws = args[1];
    let ok_seq = !dict::is_dict(ws)
        && ab::length(ws).map(|n| n == 2).unwrap_or_else(|_| {
            clear_exc();
            false
        });
    if !ok_seq {
        raise!(TypeError, "tcsetwinsize, arg 2: must be a two-item sequence");
    }
    let row = ab::getitem(ws, &int::new_int(0))?;
    let col = ab::getitem(ws, &int::new_int(1))?;
    let r = long_arg(&row)?;
    let c = long_arg(&col)?;
    let mut w: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut w) } == -1 {
        return Err(term_err());
    }
    if !(0..=u16::MAX as i64).contains(&r) || !(0..=u16::MAX as i64).contains(&c) {
        raise!(OverflowError, "winsize value(s) out of range.");
    }
    w.ws_row = r as u16;
    w.ws_col = c as u16;
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &w) } == -1 {
        return Err(term_err());
    }
    Ok(none::none_ref())
}

const CONSTANTS: &[(&str, i64)] = &[
    // speeds
    ("B0", 0), ("B50", 1), ("B75", 2), ("B110", 3), ("B134", 4), ("B150", 5), ("B200", 6), ("B300", 7),
    ("B600", 0o10), ("B1200", 0o11), ("B1800", 0o12), ("B2400", 0o13), ("B4800", 0o14), ("B9600", 0o15),
    ("B19200", 0o16), ("B38400", 0o17), ("B57600", 0o10001), ("B115200", 0o10002), ("B230400", 0o10003),
    ("B460800", 0o10004), ("B500000", 0o10005), ("B576000", 0o10006), ("B921600", 0o10007),
    ("B1000000", 0o10010), ("B1152000", 0o10011), ("B1500000", 0o10012), ("B2000000", 0o10013),
    ("B2500000", 0o10014), ("B3000000", 0o10015), ("B3500000", 0o10016), ("B4000000", 0o10017),
    ("EXTA", 0o16), ("EXTB", 0o17),
    // tcsetattr / tcflush / tcflow
    ("TCSANOW", 0), ("TCSADRAIN", 1), ("TCSAFLUSH", 2),
    ("TCIFLUSH", 0), ("TCOFLUSH", 1), ("TCIOFLUSH", 2),
    ("TCOOFF", 0), ("TCOON", 1), ("TCIOFF", 2), ("TCION", 3),
    // c_iflag
    ("IGNBRK", 0o1), ("BRKINT", 0o2), ("IGNPAR", 0o4), ("PARMRK", 0o10), ("INPCK", 0o20), ("ISTRIP", 0o40),
    ("INLCR", 0o100), ("IGNCR", 0o200), ("ICRNL", 0o400), ("IUCLC", 0o1000), ("IXON", 0o2000),
    ("IXANY", 0o4000), ("IXOFF", 0o10000), ("IMAXBEL", 0o20000), ("IUTF8", 0o40000),
    // c_oflag
    ("OPOST", 0o1), ("OLCUC", 0o2), ("ONLCR", 0o4), ("OCRNL", 0o10), ("ONOCR", 0o20), ("ONLRET", 0o40),
    ("OFILL", 0o100), ("OFDEL", 0o200), ("NLDLY", 0o400), ("NL0", 0), ("NL1", 0o400), ("CRDLY", 0o3000),
    ("CR0", 0), ("CR1", 0o1000), ("CR2", 0o2000), ("CR3", 0o3000), ("TABDLY", 0o14000), ("TAB0", 0),
    ("TAB1", 0o4000), ("TAB2", 0o10000), ("TAB3", 0o14000), ("XTABS", 0o14000), ("BSDLY", 0o20000),
    ("BS0", 0), ("BS1", 0o20000), ("VTDLY", 0o40000), ("VT0", 0), ("VT1", 0o40000), ("FFDLY", 0o100000),
    ("FF0", 0), ("FF1", 0o100000),
    // c_cflag
    ("CBAUD", 0o10017), ("CBAUDEX", 0o10000), ("CSIZE", 0o60), ("CS5", 0), ("CS6", 0o20), ("CS7", 0o40),
    ("CS8", 0o60), ("CSTOPB", 0o100), ("CREAD", 0o200), ("PARENB", 0o400), ("PARODD", 0o1000),
    ("HUPCL", 0o2000), ("CLOCAL", 0o4000), ("CIBAUD", 0o2003600000), ("CMSPAR", 0o10000000000),
    ("CRTSCTS", 0o20000000000), ("IBSHIFT", 16),
    // c_lflag
    ("ISIG", 0o1), ("ICANON", 0o2), ("XCASE", 0o4), ("ECHO", 0o10), ("ECHOE", 0o20), ("ECHOK", 0o40),
    ("ECHONL", 0o100), ("NOFLSH", 0o200), ("TOSTOP", 0o400), ("ECHOCTL", 0o1000), ("ECHOPRT", 0o2000),
    ("ECHOKE", 0o4000), ("FLUSHO", 0o10000), ("PENDIN", 0o40000), ("IEXTEN", 0o100000),
    ("EXTPROC", 0o200000),
    // c_cc indices
    ("VINTR", 0), ("VQUIT", 1), ("VERASE", 2), ("VKILL", 3), ("VEOF", 4), ("VTIME", 5), ("VMIN", 6),
    ("VSWTC", 7), ("VSWTCH", 7), ("VSTART", 8), ("VSTOP", 9), ("VSUSP", 10), ("VEOL", 11),
    ("VREPRINT", 12), ("VDISCARD", 13), ("VWERASE", 14), ("VLNEXT", 15), ("VEOL2", 16),
    ("NCCS", 32), ("NCC", 8),
    // ioctls
    ("TCGETS", 0x5401), ("TCSETS", 0x5402), ("TCSETSW", 0x5403), ("TCSETSF", 0x5404), ("TCGETA", 0x5405),
    ("TCSETA", 0x5406), ("TCSETAW", 0x5407), ("TCSETAF", 0x5408), ("TCSBRK", 0x5409), ("TCXONC", 0x540A),
    ("TCFLSH", 0x540B), ("TIOCEXCL", 0x540C), ("TIOCNXCL", 0x540D), ("TIOCSCTTY", 0x540E),
    ("TIOCGPGRP", 0x540F), ("TIOCSPGRP", 0x5410), ("TIOCOUTQ", 0x5411), ("TIOCSTI", 0x5412),
    ("TIOCGWINSZ", 0x5413), ("TIOCSWINSZ", 0x5414), ("TIOCMGET", 0x5415), ("TIOCMBIS", 0x5416),
    ("TIOCMBIC", 0x5417), ("TIOCMSET", 0x5418), ("TIOCGSOFTCAR", 0x5419), ("TIOCSSOFTCAR", 0x541A),
    ("FIONREAD", 0x541B), ("TIOCINQ", 0x541B), ("TIOCLINUX", 0x541C), ("TIOCCONS", 0x541D),
    ("TIOCGSERIAL", 0x541E), ("TIOCSSERIAL", 0x541F), ("TIOCPKT", 0x5420), ("FIONBIO", 0x5421),
    ("TIOCNOTTY", 0x5422), ("TIOCSETD", 0x5423), ("TIOCGETD", 0x5424), ("TCSBRKP", 0x5425),
    ("TIOCTTYGSTRUCT", 0x5426), ("FIONCLEX", 0x5450), ("FIOCLEX", 0x5451), ("FIOASYNC", 0x5452),
    ("TIOCSERCONFIG", 0x5453), ("TIOCSERGWILD", 0x5454), ("TIOCSERSWILD", 0x5455),
    ("TIOCGLCKTRMIOS", 0x5456), ("TIOCSLCKTRMIOS", 0x5457), ("TIOCSERGSTRUCT", 0x5458),
    ("TIOCSERGETLSR", 0x5459), ("TIOCSERGETMULTI", 0x545A), ("TIOCSERSETMULTI", 0x545B),
    ("TIOCMIWAIT", 0x545C), ("TIOCGICOUNT", 0x545D), ("TIOCGPTN", 0x8004_5430), ("TIOCSPTLCK", 0x4004_5431),
    ("TIOCSER_TEMT", 1),
    ("TIOCPKT_DATA", 0), ("TIOCPKT_FLUSHREAD", 1), ("TIOCPKT_FLUSHWRITE", 2), ("TIOCPKT_STOP", 4),
    ("TIOCPKT_START", 8), ("TIOCPKT_NOSTOP", 16), ("TIOCPKT_DOSTOP", 32),
    ("TIOCM_LE", 0x001), ("TIOCM_DTR", 0x002), ("TIOCM_RTS", 0x004), ("TIOCM_ST", 0x008),
    ("TIOCM_SR", 0x010), ("TIOCM_CTS", 0x020), ("TIOCM_CAR", 0x040), ("TIOCM_RNG", 0x080),
    ("TIOCM_DSR", 0x100), ("TIOCM_CD", 0x040), ("TIOCM_RI", 0x080),
    ("N_TTY", 0), ("N_SLIP", 1), ("N_MOUSE", 2), ("N_PPP", 3),
    ("IOCSIZE_MASK", 0x3fff_0000), ("IOCSIZE_SHIFT", 16),
];

pub fn init() -> PyResult {
    if ERROR.load(Ordering::Acquire).is_null() {
        let e = crate::modules::devtools::new_exception("termios", "error", crate::types::exceptions::Exception())?;
        ERROR.store(e.into_raw(), Ordering::Release);
    }
    let methods = mdefs![
        ("tcgetattr", O, t_tcgetattr, "tcgetattr($module, fd, /)\n--\n\nReturn the terminal attributes of fd as a list\n[iflag, oflag, cflag, lflag, ispeed, ospeed, cc]; cc holds one-byte bytes\nobjects, or ints for VMIN and VTIME in non-canonical mode."),
        ("tcsetattr", FAST, t_tcsetattr, "tcsetattr($module, fd, when, attributes, /)\n--\n\nSet the terminal attributes of fd from a list as returned by tcgetattr().\n\nwhen is TCSANOW, TCSADRAIN or TCSAFLUSH."),
        ("tcsendbreak", FAST, t_tcsendbreak, "tcsendbreak($module, fd, duration, /)\n--\n\nTransmit a break on fd (duration 0 means the default length)."),
        ("tcdrain", O, t_tcdrain, "tcdrain($module, fd, /)\n--\n\nBlock until all output written to fd has been transmitted."),
        ("tcflush", FAST, t_tcflush, "tcflush($module, fd, queue, /)\n--\n\nDiscard pending terminal data: TCIFLUSH (input), TCOFLUSH (output) or TCIOFLUSH (both)."),
        ("tcflow", FAST, t_tcflow, "tcflow($module, fd, action, /)\n--\n\nSuspend or restart terminal I/O: TCOOFF, TCOON, TCIOFF or TCION."),
        ("tcgetwinsize", O, t_tcgetwinsize, "tcgetwinsize($module, fd, /)\n--\n\nReturn the window size of the terminal fd as (rows, columns)."),
        ("tcsetwinsize", FAST, t_tcsetwinsize, "tcsetwinsize($module, fd, winsize, /)\n--\n\nSet the window size of the terminal fd from a (rows, columns) pair."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("termios", methods, Some("POSIX terminal control (tcgetattr(), tcsetattr() and related calls).\n\nEvery function takes the terminal as a file descriptor or an object with\na fileno() method; the constants mirror the C header definitions."))?;
    dict::set_item_str(&d, "error", unsafe { &*ERROR.load(Ordering::Acquire) })?;
    for (n, v) in CONSTANTS {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    Ok(m)
}
