//! `_zoneinfo`: native `zoneinfo.ZoneInfo` (tail2).
//!
//! A zone is loaded from TZif data (the binary file is decoded by the pure
//! `zoneinfo._common.load_data`, as the C accelerator of CPython does); the
//! transition tables, the POSIX TZ footer rule and all offset lookups live
//! here.  Caching differs from the pure class the way CPython's accelerator
//! does: the base class keeps its weak cache and its small LRU of strong
//! references in module state (there is no `ZoneInfo._weak_cache`), while
//! subclasses get a `_weak_cache` attribute from `__init_subclass__`.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::_datetime as dtm;
use crate::object::*;
use crate::raise;
use crate::subinterp::InterpPtr;
use crate::types::typeobject::{is_subtype, type_obj, type_ref};
use crate::types::{bool_, bytes, dict, list, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};
use std::sync::atomic::Ordering::*;

const SECONDS_PER_DAY: i64 = 86400;
/// Day number of 1970-01-01 counted from 0001-01-01 = day 1.
const EPOCH_ORDINAL: i64 = 719_163;
/// How many recently constructed base-class zones are kept alive.
const STRONG_CACHE_SIZE: usize = 8;

/// Where an instance came from (decides what `__reduce__` does).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Source {
    Cache = 0,
    NoCache = 1,
    File = 2,
}

#[repr(C)]
struct ZoneInfo {
    ob_base: PyObject,
    key: *mut PyObject,
    file_repr: *mut PyObject,
    weakreflist: *mut PyObject,
    data: *mut Zone,
    source: u8,
}

static mut ZONEINFO_TYPE: PyTypeObject = PyTypeObject::ZERO;
static WEAK_CACHE: InterpPtr = InterpPtr::new();
/// list of [key, zone] pairs, least recently used first
static STRONG_CACHE: InterpPtr = InterpPtr::new();

fn zi_type() -> *mut PyTypeObject {
    &raw mut ZONEINFO_TYPE
}

fn zi(o: &PyObject) -> &mut ZoneInfo {
    unsafe { &mut *(o.as_ptr() as *mut ZoneInfo) }
}

// ------------------------------------------------------------------ zone data

/// One local time type.
struct TType {
    offset: i64,
    dst: i64,
    utcoff: PyRef,
    dstoff: PyRef,
    name: PyRef,
}

impl TType {
    fn new(offset: i64, dst: i64, name: PyRef) -> PyResult<TType> {
        Ok(TType {
            offset,
            dst,
            utcoff: dtm::timedelta_from_seconds(offset)?,
            dstoff: dtm::timedelta_from_seconds(dst)?,
            name,
        })
    }
    fn same(&self, other: &TType) -> bool {
        self.offset == other.offset
            && self.dst == other.dst
            && pystr::str_eq(&self.name, &other.name)
    }
}

#[derive(Clone, Copy)]
enum DaySpec {
    /// Jn: 1..=365, February 29 never counted
    Julian(i64),
    /// n: 0..=365, February 29 counted
    Zero(i64),
    /// Mm.w.d
    Month(i64, i64, i64),
}

/// Daylight saving time from a POSIX TZ string.
struct Rule {
    std: TType,
    dst: TType,
    start: (DaySpec, i64),
    end: (DaySpec, i64),
}

enum After {
    Fixed(TType),
    Rule(Rule),
}

