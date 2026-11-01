//! The `_functools` accelerator: partial (with Placeholder), reduce, cmp_to_key,
//! _lru_cache_wrapper.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, opt, targs};
use crate::object::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering::*};

// ================================================================== Placeholder

#[unsafe(no_mangle)]
pub static mut _PyFunctools_PlaceholderType: PyTypeObject = PyTypeObject::ZERO;
static PLACEHOLDER: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn placeholder_type() -> *mut PyTypeObject {
    &raw mut _PyFunctools_PlaceholderType
}

pub fn placeholder() -> &'static PyObject {
    let p = PLACEHOLDER.load(Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    unsafe {
        let op = crate::alloc::alloc_object(placeholder_type(), std::mem::size_of::<PyObject>());
        make_immortal(op);
        match PLACEHOLDER.compare_exchange(std::ptr::null_mut(), op, AcqRel, Acquire) {
            Ok(_) => &*op,
            Err(existing) => &*existing,
        }
    }
}

#[inline]
fn is_ph(o: &PyObject) -> bool {
    std::ptr::eq(o, placeholder())
}

unsafe extern "C" fn placeholder_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !targs(args).is_empty() || fc::kw_len(kw) > 0 {
            return into_c(Err(err!(TypeError, "_PlaceholderType takes no arguments")));
        }
        placeholder().to_ref().into_raw()
    }
}
unsafe extern "C" fn placeholder_repr(_o: *mut PyObject) -> *mut PyObject {
    pystr::new_str("Placeholder").into_raw()
}
fn placeholder_reduce(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("Placeholder"))
}

// ================================================================== partial

#[repr(C)]
pub struct Partial {
    ob_base: PyObject,
    func: *mut PyObject,
    args: *mut PyObject, // exact tuple
    kw: *mut PyObject,   // exact dict
    dict: *mut PyObject,
    weakreflist: *mut PyObject,
    vectorcall: vectorcallfunc,
    phcount: isize,
}

#[unsafe(no_mangle)]
pub static mut _PyFunctools_Partial_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn partial_type() -> *mut PyTypeObject {
    &raw mut _PyFunctools_Partial_Type
}

#[inline]
fn pobj(o: &PyObject) -> &mut Partial {
    unsafe { &mut *(o.as_ptr() as *mut Partial) }
}

fn count_placeholders(args: &[&PyObject]) -> isize {
    args.iter().filter(|a| is_ph(a)).count() as isize
}

unsafe extern "C" fn partial_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(partial_new_impl(tp, targs(args), opt(kw))) }
}

fn partial_new_impl(tp: *mut PyTypeObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    if a.is_empty() {
        raise!(TypeError, "type 'partial' takes at least one argument");
    }
    let mut func = a[0].to_ref();
    let new_args = &a[1..];
    if !ab::is_callable(&func) {
        raise!(TypeError, "the first argument must be callable");
    }
    if let Some(last) = new_args.last() {
        if is_ph(last) {
            raise!(TypeError, "trailing Placeholders are not allowed");
        }
    }
    if let Some(k) = kw {
        for (_, v) in dict::items_vec(k) {
            if is_ph(&v) {
                raise!(TypeError, "Placeholder cannot be passed as a keyword argument");
            }
        }
    }
    let mut pargs: Vec<PyRef> = Vec::new();
    let mut pkw: Option<PyRef> = None;
    // Flatten nested partial objects of the exact base type.
    let base = partial_type();
    if func.is_type(base) && tp == base && pobj(&func).dict.is_null() {
        let inner = pobj(&func);
        let inner_args = tuple::tuple_items(unsafe { &*inner.args });
        let mut j = 0;
        for x in inner_args {
            if is_ph(x) && j < new_args.len() {
                pargs.push(new_args[j].to_ref());
                j += 1;
            } else {
                pargs.push(x.to_ref());
            }
        }
        for x in &new_args[j..] {
            pargs.push(x.to_ref());
        }
        let d = dict::copy(unsafe { &*inner.kw })?;
        if let Some(k) = kw {
            dict::merge_dict(&d, k, true)?;
        }
        pkw = Some(d);
        let f = unsafe { (*inner.func).to_ref() };
        func = f;
    } else {
        pargs.extend(new_args.iter().map(|x| x.to_ref()));
    }
    let kwd = match pkw {
        Some(d) => d,
        None => match kw {
            Some(k) => dict::copy(k)?,
            None => dict::new_dict(),
        },
    };
    let ph = pargs.iter().filter(|x| is_ph(x)).count() as isize;
    unsafe {
        let op = fc::alloc(tp)?;
        let p = &mut *(op as *mut Partial);
        p.func = func.into_raw();
        p.args = tuple::new_tuple(pargs).into_raw();
        p.kw = kwd.into_raw();
        p.phcount = ph;
        p.vectorcall = Some(partial_vectorcall);
        Ok(PyRef::from_raw(op))
    }
}

