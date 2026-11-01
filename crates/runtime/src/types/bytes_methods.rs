//! `bytes`: the type object, slots, all methods, %-formatting, plus the byte
//! string algorithms shared with `bytearray` (whose method table reuses the
//! method functions defined here; results are built with the type of `self`).
use crate::abstract_ as ab;
use crate::buffer::{self, BufferView};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::{tname, type_fqname};
use crate::types::{bool_, bytearray, bytes, int, none, slice, str as pystr, tuple};
use std::ffi::c_int;

// ====================================================================== searching

/// Crochemore–Perrin two-way string matching: O(n + m) worst case, O(1) extra space.
pub mod search {
    pub struct TwoWay<'a, T> {
        needle: &'a [T],
        crit: usize,
        period: usize,
        periodic: bool,
    }

    fn maximal_suffix<T: Ord>(arr: &[T], order_greater: bool) -> (usize, usize) {
        let mut left = 0;
        let mut right = 1;
        let mut offset = 0;
        let mut period = 1;
        while right + offset < arr.len() {
            let a = &arr[right + offset];
            let b = &arr[left + offset];
            if (a < b && !order_greater) || (a > b && order_greater) {
                right += offset + 1;
                offset = 0;
                period = right - left;
            } else if a == b {
                if offset + 1 == period {
                    right += offset + 1;
                    offset = 0;
                } else {
                    offset += 1;
                }
            } else {
                left = right;
                right += 1;
                offset = 0;
                period = 1;
            }
        }
        (left, period)
    }

    impl<'a, T: Ord + Copy> TwoWay<'a, T> {
        pub fn new(needle: &'a [T]) -> Self {
            let (c1, p1) = maximal_suffix(needle, false);
            let (c2, p2) = maximal_suffix(needle, true);
            let (crit, period) = if c1 > c2 { (c1, p1) } else { (c2, p2) };
            let periodic = period + crit <= needle.len() && needle[..crit] == needle[period..period + crit];
            let period = if periodic { period } else { crit.max(needle.len() - crit) + 1 };
            TwoWay { needle, crit, period, periodic }
        }
        /// First match at a position >= `start`.
        pub fn find_from(&self, hay: &[T], start: usize) -> Option<usize> {
            let n = self.needle;
            let m = n.len();
            if m == 0 {
                return if start <= hay.len() { Some(start) } else { None };
            }
            let mut pos = start;
            let mut memory = 0usize;
            while pos + m <= hay.len() {
                let mut i = if self.periodic { self.crit.max(memory) } else { self.crit };
                while i < m && n[i] == hay[pos + i] {
                    i += 1;
                }
                if i < m {
                    pos += i - self.crit + 1;
                    memory = 0;
                    continue;
                }
                let lo = if self.periodic { memory } else { 0 };
                let mut j = self.crit;
                while j > lo && n[j - 1] == hay[pos + j - 1] {
                    j -= 1;
                }
                if j <= lo {
                    return Some(pos);
                }
                pos += self.period;
                if self.periodic {
                    memory = m - self.period;
                }
            }
            None
        }
    }

    /// Generic forward search.
    pub fn find<T: Ord + Copy>(hay: &[T], needle: &[T]) -> Option<usize> {
        let m = needle.len();
        if m == 0 {
            return Some(0);
        }
        if m > hay.len() {
            return None;
        }
        if m == 1 {
            return hay.iter().position(|&c| c == needle[0]);
        }
        if m <= 8 {
            // short needle: anchor on the first element, verify; bounded cost.
            let first = needle[0];
            let last = hay.len() - m;
            let mut i = 0;
            while i <= last {
                match hay[i..=last].iter().position(|&c| c == first) {
                    None => return None,
                    Some(k) => {
                        i += k;
                        if hay[i..i + m] == *needle {
                            return Some(i);
                        }
                        i += 1;
                    }
                }
            }
            return None;
        }
        TwoWay::new(needle).find_from(hay, 0)
    }

    /// Generic backward search (last occurrence).
    pub fn rfind<T: Ord + Copy>(hay: &[T], needle: &[T]) -> Option<usize> {
        let m = needle.len();
        let n = hay.len();
        if m == 0 {
            return Some(n);
        }
        if m > n {
            return None;
        }
        if m == 1 {
            return hay.iter().rposition(|&c| c == needle[0]);
        }
        if m <= 8 {
            let first = needle[0];
            let mut i = n - m + 1;
            while i > 0 {
                i -= 1;
                if hay[i] == first && hay[i..i + m] == *needle {
                    return Some(i);
                }
            }
            return None;
        }
        let rh: Vec<T> = hay.iter().rev().copied().collect();
        let rn: Vec<T> = needle.iter().rev().copied().collect();
        TwoWay::new(&rn).find_from(&rh, 0).map(|p| n - p - m)
    }

    /// Count non-overlapping occurrences (at most `maxcount`).
    pub fn count<T: Ord + Copy>(hay: &[T], needle: &[T], maxcount: usize) -> usize {
        let m = needle.len();
        if m == 0 {
            return (hay.len() + 1).min(maxcount);
        }
        if m > hay.len() || maxcount == 0 {
            return 0;
        }
        let mut c = 0;
        if m == 1 {
            for &x in hay {
                if x == needle[0] {
                    c += 1;
                    if c == maxcount {
                        break;
                    }
                }
            }
            return c;
        }
        let tw = TwoWay::new(needle);
        let mut pos = 0;
        while let Some(p) = tw.find_from(hay, pos) {
            c += 1;
            if c == maxcount {
                break;
            }
            pos = p + m;
        }
        c
    }

    /// All match positions (non-overlapping, left to right), at most `maxcount`.
    pub fn find_all<T: Ord + Copy>(hay: &[T], needle: &[T], maxcount: usize) -> Vec<usize> {
        let m = needle.len();
        let mut v = Vec::new();
        if m == 0 || m > hay.len() || maxcount == 0 {
            return v;
        }
        if m == 1 {
            for (i, &x) in hay.iter().enumerate() {
                if x == needle[0] {
                    v.push(i);
                    if v.len() == maxcount {
                        break;
                    }
                }
            }
            return v;
        }
        let tw = TwoWay::new(needle);
        let mut pos = 0;
        while let Some(p) = tw.find_from(hay, pos) {
            v.push(p);
            if v.len() == maxcount {
                break;
            }
            pos = p + m;
        }
        v
    }
}

/// Fast byte search (memchr for single bytes).
pub fn bfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let m = needle.len();
    if m == 1 {
        return memchr(needle[0], hay);
    }
    if m >= 2 && m <= 8 && m <= hay.len() {
        let first = needle[0];
        let last = hay.len() - m;
        let mut i = 0;
        while i <= last {
            match memchr(first, &hay[i..=last]) {
                None => return None,
                Some(k) => {
                    i += k;
                    if &hay[i..i + m] == needle {
                        return Some(i);
                    }
                    i += 1;
                }
            }
        }
        return None;
    }
    search::find(hay, needle)
}
pub fn brfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() == 1 {
        return memrchr(needle[0], hay);
    }
    search::rfind(hay, needle)
}
pub fn memchr(c: u8, hay: &[u8]) -> Option<usize> {
    if hay.is_empty() {
        return None;
    }
    unsafe {
        let p = libc::memchr(hay.as_ptr() as *const libc::c_void, c as c_int, hay.len());
        if p.is_null() { None } else { Some(p as usize - hay.as_ptr() as usize) }
    }
}
pub fn memrchr(c: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().rposition(|&x| x == c)
}

// ====================================================================== ASCII classes

#[inline]
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}
#[inline]
fn is_lower(c: u8) -> bool {
    c.is_ascii_lowercase()
}
#[inline]
fn is_upper(c: u8) -> bool {
    c.is_ascii_uppercase()
}

// ====================================================================== self / results

/// Data of `self` (bytes or bytearray).
#[inline]
pub fn sdata(o: &PyObject) -> &[u8] {
    if bytes::is_bytes(o) { bytes::bytes_data(o) } else { bytearray::bytearray_data(o) }
}

/// Build a result of the same family as `o` (exact bytes or exact bytearray).
pub fn new_like(o: &PyObject, v: &[u8]) -> PyResult {
    if bytes::is_bytes(o) { bytes::try_new_bytes(v) } else { bytearray::new_bytearray(v) }
}

/// Return `o` itself when it is an exact bytes object and `v` is all of it,
/// otherwise a new object (bytearray results are always new).
fn self_or_new(o: &PyObject, start: usize, end: usize) -> PyResult {
    let d = sdata(o);
    if start == 0 && end == d.len() && bytes::is_bytes_exact(o) {
        return Ok(o.to_ref());
    }
    let v = d[start..end].to_vec();
    new_like(o, &v)
}

/// While alive, a bytearray `self` cannot be resized (methods that call back
/// into Python code during argument conversion use this, like CPython's
/// buffer export of self).
pub struct SelfGuard(*mut PyObject);
impl SelfGuard {
    pub fn new(o: &PyObject) -> SelfGuard {
        if bytearray::is_bytearray(o) {
            bytearray::add_export(o, 1);
            SelfGuard(o.as_ptr())
        } else {
            SelfGuard(std::ptr::null_mut())
        }
    }
}
impl Drop for SelfGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { bytearray::add_export(&*self.0, -1) };
        }
    }
}

// ====================================================================== argument helpers

/// A bytes-like argument (buffer held while alive).
pub enum BArg {
    Bytes(PyRef),
    Buf(BufferView),
}
impl BArg {
    pub fn data(&self) -> &[u8] {
        match self {
            BArg::Bytes(b) => bytes::bytes_data(b),
            BArg::Buf(v) => v.as_bytes(),
        }
    }
}

/// Acquire a simple buffer ("a bytes-like object is required, not 'str'").
pub fn barg(o: &PyObject) -> PyResult<BArg> {
    if bytes::is_bytes(o) {
        return Ok(BArg::Bytes(o.to_ref()));
    }
    Ok(BArg::Buf(BufferView::simple(o)?))
}

