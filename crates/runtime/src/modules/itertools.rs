//! The `itertools` module.

use crate::abstract_::{self as ab, NbSlot};
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, RObj, Trav, VcArgs, opt, rs, targs, trav_opt, trav_vec};
use crate::object::*;
use crate::types::{bool_, int, none, str as pystr, tuple};
use crate::{mdefs, raise, trav};
use std::ffi::c_int;

type VisitFn<'a> = &'a mut dyn FnMut(*mut PyObject) -> c_int;

macro_rules! itype {
    ($stat:ident, $getter:ident) => {
        #[unsafe(no_mangle)]
        pub static mut $stat: PyTypeObject = PyTypeObject::ZERO;
        pub fn $getter() -> *mut PyTypeObject {
            &raw mut $stat
        }
    };
}

itype!(_PyItertools_Accumulate_Type, accumulate_type);
itype!(_PyItertools_Batched_Type, batched_type);
itype!(_PyItertools_Chain_Type, chain_type);
itype!(_PyItertools_Combinations_Type, combinations_type);
itype!(_PyItertools_Cwr_Type, cwr_type);
itype!(_PyItertools_Compress_Type, compress_type);
itype!(_PyItertools_Count_Type, count_type);
itype!(_PyItertools_Cycle_Type, cycle_type);
itype!(_PyItertools_Dropwhile_Type, dropwhile_type);
itype!(_PyItertools_Takewhile_Type, takewhile_type);
itype!(_PyItertools_Filterfalse_Type, filterfalse_type);
itype!(_PyItertools_Groupby_Type, groupby_type);
itype!(_PyItertools_Grouper_Type, grouper_type);
itype!(_PyItertools_Islice_Type, islice_type);
itype!(_PyItertools_Pairwise_Type, pairwise_type);
itype!(_PyItertools_Permutations_Type, permutations_type);
itype!(_PyItertools_Product_Type, product_type);
itype!(_PyItertools_Repeat_Type, repeat_type);
itype!(_PyItertools_Starmap_Type, starmap_type);
itype!(_PyItertools_Tee_Type, tee_type);
itype!(_PyItertools_TeeData_Type, teedata_type);
itype!(_PyItertools_ZipLongest_Type, ziplongest_type);

/// Wrap an `fn(&PyObject) -> PyResult<Option<PyRef>>` as tp_iternext.
macro_rules! iternext {
    ($name:ident, $imp:ident) => {
        unsafe extern "C" fn $name(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                // the iterator's Rust state is not safe to step from two threads
                let _lk = crate::objlock::lock_held(&*o);
                match $imp(&*o) {
                    Ok(Some(v)) => v.into_raw(),
                    Ok(None) => std::ptr::null_mut(),
                    Err(_) => std::ptr::null_mut(),
                }
            }
        }
    };
}

macro_rules! tpnew {
    ($name:ident, $imp:ident) => {
        unsafe extern "C" fn $name(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
            unsafe { into_c($imp(tp, args, kw)) }
        }
    };
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

fn next_of(it: &Option<PyRef>) -> PyResult<Option<PyRef>> {
    match it {
        Some(i) => ab::iter_next(i),
        None => Ok(None),
    }
}

fn index_arg(o: &PyObject) -> PyResult<isize> {
    fc::ssize_arg(o)
}

// ================================================================== accumulate

struct Accumulate {
    it: Option<PyRef>,
    func: Option<PyRef>,
    total: Option<PyRef>,
    initial: Option<PyRef>,
}
impl Trav for Accumulate {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.it, self.func, self.total, self.initial)
    }
}

unsafe fn accumulate_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("accumulate", &["iterable", "func", "initial"], 0, 1).kwonly(1);
    let [iterable, func, initial] = a.parse::<3>(&p)?;
    let it = ab::get_iter(&iterable.unwrap())?;
    let func = func.filter(|f| !f.is(none::none()));
    let initial = initial.filter(|f| !f.is(none::none()));
    fc::robj_new(tp, Accumulate { it: Some(it), func, total: None, initial })
}
tpnew!(accumulate_new, accumulate_new_impl);

fn accumulate_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Accumulate>(o);
    if let Some(init) = s.initial.take() {
        s.total = Some(init.clone());
        return Ok(Some(init));
    }
    let it = match &s.it {
        Some(i) => i.clone(),
        None => return Ok(None),
    };
    let val = match ab::iter_next(&it)? {
        Some(v) => v,
        None => return Ok(None),
    };
    let s = rs::<Accumulate>(o);
    let newtotal = match (&s.total, &s.func) {
        (None, _) => val,
        (Some(t), None) => {
            let t = t.clone();
            ab::binary_op(&t, &val, NbSlot::Add)?
        }
        (Some(t), Some(f)) => {
            let (t, f) = (t.clone(), f.clone());
            ab::call(&f, &[&t, &val])?
        }
    };
    rs::<Accumulate>(o).total = Some(newtotal.clone());
    Ok(Some(newtotal))
}
iternext!(accumulate_next, accumulate_next_impl);

// ================================================================== batched

struct Batched {
    it: Option<PyRef>,
    n: usize,
    strict: bool,
}
impl Trav for Batched {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.it)
    }
}

unsafe fn batched_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("batched", &["iterable", "n", "strict"], 0, 2).kwonly(1);
    let [iterable, n, strict] = a.parse::<3>(&p)?;
    let n = index_arg(&n.unwrap())?;
    if n < 1 {
        raise!(ValueError, "n must be at least one");
    }
    let strict = match strict {
        Some(s) => ab::is_true(&s)?,
        None => false,
    };
    let it = ab::get_iter(&iterable.unwrap())?;
    fc::robj_new(tp, Batched { it: Some(it), n: n as usize, strict })
}
tpnew!(batched_new, batched_new_impl);

fn batched_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Batched>(o);
    let it = match &s.it {
        Some(i) => i.clone(),
        None => return Ok(None),
    };
    let n = s.n;
    let mut items = Vec::with_capacity(n.min(1024));
    while items.len() < n {
        match ab::iter_next(&it) {
            Ok(Some(v)) => items.push(v),
            Ok(None) => break,
            Err(e) => {
                rs::<Batched>(o).it = None;
                return Err(e);
            }
        }
    }
    if items.len() < n {
        let s = rs::<Batched>(o);
        s.it = None;
        if items.is_empty() {
            return Ok(None);
        }
        if s.strict {
            raise!(ValueError, "batched(): incomplete batch");
        }
    }
    Ok(Some(tuple::new_tuple(items)))
}
iternext!(batched_next, batched_next_impl);

// ================================================================== chain

struct Chain {
    source: Option<PyRef>,
    active: Option<PyRef>,
}
impl Trav for Chain {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.source, self.active)
    }
}

unsafe fn chain_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw_unless_init_overridden(tp, chain_type(), "chain", kw)? };
    let source = ab::get_iter(unsafe { &*args })?;
    fc::robj_new(tp, Chain { source: Some(source), active: None })
}
tpnew!(chain_new, chain_new_impl);

fn chain_from_iterable(cls: &PyObject, arg: &PyObject) -> PyResult {
    let source = ab::get_iter(arg)?;
    fc::robj_new(crate::types::typeobject::as_type(cls), Chain { source: Some(source), active: None })
}