/// Build the effective positional argument vector for a call.
fn partial_build_args(fargs: &PyObject, phcount: isize, call: &[&PyObject]) -> PyResult<Vec<PyRef>> {
    let items = tuple::tuple_items(fargs);
    if (call.len() as isize) < phcount {
        raise!(
            TypeError,
            "missing positional arguments in 'partial' call; expected at least {}, got {}",
            phcount,
            call.len()
        );
    }
    let mut out = Vec::with_capacity(items.len() + call.len());
    let mut j = 0;
    for x in items {
        if phcount > 0 && is_ph(x) {
            out.push(call[j].to_ref());
            j += 1;
        } else {
            out.push(x.to_ref());
        }
    }
    for x in &call[j..] {
        out.push(x.to_ref());
    }
    Ok(out)
}

fn partial_call_impl(o: &PyObject, call_pos: &[&PyObject], call_kw: Option<&PyObject>) -> PyResult {
    // Hold strong references: the call may re-enter and change our state.
    let p = pobj(o);
    let func = unsafe { (*p.func).to_ref() };
    let fargs = unsafe { (*p.args).to_ref() };
    let fkw = unsafe { (*p.kw).to_ref() };
    let phcount = p.phcount;
    let args = partial_build_args(&fargs, phcount, call_pos)?;
    let argrefs: Vec<&PyObject> = args.iter().map(|x| &**x).collect();
    let kwd: Option<PyRef> = if dict::dict_len(&fkw) == 0 {
        call_kw.map(|k| k.to_ref())
    } else {
        let d = dict::copy(&fkw)?;
        if let Some(k) = call_kw {
            dict::merge_dict(&d, k, true)?;
        }
        Some(d)
    };
    ab::call_with_kwargs_dict(&func, &argrefs, kwd.as_deref())
}

unsafe extern "C" fn partial_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(partial_call_impl(&*o, targs(args), opt(kw))) }
}

unsafe extern "C" fn partial_vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let o = &*callable;
        let n = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let all: &[&PyObject] = if n + nkw == 0 { &[] } else { std::slice::from_raw_parts(args as *const &PyObject, n + nkw) };
        let p = pobj(o);
        if dict::dict_len(&*p.kw) == 0 {
            // fast path: no stored keywords -> forward kwnames directly
            let func = (*p.func).to_ref();
            let fargs = (*p.args).to_ref();
            let r = (|| -> PyResult {
                let mut v = partial_build_args(&fargs, p.phcount, &all[..n])?;
                v.extend(all[n..].iter().map(|x| x.to_ref()));
                let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
                ab::call_kw(&func, &refs, opt(kwnames))
            })();
            return into_c(r);
        }
        let kwd = if nkw == 0 {
            None
        } else {
            let d = dict::new_dict();
            for i in 0..nkw {
                if let Err(e) = dict::set_item(&d, tuple::tuple_get(&*kwnames, i), all[n + i]) {
                    return into_c(Err(e));
                }
            }
            Some(d)
        };
        into_c(partial_call_impl(o, &all[..n], kwd.as_deref()))
    }
}

unsafe extern "C" fn partial_descr_get(o: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() || (*obj).is(none::none()) {
            return (*o).to_ref().into_raw();
        }
        crate::types::funcobject::new_method(&*o, &*obj).into_raw()
    }
}

fn type_display_name(o: &PyObject) -> PyResult<String> {
    let t = crate::types::typeobject::type_obj(o.type_ptr());
    let m = ab::getattr(t, name!("__module__"))?;
    let q = ab::getattr(t, name!("__qualname__"))?;
    Ok(format!("{}.{}", ab::str_string(&m)?, ab::str_string(&q)?))
}