/// Py_ssize_t argument through __index__ (OverflowError on overflow).
pub fn ssize_arg(o: &PyObject) -> PyResult<isize> {
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

/// _PyEval_SliceIndex: None -> default, else __index__ clamped.
pub fn slice_index(o: Option<&PyObject>, default: isize) -> PyResult<isize> {
    match o {
        None => Ok(default),
        Some(x) if none::is_none(x) => Ok(default),
        Some(x) => {
            if !ab::has_index(x) {
                raise!(TypeError, "slice indices must be integers or None or have an __index__ method");
            }
            ab::index_as_ssize(x, None)
        }
    }
}

#[inline]
pub fn adjust_indices(start: isize, end: isize, len: isize) -> (isize, isize) {
    let mut s = start;
    let mut e = end;
    if e > len {
        e = len;
    } else if e < 0 {
        e += len;
        if e < 0 {
            e = 0;
        }
    }
    if s < 0 {
        s += len;
        if s < 0 {
            s = 0;
        }
    }
    (s, e)
}

/// Value of an int argument meant as a byte ("byte must be in range(0, 256)").
pub fn byte_value(o: &PyObject) -> PyResult<u8> {
    let v = ab::index_as_ssize(o, None)?;
    if !(0..256).contains(&v) {
        raise!(ValueError, "byte must be in range(0, 256)");
    }
    Ok(v as u8)
}

/// Sub-argument of find/count/index/...: an int (single byte) or bytes-like.
pub enum SubArg {
    Byte([u8; 1]),
    Buf(BArg),
}
impl SubArg {
    pub fn data(&self) -> &[u8] {
        match self {
            SubArg::Byte(b) => b,
            SubArg::Buf(b) => b.data(),
        }
    }
}
pub fn sub_arg(o: &PyObject) -> PyResult<SubArg> {
    if ab::has_index(o) {
        let v = ab::index_as_ssize(o, None)?;
        if !(0..256).contains(&v) {
            raise!(ValueError, "byte must be in range(0, 256)");
        }
        return Ok(SubArg::Byte([v as u8]));
    }
    if !buffer::check_buffer(o) {
        raise!(TypeError, "argument should be integer or bytes-like object, not '{}'", tname(o));
    }
    Ok(SubArg::Buf(barg(o)?))
}

/// fillchar of center/ljust/rjust: bytes or bytearray of length 1.
fn fill_arg(o: Option<&PyObject>, fname: &str) -> PyResult<u8> {
    match o {
        None => Ok(b' '),
        Some(x) => {
            if bytes::is_bytes(x) && bytes::bytes_len(x) == 1 {
                return Ok(bytes::bytes_data(x)[0]);
            }
            if bytearray::is_bytearray(x) && bytearray::bytearray_len(x) == 1 {
                return Ok(bytearray::bytearray_data(x)[0]);
            }
            raise!(TypeError, "{}() argument 2 must be a byte string of length 1, not {}", fname, tname(x));
        }
    }
}

fn int_arg_named(o: &PyObject) -> PyResult<isize> {
    ssize_arg(o)
}

// ====================================================================== find family

#[derive(Clone, Copy, PartialEq, Eq)]
enum FindKind {
    Find,
    RFind,
    Index,
    RIndex,
    Count,
}

fn find_common(o: &PyObject, args: &[&PyObject], kind: FindKind) -> PyResult {
    let fname = match kind {
        FindKind::Find => "find",
        FindKind::RFind => "rfind",
        FindKind::Index => "index",
        FindKind::RIndex => "rindex",
        FindKind::Count => "count",
    };
    crate::args::check_pos(fname, args, 1, 3)?;
    let _g = SelfGuard::new(o);
    let sub = sub_arg(args[0])?;
    let start = slice_index(args.get(1).copied(), 0)?;
    let end = slice_index(args.get(2).copied(), isize::MAX)?;
    let d = sdata(o);
    let n = sub.data();
    let (s, e) = adjust_indices(start, end, d.len() as isize);
    if kind == FindKind::Count {
        if e < s || s > d.len() as isize {
            return Ok(int::new_int(0));
        }
        let c = search::count(&d[s as usize..e as usize], n, usize::MAX);
        return Ok(int::new_int_usize(c));
    }
    let r: isize = if e - s < n.len() as isize || s > d.len() as isize {
        -1
    } else {
        let h = &d[s as usize..e as usize];
        let p = match kind {
            FindKind::Find | FindKind::Index => bfind(h, n),
            _ => brfind(h, n),
        };
        match p {
            Some(p) => p as isize + s,
            None => -1,
        }
    };
    if r < 0 && (kind == FindKind::Index || kind == FindKind::RIndex) {
        raise!(ValueError, "subsection not found");
    }
    Ok(int::new_int(r as i64))
}

pub fn m_find(o: &PyObject, a: &[&PyObject]) -> PyResult {
    find_common(o, a, FindKind::Find)
}
pub fn m_rfind(o: &PyObject, a: &[&PyObject]) -> PyResult {
    find_common(o, a, FindKind::RFind)
}
pub fn m_index(o: &PyObject, a: &[&PyObject]) -> PyResult {
    find_common(o, a, FindKind::Index)
}
pub fn m_rindex(o: &PyObject, a: &[&PyObject]) -> PyResult {
    find_common(o, a, FindKind::RIndex)
}
pub fn m_count(o: &PyObject, a: &[&PyObject]) -> PyResult {
    find_common(o, a, FindKind::Count)
}

/// `x in self` for bytes/bytearray.
pub fn contains(o: &PyObject, x: &PyObject) -> PyResult<bool> {
    let _g = SelfGuard::new(o);
    if ab::has_index(x) {
        let v = ab::index_as_ssize(x, None)?;
        if !(0..256).contains(&v) {
            raise!(ValueError, "byte must be in range(0, 256)");
        }
        return Ok(memchr(v as u8, sdata(o)).is_some());
    }
    let b = barg(x)?;
    Ok(bfind(sdata(o), b.data()).is_some())
}

fn tailmatch(d: &[u8], sub: &[u8], start: isize, end: isize, at_end: bool) -> bool {
    let len = d.len() as isize;
    let slen = sub.len() as isize;
    let (mut s, e) = adjust_indices(start, end, len);
    if !at_end {
        if s > len - slen {
            return false;
        }
    } else {
        if e - s < slen || s > len {
            return false;
        }
        if e - slen > s {
            s = e - slen;
        }
    }
    if e - s < slen {
        return false;
    }
    &d[s as usize..(s + slen) as usize] == sub
}

fn startend(o: &PyObject, args: &[&PyObject], at_end: bool) -> PyResult {
    let fname = if at_end { "endswith" } else { "startswith" };
    crate::args::check_pos(fname, args, 1, 3)?;
    let _g = SelfGuard::new(o);
    let subobj = args[0];
    let start = slice_index(args.get(1).copied(), 0)?;
    let end = slice_index(args.get(2).copied(), isize::MAX)?;
    if tuple::is_tuple(subobj) {
        for item in tuple::tuple_items(subobj) {
            let b = barg(item)?;
            if tailmatch(sdata(o), b.data(), start, end, at_end) {
                return Ok(bool_::new_bool(true));
            }
        }
        return Ok(bool_::new_bool(false));
    }
    let b = match barg(subobj) {
        Ok(b) => b,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "{} first arg must be bytes or a tuple of bytes, not {}", fname, tname(subobj));
            }
            return Err(e);
        }
    };
    Ok(bool_::new_bool(tailmatch(sdata(o), b.data(), start, end, at_end)))
}
pub fn m_startswith(o: &PyObject, a: &[&PyObject]) -> PyResult {
    startend(o, a, false)
}
pub fn m_endswith(o: &PyObject, a: &[&PyObject]) -> PyResult {
    startend(o, a, true)
}

// ====================================================================== case & classification

fn map_bytes(o: &PyObject, f: impl Fn(u8) -> u8) -> PyResult {
    let v: Vec<u8> = sdata(o).iter().map(|&c| f(c)).collect();
    new_like(o, &v)
}
pub fn m_lower(o: &PyObject) -> PyResult {
    map_bytes(o, |c| c.to_ascii_lowercase())
}
pub fn m_upper(o: &PyObject) -> PyResult {
    map_bytes(o, |c| c.to_ascii_uppercase())
}
pub fn m_swapcase(o: &PyObject) -> PyResult {
    map_bytes(o, |c| {
        if is_lower(c) {
            c.to_ascii_uppercase()
        } else if is_upper(c) {
            c.to_ascii_lowercase()
        } else {
            c
        }
    })
}
pub fn m_capitalize(o: &PyObject) -> PyResult {
    let d = sdata(o);
    let mut v = Vec::with_capacity(d.len());
    for (i, &c) in d.iter().enumerate() {
        v.push(if i == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() });
    }
    new_like(o, &v)
}
pub fn m_title(o: &PyObject) -> PyResult {
    let d = sdata(o);
    let mut v = Vec::with_capacity(d.len());
    let mut prev_cased = false;
    for &c in d {
        if is_lower(c) {
            v.push(if prev_cased { c } else { c.to_ascii_uppercase() });
            prev_cased = true;
        } else if is_upper(c) {
            v.push(if prev_cased { c.to_ascii_lowercase() } else { c });
            prev_cased = true;
        } else {
            v.push(c);
            prev_cased = false;
        }
    }
    new_like(o, &v)
}

fn pred_all(o: &PyObject, f: impl Fn(u8) -> bool) -> PyResult {
    let d = sdata(o);
    Ok(bool_::new_bool(!d.is_empty() && d.iter().all(|&c| f(c))))
}
pub fn m_isalnum(o: &PyObject) -> PyResult {
    pred_all(o, |c| c.is_ascii_alphanumeric())
}
pub fn m_isalpha(o: &PyObject) -> PyResult {
    pred_all(o, |c| c.is_ascii_alphabetic())
}
pub fn m_isdigit(o: &PyObject) -> PyResult {
    pred_all(o, |c| c.is_ascii_digit())
}
pub fn m_isspace(o: &PyObject) -> PyResult {
    pred_all(o, is_space)
}
pub fn m_isascii(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(sdata(o).iter().all(|&c| c < 128)))
}
pub fn m_islower(o: &PyObject) -> PyResult {
    let d = sdata(o);
    let mut cased = false;
    for &c in d {
        if is_upper(c) {
            return Ok(bool_::new_bool(false));
        }
        if is_lower(c) {
            cased = true;
        }
    }
    Ok(bool_::new_bool(cased))
}
pub fn m_isupper(o: &PyObject) -> PyResult {
    let d = sdata(o);
    let mut cased = false;
    for &c in d {
        if is_lower(c) {
            return Ok(bool_::new_bool(false));
        }
        if is_upper(c) {
            cased = true;
        }
    }
    Ok(bool_::new_bool(cased))
}
pub fn m_istitle(o: &PyObject) -> PyResult {
    let d = sdata(o);
    let mut cased = false;
    let mut prev_cased = false;
    for &c in d {
        if is_upper(c) {
            if prev_cased {
                return Ok(bool_::new_bool(false));
            }
            prev_cased = true;
            cased = true;
        } else if is_lower(c) {
            if !prev_cased {
                return Ok(bool_::new_bool(false));
            }
            prev_cased = true;
            cased = true;
        } else {
            prev_cased = false;
        }
    }
    Ok(bool_::new_bool(cased))
}

