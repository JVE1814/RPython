//! The `time` module (Linux).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::modules::posix::kwparse;
use crate::object::PyRef;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::typeobject::tname;
use crate::types::{bool_, dict, float, int, module, none, str as pystr, tuple};
use std::ffi::CString;
use std::sync::atomic::{AtomicPtr, Ordering};

pub static STRUCT_TIME: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

const TM_FIELDS: &[Field] = &[
    Field::new("tm_year", "year, for example, 1993"),
    Field::new("tm_mon", "month of year, range [1, 12]"),
    Field::new("tm_mday", "day of month, range [1, 31]"),
    Field::new("tm_hour", "hours, range [0, 23]"),
    Field::new("tm_min", "minutes, range [0, 59]"),
    Field::new("tm_sec", "seconds, range [0, 61])"),
    Field::new("tm_wday", "day of week, range [0, 6], Monday is 0"),
    Field::new("tm_yday", "day of year, range [1, 366]"),
    Field::new("tm_isdst", "1 if summer time is in effect, 0 if not, and -1 if unknown"),
    Field::new("tm_zone", "abbreviation of timezone name"),
    Field::new("tm_gmtoff", "offset from UTC in seconds"),
];

fn struct_time_type() -> *mut PyTypeObject {
    STRUCT_TIME.load(Ordering::Acquire)
}

pub fn init_types() -> PyResult<()> {
    if struct_time_type().is_null() {
        let t = structseq::new_type(&Desc {
            name: "time.struct_time",
            doc: "The time value as returned by gmtime(), localtime(), and strptime(), and\n accepted by asctime(), mktime() and strftime().  May be considered as a\n sequence of 9 integers.\n\n Note that several fields' values are not the same as those defined by\n the C language standard for struct tm.  For example, the value of the\n field tm_year is the actual year, not year - 1900.  See individual\n fields' descriptions for details.",
            fields: TM_FIELDS,
            n_in_sequence: 9,
        })?;
        STRUCT_TIME.store(t, Ordering::Release);
    }
    Ok(())
}

// ------------------------------------------------------------------ clocks

fn clock_ts(clk: libc::clockid_t) -> PyResult<libc::timespec> {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(clk, &mut ts) } != 0 {
        return Err(crate::modules::posix::path::os_err());
    }
    Ok(ts)
}
fn ts_f64(ts: &libc::timespec) -> f64 {
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}
fn ts_ns(ts: &libc::timespec) -> PyRef {
    int::new_int_i128(ts.tv_sec as i128 * 1_000_000_000 + ts.tv_nsec as i128)
}

/// Current wall-clock time in seconds (float).
pub fn now_f64() -> f64 {
    clock_ts(libc::CLOCK_REALTIME).map(|t| ts_f64(&t)).unwrap_or(0.0)
}
/// Monotonic clock in seconds.
pub fn monotonic_f64() -> f64 {
    clock_ts(libc::CLOCK_MONOTONIC).map(|t| ts_f64(&t)).unwrap_or(0.0)
}

macro_rules! clockfn {
    ($f:ident, $fns:ident, $clk:expr) => {
        fn $f(_m: &PyObject) -> PyResult {
            Ok(float::new_float(ts_f64(&clock_ts($clk)?)))
        }
        fn $fns(_m: &PyObject) -> PyResult {
            Ok(ts_ns(&clock_ts($clk)?))
        }
    };
}
clockfn!(time_time, time_time_ns, libc::CLOCK_REALTIME);
clockfn!(time_monotonic, time_monotonic_ns, libc::CLOCK_MONOTONIC);
clockfn!(time_perf_counter, time_perf_counter_ns, libc::CLOCK_MONOTONIC);
clockfn!(time_process_time, time_process_time_ns, libc::CLOCK_PROCESS_CPUTIME_ID);
clockfn!(time_thread_time, time_thread_time_ns, libc::CLOCK_THREAD_CPUTIME_ID);

fn clk_arg(o: &PyObject) -> PyResult<libc::clockid_t> {
    crate::modules::posix::path::c_int(o)
}

