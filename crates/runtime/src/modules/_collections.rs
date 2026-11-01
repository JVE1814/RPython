//! The `_collections` accelerator: deque (+ iterators), defaultdict,
//! _count_elements, _tuplegetter.

use crate::abstract_::{self as ab};
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav, VcArgs, rs, targs};
use crate::object::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::types::typeobject::type_ref;
use crate::{mdefs, name, raise, trav};
use std::collections::VecDeque;
use std::ffi::{c_int, c_void};

type VisitFn<'a> = &'a mut dyn FnMut(*mut PyObject) -> c_int;

#[unsafe(no_mangle)]
pub static mut _PyCollections_Deque_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyCollections_DequeIter_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyCollections_DequeRevIter_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyCollections_DefaultDict_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyCollections_TupleGetter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn deque_type() -> *mut PyTypeObject {
    &raw mut _PyCollections_Deque_Type
}
fn dequeiter_type() -> *mut PyTypeObject {
    &raw mut _PyCollections_DequeIter_Type
}
fn dequereviter_type() -> *mut PyTypeObject {
    &raw mut _PyCollections_DequeRevIter_Type
}
pub fn defaultdict_type() -> *mut PyTypeObject {
    &raw mut _PyCollections_DefaultDict_Type
}
fn tuplegetter_type() -> *mut PyTypeObject {
    &raw mut _PyCollections_TupleGetter_Type
}

// ================================================================== deque

pub struct Deque {
    items: VecDeque<PyRef>,
    maxlen: isize, // -1: unbounded
    state: u64,
}
impl Trav for Deque {
    fn trav(&self, v: VisitFn) -> c_int {
        for r in self.items.iter() {
            let x = v(r.as_ptr());
            if x != 0 {
                return x;
            }
        }
        0
    }
}

fn is_deque(o: &PyObject) -> bool {
    o.is_type(deque_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), deque_type())
}

#[inline]
fn dq(o: &PyObject) -> &mut Deque {
    rs::<Deque>(o)
}

unsafe extern "C" fn deque_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, Deque { items: VecDeque::new(), maxlen: -1, state: 0 }))
}

fn deque_init_impl(o: &PyObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult<()> {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("deque", &["iterable", "maxlen"], 0, 0);
    let [iterable, maxlen] = a.parse::<2>(&p)?;
    let ml = match maxlen {
        Some(m) if !m.is(none::none()) => {
            let v = fc::ssize_arg(&m)?;
            if v < 0 {
                raise!(ValueError, "maxlen must be non-negative");
            }
            v
        }
        _ => -1,
    };
    let d = dq(o);
    d.maxlen = ml;
    if !d.items.is_empty() {
        let old: Vec<PyRef> = d.items.drain(..).collect();
        d.state += 1;
        drop(old);
    }
    if let Some(it) = iterable {
        deque_extend(o, &it)?;
    }
    Ok(())
}

unsafe extern "C" fn deque_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe { int_into_c(deque_init_impl(&*o, args, kw)) }
}

fn push_right(o: &PyObject, v: PyRef) -> Option<PyRef> {
    let d = dq(o);
    d.state += 1;
    if d.maxlen == 0 {
        return Some(v);
    }
    d.items.push_back(v);
    if d.maxlen >= 0 && d.items.len() as isize > d.maxlen {
        return d.items.pop_front();
    }
    None
}

fn push_left(o: &PyObject, v: PyRef) -> Option<PyRef> {
    let d = dq(o);
    d.state += 1;
    if d.maxlen == 0 {
        return Some(v);
    }
    d.items.push_front(v);
    if d.maxlen >= 0 && d.items.len() as isize > d.maxlen {
        return d.items.pop_back();
    }
    None
}

fn deque_append(o: &PyObject, v: &PyObject) -> PyResult {
    drop(push_right(o, v.to_ref()));
    Ok(fc::none())
}
fn deque_appendleft(o: &PyObject, v: &PyObject) -> PyResult {
    drop(push_left(o, v.to_ref()));
    Ok(fc::none())
}

fn extend_impl(o: &PyObject, iterable: &PyObject, left: bool) -> PyResult<()> {
    if iterable.is(o) {
        let snapshot: Vec<PyRef> = dq(o).items.iter().cloned().collect();
        for v in snapshot {
            drop(if left { push_left(o, v) } else { push_right(o, v) });
        }
        return Ok(());
    }
    let it = ab::get_iter(iterable)?;
    if dq(o).maxlen == 0 {
        // consume the iterator
        while ab::iter_next(&it)?.is_some() {}
        return Ok(());
    }
    while let Some(v) = ab::iter_next(&it)? {
        drop(if left { push_left(o, v) } else { push_right(o, v) });
    }
    Ok(())
}