struct Zone {
    types: Vec<TType>,
    trans_utc: Vec<i64>,
    /// type index of each transition
    trans_type: Vec<usize>,
    /// wall-clock times of the transitions for fold=0 / fold=1
    trans_wall: [Vec<i64>; 2],
    before: Option<usize>,
    after: After,
    fixed: bool,
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

const DAYS_BEFORE_MONTH: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const DAYS_IN_MONTH: [i64; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn epoch_days(year: i64, month: i64, day: i64) -> i64 {
    let y = year - 1;
    let mut days = y * 365 + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400);
    days += DAYS_BEFORE_MONTH[month as usize] + (month > 2 && is_leap(year)) as i64;
    days + day - EPOCH_ORDINAL
}

fn day_of(spec: DaySpec, year: i64) -> i64 {
    match spec {
        DaySpec::Julian(n) => {
            let extra = (is_leap(year) && n >= 60) as i64;
            epoch_days(year, 1, 1) + n - 1 + extra
        }
        DaySpec::Zero(n) => epoch_days(year, 1, 1) + n,
        DaySpec::Month(month, week, weekday) => {
            let first = epoch_days(year, month, 1);
            let first_weekday = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday
            let mut day = 1 + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
            let len = DAYS_IN_MONTH[month as usize] + (month == 2 && is_leap(year)) as i64;
            while day > len {
                day -= 7;
            }
            first + day - 1
        }
    }
}

/// (UTC seconds, enters DST, offset before, offset after)
type Event = (i64, bool, i64, i64);

impl Rule {
    fn events(&self, year: i64) -> Vec<Event> {
        let (std, dst) = (self.std.offset, self.dst.offset);
        let mut ev = Vec::with_capacity(6);
        for y in [year - 1, year, year + 1] {
            let start = day_of(self.start.0, y) * SECONDS_PER_DAY + self.start.1 - std;
            let end = day_of(self.end.0, y) * SECONDS_PER_DAY + self.end.1 - dst;
            ev.push((start, true, std, dst));
            ev.push((end, false, dst, std));
        }
        // simultaneous changes: the end sorts first, so DST carries on
        ev.sort_by_key(|e| (e.0, e.1));
        ev
    }
    fn pick(&self, dst: bool) -> &TType {
        if dst { &self.dst } else { &self.std }
    }
    fn info_local(&self, ts: i64, year: i64, fold: bool) -> &TType {
        let ev = self.events(year);
        let mut chosen = None;
        for &(utc, enters, before, after) in &ev {
            let wall = utc + if fold { before.min(after) } else { before.max(after) };
            if wall <= ts {
                chosen = Some(enters);
            }
        }
        self.pick(chosen.unwrap_or(!ev[0].1))
    }
    fn info_utc(&self, ts: i64, year: i64) -> (&TType, bool) {
        let ev = self.events(year);
        match ev.iter().rev().find(|e| e.0 <= ts) {
            None => (self.pick(!ev[0].1), false),
            Some(&(utc, enters, before, after)) => {
                let fold = before > after && ts - utc < before - after;
                (self.pick(enters), fold)
            }
        }
    }
}

// ------------------------------------------------------------- TZ strings

struct TzParser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl TzParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }
    fn at_end(&self) -> bool {
        self.pos == self.s.len()
    }
    fn expect(&mut self, c: u8) -> Option<()> {
        if self.peek() != Some(c) {
            return None;
        }
        self.pos += 1;
        Some(())
    }
    fn digits(&mut self, least: usize, most: usize) -> Option<i64> {
        let start = self.pos;
        while self.pos - start < most && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos - start < least {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.pos]).ok()?.parse().ok()
    }
    fn name(&mut self) -> Option<String> {
        if self.peek() == Some(b'<') {
            let rest = &self.s[self.pos + 1..];
            let close = rest.iter().position(|&c| c == b'>')?;
            let inner = &rest[..close];
            if inner.is_empty() || !inner.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'-') {
                return None;
            }
            self.pos += close + 2;
            return Some(String::from_utf8_lossy(inner).into_owned());
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos - start < 3 {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.pos]).into_owned())
    }
    /// [+-]hh[:mm[:ss]] as signed seconds
    fn clock(&mut self, max_hours: i64) -> Option<i64> {
        let mut sign = 1;
        if let Some(c @ (b'+' | b'-')) = self.peek() {
            sign = if c == b'-' { -1 } else { 1 };
            self.pos += 1;
        }
        let hours = self.digits(1, if max_hours > 99 { 3 } else { 2 })?;
        if hours > max_hours {
            return None;
        }
        let (mut minutes, mut seconds) = (0, 0);
        if self.peek() == Some(b':') {
            self.pos += 1;
            minutes = self.digits(2, 2)?;
            if self.peek() == Some(b':') {
                self.pos += 1;
                seconds = self.digits(2, 2)?;
            }
        }
        if minutes > 59 || seconds > 59 {
            return None;
        }
        Some(sign * (hours * 3600 + minutes * 60 + seconds))
    }
    fn offset_ahead(&self) -> bool {
        self.peek().is_some_and(|c| c == b'+' || c == b'-' || c.is_ascii_digit())
    }
    fn rule(&mut self) -> Option<(DaySpec, i64)> {
        let spec = match self.peek() {
            Some(b'J') => {
                self.pos += 1;
                let n = self.digits(1, 3)?;
                if !(1..=365).contains(&n) {
                    return None;
                }
                DaySpec::Julian(n)
            }
            Some(b'M') => {
                self.pos += 1;
                let month = self.digits(1, 2)?;
                self.expect(b'.')?;
                let week = self.digits(1, 1)?;
                self.expect(b'.')?;
                let weekday = self.digits(1, 1)?;
                if !((1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6) {
                    return None;
                }
                DaySpec::Month(month, week, weekday)
            }
            _ => {
                let n = self.digits(1, 3)?;
                if n > 365 {
                    return None;
                }
                DaySpec::Zero(n)
            }
        };
        let mut at = 2 * 3600;
        if self.peek() == Some(b'/') {
            self.pos += 1;
            at = self.clock(167)?;
        }
        Some((spec, at))
    }
}

