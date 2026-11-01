//! The native `_datetime` module (data workstream).
//!
//! Instances have exactly the memory layout described by
//! `Include/datetime.h` (`PyDateTime_Delta`, `PyDateTime_Date`,
//! `PyDateTime_Time`, `PyDateTime_DateTime`, `PyDateTime_TZInfo`), so C
//! extensions that use the `PyDateTime_GET_*` macros read the fields
//! directly, and the module exports the `datetime_CAPI` capsule with the
//! `PyDateTime_CAPI` table.
//!
//! The behaviour (arithmetic, formatting, parsing, time zones) is not
//! duplicated here: at first import the module runs the source of
//! `Lib/_pydatetime.py` (embedded at build time) in a private namespace,
//! moves the methods of the pure classes onto the native types, and points
//! the class names of that namespace at the native types.  The pure code only
//! reaches instance state through the `_year`, `_month`, ... attributes, which
//! the native types provide as getset descriptors over the C fields, and it
//! constructs objects through the public constructors, whose native `tp_new`
//! validates its arguments with the `_*_new_args` helpers of that same source.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::mdefs;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

const PURE_SOURCE: &str = include_str!("../../../../Lib/_pydatetime.py");

// ===================================================================== layouts

#[repr(C)]
struct DeltaObj {
    ob_base: PyObject,
    hashcode: Py_hash_t,
    days: c_int,
    seconds: c_int,
    microseconds: c_int,
}

#[repr(C)]
struct DateObj {
    ob_base: PyObject,
    hashcode: Py_hash_t,
    hastzinfo: u8,
    data: [u8; 4],
}

#[repr(C)]
struct TimeObj {
    ob_base: PyObject,
    hashcode: Py_hash_t,
    hastzinfo: u8,
    data: [u8; 6],
    fold: u8,
    tzinfo: *mut PyObject,
}

#[repr(C)]
struct DateTimeObj {
    ob_base: PyObject,
    hashcode: Py_hash_t,
    hastzinfo: u8,
    data: [u8; 10],
    fold: u8,
    tzinfo: *mut PyObject,
}

/// `timezone` is not part of the public header; any layout works.
#[repr(C)]
struct TimezoneObj {
    ob_base: PyObject,
    offset: *mut PyObject,
    name: *mut PyObject,
}

/// Every hashable datetime object keeps its cached hash right after the
/// object header.
#[repr(C)]
struct HashHead {
    ob_base: PyObject,
    hashcode: Py_hash_t,
}

static mut DELTA_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut DATE_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut TIME_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut DATETIME_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut TZINFO_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut TIMEZONE_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn delta_type() -> *mut PyTypeObject {
    &raw mut DELTA_TYPE
}
fn date_type() -> *mut PyTypeObject {
    &raw mut DATE_TYPE
}
fn time_type() -> *mut PyTypeObject {
    &raw mut TIME_TYPE
}
fn datetime_type() -> *mut PyTypeObject {
    &raw mut DATETIME_TYPE
}
fn tzinfo_type() -> *mut PyTypeObject {
    &raw mut TZINFO_TYPE
}
fn timezone_type() -> *mut PyTypeObject {
    &raw mut TIMEZONE_TYPE
}

// Python-level helpers taken from the executed source (strong references
// kept for the life of the process).
static H_DELTA: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static H_DATE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static H_TIME: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static H_DATETIME: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static H_TIMEZONE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static UTC: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static CAPI: AtomicPtr<DateTimeCapi> = AtomicPtr::new(std::ptr::null_mut());

/// Weak reference to the most recently created `_datetime` module object.
static MODREF: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

/// The helpers and constants behind the native types count as the module's
/// state.  As in CPython, once the module object has been collected the
/// state is fetched again by importing `_datetime`, which fails with
/// ImportError when the import is blocked.  (A null MODREF means the first
/// initialisation is still running.)
fn require_module() -> PyResult<()> {
    let w = MODREF.load(Ordering::Acquire);
    if w.is_null() || !crate::types::weakref::is_dead(unsafe { &*w }) {
        return Ok(());
    }
    crate::import::import_absolute("_datetime")?;
    Ok(())
}