fn chain_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    loop {
        let s = rs::<Chain>(o);
        if s.active.is_none() {
            let src = match &s.source {
                Some(x) => x.clone(),
                None => return Ok(None),
            };
            match ab::iter_next(&src)? {
                Some(iterable) => {
                    let it = ab::get_iter(&iterable)?;
                    rs::<Chain>(o).active = Some(it);
                }
                None => {
                    rs::<Chain>(o).source = None;
                    return Ok(None);
                }
            }
        }
        let active = match &rs::<Chain>(o).active {
            Some(a) => a.clone(),
            None => continue,
        };
        match ab::iter_next(&active)? {
            Some(v) => return Ok(Some(v)),
            None => {
                let s = rs::<Chain>(o);
                if s.active.as_ref().is_some_and(|a| a.is(&active)) {
                    s.active = None;
                }
            }
        }
    }
}
iternext!(chain_next, chain_next_impl);

// ================================================================== combinations / cwr / permutations / product

fn try_vec<T: Clone>(n: usize, v: T) -> PyResult<Vec<T>> {
    let mut x = Vec::new();
    if x.try_reserve_exact(n).is_err() {
        return Err(memory_error());
    }
    x.resize(n, v);
    Ok(x)
}

struct Combinations {
    pool: Vec<PyRef>,
    indices: Vec<usize>,
    r: usize,
    result: Option<PyRef>,
    started: bool,
    stopped: bool,
}
impl Trav for Combinations {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = trav_vec(v, &self.pool);
        if r != 0 {
            return r;
        }
        trav!(v; self.result)
    }
}

fn get_r(o: &PyObject) -> PyResult<usize> {
    let r = index_arg(o)?;
    if r < 0 {
        raise!(ValueError, "r must be non-negative");
    }
    Ok(r as usize)
}

unsafe fn combinations_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("combinations", &["iterable", "r"], 0, 2);
    let [iterable, r] = a.parse::<2>(&p)?;
    let pool = ab::collect_iterable(&iterable.unwrap())?;
    let r = get_r(&r.unwrap())?;
    let stopped = r > pool.len();
    let indices = if stopped { Vec::new() } else { (0..r).collect() };
    fc::robj_new(tp, Combinations { pool, indices, r, result: None, started: false, stopped })
}
tpnew!(combinations_new, combinations_new_impl);

fn combinations_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Combinations>(o);
    if s.stopped {
        return Ok(None);
    }
    let n = s.pool.len();
    let r = s.r;
    if s.started {
        // find the rightmost index that can be incremented
        let mut i = r as isize - 1;
        while i >= 0 && s.indices[i as usize] == i as usize + n - r {
            i -= 1;
        }
        if i < 0 {
            s.stopped = true;
            s.result = None;
            return Ok(None);
        }
        let i = i as usize;
        s.indices[i] += 1;
        for j in i + 1..r {
            s.indices[j] = s.indices[j - 1] + 1;
        }
    }
    s.started = true;
    let items: Vec<PyRef> = s.indices.iter().map(|&k| s.pool[k].clone()).collect();
    Ok(Some(fc::reuse_tuple(&mut s.result, items)))
}
iternext!(combinations_next, combinations_next_impl);

unsafe fn cwr_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("combinations_with_replacement", &["iterable", "r"], 0, 2);
    let [iterable, r] = a.parse::<2>(&p)?;
    let pool = ab::collect_iterable(&iterable.unwrap())?;
    let r = get_r(&r.unwrap())?;
    let stopped = pool.is_empty() && r > 0;
    let indices = if stopped { Vec::new() } else { try_vec(r, 0usize)? };
    fc::robj_new(tp, Combinations { pool, indices, r, result: None, started: false, stopped })
}
tpnew!(cwr_new, cwr_new_impl);

fn cwr_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Combinations>(o);
    if s.stopped {
        return Ok(None);
    }
    let n = s.pool.len();
    let r = s.r;
    if s.started {
        let mut i = r as isize - 1;
        while i >= 0 && s.indices[i as usize] == n - 1 {
            i -= 1;
        }
        if i < 0 {
            s.stopped = true;
            s.result = None;
            return Ok(None);
        }
        let i = i as usize;
        let v = s.indices[i] + 1;
        for j in i..r {
            s.indices[j] = v;
        }
    }
    s.started = true;
    let items: Vec<PyRef> = s.indices.iter().map(|&k| s.pool[k].clone()).collect();
    Ok(Some(fc::reuse_tuple(&mut s.result, items)))
}
iternext!(cwr_next, cwr_next_impl);

struct Permutations {
    pool: Vec<PyRef>,
    indices: Vec<usize>,
    cycles: Vec<usize>,
    r: usize,
    result: Option<PyRef>,
    started: bool,
    stopped: bool,
}
impl Trav for Permutations {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = trav_vec(v, &self.pool);
        if r != 0 {
            return r;
        }
        trav!(v; self.result)
    }
}

unsafe fn permutations_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("permutations", &["iterable", "r"], 0, 1);
    let [iterable, r] = a.parse::<2>(&p)?;
    let pool = ab::collect_iterable(&iterable.unwrap())?;
    let n = pool.len();
    let r = match r {
        Some(r) if !r.is(none::none()) => {
            if !int::is_int(&r) {
                raise!(TypeError, "Expected int as r");
            }
            get_r(&r)?
        }
        _ => n,
    };
    let stopped = r > n;
    let (indices, cycles) = if stopped { (Vec::new(), Vec::new()) } else { ((0..n).collect(), (0..r).map(|i| n - i).collect()) };
    fc::robj_new(tp, Permutations { pool, indices, cycles, r, result: None, started: false, stopped })
}
tpnew!(permutations_new, permutations_new_impl);

fn permutations_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Permutations>(o);
    if s.stopped {
        return Ok(None);
    }
    let n = s.pool.len();
    let r = s.r;
    if s.started {
        if n == 0 {
            s.stopped = true;
            s.result = None;
            return Ok(None);
        }
        let mut i = r as isize - 1;
        loop {
            if i < 0 {
                s.stopped = true;
                s.result = None;
                return Ok(None);
            }
            let iu = i as usize;
            s.cycles[iu] -= 1;
            if s.cycles[iu] == 0 {
                // rotate indices[i:] left by one
                let first = s.indices[iu];
                for j in iu..n - 1 {
                    s.indices[j] = s.indices[j + 1];
                }
                s.indices[n - 1] = first;
                s.cycles[iu] = n - iu;
                i -= 1;
            } else {
                let j = s.cycles[iu];
                s.indices.swap(iu, n - j);
                break;
            }
        }
    }
    s.started = true;
    let items: Vec<PyRef> = s.indices[..r].iter().map(|&k| s.pool[k].clone()).collect();
    Ok(Some(fc::reuse_tuple(&mut s.result, items)))
}
iternext!(permutations_next, permutations_next_impl);

struct Product {
    pools: Vec<Vec<PyRef>>,
    indices: Vec<usize>,
    result: Option<PyRef>,
    started: bool,
    stopped: bool,
}
impl Trav for Product {
    fn trav(&self, v: VisitFn) -> c_int {
        for p in &self.pools {
            let r = trav_vec(v, p);
            if r != 0 {
                return r;
            }
        }
        trav!(v; self.result)
    }
}