fn deque_extend(o: &PyObject, iterable: &PyObject) -> PyResult {
    extend_impl(o, iterable, false)?;
    Ok(fc::none())
}
fn deque_extendleft(o: &PyObject, iterable: &PyObject) -> PyResult {
    extend_impl(o, iterable, true)?;
    Ok(fc::none())
}

fn deque_pop(o: &PyObject) -> PyResult {
    let d = dq(o);
    match d.items.pop_back() {
        Some(v) => {
            d.state += 1;
            Ok(v)
        }
        None => raise!(IndexError, "pop from an empty deque"),
    }
}
fn deque_popleft(o: &PyObject) -> PyResult {
    let d = dq(o);
    match d.items.pop_front() {
        Some(v) => {
            d.state += 1;
            Ok(v)
        }
        None => raise!(IndexError, "pop from an empty deque"),
    }
}

fn deque_clear_impl(o: &PyObject) {
    let d = dq(o);
    let old: Vec<PyRef> = d.items.drain(..).collect();
    d.state += 1;
    drop(old);
}
fn deque_clear(o: &PyObject) -> PyResult {
    deque_clear_impl(o);
    Ok(fc::none())
}

fn rotate_impl(o: &PyObject, n: isize) {
    let d = dq(o);
    let len = d.items.len();
    if len <= 1 {
        return;
    }
    let n = n.rem_euclid(len as isize) as usize;
    if n == 0 {
        return;
    }
    d.items.rotate_right(n);
    d.state += 1;
}

fn deque_rotate(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("rotate", a, 0, 1)?;
    let n = if a.is_empty() {
        1
    } else {
        if !ab::has_index(a[0]) {
            raise!(TypeError, "'{}' object cannot be interpreted as an integer", fc::tname(a[0]));
        }
        fc::ssize_arg(a[0])?
    };
    rotate_impl(o, n);
    Ok(fc::none())
}

fn deque_reverse(o: &PyObject) -> PyResult {
    let d = dq(o);
    d.items.make_contiguous().reverse();
    d.state += 1;
    Ok(fc::none())
}

/// Iterate over items comparing with `v`; detects mutation.
fn scan(o: &PyObject, v: &PyObject, start: usize, stop: usize, mut f: impl FnMut(usize) -> bool) -> PyResult<()> {
    let state = dq(o).state;
    let mut i = start;
    while i < stop {
        let d = dq(o);
        if i >= d.items.len() {
            break;
        }
        let item = d.items[i].clone();
        let eq = item.is(v) || ab::rich_eq(&item, v)?;
        if dq(o).state != state {
            raise!(RuntimeError, "deque mutated during iteration");
        }
        if eq && !f(i) {
            return Ok(());
        }
        i += 1;
    }
    Ok(())
}

fn deque_count(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0i64;
    let len = dq(o).items.len();
    scan(o, v, 0, len, |_| {
        n += 1;
        true
    })?;
    Ok(int::new_int(n))
}

fn deque_index(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", a, 1, 3)?;
    let len = dq(o).items.len() as isize;
    let norm = |x: &PyObject| -> PyResult<isize> {
        let mut i = ab::index_as_ssize(x, None)?;
        if i < 0 {
            i += len;
            if i < 0 {
                i = 0;
            }
        }
        Ok(i.min(len))
    };
    let start = if a.len() > 1 { norm(a[1])? } else { 0 };
    let stop = if a.len() > 2 { norm(a[2])? } else { len };
    let mut found: Option<usize> = None;
    if start < stop {
        scan(o, a[0], start as usize, stop as usize, |i| {
            found = Some(i);
            false
        })?;
    }
    match found {
        Some(i) => Ok(int::new_int(i as i64)),
        None => Err(err!(ValueError, "{} is not in deque", ab::repr_string(a[0])?)),
    }
}

fn deque_contains_impl(o: &PyObject, v: &PyObject) -> PyResult<bool> {
    let mut found = false;
    let len = dq(o).items.len();
    scan(o, v, 0, len, |_| {
        found = true;
        false
    })?;
    Ok(found)
}

unsafe extern "C" fn deque_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        match deque_contains_impl(&*o, &*v) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}

