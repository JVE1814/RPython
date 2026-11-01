//! Tier-0 specialisation: inline-cached attribute / global access and fast paths for
//! arithmetic and comparisons on exact builtin types. The same helpers are called by
//! the baseline JIT, so behaviour is identical in every tier.
//!
//! Guards (see cache.rs): globals are keyed on unique dict version stamps, attribute
//! caches on type version tags (reset by `slots::type_modified` on any change of a type
//! or its bases). On a guard failure we take the generic path and re-fill the entry
//! (up to a miss budget, after which the site is treated as megamorphic).

use super::cache::{self, Entry, kind};
use super::frame::Frame;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, float, funcobject, int, module, str as pystr};
use rpy_compiler::bytecode::BinOp;

// ------------------------------------------------------------------ globals

/// LoadGlobal through the instruction's cache entry.
#[inline]
pub fn load_global(f: &Frame, nm: &PyObject, e: &Entry) -> PyResult<PyRef> {
    let g = f.globals();
    let b = f.builtins();
    if dict::is_dict_exact(g) && dict::is_dict_exact(b) {
        let gv = dict::version(g);
        let bv = dict::version(b);
        if let Some(s) = e.read() {
            if s.kind == kind::GLOBAL && s.a == gv && s.b == bv {
                return Ok(unsafe { PyRef::from_borrowed(s.c as *mut PyObject) });
            }
        }
        return load_global_fill(f, nm, e, gv, bv);
    }
    super::eval::load_global(f, nm)
}

#[cold]
fn load_global_fill(f: &Frame, nm: &PyObject, e: &Entry, gv: u64, bv: u64) -> PyResult<PyRef> {
    let r = super::eval::load_global(f, nm)?;
    // only cache if neither dict changed during the lookup (str-keyed lookups never
    // run Python code, but globals may contain non-str keys)
    if dict::version(f.globals()) == gv && dict::version(f.builtins()) == bv && e.note_miss() {
        e.write(kind::GLOBAL, gv, bv, r.as_ptr() as usize);
    }
    Ok(r)
}

// ------------------------------------------------------------------ attributes

#[inline(always)]
fn generic_getattro(tp: &PyTypeObject) -> bool {
    tp.tp_getattro.map(|f| f as usize) == Some(object_generic_getattro as usize)
}

#[inline(always)]
fn type_has_dict(tp: *mut PyTypeObject) -> bool {
    unsafe { (*tp).tp_dictoffset != 0 || (*tp).tp_flags & Py_TPFLAGS_MANAGED_DICT != 0 }
}

/// Instance dict of `o` (borrowed), if it exists.
#[inline(always)]
fn instance_dict_ptr(o: &PyObject) -> *mut PyObject {
    unsafe {
        let slot = dict_slot(o.as_ptr());
        if slot.is_null() { std::ptr::null_mut() } else { *slot }
    }
}

/// Look `nm` up in dict `d` using (and refreshing) the entry-index hint.
#[inline(always)]
fn dict_get_hinted(d: *mut PyObject, nm: &PyObject, hint: u64, e: &Entry) -> Option<*mut PyObject> {
    unsafe {
        let d = &*d;
        if !dict::is_dict_exact(d) {
            return dict::get_item(d, nm).ok().flatten().map(|v| {
                let p = v.as_ptr();
                drop(v);
                p
            });
        }
        let t = dict::table(d);
        if let Some(en) = t.entries().get(hint as usize) {
            if en.key == nm.as_ptr() {
                return Some(en.value);
            }
        }
        if dict::dict_len(d) == 0 {
            return None;
        }
        match dict::lookup_index(d, nm, pystr::str_hash(nm)) {
            Ok(Some(ix)) => {
                let v = dict::table(d).entries()[ix].value;
                e.set_hint(ix as u64);
                Some(v)
            }
            Ok(None) => None,
            Err(_) => {
                clear_exc();
                None
            }
        }
    }
}