unsafe extern "C" fn partial_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let name = type_display_name(o)?;
            let _g = match fc::ReprGuard::enter(o) {
                Some(g) => g,
                None => return Ok(pystr::new_str("...")),
            };
            let p = pobj(o);
            let func = (*p.func).to_ref();
            let fargs = (*p.args).to_ref();
            let fkw = (*p.kw).to_ref();
            let mut parts = vec![ab::repr_string(&func)?];
            for a in tuple::tuple_items(&fargs) {
                parts.push(ab::repr_string(a)?);
            }
            for (k, v) in dict::items_vec(&fkw) {
                let ks = ab::str_string(&k)?;
                parts.push(format!("{}={}", ks, ab::repr_string(&v)?));
            }
            Ok(pystr::new_str(&format!("{}({})", name, parts.join(", "))))
        })())
    }
}

fn partial_reduce(o: &PyObject) -> PyResult {
    let p = pobj(o);
    unsafe {
        let kw: PyRef = if dict::dict_len(&*p.kw) == 0 { fc::none() } else { (*p.kw).to_ref() };
        let d: PyRef = if p.dict.is_null() || dict::dict_len(&*p.dict) == 0 { fc::none() } else { (*p.dict).to_ref() };
        let state = tuple::new_tuple(vec![(*p.func).to_ref(), (*p.args).to_ref(), kw, d]);
        Ok(tuple::new_tuple(vec![
            crate::types::typeobject::type_ref(o.type_ptr()),
            tuple::new_tuple(vec![(*p.func).to_ref()]),
            state,
        ]))
    }
}

fn partial_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if !tuple::is_tuple(state) {
        raise!(TypeError, "argument to __setstate__ must be a tuple");
    }
    if tuple::tuple_len(state) != 4 {
        raise!(TypeError, "expected 4 items in state, got {}", tuple::tuple_len(state));
    }
    let it = tuple::tuple_items(state);
    let (f, fargs, kw, d) = (it[0], it[1], it[2], it[3]);
    if !ab::is_callable(f)
        || !tuple::is_tuple(fargs)
        || (!kw.is(none::none()) && !dict::is_dict(kw))
        || (!d.is(none::none()) && !dict::is_dict(d))
    {
        raise!(TypeError, "invalid partial state");
    }
    let items = tuple::tuple_items(fargs);
    if let Some(last) = items.last() {
        if is_ph(last) {
            raise!(TypeError, "trailing Placeholders are not allowed");
        }
    }
    let phcount = count_placeholders(items);
    let new_args = if tuple::is_tuple_exact(fargs) { fargs.to_ref() } else { tuple::new_tuple(tuple::tuple_to_vec(fargs)) };
    let new_kw = if kw.is(none::none()) {
        dict::new_dict()
    } else if dict::is_dict_exact(kw) {
        kw.to_ref()
    } else {
        dict::copy(kw)?
    };
    let new_dict = if d.is(none::none()) { None } else { Some(d.to_ref()) };
    let p = pobj(o);
    unsafe {
        fc::set_field(&raw mut p.func, Some(f.to_ref()));
        fc::set_field(&raw mut p.args, Some(new_args));
        fc::set_field(&raw mut p.kw, Some(new_kw));
        fc::set_field(&raw mut p.dict, new_dict);
    }
    p.phcount = phcount;
    Ok(fc::none())
}

fn partial_get_func(o: &PyObject) -> PyResult {
    Ok(unsafe { (*pobj(o).func).to_ref() })
}
fn partial_get_args(o: &PyObject) -> PyResult {
    Ok(unsafe { (*pobj(o).args).to_ref() })
}
fn partial_get_kw(o: &PyObject) -> PyResult {
    Ok(unsafe { (*pobj(o).kw).to_ref() })
}

unsafe extern "C" fn partial_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let p = &mut *(o as *mut Partial);
        crate::types::weakref::clear_weakrefs_at(o, &raw mut p.weakreflist);
        fc::clear_field(&raw mut p.func);
        fc::clear_field(&raw mut p.args);
        fc::clear_field(&raw mut p.kw);
        fc::clear_field(&raw mut p.dict);
        fc::free(o);
    }
}
unsafe extern "C" fn partial_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    let p = o as *mut Partial;
    unsafe {
        crate::visit_all!(v, arg; (*p).func, (*p).args, (*p).kw, (*p).dict);
    }
    0
}
unsafe extern "C" fn partial_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let p = &mut *(o as *mut Partial);
        fc::clear_field(&raw mut p.func);
        fc::clear_field(&raw mut p.args);
        fc::clear_field(&raw mut p.kw);
        fc::clear_field(&raw mut p.dict);
    }
    0
}

// ================================================================== reduce