fn helper(slot: &AtomicPtr<PyObject>) -> PyResult<&'static PyObject> {
    require_module()?;
    let p = slot.load(Ordering::Acquire);
    if p.is_null() {
        raise!(RuntimeError, "_datetime is not initialized");
    }
    Ok(unsafe { &*p })
}

fn keep(slot: &AtomicPtr<PyObject>, v: PyRef) {
    let old = slot.swap(v.into_raw(), Ordering::AcqRel);
    unsafe { xdecref(old) };
}

// ===================================================================== helpers

unsafe fn alloc_obj(tp: *mut PyTypeObject) -> PyResult<*mut PyObject> {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        Ok(op)
    }
}

fn small(o: &PyObject) -> PyResult<i64> {
    ab::as_i64(o)
}

/// Call a field-validating helper with the constructor's arguments.
unsafe fn call_helper(h: &PyObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe {
        let a: &[&PyObject] = if args.is_null() { &[] } else { tuple::tuple_items(&*args) };
        let k = if kw.is_null() { None } else { Some(&*kw) };
        ab::call_with_kwargs_dict(h, a, k)
    }
}

fn fields(r: &PyObject, n: usize) -> PyResult<Vec<PyRef>> {
    if !tuple::is_tuple(r) || tuple::tuple_items(r).len() != n {
        raise!(SystemError, "_datetime: malformed constructor helper result");
    }
    Ok(tuple::tuple_items(r).iter().map(|o| o.to_ref()).collect())
}

fn obj_or_null(o: &PyObject) -> *mut PyObject {
    if o.is(none::none()) {
        std::ptr::null_mut()
    } else {
        o.to_ref().into_raw()
    }
}

// ===================================================================== tp_new

unsafe extern "C" fn delta_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let r = call_helper(helper(&H_DELTA)?, args, kw)?;
            let f = fields(&r, 3)?;
            new_delta_raw(tp, small(&f[0])? as c_int, small(&f[1])? as c_int, small(&f[2])? as c_int)
        })())
    }
}

unsafe fn new_delta_raw(tp: *mut PyTypeObject, d: c_int, s: c_int, us: c_int) -> PyResult {
    unsafe {
        let op = alloc_obj(tp)?;
        let o = &mut *(op as *mut DeltaObj);
        o.hashcode = -1;
        o.days = d;
        o.seconds = s;
        o.microseconds = us;
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn date_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let r = call_helper(helper(&H_DATE)?, args, kw)?;
            let f = fields(&r, 3)?;
            let y = small(&f[0])?;
            let op = alloc_obj(tp)?;
            let o = &mut *(op as *mut DateObj);
            o.hashcode = -1;
            o.hastzinfo = 0;
            o.data = [(y >> 8) as u8, y as u8, small(&f[1])? as u8, small(&f[2])? as u8];
            Ok(PyRef::from_raw(op))
        })())
    }
}

unsafe extern "C" fn time_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let r = call_helper(helper(&H_TIME)?, args, kw)?;
            let f = fields(&r, 6)?;
            let us = small(&f[3])?;
            let op = alloc_obj(tp)?;
            let o = &mut *(op as *mut TimeObj);
            o.hashcode = -1;
            o.data = [
                small(&f[0])? as u8,
                small(&f[1])? as u8,
                small(&f[2])? as u8,
                (us >> 16) as u8,
                (us >> 8) as u8,
                us as u8,
            ];
            o.fold = small(&f[5])? as u8;
            o.tzinfo = obj_or_null(&f[4]);
            o.hastzinfo = (!o.tzinfo.is_null()) as u8;
            Ok(PyRef::from_raw(op))
        })())
    }
}