enum Footer {
    Fixed(i64, String),
    Dst { std: (i64, String), dst: (i64, String), start: (DaySpec, i64), end: (DaySpec, i64) },
}

fn parse_footer(s: &[u8]) -> Option<Footer> {
    let mut p = TzParser { s, pos: 0 };
    let std_name = p.name()?;
    let std_offset = -p.clock(24)?; // POSIX offsets count westwards
    if p.at_end() {
        return Some(Footer::Fixed(std_offset, std_name));
    }
    let dst_name = p.name()?;
    let mut dst_offset = std_offset + 3600;
    if p.offset_ahead() {
        dst_offset = -p.clock(24)?;
    }
    p.expect(b',')?;
    let start = p.rule()?;
    p.expect(b',')?;
    let end = p.rule()?;
    if !p.at_end() {
        return None;
    }
    Some(Footer::Dst { std: (std_offset, std_name), dst: (dst_offset, dst_name), start, end })
}

fn footer_after(raw: &PyObject) -> PyResult<After> {
    let data = bytes::bytes_data(raw).to_vec();
    match parse_footer(&data) {
        Some(Footer::Fixed(off, name)) => Ok(After::Fixed(TType::new(off, 0, pystr::new_str(&name))?)),
        Some(Footer::Dst { std, dst, start, end }) => Ok(After::Rule(Rule {
            std: TType::new(std.0, 0, pystr::new_str(&std.1))?,
            dst: TType::new(dst.0, dst.0 - std.0, pystr::new_str(&dst.1))?,
            start,
            end,
        })),
        None => {
            let text = String::from_utf8_lossy(&data).into_owned();
            let r = ab::repr(raw)?;
            raise!(ValueError, "Invalid TZ string: {} (file footer {})", text, pystr::to_string_lossy(&r))
        }
    }
}

// ------------------------------------------------------------------ loading

fn int_vec(o: &PyObject) -> PyResult<Vec<i64>> {
    let t = ab::sequence_tuple(o)?;
    tuple::tuple_items(&t).iter().map(|x| ab::as_i64(x)).collect()
}

/// DST amount of each local time type: the difference to the standard-time
/// type next to it in the transition sequence, one hour when that cannot be told.
fn dst_amounts(idx: &[usize], offsets: &[i64], isdst: &[bool]) -> Vec<i64> {
    let mut out = vec![0i64; offsets.len()];
    for (pos, &k) in idx.iter().enumerate() {
        if !isdst[k] || out[k] != 0 {
            continue;
        }
        let mut amount = 0;
        if pos > 0 && !isdst[idx[pos - 1]] {
            amount = offsets[k] - offsets[idx[pos - 1]];
        }
        if amount == 0 && pos + 1 < idx.len() && !isdst[idx[pos + 1]] {
            amount = offsets[k] - offsets[idx[pos + 1]];
        }
        out[k] = amount;
    }
    for (k, &d) in isdst.iter().enumerate() {
        if d && out[k] == 0 {
            out[k] = 3600;
        }
    }
    out
}

fn import(name: &str) -> PyResult {
    let fromlist = tuple::new_tuple(vec![pystr::new_str("__doc__")]);
    crate::import::import_module_level(&pystr::new_str(name), None, &fromlist, 0)
}

