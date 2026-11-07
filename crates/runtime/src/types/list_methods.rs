//! list methods, including `sort` (a timsort with galloping merges and the
//! powersort merge policy, like CPython's listsort).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::list;
use crate::types::typeobject::*;
use crate::types::{exceptions, float, int, none, str as pystr, tuple};

// ================================================================== methods

fn append_m(o: &PyObject, v: &PyObject) -> PyResult {
    if !list::try_append(o, v.to_ref()) {
        return Err(crate::errors::memory_error());
    }
    Ok(none::none_ref())
}
fn extend_m(o: &PyObject, v: &PyObject) -> PyResult {
    list::extend(o, v)?;
    Ok(none::none_ref())
}
fn pop_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 0, 1)?;
    let i = match args.first() {
        Some(a) => ab::index_as_ssize(a, Some(exceptions::IndexError()))?,
        None => -1,
    };
    match list::pop_index(o, i) {
        Ok(v) => Ok(v),
        Err(0) => raise!(IndexError, "pop from empty list"),
        Err(_) => raise!(IndexError, "pop index out of range"),
    }
}
fn insert_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", args, 2, 2)?;
    let n = list::list_len(o) as isize;
    let mut i = ab::index_as_ssize(args[0], None)?;
    if i < 0 {
        i = (i + n).max(0);
    }
    let i = i.min(n);
    list::insert(o, i as usize, args[1].to_ref());
    Ok(none::none_ref())
}
fn sort_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("sort", &["key", "reverse"], 0, 0).kwonly(2);
    let [key, reverse] = crate::args::parse::<2>(&P, args, kw)?;
    let key = key.filter(|k| !k.is(none::none()));
    let rev = match reverse {
        Some(r) => {
            if crate::types::float::is_float(r) {
                raise!(TypeError, "'float' object cannot be interpreted as an integer");
            }
            ab::index_as_ssize(r, None).map(|x| x != 0).or_else(|_| {
                clear_exc();
                ab::is_true(r)
            })?
        }
        None => false,
    };
    sort_list(o, key, rev)?;
    Ok(none::none_ref())
}
fn reverse_m(o: &PyObject) -> PyResult {
    let mut v = list::take_items(o);
    v.reverse();
    list::release_items(list::restore_items(o, v));
    Ok(none::none_ref())
}
fn index_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    let n = list::list_len(o) as isize;
    let norm = |v: &PyObject| -> PyResult<isize> {
        let mut x = crate::types::slice::slice_index(v)?;
        if x < 0 {
            x += n;
            if x < 0 {
                x = 0;
            }
        }
        Ok(x)
    };
    let start = match args.get(1) {
        Some(v) => norm(v)?,
        None => 0,
    };
    let stop = match args.get(2) {
        Some(v) => norm(v)?,
        None => isize::MAX,
    };
    let mut i = start;
    while i < stop {
        let it = match list::get(o, i as usize) {
            Some(x) => x,
            None => break,
        };
        if it.is(args[0]) || ab::rich_compare_bool(&it, args[0], Py_EQ)? {
            return Ok(int::new_int(i as i64));
        }
        i += 1;
    }
    raise!(ValueError, "list.index(x): x not in list");
}
fn count_m(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0;
    let mut i = 0;
    while let Some(it) = list::get(o, i) {
        if it.is(v) || ab::rich_compare_bool(&it, v, Py_EQ)? {
            n += 1;
        }
        i += 1;
    }
    Ok(int::new_int(n))
}
fn remove_m(o: &PyObject, v: &PyObject) -> PyResult {
    let mut i = 0;
    while let Some(it) = list::get(o, i) {
        if it.is(v) || ab::rich_compare_bool(&it, v, Py_EQ)? {
            if i < list::list_len(o) {
                drop(list::pop(o, i));
            }
            return Ok(none::none_ref());
        }
        i += 1;
    }
    raise!(ValueError, "list.remove(x): x not in list");
}
fn clear_m(o: &PyObject) -> PyResult {
    list::clear(o);
    Ok(none::none_ref())
}
fn copy_m(o: &PyObject) -> PyResult {
    Ok(list::new_list(list::list_to_vec(o)))
}
fn reversed_m(o: &PyObject) -> PyResult {
    list::list_reversed(o)
}
fn sizeof_m(o: &PyObject) -> PyResult {
    let l = unsafe { &*(o.as_ptr() as *const PyListObject) };
    Ok(int::new_int((o.ty().tp_basicsize as usize + l.allocated.max(0) as usize * 8) as i64))
}
fn getitem_m(o: &PyObject, k: &PyObject) -> PyResult {
    // list's own subscript, never the (possibly overridden) slot of a subclass
    crate::types::list::list_getitem(o, k)
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

pub fn method_table() -> *mut PyMethodDef {
    crate::mdefs![
        ("__getitem__", O, getitem_m, "__getitem__($self, index, /)\n--\n\nReturn self[index]."; METH_COEXIST),
        ("__reversed__", NOARGS, reversed_m, "__reversed__($self, /)\n--\n\nReturn a reverse iterator over the list."),
        ("__sizeof__", NOARGS, sizeof_m, "__sizeof__($self, /)\n--\n\nReturn the size of the list in memory, in bytes."),
        ("clear", NOARGS, clear_m, "clear($self, /)\n--\n\nRemove all items from list."),
        ("copy", NOARGS, copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of the list."),
        ("append", O, append_m, "append($self, object, /)\n--\n\nAppend object to the end of the list."),
        ("insert", FAST, insert_m, "insert($self, index, object, /)\n--\n\nInsert object before index."),
        ("extend", O, extend_m, "extend($self, iterable, /)\n--\n\nExtend list by appending elements from the iterable."),
        ("pop", FAST, pop_m, "pop($self, index=-1, /)\n--\n\nRemove and return item at index (default last).\n\nRaises IndexError if list is empty or index is out of range."),
        ("remove", O, remove_m, "remove($self, value, /)\n--\n\nRemove first occurrence of value.\n\nRaises ValueError if the value is not present."),
        ("index", FAST, index_m, "index($self, value, start=0, stop=sys.maxsize, /)\n--\n\nReturn first index of value.\n\nRaises ValueError if the value is not present."),
        ("count", O, count_m, "count($self, value, /)\n--\n\nReturn number of occurrences of value."),
        ("reverse", NOARGS, reverse_m, "reverse($self, /)\n--\n\nReverse *IN PLACE*."),
        ("sort", FASTKW, sort_m, "sort($self, /, *, key=None, reverse=False)\n--\n\nSort the list in ascending order and return None.\n\nThe sort is in-place (i.e. the list itself is modified) and stable (i.e. the\norder of two equal elements is maintained).\n\nIf a key function is given, apply it once to each list item and sort them,\nascending or descending, according to their function values.\n\nThe reverse flag can be set to sort in descending order."),
        ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
    ]
}

// ================================================================== sort

/// Comparison strategy selected by a pre-scan of the keys.
#[derive(Clone, Copy)]
enum CmpKind {
    Generic,
    Str,
    CompactInt,
    Int,
    Float,
}

struct Cmp {
    kind: CmpKind,
    failed: bool,
}

impl Cmp {
    #[inline]
    fn lt(&mut self, a: *mut PyObject, b: *mut PyObject) -> bool {
        if self.failed {
            return false;
        }
        unsafe {
            let (x, y) = (&*a, &*b);
            match self.kind {
                CmpKind::CompactInt => int::compact_value(x) < int::compact_value(y),
                CmpKind::Int => crate::types::int_methods::int_cmp(x, y) == std::cmp::Ordering::Less,
                CmpKind::Float => float::float_value(x) < float::float_value(y),
                CmpKind::Str => pystr::str_cmp(x, y) == std::cmp::Ordering::Less,
                CmpKind::Generic => match ab::rich_compare_bool(x, y, Py_LT) {
                    Ok(b) => b,
                    Err(_) => {
                        self.failed = true;
                        false
                    }
                },
            }
        }
    }
}

fn select_kind(keys: &[*mut PyObject]) -> CmpKind {
    if keys.is_empty() {
        return CmpKind::Generic;
    }
    let t0 = unsafe { (*keys[0]).type_ptr() };
    if !keys.iter().all(|k| unsafe { (**k).type_ptr() } == t0) {
        return CmpKind::Generic;
    }
    if t0 == pystr::str_type() {
        CmpKind::Str
    } else if t0 == int::int_type() {
        if keys.iter().all(|k| int::is_compact(unsafe { &**k })) { CmpKind::CompactInt } else { CmpKind::Int }
    } else if t0 == float::float_type() {
        CmpKind::Float
    } else {
        CmpKind::Generic
    }
}

const MIN_GALLOP: usize = 7;

/// Timsort over `keys`, permuting `vals` (if present) in tandem.
struct Sorter<'a> {
    keys: &'a mut [*mut PyObject],
    vals: Option<&'a mut [*mut PyObject]>,
    cmp: Cmp,
    min_gallop: usize,
    tmpk: Vec<*mut PyObject>,
    tmpv: Vec<*mut PyObject>,
}

#[derive(Clone, Copy)]
struct Run {
    base: usize,
    len: usize,
    power: u32,
}

impl<'a> Sorter<'a> {
    #[inline]
    fn lt(&mut self, a: *mut PyObject, b: *mut PyObject) -> bool {
        self.cmp.lt(a, b)
    }
    fn reverse_range(&mut self, lo: usize, hi: usize) {
        self.keys[lo..hi].reverse();
        if let Some(v) = self.vals.as_deref_mut() {
            v[lo..hi].reverse();
        }
    }
    /// Copy within the arrays (memmove semantics).
    #[inline]
    fn mv(&mut self, src: usize, dst: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.keys.copy_within(src..src + n, dst);
        if let Some(v) = self.vals.as_deref_mut() {
            v.copy_within(src..src + n, dst);
        }
    }
    #[inline]
    fn set_from_tmp(&mut self, tsrc: usize, dst: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.keys[dst..dst + n].copy_from_slice(&self.tmpk[tsrc..tsrc + n]);
        if let Some(v) = self.vals.as_deref_mut() {
            v[dst..dst + n].copy_from_slice(&self.tmpv[tsrc..tsrc + n]);
        }
    }
    #[inline]
    fn set1(&mut self, src: usize, dst: usize) {
        self.keys[dst] = self.keys[src];
        if let Some(v) = self.vals.as_deref_mut() {
            v[dst] = v[src];
        }
    }
    #[inline]
    fn set1_tmp(&mut self, tsrc: usize, dst: usize) {
        self.keys[dst] = self.tmpk[tsrc];
        if let Some(v) = self.vals.as_deref_mut() {
            v[dst] = self.tmpv[tsrc];
        }
    }
    fn to_tmp(&mut self, src: usize, n: usize) {
        self.tmpk.clear();
        self.tmpk.extend_from_slice(&self.keys[src..src + n]);
        if let Some(v) = self.vals.as_deref() {
            self.tmpv.clear();
            self.tmpv.extend_from_slice(&v[src..src + n]);
        }
    }

    /// Binary insertion sort of [lo, hi) where [lo, start) is already sorted.
    fn binarysort(&mut self, lo: usize, hi: usize, mut start: usize) {
        if start == lo {
            start += 1;
        }
        while start < hi {
            let pivot = self.keys[start];
            let pv = self.vals.as_deref().map(|v| v[start]);
            let (mut l, mut r) = (lo, start);
            while l < r {
                let p = l + ((r - l) >> 1);
                if self.lt(pivot, self.keys[p]) {
                    r = p;
                } else {
                    l = p + 1;
                }
            }
            self.mv(l, l + 1, start - l);
            self.keys[l] = pivot;
            if let (Some(v), Some(pv)) = (self.vals.as_deref_mut(), pv) {
                v[l] = pv;
            }
            start += 1;
        }
    }

    /// Length of the run starting at lo; descending runs are reversed in place.
    fn count_run(&mut self, lo: usize, hi: usize) -> usize {
        if lo + 1 >= hi {
            return hi - lo;
        }
        let mut n = 2;
        if self.lt(self.keys[lo + 1], self.keys[lo]) {
            while lo + n < hi && self.lt(self.keys[lo + n], self.keys[lo + n - 1]) {
                n += 1;
            }
            self.reverse_range(lo, lo + n);
        } else {
            while lo + n < hi && !self.lt(self.keys[lo + n], self.keys[lo + n - 1]) {
                n += 1;
            }
        }
        n
    }

    /// Leftmost position in a[base..base+n] where key belongs (a[k-1] < key <= a[k]).
    fn gallop_left(&mut self, key: *mut PyObject, base: usize, n: usize, hint: usize, from_tmp: bool) -> usize {
        let get = |s: &Self, i: usize| if from_tmp { s.tmpk[base + i] } else { s.keys[base + i] };
        let mut ofs: isize = 1;
        let mut lastofs: isize = 0;
        let hint = hint as isize;
        let n = n as isize;
        let g = get(self, hint as usize);
        if self.lt(g, key) {
            let maxofs = n - hint;
            while ofs < maxofs {
                let g = get(self, (hint + ofs) as usize);
                if self.lt(g, key) {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            if ofs > maxofs {
                ofs = maxofs;
            }
            lastofs += hint;
            ofs += hint;
        } else {
            let maxofs = hint + 1;
            while ofs < maxofs {
                let g = get(self, (hint - ofs) as usize);
                if self.lt(g, key) {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            if ofs > maxofs {
                ofs = maxofs;
            }
            let k = lastofs;
            lastofs = hint - ofs;
            ofs = hint - k;
        }
        lastofs += 1;
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            let g = get(self, m as usize);
            if self.lt(g, key) {
                lastofs = m + 1;
            } else {
                ofs = m;
            }
        }
        ofs as usize
    }

    /// Rightmost position in a[base..base+n] where key belongs (a[k-1] <= key < a[k]).
    fn gallop_right(&mut self, key: *mut PyObject, base: usize, n: usize, hint: usize, from_tmp: bool) -> usize {
        let get = |s: &Self, i: usize| if from_tmp { s.tmpk[base + i] } else { s.keys[base + i] };
        let mut ofs: isize = 1;
        let mut lastofs: isize = 0;
        let hint = hint as isize;
        let n = n as isize;
        let g = get(self, hint as usize);
        if self.lt(key, g) {
            let maxofs = hint + 1;
            while ofs < maxofs {
                let g = get(self, (hint - ofs) as usize);
                if self.lt(key, g) {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            if ofs > maxofs {
                ofs = maxofs;
            }
            let k = lastofs;
            lastofs = hint - ofs;
            ofs = hint - k;
        } else {
            let maxofs = n - hint;
            while ofs < maxofs {
                let g = get(self, (hint + ofs) as usize);
                if self.lt(key, g) {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            if ofs > maxofs {
                ofs = maxofs;
            }
            lastofs += hint;
            ofs += hint;
        }
        lastofs += 1;
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            let g = get(self, m as usize);
            if self.lt(key, g) {
                ofs = m;
            } else {
                lastofs = m + 1;
            }
        }
        ofs as usize
    }

    /// Merge runs [pa, pa+na) and [pa+na, pa+na+nb) with na <= nb.
    fn merge_lo(&mut self, pa0: usize, mut na: usize, mut nb: usize) {
        self.to_tmp(pa0, na);
        let mut dest = pa0;
        let mut pa = 0usize; // into tmp
        let mut pb = pa0 + na;
        self.set1(pb, dest);
        dest += 1;
        pb += 1;
        nb -= 1;
        'outer: {
            if nb == 0 {
                break 'outer;
            }
            if na == 1 {
                // CopyB
                self.mv(pb, dest, nb);
                self.set1_tmp(pa, dest + nb);
                return;
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut acount = 0usize;
                let mut bcount = 0usize;
                loop {
                    if self.lt(self.keys[pb], self.tmpk[pa]) {
                        self.set1(pb, dest);
                        dest += 1;
                        pb += 1;
                        nb -= 1;
                        if nb == 0 {
                            break 'outer;
                        }
                        bcount += 1;
                        acount = 0;
                        if bcount >= min_gallop {
                            break;
                        }
                    } else {
                        self.set1_tmp(pa, dest);
                        dest += 1;
                        pa += 1;
                        na -= 1;
                        if na == 1 {
                            self.mv(pb, dest, nb);
                            self.set1_tmp(pa, dest + nb);
                            self.min_gallop = min_gallop;
                            return;
                        }
                        acount += 1;
                        bcount = 0;
                        if acount >= min_gallop {
                            break;
                        }
                    }
                }
                min_gallop += 1;
                loop {
                    if min_gallop > 1 {
                        min_gallop -= 1;
                    }
                    self.min_gallop = min_gallop;
                    let k = self.gallop_right(self.keys[pb], pa, na, 0, true);
                    acount = k;
                    if k > 0 {
                        self.set_from_tmp(pa, dest, k);
                        dest += k;
                        pa += k;
                        na -= k;
                        if na == 1 {
                            self.mv(pb, dest, nb);
                            self.set1_tmp(pa, dest + nb);
                            return;
                        }
                        if na == 0 {
                            break 'outer;
                        }
                    }
                    self.set1(pb, dest);
                    dest += 1;
                    pb += 1;
                    nb -= 1;
                    if nb == 0 {
                        break 'outer;
                    }
                    let k = self.gallop_left(self.tmpk[pa], pb, nb, 0, false);
                    bcount = k;
                    if k > 0 {
                        self.mv(pb, dest, k);
                        dest += k;
                        pb += k;
                        nb -= k;
                        if nb == 0 {
                            break 'outer;
                        }
                    }
                    self.set1_tmp(pa, dest);
                    dest += 1;
                    pa += 1;
                    na -= 1;
                    if na == 1 {
                        self.mv(pb, dest, nb);
                        self.set1_tmp(pa, dest + nb);
                        return;
                    }
                    if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                        break;
                    }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        }
        // Succeed: copy what is left of A
        if na > 0 {
            self.set_from_tmp(pa, dest, na);
        }
    }

    /// Merge runs [pa0, pa0+na) and [pa0+na, pa0+na+nb) with na > nb (from the right).
    fn merge_hi(&mut self, pa0: usize, mut na: usize, mut nb: usize) {
        let pb0 = pa0 + na;
        self.to_tmp(pb0, nb);
        let mut dest: isize = (pb0 + nb - 1) as isize;
        let mut pa: isize = (pb0 - 1) as isize; // into keys
        let mut pb: isize = nb as isize - 1; // into tmp
        let base = pa0 as isize;
        self.set1(pa as usize, dest as usize);
        dest -= 1;
        pa -= 1;
        na -= 1;
        macro_rules! copy_a {
            () => {{
                // nb == 1: move the rest of A up, then put the last B element
                dest -= na as isize;
                pa -= na as isize;
                self.mv((pa + 1) as usize, (dest + 1) as usize, na);
                self.set1_tmp(pb as usize, dest as usize);
                return;
            }};
        }
        'outer: {
            if na == 0 {
                break 'outer;
            }
            if nb == 1 {
                copy_a!();
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut acount = 0usize;
                let mut bcount = 0usize;
                loop {
                    if self.lt(self.tmpk[pb as usize], self.keys[pa as usize]) {
                        self.set1(pa as usize, dest as usize);
                        dest -= 1;
                        pa -= 1;
                        na -= 1;
                        if na == 0 {
                            break 'outer;
                        }
                        acount += 1;
                        bcount = 0;
                        if acount >= min_gallop {
                            break;
                        }
                    } else {
                        self.set1_tmp(pb as usize, dest as usize);
                        dest -= 1;
                        pb -= 1;
                        nb -= 1;
                        if nb == 1 {
                            self.min_gallop = min_gallop;
                            copy_a!();
                        }
                        bcount += 1;
                        acount = 0;
                        if bcount >= min_gallop {
                            break;
                        }
                    }
                }
                min_gallop += 1;
                loop {
                    if min_gallop > 1 {
                        min_gallop -= 1;
                    }
                    self.min_gallop = min_gallop;
                    // A elements > B[pb]
                    let abase = (pa + 1) as usize - na;
                    let k = self.gallop_right(self.tmpk[pb as usize], abase, na, na - 1, false);
                    let k = na - k;
                    acount = k;
                    if k > 0 {
                        dest -= k as isize;
                        pa -= k as isize;
                        self.mv((pa + 1) as usize, (dest + 1) as usize, k);
                        na -= k;
                        if na == 0 {
                            break 'outer;
                        }
                    }
                    self.set1_tmp(pb as usize, dest as usize);
                    dest -= 1;
                    pb -= 1;
                    nb -= 1;
                    if nb == 1 {
                        copy_a!();
                    }
                    // B elements >= A[pa]
                    let k = self.gallop_left(self.keys[pa as usize], 0, nb, nb - 1, true);
                    let k = nb - k;
                    bcount = k;
                    if k > 0 {
                        dest -= k as isize;
                        pb -= k as isize;
                        self.set_from_tmp((pb + 1) as usize, (dest + 1) as usize, k);
                        nb -= k;
                        if nb == 1 {
                            copy_a!();
                        }
                        if nb == 0 {
                            break 'outer;
                        }
                    }
                    self.set1(pa as usize, dest as usize);
                    dest -= 1;
                    pa -= 1;
                    na -= 1;
                    if na == 0 {
                        break 'outer;
                    }
                    if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                        break;
                    }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        }
        let _ = base;
        // Succeed: copy what is left of B
        if nb > 0 {
            self.set_from_tmp(0, (dest + 1) as usize - nb, nb);
        }
    }

    fn merge_at(&mut self, runs: &mut Vec<Run>, i: usize) {
        let a = runs[i];
        let b = runs[i + 1];
        runs[i].len = a.len + b.len;
        runs.remove(i + 1);
        let (mut pa, mut na) = (a.base, a.len);
        let (pb, mut nb) = (b.base, b.len);
        // where does b[0] go in a?
        let k = self.gallop_right(self.keys[pb], pa, na, 0, false);
        pa += k;
        na -= k;
        if na == 0 {
            return;
        }
        // where does a[-1] go in b?
        nb = self.gallop_left(self.keys[pa + na - 1], pb, nb, nb - 1, false);
        if nb == 0 {
            return;
        }
        if na <= nb {
            self.merge_lo(pa, na, nb);
        } else {
            self.merge_hi(pa, na, nb);
        }
    }

    fn sort(&mut self) {
        let n = self.keys.len();
        if n < 2 {
            return;
        }
        let minrun = {
            let mut r = 0;
            let mut m = n;
            while m >= 64 {
                r |= m & 1;
                m >>= 1;
            }
            m + r
        };
        let mut runs: Vec<Run> = Vec::new();
        let mut lo = 0;
        while lo < n {
            let mut rl = self.count_run(lo, n);
            if rl < minrun {
                let force = minrun.min(n - lo);
                self.binarysort(lo, lo + force, lo + rl);
                rl = force;
            }
            // powersort merge policy
            if let Some(last) = runs.last().copied() {
                let power = powerloop(last.base, last.len, rl, n);
                while runs.len() > 1 && runs[runs.len() - 2].power > power {
                    let i = runs.len() - 2;
                    self.merge_at(&mut runs, i);
                }
                let l = runs.len();
                runs[l - 1].power = power;
            }
            runs.push(Run { base: lo, len: rl, power: 0 });
            lo += rl;
        }
        while runs.len() > 1 {
            let mut i = runs.len() - 2;
            if i > 0 && runs[i - 1].len < runs[i + 1].len {
                i -= 1;
            }
            self.merge_at(&mut runs, i);
        }
    }
}

fn powerloop(s1: usize, n1: usize, n2: usize, n: usize) -> u32 {
    let mut result = 0;
    let mut a = 2 * s1 + n1;
    let mut b = a + n1 + n2;
    loop {
        result += 1;
        if a >= n {
            a -= n;
            b -= n;
        } else if b >= n {
            break;
        }
        a <<= 1;
        b <<= 1;
    }
    result
}

/// Sort the list in place (list.sort semantics).
pub fn sort_list(l: &PyObject, key: Option<&PyObject>, reverse: bool) -> PyResult<()> {
    let mut items = list::take_items(l);
    // compute keys
    let mut keys_owned: Vec<PyRef> = Vec::new();
    if let Some(k) = key {
        keys_owned.reserve(items.len());
        for &it in items.iter() {
            match ab::call(k, &[unsafe { &*it }]) {
                Ok(v) => keys_owned.push(v),
                Err(e) => {
                    list::release_items(list::restore_items(l, items));
                    return Err(e);
                }
            }
        }
    }
    if reverse {
        items.reverse();
        keys_owned.reverse();
    }
    let failed;
    {
        let mut keys: Vec<*mut PyObject> = if key.is_some() { keys_owned.iter().map(|k| k.as_ptr()).collect() } else { Vec::new() };
        let kind = select_kind(if key.is_some() { &keys } else { &items });
        let mut s = if key.is_some() {
            Sorter { keys: &mut keys, vals: Some(&mut items), cmp: Cmp { kind, failed: false }, min_gallop: MIN_GALLOP, tmpk: Vec::new(), tmpv: Vec::new() }
        } else {
            Sorter { keys: &mut items, vals: None, cmp: Cmp { kind, failed: false }, min_gallop: MIN_GALLOP, tmpk: Vec::new(), tmpv: Vec::new() }
        };
        s.sort();
        failed = s.cmp.failed;
    }
    if reverse {
        items.reverse();
    }
    let saved_exc = if failed { take_exc() } else { None };
    // the keys go first, while the list is still detached: their finalizers may
    // mutate it, which counts as a modification during the sort
    drop(keys_owned);
    let modified = list::list_len(l) != 0 || unsafe { (*(l.as_ptr() as *const PyListObject)).allocated } != -1;
    let extra = list::restore_items(l, items);
    list::release_items(extra);
    if let Some(e) = saved_exc {
        set_exc(e);
        return Err(PyErr);
    }
    if modified {
        raise!(ValueError, "list modified during sort");
    }
    Ok(())
}

#[allow(dead_code)]
fn _unused(o: &PyObject) -> String {
    let _ = tuple::empty_tuple();
    tname(o)
}