fn functools_reduce(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("reduce", &["function", "iterable", "initial"], 2, 2);
    let [f, seq, init] = crate::args::parse::<3>(&p, args, kw)?;
    let (f, seq) = (f.unwrap(), seq.unwrap());
    let it = match ab::get_iter(seq) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "reduce() arg 2 must support iteration");
            }
            return Err(e);
        }
    };
    let mut acc: Option<PyRef> = init.map(|x| x.to_ref());
    while let Some(x) = ab::iter_next(&it)? {
        acc = Some(match acc {
            None => x,
            Some(a) => ab::call(f, &[&a, &x])?,
        });
    }
    match acc {
        Some(a) => Ok(a),
        None => raise!(TypeError, "reduce() of empty iterable with no initial value"),
    }
}

// ================================================================== cmp_to_key

#[repr(C)]
struct KeyObject {
    ob_base: PyObject,
    cmp: *mut PyObject,
    object: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut _PyFunctools_KeyWrapper_Type: PyTypeObject = PyTypeObject::ZERO;
fn keyobject_type() -> *mut PyTypeObject {
    &raw mut _PyFunctools_KeyWrapper_Type
}

fn new_keyobject(cmp: &PyObject, obj: Option<&PyObject>) -> PyResult {
    unsafe {
        let op = fc::alloc(keyobject_type())?;
        let k = &mut *(op as *mut KeyObject);
        k.cmp = cmp.to_ref().into_raw();
        k.object = obj.map_or(std::ptr::null_mut(), |o| o.to_ref().into_raw());
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn keyobject_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let a = targs(args);
            let mut obj: Option<&PyObject> = a.first().copied();
            if a.len() > 1 {
                raise!(TypeError, "K() takes at most 1 argument ({} given)", a.len());
            }
            if let Some(k) = opt(kw) {
                for (key, v) in dict::items_vec(k) {
                    if pystr::to_string_lossy(&key) == "obj" && obj.is_none() {
                        // safe: v kept alive by kw dict
                        obj = Some(&*(v.as_ptr()));
                    } else {
                        raise!(TypeError, "K() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&key));
                    }
                }
            }
            let obj = match obj {
                Some(o) => o,
                None => raise!(TypeError, "K() missing required argument 'obj' (pos 1)"),
            };
            new_keyobject(&*(*(o as *mut KeyObject)).cmp, Some(obj))
        })())
    }
}

unsafe extern "C" fn keyobject_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            if !(*b).is_type(keyobject_type()) {
                raise!(TypeError, "other argument must be K instance");
            }
            let ka = &*(a as *mut KeyObject);
            let kb = &*(b as *mut KeyObject);
            if ka.object.is_null() || kb.object.is_null() {
                raise!(AttributeError, "object");
            }
            let res = ab::call(&*ka.cmp, &[&*ka.object, &*kb.object])?;
            let zero = int::new_int(0);
            ab::rich_compare(&res, &zero, op)
        })())
    }
}

fn keyobject_get_obj(o: &PyObject) -> PyResult {
    let k = unsafe { &*(o.as_ptr() as *mut KeyObject) };
    if k.object.is_null() {
        raise!(AttributeError, "obj");
    }
    Ok(unsafe { (*k.object).to_ref() })
}

unsafe extern "C" fn keyobject_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let k = &mut *(o as *mut KeyObject);
        fc::clear_field(&raw mut k.cmp);
        fc::clear_field(&raw mut k.object);
        fc::free(o);
    }
}
unsafe extern "C" fn keyobject_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    let k = o as *mut KeyObject;
    unsafe {
        crate::visit_all!(v, arg; (*k).cmp, (*k).object);
    }
    0
}

fn functools_cmp_to_key(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("cmp_to_key", &["mycmp"], 0, 1);
    let [cmp] = crate::args::parse::<1>(&p, args, kw)?;
    new_keyobject(cmp.unwrap(), None)
}

// ================================================================== _lru_cache_wrapper

struct Node {
    key: Option<PyRef>,
    result: Option<PyRef>,
    hash: isize,
    prev: usize,
    next: usize,
}

/// Doubly linked list in a slab; index 0 is the root sentinel.
struct LruList {
    nodes: Vec<Node>,
    free: Vec<usize>,
}