/// Byte offset of an object-valued member descriptor usable for direct access, or None.
unsafe fn member_offset(descr: &PyObject, tp: *mut PyTypeObject, for_store: bool) -> Option<(usize, i32)> {
    unsafe {
        if !descr.is_type(crate::types::descr::member_descr_type()) {
            return None;
        }
        let m = (*(descr.as_ptr() as *mut PyMemberDescrObject)).d_member;
        if m.is_null() {
            return None;
        }
        let tc = (*m).type_code;
        if tc != Py_T_OBJECT_EX && tc != Py_T_OBJECT {
            return None;
        }
        if (*m).flags & Py_AUDIT_READ != 0 || (for_store && (*m).flags & Py_READONLY != 0) {
            return None;
        }
        // the descriptor must belong to a base of tp (it does if lookup found it, but be strict)
        let owner = (*(descr.as_ptr() as *mut PyDescrObject)).d_type;
        if owner.is_null() || !is_subtype(tp, owner) {
            return None;
        }
        let mut off = (*m).offset;
        if (*m).flags & Py_RELATIVE_OFFSET != 0 {
            off += (*(*owner).tp_base).tp_basicsize;
        }
        if off <= 0 {
            return None;
        }
        Some((off as usize, tc))
    }
}

#[inline(always)]
fn bind(descr: *mut PyObject, o: &PyObject) -> PyResult<PyRef> {
    unsafe {
        let d = &*descr;
        match d.ty().tp_descr_get {
            Some(g) => {
                if funcobject::is_function(d) {
                    return Ok(funcobject::new_method(d, o));
                }
                from_c(g(descr, o.as_ptr(), o.type_ptr() as *mut PyObject))
            }
            None => Ok(d.to_ref()),
        }
    }
}

/// Fast path of a filled LoadAttr entry. `None`: guard failed (take the slow path).
#[inline(always)]
fn load_attr_hit(o: &PyObject, nm: &PyObject, e: &Entry) -> Option<PyResult<PyRef>> {
    let s = e.read()?;
    let tp = o.type_ptr();
    match s.kind {
        kind::INSTANCE_DICT => {
            if cache::type_tag(tp) as u64 != s.a {
                return None;
            }
            let d = instance_dict_ptr(o);
            if !d.is_null() {
                if let Some(v) = dict_get_hinted(d, nm, s.b, e) {
                    return Some(Ok(unsafe { PyRef::from_borrowed(v) }));
                }
            }
            if s.c != 0 {
                return Some(bind(s.c as *mut PyObject, o));
            }
            None
        }
        kind::SLOT => {
            if cache::type_tag(tp) as u64 != s.a {
                return None;
            }
            let p = unsafe { *((o.as_ptr() as *mut u8).add(s.c) as *const *mut PyObject) };
            if p.is_null() {
                if s.b as i32 == Py_T_OBJECT {
                    return Some(Ok(crate::types::none::none_ref()));
                }
                return None; // AttributeError via the generic path
            }
            Some(Ok(unsafe { PyRef::from_borrowed(p) }))
        }
        kind::DATA_DESCR => {
            if cache::type_tag(tp) as u64 != s.a {
                return None;
            }
            let d = s.c as *mut PyObject;
            let g = unsafe { (*d).ty().tp_descr_get? };
            Some(unsafe { from_c(g(d, o.as_ptr(), tp as *mut PyObject)) })
        }
        kind::CLASS_ATTR | kind::METHOD => {
            if cache::type_tag(tp) as u64 != s.a {
                return None;
            }
            Some(bind(s.c as *mut PyObject, o))
        }
        kind::CLASS_ATTR_SHADOWABLE | kind::METHOD_SHADOWABLE => {
            if cache::type_tag(tp) as u64 != s.a {
                return None;
            }
            let d = instance_dict_ptr(o);
            if !d.is_null() && dict::dict_len(unsafe { &*d }) != 0 {
                if let Some(v) = dict_get_hinted(d, nm, u64::MAX, e) {
                    return Some(Ok(unsafe { PyRef::from_borrowed(v) }));
                }
            }
            Some(bind(s.c as *mut PyObject, o))
        }
        kind::MODULE => {
            if !o.is_type(module::module_type()) {
                return None;
            }
            let d = module::m(o).md_dict;
            if d as u64 != s.b || d.is_null() || dict::version(unsafe { &*d }) != s.a {
                return None;
            }
            Some(Ok(unsafe { PyRef::from_borrowed(s.c as *mut PyObject) }))
        }
        kind::TYPE_ATTR => {
            if o.as_ptr() as u64 != s.b || !o.is_type(type_type()) || cache::type_tag(o.as_ptr() as *mut PyTypeObject) as u64 != s.a {
                return None;
            }
            Some(Ok(unsafe { PyRef::from_borrowed(s.c as *mut PyObject) }))
        }
        _ => None,
    }
}
/// A cache entry to (re)fill: an entry `a`-keyed on the same type tag if present,
/// else an empty one, else round robin. None once the site exceeded its miss budget.
type Fill = (u32, u64, u64, usize);