fn time_clock_gettime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("clock_gettime", args, 1, 1)?;
    Ok(float::new_float(ts_f64(&clock_ts(clk_arg(args[0])?)?)))
}
fn time_clock_gettime_ns(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("clock_gettime_ns", args, 1, 1)?;
    Ok(ts_ns(&clock_ts(clk_arg(args[0])?)?))
}
fn time_clock_getres(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("clock_getres", args, 1, 1)?;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_getres(clk_arg(args[0])?, &mut ts) } != 0 {
        return Err(crate::modules::posix::path::os_err());
    }
    Ok(float::new_float(ts_f64(&ts)))
}
fn time_clock_settime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("clock_settime", args, 2, 2)?;
    let clk = clk_arg(args[0])?;
    let t = ab::float_value(args[1])?;
    let sec = t.floor();
    let ts = libc::timespec { tv_sec: sec as i64, tv_nsec: ((t - sec) * 1e9) as i64 };
    if unsafe { libc::clock_settime(clk, &ts) } != 0 {
        return Err(crate::modules::posix::path::os_err());
    }
    Ok(none::none_ref())
}
fn time_clock_settime_ns(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("clock_settime_ns", args, 2, 2)?;
    let clk = clk_arg(args[0])?;
    let t = ab::as_i128_index(args[1])?;
    let ts = libc::timespec { tv_sec: t.div_euclid(1_000_000_000) as i64, tv_nsec: t.rem_euclid(1_000_000_000) as i64 };
    if unsafe { libc::clock_settime(clk, &ts) } != 0 {
        return Err(crate::modules::posix::path::os_err());
    }
    Ok(none::none_ref())
}
fn time_pthread_getcpuclockid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pthread_getcpuclockid", args, 1, 1)?;
    let tid = ab::as_i128_index(args[0])? as libc::pthread_t;
    let mut clk: libc::clockid_t = 0;
    let r = unsafe { libc::pthread_getcpuclockid(tid, &mut clk) };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    Ok(int::new_int(clk as i64))
}

fn time_get_clock_info(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_clock_info", args, 1, 1)?;
    if !pystr::is_str(args[0]) {
        raise!(TypeError, "get_clock_info() argument must be str, not {}", tname(args[0]));
    }
    let name = pystr::to_string_lossy(args[0]);
    let (imp, clk, mono, adj) = match name.as_str() {
        "time" => ("clock_gettime(CLOCK_REALTIME)", libc::CLOCK_REALTIME, false, true),
        "monotonic" => ("clock_gettime(CLOCK_MONOTONIC)", libc::CLOCK_MONOTONIC, true, false),
        "perf_counter" => ("clock_gettime(CLOCK_MONOTONIC)", libc::CLOCK_MONOTONIC, true, false),
        "process_time" => ("clock_gettime(CLOCK_PROCESS_CPUTIME_ID)", libc::CLOCK_PROCESS_CPUTIME_ID, true, false),
        "thread_time" => ("clock_gettime(CLOCK_THREAD_CPUTIME_ID)", libc::CLOCK_THREAD_CPUTIME_ID, true, false),
        _ => raise!(ValueError, "unknown clock"),
    };
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let res = if unsafe { libc::clock_getres(clk, &mut ts) } == 0 { ts_f64(&ts) } else { 1e-9 };
    let types = crate::import::import_absolute("types")?;
    let ns = ab::getattr_str(&types, "SimpleNamespace")?;
    let kwn = tuple::new_tuple(vec![
        pystr::new_str("implementation"),
        pystr::new_str("monotonic"),
        pystr::new_str("adjustable"),
        pystr::new_str("resolution"),
    ]);
    let a = pystr::new_str(imp);
    let b = bool_::new_bool(mono);
    let c = bool_::new_bool(adj);
    let d = float::new_float(res);
    ab::call_kw(&ns, &[&a, &b, &c, &d], Some(&kwn))
}

// ------------------------------------------------------------------ sleep

fn time_sleep(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sleep", args, 1, 1)?;
    let o = args[0];
    crate::modules::sys_full::audit("time.sleep", &[o])?;
    let secs: f64 = if int::is_int(o) {
        let v = ab::as_i128_index(o)?;
        if v < 0 {
            raise!(ValueError, "sleep length must be non-negative");
        }
        v as f64
    } else if float::is_float(o) || ab::lookup_special(o, crate::name!("__float__"))?.is_some() {
        ab::float_value(o)?
    } else if ab::has_index(o) {
        ab::as_i128_index(o)? as f64
    } else {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer or float, must be real number", tname(o));
    };
    if secs.is_nan() {
        raise!(ValueError, "Invalid value NaN (not a number)");
    }
    if secs < 0.0 {
        raise!(ValueError, "sleep length must be non-negative");
    }
    if secs > 9.2e9 {
        raise!(OverflowError, "sleep length is too large");
    }
    sleep_secs(secs)?;
    Ok(none::none_ref())
}