impl LruList {
    fn new() -> LruList {
        LruList { nodes: vec![Node { key: None, result: None, hash: 0, prev: 0, next: 0 }], free: Vec::new() }
    }
    fn unlink(&mut self, i: usize) {
        let (p, n) = (self.nodes[i].prev, self.nodes[i].next);
        self.nodes[p].next = n;
        self.nodes[n].prev = p;
    }
    /// Append before root (most recently used position).
    fn append(&mut self, i: usize) {
        let last = self.nodes[0].prev;
        self.nodes[i].prev = last;
        self.nodes[i].next = 0;
        self.nodes[last].next = i;
        self.nodes[0].prev = i;
    }
    fn alloc(&mut self, key: PyRef, result: PyRef, hash: isize) -> usize {
        let n = Node { key: Some(key), result: Some(result), hash, prev: 0, next: 0 };
        match self.free.pop() {
            Some(i) => {
                self.nodes[i] = n;
                i
            }
            None => {
                self.nodes.push(n);
                self.nodes.len() - 1
            }
        }
    }
    fn release(&mut self, i: usize) -> (Option<PyRef>, Option<PyRef>) {
        let k = self.nodes[i].key.take();
        let r = self.nodes[i].result.take();
        self.free.push(i);
        (k, r)
    }
    fn clear(&mut self) -> Vec<PyRef> {
        let mut out = Vec::new();
        for n in self.nodes.iter_mut().skip(1) {
            if let Some(k) = n.key.take() {
                out.push(k);
            }
            if let Some(r) = n.result.take() {
                out.push(r);
            }
        }
        self.nodes.truncate(1);
        self.nodes[0].prev = 0;
        self.nodes[0].next = 0;
        self.free.clear();
        out
    }
}

#[repr(C)]
struct LruWrapper {
    ob_base: PyObject,
    func: *mut PyObject,
    cache: *mut PyObject, // dict key -> int index (bounded) or result (unbounded)
    cache_info_type: *mut PyObject,
    dict: *mut PyObject,
    weakreflist: *mut PyObject,
    list: *mut LruList,
    maxsize: isize, // -1 => unbounded
    typed: bool,
    hits: isize,
    misses: isize,
}

#[unsafe(no_mangle)]
pub static mut _PyFunctools_LruCacheWrapper_Type: PyTypeObject = PyTypeObject::ZERO;
fn lru_type() -> *mut PyTypeObject {
    &raw mut _PyFunctools_LruCacheWrapper_Type
}

static KWD_MARK: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
fn kwd_mark() -> &'static PyObject {
    let p = KWD_MARK.load(Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let o = ab::call(crate::types::typeobject::type_obj(crate::types::typeobject::object_type()), &[]).unwrap();
    let raw = o.into_raw();
    match KWD_MARK.compare_exchange(std::ptr::null_mut(), raw, AcqRel, Acquire) {
        Ok(_) => unsafe { &*raw },
        Err(e) => unsafe {
            decref(raw);
            &*e
        },
    }
}

#[inline]
fn lobj(o: &PyObject) -> &mut LruWrapper {
    unsafe { &mut *(o.as_ptr() as *mut LruWrapper) }
}

fn make_key(args: &[&PyObject], kw: Option<&PyObject>, typed: bool) -> PyResult {
    let kw = kw.filter(|k| dict::dict_len(k) > 0);
    if kw.is_none() && !typed {
        if args.len() == 1 && (pystr::is_str_exact(args[0]) || int::is_int_exact(args[0])) {
            return Ok(args[0].to_ref());
        }
        return Ok(tuple::new_tuple(args.iter().map(|x| x.to_ref()).collect()));
    }
    let mut v: Vec<PyRef> = args.iter().map(|x| x.to_ref()).collect();
    let items = kw.map(dict::items_vec).unwrap_or_default();
    if !items.is_empty() {
        v.push(kwd_mark().to_ref());
        for (k, val) in items.iter() {
            v.push(k.clone());
            v.push(val.clone());
        }
    }
    if typed {
        for a in args {
            v.push(crate::types::typeobject::type_ref(a.type_ptr()));
        }
        for (_, val) in items.iter() {
            v.push(crate::types::typeobject::type_ref(val.type_ptr()));
        }
    }
    Ok(tuple::new_tuple(v))
}