fn store_fill(es: &[Entry], f: Fill) {
    let (k, a, b, c) = f;
    let tagged = !matches!(k, kind::MODULE | kind::GLOBAL);
    let mut victim: Option<&Entry> = None;
    for e in es {
        if let Some(s) = e.read() {
            if tagged && s.kind != kind::EMPTY && !matches!(s.kind, kind::MODULE | kind::GLOBAL) && s.a == a {
                victim = Some(e);
                break;
            }
            if !tagged && s.kind == k {
                victim = Some(e);
                break;
            }
        }
    }
    if victim.is_none() {
        victim = es.iter().find(|e| e.read().is_some_and(|s| s.kind == kind::EMPTY));
    }
    let v = victim.unwrap_or_else(|| &es[(es[0].misses.load(std::sync::atomic::Ordering::Relaxed) as usize) % es.len()]);
    v.write(k, a, b, c);
}

/// LoadAttr through the instruction's (polymorphic) cache entries.
#[inline]
pub fn load_attr(o: &PyObject, nm: &PyObject, es: &[Entry]) -> PyResult<PyRef> {
    for e in es {
        if let Some(r) = load_attr_hit(o, nm, e) {
            return r;
        }
    }
    load_attr_miss(o, nm, es)
}

#[cold]
fn load_attr_miss(o: &PyObject, nm: &PyObject, es: &[Entry]) -> PyResult<PyRef> {
    if es[0].note_miss() {
        if let Some(f) = fill_load_attr(o, nm, false) {
            store_fill(es, f);
        }
    }
    ab::getattr(o, nm)
}

/// Compute a LoadAttr/LoadMethod entry for receiver `o` (no Python code runs here).
fn fill_load_attr(o: &PyObject, nm: &PyObject, method: bool) -> Option<Fill> {
    let tp = o.type_ptr();
    let t = o.ty();
    if !pystr::is_str_exact(nm) {
        return None;
    }
    if generic_getattro(t) {
        let tag = cache::assign_type_tag(tp);
        if tag == 0 {
            return None;
        }
        let descr = lookup(tp, nm);
        let has_dict = type_has_dict(tp);
        if let Some(d) = &descr {
            let dt = d.ty();
            if method && dt.tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 {
                let k = if has_dict { kind::METHOD_SHADOWABLE } else { kind::METHOD };
                return Some((k, tag as u64, 0, d.as_ptr() as usize));
            }
            if dt.tp_descr_get.is_some() && dt.tp_descr_set.is_some() {
                if let Some((off, tc)) = unsafe { member_offset(d, tp, false) } {
                    return Some((kind::SLOT, tag as u64, tc as u64, off));
                }
                return Some((kind::DATA_DESCR, tag as u64, 0, d.as_ptr() as usize));
            }
        }
        if has_dict {
            // instance dict first, then the (non-data) class attribute if any
            let dp = instance_dict_ptr(o);
            let ix = if !dp.is_null() && dict::is_dict_exact(unsafe { &*dp }) {
                match dict::lookup_index(unsafe { &*dp }, nm, pystr::str_hash(nm)) {
                    Ok(Some(ix)) => Some(ix),
                    _ => None,
                }
            } else {
                None
            };
            return match (&descr, ix) {
                (Some(d), None) => Some((kind::CLASS_ATTR_SHADOWABLE, tag as u64, 0, d.as_ptr() as usize)),
                _ => Some((
                    kind::INSTANCE_DICT,
                    tag as u64,
                    ix.map_or(u64::MAX, |i| i as u64),
                    descr.as_ref().map_or(0, |d| d.as_ptr() as usize),
                )),
            };
        }
        return descr.map(|d| (kind::CLASS_ATTR, tag as u64, 0, d.as_ptr() as usize));
    }
    if o.is_type(module::module_type()) {
        if lookup(tp, nm).is_some() {
            return None;
        }
        let d = module::m(o).md_dict;
        if d.is_null() || !dict::is_dict_exact(unsafe { &*d }) {
            return None;
        }
        let dd = unsafe { &*d };
        // lazy import placeholders must go through the module getattr (reification)
        return dict::get_item_str(dd, nm)
            .filter(|v| !crate::types::lazyimport::is_lazy(v))
            .map(|v| (kind::MODULE, dict::version(dd), d as u64, v.as_ptr() as usize));
    }
    if o.is_type(type_type()) {
        // class attribute through a plain `type` metaclass
        if lookup(type_type(), nm).is_some() {
            return None;
        }
        let cls = as_type(o);
        let tag = cache::assign_type_tag(cls);
        if tag == 0 {
            return None;
        }
        if let Some(v) = lookup(cls, nm) {
            let plain = v.ty().tp_descr_get.is_none() || funcobject::is_function(&v);
            if plain {
                return Some((kind::TYPE_ATTR, tag as u64, o.as_ptr() as u64, v.as_ptr() as usize));
            }
        }
    }
    None
}