fn load_zone(file_obj: &PyObject) -> PyResult<Zone> {
    let common = import("zoneinfo._common")?;
    let r = ab::call_method_str(&common, "load_data", &[file_obj])?;
    let r = ab::sequence_tuple(&r)?;
    let items = tuple::tuple_items(&r);
    if items.len() != 6 {
        raise!(ValueError, "load_data() must return 6 items");
    }
    let idx: Vec<usize> = int_vec(items[0])?.into_iter().map(|v| v as usize).collect();
    let trans_utc = int_vec(items[1])?;
    let offsets = int_vec(items[2])?;
    let isdst: Vec<bool> = int_vec(items[3])?.into_iter().map(|v| v != 0).collect();
    let abbrs_t = ab::sequence_tuple(items[4])?;
    let abbrs = tuple::tuple_items(&abbrs_t);
    let tz_str = items[5];
    if idx.len() != trans_utc.len()
        || isdst.len() != offsets.len()
        || abbrs.len() != offsets.len()
        || idx.iter().any(|&i| i >= offsets.len())
    {
        raise!(ValueError, "inconsistent TZif data");
    }
    let dst = dst_amounts(&idx, &offsets, &isdst);
    let mut types = Vec::with_capacity(offsets.len());
    for i in 0..offsets.len() {
        types.push(TType::new(offsets[i], dst[i], abbrs[i].to_ref())?);
    }
    // local time before the first transition: the first standard-time type
    let before = (0..types.len()).find(|&i| !isdst[i]).or(if types.is_empty() { None } else { Some(0) });
    let has_footer = !tz_str.is(none::none()) && ab::is_true(tz_str)?;
    let after = if has_footer {
        footer_after(tz_str)?
    } else if let Some(&last) = idx.last() {
        After::Fixed(TType::new(types[last].offset, dst[last], types[last].name.clone())?)
    } else if !types.is_empty() {
        After::Fixed(TType::new(types[0].offset, dst[0], types[0].name.clone())?)
    } else {
        raise!(ValueError, "No time zone information found.");
    };
    // wall-clock times of each transition: a skipped or repeated stretch belongs to
    // the earlier offset for fold=0 and to the later one for fold=1
    let mut w0 = Vec::with_capacity(trans_utc.len());
    let mut w1 = Vec::with_capacity(trans_utc.len());
    let mut prev = before.map(|b| types[b].offset);
    for (&utc, &k) in trans_utc.iter().zip(&idx) {
        let after_off = types[k].offset;
        let before_off = prev.unwrap_or(after_off);
        w0.push(utc + before_off.max(after_off));
        w1.push(utc + before_off.min(after_off));
        prev = Some(after_off);
    }
    let fixed = match &after {
        After::Rule(_) => false,
        After::Fixed(t) => types.iter().all(|x| x.same(t)),
    };
    Ok(Zone { types, trans_utc, trans_type: idx, trans_wall: [w0, w1], before, after, fixed })
}

fn bisect_right(v: &[i64], x: i64) -> usize {
    v.partition_point(|&e| e <= x)
}

impl Zone {
    fn ttype(&self, i: usize) -> &TType {
        &self.types[self.trans_type[i]]
    }
    fn before_type(&self) -> Option<&TType> {
        self.before.map(|b| &self.types[b])
    }
    /// The type for a wall time (None: no answer without a date).
    fn info_local(&self, wall: Option<(i64, i64, bool)>) -> Option<&TType> {
        let Some((ts, year, fold)) = wall else {
            return match (&self.after, self.fixed) {
                (After::Fixed(t), true) => Some(t),
                _ => None,
            };
        };
        let walls = &self.trans_wall[fold as usize];
        if let (Some(&first), Some(&last)) = (walls.first(), walls.last()) {
            if ts < first {
                return self.before_type();
            }
            if ts < last {
                return Some(self.ttype(bisect_right(walls, ts) - 1));
            }
        }
        match &self.after {
            After::Rule(r) => Some(r.info_local(ts, year, fold)),
            After::Fixed(t) => Some(t),
        }
    }
    /// Is UTC time `ts` in the repeated stretch after transition `index - 1`?
    fn fold_after(&self, index: usize, ts: i64) -> bool {
        let cur = self.ttype(index - 1).offset;
        let prev = if index >= 2 {
            self.ttype(index - 2).offset
        } else {
            self.before_type().map_or(cur, |t| t.offset)
        };
        prev - cur > ts - self.trans_utc[index - 1]
    }
    fn info_utc(&self, ts: i64, year: i64) -> (&TType, bool) {
        let n = self.trans_utc.len();
        let index = bisect_right(&self.trans_utc, ts);
        if n > 0 && index == 0 {
            return (self.before_type().unwrap_or(self.ttype(0)), false);
        }
        if n == 0 || ts > self.trans_utc[n - 1] {
            let (t, mut fold) = match &self.after {
                After::Rule(r) => r.info_utc(ts, year),
                After::Fixed(t) => (t, false),
            };
            if n > 0 && !fold {
                fold = self.fold_after(n, ts);
            }
            return (t, fold);
        }
        (self.ttype(index - 1), self.fold_after(index, ts))
    }
}