/// Interruptible sleep: signal handlers run on EINTR, exceptions propagate.
pub fn sleep_secs(secs: f64) -> PyResult<()> {
    let mut deadline = clock_ts(libc::CLOCK_MONOTONIC)?;
    let whole = secs.floor();
    let mut nsec = deadline.tv_nsec + ((secs - whole) * 1e9).round() as i64;
    deadline.tv_sec += whole as i64 + nsec / 1_000_000_000;
    nsec %= 1_000_000_000;
    deadline.tv_nsec = nsec;
    loop {
        // A signal that arrived just before the call would otherwise only be
        // handled after the whole sleep.
        crate::modules::posix::path::check_signals()?;
        let r = crate::threads::blocking_pure(true, || unsafe { // (stability3)
            libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &deadline, std::ptr::null_mut())
        });
        if r == 0 {
            return Ok(());
        }
        if r == libc::EINTR {
            crate::modules::posix::path::check_signals()?;
            continue;
        }
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
}

// ------------------------------------------------------------------ struct tm conversion

/// Convert a timestamp object to time_t (floor rounding).
fn object_to_time_t(o: &PyObject) -> PyResult<libc::time_t> {
    if float::is_float(o) {
        let d = float::float_value(o);
        if d.is_nan() {
            raise!(ValueError, "Invalid value NaN (not a number)");
        }
        let f = d.floor();
        if !(f >= -9.223372036854776e18 && f < 9.223372036854776e18) {
            raise!(OverflowError, "timestamp out of range for platform time_t");
        }
        return Ok(f as i64);
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "timestamp out of range for platform time_t"),
    }
}

fn opt_time(o: Option<&PyObject>) -> PyResult<libc::time_t> {
    match o {
        None => Ok(clock_ts(libc::CLOCK_REALTIME)?.tv_sec),
        Some(x) if x.is(none::none()) => Ok(clock_ts(libc::CLOCK_REALTIME)?.tv_sec),
        Some(x) => object_to_time_t(x),
    }
}

fn cstr_opt(p: *const libc::c_char) -> PyRef {
    if p.is_null() {
        return none::none_ref();
    }
    let b = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
    crate::modules::posix::path::fs_decode(b).unwrap_or_else(|_| {
        clear_exc();
        none::none_ref()
    })
}

fn tm_to_struct(tm: &libc::tm) -> PyRef {
    let i = |v: i64| int::new_int(v);
    let vals = vec![
        i(tm.tm_year as i64 + 1900),
        i(tm.tm_mon as i64 + 1),
        i(tm.tm_mday as i64),
        i(tm.tm_hour as i64),
        i(tm.tm_min as i64),
        i(tm.tm_sec as i64),
        i(((tm.tm_wday + 6) % 7) as i64),
        i(tm.tm_yday as i64 + 1),
        i(tm.tm_isdst as i64),
        cstr_opt(tm.tm_zone),
        i(tm.tm_gmtoff),
    ];
    structseq::new_instance(struct_time_type(), vals)
}

pub fn gmtime_tm(t: libc::time_t) -> PyResult<libc::tm> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::gmtime_r(&t, &mut tm) }.is_null() {
        let e = crate::modules::posix::path::errno();
        if e == libc::EOVERFLOW || e == 0 {
            raise!(OverflowError, "timestamp out of range for platform time_t");
        }
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(tm)
}

pub fn localtime_tm(t: libc::time_t) -> PyResult<libc::tm> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { *libc::__errno_location() = 0 };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        let e = crate::modules::posix::path::errno();
        let e = if e == 0 { libc::EINVAL } else { e };
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(tm)
}

fn time_gmtime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("gmtime", args, 0, 1)?;
    let t = opt_time(args.first().copied())?;
    Ok(tm_to_struct(&gmtime_tm(t)?))
}