/// LoadMethod through the cache: (callable, Some(self)) for unbound method calls,
/// (attribute, None) otherwise.
#[inline]
pub fn load_method(o: &PyObject, nm: &PyObject, es: &[Entry]) -> PyResult<(PyRef, Option<PyRef>)> {
    let tp = o.type_ptr();
    for e in es {
        if let Some(s) = e.read() {
            match s.kind {
                kind::METHOD => {
                    if cache::type_tag(tp) as u64 == s.a {
                        return Ok((unsafe { PyRef::from_borrowed(s.c as *mut PyObject) }, Some(o.to_ref())));
                    }
                }
                kind::METHOD_SHADOWABLE => {
                    if cache::type_tag(tp) as u64 == s.a {
                        let d = instance_dict_ptr(o);
                        if !d.is_null() && dict::dict_len(unsafe { &*d }) != 0 {
                            if let Some(v) = dict_get_hinted(d, nm, u64::MAX, e) {
                                return Ok((unsafe { PyRef::from_borrowed(v) }, None));
                            }
                        }
                        return Ok((unsafe { PyRef::from_borrowed(s.c as *mut PyObject) }, Some(o.to_ref())));
                    }
                }
                kind::EMPTY => {}
                _ => {
                    if let Some(r) = load_attr_hit(o, nm, e) {
                        return Ok((r?, None));
                    }
                }
            }
        }
    }
    load_method_miss(o, nm, es)
}

#[cold]
fn load_method_miss(o: &PyObject, nm: &PyObject, es: &[Entry]) -> PyResult<(PyRef, Option<PyRef>)> {
    if es[0].note_miss() {
        if let Some(f) = fill_load_attr(o, nm, true) {
            store_fill(es, f);
        }
    }
    super::eval::load_method(o, nm)
}

/// StoreAttr through the cache.
#[inline]
pub fn store_attr(o: &PyObject, nm: &PyObject, v: &PyObject, es: &[Entry]) -> PyResult<()> {
    let tp = o.type_ptr();
    let tag = cache::type_tag(tp) as u64;
    for e in es {
        if let Some(s) = e.read() {
            if tag == s.a && s.kind != kind::EMPTY {
                match s.kind {
                    kind::INSTANCE_DICT => unsafe {
                        let slot = dict_slot(o.as_ptr());
                        if !slot.is_null() {
                            if (*slot).is_null() {
                                *slot = dict::new_dict().into_raw();
                            }
                            return dict::set_item(&**slot, nm, v);
                        }
                    },
                    kind::SLOT => unsafe {
                        let p = (o.as_ptr() as *mut u8).add(s.c) as *mut *mut PyObject;
                        let old = *p;
                        *p = v.to_ref().into_raw();
                        xdecref(old);
                        return Ok(());
                    },
                    kind::DATA_DESCR => unsafe {
                        let d = s.c as *mut PyObject;
                        if let Some(set) = (*d).ty().tp_descr_set {
                            let r = set(d, o.as_ptr(), v.as_ptr());
                            return if r < 0 { Err(PyErr) } else { Ok(()) };
                        }
                    },
                    _ => {}
                }
            }
        }
    }
    store_attr_miss(o, nm, v, es)
}