unsafe extern "C" fn datetime_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let r = call_helper(helper(&H_DATETIME)?, args, kw)?;
            let f = fields(&r, 9)?;
            let y = small(&f[0])?;
            let us = small(&f[6])?;
            let op = alloc_obj(tp)?;
            let o = &mut *(op as *mut DateTimeObj);
            o.hashcode = -1;
            o.data = [
                (y >> 8) as u8,
                y as u8,
                small(&f[1])? as u8,
                small(&f[2])? as u8,
                small(&f[3])? as u8,
                small(&f[4])? as u8,
                small(&f[5])? as u8,
                (us >> 16) as u8,
                (us >> 8) as u8,
                us as u8,
            ];
            o.fold = small(&f[8])? as u8;
            o.tzinfo = obj_or_null(&f[7]);
            o.hastzinfo = (!o.tzinfo.is_null()) as u8;
            Ok(PyRef::from_raw(op))
        })())
    }
}

fn is_zero_delta(o: &PyObject) -> bool {
    if !o.is_type(delta_type()) {
        return false;
    }
    let d = unsafe { &*(o.as_ptr() as *const DeltaObj) };
    d.days == 0 && d.seconds == 0 && d.microseconds == 0
}

unsafe extern "C" fn timezone_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let r = call_helper(helper(&H_TIMEZONE)?, args, kw)?;
            let f = fields(&r, 2)?;
            let utc = UTC.load(Ordering::Acquire);
            if tp == timezone_type() && f[1].is(none::none()) && is_zero_delta(&f[0]) && !utc.is_null() {
                return Ok((*utc).to_ref());
            }
            let op = alloc_obj(tp)?;
            let o = &mut *(op as *mut TimezoneObj);
            o.offset = f[0].to_ref().into_raw();
            o.name = obj_or_null(&f[1]);
            Ok(PyRef::from_raw(op))
        })())
    }
}

// ===================================================================== dealloc

unsafe extern "C" fn plain_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn time_dealloc(o: *mut PyObject) {
    unsafe {
        let t = &mut *(o as *mut TimeObj);
        let tz = std::mem::replace(&mut t.tzinfo, std::ptr::null_mut());
        xdecref(tz);
        plain_dealloc(o);
    }
}

unsafe extern "C" fn datetime_dealloc(o: *mut PyObject) {
    unsafe {
        let t = &mut *(o as *mut DateTimeObj);
        let tz = std::mem::replace(&mut t.tzinfo, std::ptr::null_mut());
        xdecref(tz);
        plain_dealloc(o);
    }
}

unsafe extern "C" fn timezone_dealloc(o: *mut PyObject) {
    unsafe {
        let t = &mut *(o as *mut TimezoneObj);
        let a = std::mem::replace(&mut t.offset, std::ptr::null_mut());
        let b = std::mem::replace(&mut t.name, std::ptr::null_mut());
        xdecref(a);
        xdecref(b);
        plain_dealloc(o);
    }
}

// ===================================================================== field access

fn head(o: &PyObject) -> &mut HashHead {
    unsafe { &mut *(o.as_ptr() as *mut HashHead) }
}
fn delta(o: &PyObject) -> &DeltaObj {
    unsafe { &*(o.as_ptr() as *const DeltaObj) }
}
fn date_(o: &PyObject) -> &DateObj {
    unsafe { &*(o.as_ptr() as *const DateObj) }
}
fn time_(o: &PyObject) -> &TimeObj {
    unsafe { &*(o.as_ptr() as *const TimeObj) }
}
fn dt(o: &PyObject) -> &DateTimeObj {
    unsafe { &*(o.as_ptr() as *const DateTimeObj) }
}
fn tzobj(o: &PyObject) -> &TimezoneObj {
    unsafe { &*(o.as_ptr() as *const TimezoneObj) }
}

fn small_int(v: i64) -> PyResult {
    Ok(int::new_int(v))
}
fn ptr_or_none(p: *mut PyObject) -> PyResult {
    Ok(if p.is_null() { none::none_ref() } else { unsafe { (*p).to_ref() } })
}

fn get_hashcode(o: &PyObject) -> PyResult {
    small_int(head(o).hashcode as i64)
}
fn set_hashcode(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) => {
            head(o).hashcode = ab::as_i64(v)? as Py_hash_t;
            Ok(())
        }
        None => raise!(AttributeError, "cannot delete the cached hash"),
    }
}