// ------------------------------------------------------------------ objects

fn zone_of(o: &PyObject) -> PyResult<&Zone> {
    let p = zi(o).data;
    if p.is_null() {
        raise!(ValueError, "ZoneInfo object is not initialized");
    }
    Ok(unsafe { &*p })
}

fn alloc_zone(cls: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*cls).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(cls, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let z = &mut *(op as *mut ZoneInfo);
        z.key = none::none_ref().into_raw();
        z.file_repr = std::ptr::null_mut();
        z.data = std::ptr::null_mut();
        z.source = Source::NoCache as u8;
        Ok(PyRef::from_raw(op))
    }
}

fn set_key(o: &PyObject, key: &PyObject) {
    let z = zi(o);
    let old = std::mem::replace(&mut z.key, key.to_ref().into_raw());
    unsafe { xdecref(old) };
}

fn install(o: &PyObject, zone: Zone) {
    zi(o).data = Box::into_raw(Box::new(zone));
}

/// A fresh zone for `key` from TZPATH or the tzdata package (never cached).
fn new_instance(cls: *mut PyTypeObject, key: &PyObject) -> PyResult {
    let tzpath = import("zoneinfo._tzpath")?;
    let path = ab::call_method_str(&tzpath, "find_tzfile", &[key])?;
    let file_obj = if path.is(none::none()) {
        let common = import("zoneinfo._common")?;
        ab::call_method_str(&common, "load_tzdata", &[key])?
    } else {
        let io = import("io")?;
        ab::call_method_str(&io, "open", &[&path, &pystr::new_str("rb")])?
    };
    let loaded = load_zone(&file_obj);
    let closed = ab::call_method_str(&file_obj, "close", &[]);
    let zone = loaded?;
    closed?;
    let o = alloc_zone(cls)?;
    set_key(&o, key);
    install(&o, zone);
    Ok(o)
}

fn weak_cache_for(cls: *mut PyTypeObject) -> PyResult {
    if cls == zi_type() {
        let p = WEAK_CACHE.load(Acquire);
        if !p.is_null() {
            return Ok(unsafe { (*p).to_ref() });
        }
        let weakref = import("weakref")?;
        let wc = ab::call_method_str(&weakref, "WeakValueDictionary", &[])?;
        match WEAK_CACHE.compare_exchange(std::ptr::null_mut(), wc.to_ref().into_raw(), AcqRel, Acquire) {
            Ok(_) => Ok(wc),
            Err(cur) => {
                unsafe { decref(wc.to_ref().into_raw()) };
                Ok(unsafe { (*cur).to_ref() })
            }
        }
    } else {
        ab::getattr_str(type_obj(cls), "_weak_cache")
    }
}

fn strong_cache() -> PyRef {
    let p = STRONG_CACHE.load(Acquire);
    if !p.is_null() {
        return unsafe { (*p).to_ref() };
    }
    let l = list::new_list(Vec::new());
    match STRONG_CACHE.compare_exchange(std::ptr::null_mut(), l.to_ref().into_raw(), AcqRel, Acquire) {
        Ok(_) => l,
        Err(cur) => {
            unsafe { decref(l.to_ref().into_raw()) };
            unsafe { (*cur).to_ref() }
        }
    }
}