// ====================================================================== padding

fn pad(o: &PyObject, left: usize, right: usize, fill: u8) -> PyResult {
    let d = sdata(o);
    let total = d.len().checked_add(left).and_then(|x| x.checked_add(right)).ok_or_else(memory_error)?;
    if total > bytes::MAX_SIZE {
        return Err(memory_error());
    }
    let mut v = Vec::new();
    v.try_reserve_exact(total).map_err(|_| memory_error())?;
    v.resize(left, fill);
    v.extend_from_slice(d);
    v.resize(total, fill);
    new_like(o, &v)
}

fn width_and_fill(fname: &str, args: &[&PyObject]) -> PyResult<(isize, u8)> {
    crate::args::check_pos(fname, args, 1, 2)?;
    let w = int_arg_named(args[0])?;
    let f = fill_arg(args.get(1).copied(), fname)?;
    Ok((w, f))
}
pub fn m_ljust(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_and_fill("ljust", args)?;
    let len = sdata(o).len() as isize;
    if w <= len {
        return self_or_new(o, 0, len as usize);
    }
    pad(o, 0, (w - len) as usize, f)
}
pub fn m_rjust(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_and_fill("rjust", args)?;
    let len = sdata(o).len() as isize;
    if w <= len {
        return self_or_new(o, 0, len as usize);
    }
    pad(o, (w - len) as usize, 0, f)
}
pub fn m_center(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_and_fill("center", args)?;
    let len = sdata(o).len() as isize;
    if w <= len {
        return self_or_new(o, 0, len as usize);
    }
    let marg = w - len;
    let left = marg / 2 + (marg & w & 1);
    pad(o, left as usize, (marg - left) as usize, f)
}
pub fn m_zfill(o: &PyObject, w: &PyObject) -> PyResult {
    let w = int_arg_named(w)?;
    let d = sdata(o);
    let len = d.len() as isize;
    if w <= len {
        return self_or_new(o, 0, d.len());
    }
    let fill = (w - len) as usize;
    let mut v = Vec::new();
    v.try_reserve_exact(w as usize).map_err(|_| memory_error())?;
    v.resize(fill, b'0');
    v.extend_from_slice(d);
    if !d.is_empty() && (d[0] == b'+' || d[0] == b'-') {
        v[0] = d[0];
        v[fill] = b'0';
    }
    new_like(o, &v)
}

pub fn m_expandtabs(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("expandtabs", &["tabsize"], 0, 0);
    let [ts] = crate::args::parse::<1>(&P, args, kw)?;
    let tabsize: i64 = match ts {
        None => 8,
        Some(t) => {
            let v = ab::index(t)?;
            match int::as_i64(&v) {
                Some(x) if x >= i32::MIN as i64 && x <= i32::MAX as i64 => x,
                _ => raise!(OverflowError, "Python int too large to convert to C int"),
            }
        }
    };
    let d = sdata(o);
    let mut v: Vec<u8> = Vec::with_capacity(d.len());
    let mut col: u64 = 0;
    for &c in d {
        if c == b'\t' {
            if tabsize > 0 {
                let incr = tabsize as u64 - (col % tabsize as u64);
                col += incr;
                if v.len() as u64 + incr > bytes::MAX_SIZE as u64 {
                    raise!(OverflowError, "result too long");
                }
                v.try_reserve(incr as usize).map_err(|_| memory_error())?;
                v.resize(v.len() + incr as usize, b' ');
            }
        } else {
            v.push(c);
            col += 1;
            if c == b'\n' || c == b'\r' {
                col = 0;
            }
        }
    }
    new_like(o, &v)
}

// ====================================================================== strip / prefix

fn strip_common(o: &PyObject, args: &[&PyObject], fname: &str, left: bool, right: bool) -> PyResult {
    crate::args::check_pos(fname, args, 0, 1)?;
    let _g = SelfGuard::new(o);
    let chars: Option<BArg> = match args.first() {
        None => None,
        Some(x) if none::is_none(x) => None,
        Some(x) => Some(barg(x)?),
    };
    let d = sdata(o);
    let pred = |c: u8| -> bool {
        match &chars {
            None => is_space(c),
            Some(b) => b.data().contains(&c),
        }
    };
    let mut s = 0;
    let mut e = d.len();
    if left {
        while s < e && pred(d[s]) {
            s += 1;
        }
    }
    if right {
        while e > s && pred(d[e - 1]) {
            e -= 1;
        }
    }
    self_or_new(o, s, e)
}
pub fn m_strip(o: &PyObject, a: &[&PyObject]) -> PyResult {
    strip_common(o, a, "strip", true, true)
}
pub fn m_lstrip(o: &PyObject, a: &[&PyObject]) -> PyResult {
    strip_common(o, a, "lstrip", true, false)
}
pub fn m_rstrip(o: &PyObject, a: &[&PyObject]) -> PyResult {
    strip_common(o, a, "rstrip", false, true)
}
pub fn m_removeprefix(o: &PyObject, p: &PyObject) -> PyResult {
    let _g = SelfGuard::new(o);
    let b = barg(p)?;
    let d = sdata(o);
    let pd = b.data();
    if d.starts_with(pd) && !pd.is_empty() {
        return self_or_new(o, pd.len(), d.len());
    }
    self_or_new(o, 0, d.len())
}
pub fn m_removesuffix(o: &PyObject, p: &PyObject) -> PyResult {
    let _g = SelfGuard::new(o);
    let b = barg(p)?;
    let d = sdata(o);
    let pd = b.data();
    if d.ends_with(pd) && !pd.is_empty() {
        return self_or_new(o, 0, d.len() - pd.len());
    }
    self_or_new(o, 0, d.len())
}

// ====================================================================== split / partition / join

fn new_list_of(o: &PyObject, parts: Vec<&[u8]>) -> PyResult {
    let mut v = Vec::with_capacity(parts.len());
    let whole = sdata(o).len();
    for p in parts {
        if p.len() == whole && bytes::is_bytes_exact(o) && p.as_ptr() == sdata(o).as_ptr() {
            v.push(o.to_ref());
        } else {
            v.push(new_like(o, p)?);
        }
    }
    Ok(crate::types::list::new_list(v))
}

fn split_whitespace(d: &[u8], maxsplit: isize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let n = d.len();
    let mut i = 0;
    let mut count = 0;
    while i < n {
        while i < n && is_space(d[i]) {
            i += 1;
        }
        if i == n {
            break;
        }
        if maxsplit >= 0 && count >= maxsplit {
            // rest (with trailing whitespace stripped)
            let mut e = n;
            while e > i && is_space(d[e - 1]) {
                e -= 1;
            }
            out.push(&d[i..e]);
            return out;
        }
        let s = i;
        while i < n && !is_space(d[i]) {
            i += 1;
        }
        out.push(&d[s..i]);
        count += 1;
    }
    out
}

fn rsplit_whitespace(d: &[u8], maxsplit: isize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = d.len();
    let mut count = 0;
    while i > 0 {
        while i > 0 && is_space(d[i - 1]) {
            i -= 1;
        }
        if i == 0 {
            break;
        }
        if maxsplit >= 0 && count >= maxsplit {
            let mut s = 0;
            while s < i && is_space(d[s]) {
                s += 1;
            }
            out.push(&d[s..i]);
            break;
        }
        let e = i;
        while i > 0 && !is_space(d[i - 1]) {
            i -= 1;
        }
        out.push(&d[i..e]);
        count += 1;
    }
    out.reverse();
    out
}