fn deque_remove(o: &PyObject, v: &PyObject) -> PyResult {
    let state = dq(o).state;
    let len = dq(o).items.len();
    for i in 0..len {
        let item = match dq(o).items.get(i) {
            Some(x) => x.clone(),
            None => break,
        };
        let eq = item.is(v) || ab::rich_eq(&item, v)?;
        if dq(o).state != state {
            raise!(IndexError, "deque mutated during remove().");
        }
        if eq {
            let d = dq(o);
            let r = d.items.remove(i);
            d.state += 1;
            drop(r);
            return Ok(fc::none());
        }
    }
    raise!(ValueError, "{} is not in deque", ab::repr_string(v)?);
}

fn deque_insert(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", a, 2, 2)?;
    let mut i = fc::ssize_arg(a[0])?;
    let d = dq(o);
    let n = d.items.len() as isize;
    if d.maxlen >= 0 && n >= d.maxlen {
        raise!(IndexError, "deque already at its maximum size");
    }
    if i < 0 {
        i += n;
        if i < 0 {
            i = 0;
        }
    }
    if i > n {
        i = n;
    }
    d.items.insert(i as usize, a[1].to_ref());
    d.state += 1;
    Ok(fc::none())
}

unsafe extern "C" fn deque_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dq(&*o).items.len() as Py_ssize_t }
}

unsafe extern "C" fn deque_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let d = dq(&*o);
        if i < 0 || i as usize >= d.items.len() {
            return into_c(Err(err!(IndexError, "deque index out of range")));
        }
        d.items[i as usize].clone().into_raw()
    }
}

unsafe extern "C" fn deque_ass_item(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let d = dq(&*o);
        if i < 0 || i as usize >= d.items.len() {
            let _ = err!(IndexError, "deque index out of range");
            return -1;
        }
        if v.is_null() {
            let r = d.items.remove(i as usize);
            d.state += 1;
            drop(r);
        } else {
            let old = std::mem::replace(&mut d.items[i as usize], (*v).to_ref());
            drop(old);
        }
        0
    }
}

/// Make a copy with the same type and maxlen.
fn deque_copy(o: &PyObject) -> PyResult {
    let d = dq(o);
    if o.is_type(deque_type()) {
        let items = d.items.clone();
        return fc::robj_new(deque_type(), Deque { items, maxlen: d.maxlen, state: 0 });
    }
    let tp = type_ref(o.type_ptr());
    let r = if d.maxlen < 0 {
        ab::call(&tp, &[o])?
    } else {
        let ml = int::new_int(d.maxlen as i64);
        let names = tuple::new_tuple(vec![pystr::new_str("maxlen")]);
        ab::call_kw(&tp, &[o, &ml], Some(&names))?
    };
    if !is_deque(&r) {
        raise!(TypeError, "{}() must return a deque, not {}", fc::repr_type_name(o), fc::tname(&r));
    }
    Ok(r)
}

unsafe extern "C" fn deque_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let (a, b) = (&*a, &*b);
            if !is_deque(b) {
                raise!(TypeError, "can only concatenate deque (not \"{}\") to deque", fc::tname(b));
            }
            let new = deque_copy(a)?;
            extend_impl(&new, b, false)?;
            Ok(new)
        })())
    }
}

unsafe extern "C" fn deque_inplace_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            extend_impl(&*a, &*b, false)?;
            Ok((*a).to_ref())
        })())
    }
}

fn repeat_inplace(o: &PyObject, n: isize) {
    let d = dq(o);
    if n <= 0 || d.items.is_empty() {
        let old: Vec<PyRef> = d.items.drain(..).collect();
        d.state += 1;
        drop(old);
        return;
    }
    if n == 1 {
        return;
    }
    let base: Vec<PyRef> = d.items.iter().cloned().collect();
    let total = base.len().saturating_mul(n as usize);
    let keep = if d.maxlen >= 0 { total.min(d.maxlen as usize) } else { total };
    // only the last `keep` items survive
    let mut out = VecDeque::with_capacity(keep);
    let start = total - keep;
    for k in start..total {
        out.push_back(base[k % base.len()].clone());
    }
    let old = std::mem::replace(&mut d.items, out);
    d.state += 1;
    drop(old);
}

unsafe extern "C" fn deque_inplace_repeat(o: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        repeat_inplace(&*o, n);
        (*o).to_ref().into_raw()
    }
}

unsafe extern "C" fn deque_repeat(o: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let new = deque_copy(&*o)?;
            repeat_inplace(&new, n);
            Ok(new)
        })())
    }
}