fn lru_call_impl(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let w = lobj(o);
    let func = unsafe { (*w.func).to_ref() };
    if w.maxsize == 0 {
        w.misses += 1;
        return ab::call_with_kwargs_dict(&func, args, kw);
    }
    let key = make_key(args, kw, w.typed)?;
    let hash = ab::hash(&key)?;
    let cache = unsafe { (*w.cache).to_ref() };
    if w.maxsize < 0 {
        // unbounded
        if let Some(r) = dict::get_item_hash(&cache, &key, hash)? {
            w.hits += 1;
            return Ok(r);
        }
        w.misses += 1;
        let r = ab::call_with_kwargs_dict(&func, args, kw)?;
        dict::set_item_hash(&cache, &key, hash, &r)?;
        return Ok(r);
    }
    // bounded
    if let Some(ix) = dict::get_item_hash(&cache, &key, hash)? {
        let i = int::as_i64(&ix).unwrap_or(0) as usize;
        let l = unsafe { &mut *lobj(o).list };
        if let Some(r) = l.nodes.get(i).and_then(|n| n.result.clone()) {
            l.unlink(i);
            l.append(i);
            lobj(o).hits += 1;
            return Ok(r);
        }
    }
    lobj(o).misses += 1;
    let result = ab::call_with_kwargs_dict(&func, args, kw)?;
    let w = lobj(o);
    // The call may have re-entered and already cached this key.
    if dict::get_item_hash(&cache, &key, hash)?.is_some() {
        return Ok(result);
    }
    let l = unsafe { &mut *w.list };
    if (dict::dict_len(&cache) as isize) < w.maxsize {
        let i = l.alloc(key.clone(), result.clone(), hash);
        dict::set_item_hash(&cache, &key, hash, &int::new_int(i as i64))?;
        l.append(i);
        return Ok(result);
    }
    // Evict the oldest entry (root.next) and reuse its slot.
    let oldest = l.nodes[0].next;
    if oldest == 0 {
        return Ok(result);
    }
    l.unlink(oldest);
    let ohash = l.nodes[oldest].hash;
    let (okey, oresult) = l.release(oldest);
    if let Some(k) = &okey {
        if !dict::del_item_hash(&cache, k, ohash)? {
            // should not happen; keep going
        }
    }
    let i = l.alloc(key.clone(), result.clone(), hash);
    if let Err(e) = dict::set_item_hash(&cache, &key, hash, &int::new_int(i as i64)) {
        l.release(i);
        return Err(e);
    }
    l.append(i);
    drop(okey);
    drop(oresult);
    Ok(result)
}

unsafe extern "C" fn lru_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let ts = crate::threads::current();
        if crate::interp::enter_recursive_call(ts, " while calling a Python object").is_err() {
            return std::ptr::null_mut();
        }
        let r = lru_call_impl(o, targs(args), opt(kw));
        crate::interp::leave_recursive_call(ts);
        into_c(r)
    }
}

unsafe extern "C" fn lru_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let (v, names) = fc::vc_from_kwdict(targs(args), opt(kw));
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let p = crate::args::Params::new(
                "_lru_cache_wrapper",
                &["user_function", "maxsize", "typed", "cache_info_type"],
                0,
                4,
            );
            let [f, maxsize, typed, cit] = crate::args::parse::<4>(&p, &refs, names.as_deref())?;
            let (f, maxsize, typed, cit) = (f.unwrap(), maxsize.unwrap(), typed.unwrap(), cit.unwrap());
            if !ab::is_callable(f) {
                raise!(TypeError, "the first argument must be callable");
            }
            let ms: isize = if maxsize.is(none::none()) {
                -1
            } else if int::is_int(maxsize) {
                let m = fc::ssize_arg(maxsize)?;
                if m < 0 { 0 } else { m }
            } else {
                raise!(TypeError, "maxsize should be integer or None");
            };
            let typed = ab::is_true(typed)?;
            let op = fc::alloc(tp)?;
            let w = &mut *(op as *mut LruWrapper);
            w.func = f.to_ref().into_raw();
            w.cache = dict::new_dict().into_raw();
            w.cache_info_type = cit.to_ref().into_raw();
            w.list = Box::into_raw(Box::new(LruList::new()));
            w.maxsize = ms;
            w.typed = typed;
            Ok(PyRef::from_raw(op))
        })())
    }
}

fn lru_cache_info(o: &PyObject) -> PyResult {
    let w = lobj(o);
    let currsize = unsafe { dict::dict_len(&*w.cache) };
    let maxsize = if w.maxsize < 0 { fc::none() } else { int::new_int(w.maxsize as i64) };
    ab::call(
        unsafe { &*w.cache_info_type },
        &[&int::new_int(w.hits as i64), &int::new_int(w.misses as i64), &maxsize, &int::new_int(currsize as i64)],
    )
}