fn g_days(o: &PyObject) -> PyResult {
    small_int(delta(o).days as i64)
}
fn g_seconds(o: &PyObject) -> PyResult {
    small_int(delta(o).seconds as i64)
}
fn g_microseconds(o: &PyObject) -> PyResult {
    small_int(delta(o).microseconds as i64)
}

fn g_year(o: &PyObject) -> PyResult {
    let d = &date_(o).data;
    small_int(((d[0] as i64) << 8) | d[1] as i64)
}
fn g_month(o: &PyObject) -> PyResult {
    small_int(date_(o).data[2] as i64)
}
fn g_day(o: &PyObject) -> PyResult {
    small_int(date_(o).data[3] as i64)
}

fn us_of(b: &[u8]) -> i64 {
    ((b[0] as i64) << 16) | ((b[1] as i64) << 8) | b[2] as i64
}

fn t_hour(o: &PyObject) -> PyResult {
    small_int(time_(o).data[0] as i64)
}
fn t_minute(o: &PyObject) -> PyResult {
    small_int(time_(o).data[1] as i64)
}
fn t_second(o: &PyObject) -> PyResult {
    small_int(time_(o).data[2] as i64)
}
fn t_microsecond(o: &PyObject) -> PyResult {
    small_int(us_of(&time_(o).data[3..6]))
}
fn t_fold(o: &PyObject) -> PyResult {
    small_int(time_(o).fold as i64)
}
fn t_tzinfo(o: &PyObject) -> PyResult {
    let t = time_(o);
    ptr_or_none(if t.hastzinfo != 0 { t.tzinfo } else { std::ptr::null_mut() })
}

fn dt_hour(o: &PyObject) -> PyResult {
    small_int(dt(o).data[4] as i64)
}
fn dt_minute(o: &PyObject) -> PyResult {
    small_int(dt(o).data[5] as i64)
}
fn dt_second(o: &PyObject) -> PyResult {
    small_int(dt(o).data[6] as i64)
}
fn dt_microsecond(o: &PyObject) -> PyResult {
    small_int(us_of(&dt(o).data[7..10]))
}
fn dt_fold(o: &PyObject) -> PyResult {
    small_int(dt(o).fold as i64)
}
fn dt_tzinfo(o: &PyObject) -> PyResult {
    let t = dt(o);
    ptr_or_none(if t.hastzinfo != 0 { t.tzinfo } else { std::ptr::null_mut() })
}

fn tz_offset(o: &PyObject) -> PyResult {
    ptr_or_none(tzobj(o).offset)
}
fn tz_name(o: &PyObject) -> PyResult {
    ptr_or_none(tzobj(o).name)
}

// ===================================================================== native methods

fn m_total_seconds(o: &PyObject) -> PyResult {
    require_module()?;
    let d = delta(o);
    let us = (d.days as i128 * 86_400 + d.seconds as i128) * 1_000_000 + d.microseconds as i128;
    // exact integer division, correctly rounded like int / int
    ab::binary_op(&int::new_int_i128(us), &int::new_int(1_000_000), ab::NbSlot::TrueDiv)
}

fn days_before_year(y: i64) -> i64 {
    let p = y - 1;
    p * 365 + p / 4 - p / 100 + p / 400
}

fn ordinal(y: i64, m: i64, d: i64) -> i64 {
    const BEFORE: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    days_before_year(y) + BEFORE[m as usize] + (if leap && m > 2 { 1 } else { 0 }) + d
}

/// Ordinal of the Monday that starts ISO week 1 of `y`.
fn week1_monday(y: i64) -> i64 {
    let jan1 = ordinal(y, 1, 1);
    let wd = (jan1 + 6) % 7; // Monday == 0
    let monday = jan1 - wd;
    if wd > 3 { monday + 7 } else { monday }
}