unsafe fn product_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let mut repeat: isize = 1;
    if let Some(k) = unsafe { opt(kw) } {
        for (key, v) in crate::types::dict::items_vec(k) {
            if pystr::is_str(&key) && pystr::to_string_lossy(&key) == "repeat" {
                repeat = index_arg(&v)?;
                if repeat < 0 {
                    raise!(ValueError, "repeat argument cannot be negative");
                }
            } else {
                raise!(TypeError, "product() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&key));
            }
        }
    }
    let a = unsafe { targs(args) };
    let npools = a.len().checked_mul(repeat as usize);
    let npools = match npools {
        Some(n) if n < (isize::MAX as usize) / 8 => n,
        _ => raise!(OverflowError, "repeat argument too large"),
    };
    let mut base = Vec::with_capacity(a.len());
    for x in a {
        base.push(ab::collect_iterable(x)?);
    }
    let mut pools = Vec::new();
    if pools.try_reserve_exact(npools).is_err() {
        return Err(memory_error());
    }
    for _ in 0..repeat {
        for b in &base {
            pools.push(b.clone());
        }
    }
    let stopped = pools.iter().any(|p| p.is_empty());
    let indices = try_vec(npools, 0usize)?;
    fc::robj_new(tp, Product { pools, indices, result: None, started: false, stopped })
}
tpnew!(product_new, product_new_impl);

fn product_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Product>(o);
    if s.stopped {
        return Ok(None);
    }
    if s.started {
        let mut i = s.pools.len() as isize - 1;
        loop {
            if i < 0 {
                s.stopped = true;
                s.result = None;
                return Ok(None);
            }
            let iu = i as usize;
            s.indices[iu] += 1;
            if s.indices[iu] == s.pools[iu].len() {
                s.indices[iu] = 0;
                i -= 1;
            } else {
                break;
            }
        }
    }
    s.started = true;
    let items: Vec<PyRef> = s.indices.iter().enumerate().map(|(i, &k)| s.pools[i][k].clone()).collect();
    Ok(Some(fc::reuse_tuple(&mut s.result, items)))
}
iternext!(product_next, product_next_impl);

// ================================================================== compress

struct Compress {
    data: Option<PyRef>,
    selectors: Option<PyRef>,
}
impl Trav for Compress {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.data, self.selectors)
    }
}

unsafe fn compress_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("compress", &["data", "selectors"], 0, 2);
    let [data, sel] = a.parse::<2>(&p)?;
    let data = ab::get_iter(&data.unwrap())?;
    let sel = ab::get_iter(&sel.unwrap())?;
    fc::robj_new(tp, Compress { data: Some(data), selectors: Some(sel) })
}
tpnew!(compress_new, compress_new_impl);

fn compress_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Compress>(o);
    let (d, sel) = match (&s.data, &s.selectors) {
        (Some(d), Some(sel)) => (d.clone(), sel.clone()),
        _ => return Ok(None),
    };
    loop {
        let datum = match ab::iter_next(&d)? {
            Some(x) => x,
            None => return Ok(None),
        };
        let selector = match ab::iter_next(&sel)? {
            Some(x) => x,
            None => return Ok(None),
        };
        if ab::is_true(&selector)? {
            return Ok(Some(datum));
        }
    }
}
iternext!(compress_next, compress_next_impl);

// ================================================================== count

struct Count {
    cnt: Option<PyRef>,
    step: Option<PyRef>,
}
impl Trav for Count {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.cnt, self.step)
    }
}

fn is_number(o: &PyObject) -> bool {
    if int::is_int(o) || crate::types::float::is_float(o) || crate::types::complex::is_complex(o) {
        return true;
    }
    let nb = o.ty().tp_as_number;
    !nb.is_null() && unsafe { (*nb).nb_index.is_some() || (*nb).nb_int.is_some() || (*nb).nb_float.is_some() }
}

unsafe fn count_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("count", &["start", "step"], 0, 0);
    let [start, step] = a.parse::<2>(&p)?;
    let start = start.unwrap_or_else(|| int::new_int(0));
    let step = step.unwrap_or_else(|| int::new_int(1));
    if !is_number(&start) || !is_number(&step) {
        raise!(TypeError, "a number is required");
    }
    fc::robj_new(tp, Count { cnt: Some(start), step: Some(step) })
}
tpnew!(count_new, count_new_impl);

fn count_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    use crate::types::dict::ob_lock;
    // Concurrent next() calls on one count object must neither lose increments nor
    // free a value another thread is still reading.  The fields are read (with a
    // new reference) and replaced only under the object's lock; the sum is computed
    // outside it (it may allocate or run Python code) and is published only if no
    // other thread advanced the counter meanwhile, otherwise we start over.
    loop {
        let (cnt, step) = {
            let _g = ob_lock(o);
            let s = rs::<Count>(o);
            (s.cnt.clone().unwrap(), s.step.clone().unwrap())
        };
        let next = if int::is_int_exact(&cnt) && int::is_int_exact(&step) && int::is_compact(&cnt) && int::is_compact(&step) {
            match int::compact_value(&cnt).checked_add(int::compact_value(&step)) {
                Some(v) => int::new_int(v),
                None => ab::binary_op(&cnt, &step, NbSlot::Add)?,
            }
        } else {
            ab::binary_op(&cnt, &step, NbSlot::Add)?
        };
        let old = {
            let _g = ob_lock(o);
            let s = rs::<Count>(o);
            if !s.cnt.as_ref().is_some_and(|c| c.is(&cnt)) {
                None
            } else {
                Some(s.cnt.replace(next))
            }
        };
        match old {
            Some(old) => {
                drop(old);
                return Ok(Some(cnt));
            }
            None => continue,
        }
    }
}
iternext!(count_next, count_next_impl);

unsafe extern "C" fn count_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let (cnt, step) = {
                let _g = crate::types::dict::ob_lock(o);
                let s = rs::<Count>(o);
                (s.cnt.clone().unwrap(), s.step.clone().unwrap())
            };
            let name = fc::repr_type_name(o);
            let cr = ab::repr_string(&cnt)?;
            let step_is_one = int::is_int(&step) && int::as_i64(&step) == Some(1);
            if step_is_one {
                return Ok(pystr::new_str(&format!("{}({})", name, cr)));
            }
            Ok(pystr::new_str(&format!("{}({}, {})", name, cr, ab::repr_string(&step)?)))
        })())
    }
}

// ================================================================== cycle

struct Cycle {
    it: Option<PyRef>,
    saved: Vec<PyRef>,
    index: usize,
    firstpass: bool,
}
impl Trav for Cycle {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = trav_vec(v, &self.saved);
        if r != 0 {
            return r;
        }
        trav!(v; self.it)
    }
}

unsafe fn cycle_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw_unless_init_overridden(tp, cycle_type(), "cycle", kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos("cycle", a, 1, 1)?;
    let it = ab::get_iter(a[0])?;
    fc::robj_new(tp, Cycle { it: Some(it), saved: Vec::new(), index: 0, firstpass: true })
}
tpnew!(cycle_new, cycle_new_impl);

fn cycle_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Cycle>(o);
    if let Some(it) = s.it.clone() {
        match ab::iter_next(&it)? {
            Some(v) => {
                let s = rs::<Cycle>(o);
                if s.firstpass {
                    s.saved.push(v.clone());
                }
                return Ok(Some(v));
            }
            None => {
                let s = rs::<Cycle>(o);
                s.it = None;
                s.firstpass = false;
            }
        }
    }
    let s = rs::<Cycle>(o);
    if s.saved.is_empty() {
        return Ok(None);
    }
    let v = s.saved[s.index].clone();
    s.index += 1;
    if s.index >= s.saved.len() {
        s.index = 0;
    }
    Ok(Some(v))
}
iternext!(cycle_next, cycle_next_impl);

// ================================================================== dropwhile / takewhile / filterfalse / starmap

struct PredIter {
    func: Option<PyRef>,
    it: Option<PyRef>,
    state: bool, // dropwhile: done dropping; takewhile: stopped
}
impl Trav for PredIter {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.func, self.it)
    }
}