fn time_localtime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("localtime", args, 0, 1)?;
    let t = opt_time(args.first().copied())?;
    Ok(tm_to_struct(&localtime_tm(t)?))
}

/// Parsed time tuple (with storage for tm_zone).
struct TmArg {
    tm: libc::tm,
    _zone: Option<CString>,
}

fn int_field(o: &PyObject) -> PyResult<i32> {
    if float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v > i32::MAX as i64 => raise!(OverflowError, "signed integer is greater than maximum"),
        Some(v) if v < i32::MIN as i64 => raise!(OverflowError, "signed integer is less than minimum"),
        Some(v) => Ok(v as i32),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

fn gettmarg(fname: &str, o: &PyObject) -> PyResult<TmArg> {
    if !tuple::is_tuple(o) {
        raise!(TypeError, "Tuple or struct_time argument required");
    }
    let items = tuple::tuple_items(o);
    if items.len() != 9 {
        raise!(TypeError, "{}(): illegal time tuple argument", fname);
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let y = int_field(items[0])?;
    tm.tm_mon = int_field(items[1])?;
    tm.tm_mday = int_field(items[2])?;
    tm.tm_hour = int_field(items[3])?;
    tm.tm_min = int_field(items[4])?;
    tm.tm_sec = int_field(items[5])?;
    tm.tm_wday = int_field(items[6])?;
    tm.tm_yday = int_field(items[7])?;
    tm.tm_isdst = int_field(items[8])?;
    if (y as i64) - 1900 < i32::MIN as i64 {
        raise!(OverflowError, "year out of range");
    }
    tm.tm_year = y.wrapping_sub(1900);
    tm.tm_mon -= 1;
    tm.tm_wday = (tm.tm_wday + 1) % 7;
    if tm.tm_wday < 0 {
        raise!(ValueError, "day of week out of range");
    }
    tm.tm_yday -= 1;
    let mut zone = None;
    if o.is_type(struct_time_type()) {
        let z = structseq::get_item(o, 9);
        if pystr::is_str(z) {
            let b = crate::modules::posix::path::fs_encode_str(z)?;
            if !b.contains(&0) {
                let c = CString::new(b).unwrap();
                tm.tm_zone = c.as_ptr();
                zone = Some(c);
            }
        }
        let off = structseq::get_item(o, 10);
        if int::is_int(off) {
            tm.tm_gmtoff = ab::as_i64(off)?;
        }
    }
    Ok(TmArg { tm, _zone: zone })
}

fn checktm(tm: &libc::tm) -> PyResult<()> {
    if tm.tm_mon == -1 {
        // month 0 is allowed (normalised later)
    } else if tm.tm_mon < 0 || tm.tm_mon > 11 {
        raise!(ValueError, "month out of range");
    }
    if tm.tm_mday < 0 || tm.tm_mday > 31 {
        raise!(ValueError, "day of month out of range");
    }
    if tm.tm_hour < 0 || tm.tm_hour > 23 {
        raise!(ValueError, "hour out of range");
    }
    if tm.tm_min < 0 || tm.tm_min > 59 {
        raise!(ValueError, "minute out of range");
    }
    if tm.tm_sec < 0 || tm.tm_sec > 61 {
        raise!(ValueError, "seconds out of range");
    }
    if tm.tm_wday < 0 {
        raise!(ValueError, "day of week out of range");
    }
    if tm.tm_yday < -1 || tm.tm_yday > 365 {
        raise!(ValueError, "day of year out of range");
    }
    Ok(())
}

fn normalize_zero(tm: &mut libc::tm) {
    if tm.tm_mon == -1 {
        tm.tm_mon = 0;
    }
    if tm.tm_mday == 0 {
        tm.tm_mday = 1;
    }
    if tm.tm_yday == -1 {
        tm.tm_yday = 0;
    }
    if tm.tm_isdst < -1 {
        tm.tm_isdst = -1;
    } else if tm.tm_isdst > 1 {
        tm.tm_isdst = 1;
    }
}

fn current_local_tm() -> PyResult<libc::tm> {
    let t = clock_ts(libc::CLOCK_REALTIME)?.tv_sec;
    localtime_tm(t)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn format_asctime(tm: &libc::tm) -> String {
    format!(
        "{} {}{:3} {:02}:{:02}:{:02} {}",
        DAYS[tm.tm_wday.rem_euclid(7) as usize],
        MONTHS[tm.tm_mon.rem_euclid(12) as usize],
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        1900i64 + tm.tm_year as i64
    )
}

fn time_asctime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("asctime", args, 0, 1)?;
    let tm = match args.first() {
        None => current_local_tm()?,
        Some(o) => {
            let mut a = gettmarg("asctime", o)?;
            checktm(&a.tm)?;
            normalize_zero(&mut a.tm);
            a.tm
        }
    };
    Ok(pystr::new_str(&format_asctime(&tm)))
}

fn time_ctime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ctime", args, 0, 1)?;
    let t = opt_time(args.first().copied())?;
    let tm = localtime_tm(t)?;
    Ok(pystr::new_str(&format_asctime(&tm)))
}

fn time_mktime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("mktime", args, 1, 1)?;
    let a = gettmarg("mktime", args[0])?;
    let mut tm = a.tm;
    tm.tm_wday = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    if t == -1 && tm.tm_wday == -1 {
        raise!(OverflowError, "mktime argument out of range");
    }
    Ok(float::new_float(t as f64))
}