#[cold]
fn store_attr_miss(o: &PyObject, nm: &PyObject, v: &PyObject, es: &[Entry]) -> PyResult<()> {
    let r = ab::setattr(o, nm, v);
    if r.is_ok() && es[0].note_miss() {
        if let Some(f) = fill_store_attr(o, nm) {
            store_fill(es, f);
        }
    }
    r
}

fn fill_store_attr(o: &PyObject, nm: &PyObject) -> Option<Fill> {
    let tp = o.type_ptr();
    let t = o.ty();
    if !pystr::is_str_exact(nm) || t.tp_setattro.map(|f| f as usize) != Some(object_generic_setattro as usize) {
        return None;
    }
    let tag = cache::assign_type_tag(tp);
    if tag == 0 {
        return None;
    }
    // hint: where the attribute now lives in the instance dict
    let hint = || {
        let dp = instance_dict_ptr(o);
        if !dp.is_null() && dict::is_dict_exact(unsafe { &*dp }) {
            if let Ok(Some(ix)) = dict::lookup_index(unsafe { &*dp }, nm, pystr::str_hash(nm)) {
                return ix as u64;
            }
        }
        u64::MAX
    };
    match lookup(tp, nm) {
        Some(d) => {
            if d.ty().tp_descr_set.is_some() {
                if let Some((off, tc)) = unsafe { member_offset(&d, tp, true) } {
                    return Some((kind::SLOT, tag as u64, tc as u64, off));
                }
                return Some((kind::DATA_DESCR, tag as u64, 0, d.as_ptr() as usize));
            }
            if type_has_dict(tp) {
                return Some((kind::INSTANCE_DICT, tag as u64, hint(), 0));
            }
            None
        }
        None => {
            if type_has_dict(tp) {
                return Some((kind::INSTANCE_DICT, tag as u64, hint(), 0));
            }
            None
        }
    }
}

// ------------------------------------------------------------------ arithmetic

/// Addresses of the (immortal) small int objects -5..=1025, for JIT code.
pub fn small_int_table() -> *const usize {
    static T: std::sync::OnceLock<Box<[usize]>> = std::sync::OnceLock::new();
    T.get_or_init(|| (-5i64..=1025).map(|v| int::new_int(v).into_raw() as usize).collect()).as_ptr()
}

/// Value of an exact int with at most two 30-bit digits (|v| < 2**60).
#[inline(always)]
pub fn small_int_value(o: &PyObject) -> Option<i64> {
    unsafe {
        let tag = (*(o.as_ptr() as *const PyLongObject)).lv_tag;
        if tag < (2 << 3) {
            return Some(int::compact_value(o));
        }
        if tag >> 3 == 2 {
            let d = (o.as_ptr() as *const u8).add(std::mem::offset_of!(PyLongObject, ob_digit)) as *const u32;
            let v = (*d as i64) | ((*d.add(1) as i64) << 30);
            return Some(if tag & 3 == 2 { -v } else { v });
        }
        None
    }
}

#[inline(always)]
fn int_binop(op: BinOp, x: i64, y: i64) -> Option<PyRef> {
    use BinOp::*;
    Some(match op {
        Add | InplaceAdd => int::new_int(x + y),
        Subtract | InplaceSubtract => int::new_int(x - y),
        Multiply | InplaceMultiply => int::new_int_i128(x as i128 * y as i128),
        And | InplaceAnd => int::new_int(x & y),
        Or | InplaceOr => int::new_int(x | y),
        Xor | InplaceXor => int::new_int(x ^ y),
        FloorDivide | InplaceFloorDivide => {
            if y == 0 {
                return None;
            }
            let q = x / y;
            int::new_int(if x % y != 0 && ((x < 0) != (y < 0)) { q - 1 } else { q })
        }
        Remainder | InplaceRemainder => {
            if y == 0 {
                return None;
            }
            let r = x % y;
            int::new_int(if r != 0 && ((r < 0) != (y < 0)) { r + y } else { r })
        }
        Lshift | InplaceLshift => {
            if !(0..63).contains(&y) {
                return None;
            }
            let r = x << y;
            if r >> y != x {
                return None;
            }
            int::new_int(r)
        }
        Rshift | InplaceRshift => {
            if y < 0 {
                return None;
            }
            int::new_int(if y >= 63 { if x < 0 { -1 } else { 0 } } else { x >> y })
        }
        TrueDivide | InplaceTrueDivide => {
            const LIM: i64 = 1 << 53;
            if y == 0 || x.abs() > LIM || y.abs() > LIM {
                return None;
            }
            float::new_float(x as f64 / y as f64)
        }
        Power | InplacePower => {
            if !(0..64).contains(&y) {
                return None;
            }
            int::new_int(x.checked_pow(y as u32)?)
        }
        _ => return None,
    })
}