unsafe fn pred_new(tp: *mut PyTypeObject, base: *mut PyTypeObject, name: &str, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw_unless_init_overridden(tp, base, name, kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos(name, a, 2, 2)?;
    let it = ab::get_iter(a[1])?;
    fc::robj_new(tp, PredIter { func: Some(a[0].to_ref()), it: Some(it), state: false })
}
unsafe fn dropwhile_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { pred_new(tp, dropwhile_type(), "dropwhile", args, kw) }
}
unsafe fn takewhile_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { pred_new(tp, takewhile_type(), "takewhile", args, kw) }
}
unsafe fn filterfalse_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { pred_new(tp, filterfalse_type(), "filterfalse", args, kw) }
}
unsafe fn starmap_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { pred_new(tp, starmap_type(), "starmap", args, kw) }
}
tpnew!(dropwhile_new, dropwhile_new_impl);
tpnew!(takewhile_new, takewhile_new_impl);
tpnew!(filterfalse_new, filterfalse_new_impl);
tpnew!(starmap_new, starmap_new_impl);

fn pred_parts(o: &PyObject) -> (PyRef, PyRef) {
    let s = rs::<PredIter>(o);
    (s.func.clone().unwrap(), s.it.clone().unwrap())
}

fn dropwhile_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let (f, it) = pred_parts(o);
    loop {
        let item = match ab::iter_next(&it)? {
            Some(x) => x,
            None => return Ok(None),
        };
        if rs::<PredIter>(o).state {
            return Ok(Some(item));
        }
        let good = ab::call(&f, &[&item])?;
        if !ab::is_true(&good)? {
            rs::<PredIter>(o).state = true;
            return Ok(Some(item));
        }
    }
}
iternext!(dropwhile_next, dropwhile_next_impl);

fn takewhile_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    if rs::<PredIter>(o).state {
        return Ok(None);
    }
    let (f, it) = pred_parts(o);
    let item = match ab::iter_next(&it)? {
        Some(x) => x,
        None => return Ok(None),
    };
    let good = ab::call(&f, &[&item])?;
    if ab::is_true(&good)? {
        return Ok(Some(item));
    }
    rs::<PredIter>(o).state = true;
    Ok(None)
}
iternext!(takewhile_next, takewhile_next_impl);

fn filterfalse_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let (f, it) = pred_parts(o);
    let is_bool_or_none = f.is(none::none()) || f.is(crate::types::typeobject::type_obj(bool_::bool_type()));
    loop {
        let item = match ab::iter_next(&it)? {
            Some(x) => x,
            None => return Ok(None),
        };
        let ok = if is_bool_or_none {
            ab::is_true(&item)?
        } else {
            let r = ab::call(&f, &[&item])?;
            ab::is_true(&r)?
        };
        if !ok {
            return Ok(Some(item));
        }
    }
}
iternext!(filterfalse_next, filterfalse_next_impl);

fn starmap_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let (f, it) = pred_parts(o);
    let args = match ab::iter_next(&it)? {
        Some(x) => x,
        None => return Ok(None),
    };
    let t = ab::sequence_tuple(&args)?;
    Ok(Some(ab::call(&f, tuple::tuple_items(&t))?))
}
iternext!(starmap_next, starmap_next_impl);

// ================================================================== groupby / _grouper

struct GroupBy {
    it: Option<PyRef>,
    keyfunc: Option<PyRef>,
    tgtkey: Option<PyRef>,
    currkey: Option<PyRef>,
    currvalue: Option<PyRef>,
    currgrouper: usize, // identity of the active _grouper (borrowed)
}
impl Trav for GroupBy {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.it, self.keyfunc, self.tgtkey, self.currkey, self.currvalue)
    }
}

struct Grouper {
    parent: Option<PyRef>,
    tgtkey: Option<PyRef>,
}
impl Trav for Grouper {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.parent, self.tgtkey)
    }
}

unsafe fn groupby_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("groupby", &["iterable", "key"], 0, 1);
    let [iterable, key] = a.parse::<2>(&p)?;
    let it = ab::get_iter(&iterable.unwrap())?;
    let keyfunc = key.filter(|k| !k.is(none::none()));
    fc::robj_new(tp, GroupBy { it: Some(it), keyfunc, tgtkey: None, currkey: None, currvalue: None, currgrouper: 0 })
}
tpnew!(groupby_new, groupby_new_impl);

/// Advance the underlying iterator: sets currvalue/currkey. Ok(false) on exhaustion.
fn groupby_step(o: &PyObject) -> PyResult<bool> {
    let s = rs::<GroupBy>(o);
    let it = match &s.it {
        Some(i) => i.clone(),
        None => return Ok(false),
    };
    let kf = s.keyfunc.clone();
    let newvalue = match ab::iter_next(&it)? {
        Some(v) => v,
        None => return Ok(false),
    };
    let newkey = match kf {
        Some(f) => ab::call(&f, &[&newvalue])?,
        None => newvalue.clone(),
    };
    let s = rs::<GroupBy>(o);
    s.currvalue = Some(newvalue);
    s.currkey = Some(newkey);
    Ok(true)
}

fn groupby_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    rs::<GroupBy>(o).currgrouper = 0;
    loop {
        let s = rs::<GroupBy>(o);
        if s.currkey.is_none() {
            if !groupby_step(o)? {
                return Ok(None);
            }
            continue;
        }
        let tgt = match &s.tgtkey {
            None => break,
            Some(t) => t.clone(),
        };
        let cur = s.currkey.clone().unwrap();
        if !ab::rich_eq(&tgt, &cur)? {
            break;
        }
        if !groupby_step(o)? {
            return Ok(None);
        }
    }
    let s = rs::<GroupBy>(o);
    let key = match s.currkey.clone() {
        Some(k) => k,
        None => return Ok(None),
    };
    s.tgtkey = Some(key.clone());
    let g = fc::robj_new(grouper_type(), Grouper { parent: Some(o.to_ref()), tgtkey: Some(key.clone()) })?;
    rs::<GroupBy>(o).currgrouper = g.as_ptr() as usize;
    Ok(Some(tuple::tuple2(key, g)))
}
iternext!(groupby_next, groupby_next_impl);

unsafe fn grouper_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw("_grouper", kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos("_grouper", a, 2, 2)?;
    if !a[0].is_type(groupby_type()) {
        raise!(TypeError, "_grouper() argument 1 must be itertools.groupby, not {}", fc::tname(a[0]));
    }
    fc::robj_new(tp, Grouper { parent: Some(a[0].to_ref()), tgtkey: Some(a[1].to_ref()) })
}
tpnew!(grouper_new, grouper_new_impl);

fn grouper_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let g = rs::<Grouper>(o);
    let parent = g.parent.clone().unwrap();
    let tgt = g.tgtkey.clone().unwrap();
    if rs::<GroupBy>(&parent).currgrouper != o.as_ptr() as usize {
        return Ok(None);
    }
    if rs::<GroupBy>(&parent).currvalue.is_none() && !groupby_step(&parent)? {
        return Ok(None);
    }
    let cur = match rs::<GroupBy>(&parent).currkey.clone() {
        Some(c) => c,
        None => return Ok(None),
    };
    if !ab::rich_eq(&tgt, &cur)? {
        return Ok(None);
    }
    // the comparison may have advanced the parent
    if rs::<GroupBy>(&parent).currgrouper != o.as_ptr() as usize {
        return Ok(None);
    }
    Ok(rs::<GroupBy>(&parent).currvalue.take())
}
iternext!(grouper_next, grouper_next_impl);