unsafe extern "C" fn deque_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_deque(a) || !is_deque(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        into_c((|| -> PyResult {
            let la = ab::sequence_list(a)?;
            let lb = ab::sequence_list(b)?;
            ab::rich_compare(&la, &lb, op)
        })())
    }
}

unsafe extern "C" fn deque_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let _g = match fc::ReprGuard::enter(o) {
                Some(g) => g,
                None => return Ok(pystr::new_str("[...]")),
            };
            let l = ab::sequence_list(o)?;
            let lr = ab::repr_string(&l)?;
            let name = fc::repr_type_name(o);
            let ml = dq(o).maxlen;
            if ml >= 0 {
                Ok(pystr::new_str(&format!("{}({}, maxlen={})", name, lr, ml)))
            } else {
                Ok(pystr::new_str(&format!("{}({})", name, lr)))
            }
        })())
    }
}

fn deque_reduce(o: &PyObject) -> PyResult {
    let tp = type_ref(o.type_ptr());
    let ml = dq(o).maxlen;
    let args = if ml < 0 {
        tuple::empty_tuple()
    } else {
        tuple::tuple2(tuple::empty_tuple(), int::new_int(ml as i64))
    };
    let state = ab::call_method(o, name!("__getstate__"), &[])?;
    let it = ab::get_iter(o)?;
    Ok(tuple::new_tuple(vec![tp, args, state, it]))
}

fn deque_get_maxlen(o: &PyObject) -> PyResult {
    let ml = dq(o).maxlen;
    Ok(if ml < 0 { fc::none() } else { int::new_int(ml as i64) })
}

fn deque_sizeof(o: &PyObject) -> PyResult {
    let d = dq(o);
    let base = std::mem::size_of::<fc::RObj<Deque>>();
    Ok(int::new_int((base + d.items.capacity() * 8) as i64))
}

// ------------------------------------------------------------------ deque iterators

struct DequeIter {
    deque: Option<PyRef>,
    index: usize,
    state: u64,
    counter: usize,
}
impl Trav for DequeIter {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.deque)
    }
}

fn new_deque_iter(tp: *mut PyTypeObject, d: &PyObject, reverse: bool, skip: usize) -> PyResult {
    let dd = dq(d);
    let len = dd.items.len();
    let it = fc::robj_new(
        tp,
        DequeIter { deque: Some(d.to_ref()), index: if reverse { len.wrapping_sub(1) } else { 0 }, state: dd.state, counter: len },
    )?;
    for _ in 0..skip {
        let s = rs::<DequeIter>(&it);
        if s.counter == 0 {
            break;
        }
        s.counter -= 1;
        if reverse {
            s.index = s.index.wrapping_sub(1);
        } else {
            s.index += 1;
        }
    }
    Ok(it)
}

unsafe extern "C" fn deque_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_deque_iter(dequeiter_type(), &*o, false, 0)) }
}
fn deque_reversed(o: &PyObject) -> PyResult {
    new_deque_iter(dequereviter_type(), o, true, 0)
}

fn dequeiter_next_impl(o: &PyObject, reverse: bool) -> PyResult<Option<PyRef>> {
    let s = rs::<DequeIter>(o);
    let d = match &s.deque {
        Some(d) => d.clone(),
        None => return Ok(None),
    };
    if dq(&d).state != s.state {
        s.counter = 0;
        raise!(RuntimeError, "deque mutated during iteration");
    }
    if s.counter == 0 {
        return Ok(None);
    }
    let v = dq(&d).items.get(s.index).cloned();
    s.counter -= 1;
    if reverse {
        s.index = s.index.wrapping_sub(1);
    } else {
        s.index += 1;
    }
    Ok(v)
}

unsafe extern "C" fn dequeiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match dequeiter_next_impl(&*o, false) {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn dequereviter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match dequeiter_next_impl(&*o, true) {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}

unsafe fn dequeiter_new_common(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject, reverse: bool) -> PyResult {
    unsafe { fc::no_kw(if reverse { "_deque_reverse_iterator" } else { "_deque_iterator" }, kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos(if reverse { "_deque_reverse_iterator" } else { "_deque_iterator" }, a, 1, 2)?;
    if !is_deque(a[0]) {
        raise!(TypeError, "expected deque, got {}", fc::tname(a[0]));
    }
    let skip = if a.len() > 1 { fc::ssize_arg(a[1])?.max(0) as usize } else { 0 };
    new_deque_iter(tp, a[0], reverse, skip)
}
unsafe extern "C" fn dequeiter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(dequeiter_new_common(tp, args, kw, false)) }
}
unsafe extern "C" fn dequereviter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(dequeiter_new_common(tp, args, kw, true)) }
}