fn m_isocalendar(o: &PyObject) -> PyResult {
    require_module()?;
    let data = &date_(o).data;
    let mut y = ((data[0] as i64) << 8) | data[1] as i64;
    let today = ordinal(y, data[2] as i64, data[3] as i64);
    let mut start = week1_monday(y);
    if today < start {
        y -= 1;
        start = week1_monday(y);
    } else if today >= week1_monday(y + 1) {
        y += 1;
        start = week1_monday(y);
    }
    let off = today - start;
    let ns = NS.load(Ordering::Acquire);
    let Some(cls) = (if ns.is_null() { None } else { dict::get_item_rs(unsafe { &*ns }, "_IsoCalendarDate") }) else {
        raise!(SystemError, "_datetime: _IsoCalendarDate missing");
    };
    ab::call(&cls, &[&int::new_int(y), &int::new_int(off / 7 + 1), &int::new_int(off % 7 + 1)])
}

// ===================================================================== bridging

/// Names of the pure class namespace that must not be moved onto the
/// native types (instance layout and class bookkeeping).
const NOT_COPIED: &[&str] = &[
    "__new__",
    "__slots__",
    "__module__",
    "__qualname__",
    "__dict__",
    "__weakref__",
    "__init_subclass__",
    "__firstlineno__",
    "__static_attributes__",
    "__classdictcell__",
    "__classcell__",
];

fn copy_members(pure: &PyObject, native: *mut PyTypeObject) -> PyResult<()> {
    if !is_type(pure) {
        raise!(SystemError, "_datetime: expected a class from the pure implementation");
    }
    let src = type_dict(pure.as_ptr() as *mut PyTypeObject);
    let dst = type_dict(native);
    for (k, v) in dict::items_vec(src) {
        let Some(name) = pystr::ascii_str(&k) else { continue };
        if NOT_COPIED.contains(&name) {
            continue;
        }
        if type_name(v.ty()) == "member_descriptor" {
            continue;
        }
        // methods implemented natively win over their pure counterparts
        if let Some(existing) = dict::get_item(dst, &k)? {
            let kind = type_name(existing.ty());
            if kind == "method_descriptor" || kind == "classmethod_descriptor" {
                continue;
            }
        }
        dict::set_item(dst, &k, &v)?;
    }
    Ok(())
}

fn type_from_arg(o: &PyObject) -> PyResult<*mut PyTypeObject> {
    let tp = o.as_ptr() as *mut PyTypeObject;
    for t in [delta_type(), date_type(), time_type(), datetime_type(), tzinfo_type(), timezone_type()] {
        if tp == t {
            return Ok(t);
        }
    }
    if is_type(o) {
        return Ok(tp);
    }
    raise!(TypeError, "_set_class_attr() expects a type")
}

/// Replacement for the pure module's `_set_class_attr`: store a class
/// attribute even on the immutable native types.
fn m_set_class_attr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_set_class_attr", args, 3, 3)?;
    let tp = type_from_arg(args[0])?;
    dict::set_item(type_dict(tp), args[1], args[2])?;
    crate::types::slots::type_modified(tp);
    Ok(none::none_ref())
}

fn run_in(ns: &PyObject, src: &str, filename: &str) -> PyResult<()> {
    let code = crate::compile::compile_str(src, filename, crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, ns, Some(ns))?;
    Ok(())
}

static NS: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn build_namespace() -> PyResult<()> {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__name__", &pystr::new_str("datetime"))?;
    dict::set_item_str(&ns, "__builtins__", &crate::builtins::builtins_module())?;
    run_in(&ns, PURE_SOURCE, "<frozen _pydatetime>")?;

    let pairs: [(&str, *mut PyTypeObject); 6] = [
        ("tzinfo", tzinfo_type()),
        ("timezone", timezone_type()),
        ("timedelta", delta_type()),
        ("date", date_type()),
        ("time", time_type()),
        ("datetime", datetime_type()),
    ];
    for (name, tp) in pairs {
        let Some(pure) = dict::get_item_rs(&ns, name) else {
            raise!(SystemError, "_datetime: pure implementation lacks {}", name);
        };
        copy_members(&pure, tp)?;
    }
    for (name, tp) in pairs {
        dict::set_item_str(&ns, name, &type_ref(tp))?;
    }
    crate::builtins::add_methods_to_dict(
        &ns,
        mdefs![("_set_class_attr", FAST, m_set_class_attr, "Store a class attribute on a datetime type.")],
        None,
    )?;
    for (slot, name) in [
        (&H_DELTA, "_timedelta_fields"),
        (&H_DATE, "_date_new_args"),
        (&H_TIME, "_time_new_args"),
        (&H_DATETIME, "_datetime_new_args"),
        (&H_TIMEZONE, "_timezone_new_args"),
    ] {
        let Some(h) = dict::get_item_rs(&ns, name) else {
            raise!(SystemError, "_datetime: pure implementation lacks {}", name);
        };
        keep(slot, h);
    }
    // Slots of the static types are recomputed from their (now complete)
    // dictionaries, bases first.
    for (_, tp) in pairs {
        crate::types::slots::fixup_slots(tp);
        crate::types::slots::type_modified(tp);
    }
    // Class constants and module constants are rebuilt with native objects.
    run_in(&ns, "UTC = _init_constants()\n_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)\n", "<_datetime init>")?;
    let Some(utc) = dict::get_item_rs(&ns, "UTC") else {
        raise!(SystemError, "_datetime: UTC missing");
    };
    keep(&UTC, utc);
    keep(&NS, ns);
    Ok(())
}