// ================================================================== islice

struct Islice {
    it: Option<PyRef>,
    next: isize,
    stop: isize, // -1: no stop
    step: isize,
    cnt: isize,
}
impl Trav for Islice {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.it)
    }
}

fn islice_index(o: &PyObject, msg: &str) -> PyResult<isize> {
    if o.is(none::none()) {
        return Ok(-1);
    }
    if !ab::has_index(o) {
        return Err(value_error(msg));
    }
    let v = match ab::index_as_ssize(o, None) {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    if v < 0 {
        return Err(value_error(msg));
    }
    Ok(v)
}

unsafe fn islice_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw_unless_init_overridden(tp, islice_type(), "islice", kw)? };
    let a = unsafe { targs(args) };
    if a.len() < 2 || a.len() > 4 {
        if a.len() < 2 {
            raise!(TypeError, "islice expected at least 2 arguments, got {}", a.len());
        }
        raise!(TypeError, "islice expected at most 4 arguments, got {}", a.len());
    }
    let (start, stop, step);
    if a.len() == 2 {
        start = 0;
        stop = islice_index(a[1], "Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.")?;
        step = 1;
    } else {
        let st = islice_index(a[1], "Indices for islice() must be None or an integer: 0 <= x <= sys.maxsize.")?;
        start = if st < 0 { 0 } else { st };
        stop = islice_index(a[2], "Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.")?;
        if a.len() == 4 && !a[3].is(none::none()) {
            if !ab::has_index(a[3]) {
                raise!(ValueError, "Step for islice() must be a positive integer or None.");
            }
            let s = ab::index_as_ssize(a[3], None)?;
            if s < 1 {
                raise!(ValueError, "Step for islice() must be a positive integer or None.");
            }
            step = s;
        } else {
            step = 1;
        }
    }
    let it = ab::get_iter(a[0])?;
    fc::robj_new(tp, Islice { it: Some(it), next: start, stop, step, cnt: 0 })
}
tpnew!(islice_new, islice_new_impl);

fn islice_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Islice>(o);
    let it = match &s.it {
        Some(i) => i.clone(),
        None => return Ok(None),
    };
    let stop = s.stop;
    while rs::<Islice>(o).cnt < rs::<Islice>(o).next {
        match ab::iter_next(&it)? {
            Some(_) => rs::<Islice>(o).cnt += 1,
            None => {
                rs::<Islice>(o).it = None;
                return Ok(None);
            }
        }
    }
    let s = rs::<Islice>(o);
    if stop != -1 && s.cnt >= stop {
        s.it = None;
        return Ok(None);
    }
    let item = match ab::iter_next(&it)? {
        Some(x) => x,
        None => {
            rs::<Islice>(o).it = None;
            return Ok(None);
        }
    };
    let s = rs::<Islice>(o);
    s.cnt += 1;
    let old = s.next;
    s.next = s.next.wrapping_add(s.step);
    if s.next < old || (stop != -1 && s.next > stop) {
        s.next = stop;
    }
    Ok(Some(item))
}
iternext!(islice_next, islice_next_impl);

// ================================================================== pairwise

struct Pairwise {
    it: Option<PyRef>,
    old: Option<PyRef>,
    result: Option<PyRef>,
}
impl Trav for Pairwise {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.it, self.old, self.result)
    }
}

unsafe fn pairwise_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw_unless_init_overridden(tp, pairwise_type(), "pairwise", kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos("pairwise", a, 1, 1)?;
    let it = ab::get_iter(a[0])?;
    fc::robj_new(tp, Pairwise { it: Some(it), old: None, result: None })
}
tpnew!(pairwise_new, pairwise_new_impl);

fn pairwise_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let it = match &rs::<Pairwise>(o).it {
        Some(i) => i.clone(),
        None => return Ok(None),
    };
    let old = match rs::<Pairwise>(o).old.clone() {
        Some(x) => x,
        None => {
            let r = ab::iter_next(&it);
            let s = rs::<Pairwise>(o);
            match r {
                Ok(Some(v)) => {
                    s.old = Some(v.clone());
                    if s.it.is_none() {
                        s.old = None;
                        return Ok(None);
                    }
                    v
                }
                Ok(None) => {
                    s.old = None;
                    s.it = None;
                    return Ok(None);
                }
                Err(e) => {
                    s.old = None;
                    s.it = None;
                    return Err(e);
                }
            }
        }
    };
    let it = match &rs::<Pairwise>(o).it {
        Some(i) => i.clone(),
        None => return Ok(None),
    };
    let new = match ab::iter_next(&it) {
        Ok(Some(v)) => v,
        Ok(None) => {
            let s = rs::<Pairwise>(o);
            s.it = None;
            s.old = None;
            return Ok(None);
        }
        Err(e) => {
            let s = rs::<Pairwise>(o);
            s.it = None;
            s.old = None;
            return Err(e);
        }
    };
    let s = rs::<Pairwise>(o);
    let r = fc::reuse_tuple(&mut s.result, vec![old, new.clone()]);
    s.old = Some(new);
    Ok(Some(r))
}
iternext!(pairwise_next, pairwise_next_impl);

// ================================================================== repeat

struct Repeat {
    element: Option<PyRef>,
    cnt: isize, // -1 = forever
}
impl Trav for Repeat {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.element)
    }
}

unsafe fn repeat_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let a = unsafe { VcArgs::from_c(args, kw) };
    let p = Params::new("repeat", &["object", "times"], 0, 1);
    let [elem, times] = a.parse::<2>(&p)?;
    let cnt = match times {
        Some(t) => {
            let n = index_arg(&t)?;
            if n < 0 { 0 } else { n }
        }
        None => -1,
    };
    fc::robj_new(tp, Repeat { element: elem, cnt })
}
tpnew!(repeat_new, repeat_new_impl);

fn repeat_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Repeat>(o);
    if s.cnt == 0 {
        return Ok(None);
    }
    if s.cnt > 0 {
        s.cnt -= 1;
    }
    Ok(s.element.clone())
}
iternext!(repeat_next, repeat_next_impl);

unsafe extern "C" fn repeat_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let s = rs::<Repeat>(o);
            let (e, cnt) = (s.element.clone().unwrap(), s.cnt);
            let name = fc::repr_type_name(o);
            let er = ab::repr_string(&e)?;
            if cnt == -1 {
                Ok(pystr::new_str(&format!("{}({})", name, er)))
            } else {
                Ok(pystr::new_str(&format!("{}({}, {})", name, er, cnt)))
            }
        })())
    }
}

fn repeat_length_hint(o: &PyObject) -> PyResult {
    let s = rs::<Repeat>(o);
    if s.cnt == -1 {
        raise!(TypeError, "len() of unsized object");
    }
    Ok(int::new_int(s.cnt as i64))
}

// ================================================================== zip_longest

struct ZipLongest {
    its: Vec<Option<PyRef>>,
    numactive: usize,
    fillvalue: Option<PyRef>,
    result: Option<PyRef>,
}
impl Trav for ZipLongest {
    fn trav(&self, v: VisitFn) -> c_int {
        for i in &self.its {
            let r = trav_opt(v, i);
            if r != 0 {
                return r;
            }
        }
        trav!(v; self.fillvalue, self.result)
    }
}