fn dequeiter_length_hint(o: &PyObject) -> PyResult {
    Ok(int::new_int(rs::<DequeIter>(o).counter as i64))
}

fn dequeiter_reduce(o: &PyObject) -> PyResult {
    let s = rs::<DequeIter>(o);
    let d = s.deque.clone().unwrap();
    let len = dq(&d).items.len();
    let consumed = len.saturating_sub(s.counter);
    Ok(tuple::tuple2(type_ref(o.type_ptr()), tuple::tuple2(d, int::new_int(consumed as i64))))
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

// ================================================================== defaultdict

#[repr(C)]
struct DefDict {
    dict: PyDictObject,
    default_factory: *mut PyObject,
}

fn dd(o: &PyObject) -> &mut DefDict {
    unsafe { &mut *(o.as_ptr() as *mut DefDict) }
}

fn factory_of(o: &PyObject) -> Option<PyRef> {
    let f = dd(o).default_factory;
    if f.is_null() { None } else { Some(unsafe { (*f).to_ref() }) }
}

fn defdict_missing(o: &PyObject, key: &PyObject) -> PyResult {
    let f = match factory_of(o).filter(|f| !f.is(none::none())) {
        Some(f) => f,
        None => {
            return Err(set_err_obj(crate::types::exceptions::KeyError(), key.to_ref()));
        }
    };
    let v = ab::call(&f, &[])?;
    ab::setitem(o, key, Some(&v))?;
    Ok(v)
}

fn defdict_copy(o: &PyObject) -> PyResult {
    let tp = type_ref(o.type_ptr());
    let f = factory_of(o).unwrap_or_else(fc::none);
    ab::call(&tp, &[&f, o])
}

fn defdict_reduce(o: &PyObject) -> PyResult {
    let tp = type_ref(o.type_ptr());
    let args = match factory_of(o).filter(|f| !f.is(none::none())) {
        Some(f) => tuple::new_tuple(vec![f]),
        None => tuple::empty_tuple(),
    };
    let items = ab::call_method_str(o, "items", &[])?;
    let it = ab::get_iter(&items)?;
    Ok(tuple::new_tuple(vec![tp, args, fc::none(), fc::none(), it]))
}

unsafe extern "C" fn defdict_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let base = (*crate::types::dict::dict_type()).tp_repr.unwrap();
            let baserepr = from_c(base(o.as_ptr()))?;
            let fr = match factory_of(o) {
                None => "None".to_string(),
                Some(f) => match fc::ReprGuard::enter(&f) {
                    None => "...".to_string(),
                    Some(_g) => ab::repr_string(&f)?,
                },
            };
            Ok(pystr::new_str(&format!("{}({}, {})", fc::repr_type_name(o), fr, pystr::to_string_lossy(&baserepr))))
        })())
    }
}

unsafe extern "C" fn defdict_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let a = targs(args);
            let mut rest = a;
            let mut factory: Option<PyRef> = None;
            if let Some(f) = a.first() {
                if !f.is(none::none()) && !ab::is_callable(f) {
                    raise!(TypeError, "first argument must be callable or None");
                }
                factory = Some(f.to_ref());
                rest = &a[1..];
            }
            fc::set_field(&raw mut dd(&*o).default_factory, factory);
            let newargs = tuple::new_tuple(rest.iter().map(|x| x.to_ref()).collect());
            let init = (*crate::types::dict::dict_type()).tp_init.unwrap();
            if init(o, newargs.as_ptr(), kw) < 0 {
                return Err(PyErr);
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn defdict_dealloc(o: *mut PyObject) {
    unsafe {
        fc::clear_field(&raw mut dd(&*o).default_factory);
        (*crate::types::dict::dict_type()).tp_dealloc.unwrap()(o);
    }
}
unsafe extern "C" fn defdict_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let r = fc::visit(dd(&*o).default_factory, v, arg);
        if r != 0 {
            return r;
        }
        match (*crate::types::dict::dict_type()).tp_traverse {
            Some(t) => t(o, v, arg),
            None => 0,
        }
    }
}
unsafe extern "C" fn defdict_clear(o: *mut PyObject) -> c_int {
    unsafe {
        fc::clear_field(&raw mut dd(&*o).default_factory);
        match (*crate::types::dict::dict_type()).tp_clear {
            Some(c) => c(o),
            None => 0,
        }
    }
}