/// Position of `key` in the strong cache (keys compare with ==).
fn strong_find(cache: &PyObject, key: &PyObject) -> PyResult<Option<usize>> {
    let entries = list::list_to_vec(cache);
    for (i, e) in entries.iter().enumerate() {
        let k = tuple::tuple_get(e, 0);
        if k.is(key) || ab::rich_compare_bool(k, key, Py_EQ)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn strong_remove(cache: &PyObject, key: &PyObject) -> PyResult<()> {
    if let Some(i) = strong_find(cache, key)? {
        ab::call_method_str(cache, "pop", &[&crate::types::int::new_int(i as i64)])?;
    }
    Ok(())
}

fn strong_get(cls: *mut PyTypeObject, key: &PyObject) -> PyResult<Option<PyRef>> {
    if cls != zi_type() {
        return Ok(None);
    }
    let cache = strong_cache();
    let Some(i) = strong_find(&cache, key)? else { return Ok(None) };
    let e = ab::call_method_str(&cache, "pop", &[&crate::types::int::new_int(i as i64)])?;
    let zone = tuple::tuple_get(&e, 1).to_ref();
    list::append(&cache, e.clone());
    Ok(Some(zone))
}

fn strong_put(cls: *mut PyTypeObject, key: &PyObject, zone: &PyObject) -> PyResult<()> {
    if cls != zi_type() {
        return Ok(());
    }
    let cache = strong_cache();
    strong_remove(&cache, key)?;
    list::append(&cache, tuple::new_tuple(vec![key.to_ref(), zone.to_ref()]));
    while list::list_len(&cache) > STRONG_CACHE_SIZE {
        ab::call_method_str(&cache, "pop", &[&crate::types::int::new_int(0)])?;
    }
    Ok(())
}

fn type_name(tp: *mut PyTypeObject) -> String {
    ab::getattr_str(type_obj(tp), "__name__").map(|n| pystr::to_string_lossy(&n)).unwrap_or_default()
}

fn zi_new_impl(cls: *mut PyTypeObject, key: &PyObject) -> PyResult {
    if let Some(z) = strong_get(cls, key)? {
        return Ok(z);
    }
    let weak = weak_cache_for(cls)?;
    let mut zone = ab::call_method_str(&weak, "get", &[key, none::none()])?;
    if zone.is(none::none()) {
        let fresh = new_instance(cls, key)?;
        zi(&fresh).source = Source::Cache as u8;
        zone = ab::call_method_str(&weak, "setdefault", &[key, &fresh])?;
    }
    if !is_subtype(zone.type_ptr(), cls) {
        let r = ab::repr(key)?;
        raise!(
            RuntimeError,
            "Unexpected instance of {} in {} weak cache for key {}",
            type_name(zone.type_ptr()),
            type_name(cls),
            pystr::to_string_lossy(&r)
        );
    }
    strong_put(cls, key, &zone)?;
    Ok(zone)
}

unsafe extern "C" fn zi_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            static P: crate::args::Params = crate::args::Params::new("ZoneInfo", &["key"], 0, 1);
            let (vals, names) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|v| &**v).collect();
            let [key] = crate::args::parse::<1>(&P, &refs, names.as_deref())?;
            zi_new_impl(tp, key.unwrap())
        })())
    }
}

unsafe extern "C" fn zi_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        crate::types::weakref::clear_weakrefs(op);
        let z = &mut *(op as *mut ZoneInfo);
        xdecref(std::mem::replace(&mut z.key, std::ptr::null_mut()));
        xdecref(std::mem::replace(&mut z.file_repr, std::ptr::null_mut()));
        let d = std::mem::replace(&mut z.data, std::ptr::null_mut());
        if !d.is_null() {
            drop(Box::from_raw(d));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

unsafe extern "C" fn zi_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let z = &*(op as *const ZoneInfo);
        for p in [z.key, z.file_repr] {
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if crate::types::typeobject::is_heap_type((*op).type_ptr()) {
            return visit.unwrap()((*op).type_ptr() as *mut PyObject, arg);
        }
        0
    }
}

unsafe extern "C" fn zi_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let z = &mut *(op as *mut ZoneInfo);
        xdecref(std::mem::replace(&mut z.key, none::none_ref().into_raw()));
        xdecref(std::mem::replace(&mut z.file_repr, std::ptr::null_mut()));
        0
    }
}

fn cls_ptr(cls: &PyObject) -> *mut PyTypeObject {
    cls.as_ptr() as *mut PyTypeObject
}

fn zi_no_cache(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("no_cache", &["key"], 0, 1);
    let [key] = crate::args::parse::<1>(&P, args, kw)?;
    let o = new_instance(cls_ptr(cls), key.unwrap())?;
    zi(&o).source = Source::NoCache as u8;
    Ok(o)
}

fn zi_from_file(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("from_file", &["file_obj", "key"], 1, 1);
    let [file_obj, key] = crate::args::parse::<2>(&P, args, kw)?;
    let file_obj = file_obj.unwrap();
    let zone = load_zone(file_obj)?;
    let file_repr = ab::repr(file_obj)?;
    let o = alloc_zone(cls_ptr(cls))?;
    set_key(&o, key.unwrap_or(none::none()));
    install(&o, zone);
    let z = zi(&o);
    z.file_repr = file_repr.into_raw();
    z.source = Source::File as u8;
    Ok(o)
}