fn lru_cache_clear(o: &PyObject) -> PyResult {
    let w = lobj(o);
    w.hits = 0;
    w.misses = 0;
    let old = unsafe { (*w.list).clear() };
    // Replace the dict first so re-entrant code sees an empty cache.
    let oldd = unsafe { (*w.cache).to_ref() };
    unsafe { fc::set_field(&raw mut w.cache, Some(dict::new_dict())) };
    dict::clear(&oldd);
    drop(old);
    Ok(fc::none())
}

fn lru_reduce(o: &PyObject) -> PyResult {
    ab::getattr(o, name!("__qualname__"))
}
fn lru_copy(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn lru_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

unsafe extern "C" fn lru_descr_get(o: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() || (*obj).is(none::none()) {
            return (*o).to_ref().into_raw();
        }
        crate::types::funcobject::new_method(&*o, &*obj).into_raw()
    }
}

#[allow(dead_code)]
fn lru_get_wrapped(o: &PyObject) -> PyResult {
    Ok(unsafe { (*lobj(o).func).to_ref() })
}

unsafe extern "C" fn lru_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let w = &mut *(o as *mut LruWrapper);
        crate::types::weakref::clear_weakrefs_at(o, &raw mut w.weakreflist);
        lru_clear_refs(w);
        fc::free(o);
    }
}
unsafe fn lru_clear_refs(w: &mut LruWrapper) {
    unsafe {
        if !w.list.is_null() {
            let l = Box::from_raw(w.list);
            w.list = std::ptr::null_mut();
            let mut l = l;
            drop(l.clear());
        }
        fc::clear_field(&raw mut w.func);
        fc::clear_field(&raw mut w.cache);
        fc::clear_field(&raw mut w.cache_info_type);
        fc::clear_field(&raw mut w.dict);
    }
}
unsafe extern "C" fn lru_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let w = &*(o as *mut LruWrapper);
        crate::visit_all!(v, arg; w.func, w.cache, w.cache_info_type, w.dict);
        if !w.list.is_null() {
            for n in (*w.list).nodes.iter().skip(1) {
                if let Some(k) = &n.key {
                    crate::visit_all!(v, arg; k.as_ptr());
                }
                if let Some(r) = &n.result {
                    crate::visit_all!(v, arg; r.as_ptr());
                }
            }
        }
    }
    0
}
unsafe extern "C" fn lru_tp_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let w = &mut *(o as *mut LruWrapper);
        if !w.list.is_null() {
            drop((*w.list).clear());
        }
        fc::clear_field(&raw mut w.func);
        fc::clear_field(&raw mut w.cache);
        fc::clear_field(&raw mut w.cache_info_type);
        fc::clear_field(&raw mut w.dict);
    }
    0
}

// ================================================================== module