fn split_args(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<(Option<BArg>, isize)> {
    let names: &[&str] = &["sep", "maxsplit"];
    let p = crate::args::Params::new(fname, names, 0, 0);
    let [sep, ms] = crate::args::parse::<2>(&p, args, kw)?;
    let maxsplit = match ms {
        None => -1,
        Some(m) => ssize_arg(m)?,
    };
    let sep = match sep {
        None => None,
        Some(s) if none::is_none(s) => None,
        Some(s) => Some(barg(s)?),
    };
    if let Some(s) = &sep {
        if s.data().is_empty() {
            raise!(ValueError, "empty separator");
        }
    }
    Ok((sep, maxsplit))
}

pub fn m_split(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let _g = SelfGuard::new(o);
    let (sep, maxsplit) = split_args("split", args, kw)?;
    let d = sdata(o);
    let parts: Vec<&[u8]> = match &sep {
        None => split_whitespace(d, maxsplit),
        Some(s) => {
            let s = s.data();
            let max = if maxsplit < 0 { usize::MAX } else { maxsplit as usize };
            let pos = search::find_all(d, s, max);
            let mut v = Vec::with_capacity(pos.len() + 1);
            let mut last = 0;
            for p in pos {
                v.push(&d[last..p]);
                last = p + s.len();
            }
            v.push(&d[last..]);
            v
        }
    };
    new_list_of(o, parts)
}

pub fn m_rsplit(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let _g = SelfGuard::new(o);
    let (sep, maxsplit) = split_args("rsplit", args, kw)?;
    let d = sdata(o);
    let parts: Vec<&[u8]> = match &sep {
        None => rsplit_whitespace(d, maxsplit),
        Some(s) => {
            let s = s.data();
            let mut v = Vec::new();
            let mut end = d.len();
            let mut count = 0;
            loop {
                if maxsplit >= 0 && count >= maxsplit {
                    break;
                }
                match brfind(&d[..end], s) {
                    Some(p) => {
                        v.push(&d[p + s.len()..end]);
                        end = p;
                        count += 1;
                    }
                    None => break,
                }
            }
            v.push(&d[..end]);
            v.reverse();
            v
        }
    };
    new_list_of(o, parts)
}

fn partition_common(o: &PyObject, sep: &PyObject, rev: bool) -> PyResult {
    let _g = SelfGuard::new(o);
    let s = barg(sep)?;
    let sd = s.data();
    if sd.is_empty() {
        raise!(ValueError, "empty separator");
    }
    let d = sdata(o);
    let pos = if rev { brfind(d, sd) } else { bfind(d, sd) };
    let empty = || new_like(o, &[]);
    let whole = || self_or_new(o, 0, d.len());
    let (a, b, c) = match pos {
        Some(p) => {
            // immutable bytes return the separator object itself (as CPython's stringlib)
            let mid = if bytes::is_bytes(o) { sep.to_ref() } else { new_like(o, sd)? };
            (new_like(o, &d[..p])?, mid, new_like(o, &d[p + sd.len()..])?)
        }
        None => {
            if rev {
                (empty()?, empty()?, whole()?)
            } else {
                (whole()?, empty()?, empty()?)
            }
        }
    };
    Ok(tuple::new_tuple(vec![a, b, c]))
}
pub fn m_partition(o: &PyObject, sep: &PyObject) -> PyResult {
    partition_common(o, sep, false)
}
pub fn m_rpartition(o: &PyObject, sep: &PyObject) -> PyResult {
    partition_common(o, sep, true)
}

pub fn m_splitlines(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("splitlines", &["keepends"], 0, 0);
    let [ke] = crate::args::parse::<1>(&P, args, kw)?;
    let keepends = match ke {
        None => false,
        Some(k) => {
            let v = ab::index(k)?;
            int::sign(&v) != 0
        }
    };
    let d = sdata(o);
    let mut parts = Vec::new();
    let n = d.len();
    let mut i = 0;
    while i < n {
        let s = i;
        while i < n && d[i] != b'\n' && d[i] != b'\r' {
            i += 1;
        }
        let mut eol = i;
        if i < n {
            if d[i] == b'\r' && i + 1 < n && d[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            if keepends {
                eol = i;
            }
        }
        parts.push(&d[s..eol]);
    }
    new_list_of(o, parts)
}

pub fn m_join(o: &PyObject, it: &PyObject) -> PyResult {
    let _g = SelfGuard::new(o);
    let sep: Vec<u8> = sdata(o).to_vec();
    let is_list = crate::types::list::is_list(it);
    let items: Vec<PyRef> = if is_list {
        crate::types::list::list_to_vec(it)
    } else if tuple::is_tuple(it) {
        tuple::tuple_to_vec(it)
    } else {
        match ab::get_iter(it) {
            Ok(i) => {
                let mut v = Vec::new();
                while let Some(x) = ab::iter_next(&i)? {
                    v.push(x);
                }
                v
            }
            Err(e) => {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    raise!(TypeError, "can only join an iterable");
                }
                return Err(e);
            }
        }
    };
    let n0 = items.len();
    if n0 == 1 && bytes::is_bytes(o) && bytes::is_bytes_exact(&items[0]) {
        return Ok(items[0].clone());
    }
    let mut out: Vec<u8> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        if bytes::is_bytes(item) {
            out.extend_from_slice(bytes::bytes_data(item));
        } else {
            if !buffer::check_buffer(item) {
                raise!(TypeError, "sequence item {}: expected a bytes-like object, {} found", i, tname(item));
            }
            let b = BufferView::simple(item)?;
            if is_list && crate::types::list::list_len(it) != n0 {
                raise!(RuntimeError, "sequence changed size during iteration");
            }
            out.extend_from_slice(b.as_bytes());
        }
    }
    new_like(o, &out)
}

// ====================================================================== replace / translate

/// Generic replace of up to `maxcount` occurrences (maxcount < 0: all).
pub fn replace_bytes(d: &[u8], from: &[u8], to: &[u8], maxcount: isize) -> Option<Vec<u8>> {
    let max = if maxcount < 0 { usize::MAX } else { maxcount as usize };
    if max == 0 || (from.is_empty() && to.is_empty()) {
        return None;
    }
    if from.is_empty() {
        let k = (d.len() + 1).min(max);
        let mut v = Vec::with_capacity(d.len() + k * to.len());
        for i in 0..d.len() {
            if i < k {
                v.extend_from_slice(to);
            }
            v.push(d[i]);
        }
        if k > d.len() {
            v.extend_from_slice(to);
        }
        return Some(v);
    }
    if d.is_empty() {
        return None;
    }
    let pos = search::find_all(d, from, max);
    if pos.is_empty() {
        return None;
    }
    let mut v = Vec::with_capacity(d.len() - pos.len() * from.len() + pos.len() * to.len());
    let mut last = 0;
    for p in pos {
        v.extend_from_slice(&d[last..p]);
        v.extend_from_slice(to);
        last = p + from.len();
    }
    v.extend_from_slice(&d[last..]);
    Some(v)
}

pub fn m_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("replace", &["old", "new", "count"], 2, 2);
    let _g = SelfGuard::new(o);
    let [old, new, count] = crate::args::parse::<3>(&P, args, kw)?;
    let old = barg(old.unwrap())?;
    let new = barg(new.unwrap())?;
    let count = match count {
        None => -1,
        Some(c) => ssize_arg(c)?,
    };
    let d = sdata(o);
    match replace_bytes(d, old.data(), new.data(), count) {
        None => self_or_new(o, 0, d.len()),
        Some(v) => new_like(o, &v),
    }
}

pub fn m_translate(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("translate", &["table", "delete"], 1, 1);
    let _g = SelfGuard::new(o);
    let [table, delete] = crate::args::parse::<2>(&P, args, kw)?;
    let table = table.unwrap();
    let t: Option<BArg> = if none::is_none(table) {
        None
    } else {
        let b = barg(table)?;
        if b.data().len() != 256 {
            raise!(ValueError, "translation table must be 256 characters long");
        }
        Some(b)
    };
    let del: Option<BArg> = match delete {
        None => None,
        Some(x) => Some(barg(x)?),
    };
    let mut deltab = [false; 256];
    if let Some(dl) = &del {
        for &c in dl.data() {
            deltab[c as usize] = true;
        }
    }
    let d = sdata(o);
    let mut v = Vec::with_capacity(d.len());
    let mut changed = false;
    for &c in d {
        if deltab[c as usize] {
            changed = true;
            continue;
        }
        let nc = match &t {
            None => c,
            Some(tb) => tb.data()[c as usize],
        };
        if nc != c {
            changed = true;
        }
        v.push(nc);
    }
    if !changed {
        return self_or_new(o, 0, d.len());
    }
    new_like(o, &v)
}

pub fn m_maketrans(_cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("maketrans", args, 2, 2)?;
    let f = barg(args[0])?;
    let t = barg(args[1])?;
    if f.data().len() != t.data().len() {
        raise!(ValueError, "maketrans arguments must have same length");
    }
    let mut tab = [0u8; 256];
    for (i, x) in tab.iter_mut().enumerate() {
        *x = i as u8;
    }
    for (&a, &b) in f.data().iter().zip(t.data()) {
        tab[a as usize] = b;
    }
    Ok(bytes::new_bytes(&tab))
}

// ====================================================================== hex / fromhex

/// Validate the `sep` argument of hex() (may call `__len__`).
pub fn hex_sep(sep: Option<&PyObject>) -> PyResult<Option<u8>> {
    let s = match sep {
        None => return Ok(None),
        Some(s) => s,
    };
    let n = ab::length(s)?;
    if n != 1 {
        raise!(ValueError, "sep must be length 1.");
    }
    let c: u32 = if pystr::is_str(s) {
        pystr::str_data(s).get(0)
    } else if bytes::is_bytes(s) {
        bytes::bytes_data(s).first().copied().unwrap_or(0) as u32
    } else {
        raise!(TypeError, "sep must be str or bytes.");
    };
    if c > 127 {
        raise!(ValueError, "sep must be ASCII.");
    }
    Ok(Some(c as u8))
}

pub fn strhex_raw(data: &[u8], sep: Option<u8>, bytes_per_sep: isize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let abs = bytes_per_sep.unsigned_abs();
    let n = data.len();
    let use_sep = sep.is_some() && bytes_per_sep != 0 && n > abs;
    let mut out: Vec<u8> = Vec::with_capacity(n * 2 + if use_sep { (n - 1) / abs } else { 0 });
    if !use_sep {
        for &b in data {
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 15) as usize]);
        }
    } else {
        let sc = sep.unwrap();
        for (i, &b) in data.iter().enumerate() {
            if i > 0 {
                let boundary = if bytes_per_sep > 0 { (n - i) % abs == 0 } else { i % abs == 0 };
                if boundary {
                    out.push(sc);
                }
            }
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 15) as usize]);
        }
    }
    unsafe { String::from_utf8_unchecked(out) }
}

/// Parse (sep, bytes_per_sep) of hex(); returns the validated separator byte.
pub fn hex_args(args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<(Option<u8>, isize)> {
    static P: crate::args::Params = crate::args::Params::new("hex", &["sep", "bytes_per_sep"], 0, 0);
    let [sep, bps] = crate::args::parse::<2>(&P, args, kw)?;
    let bps = match bps {
        None => 1,
        Some(b) => ssize_arg(b)?,
    };
    Ok((hex_sep(sep)?, bps))
}

pub fn m_hex(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let _g = SelfGuard::new(o);
    let (sepc, bps) = hex_args(args, kw)?;
    Ok(pystr::new_str(&strhex_raw(sdata(o), sepc, bps)))
}

fn hexval(c: u32) -> Option<u8> {
    match c {
        0x30..=0x39 => Some((c - 0x30) as u8),
        0x61..=0x66 => Some((c - 0x61 + 10) as u8),
        0x41..=0x46 => Some((c - 0x41 + 10) as u8),
        _ => None,
    }
}

/// _PyBytes_FromHex: parse the fromhex() argument.
pub fn parse_fromhex(arg: &PyObject) -> PyResult<Vec<u8>> {
    let chars: Vec<u32> = if pystr::is_str(arg) {
        pystr::str_data(arg).iter().collect()
    } else if buffer::check_buffer(arg) {
        let b = BufferView::simple(arg)?;
        b.as_bytes().iter().map(|&c| c as u32).collect()
    } else {
        raise!(TypeError, "fromhex() argument must be str or bytes-like, not {}", tname(arg));
    };
    let n = chars.len();
    let mut out = Vec::with_capacity(n / 2);
    let mut i = 0;
    loop {
        while i < n && chars[i] < 128 && is_space(chars[i] as u8) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let hi = match hexval(chars[i]) {
            Some(v) => v,
            None => raise!(ValueError, "non-hexadecimal number found in fromhex() arg at position {}", i),
        };
        if i + 1 >= n {
            raise!(ValueError, "fromhex() arg must contain an even number of hexadecimal digits");
        }
        let lo = match hexval(chars[i + 1]) {
            Some(v) => v,
            None => raise!(ValueError, "non-hexadecimal number found in fromhex() arg at position {}", i + 1),
        };
        out.push(hi * 16 + lo);
        i += 2;
    }
    Ok(out)
}

pub fn m_fromhex(cls: &PyObject, arg: &PyObject) -> PyResult {
    let v = parse_fromhex(arg)?;
    let r = bytes::try_new_bytes(&v)?;
    if cls.as_ptr() as *mut PyTypeObject == bytes::bytes_type() {
        return Ok(r);
    }
    ab::call(cls, &[&r])
}

// ====================================================================== decode

pub fn m_decode(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("decode", &["encoding", "errors"], 0, 0);
    let [enc, errs] = crate::args::parse::<2>(&P, args, kw)?;
    if let Some(e) = enc {
        if !pystr::is_str(e) {
            raise!(TypeError, "decode() argument 'encoding' must be str, not {}", tname(e));
        }
    }
    if let Some(e) = errs {
        if !pystr::is_str(e) {
            raise!(TypeError, "decode() argument 'errors' must be str, not {}", tname(e));
        }
    }
    crate::modules::codecs_core::decode_object(o, enc, errs)
}

// ====================================================================== %-formatting

enum ArgPos {
    Single,
    Index(usize),
    Key(PyRef),
}

struct FmtArgs<'a> {
    args: &'a PyObject,
    is_tuple: bool,
    n: usize,
    idx: usize,
    dict: Option<&'a PyObject>,
}