// ===================================================================== C API

#[repr(C)]
struct DateTimeCapi {
    date_type: *mut PyTypeObject,
    datetime_type: *mut PyTypeObject,
    time_type: *mut PyTypeObject,
    delta_type: *mut PyTypeObject,
    tzinfo_type: *mut PyTypeObject,
    timezone_utc: *mut PyObject,
    date_from_date: unsafe extern "C" fn(c_int, c_int, c_int, *mut PyTypeObject) -> *mut PyObject,
    datetime_from_date_and_time:
        unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int, *mut PyObject, *mut PyTypeObject) -> *mut PyObject,
    time_from_time: unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut PyObject, *mut PyTypeObject) -> *mut PyObject,
    delta_from_delta: unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut PyTypeObject) -> *mut PyObject,
    timezone_from_timezone: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    datetime_from_timestamp: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
    date_from_timestamp: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    datetime_from_date_and_time_and_fold: unsafe extern "C" fn(
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        c_int,
        *mut PyObject,
        c_int,
        *mut PyTypeObject,
    ) -> *mut PyObject,
    time_from_time_and_fold:
        unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut PyObject, c_int, *mut PyTypeObject) -> *mut PyObject,
}

fn ints(v: &[c_int]) -> Vec<PyRef> {
    v.iter().map(|&i| int::new_int(i as i64)).collect()
}

unsafe fn borrowed_or_none(p: *mut PyObject) -> PyRef {
    if p.is_null() { none::none_ref() } else { unsafe { (*p).to_ref() } }
}

/// Call `tp(*ints, *extra, **{"fold": fold})`.
unsafe fn construct(tp: *mut PyTypeObject, nums: &[c_int], extra: Option<*mut PyObject>, fold: Option<c_int>) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let mut owned = ints(nums);
            if let Some(p) = extra {
                owned.push(borrowed_or_none(p));
            }
            let refs: Vec<&PyObject> = owned.iter().map(|r| &**r).collect();
            let callable = type_ref(tp);
            match fold {
                None => ab::call(&callable, &refs),
                Some(f) => {
                    let kw = dict::new_dict();
                    dict::set_item_str(&kw, "fold", &int::new_int(f as i64))?;
                    ab::call_with_kwargs_dict(&callable, &refs, Some(&kw))
                }
            }
        })())
    }
}