fn get_factory(o: &PyObject) -> PyResult {
    Ok(factory_of(o).unwrap_or_else(fc::none))
}
fn set_factory(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    unsafe { fc::set_field(&raw mut dd(o).default_factory, Some(v.map(|x| x.to_ref()).unwrap_or_else(fc::none))) };
    Ok(())
}

unsafe extern "C" fn defdict_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let (a, b) = (&*a, &*b);
            let (slf, other, left) = if a.is_type(defaultdict_type()) || crate::types::typeobject::is_subtype(a.type_ptr(), defaultdict_type()) {
                (a, b, true)
            } else {
                (b, a, false)
            };
            if !dict::is_dict(other) {
                return Ok(none::not_implemented().to_ref());
            }
            let tp = type_ref(slf.type_ptr());
            let f = factory_of(slf).unwrap_or_else(fc::none);
            let new = if left { ab::call(&tp, &[&f, slf])? } else { ab::call(&tp, &[&f, other])? };
            let src = if left { other } else { slf };
            ab::call_method_str(&new, "update", &[src])?;
            Ok(new)
        })())
    }
}

// ================================================================== _tuplegetter

#[repr(C)]
struct TupleGetter {
    ob_base: PyObject,
    index: isize,
    doc: *mut PyObject,
}

unsafe extern "C" fn tuplegetter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            fc::no_kw("_tuplegetter", kw)?;
            let a = targs(args);
            crate::args::check_pos("_tuplegetter", a, 2, 2)?;
            let idx = fc::ssize_arg(a[0])?;
            let op = fc::alloc(tp)?;
            (*(op as *mut TupleGetter)).index = idx;
            (*(op as *mut TupleGetter)).doc = a[1].to_ref().into_raw();
            Ok(PyRef::from_raw(op))
        })())
    }
}

unsafe extern "C" fn tuplegetter_get(o: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let g = &*(o as *mut TupleGetter);
        if obj.is_null() || (*obj).is(none::none()) {
            return (*o).to_ref().into_raw();
        }
        let obj = &*obj;
        if !tuple::is_tuple(obj) {
            return into_c(Err(err!(
                TypeError,
                "descriptor for index '{}' for tuple subclasses doesn't apply to '{}' object",
                g.index,
                fc::tname(obj)
            )));
        }
        if g.index < 0 || g.index as usize >= tuple::tuple_len(obj) {
            return into_c(Err(err!(IndexError, "tuple index out of range")));
        }
        tuple::tuple_get(obj, g.index as usize).to_ref().into_raw()
    }
}

unsafe extern "C" fn tuplegetter_set(_o: *mut PyObject, _obj: *mut PyObject, v: *mut PyObject) -> c_int {
    if v.is_null() {
        let _ = err!(AttributeError, "can't delete attribute");
    } else {
        let _ = err!(AttributeError, "can't set attribute");
    }
    -1
}

fn tuplegetter_reduce(o: &PyObject) -> PyResult {
    let g = unsafe { &*(o.as_ptr() as *mut TupleGetter) };
    Ok(tuple::tuple2(
        type_ref(o.type_ptr()),
        tuple::tuple2(int::new_int(g.index as i64), unsafe { (*g.doc).to_ref() }),
    ))
}

unsafe extern "C" fn tuplegetter_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let g = &*(o as *mut TupleGetter);
            let doc = if g.doc.is_null() { "None".to_string() } else { ab::repr_string(&*g.doc)? };
            Ok(pystr::new_str(&format!("{}({}, {})", fc::repr_type_name(&*o), g.index, doc)))
        })())
    }
}

fn tuplegetter_get_doc(o: &PyObject) -> PyResult {
    let g = unsafe { &*(o.as_ptr() as *mut TupleGetter) };
    Ok(if g.doc.is_null() { fc::none() } else { unsafe { (*g.doc).to_ref() } })
}
fn tuplegetter_set_doc(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    unsafe { fc::set_field(&raw mut (*(o.as_ptr() as *mut TupleGetter)).doc, v.map(|x| x.to_ref())) };
    Ok(())
}

unsafe extern "C" fn tuplegetter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut TupleGetter)).doc);
        fc::free(o);
    }
}
unsafe extern "C" fn tuplegetter_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe { fc::visit((*(o as *mut TupleGetter)).doc, v, arg) }
}