fn strftime_chunk(fmt: &[u8], tm: &libc::tm) -> PyResult<Vec<u8>> {
    if fmt.is_empty() {
        return Ok(Vec::new());
    }
    let c = CString::new(fmt.to_vec()).unwrap();
    let mut size = 1024usize.max(fmt.len() * 4);
    loop {
        let mut buf = vec![0u8; size];
        let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, size, c.as_ptr(), tm) };
        if n > 0 || size >= 256 * fmt.len() + 4096 {
            buf.truncate(n);
            return Ok(buf);
        }
        size *= 4;
    }
}

fn time_strftime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("strftime", args, 1, 2)?;
    let f = args[0];
    if !pystr::is_str(f) {
        raise!(TypeError, "strftime() argument 1 must be str, not {}", tname(f));
    }
    let mut storage: Option<TmArg> = None;
    let tm = match args.get(1) {
        None => current_local_tm()?,
        Some(o) => {
            let mut a = gettmarg("strftime", o)?;
            checktm(&a.tm)?;
            normalize_zero(&mut a.tm);
            let tm = a.tm;
            storage = Some(a);
            tm
        }
    };
    let cps: Vec<u32> = pystr::str_data(f).iter().collect();
    let mut out: Vec<u32> = Vec::with_capacity(cps.len() * 2);
    let mut i = 0;
    while i < cps.len() {
        if cps[i] == 0 || cps[i] >= 0x80 {
            out.push(cps[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < cps.len() && cps[i] != 0 && cps[i] < 0x80 {
            i += 1;
        }
        let mut chunk: Vec<u8> = cps[start..i].iter().map(|&c| c as u8).collect();
        // a trailing lone '%' directly before a non-ASCII char stays literal
        let mut trailing = 0;
        while chunk.last() == Some(&b'%') {
            trailing += 1;
            chunk.pop();
        }
        if trailing % 2 == 1 {
            if i == cps.len() {
                raise!(ValueError, "Invalid format string");
            }
        }
        for _ in 0..trailing / 2 {
            chunk.extend_from_slice(b"%%");
        }
        let res = strftime_chunk(&chunk, &tm)?;
        let s = crate::modules::posix::path::fs_decode(&res)?;
        out.extend(pystr::str_data(&s).iter());
        if trailing % 2 == 1 {
            out.push('%' as u32);
        }
    }
    drop(storage);
    Ok(pystr::from_codepoints(&out))
}

fn time_strptime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = crate::import::import_absolute("_strptime")?;
    let f = ab::getattr_str(&m, "_strptime_time")?;
    ab::call(&f, args)
}

// ------------------------------------------------------------------ timezone

fn init_timezone(d: &PyObject) -> PyResult<()> {
    unsafe {
        unsafe extern "C" {
            fn tzset();
        }
        tzset();
    }
    const YEAR: i64 = (365 * 24 + 6) * 3600;
    let now = clock_ts(libc::CLOCK_REALTIME)?.tv_sec;
    let t = (now / YEAR) * YEAR;
    let jan = localtime_tm(t)?;
    let jul = localtime_tm(t + YEAR / 2)?;
    let janzone = -jan.tm_gmtoff;
    let julzone = -jul.tm_gmtoff;
    let janname = cstr_opt(jan.tm_zone);
    let julname = cstr_opt(jul.tm_zone);
    let (tz, alt, names) = if janzone < julzone {
        (julzone, janzone, tuple::new_tuple(vec![julname, janname]))
    } else {
        (janzone, julzone, tuple::new_tuple(vec![janname, julname]))
    };
    dict::set_item_str(d, "timezone", &int::new_int(tz))?;
    dict::set_item_str(d, "altzone", &int::new_int(alt))?;
    dict::set_item_str(d, "daylight", &int::new_int((janzone != julzone) as i64))?;
    dict::set_item_str(d, "tzname", &names)?;
    Ok(())
}

fn time_tzset(m: &PyObject) -> PyResult {
    if crate::types::module::is_module(m) {
        init_timezone(&module::module_dict(m))?;
    }
    Ok(none::none_ref())
}

pub fn init() -> PyResult {
    init_types()?;
    let m = module::new_module("time")?;
    let d = module::module_dict(&m);
    let methods = crate::mdefs![
        ("time", NOARGS, time_time),
        ("time_ns", NOARGS, time_time_ns),
        ("monotonic", NOARGS, time_monotonic),
        ("monotonic_ns", NOARGS, time_monotonic_ns),
        ("perf_counter", NOARGS, time_perf_counter),
        ("perf_counter_ns", NOARGS, time_perf_counter_ns),
        ("process_time", NOARGS, time_process_time),
        ("process_time_ns", NOARGS, time_process_time_ns),
        ("thread_time", NOARGS, time_thread_time),
        ("thread_time_ns", NOARGS, time_thread_time_ns),
        ("clock_gettime", FAST, time_clock_gettime, "clock_gettime($module, clk_id, /)\n--\n\nReturn the time of the given clock as a float, in seconds."),
        ("clock_gettime_ns", FAST, time_clock_gettime_ns, "clock_gettime_ns($module, clk_id, /)\n--\n\nReturn the time of the given clock as an int, in nanoseconds."),
        ("clock_getres", FAST, time_clock_getres),
        ("clock_settime", FAST, time_clock_settime),
        ("clock_settime_ns", FAST, time_clock_settime_ns),
        ("pthread_getcpuclockid", FAST, time_pthread_getcpuclockid),
        ("get_clock_info", FAST, time_get_clock_info),
        ("sleep", FAST, time_sleep, "sleep($module, seconds, /)\n--\n\nSuspend the calling thread for the given number of seconds (a float is fine)."),
        ("gmtime", FAST, time_gmtime),
        ("localtime", FAST, time_localtime),
        ("asctime", FAST, time_asctime),
        ("ctime", FAST, time_ctime),
        ("mktime", FAST, time_mktime, "mktime($module, time_tuple, /)\n--\n\nTurn a local-time struct_time or 9-tuple into seconds since the Epoch."),
        ("strftime", FAST, time_strftime),
        ("strptime", FAST, time_strptime),
        ("tzset", NOARGS, time_tzset),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    for (n, v) in [
        ("CLOCK_REALTIME", libc::CLOCK_REALTIME),
        ("CLOCK_MONOTONIC", libc::CLOCK_MONOTONIC),
        ("CLOCK_MONOTONIC_RAW", libc::CLOCK_MONOTONIC_RAW),
        ("CLOCK_PROCESS_CPUTIME_ID", libc::CLOCK_PROCESS_CPUTIME_ID),
        ("CLOCK_THREAD_CPUTIME_ID", libc::CLOCK_THREAD_CPUTIME_ID),
        ("CLOCK_BOOTTIME", libc::CLOCK_BOOTTIME),
        ("CLOCK_TAI", libc::CLOCK_TAI),
    ] {
        dict::set_item_str(&d, n, &int::new_int(v as i64))?;
    }
    dict::set_item_str(&d, "_STRUCT_TM_ITEMS", &int::new_int(11))?;
    dict::set_item_str(&d, "struct_time", &crate::types::typeobject::type_ref(struct_time_type()))?;
    init_timezone(&d)?;
    let _ = kwparse::<0>;
    Ok(m)
}