fn zi_clear_cache(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("clear_cache", &["only_keys"], 0, 0).kwonly(1);
    let [only_keys] = crate::args::parse::<1>(&P, args, kw)?;
    let tp = cls_ptr(cls);
    let weak = weak_cache_for(tp)?;
    match only_keys {
        None => {
            ab::call_method_str(&weak, "clear", &[])?;
            if tp == zi_type() {
                let cache = strong_cache();
                ab::call_method_str(&cache, "clear", &[])?;
            }
        }
        Some(k) if k.is(none::none()) => {
            ab::call_method_str(&weak, "clear", &[])?;
            if tp == zi_type() {
                let cache = strong_cache();
                ab::call_method_str(&cache, "clear", &[])?;
            }
        }
        Some(keys) => {
            let it = ab::get_iter(keys)?;
            while let Some(key) = ab::iter_next(&it)? {
                if tp == zi_type() {
                    strong_remove(&strong_cache(), &key)?;
                }
                ab::call_method_str(&weak, "pop", &[&key, none::none()])?;
            }
        }
    }
    Ok(none::none_ref())
}

fn zi_unpickle(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_unpickle", args, 2, 2)?;
    if ab::is_true(args[1])? {
        return zi_new_impl(cls_ptr(cls), args[0]);
    }
    let o = new_instance(cls_ptr(cls), args[0])?;
    zi(&o).source = Source::NoCache as u8;
    Ok(o)
}

fn zi_init_subclass(cls: &PyObject) -> PyResult {
    // every subclass caches its own instances
    let weakref = import("weakref")?;
    let wc = ab::call_method_str(&weakref, "WeakValueDictionary", &[])?;
    ab::setattr(cls, &pystr::new_str("_weak_cache"), &wc)?;
    Ok(none::none_ref())
}

fn zi_key(o: &PyObject) -> PyResult {
    let k = zi(o).key;
    Ok(if k.is_null() { none::none_ref() } else { unsafe { (*k).to_ref() } })
}

/// (seconds since the epoch of the wall time, year, fold) of a datetime argument.
fn wall_of(dt: &PyObject, what: &str) -> PyResult<Option<(i64, i64, bool)>> {
    if dt.is(none::none()) {
        return Ok(None);
    }
    if !dtm::is_datetime(dt) {
        raise!(TypeError, "{}() argument must be a datetime.datetime or None, not {}", what, crate::types::typeobject::tname(dt));
    }
    let (y, mo, d, h, mi, s, fold) = dtm::datetime_wall_fields(dt);
    let ts = epoch_days(y, mo as i64, d as i64) * SECONDS_PER_DAY + (h as i64) * 3600 + (mi as i64) * 60 + s as i64;
    Ok(Some((ts, y, fold != 0)))
}

fn zi_utcoffset(o: &PyObject, dt: &PyObject) -> PyResult {
    let w = wall_of(dt, "utcoffset")?;
    Ok(zone_of(o)?.info_local(w).map_or_else(none::none_ref, |t| t.utcoff.clone()))
}

fn zi_dst(o: &PyObject, dt: &PyObject) -> PyResult {
    let w = wall_of(dt, "dst")?;
    Ok(zone_of(o)?.info_local(w).map_or_else(none::none_ref, |t| t.dstoff.clone()))
}

fn zi_tzname(o: &PyObject, dt: &PyObject) -> PyResult {
    let w = wall_of(dt, "tzname")?;
    Ok(zone_of(o)?.info_local(w).map_or_else(none::none_ref, |t| t.name.clone()))
}

fn zi_fromutc(o: &PyObject, dt: &PyObject) -> PyResult {
    if !dtm::is_datetime(dt) {
        raise!(TypeError, "fromutc() requires a datetime argument");
    }
    if dtm::datetime_tzinfo_ptr(dt) != o.as_ptr() {
        raise!(ValueError, "dt.tzinfo is not self");
    }
    let (ts, year, _) = wall_of(dt, "fromutc")?.unwrap();
    let (t, fold) = zone_of(o)?.info_utc(ts, year);
    let local = ab::binary_op(dt, &t.utcoff, ab::NbSlot::Add)?;
    if fold {
        let replace = ab::getattr_str(&local, "replace")?;
        let names = tuple::new_tuple(vec![pystr::new_str("fold")]);
        return ab::call_kw(&replace, &[&crate::types::int::new_int(1)], Some(&names));
    }
    Ok(local)
}

fn zi_str(o: &PyObject) -> PyResult {
    let k = zi_key(o)?;
    if k.is(none::none()) {
        return zi_repr(o);
    }
    ab::str(&k)
}