impl<'a> FmtArgs<'a> {
    fn next(&mut self) -> PyResult<(PyRef, ArgPos)> {
        if self.idx >= self.n {
            raise!(TypeError, "not enough arguments for format string (got {})", self.n);
        }
        self.idx += 1;
        if self.is_tuple {
            Ok((tuple::tuple_get(self.args, self.idx - 1).to_ref(), ArgPos::Index(self.idx)))
        } else {
            Ok((self.args.to_ref(), ArgPos::Single))
        }
    }
}

fn arg_desc(p: &ArgPos) -> PyResult<String> {
    Ok(match p {
        ArgPos::Single => "format argument".to_string(),
        ArgPos::Index(i) => format!("format argument {}", i),
        ArgPos::Key(k) => format!("format argument {}", ab::repr_string(k)?),
    })
}

fn is_real_number(v: &PyObject) -> bool {
    if int::is_int(v) || crate::types::float::is_float(v) {
        return true;
    }
    if crate::types::complex::is_complex(v) {
        return false;
    }
    let nb = v.ty().tp_as_number;
    !nb.is_null() && unsafe { (*nb).nb_int.is_some() || (*nb).nb_index.is_some() || (*nb).nb_float.is_some() }
}

/// Bytes for %b / %s.
fn format_obj_bytes(v: &PyObject, pos: &ArgPos) -> PyResult<Vec<u8>> {
    if bytes::is_bytes(v) {
        return Ok(bytes::bytes_data(v).to_vec());
    }
    if bytearray::is_bytearray(v) {
        return Ok(bytearray::bytearray_data(v).to_vec());
    }
    if let Some(f) = ab::lookup_special(v, crate::name!("__bytes__"))? {
        let r = ab::call0(&f)?;
        if !bytes::is_bytes(&r) {
            raise!(TypeError, "__bytes__ returned non-bytes (type {})", tname(&r));
        }
        return Ok(bytes::bytes_data(&r).to_vec());
    }
    if buffer::check_buffer(v) {
        let b = BufferView::get(v, buffer::PyBUF_FULL_RO)?;
        return Ok(b.to_vec());
    }
    raise!(
        TypeError,
        "{}: %b requires a bytes-like object, or an object that implements __bytes__, not {}",
        arg_desc(pos)?,
        type_fqname(v.type_ptr())
    );
}

fn push_padded(out: &mut Vec<u8>, s: &[u8], conv: char, opts: &rpy_numfmt::percent::PercentOpts) -> PyResult<()> {
    let (take, l, r) = rpy_numfmt::percent::percent_str_layout(s.len(), conv, opts);
    out.try_reserve(l.saturating_add(r).saturating_add(take)).map_err(|_| memory_error())?;
    out.resize(out.len() + l, b' ');
    out.extend_from_slice(&s[..take]);
    out.resize(out.len() + r, b' ');
    Ok(())
}

/// bytes % args (shared by bytes and bytearray).
pub fn percent_format(fmt: &[u8], args: &PyObject) -> PyResult<Vec<u8>> {
    use rpy_numfmt::percent::{PercentNum, PercentOpts, parse_percent_spec};
    let is_tuple = tuple::is_tuple(args);
    let has_mapping = {
        let m = args.ty().tp_as_mapping;
        !m.is_null() && unsafe { (*m).mp_subscript.is_some() }
    };
    let dict = if has_mapping && !is_tuple && !bytes::is_bytes(args) && !bytearray::is_bytearray(args) && !pystr::is_str(args) {
        Some(args)
    } else {
        None
    };
    let mut fa = FmtArgs { args, is_tuple, n: if is_tuple { tuple::tuple_len(args) } else { 1 }, idx: 0, dict };
    let mut out: Vec<u8> = Vec::with_capacity(fmt.len() + 16);
    let mut i = 0;
    let mut key_used = false;
    let printable = |c: u32| rpy_numfmt::percent::default_is_printable(c);
    while i < fmt.len() {
        if fmt[i] != b'%' {
            let j = memchr(b'%', &fmt[i..]).map_or(fmt.len(), |k| i + k);
            out.extend_from_slice(&fmt[i..j]);
            i = j;
            continue;
        }
        let spec = parse_percent_spec(fmt, i, true, &printable).map_err(crate::types::format_impl::numfmt_error)?;
        i = spec.end;
        if spec.conv == '%' {
            out.push(b'%');
            continue;
        }
        let mut opts = PercentOpts::from_spec(&spec);
        if spec.width == PercentNum::Star {
            let (v, p) = fa.next()?;
            if !int::is_int(&v) {
                raise!(TypeError, "{}: * requires int, not {}", arg_desc(&p)?, type_fqname(v.type_ptr()));
            }
            let w = match int::as_i64(&v) {
                Some(w) if w != i64::MIN => w,
                _ => raise!(OverflowError, "{}: too big for width", arg_desc(&p)?),
            };
            if w < 0 {
                opts.flags.left = true;
            }
            opts.width = Some(w.unsigned_abs() as usize);
        }
        if spec.precision == PercentNum::Star {
            let (v, p) = fa.next()?;
            if !int::is_int(&v) {
                raise!(TypeError, "{}: * requires int, not {}", arg_desc(&p)?, type_fqname(v.type_ptr()));
            }
            let pr = match int::as_i64(&v) {
                Some(x) if x >= i32::MIN as i64 && x <= i32::MAX as i64 => x,
                _ => raise!(OverflowError, "{}: too big for precision", arg_desc(&p)?),
            };
            opts.precision = Some(pr.max(0) as usize);
        }
        let (v, pos): (PyRef, ArgPos) = match spec.key {
            Some((ks, ke)) => {
                let d = match fa.dict {
                    Some(d) => d,
                    None => raise!(TypeError, "format requires a mapping, not {}", tname(args)),
                };
                let key = bytes::new_bytes(&fmt[ks..ke]);
                let v = ab::getitem(d, &key)?;
                key_used = true;
                (v, ArgPos::Key(key))
            }
            None => {
                if fa.dict.is_some() && key_used {
                    raise!(ValueError, "format requires a parenthesised mapping key at position {}", spec.start);
                }
                fa.next()?
            }
        };
        let conv = spec.conv;
        match conv {
            's' | 'b' => {
                let b = format_obj_bytes(&v, &pos)?;
                push_padded(&mut out, &b, conv, &opts)?;
            }
            'r' | 'a' => {
                let s = ab::ascii(&v)?;
                let st = pystr::to_string_lossy(&s);
                push_padded(&mut out, st.as_bytes(), conv, &opts)?;
            }
            'c' => {
                let c: u8 = if (bytes::is_bytes(&v) || bytearray::is_bytearray(&v)) && sdata(&v).len() == 1 {
                    sdata(&v)[0]
                } else if bytes::is_bytes(&v) || bytearray::is_bytearray(&v) {
                    raise!(
                        TypeError,
                        "{}: %c requires an integer in range(256) or a single byte, not a {} object of length {}",
                        arg_desc(&pos)?,
                        if bytes::is_bytes(&v) { "bytes" } else { "bytearray" },
                        sdata(&v).len()
                    );
                } else if ab::has_index(&v) {
                    let iv = ab::index(&v)?;
                    match int::as_i64(&iv) {
                        Some(x) if (0..256).contains(&x) => x as u8,
                        _ => raise!(OverflowError, "{}: %c argument not in range(256)", arg_desc(&pos)?),
                    }
                } else {
                    raise!(
                        TypeError,
                        "{}: %c requires an integer in range(256) or a single byte, not {}",
                        arg_desc(&pos)?,
                        type_fqname(v.type_ptr())
                    );
                };
                push_padded(&mut out, &[c], 'c', &opts)?;
            }
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' => {
                let iv: PyRef = if matches!(conv, 'o' | 'x' | 'X') {
                    if !ab::has_index(&v) {
                        raise!(TypeError, "{}: %{} requires an integer, not {}", arg_desc(&pos)?, conv, type_fqname(v.type_ptr()));
                    }
                    ab::index(&v)?
                } else if int::is_int(&v) {
                    v.clone()
                } else if is_real_number(&v) {
                    crate::types::int_methods::number_long(&v)?
                } else {
                    raise!(TypeError, "{}: %{} requires a real number, not {}", arg_desc(&pos)?, conv, type_fqname(v.type_ptr()));
                };
                let radix = match conv {
                    'o' => 8,
                    'x' | 'X' => 16,
                    _ => 10,
                };
                let digits = int::int_methods_mag_string(&iv, radix);
                let mut s = String::new();
                let c = if matches!(conv, 'i' | 'u') { 'd' } else { conv };
                rpy_numfmt::percent::percent_format_int(&mut s, int::sign(&iv) < 0, &digits, c, &opts)
                    .map_err(crate::types::format_impl::numfmt_error)?;
                out.try_reserve(s.len()).map_err(|_| memory_error())?;
                out.extend_from_slice(s.as_bytes());
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                if !is_real_number(&v) {
                    raise!(TypeError, "{}: %{} requires a real number, not {}", arg_desc(&pos)?, conv, type_fqname(v.type_ptr()));
                }
                let x = ab::float_value(&v)?;
                let mut s = String::new();
                rpy_numfmt::percent::percent_format_float(&mut s, x, conv, &opts).map_err(crate::types::format_impl::numfmt_error)?;
                out.try_reserve(s.len()).map_err(|_| memory_error())?;
                out.extend_from_slice(s.as_bytes());
            }
            _ => raise!(ValueError, "unsupported format %{} at position {}", conv, spec.start),
        }
    }
    if fa.dict.is_none() && fa.idx < fa.n {
        raise!(TypeError, "not all arguments converted during bytes formatting (required {}, got {})", fa.idx, fa.n);
    }
    Ok(out)
}