fn init_types() -> PyResult<()> {
    unsafe {
        let t = placeholder_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_type(t, c"functools._PlaceholderType", std::mem::size_of::<PyObject>(), Py_TPFLAGS_IMMUTABLETYPE);
        (*t).tp_new = Some(placeholder_new);
        (*t).tp_repr = Some(placeholder_repr);
        (*t).tp_doc = c"The type of the Placeholder singleton.\n\nUsed as a placeholder for partial arguments.".as_ptr();
        (*t).tp_methods = mdefs![("__reduce__", NOARGS, placeholder_reduce)];

        let t = partial_type();
        fc::init_type(
            t,
            c"functools.partial",
            std::mem::size_of::<Partial>(),
            Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_VECTORCALL | Py_TPFLAGS_METHOD_DESCRIPTOR,
        );
        (*t).tp_new = Some(partial_new);
        (*t).tp_call = Some(partial_call);
        (*t).tp_repr = Some(partial_repr);
        (*t).tp_dealloc = Some(partial_dealloc);
        (*t).tp_traverse = Some(partial_traverse);
        (*t).tp_clear = Some(partial_clear);
        (*t).tp_descr_get = Some(partial_descr_get);
        (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        (*t).tp_dictoffset = std::mem::offset_of!(Partial, dict) as isize;
        (*t).tp_weaklistoffset = std::mem::offset_of!(Partial, weakreflist) as isize;
        (*t).tp_vectorcall_offset = std::mem::offset_of!(Partial, vectorcall) as isize;
        (*t).tp_doc = c"partial(func, /, *args, **keywords)\n--\n\nCreate a new function with partial application of the given arguments\nand keywords.".as_ptr();
        (*t).tp_methods = mdefs![
            ("__reduce__", NOARGS, partial_reduce),
            ("__setstate__", O, partial_setstate),
            ("__class_getitem__", O, fc::class_getitem, "See PEP 585"; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![
            ("func", partial_get_func; "function object to use in future partial calls"),
            ("args", partial_get_args; "tuple of arguments to future partial calls"),
            ("keywords", partial_get_kw; "dictionary of keyword arguments to future partial calls"),
            ("__dict__", fc::get_dict_attr, fc::set_dict_attr),
        ];

        let t = keyobject_type();
        fc::init_type(t, c"functools.KeyWrapper", std::mem::size_of::<KeyObject>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_call = Some(keyobject_call);
        (*t).tp_richcompare = Some(keyobject_richcompare);
        (*t).tp_dealloc = Some(keyobject_dealloc);
        (*t).tp_traverse = Some(keyobject_traverse);
        (*t).tp_hash = Some(crate::types::slots::hash_not_implemented);
        (*t).tp_getset = crate::getset_table![("obj", keyobject_get_obj; "Value wrapped by a key function.")];

        let t = lru_type();
        fc::init_type(
            t,
            c"functools._lru_cache_wrapper",
            std::mem::size_of::<LruWrapper>(),
            Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_METHOD_DESCRIPTOR,
        );
        (*t).tp_new = Some(lru_new);
        (*t).tp_call = Some(lru_call);
        (*t).tp_dealloc = Some(lru_dealloc);
        (*t).tp_traverse = Some(lru_traverse);
        (*t).tp_clear = Some(lru_tp_clear);
        (*t).tp_descr_get = Some(lru_descr_get);
        (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        (*t).tp_dictoffset = std::mem::offset_of!(LruWrapper, dict) as isize;
        (*t).tp_weaklistoffset = std::mem::offset_of!(LruWrapper, weakreflist) as isize;
        (*t).tp_doc = c"Create a cached callable that wraps another function.\n\nuser_function:      the function being cached\n\nmaxsize:  0         for no caching\n          None      for unlimited cache size\n          n         for a bounded cache\n\ntyped:    False     cache f(3) and f(3.0) as identical calls\n          True      cache f(3) and f(3.0) as distinct calls\n\ncache_info_type:    namedtuple class with the fields:\n                        hits misses currsize maxsize\n".as_ptr();
        (*t).tp_methods = mdefs![
            ("cache_info", NOARGS, lru_cache_info, "cache_info($self, /)\n--\n\nReport cache statistics"),
            ("cache_clear", NOARGS, lru_cache_clear, "cache_clear($self, /)\n--\n\nClear the cache and cache statistics"),
            ("__reduce__", NOARGS, lru_reduce),
            ("__copy__", NOARGS, lru_copy),
            ("__deepcopy__", O, lru_deepcopy),
        ];
        (*t).tp_getset = crate::getset_table![
            ("__dict__", fc::get_dict_attr, fc::set_dict_attr),
        ];
        for t in [placeholder_type(), partial_type(), keyobject_type(), lru_type()] {
            crate::types::typeobject::type_ready(t)?;
        }
        crate::types::typeobject::type_dict_set(keyobject_type(), "__text_signature__", &pystr::new_str("(obj)"));
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    let methods = mdefs![
        (
            "reduce",
            FASTKW,
            functools_reduce,
            "reduce($module, function, iterable, /, initial=<unrepresentable>)\n--\n\nApply a function of two arguments cumulatively to the items of an iterable, from left to right.\n\nThis effectively reduces the iterable to a single value.  If initial is present,\nit is placed before the items of the iterable in the calculation, and serves as\na default when the iterable is empty.\n\nFor example, reduce(lambda x, y: x+y, [1, 2, 3, 4, 5])\ncalculates ((((1 + 2) + 3) + 4) + 5)."
        ),
        ("cmp_to_key", FASTKW, functools_cmp_to_key, "cmp_to_key($module, /, mycmp)\n--\n\nConvert a cmp= function into a key= function.\n\n  mycmp\n    Function that compares two objects."),
    ];
    let (m, d) = fc::new_module("_functools", methods, Some("Tools that operate on functions."))?;
    fc::add_type(&d, "partial", partial_type())?;
    fc::add_type(&d, "_PlaceholderType", placeholder_type())?;
    fc::add_type(&d, "_lru_cache_wrapper", lru_type())?;
    dict::set_item_str(&d, "Placeholder", placeholder())?;
    Ok(m)
}