fn zi_repr(o: &PyObject) -> PyResult {
    let name = type_name(o.type_ptr());
    let z = zi(o);
    if !z.key.is_null() && !unsafe { &*z.key }.is(none::none()) {
        let r = ab::repr(unsafe { &*z.key })?;
        return Ok(pystr::new_str(&format!("{}(key={})", name, pystr::to_string_lossy(&r))));
    }
    let fr = if z.file_repr.is_null() { "None".to_string() } else { pystr::to_string_lossy(unsafe { &*z.file_repr }) };
    Ok(pystr::new_str(&format!("{}.from_file({})", name, fr)))
}

fn zi_reduce(o: &PyObject) -> PyResult {
    let z = zi(o);
    if z.source == Source::File as u8 {
        let pickle = import("pickle")?;
        let cls = ab::getattr_str(&pickle, "PicklingError")?;
        let e = ab::call(&cls, &[&pystr::new_str("Cannot pickle a ZoneInfo file created from a file stream.")])?;
        set_exc(e);
        return Err(PyErr);
    }
    let cls = type_ref(o.type_ptr());
    let unpickle = ab::getattr_str(&cls, "_unpickle")?;
    let args = tuple::new_tuple(vec![zi_key(o)?, bool_::new_bool(z.source == Source::Cache as u8)]);
    Ok(tuple::new_tuple(vec![unpickle, args]))
}

unsafe extern "C" fn zi_str_slot(o: *mut PyObject) -> *mut PyObject {
    into_c(zi_str(unsafe { &*o }))
}

unsafe extern "C" fn zi_repr_slot(o: *mut PyObject) -> *mut PyObject {
    into_c(zi_repr(unsafe { &*o }))
}

pub fn init_module() -> PyResult {
    // the datetime types must exist (an import failure lets zoneinfo fall back
    // to the pure class)
    import("_datetime")?;
    unsafe {
        if ZONEINFO_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            let t = &mut *zi_type();
            t.tp_name = c"zoneinfo.ZoneInfo".as_ptr();
            t.tp_basicsize = std::mem::size_of::<ZoneInfo>() as isize;
            t.tp_base = dtm::tzinfo_type_ptr();
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
            t.tp_weaklistoffset = std::mem::offset_of!(ZoneInfo, weakreflist) as isize;
            t.tp_new = Some(zi_new);
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
            t.tp_dealloc = Some(zi_dealloc);
            t.tp_traverse = Some(zi_traverse);
            t.tp_clear = Some(zi_clear);
            t.tp_str = Some(zi_str_slot);
            t.tp_repr = Some(zi_repr_slot);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_doc = c"ZoneInfo(key)\n--\n\nIANA time zone loaded from TZif data, identified by its key.".as_ptr();
            t.tp_methods = crate::mdefs![
                ("utcoffset", O, zi_utcoffset, "utcoffset($self, dt, /)\n--\n\nOffset from UTC of the local time dt."),
                ("dst", O, zi_dst, "dst($self, dt, /)\n--\n\nDaylight saving time adjustment in effect at dt."),
                ("tzname", O, zi_tzname, "tzname($self, dt, /)\n--\n\nAbbreviated name of the local time type at dt."),
                ("fromutc", O, zi_fromutc, "fromutc($self, dt, /)\n--\n\nConvert dt (a UTC time with tzinfo=self) to local time."),
                ("__reduce__", NOARGS, zi_reduce, "__reduce__($self, /)\n--\n\nPickle by key."),
                ("no_cache", FASTKW, zi_no_cache, "no_cache($type, /, key)\n--\n\nLoad key afresh, bypassing (and not populating) the cache."; METH_CLASS),
                ("from_file", FASTKW, zi_from_file, "from_file($type, file_obj, /, key=None)\n--\n\nCreate a zone from an open binary TZif file (never cached)."; METH_CLASS),
                ("clear_cache", FASTKW, zi_clear_cache, "clear_cache($type, /, *, only_keys=None)\n--\n\nDrop cached zones (all of them, or those in only_keys)."; METH_CLASS),
                ("_unpickle", FAST, zi_unpickle, "_unpickle($type, key, from_cache, /)\n--\n\nRecreate a pickled zone."; METH_CLASS),
                ("__init_subclass__", NOARGS, zi_init_subclass, "__init_subclass__($type, /)\n--\n\nGive the subclass its own cache."; METH_CLASS),
            ];
            t.tp_getset = crate::getset_table![("key", zi_key; "The IANA key of the zone (None for zones read from a file without one).")];
            crate::types::typeobject::type_ready(zi_type())?;
        }
    }
    let m = crate::types::module::new_module("_zoneinfo")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "ZoneInfo", type_obj(zi_type()))?;
    Ok(m)
}