unsafe extern "C" fn capi_date(y: c_int, m: c_int, d: c_int, tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe { construct(tp, &[y, m, d], None, None) }
}
unsafe extern "C" fn capi_datetime(
    y: c_int,
    mo: c_int,
    d: c_int,
    h: c_int,
    mi: c_int,
    s: c_int,
    us: c_int,
    tz: *mut PyObject,
    tp: *mut PyTypeObject,
) -> *mut PyObject {
    unsafe { construct(tp, &[y, mo, d, h, mi, s, us], Some(tz), None) }
}
unsafe extern "C" fn capi_datetime_fold(
    y: c_int,
    mo: c_int,
    d: c_int,
    h: c_int,
    mi: c_int,
    s: c_int,
    us: c_int,
    tz: *mut PyObject,
    fold: c_int,
    tp: *mut PyTypeObject,
) -> *mut PyObject {
    unsafe { construct(tp, &[y, mo, d, h, mi, s, us], Some(tz), Some(fold)) }
}
unsafe extern "C" fn capi_time(h: c_int, mi: c_int, s: c_int, us: c_int, tz: *mut PyObject, tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe { construct(tp, &[h, mi, s, us], Some(tz), None) }
}
unsafe extern "C" fn capi_time_fold(
    h: c_int,
    mi: c_int,
    s: c_int,
    us: c_int,
    tz: *mut PyObject,
    fold: c_int,
    tp: *mut PyTypeObject,
) -> *mut PyObject {
    unsafe { construct(tp, &[h, mi, s, us], Some(tz), Some(fold)) }
}
unsafe extern "C" fn capi_delta(d: c_int, s: c_int, us: c_int, _normalize: c_int, tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe { construct(tp, &[d, s, us], None, None) }
}
unsafe extern "C" fn capi_timezone(offset: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let tp = type_ref(timezone_type());
            let off = borrowed_or_none(offset);
            if name.is_null() {
                ab::call(&tp, &[&off])
            } else {
                ab::call(&tp, &[&off, &*name])
            }
        })())
    }
}
unsafe extern "C" fn capi_datetime_ts(cls: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let f = ab::getattr_str(&*cls, "fromtimestamp")?;
            let k = if kw.is_null() { None } else { Some(&*kw) };
            ab::call_tp_call(&f, &*args, k)
        })())
    }
}
unsafe extern "C" fn capi_date_ts(cls: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let f = ab::getattr_str(&*cls, "fromtimestamp")?;
            ab::call_tp_call(&f, &*args, None)
        })())
    }
}

fn capi_table() -> *mut DateTimeCapi {
    let p = CAPI.load(Ordering::Acquire);
    if !p.is_null() {
        return p;
    }
    let t = Box::into_raw(Box::new(DateTimeCapi {
        date_type: date_type(),
        datetime_type: datetime_type(),
        time_type: time_type(),
        delta_type: delta_type(),
        tzinfo_type: tzinfo_type(),
        timezone_utc: UTC.load(Ordering::Acquire),
        date_from_date: capi_date,
        datetime_from_date_and_time: capi_datetime,
        time_from_time: capi_time,
        delta_from_delta: capi_delta,
        timezone_from_timezone: capi_timezone,
        datetime_from_timestamp: capi_datetime_ts,
        date_from_timestamp: capi_date_ts,
        datetime_from_date_and_time_and_fold: capi_datetime_fold,
        time_from_time_and_fold: capi_time_fold,
    }));
    match CAPI.compare_exchange(std::ptr::null_mut(), t, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => t,
        Err(existing) => {
            unsafe { drop(Box::from_raw(t)) };
            existing
        }
    }
}

// ===================================================================== module

static INIT: std::sync::Once = std::sync::Once::new();

unsafe fn setup_type(
    tp: *mut PyTypeObject,
    name: &'static std::ffi::CStr,
    size: usize,
    base: *mut PyTypeObject,
    subclassable: bool,
    new: newfunc,
    dealloc: unsafe extern "C" fn(*mut PyObject),
    getset: *mut PyGetSetDef,
) -> PyResult<()> {
    unsafe {
        let t = &mut *tp;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | if subclassable { Py_TPFLAGS_BASETYPE } else { 0 };
        if !base.is_null() {
            t.tp_base = base;
        }
        t.tp_new = new;
        t.tp_dealloc = Some(dealloc);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = getset;
        type_ready(tp)?;
        Ok(())
    }
}