// ====================================================================== repr

pub fn bytes_repr_string(b: &[u8], prefix: &str, suffix: &str) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let q = if has_sq && !has_dq { b'"' } else { b'\'' };
    let mut out = String::with_capacity(b.len() + prefix.len() + suffix.len() + 3);
    out.push_str(prefix);
    out.push(q as char);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            _ if c == q => {
                out.push('\\');
                out.push(q as char);
            }
            0x20..=0x7e => out.push(c as char),
            _ => {
                out.push_str("\\x");
                out.push(HEX[(c >> 4) as usize] as char);
                out.push(HEX[(c & 15) as usize] as char);
            }
        }
    }
    out.push(q as char);
    out.push_str(suffix);
    out
}

// ====================================================================== construction

fn byte_from_item(item: &PyObject) -> PyResult<u8> {
    let v = ab::index_as_ssize(item, None)?;
    if !(0..256).contains(&v) {
        raise!(ValueError, "bytes must be in range(0, 256)");
    }
    Ok(v as u8)
}

/// Collect the integers of an iterable into bytes (list/tuple fast paths are
/// re-entrancy safe: the list length is re-read on every step).
pub fn bytes_from_iterable(x: &PyObject, it_err_type: &str) -> PyResult<Vec<u8>> {
    if crate::types::list::is_list(x) {
        let mut v = Vec::with_capacity(crate::types::list::list_len(x));
        let mut i = 0;
        while i < crate::types::list::list_len(x) {
            let item = match crate::types::list::get(x, i) {
                Some(it) => it,
                None => break,
            };
            v.push(byte_from_item(&item)?);
            i += 1;
        }
        return Ok(v);
    }
    if tuple::is_tuple(x) {
        let mut v = Vec::with_capacity(tuple::tuple_len(x));
        for item in tuple::tuple_items(x) {
            v.push(byte_from_item(item)?);
        }
        return Ok(v);
    }
    let it = match ab::get_iter(x) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "cannot convert '{}' object to {}", tname(x), it_err_type);
            }
            return Err(e);
        }
    };
    let hint = ab::length_hint(x, 64).unwrap_or_else(|_| {
        clear_exc();
        64
    });
    let mut v = Vec::with_capacity(hint.min(1 << 20));
    while let Some(item) = ab::iter_next(&it)? {
        v.push(byte_from_item(&item)?);
    }
    Ok(v)
}

/// PyBytes_FromObject.
pub fn bytes_from_object(x: &PyObject) -> PyResult {
    if bytes::is_bytes_exact(x) {
        return Ok(x.to_ref());
    }
    bytes::try_new_bytes(&bytes_data_from_object(x)?)
}

fn bytes_data_from_object(x: &PyObject) -> PyResult<Vec<u8>> {
    if buffer::check_buffer(x) {
        let b = BufferView::get(x, buffer::PyBUF_FULL_RO)?;
        return Ok(b.to_vec());
    }
    if !pystr::is_str(x) {
        return bytes_from_iterable(x, "bytes");
    }
    raise!(TypeError, "cannot convert '{}' object to bytes", tname(x));
}

fn check_str_arg(o: Option<&PyObject>, fname: &str, argname: &str) -> PyResult<()> {
    if let Some(x) = o {
        if !pystr::is_str(x) {
            raise!(TypeError, "{}() argument '{}' must be str, not {}", fname, argname, tname(x));
        }
    }
    Ok(())
}
pub fn check_encoding_args(fname: &str, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult<()> {
    check_str_arg(enc, fname, "encoding")?;
    check_str_arg(errs, fname, "errors")
}

/// Encode `s` with `encoding`/`errors`; the result must be bytes.
pub fn encode_for_ctor(s: &PyObject, enc: &PyObject, errs: Option<&PyObject>) -> PyResult {
    let r = crate::modules::codecs_core::encode_str(s, Some(enc), errs)?;
    if !bytes::is_bytes(&r) {
        if bytearray::is_bytearray(&r) {
            return bytes::try_new_bytes(bytearray::bytearray_data(&r));
        }
        raise!(
            TypeError,
            "'{}' encoder returned '{}' instead of 'bytes'; use codecs.encode() to encode to arbitrary types",
            pystr::to_string_lossy(enc),
            tname(&r)
        );
    }
    Ok(r)
}

fn bytes_new_impl(tp: *mut PyTypeObject, x: Option<&PyObject>, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    check_encoding_args("bytes", enc, errs)?;
    let x = match x {
        None => {
            if enc.is_some() {
                raise!(TypeError, "encoding without a string argument");
            }
            if errs.is_some() {
                raise!(TypeError, "errors without a string argument");
            }
            return bytes::new_bytes_of_type(tp, &[]);
        }
        Some(x) => x,
    };
    if let Some(e) = enc {
        if !pystr::is_str(x) {
            raise!(TypeError, "encoding without a string argument");
        }
        let r = encode_for_ctor(x, e, errs)?;
        if tp == bytes::bytes_type() {
            return Ok(r);
        }
        return bytes::new_bytes_of_type(tp, bytes::bytes_data(&r));
    }
    if errs.is_some() {
        if pystr::is_str(x) {
            raise!(TypeError, "string argument without an encoding");
        }
        raise!(TypeError, "errors without a string argument");
    }
    // __bytes__
    if let Some(f) = ab::lookup_special(x, crate::name!("__bytes__"))? {
        let r = ab::call0(&f)?;
        if !bytes::is_bytes(&r) {
            raise!(TypeError, "__bytes__ returned non-bytes (type {})", tname(&r));
        }
        if tp == bytes::bytes_type() {
            return Ok(r);
        }
        return bytes::new_bytes_of_type(tp, bytes::bytes_data(&r));
    }
    if pystr::is_str(x) {
        raise!(TypeError, "string argument without an encoding");
    }
    if ab::has_index(x) {
        match ab::index_as_ssize(x, Some(crate::types::exceptions::OverflowError())) {
            Ok(n) => {
                if n < 0 {
                    raise!(ValueError, "negative count");
                }
                if tp == bytes::bytes_type() {
                    return bytes::try_alloc_bytes(n as usize);
                }
                let v = vec_zeroed(n as usize)?;
                return bytes::new_bytes_of_type(tp, &v);
            }
            Err(e) => {
                if !exc_matches(crate::types::exceptions::TypeError()) {
                    return Err(e);
                }
                clear_exc();
            }
        }
    }
    if tp == bytes::bytes_type() {
        return bytes_from_object(x);
    }
    let d = bytes_data_from_object(x)?;
    bytes::new_bytes_of_type(tp, &d)
}

pub fn vec_zeroed(n: usize) -> PyResult<Vec<u8>> {
    if n > bytes::MAX_SIZE {
        return Err(memory_error());
    }
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| memory_error())?;
    v.resize(n, 0);
    Ok(v)
}

unsafe extern "C" fn bytes_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (v, names) = crate::args::kwdict_to_vectorcall(&*args, kw);
        let refs: Vec<&PyObject> = v.iter().map(|r| &**r).collect();
        static P: crate::args::Params = crate::args::Params::new("bytes", &["source", "encoding", "errors"], 0, 0);
        let r = (|| {
            let [x, enc, errs] = crate::args::parse::<3>(&P, &refs, names.as_deref())?;
            bytes_new_impl(tp, x, enc, errs)
        })();
        into_c(r)
    }
}

// ====================================================================== slots

fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}

pub fn cmp_result(c: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => c == Less,
        Py_LE => c != Greater,
        Py_EQ => c == Equal,
        Py_NE => c != Equal,
        Py_GT => c == Greater,
        _ => c != Less,
    }
}

unsafe extern "C" fn bytes_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { pystr::new_str(&bytes_repr_string(bytes::bytes_data(&*o), "b", "")).into_raw() }
}
unsafe extern "C" fn bytes_str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if crate::modules::sys::bytes_warning() > 0 {
            if crate::modules::warnings::warn(crate::types::exceptions::BytesWarning(), "str() on a bytes instance", 1).is_err() {
                return std::ptr::null_mut();
            }
        }
        bytes_repr(o)
    }
}
unsafe extern "C" fn bytes_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { bytes::bytes_hash(&*o) }
}

/// BytesWarning for comparisons with str/int under -b.
pub fn cmp_warning(b: &PyObject, op: c_int, what: &str) -> PyResult<()> {
    if (op == Py_EQ || op == Py_NE) && crate::modules::sys::bytes_warning() > 0 {
        if pystr::is_str(b) {
            crate::modules::warnings::warn(crate::types::exceptions::BytesWarning(), &format!("Comparison between {} and string", what), 1)?;
        } else if int::is_int(b) && what == "bytes" {
            crate::modules::warnings::warn(crate::types::exceptions::BytesWarning(), "Comparison between bytes and int", 1)?;
        }
    }
    Ok(())
}

unsafe extern "C" fn bytes_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !bytes::is_bytes(a) || !bytes::is_bytes(b) {
            let other = if bytes::is_bytes(a) { b } else { a };
            if cmp_warning(other, op, "bytes").is_err() {
                return std::ptr::null_mut();
            }
            return ni();
        }
        let (x, y) = (bytes::bytes_data(a), bytes::bytes_data(b));
        if (op == Py_EQ || op == Py_NE) && x.len() != y.len() {
            return bool_::new_bool(op == Py_NE).into_raw();
        }
        bool_::new_bool(cmp_result(x.cmp(y), op)).into_raw()
    }
}
unsafe extern "C" fn bytes_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { var_size(&*o) }
}
unsafe extern "C" fn bytes_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let d = bytes::bytes_data(&*o);
        if i < 0 || i as usize >= d.len() {
            let _ = err!(IndexError, "index out of range");
            return std::ptr::null_mut();
        }
        int::new_int(d[i as usize] as i64).into_raw()
    }
}

/// Elements of a slice of `d`.
pub fn slice_bytes(d: &[u8], start: isize, step: isize, len: isize) -> Vec<u8> {
    if len <= 0 {
        return Vec::new();
    }
    if step == 1 {
        return d[start as usize..(start + len) as usize].to_vec();
    }
    let mut v = Vec::with_capacity(len as usize);
    let mut i = start;
    for _ in 0..len {
        v.push(d[i as usize]);
        i = i.wrapping_add(step);
    }
    v
}