// ================================================================== _count_elements

fn count_elements(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_count_elements", a, 2, 2)?;
    let (mapping, iterable) = (a[0], a[1]);
    let it = ab::get_iter(iterable)?;
    let one = int::new_int(1);
    let fast = dict::is_dict_exact(mapping);
    let get = if fast { None } else { Some(ab::getattr(mapping, name!("get"))?) };
    let zero = int::new_int(0);
    while let Some(k) = ab::iter_next(&it)? {
        if fast {
            let h = ab::hash(&k)?;
            let new = match dict::get_item_hash(mapping, &k, h)? {
                Some(old) => ab::binary_op(&old, &one, ab::NbSlot::Add)?,
                None => one.clone(),
            };
            dict::set_item_hash(mapping, &k, h, &new)?;
        } else {
            let old = ab::call(get.as_ref().unwrap(), &[&k, &zero])?;
            let new = ab::binary_op(&old, &one, ab::NbSlot::Add)?;
            ab::setitem(mapping, &k, Some(&new))?;
        }
    }
    Ok(fc::none())
}

// ================================================================== module

fn leak<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

fn init_types() -> PyResult<()> {
    unsafe {
        let t = deque_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Deque>(t, c"collections.deque", Py_TPFLAGS_BASETYPE | Py_TPFLAGS_SEQUENCE);
        (*t).tp_new = Some(deque_new);
        (*t).tp_init = Some(deque_init);
        (*t).tp_iter = Some(deque_iter);
        (*t).tp_repr = Some(deque_repr);
        (*t).tp_richcompare = Some(deque_richcompare);
        (*t).tp_hash = Some(crate::types::slots::hash_not_implemented);
        (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_length = Some(deque_len);
        sq.sq_concat = Some(deque_concat);
        sq.sq_repeat = Some(deque_repeat);
        sq.sq_item = Some(deque_item);
        sq.sq_ass_item = Some(deque_ass_item);
        sq.sq_contains = Some(deque_contains);
        sq.sq_inplace_concat = Some(deque_inplace_concat);
        sq.sq_inplace_repeat = Some(deque_inplace_repeat);
        (*t).tp_as_sequence = leak(sq);
        (*t).tp_doc = c"deque([iterable[, maxlen]])\n--\n\nA list-like sequence optimized for data accesses near its endpoints.".as_ptr();
        (*t).tp_methods = mdefs![
            ("append", O, deque_append, "Add an element to the right side of the deque."),
            ("appendleft", O, deque_appendleft, "Add an element to the left side of the deque."),
            ("clear", NOARGS, deque_clear, "Remove all elements from the deque."),
            ("__copy__", NOARGS, deque_copy, "Return a shallow copy of a deque."),
            ("copy", NOARGS, deque_copy, "Return a shallow copy of a deque."),
            ("count", O, deque_count, "Return number of occurrences of value."),
            ("extend", O, deque_extend, "Extend the right side of the deque with elements from the iterable."),
            ("extendleft", O, deque_extendleft, "Extend the left side of the deque with elements from the iterable."),
            ("index", FAST, deque_index, "index($self, value, [start, [stop]])\n--\n\nReturn first index of value.\n\nRaises ValueError if the value is not present."),
            ("insert", FAST, deque_insert, "insert($self, index, value, /)\n--\n\nInsert value before index."),
            ("pop", NOARGS, deque_pop, "Remove and return the rightmost element."),
            ("popleft", NOARGS, deque_popleft, "Remove and return the leftmost element."),
            ("__reduce__", NOARGS, deque_reduce, "Return state information for pickling."),
            ("remove", O, deque_remove, "Remove first occurrence of value."),
            ("__reversed__", NOARGS, deque_reversed, "Return a reverse iterator over the deque."),
            ("reverse", NOARGS, deque_reverse, "Reverse *IN PLACE*."),
            ("rotate", FAST, deque_rotate, "rotate($self, n=1, /)\n--\n\nRotate the deque n steps to the right.  If n is negative, rotates left."),
            ("__sizeof__", NOARGS, deque_sizeof, "Size of the deque in memory, in bytes."),
            ("__class_getitem__", O, fc::class_getitem, "See PEP 585"; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![("maxlen", deque_get_maxlen; "maximum size of a deque or None if unbounded")];

        for (t, name, newf, nextf) in [
            (dequeiter_type(), c"_collections._deque_iterator", dequeiter_new as unsafe extern "C" fn(_, _, _) -> _, dequeiter_next as unsafe extern "C" fn(_) -> _),
            (dequereviter_type(), c"_collections._deque_reverse_iterator", dequereviter_new, dequereviter_next),
        ] {
            fc::init_robj_type::<DequeIter>(t, name, 0);
            (*t).tp_new = Some(newf);
            (*t).tp_iter = Some(self_iter);
            (*t).tp_iternext = Some(nextf);
            (*t).tp_methods = mdefs![
                ("__length_hint__", NOARGS, dequeiter_length_hint, "Private method returning an estimate of len(list(it))."),
                ("__reduce__", NOARGS, dequeiter_reduce, "Return state information for pickling."),
            ];
        }

        let t = defaultdict_type();
        fc::init_type(t, c"collections.defaultdict", std::mem::size_of::<DefDict>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_DICT_SUBCLASS | Py_TPFLAGS_MAPPING);
        (*t).tp_base = crate::types::dict::dict_type();
        (*t).tp_alloc = None;
        (*t).tp_free = None;
        (*t).tp_dealloc = Some(defdict_dealloc);
        (*t).tp_traverse = Some(defdict_traverse);
        (*t).tp_clear = Some(defdict_clear);
        (*t).tp_init = Some(defdict_init);
        (*t).tp_repr = Some(defdict_repr);
        let mut nb: PyNumberMethods = std::mem::zeroed();
        nb.nb_or = Some(defdict_or);
        (*t).tp_as_number = leak(nb);
        (*t).tp_doc = c"defaultdict(default_factory=None, /, [...]) --> dict with default factory\n\nThe default factory is called without arguments to produce\na new value when a key is not present, in __getitem__ only.\nA defaultdict compares equal to a dict with the same items.\nAll remaining arguments are treated the same as if they were\npassed to the dict constructor, including keyword arguments.\n".as_ptr();
        (*t).tp_methods = mdefs![
            ("__missing__", O, defdict_missing, "__missing__(key) # Called by __getitem__ for missing key; pseudo-code:\n  if self.default_factory is None: raise KeyError((key,))\n  self[key] = value = self.default_factory()\n  return value\n"),
            ("copy", NOARGS, defdict_copy, "D.copy() -> a shallow copy of D."),
            ("__copy__", NOARGS, defdict_copy, "D.copy() -> a shallow copy of D."),
            ("__reduce__", NOARGS, defdict_reduce, "Return state information for pickling."),
            ("__class_getitem__", O, fc::class_getitem, "See PEP 585"; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![("default_factory", get_factory, set_factory; "Factory for default value called by __missing__().")];

        let t = tuplegetter_type();
        fc::init_type(t, c"_collections._tuplegetter", std::mem::size_of::<TupleGetter>(), Py_TPFLAGS_HAVE_GC);
        (*t).tp_new = Some(tuplegetter_new);
        (*t).tp_descr_get = Some(tuplegetter_get);
        (*t).tp_descr_set = Some(tuplegetter_set);
        (*t).tp_repr = Some(tuplegetter_repr);
        (*t).tp_dealloc = Some(tuplegetter_dealloc);
        (*t).tp_traverse = Some(tuplegetter_traverse);
        (*t).tp_methods = mdefs![("__reduce__", NOARGS, tuplegetter_reduce)];
        (*t).tp_getset = crate::getset_table![("__doc__", tuplegetter_get_doc, tuplegetter_set_doc)];

        for t in [deque_type(), dequeiter_type(), dequereviter_type(), defaultdict_type(), tuplegetter_type()] {
            crate::types::typeobject::type_ready(t)?;
        }
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    let methods = mdefs![(
        "_count_elements",
        FAST,
        count_elements,
        "_count_elements($module, mapping, iterable, /)\n--\n\nCount elements in the iterable, updating the mapping"
    )];
    let (m, d) = fc::new_module(
        "_collections",
        methods,
        Some("High performance data structures.\n- deque:        ordered collection accessible from endpoints only\n- defaultdict:  dict subclass with a default value factory\n"),
    )?;
    fc::add_type(&d, "deque", deque_type())?;
    fc::add_type(&d, "defaultdict", defaultdict_type())?;
    fc::add_type(&d, "_deque_iterator", dequeiter_type())?;
    fc::add_type(&d, "_deque_reverse_iterator", dequereviter_type())?;
    fc::add_type(&d, "_tuplegetter", tuplegetter_type())?;
    Ok(m)
}