#[inline(always)]
fn float_binop(op: BinOp, x: f64, y: f64) -> Option<PyRef> {
    use BinOp::*;
    let r = match op {
        Add | InplaceAdd => x + y,
        Subtract | InplaceSubtract => x - y,
        Multiply | InplaceMultiply => x * y,
        TrueDivide | InplaceTrueDivide if y != 0.0 => x / y,
        _ => return None,
    };
    Some(float::new_float(r))
}

/// Fast paths for binary operations on exact int/float/str operands.
#[inline(always)]
pub fn binary_fast(op: BinOp, a: &PyObject, b: &PyObject) -> Option<PyRef> {
    let ta = a.type_ptr();
    let tb = b.type_ptr();
    if ta == int::int_type() {
        if tb == int::int_type() {
            let x = small_int_value(a)?;
            let y = small_int_value(b)?;
            return int_binop(op, x, y);
        }
        if tb == float::float_type() && int::is_compact(a) {
            return float_binop(op, int::compact_value(a) as f64, float::float_value(b));
        }
        return None;
    }
    if ta == float::float_type() {
        if tb == float::float_type() {
            return float_binop(op, float::float_value(a), float::float_value(b));
        }
        if tb == int::int_type() && int::is_compact(b) {
            return float_binop(op, float::float_value(a), int::compact_value(b) as f64);
        }
        return None;
    }
    if ta == pystr::str_type() && tb == pystr::str_type() && matches!(op, BinOp::Add | BinOp::InplaceAdd) {
        return Some(pystr::concat(a, b));
    }
    None
}

/// Fast comparisons; `cop` is the Py_LT.. code. None: use rich comparison.
#[inline(always)]
pub fn compare_fast(cop: i32, a: &PyObject, b: &PyObject) -> Option<bool> {
    #[inline(always)]
    fn cmp<T: PartialOrd>(cop: i32, x: T, y: T) -> bool {
        match cop {
            Py_LT => x < y,
            Py_LE => x <= y,
            Py_EQ => x == y,
            Py_NE => x != y,
            Py_GT => x > y,
            _ => x >= y,
        }
    }
    let ta = a.type_ptr();
    let tb = b.type_ptr();
    if ta == int::int_type() {
        if tb == int::int_type() {
            return Some(cmp(cop, small_int_value(a)?, small_int_value(b)?));
        }
        if tb == float::float_type() && int::is_compact(a) {
            return Some(cmp(cop, int::compact_value(a) as f64, float::float_value(b)));
        }
        return None;
    }
    if ta == float::float_type() {
        if tb == float::float_type() {
            return Some(cmp(cop, float::float_value(a), float::float_value(b)));
        }
        if tb == int::int_type() && int::is_compact(b) {
            return Some(cmp(cop, float::float_value(a), int::compact_value(b) as f64));
        }
        return None;
    }
    if ta == pystr::str_type() && tb == pystr::str_type() && (cop == Py_EQ || cop == Py_NE) {
        return Some(pystr::str_eq(a, b) == (cop == Py_EQ));
    }
    None
}

/// Profile bits describing an operand.
#[inline(always)]
pub fn profile_bits(o: &PyObject) -> u8 {
    use cache::prof;
    let t = o.type_ptr();
    if t == int::int_type() {
        if small_int_value(o).is_some() { prof::INT } else { prof::BIGINT }
    } else if t == float::float_type() {
        prof::FLOAT
    } else if t == pystr::str_type() {
        prof::STR
    } else if t == crate::types::list::list_type() {
        prof::LIST
    } else if t == crate::types::tuple::tuple_type() {
        prof::TUPLE
    } else if t == dict::dict_type() {
        prof::DICT
    } else {
        prof::OTHER
    }
}