fn init_types() -> PyResult<()> {
    unsafe {
        (*delta_type()).tp_methods = mdefs![
            ("total_seconds", NOARGS, m_total_seconds, "total_seconds($self, /)\n--\n\nThe duration expressed in seconds, as a float."),
        ];
        (*date_type()).tp_methods = mdefs![
            ("isocalendar", NOARGS, m_isocalendar, "isocalendar($self, /)\n--\n\nThe ISO 8601 (year, week, weekday) of this date as a named tuple."),
        ];
        setup_type(
            tzinfo_type(),
            c"datetime.tzinfo",
            std::mem::size_of::<PyObject>(),
            std::ptr::null_mut(),
            true,
            Some(crate::types::object_type::object_new),
            plain_dealloc,
            std::ptr::null_mut(),
        )?;
        setup_type(
            timezone_type(),
            c"datetime.timezone",
            std::mem::size_of::<TimezoneObj>(),
            tzinfo_type(),
            false,
            Some(timezone_new),
            timezone_dealloc,
            crate::getset_table![("_offset", tz_offset), ("_name", tz_name)],
        )?;
        setup_type(
            delta_type(),
            c"datetime.timedelta",
            std::mem::size_of::<DeltaObj>(),
            std::ptr::null_mut(),
            true,
            Some(delta_new),
            plain_dealloc,
            crate::getset_table![
                ("_days", g_days),
                ("_seconds", g_seconds),
                ("_microseconds", g_microseconds),
                ("_hashcode", get_hashcode, set_hashcode),
            ],
        )?;
        setup_type(
            date_type(),
            c"datetime.date",
            std::mem::size_of::<DateObj>(),
            std::ptr::null_mut(),
            true,
            Some(date_new),
            plain_dealloc,
            crate::getset_table![
                ("_year", g_year),
                ("_month", g_month),
                ("_day", g_day),
                ("_hashcode", get_hashcode, set_hashcode),
            ],
        )?;
        setup_type(
            time_type(),
            c"datetime.time",
            std::mem::size_of::<TimeObj>(),
            std::ptr::null_mut(),
            true,
            Some(time_new),
            time_dealloc,
            crate::getset_table![
                ("_hour", t_hour),
                ("_minute", t_minute),
                ("_second", t_second),
                ("_microsecond", t_microsecond),
                ("_tzinfo", t_tzinfo),
                ("_fold", t_fold),
                ("_hashcode", get_hashcode, set_hashcode),
            ],
        )?;
        setup_type(
            datetime_type(),
            c"datetime.datetime",
            std::mem::size_of::<DateTimeObj>(),
            date_type(),
            true,
            Some(datetime_new),
            datetime_dealloc,
            crate::getset_table![
                ("_hour", dt_hour),
                ("_minute", dt_minute),
                ("_second", dt_second),
                ("_microsecond", dt_microsecond),
                ("_tzinfo", dt_tzinfo),
                ("_fold", dt_fold),
            ],
        )?;
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| {
        res = init_types().and_then(|_| build_namespace());
    });
    res?;
    if NS.load(Ordering::Acquire).is_null() {
        raise!(ImportError, "_datetime failed to initialize");
    }
    let m = crate::types::module::new_module("_datetime")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Fast implementation of the datetime module."))?;
    for (name, tp) in [
        ("date", date_type()),
        ("datetime", datetime_type()),
        ("time", time_type()),
        ("timedelta", delta_type()),
        ("timezone", timezone_type()),
        ("tzinfo", tzinfo_type()),
    ] {
        dict::set_item_str(&d, name, &type_ref(tp))?;
    }
    dict::set_item_str(&d, "MINYEAR", &int::new_int(1))?;
    dict::set_item_str(&d, "MAXYEAR", &int::new_int(9999))?;
    let utc = unsafe { (*UTC.load(Ordering::Acquire)).to_ref() };
    dict::set_item_str(&d, "UTC", &utc)?;
    let cap = unsafe {
        crate::capi::capsule::PyCapsule_New(capi_table() as *mut c_void, c"datetime.datetime_CAPI".as_ptr(), None)
    };
    if cap.is_null() {
        return Err(PyErr);
    }
    let cap = unsafe { PyRef::from_raw(cap) };
    dict::set_item_str(&d, "datetime_CAPI", &cap)?;
    let w = crate::types::weakref::new_ref(crate::types::weakref::ref_type(), &m, None)?;
    keep(&MODREF, w);
    Ok(m)
}