fn bytes_subscript_impl(o: &PyObject, k: &PyObject) -> PyResult {
    if slice::is_slice(k) {
        let (start, _s, step, len) = slice::indices(k, bytes::bytes_len(o) as isize)?;
        let d = bytes::bytes_data(o);
        if start == 0 && step == 1 && len as usize == d.len() && bytes::is_bytes_exact(o) {
            return Ok(o.to_ref());
        }
        return bytes::try_new_bytes(&slice_bytes(d, start, step, len));
    }
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError()))?;
        let d = bytes::bytes_data(o);
        if i < 0 {
            i += d.len() as isize;
        }
        if i < 0 || i as usize >= d.len() {
            raise!(IndexError, "index out of range");
        }
        return Ok(int::new_int(d[i as usize] as i64));
    }
    raise!(TypeError, "byte indices must be integers or slices, not {}", tname(k));
}
unsafe extern "C" fn bytes_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(bytes_subscript_impl(&*o, &*k)) }
}
unsafe extern "C" fn bytes_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| {
            let va = BufferView::simple(a);
            let vb = BufferView::simple(b);
            let (va, vb) = match (va, vb) {
                (Ok(x), Ok(y)) => (x, y),
                _ => {
                    clear_exc();
                    raise!(TypeError, "can't concat {} to {}", tname(b), tname(a));
                }
            };
            if vb.len() == 0 && bytes::is_bytes_exact(a) {
                return Ok(a.to_ref());
            }
            if va.len() == 0 && bytes::is_bytes_exact(b) {
                return Ok(b.to_ref());
            }
            let total = va.len().checked_add(vb.len()).filter(|&t| t <= bytes::MAX_SIZE).ok_or_else(memory_error)?;
            let r = bytes::try_alloc_bytes(total)?;
            std::ptr::copy_nonoverlapping(va.as_bytes().as_ptr(), bytes::bytes_ptr(&r), va.len());
            std::ptr::copy_nonoverlapping(vb.as_bytes().as_ptr(), bytes::bytes_ptr(&r).add(va.len()), vb.len());
            Ok(r)
        })();
        into_c(r)
    }
}

/// Repeat `d` `n` times.
pub fn repeat_bytes(d: &[u8], n: isize, what: &str) -> PyResult<Vec<u8>> {
    if n <= 0 || d.is_empty() {
        return Ok(Vec::new());
    }
    let total = match d.len().checked_mul(n as usize) {
        Some(t) if t <= bytes::MAX_SIZE => t,
        _ => {
            if what == "bytes" {
                raise!(OverflowError, "repeated bytes are too long");
            }
            return Err(memory_error());
        }
    };
    let mut v = Vec::new();
    v.try_reserve_exact(total).map_err(|_| memory_error())?;
    if d.len() == 1 {
        v.resize(total, d[0]);
    } else {
        v.extend_from_slice(d);
        while v.len() < total {
            let k = (total - v.len()).min(v.len());
            v.extend_from_within(..k);
        }
    }
    Ok(v)
}

unsafe extern "C" fn bytes_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if n == 1 && bytes::is_bytes_exact(a) {
            return a.to_ref().into_raw();
        }
        into_c(repeat_bytes(bytes::bytes_data(a), n, "bytes").and_then(|v| bytes::try_new_bytes(&v)))
    }
}
unsafe extern "C" fn bytes_contains(o: *mut PyObject, x: *mut PyObject) -> c_int {
    unsafe {
        match contains(&*o, &*x) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn bytes_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let r = buffer::fill_info(view, o, bytes::bytes_ptr(&*o) as *mut _, var_size(&*o), true, flags);
        int_into_c(r)
    }
}
unsafe extern "C" fn bytes_mod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if !bytes::is_bytes(a) {
            return ni();
        }
        into_c(percent_format(bytes::bytes_data(a), &*b).and_then(|v| bytes::try_new_bytes(&v)))
    }
}
unsafe extern "C" fn bytes_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { new_bytes_iter(&*o).into_raw() }
}
unsafe extern "C" fn bytes_dealloc(o: *mut PyObject) {
    unsafe { crate::types::typeobject::generic_free_and_type_decref(o, (*o).type_ptr()) }
}

// ---------------------------------------------------------------------- methods

fn m_getnewargs(o: &PyObject) -> PyResult {
    let b = bytes::try_new_bytes(bytes::bytes_data(o))?;
    Ok(tuple::new_tuple(vec![b]))
}
fn m_bytes_dunder(o: &PyObject) -> PyResult {
    if bytes::is_bytes_exact(o) {
        return Ok(o.to_ref());
    }
    bytes::try_new_bytes(bytes::bytes_data(o))
}
fn m_sizeof(o: &PyObject) -> PyResult {
    let tp = o.ty();
    Ok(int::new_int(tp.tp_basicsize as i64 + var_size(o) as i64 * tp.tp_itemsize as i64))
}

// ---------------------------------------------------------------------- iterator

#[repr(C)]
pub struct BytesIter {
    pub ob_base: PyObject,
    pub index: isize,
    pub seq: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyBytesIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn new_bytes_iter(seq: &PyObject) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PyBytesIter_Type, std::mem::size_of::<BytesIter>());
        (*(op as *mut BytesIter)).seq = seq.to_ref().into_raw();
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
unsafe extern "C" fn bytesiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut BytesIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        let d = bytes::bytes_data(&*it.seq);
        if (it.index as usize) < d.len() {
            let v = d[it.index as usize];
            it.index += 1;
            return int::new_int(v as i64).into_raw();
        }
        let s = std::mem::replace(&mut it.seq, std::ptr::null_mut());
        decref(s);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn bytesiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut BytesIter)).seq);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn bytesiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut BytesIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
pub unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn bytesiter_length_hint(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *mut BytesIter) };
    if it.seq.is_null() {
        return Ok(int::new_int(0));
    }
    let n = bytes::bytes_len(unsafe { &*it.seq }) as isize - it.index;
    Ok(int::new_int(n.max(0) as i64))
}
/// (iter, (seq,), index) / (iter, ((),)) — shared by the bytes-like iterators.
pub fn iter_reduce(seq: *mut PyObject, index: isize) -> PyResult {
    let it = crate::builtins::builtin("iter")?;
    if seq.is_null() {
        return Ok(tuple::new_tuple(vec![it, tuple::new_tuple(vec![tuple::empty_tuple()])]));
    }
    let s = unsafe { (*seq).to_ref() };
    Ok(tuple::new_tuple(vec![it, tuple::new_tuple(vec![s]), int::new_int(index as i64)]))
}
fn bytesiter_reduce(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *mut BytesIter) };
    iter_reduce(it.seq, it.index)
}
fn bytesiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = unsafe { &mut *(o.as_ptr() as *mut BytesIter) };
    let mut idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        let n = bytes::bytes_len(unsafe { &*it.seq }) as isize;
        if idx < 0 {
            idx = 0;
        } else if idx > n {
            idx = n;
        }
        it.index = idx;
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------------- type

static mut BYTES_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut BYTES_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut BYTES_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut BYTES_BUF: PyBufferProcs = PyBufferProcs::ZERO;

pub const BYTES_DOC: &str = "bytes(iterable_of_ints) -> bytes\nbytes(string, encoding[, errors]) -> bytes\nbytes(bytes_or_buffer) -> immutable copy of bytes_or_buffer\nbytes(int) -> bytes object of size given by the parameter initialized with null bytes\nbytes() -> empty bytes object\n\nConstruct an immutable array of bytes from:\n  - an iterable yielding integers in range(256)\n  - a text string encoded using the specified encoding\n  - any object implementing the buffer API.\n  - an integer";