unsafe fn ziplongest_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    let mut fill = none::none_ref();
    if let Some(k) = unsafe { opt(kw) } {
        for (key, v) in crate::types::dict::items_vec(k) {
            if pystr::is_str(&key) && pystr::to_string_lossy(&key) == "fillvalue" {
                fill = v;
            } else {
                raise!(TypeError, "zip_longest() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&key));
            }
        }
    }
    let a = unsafe { targs(args) };
    let mut its = Vec::with_capacity(a.len());
    for x in a.iter() {
        match ab::get_iter(x) {
            Ok(it) => its.push(Some(it)),
            Err(e) => return Err(e),
        }
    }
    let n = its.len();
    fc::robj_new(tp, ZipLongest { its, numactive: n, fillvalue: Some(fill), result: None })
}
tpnew!(ziplongest_new, ziplongest_new_impl);

fn ziplongest_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<ZipLongest>(o);
    let n = s.its.len();
    if n == 0 || s.numactive == 0 {
        return Ok(None);
    }
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        let s = rs::<ZipLongest>(o);
        let it = s.its[i].clone();
        let item = match it {
            None => s.fillvalue.clone().unwrap(),
            Some(it) => match ab::iter_next(&it)? {
                Some(v) => v,
                None => {
                    let s = rs::<ZipLongest>(o);
                    s.numactive -= 1;
                    s.its[i] = None;
                    if s.numactive == 0 {
                        return Ok(None);
                    }
                    s.fillvalue.clone().unwrap()
                }
            },
        };
        items.push(item);
    }
    let s = rs::<ZipLongest>(o);
    Ok(Some(fc::reuse_tuple(&mut s.result, items)))
}
iternext!(ziplongest_next, ziplongest_next_impl);

// ================================================================== tee

const LINKCELLS: usize = 57;

struct TeeData {
    it: Option<PyRef>,
    values: Vec<PyRef>,
    next: Option<PyRef>,
    running: bool,
}
impl Trav for TeeData {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = trav_vec(v, &self.values);
        if r != 0 {
            return r;
        }
        trav!(v; self.it, self.next)
    }
}
impl Drop for TeeData {
    fn drop(&mut self) {
        // Unlink the chain iteratively to avoid deep recursion on long tees.
        let mut nxt = self.next.take();
        while let Some(n) = nxt {
            if n.refcnt() == 1 && n.is_type(teedata_type()) {
                nxt = rs::<TeeData>(&n).next.take();
            } else {
                nxt = None;
            }
            drop(n);
        }
    }
}

struct Tee {
    data: Option<PyRef>,
    index: usize,
}
impl Trav for Tee {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.data)
    }
}

fn new_teedata(it: PyRef) -> PyResult {
    fc::robj_new(teedata_type(), TeeData { it: Some(it), values: Vec::with_capacity(LINKCELLS), next: None, running: false })
}

unsafe fn teedata_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw("_tee_dataobject", kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos("_tee_dataobject", a, 3, 3)?;
    if !crate::types::list::is_list(a[1]) {
        raise!(TypeError, "_tee_dataobject() argument 2 must be list, not {}", fc::tname(a[1]));
    }
    let values = crate::types::list::list_to_vec(a[1]);
    if values.len() > LINKCELLS {
        raise!(ValueError, "_tee_dataobject should have at most {} values", LINKCELLS);
    }
    let next = if a[2].is(none::none()) {
        None
    } else if a[2].is_type(teedata_type()) {
        Some(a[2].to_ref())
    } else {
        raise!(TypeError, "_tee_dataobject next link must be a _tee_dataobject");
    };
    fc::robj_new(tp, TeeData { it: Some(a[0].to_ref()), values, next, running: false })
}
tpnew!(teedata_new, teedata_new_impl);

fn teedata_jumplink(d: &PyObject) -> PyResult {
    // shared by every tee of the group, which may run on different threads
    let _lk = crate::objlock::lock_held(d);
    let s = rs::<TeeData>(d);
    if let Some(n) = &s.next {
        return Ok(n.clone());
    }
    let it = s.it.clone().unwrap();
    let n = new_teedata(it)?;
    rs::<TeeData>(d).next = Some(n.clone());
    Ok(n)
}