/// Method entries shared by bytes and bytearray (non-mutating methods).
pub fn common_methods() -> Vec<PyMethodDef> {
    use crate::methods::{def, fast, fastkw, noargs, o};
    let mut v = Vec::new();
    v.push(def("capitalize", noargs(m_capitalize), METH_NOARGS, "capitalize($self, /)\n--\n\nB.capitalize() -> copy of B\n\nReturn a copy of B with only its first character capitalized (ASCII)\nand the rest lower-cased."));
    v.push(def("center", fast(m_center), METH_FASTCALL, "center($self, width, fillchar=b' ', /)\n--\n\nReturn a centered string of length width.\n\nPadding is done using the specified fill character."));
    v.push(def("count", fast(m_count), METH_FASTCALL, "count($self, sub[, start[, end]], /)\n--\n\nReturn the number of non-overlapping occurrences of subsection 'sub' in bytes B[start:end]."));
    v.push(def("decode", fastkw(m_decode), METH_FASTCALL | METH_KEYWORDS, "decode($self, /, encoding='utf-8', errors='strict')\n--\n\nDecode the bytes using the codec registered for encoding."));
    v.push(def("endswith", fast(m_endswith), METH_FASTCALL, "endswith($self, suffix[, start[, end]], /)\n--\n\nReturn True if the bytes ends with the specified suffix, False otherwise."));
    v.push(def("expandtabs", fastkw(m_expandtabs), METH_FASTCALL | METH_KEYWORDS, "expandtabs($self, /, tabsize=8)\n--\n\nReturn a copy where all tab characters are expanded using spaces.\n\nIf tabsize is not given, a tab size of 8 characters is assumed."));
    v.push(def("find", fast(m_find), METH_FASTCALL, "find($self, sub[, start[, end]], /)\n--\n\nReturn the lowest index in B where subsection 'sub' is found, such that 'sub' is contained within B[start,end].\n\nReturn -1 on failure."));
    v.push(def("hex", fastkw(m_hex), METH_FASTCALL | METH_KEYWORDS, "hex($self, /, sep=<unrepresentable>, bytes_per_sep=1)\n--\n\nReturn the bytes as a string of two hexadecimal digits per byte.\n\nsep, if given, is a one-character str or bytes placed between groups of\nbytes_per_sep bytes; groups are counted from the right, or from the left\nwhen bytes_per_sep is negative.\n\n>>> data = b'\\x01\\x23\\x45\\x67\\x89'\n>>> data.hex()\n'0123456789'\n>>> data.hex('-')\n'01-23-45-67-89'\n>>> data.hex('-', 2)\n'01-2345-6789'\n>>> data.hex(' ', -2)\n'0123 4567 89'"));
    v.push(def("index", fast(m_index), METH_FASTCALL, "index($self, sub[, start[, end]], /)\n--\n\nReturn the lowest index in B where subsection 'sub' is found, such that 'sub' is contained within B[start,end].\n\nRaise ValueError if the subsection is not found."));
    v.push(def("isalnum", noargs(m_isalnum), METH_NOARGS, "isalnum($self, /)\n--\n\nB.isalnum() -> bool\n\nReturn True if all characters in B are alphanumeric\nand there is at least one character in B, False otherwise."));
    v.push(def("isalpha", noargs(m_isalpha), METH_NOARGS, "isalpha($self, /)\n--\n\nB.isalpha() -> bool\n\nReturn True if all characters in B are alphabetic\nand there is at least one character in B, False otherwise."));
    v.push(def("isascii", noargs(m_isascii), METH_NOARGS, "isascii($self, /)\n--\n\nB.isascii() -> bool\n\nReturn True if B is empty or all characters in B are ASCII,\nFalse otherwise."));
    v.push(def("isdigit", noargs(m_isdigit), METH_NOARGS, "isdigit($self, /)\n--\n\nB.isdigit() -> bool\n\nReturn True if all characters in B are digits\nand there is at least one character in B, False otherwise."));
    v.push(def("islower", noargs(m_islower), METH_NOARGS, "islower($self, /)\n--\n\nB.islower() -> bool\n\nReturn True if all cased characters in B are lowercase and there is\nat least one cased character in B, False otherwise."));
    v.push(def("isspace", noargs(m_isspace), METH_NOARGS, "isspace($self, /)\n--\n\nB.isspace() -> bool\n\nReturn True if all characters in B are whitespace\nand there is at least one character in B, False otherwise."));
    v.push(def("istitle", noargs(m_istitle), METH_NOARGS, "istitle($self, /)\n--\n\nB.istitle() -> bool\n\nReturn True if B is a titlecased string and there is at least one\ncharacter in B, i.e. uppercase characters may only follow uncased\ncharacters and lowercase characters only cased ones. Return False\notherwise."));
    v.push(def("isupper", noargs(m_isupper), METH_NOARGS, "isupper($self, /)\n--\n\nB.isupper() -> bool\n\nReturn True if all cased characters in B are uppercase and there is\nat least one cased character in B, False otherwise."));
    v.push(def("join", o(m_join), METH_O, "join($self, iterable_of_bytes, /)\n--\n\nConcatenate any number of bytes objects.\n\nThe bytes whose method is called is inserted in between each pair."));
    v.push(def("ljust", fast(m_ljust), METH_FASTCALL, "ljust($self, width, fillchar=b' ', /)\n--\n\nReturn a left-justified string of length width.\n\nPadding is done using the specified fill character."));
    v.push(def("lower", noargs(m_lower), METH_NOARGS, "lower($self, /)\n--\n\nB.lower() -> copy of B\n\nReturn a copy of B with all ASCII characters converted to lowercase."));
    v.push(def("lstrip", fast(m_lstrip), METH_FASTCALL, "lstrip($self, bytes=None, /)\n--\n\nStrip leading bytes contained in the argument.\n\nIf the argument is omitted or None, strip leading  ASCII whitespace."));
    v.push(def("maketrans", fast(m_maketrans), METH_FASTCALL | METH_STATIC, "maketrans(frm, to, /)\n--\n\nReturn a translation table usable for the bytes or bytearray translate method."));
    v.push(def("partition", o(m_partition), METH_O, "partition($self, sep, /)\n--\n\nPartition the bytes into three parts using the given separator."));
    v.push(def("removeprefix", o(m_removeprefix), METH_O, "removeprefix($self, prefix, /)\n--\n\nReturn a bytes object with the given prefix string removed if present."));
    v.push(def("removesuffix", o(m_removesuffix), METH_O, "removesuffix($self, suffix, /)\n--\n\nReturn a bytes object with the given suffix string removed if present."));
    v.push(def("replace", fastkw(m_replace), METH_FASTCALL | METH_KEYWORDS, "replace($self, old, new, /, count=-1)\n--\n\nReturn a copy with all occurrences of substring old replaced by new."));
    v.push(def("rfind", fast(m_rfind), METH_FASTCALL, "rfind($self, sub[, start[, end]], /)\n--\n\nReturn the highest index in B where subsection 'sub' is found, such that 'sub' is contained within B[start,end].\n\nReturn -1 on failure."));
    v.push(def("rindex", fast(m_rindex), METH_FASTCALL, "rindex($self, sub[, start[, end]], /)\n--\n\nReturn the highest index in B where subsection 'sub' is found, such that 'sub' is contained within B[start,end].\n\nRaise ValueError if the subsection is not found."));
    v.push(def("rjust", fast(m_rjust), METH_FASTCALL, "rjust($self, width, fillchar=b' ', /)\n--\n\nReturn a right-justified string of length width.\n\nPadding is done using the specified fill character."));
    v.push(def("rpartition", o(m_rpartition), METH_O, "rpartition($self, sep, /)\n--\n\nPartition the bytes into three parts using the given separator."));
    v.push(def("rsplit", fastkw(m_rsplit), METH_FASTCALL | METH_KEYWORDS, "rsplit($self, /, sep=None, maxsplit=-1)\n--\n\nReturn a list of the sections in the bytes, using sep as the delimiter."));
    v.push(def("rstrip", fast(m_rstrip), METH_FASTCALL, "rstrip($self, bytes=None, /)\n--\n\nStrip trailing bytes contained in the argument.\n\nIf the argument is omitted or None, strip trailing ASCII whitespace."));
    v.push(def("split", fastkw(m_split), METH_FASTCALL | METH_KEYWORDS, "split($self, /, sep=None, maxsplit=-1)\n--\n\nReturn a list of the sections in the bytes, using sep as the delimiter."));
    v.push(def("splitlines", fastkw(m_splitlines), METH_FASTCALL | METH_KEYWORDS, "splitlines($self, /, keepends=False)\n--\n\nReturn a list of the lines in the bytes, breaking at line boundaries.\n\nLine breaks are not included in the resulting list unless keepends is given and\ntrue."));
    v.push(def("startswith", fast(m_startswith), METH_FASTCALL, "startswith($self, prefix[, start[, end]], /)\n--\n\nReturn True if the bytes starts with the specified prefix, False otherwise."));
    v.push(def("strip", fast(m_strip), METH_FASTCALL, "strip($self, bytes=None, /)\n--\n\nStrip leading and trailing bytes contained in the argument.\n\nIf the argument is omitted or None, strip leading and trailing ASCII whitespace."));
    v.push(def("swapcase", noargs(m_swapcase), METH_NOARGS, "swapcase($self, /)\n--\n\nB.swapcase() -> copy of B\n\nReturn a copy of B with uppercase ASCII characters converted\nto lowercase ASCII and vice versa."));
    v.push(def("title", noargs(m_title), METH_NOARGS, "title($self, /)\n--\n\nB.title() -> copy of B\n\nReturn a titlecased version of B, i.e. ASCII words start with uppercase\ncharacters, all remaining cased characters have lowercase."));
    v.push(def("translate", fastkw(m_translate), METH_FASTCALL | METH_KEYWORDS, "translate($self, table, /, delete=b'')\n--\n\nReturn a copy with each character mapped by the given translation table."));
    v.push(def("upper", noargs(m_upper), METH_NOARGS, "upper($self, /)\n--\n\nB.upper() -> copy of B\n\nReturn a copy of B with all ASCII characters converted to uppercase."));
    v.push(def("zfill", o(m_zfill), METH_O, "zfill($self, width, /)\n--\n\nPad a numeric string with zeros on the left, to fill a field of the given width.\n\nThe original string is never truncated."));
    v
}

pub fn init_types() {
    unsafe {
        let t = &mut *bytes::bytes_type();
        t.tp_name = c"bytes".as_ptr();
        t.tp_basicsize = bytes::HEADER as isize + 1;
        t.tp_itemsize = 1;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_BYTES_SUBCLASS;
        t.tp_dealloc = Some(bytes_dealloc);
        t.tp_repr = Some(bytes_repr);
        t.tp_str = Some(bytes_str);
        t.tp_hash = Some(bytes_hash_slot);
        t.tp_richcompare = Some(bytes_richcompare);
        t.tp_iter = Some(bytes_iter);
        t.tp_new = Some(bytes_new);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_free = Some(crate::types::typeobject::generic_free);
        t.tp_doc = crate::methods::cstr_static(BYTES_DOC);
        let s = &mut *(&raw mut BYTES_SEQ);
        s.sq_length = Some(bytes_len_slot);
        s.sq_concat = Some(bytes_concat);
        s.sq_repeat = Some(bytes_repeat);
        s.sq_item = Some(bytes_item);
        s.sq_contains = Some(bytes_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut BYTES_MAP);
        m.mp_length = Some(bytes_len_slot);
        m.mp_subscript = Some(bytes_subscript);
        t.tp_as_mapping = m;
        let n = &mut *(&raw mut BYTES_NUM);
        n.nb_remainder = Some(bytes_mod);
        t.tp_as_number = n;
        let b = &mut *(&raw mut BYTES_BUF);
        b.bf_getbuffer = Some(bytes_getbuffer);
        t.tp_as_buffer = b;
        let mut v = common_methods();
        use crate::methods::{def, noargs, o};
        v.push(def("__getnewargs__", noargs(m_getnewargs), METH_NOARGS, ""));
        v.push(def("__bytes__", noargs(m_bytes_dunder), METH_NOARGS, "__bytes__($self, /)\n--\n\nConvert this value to exact type bytes."));
        v.push(def("__sizeof__", noargs(m_sizeof), METH_NOARGS, "__sizeof__($self, /)\n--\n\nReturn the size of the bytes in memory, in bytes."));
        v.push(def("fromhex", o(m_fromhex), METH_O | METH_CLASS, "fromhex($type, string, /)\n--\n\nCreate a bytes object from a string of hexadecimal numbers.\n\nSpaces between two numbers are accepted.\nExample: bytes.fromhex('B9 01EF') -> b'\\\\xb9\\\\x01\\\\xef'."));
        t.tp_methods = crate::methods::table(v);

        let t = &mut *(&raw mut PyBytesIter_Type);
        t.tp_name = c"bytes_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<BytesIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(bytesiter_dealloc);
        t.tp_traverse = Some(bytesiter_traverse);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(bytesiter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, bytesiter_length_hint, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, bytesiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, bytesiter_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    crate::types::typeobject::type_ready(&raw mut PyBytesIter_Type)
}