fn teedata_getitem(d: &PyObject, i: usize) -> PyResult<Option<PyRef>> {
    // The lock covers the bookkeeping only, not the call into the source iterator
    // (it may block, waiting for another thread that uses this tee): the `running`
    // flag makes a concurrent step fail with "cannot re-enter" instead of waiting.
    let it = {
        let _lk = crate::objlock::lock_held(d);
        let s = rs::<TeeData>(d);
        if i < s.values.len() {
            return Ok(Some(s.values[i].clone()));
        }
        if s.running {
            raise!(RuntimeError, "cannot re-enter the tee iterator");
        }
        let it = match &s.it {
            Some(i) => i.clone(),
            None => return Ok(None),
        };
        s.running = true;
        it
    };
    let r = ab::iter_next(&it);
    let _lk = crate::objlock::lock_held(d);
    let s = rs::<TeeData>(d);
    s.running = false;
    match r? {
        Some(v) => {
            s.values.push(v.clone());
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

fn tee_from_iterable(tp: *mut PyTypeObject, iterable: &PyObject) -> PyResult {
    let it = ab::get_iter(iterable)?;
    if it.is_type(tee_type()) {
        return tee_copy(&it);
    }
    let data = new_teedata(it)?;
    fc::robj_new(tp, Tee { data: Some(data), index: 0 })
}

unsafe fn tee_new_impl(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> PyResult {
    unsafe { fc::no_kw("_tee", kw)? };
    let a = unsafe { targs(args) };
    crate::args::check_pos("_tee", a, 1, 1)?;
    tee_from_iterable(tp, a[0])
}
tpnew!(tee_new, tee_new_impl);

fn tee_copy(o: &PyObject) -> PyResult {
    let s = rs::<Tee>(o);
    fc::robj_new(tee_type(), Tee { data: s.data.clone(), index: s.index })
}

fn tee_next_impl(o: &PyObject) -> PyResult<Option<PyRef>> {
    let s = rs::<Tee>(o);
    if s.index >= LINKCELLS {
        let d = s.data.clone().unwrap();
        let link = teedata_jumplink(&d)?;
        let s = rs::<Tee>(o);
        s.data = Some(link);
        s.index = 0;
    }
    let s = rs::<Tee>(o);
    let d = s.data.clone().unwrap();
    let idx = s.index;
    match teedata_getitem(&d, idx)? {
        Some(v) => {
            rs::<Tee>(o).index += 1;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}
iternext!(tee_next, tee_next_impl);

fn itertools_tee(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tee", a, 1, 2)?;
    let n = if a.len() > 1 { index_arg(a[1])? } else { 2 };
    if n < 0 {
        raise!(ValueError, "n must be >= 0");
    }
    if n == 0 {
        return Ok(tuple::empty_tuple());
    }
    let it = ab::get_iter(a[0])?;
    let first = tee_from_iterable(tee_type(), &it)?;
    let mut out = vec![first.clone()];
    for _ in 1..n {
        out.push(tee_copy(&first)?);
    }
    Ok(tuple::new_tuple(out))
}

// ================================================================== module

// __sizeof__ of the combinatoric iterators reports the footprint of the classic C layout
// (header, object pointers, counters, plus one machine word per index entry), which
// is what sys.getsizeof() callers budget with; our Rust state is laid out differently.
fn classic_sizeof(o: &PyObject, base: *mut PyTypeObject, words: usize, ints: usize, nindex: usize) -> PyResult {
    let w = std::mem::size_of::<isize>();
    let raw = std::mem::size_of::<PyObject>() + words * w + ints * 4;
    let fixed = raw.div_ceil(w) * w;
    // instance dicts/weakref slots a subclass adds
    let extra = unsafe { (o.ty().tp_basicsize - (*base).tp_basicsize).max(0) as usize };
    Ok(int::new_int((fixed + extra + nindex * w) as i64))
}
fn product_sizeof(o: &PyObject) -> PyResult {
    let n = rs::<Product>(o).indices.len();
    classic_sizeof(o, product_type(), 3, 1, n)
}
fn combinations_sizeof(o: &PyObject) -> PyResult {
    let n = rs::<Combinations>(o).r;
    let base = if crate::types::typeobject::is_subtype(o.type_ptr(), cwr_type()) { cwr_type() } else { combinations_type() };
    classic_sizeof(o, base, 4, 1, n)
}
fn permutations_sizeof(o: &PyObject) -> PyResult {
    let s = rs::<Permutations>(o);
    classic_sizeof(o, permutations_type(), 5, 1, s.pool.len() + s.r)
}

fn class_getitem_m() -> PyMethodDef {
    crate::methods::def("__class_getitem__", crate::methods::o(fc::class_getitem), METH_O | METH_CLASS, "See PEP 585")
}

unsafe fn setup<T: Trav>(
    t: *mut PyTypeObject,
    name: &'static std::ffi::CStr,
    newf: newfunc,
    next: iternextfunc,
    doc: &'static std::ffi::CStr,
    basetype: bool,
) {
    unsafe {
        fc::init_robj_type::<T>(t, name, if basetype { Py_TPFLAGS_BASETYPE } else { 0 });
        (*t).tp_new = newf;
        (*t).tp_iter = Some(self_iter);
        (*t).tp_iternext = next;
        (*t).tp_doc = doc.as_ptr();
    }
}

fn init_types() -> PyResult<()> {
    unsafe {
        if (*accumulate_type()).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup::<Accumulate>(accumulate_type(), c"itertools.accumulate", Some(accumulate_new), Some(accumulate_next),
            c"accumulate(iterable, func=None, *, initial=None)\n--\n\nReturn series of accumulated sums (or other binary function results).", true);
        setup::<Batched>(batched_type(), c"itertools.batched", Some(batched_new), Some(batched_next),
            c"batched(iterable, n, *, strict=False)\n--\n\nBatch data into tuples of length n. The last batch may be shorter than n.\n\nLoops over the input iterable and accumulates data into tuples\nup to size n.  The input is consumed lazily, just enough to\nfill a batch.  The result is yielded as soon as a batch is full\nor when the input iterable is exhausted.\n\n    >>> for batch in batched('ABCDEFG', 3):\n    ...     print(batch)\n    ...\n    ('A', 'B', 'C')\n    ('D', 'E', 'F')\n    ('G',)\n\nIf \"strict\" is True, raises a ValueError if the final batch is shorter\nthan n.", true);
        setup::<Chain>(chain_type(), c"itertools.chain", Some(chain_new), Some(chain_next),
            c"chain(*iterables)\n--\n\nReturn a chain object whose .__next__() method returns elements from the\nfirst iterable until it is exhausted, then elements from the next\niterable, until all of the iterables are exhausted.", true);
        (*chain_type()).tp_methods = crate::methods::table(vec![
            crate::methods::def("from_iterable", crate::methods::o(chain_from_iterable), METH_O | METH_CLASS,
                "from_iterable($type, iterable, /)\n--\n\nAlternative chain() constructor taking a single iterable argument that evaluates lazily."),
            class_getitem_m(),
        ]);
        setup::<Combinations>(combinations_type(), c"itertools.combinations", Some(combinations_new), Some(combinations_next),
            c"combinations(iterable, r)\n--\n\nReturn successive r-length combinations of elements in the iterable.\n\ncombinations(range(4), 3) --> (0,1,2), (0,1,3), (0,2,3), (1,2,3)", true);
        setup::<Combinations>(cwr_type(), c"itertools.combinations_with_replacement", Some(cwr_new), Some(cwr_next),
            c"combinations_with_replacement(iterable, r)\n--\n\nReturn successive r-length combinations of elements in the iterable allowing individual elements to have successive repeats.\n\ncombinations_with_replacement('ABC', 2) --> ('A','A'), ('A','B'), ('A','C'), ('B','B'), ('B','C'), ('C','C')", true);
        setup::<Compress>(compress_type(), c"itertools.compress", Some(compress_new), Some(compress_next),
            c"compress(data, selectors)\n--\n\nReturn data elements corresponding to true selector elements.\n\nForms a shorter iterator from selected data elements using the selectors to\nchoose the data elements.", true);
        setup::<Count>(count_type(), c"itertools.count", Some(count_new), Some(count_next),
            c"count(start=0, step=1)\n--\n\nReturn a count object whose .__next__() method returns consecutive values.\n\nEquivalent to:\n    def count(firstval=0, step=1):\n        x = firstval\n        while 1:\n            yield x\n            x += step", true);
        (*count_type()).tp_repr = Some(count_repr);
        setup::<Cycle>(cycle_type(), c"itertools.cycle", Some(cycle_new), Some(cycle_next),
            c"cycle(iterable, /)\n--\n\nReturn elements from the iterable until it is exhausted. Then repeat the sequence indefinitely.", true);
        setup::<PredIter>(dropwhile_type(), c"itertools.dropwhile", Some(dropwhile_new), Some(dropwhile_next),
            c"dropwhile(predicate, iterable, /)\n--\n\nDrop items from the iterable while predicate(item) is true.\n\nAfterwards, return every element until the iterable is exhausted.", true);
        setup::<PredIter>(takewhile_type(), c"itertools.takewhile", Some(takewhile_new), Some(takewhile_next),
            c"takewhile(predicate, iterable, /)\n--\n\nReturn successive entries from an iterable as long as the predicate evaluates to true for each entry.", true);
        setup::<PredIter>(filterfalse_type(), c"itertools.filterfalse", Some(filterfalse_new), Some(filterfalse_next),
            c"filterfalse(function, iterable, /)\n--\n\nReturn those items of iterable for which function(item) is false.\n\nIf function is None, return the items that are false.", true);
        setup::<PredIter>(starmap_type(), c"itertools.starmap", Some(starmap_new), Some(starmap_next),
            c"starmap(function, iterable, /)\n--\n\nReturn an iterator whose values are returned from the function evaluated with an argument tuple taken from the given sequence.", true);
        setup::<GroupBy>(groupby_type(), c"itertools.groupby", Some(groupby_new), Some(groupby_next),
            c"groupby(iterable, key=None)\n--\n\nmake an iterator that returns consecutive keys and groups from the iterable\n\n  iterable\n    Elements to divide into groups according to the key function.\n  key\n    A function for computing the group category for each element.\n    If the key function is not specified or is None, the element itself\n    is used for grouping.", true);
        setup::<Grouper>(grouper_type(), c"itertools._grouper", Some(grouper_new), Some(grouper_next), c"", false);
        (*grouper_type()).tp_doc = std::ptr::null();
        setup::<Islice>(islice_type(), c"itertools.islice", Some(islice_new), Some(islice_next),
            c"islice(iterable, stop) --> islice object\nislice(iterable, start, stop[, step]) --> islice object\n\nReturn an iterator whose next() method returns selected values from an\niterable.  If start is specified, will skip all preceding elements;\notherwise, start defaults to zero.  Step defaults to one.  If\nspecified as another value, step determines how many values are\nskipped between successive calls.  Works like a slice() on a list\nbut returns an iterator.", true);
        setup::<Pairwise>(pairwise_type(), c"itertools.pairwise", Some(pairwise_new), Some(pairwise_next),
            c"pairwise(iterable, /)\n--\n\nReturn an iterator of overlapping pairs taken from the input iterator.\n\n    s -> (s0,s1), (s1,s2), (s2, s3), ...", true);
        setup::<Permutations>(permutations_type(), c"itertools.permutations", Some(permutations_new), Some(permutations_next),
            c"permutations(iterable, r=None)\n--\n\nReturn successive r-length permutations of elements in the iterable.\n\npermutations(range(3), 2) --> (0,1), (0,2), (1,0), (1,2), (2,0), (2,1)", true);
        setup::<Product>(product_type(), c"itertools.product", Some(product_new), Some(product_next),
            c"product(*iterables, repeat=1)\n--\n\nCartesian product of input iterables.  Equivalent to nested for-loops.\n\nFor example, product(A, B) returns the same as:  ((x,y) for x in A for y in B).\nThe leftmost iterators are in the outermost for-loop, so the output tuples\ncycle in a manner similar to an odometer (with the rightmost element changing\non every iteration).\n\nTo compute the product of an iterable with itself, specify the number\nof repetitions with the optional repeat keyword argument. For example,\nproduct(A, repeat=4) means the same as product(A, A, A, A).\n\nproduct('ab', range(3)) --> ('a',0) ('a',1) ('a',2) ('b',0) ('b',1) ('b',2)\nproduct((0,1), (0,1), (0,1)) --> (0,0,0) (0,0,1) (0,1,0) (0,1,1) (1,0,0) ...", true);
        (*product_type()).tp_methods = mdefs![("__sizeof__", NOARGS, product_sizeof, "Returns size in memory, in bytes.")];
        (*combinations_type()).tp_methods = mdefs![("__sizeof__", NOARGS, combinations_sizeof, "Returns size in memory, in bytes.")];
        (*cwr_type()).tp_methods = mdefs![("__sizeof__", NOARGS, combinations_sizeof, "Returns size in memory, in bytes.")];
        (*permutations_type()).tp_methods = mdefs![("__sizeof__", NOARGS, permutations_sizeof, "Returns size in memory, in bytes.")];
        setup::<Repeat>(repeat_type(), c"itertools.repeat", Some(repeat_new), Some(repeat_next),
            c"repeat(object [,times]) -> create an iterator which returns the object\nfor the specified number of times.  If not specified, returns the object\nendlessly.", true);
        (*repeat_type()).tp_repr = Some(repeat_repr);
        (*repeat_type()).tp_methods = mdefs![("__length_hint__", NOARGS, repeat_length_hint, "Private method returning an estimate of len(list(it)).")];
        setup::<ZipLongest>(ziplongest_type(), c"itertools.zip_longest", Some(ziplongest_new), Some(ziplongest_next),
            c"zip_longest(*iterables, fillvalue=None)\n--\n\nReturn a zip_longest object whose .__next__() method returns a tuple where\nthe i-th element comes from the i-th iterable argument.  The .__next__()\nmethod continues until the longest iterable in the argument sequence\nis exhausted and then it raises StopIteration.  When the shorter iterables\nare exhausted, the fillvalue is substituted in their place.  The fillvalue\ndefaults to None or can be specified by a keyword argument.", true);
        setup::<Tee>(tee_type(), c"itertools._tee", Some(tee_new), Some(tee_next),
            c"_tee(iterable, /)\n--\n\nIterator wrapped to make it copyable.", false);
        (*tee_type()).tp_methods = mdefs![("__copy__", NOARGS, tee_copy, "Returns an independent iterator.")];
        fc::init_robj_type::<TeeData>(teedata_type(), c"itertools._tee_dataobject", 0);
        (*teedata_type()).tp_new = Some(teedata_new);
        (*teedata_type()).tp_doc = c"teedataobject(iterable, values, next, /)\n--\n\nData container common to multiple tee objects.".as_ptr();
        for t in all_types() {
            crate::types::typeobject::type_ready(t)?;
        }
    }
    Ok(())
}

fn all_types() -> [*mut PyTypeObject; 22] {
    [
        accumulate_type(),
        batched_type(),
        chain_type(),
        combinations_type(),
        cwr_type(),
        compress_type(),
        count_type(),
        cycle_type(),
        dropwhile_type(),
        takewhile_type(),
        filterfalse_type(),
        groupby_type(),
        grouper_type(),
        islice_type(),
        pairwise_type(),
        permutations_type(),
        product_type(),
        repeat_type(),
        starmap_type(),
        tee_type(),
        teedata_type(),
        ziplongest_type(),
    ]
}

pub fn init_module() -> PyResult {
    init_types()?;
    let methods = mdefs![(
        "tee",
        FAST,
        itertools_tee,
        "tee($module, iterable, n=2, /)\n--\n\nReturns a tuple of n independent iterators."
    )];
    let (m, d) = fc::new_module(
        "itertools",
        methods,
        Some("Functional tools for creating and using iterators.\n\nInfinite iterators:\ncount(start=0, step=1) --> start, start+step, start+2*step, ...\ncycle(p) --> p0, p1, ... plast, p0, p1, ...\nrepeat(elem [,n]) --> elem, elem, elem, ... endlessly or up to n times\n\nIterators terminating on the shortest input sequence:\naccumulate(p[, func]) --> p0, p0+p1, p0+p1+p2\nbatched(p, n) --> [p0, p1, ..., p_n-1], [p_n, p_n+1, ..., p_2n-1], ...\nchain(p, q, ...) --> p0, p1, ... plast, q0, q1, ...\nchain.from_iterable([p, q, ...]) --> p0, p1, ... plast, q0, q1, ...\ncompress(data, selectors) --> (d[0] if s[0]), (d[1] if s[1]), ...\ndropwhile(predicate, seq) --> seq[n], seq[n+1], starting when predicate fails\ngroupby(iterable[, keyfunc]) --> sub-iterators grouped by value of keyfunc(v)\nfilterfalse(predicate, seq) --> elements of seq where predicate(elem) is False\nislice(seq, [start,] stop [, step]) --> elements from\n       seq[start:stop:step]\npairwise(s) --> (s[0],s[1]), (s[1],s[2]), (s[2], s[3]), ...\nstarmap(fun, seq) --> fun(*seq[0]), fun(*seq[1]), ...\ntee(it, n=2) --> (it1, it2 , ... itn) splits one iterator into n\ntakewhile(predicate, seq) --> seq[0], seq[1], until predicate fails\nzip_longest(p, q, ...) --> (p[0], q[0]), (p[1], q[1]), ...\n\nCombinatoric generators:\nproduct(p, q, ... [repeat=1]) --> cartesian product\npermutations(p[, r])\ncombinations(p, r)\ncombinations_with_replacement(p, r)\n"),
    )?;
    let names = [
        "accumulate",
        "batched",
        "chain",
        "combinations",
        "combinations_with_replacement",
        "compress",
        "count",
        "cycle",
        "dropwhile",
        "takewhile",
        "filterfalse",
        "groupby",
        "_grouper",
        "islice",
        "pairwise",
        "permutations",
        "product",
        "repeat",
        "starmap",
        "_tee",
        "_tee_dataobject",
        "zip_longest",
    ];
    for (n, t) in names.iter().zip(all_types()) {
        fc::add_type(&d, n, t)?;
    }
    let _: Option<&RObj<()>> = None;
    Ok(m)
}
