//! (capi3) Self-tests of runtime internals, exposed by `_testinternalcapi` as `test_*`
//! functions (test_capi.test_misc collects every such attribute as a test case, and
//! `test_lock_*` ones as Test_PyLock cases). Each runs a check of our own primitive
//! (locks, critical sections, stop-the-world, once-initialisation, bit helpers, ...)
//! and raises AssertionError describing the first broken expectation.
//!
//! Also here: the debug-allocator probes `check_pyobject_*_is_freed` used by test_mem.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::none;
use std::ffi::c_void;

fn fail(what: &str) -> PyErr {
    crate::err!(AssertionError, "{}", what)
}

macro_rules! check {
    ($cond:expr, $($fmt:tt)*) => {
        if !$cond {
            return Err(fail(&format!($($fmt)*)));
        }
    };
}

// ---------------------------------------------------------------- debug allocator probes

fn expect_freed(op: *const PyObject) -> PyResult {
    check!(unsafe { crate::capi::memdebug::object_looks_freed(op) }, "object is not seen as freed");
    Ok(none::none_ref())
}

fn check_pyobject_null_is_freed(_m: &PyObject) -> PyResult {
    expect_freed(std::ptr::null())
}

/// A block from the object domain that was never initialised (the debug hooks fill it).
fn check_pyobject_uninitialized_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::capi::mem::PyObject_Malloc(std::mem::size_of::<PyObject>()) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        // the block is deliberately leaked: the process exits right after the probe
        expect_freed(op)
    }
}

/// A block too short to hold `ob_type`: the type slot lies in the trailing guard bytes.
fn check_pyobject_forbidden_bytes_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let n = std::mem::offset_of!(PyObject, ob_type);
        let op = crate::capi::mem::PyObject_Malloc(n) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        expect_freed(op)
    }
}

/// An object released the way an extension type with `tp_free = PyObject_Free` is.
fn check_pyobject_freed_is_freed(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::capi::mem::PyObject_Malloc(std::mem::size_of::<PyObject>()) as *mut PyObject;
        if op.is_null() {
            return Err(crate::errors::memory_error());
        }
        crate::capi::mem::PyObject_Init(op, crate::types::typeobject::object_type());
        crate::capi::mem::PyObject_Free(op as *mut c_void);
        expect_freed(op)
    }
}

// ---------------------------------------------------------------- helpers

use crate::abstract_ as ab;
use crate::capi::pythread::{self as pt, OnceFlag, RecursiveMutex, RwMutex};
use crate::types::{bytes, dict, int, str as pystr, typeobject::type_obj};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::*};
use std::time::{Duration, Instant};

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Wait for a native thread without holding up stop-the-world on this thread.
fn join<T>(h: std::thread::JoinHandle<T>) -> T {
    crate::threads::blocking(|| h.join().expect("self-test thread panicked"))
}

/// Run `f` on a new OS thread that has a Python thread state (PyGILState_Ensure).
fn spawn_py<F: FnOnce() + Send + 'static>(f: F) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || unsafe {
        let g = crate::capi::pystate::PyGILState_Ensure();
        f();
        crate::capi::pystate::PyGILState_Release(g);
    })
}

/// Poll `cond` (sleeping, without holding up stop-the-world) for at most `ms`.
fn wait_for(ms: u64, cond: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    crate::threads::blocking(|| {
        while !cond() {
            if Instant::now() > end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    })
}

#[derive(Clone, Copy)]
struct SendPtr(usize);
unsafe impl Send for SendPtr {}

fn int_obj(s: &str) -> PyResult {
    ab::call(type_obj(int::int_type()), &[&pystr::new_str(s)])
}

// ---------------------------------------------------------------- integer bit helpers

/// int.to_bytes / int.from_bytes in both byte orders against byte-reversed patterns.
fn test_bswap(_m: &PyObject) -> PyResult {
    for v in [0u64, 0x12, 0x1234, 0x12345678, 0x0102030405060708, 0xFEDCBA9876543210, u64::MAX] {
        let o = int_obj(&v.to_string())?;
        for (order, expect) in [("big", v.to_be_bytes()), ("little", v.to_le_bytes())] {
            let b = ab::call_method_str(&o, "to_bytes", &[&int::new_int(8), &pystr::new_str(order)])?;
            check!(bytes::bytes_data(&b) == expect, "{:#x}.to_bytes(8, {:?}) gave {:?}", v, order, bytes::bytes_data(&b));
            let back = ab::call_method_str(type_obj(int::int_type()), "from_bytes", &[&b, &pystr::new_str(order)])?;
            check!(ab::str_string(&back)? == v.to_string(), "int.from_bytes(.., {:?}) round trip of {:#x}", order, v);
        }
        // the other order reads the swapped value
        let b = ab::call_method_str(&o, "to_bytes", &[&int::new_int(8), &pystr::new_str("big")])?;
        let sw = ab::call_method_str(type_obj(int::int_type()), "from_bytes", &[&b, &pystr::new_str("little")])?;
        check!(ab::str_string(&sw)? == v.swap_bytes().to_string(), "byte swap of {:#x}", v);
    }
    Ok(none::none_ref())
}

/// int.bit_count on small, multi-digit and negative values.
fn test_popcount(_m: &PyObject) -> PyResult {
    let mut cases: Vec<(String, u32)> = vec![];
    for v in [0u128, 1, 2, 3, 0xff, 0x8000_0000, 0x5555_5555_5555_5555, u64::MAX as u128, 1 << 100, (1 << 127) | 0xf0f0] {
        cases.push((v.to_string(), v.count_ones()));
        cases.push((format!("-{}", v), v.count_ones()));
    }
    for (s, n) in cases {
        let r = ab::call_method_str(&*int_obj(&s)?, "bit_count", &[])?;
        check!(ab::as_i64(&r)? == n as i64, "({}).bit_count() == {}, expected {}", s, ab::as_i64(&r)?, n);
    }
    Ok(none::none_ref())
}

/// int.bit_length around digit and machine-word boundaries.
fn test_bit_length(_m: &PyObject) -> PyResult {
    for shift in [0u32, 1, 7, 8, 29, 30, 31, 32, 59, 60, 61, 63, 64, 65, 90, 127] {
        for delta in [-1i128, 0, 1] {
            let v = ((1u128 << shift) as i128 + delta).max(0) as u128;
            let expect = 128 - v.leading_zeros();
            for s in [v.to_string(), format!("-{}", v)] {
                let r = ab::call_method_str(&*int_obj(&s)?, "bit_length", &[])?;
                check!(ab::as_i64(&r)? == expect as i64, "({}).bit_length() == {}, expected {}", s, ab::as_i64(&r)?, expect);
            }
        }
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- hash tables

/// The runtime's hash tables: dict (open addressing with deletions and colliding
/// hashes) and the interned-string table (one canonical object per content).
fn test_hashtable(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    const N: i64 = 2000;
    for i in 0..N {
        dict::set_item(&d, &int::new_int(i * 7919), &int::new_int(i))?;
    }
    check!(dict::dict_len(&d) == N as usize, "dict holds {} entries, expected {}", dict::dict_len(&d), N);
    for i in (0..N).step_by(2) {
        dict::del_item(&d, &int::new_int(i * 7919))?;
    }
    for i in 0..N {
        let got = dict::get_item(&d, &int::new_int(i * 7919))?;
        check!(got.is_some() == (i % 2 == 1), "key {} present={} after deleting the even keys", i * 7919, got.is_some());
    }
    // keys that all hash alike (hash(-1) == hash(-2) == -2) plus ones that differ
    let c = dict::new_dict();
    let ks = [-1i64, -2, (1 << 61) - 3, 2 * ((1 << 61) - 1) - 3];
    for (n, &k) in ks.iter().enumerate() {
        dict::set_item(&c, &int::new_int(k), &int::new_int(n as i64))?;
    }
    for (n, &k) in ks.iter().enumerate() {
        let v = dict::get_item(&c, &int::new_int(k))?;
        check!(v.as_deref().map(|x| ab::as_i64(x)) .transpose()? == Some(n as i64), "colliding key {} lost its value", k);
    }
    // interning: equal content -> the same object, different content -> different
    let base = format!("capi3-selftest-intern-{}", std::process::id());
    let a = pystr::intern(pystr::new_str(&base));
    let b = pystr::intern(pystr::new_str(&base));
    check!(a.is(&b), "two interned copies of {:?} are different objects", base);
    let c2 = pystr::intern(pystr::new_str(&(base.clone() + "x")));
    check!(!a.is(&c2), "different strings interned to one object");
    check!(pystr::lookup_interned(&pystr::new_str(&base)) == Some(a.as_ptr()), "lookup of an interned string failed");
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- suggestions, bytes.find

/// The cost model of "Did you mean" suggestions: equal characters cost 0, characters
/// differing only in case 1, any other substitution, insertion or deletion 2.
fn test_edit_cost(_m: &PyObject) -> PyResult {
    use crate::modules::suggestions::levenshtein;
    let cases: &[(&str, &str, usize)] = &[
        ("", "", 0),
        ("", "ab", 4),
        ("xyz", "", 6),
        ("q", "Q", 1),
        ("Hello", "hello", 1),
        ("abc", "abd", 2),
        ("ab", "ba", 4),
        ("kitten", "sitting", 6),
        ("Foo", "foo_", 3),
        ("SPAM", "spam", 4),
        ("name", "nmae", 4),
        ("ValueError", "ValueErorr", 4),
        ("getattr", "getAttr", 1),
        ("tuple", "tupel", 4),
    ];
    for &(a, b, n) in cases {
        let d = levenshtein(a, b);
        check!(d == n, "edit cost {:?} -> {:?} is {}, expected {}", a, b, d, n);
        check!(levenshtein(b, a) == n, "edit cost is not symmetric for {:?} / {:?}", a, b);
    }
    Ok(none::none_ref())
}

/// bytes.find / rfind / count against a naive search over generated data.
fn test_bytes_find(_m: &PyObject) -> PyResult {
    let naive_find = |h: &[u8], n: &[u8], start: usize| -> i64 {
        if n.len() > h.len() {
            return -1;
        }
        (start..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n).map_or(-1, |i| i as i64)
    };
    let naive_rfind = |h: &[u8], n: &[u8]| -> i64 {
        if n.len() > h.len() {
            return -1;
        }
        (0..=h.len() - n.len()).rev().find(|&i| &h[i..i + n.len()] == n).map_or(-1, |i| i as i64)
    };
    let naive_count = |h: &[u8], n: &[u8]| -> i64 {
        if n.is_empty() {
            return h.len() as i64 + 1;
        }
        let (mut i, mut c) = (0, 0);
        while i + n.len() <= h.len() {
            if &h[i..i + n.len()] == n {
                c += 1;
                i += n.len();
            } else {
                i += 1;
            }
        }
        c
    };
    // haystacks: periodic, random-ish, long runs; needles cut from them and absent ones
    let mut seed: u32 = 12345;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut hays: Vec<Vec<u8>> = vec![b"".to_vec(), b"a".to_vec(), b"abababababc".to_vec(), vec![b'x'; 3000]];
    hays.push((0..5000).map(|_| b"acgt"[(rnd() % 4) as usize]).collect());
    let mut tail = vec![b'a'; 4000];
    tail.extend_from_slice(b"ab");
    hays.push(tail);
    for h in &hays {
        let hb = bytes::new_bytes(h);
        let mut needles: Vec<Vec<u8>> = vec![b"".to_vec(), b"zz".to_vec(), b"ab".to_vec(), b"aab".to_vec(), vec![b'x'; 100]];
        for len in [1usize, 2, 3, 5, 8, 17, 40, 300] {
            if h.len() >= len {
                let s = (rnd() as usize) % (h.len() - len + 1);
                needles.push(h[s..s + len].to_vec());
            }
        }
        for n in &needles {
            let nb = bytes::new_bytes(n);
            for start in [0usize, 1, h.len() / 2] {
                let r = ab::as_i64(&*ab::call_method_str(&hb, "find", &[&nb, &int::new_int(start as i64)])?)?;
                let e = if start > h.len() { -1 } else { naive_find(h, n, start) };
                check!(r == e, "find of a {}-byte needle at {} in {} bytes: {} != {}", n.len(), start, h.len(), r, e);
            }
            let r = ab::as_i64(&*ab::call_method_str(&hb, "rfind", &[&nb])?)?;
            check!(r == naive_rfind(h, n), "rfind of a {}-byte needle in {} bytes: {}", n.len(), h.len(), r);
            let r = ab::as_i64(&*ab::call_method_str(&hb, "count", &[&nb])?)?;
            check!(r == naive_count(h, n), "count of a {}-byte needle in {} bytes: {}", n.len(), h.len(), r);
        }
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- thread states

fn test_tstate_capi(_m: &PyObject) -> PyResult {
    unsafe {
        use crate::capi::pystate as ps;
        let ts = ps::PyThreadState_Get();
        check!(!ts.is_null(), "PyThreadState_Get() returned NULL");
        check!(ps::PyThreadState_GetUnchecked() == ts, "PyThreadState_GetUnchecked() disagrees with PyThreadState_Get()");
        let d = ps::PyThreadState_GetDict();
        check!(!d.is_null() && dict::is_dict(&*d), "PyThreadState_GetDict() is not a dict");
        check!(ps::PyThreadState_GetDict() == d, "PyThreadState_GetDict() is not stable");
        check!(ps::PyThreadState_GetInterpreter(ts) == ps::PyInterpreterState_Get(), "thread state of another interpreter");
        // detached: no current thread state
        let saved = ps::PyEval_SaveThread();
        let none_now = ps::PyThreadState_GetUnchecked().is_null();
        ps::PyEval_RestoreThread(saved);
        check!(none_now, "a thread state is current inside Py_BEGIN_ALLOW_THREADS");
        check!(ps::PyThreadState_Get() == ts, "a different thread state after Py_END_ALLOW_THREADS");
    }
    Ok(none::none_ref())
}

fn test_threadstate_set_stack_protection(_m: &PyObject) -> PyResult {
    use crate::capi::pystate as ps;
    let ts = crate::threads::current_mut();
    let t = unsafe { &*ts };
    let (low0, margin0) = (t.stack_low.get(), t.stack_margin.get());
    let x = 0u8;
    let sp = &x as *const u8 as usize;
    for size in [ps::MIN_STACK_SIZE, 7_654_321] {
        let start = sp - size;
        let r = unsafe { ps::PyUnstable_ThreadState_SetStackProtection(ts as *mut c_void, start as *mut c_void, size) };
        let (soft, hard) = (t.stack_soft_limit(), t.stack_hard_limit());
        unsafe { ps::PyUnstable_ThreadState_ResetStackProtection(ts as *mut c_void) };
        check!(r == 0 && !err_occurred(), "SetStackProtection(size={}) failed", size);
        check!(start <= hard && hard <= soft && soft < start + size, "limits {:#x}/{:#x} outside the stack [{:#x}, {:#x})", hard, soft, start, start + size);
    }
    let r = unsafe { ps::PyUnstable_ThreadState_SetStackProtection(ts as *mut c_void, (sp - 5) as *mut c_void, 5) };
    check!(r == -1 && exc_matches(crate::types::exceptions::ValueError()), "a 5-byte stack was accepted");
    clear_exc();
    unsafe { ps::PyUnstable_ThreadState_ResetStackProtection(ts as *mut c_void) };
    check!(t.stack_low.get() == low0 && t.stack_margin.get() == margin0, "ResetStackProtection did not restore the thread's stack bounds");
    Ok(none::none_ref())
}

const NUM_GUARDS: usize = 100;

fn test_interp_guard_countdown(_m: &PyObject) -> PyResult {
    use crate::capi::pystate as ps;
    unsafe {
        let interp = ps::PyInterpreterState_Get();
        let base = ps::guard_count(interp);
        let mut guards = Vec::new();
        for i in 0..NUM_GUARDS {
            let g = ps::PyInterpreterGuard_FromCurrent();
            check!(!g.is_null(), "PyInterpreterGuard_FromCurrent() failed");
            check!(ps::guard_count(interp) == base + i + 1, "guard count {} after {} guards", ps::guard_count(interp), i + 1);
            guards.push(g);
        }
        for (i, g) in guards.into_iter().enumerate() {
            ps::PyInterpreterGuard_Close(g);
            check!(ps::guard_count(interp) == base + NUM_GUARDS - i - 1, "guard count {} after closing {}", ps::guard_count(interp), i + 1);
        }
    }
    Ok(none::none_ref())
}

fn test_interp_view_countdown(_m: &PyObject) -> PyResult {
    use crate::capi::pystate as ps;
    unsafe {
        let interp = ps::PyInterpreterState_Get();
        let view = ps::PyInterpreterView_FromCurrent();
        check!(!view.is_null(), "PyInterpreterView_FromCurrent() failed");
        let base = ps::guard_count(interp);
        let mut guards = Vec::new();
        for i in 0..NUM_GUARDS {
            let g = ps::PyInterpreterGuard_FromView(view);
            check!(!g.is_null(), "PyInterpreterGuard_FromView() failed");
            check!(ps::guard_interpreter(g) == interp, "guard of another interpreter");
            check!(ps::guard_count(interp) == base + i + 1, "guard count {} after {} guards", ps::guard_count(interp), i + 1);
            guards.push(g);
        }
        for (i, g) in guards.into_iter().enumerate() {
            ps::PyInterpreterGuard_Close(g);
            check!(ps::guard_count(interp) == base + NUM_GUARDS - i - 1, "guard count after closing {}", i + 1);
        }
        ps::PyInterpreterView_Close(view);
    }
    Ok(none::none_ref())
}

fn test_thread_state_ensure_from_view_interp_switch(_m: &PyObject) -> PyResult {
    use crate::capi::pystate as ps;
    unsafe {
        let interp = ps::PyInterpreterState_Get();
        let view = ps::PyInterpreterView_FromCurrent();
        let base = ps::guard_count(interp);
        // on the attached thread state
        let t1 = ps::PyThreadState_EnsureFromView(view);
        check!(!t1.is_null() && ps::guard_count(interp) == base + 1, "EnsureFromView did not hold a guard");
        ps::PyThreadState_Release(t1);
        check!(ps::guard_count(interp) == base, "Release did not close the guard");
        check!(!ps::PyThreadState_GetUnchecked().is_null(), "Release detached the caller's thread state");
        // from a detached thread
        let saved = ps::PyThreadState_Swap(std::ptr::null_mut());
        let t2 = ps::PyThreadState_EnsureFromView(view);
        let held = ps::guard_count(interp);
        ps::PyThreadState_Release(t2);
        let after = ps::guard_count(interp);
        let detached = ps::PyThreadState_GetUnchecked().is_null();
        ps::PyThreadState_Swap(saved);
        check!(!t2.is_null() && held == base + 1, "EnsureFromView from a detached thread: {} guards", held);
        check!(after == base, "guard count {} after the second Release", after);
        check!(detached, "Release did not restore the detached state");
        // guards still work afterwards
        let g = ps::PyInterpreterGuard_FromCurrent();
        check!(!g.is_null() && ps::guard_count(interp) == base + 1, "guards stopped working");
        ps::PyInterpreterGuard_Close(g);
        check!(ps::guard_count(interp) == base, "guard count after the last close");
        ps::PyInterpreterView_Close(view);
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- PyMutex and friends

fn mutex_bits(m: &AtomicU8) -> u8 {
    m.load(Relaxed)
}
fn mlock(m: &AtomicU8) {
    unsafe { pt::PyMutex_Lock(m.as_ptr()) }
}
fn munlock(m: &AtomicU8) {
    unsafe { pt::PyMutex_Unlock(m.as_ptr()) }
}

fn test_lock_basic(_m: &PyObject) -> PyResult {
    let m = AtomicU8::new(0);
    mlock(&m);
    check!(mutex_bits(&m) == 1, "locked mutex bits {}", mutex_bits(&m));
    check!(unsafe { pt::PyMutex_IsLocked(m.as_ptr()) } == 1, "PyMutex_IsLocked() false while locked");
    munlock(&m);
    check!(mutex_bits(&m) == 0, "unlocked mutex bits {}", mutex_bits(&m));
    Ok(none::none_ref())
}

fn test_lock_two_threads(_m: &PyObject) -> PyResult {
    struct D {
        m: AtomicU8,
        started: AtomicBool,
        bits_seen: AtomicU8,
    }
    let d = Arc::new(D { m: AtomicU8::new(0), started: AtomicBool::new(false), bits_seen: AtomicU8::new(0) });
    mlock(&d.m);
    let d2 = d.clone();
    let h = std::thread::spawn(move || {
        d2.started.store(true, SeqCst);
        mlock(&d2.m);
        d2.bits_seen.store(mutex_bits(&d2.m), SeqCst);
        munlock(&d2.m);
    });
    // the waiter announces itself in the "has parked" bit
    let parked = wait_for(2000, || mutex_bits(&d.m) == 3);
    let bits = mutex_bits(&d.m);
    munlock(&d.m);
    join(h);
    check!(d.started.load(SeqCst), "the second thread did not start");
    check!(parked, "a waiting thread did not set the has-parked bit (bits {})", bits);
    let seen = d.bits_seen.load(SeqCst);
    check!(seen == 1 || seen == 3, "the second thread saw bits {} while holding the lock", seen);
    check!(mutex_bits(&d.m) == 0, "bits {} after both threads unlocked", mutex_bits(&d.m));
    Ok(none::none_ref())
}

fn run_counter(threads: usize, iters: usize, slow: bool) -> PyResult {
    struct D {
        m: AtomicU8,
        counter: AtomicUsize,
    }
    let d = Arc::new(D { m: AtomicU8::new(0), counter: AtomicUsize::new(0) });
    let hs: Vec<_> = (0..threads)
        .map(|_| {
            let d = d.clone();
            std::thread::spawn(move || {
                for i in 0..iters {
                    mlock(&d.m);
                    if slow && i % 7 == 0 {
                        sleep_ms(2);
                    }
                    // a non-atomic read-modify-write: only the mutex keeps it exact
                    let v = d.counter.load(Relaxed);
                    d.counter.store(v + 1, Relaxed);
                    munlock(&d.m);
                }
            })
        })
        .collect();
    for h in hs {
        join(h);
    }
    let got = d.counter.load(SeqCst);
    check!(got == threads * iters, "counter {} after {} increments", got, threads * iters);
    Ok(none::none_ref())
}
fn test_lock_counter(_m: &PyObject) -> PyResult {
    run_counter(5, 10_000, false)
}
fn test_lock_counter_slow(_m: &PyObject) -> PyResult {
    run_counter(5, 100, true)
}

/// Lock throughput: `num_threads` threads take one of `num_locks` mutexes
/// `num_acquisitions` times per iteration, doing `work_inside` / `work_outside` units of
/// work with and without it, for `time_ms` (or until `total_iters` per thread).
/// Returns (acquisitions per second, per-thread acquisitions, elapsed ns).
fn bench_locks(
    num_threads: usize,
    work_inside: usize,
    work_outside: usize,
    time_ms: u64,
    num_acquisitions: usize,
    total_iters: usize,
    num_locks: usize,
    random_locks: bool,
) -> (f64, Vec<usize>, u128) {
    struct L {
        m: AtomicU8,
        value: AtomicUsize,
    }
    let locks: Arc<Vec<L>> = Arc::new((0..num_locks.max(1)).map(|_| L { m: AtomicU8::new(0), value: AtomicUsize::new(0) }).collect());
    let stop = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();
    let hs: Vec<_> = (0..num_threads)
        .map(|t| {
            let (locks, stop) = (locks.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut rng = (t as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let mut iters = 0usize;
                let mut local = 0usize;
                loop {
                    if (total_iters > 0 && iters >= total_iters) || (total_iters == 0 && stop.load(Relaxed)) {
                        break;
                    }
                    let idx = if random_locks {
                        rng ^= rng << 7;
                        rng ^= rng >> 9;
                        (rng as usize) % locks.len()
                    } else {
                        t % locks.len()
                    };
                    let l = &locks[idx];
                    for _ in 0..num_acquisitions.max(1) {
                        mlock(&l.m);
                        for _ in 0..work_inside {
                            let v = l.value.load(Relaxed);
                            l.value.store(v.wrapping_add(1), Relaxed);
                        }
                        munlock(&l.m);
                    }
                    for _ in 0..work_outside {
                        local = std::hint::black_box(local.wrapping_add(1));
                    }
                    iters += num_acquisitions.max(1);
                }
                iters
            })
        })
        .collect();
    if total_iters == 0 {
        crate::threads::blocking(|| sleep_ms(time_ms));
        stop.store(true, SeqCst);
    }
    let per: Vec<usize> = hs.into_iter().map(join).collect();
    let ns = t0.elapsed().as_nanos().max(1);
    let rate = per.iter().sum::<usize>() as f64 * 1e9 / ns as f64;
    (rate, per, ns)
}

fn benchmark_locks(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("benchmark_locks", args, 1, 8)?;
    let get = |i: usize, d: i64| -> PyResult<i64> { if i < args.len() { ab::as_i64(args[i]) } else { Ok(d) } };
    let n = get(0, 1)?.max(1) as usize;
    let random = if args.len() > 7 { ab::is_true(args[7])? } else { false };
    let (rate, per, ns) = bench_locks(
        n,
        get(1, 1)?.max(0) as usize,
        get(2, 0)?.max(0) as usize,
        get(3, 1000)?.max(0) as u64,
        get(4, 1)?.max(1) as usize,
        get(5, 0)?.max(0) as usize,
        get(6, 1)?.max(1) as usize,
        random,
    );
    let l = crate::types::list::new_list(per.into_iter().map(|x| int::new_int(x as i64)).collect());
    Ok(crate::types::tuple::new_tuple(vec![crate::types::float::new_float(rate), l, int::new_int(ns as i64)]))
}

fn test_lock_benchmark(_m: &PyObject) -> PyResult {
    let (rate, per, ns) = bench_locks(1, 1, 0, 100, 1, 0, 1, false);
    check!(rate > 0.0 && per.len() == 1 && per[0] > 0 && ns > 0, "lock benchmark made no progress");
    let (_, per, _) = bench_locks(3, 2, 1, 0, 2, 1000, 2, true);
    check!(per.iter().all(|&x| x == 1000), "fixed-iteration benchmark gave {:?}", per);
    Ok(none::none_ref())
}

fn test_lock_once(_m: &PyObject) -> PyResult {
    let once = OnceFlag::new();
    let mut calls = 0;
    for i in 0..10 {
        let r = once.call_once(|| {
            calls += 1;
            if calls < 5 { Err(()) } else { Ok(()) }
        });
        if i < 4 {
            check!(r.is_err() && !once.is_done(), "attempt {} should have failed", i + 1);
        } else {
            check!(r.is_ok() && once.is_done() && calls == 5, "attempt {}: ok={} calls={}", i + 1, r.is_ok(), calls);
        }
    }
    // racing threads: the initialiser runs exactly once, everyone sees it finished
    let once = Arc::new(OnceFlag::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let done_seen = Arc::new(AtomicUsize::new(0));
    let hs: Vec<_> = (0..6)
        .map(|_| {
            let (once, runs, done_seen) = (once.clone(), runs.clone(), done_seen.clone());
            std::thread::spawn(move || {
                let _ = once.call_once(|| -> Result<(), ()> {
                    sleep_ms(20);
                    runs.fetch_add(1, SeqCst);
                    Ok(())
                });
                if runs.load(SeqCst) == 1 {
                    done_seen.fetch_add(1, SeqCst);
                }
            })
        })
        .collect();
    for h in hs {
        join(h);
    }
    check!(runs.load(SeqCst) == 1, "initialiser ran {} times", runs.load(SeqCst));
    check!(done_seen.load(SeqCst) == 6, "{} threads returned before initialisation finished", 6 - done_seen.load(SeqCst));
    Ok(none::none_ref())
}

fn test_lock_rwlock(_m: &PyObject) -> PyResult {
    let rw = Arc::new(RwMutex::new());
    let order = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    // two readers share the lock
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (rw, release) = (rw.clone(), release.clone());
            std::thread::spawn(move || {
                rw.read_lock();
                while !release.load(SeqCst) {
                    sleep_ms(1);
                }
                rw.read_unlock();
            })
        })
        .collect();
    check!(wait_for(2000, || rw.state().0 == 2), "two readers could not hold the lock together ({:?})", rw.state());
    // a writer waits for them
    let writer_at = Arc::new(AtomicUsize::new(0));
    let w = {
        let (rw, order, writer_at) = (rw.clone(), order.clone(), writer_at.clone());
        std::thread::spawn(move || {
            rw.write_lock();
            writer_at.store(order.fetch_add(1, SeqCst) + 1, SeqCst);
            sleep_ms(10);
            rw.write_unlock();
        })
    };
    crate::threads::blocking(|| sleep_ms(30));
    check!(writer_at.load(SeqCst) == 0, "the writer got in while readers held the lock");
    // a new reader queues behind the waiting writer
    let reader_at = Arc::new(AtomicUsize::new(0));
    let r3 = {
        let (rw, order, reader_at) = (rw.clone(), order.clone(), reader_at.clone());
        std::thread::spawn(move || {
            rw.read_lock();
            reader_at.store(order.fetch_add(1, SeqCst) + 1, SeqCst);
            rw.read_unlock();
        })
    };
    crate::threads::blocking(|| sleep_ms(30));
    check!(reader_at.load(SeqCst) == 0, "a new reader overtook the waiting writer");
    release.store(true, SeqCst);
    for h in readers {
        join(h);
    }
    join(w);
    join(r3);
    check!(writer_at.load(SeqCst) == 1 && reader_at.load(SeqCst) == 2, "order writer={} reader={}", writer_at.load(SeqCst), reader_at.load(SeqCst));
    check!(rw.state() == (0, false), "lock still held at the end: {:?}", rw.state());
    Ok(none::none_ref())
}

fn test_lock_recursive(_m: &PyObject) -> PyResult {
    let m = Arc::new(RecursiveMutex::new());
    for _ in 0..3 {
        m.lock();
    }
    check!(m.is_locked() && m.is_owned_by_me(), "recursive mutex not held after three locks");
    let m2 = m.clone();
    let foreign_unlock = std::thread::spawn(move || m2.unlock());
    check!(!join(foreign_unlock), "another thread could release the owner's lock");
    // another thread waits until the owner released every level
    let got = Arc::new(AtomicBool::new(false));
    let h = {
        let (m, got) = (m.clone(), got.clone());
        std::thread::spawn(move || {
            m.lock();
            got.store(true, SeqCst);
            m.unlock();
        })
    };
    for level in (1..=3).rev() {
        crate::threads::blocking(|| sleep_ms(10));
        check!(!got.load(SeqCst), "another thread got the lock with {} levels still held", level);
        check!(m.unlock(), "unlock by the owner failed");
    }
    join(h);
    check!(got.load(SeqCst) && !m.is_locked(), "lock not handed over / not released");
    check!(!m.unlock(), "unlocking a free recursive mutex succeeded");
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- critical sections

fn ob_locked(o: &PyObject) -> bool {
    unsafe { pt::PyMutex_IsLocked((&raw const (*o.as_ptr()).ob_mutex) as *mut u8) != 0 }
}
fn cs_head() -> usize {
    unsafe { (*crate::threads::current_ptr()).c.critical_section }
}
fn zero_cs() -> pt::PyCriticalSection {
    pt::PyCriticalSection { _cs_prev: 0, _cs_mutex: std::ptr::null_mut() }
}
fn zero_cs2() -> pt::PyCriticalSection2 {
    pt::PyCriticalSection2 { _cs_base: zero_cs(), _cs_mutex2: std::ptr::null_mut() }
}
fn with_cs<R>(o: &PyObject, f: impl FnOnce() -> R) -> R {
    let mut c = zero_cs();
    unsafe { pt::PyCriticalSection_Begin(&mut c, o.as_ptr()) };
    let r = f();
    unsafe { pt::PyCriticalSection_End(&mut c) };
    r
}
fn with_cs2<R>(a: &PyObject, b: &PyObject, f: impl FnOnce() -> R) -> R {
    let mut c = zero_cs2();
    unsafe { pt::PyCriticalSection2_Begin(&mut c, a.as_ptr(), b.as_ptr()) };
    let r = f();
    unsafe { pt::PyCriticalSection2_End(&mut c) };
    r
}
fn nest1(o: &PyObject, depth: usize) {
    with_cs(o, || {
        if depth > 0 {
            nest1(o, depth - 1)
        }
    })
}
fn nest2(a: &PyObject, b: &PyObject, depth: usize) {
    with_cs2(a, b, || {
        if depth > 0 {
            nest2(a, b, depth - 1)
        }
    })
}

fn test_critical_sections(_m: &PyObject) -> PyResult {
    let (d1, d2) = (dict::new_dict(), dict::new_dict());
    let (locked, active) = with_cs(&d1, || (ob_locked(&d1), cs_head() != 0));
    check!(locked && active, "a critical section did not lock its object (locked={} active={})", locked, active);
    check!(!ob_locked(&d1) && cs_head() == 0, "the object stayed locked after the section");
    let both = with_cs2(&d1, &d2, || ob_locked(&d1) && ob_locked(&d2));
    check!(both && !ob_locked(&d1) && !ob_locked(&d2), "a two-object section did not lock / unlock both");
    let same = with_cs2(&d2, &d2, || ob_locked(&d2));
    check!(same && !ob_locked(&d2), "a section on the same object twice misbehaved");
    Ok(none::none_ref())
}

fn test_critical_sections_nest(_m: &PyObject) -> PyResult {
    let (a, b) = (dict::new_dict(), dict::new_dict());
    let r = with_cs(&a, || {
        nest1(&a, 10);
        ob_locked(&a)
    });
    check!(r && !ob_locked(&a), "re-entering a section on a held object released it early");
    let r = with_cs2(&b, &a, || {
        nest2(&a, &b, 10);
        ob_locked(&a) && ob_locked(&b)
    });
    check!(r && !ob_locked(&a) && !ob_locked(&b), "nested two-object sections released locks early");
    Ok(none::none_ref())
}

fn test_critical_sections_suspend(_m: &PyObject) -> PyResult {
    use crate::capi::pystate as ps;
    let a = dict::new_dict();
    let (inside, detached, back) = with_cs(&a, || unsafe {
        let inside = ob_locked(&a);
        let ts = ps::PyEval_SaveThread();
        let detached = ob_locked(&a);
        ps::PyEval_RestoreThread(ts);
        (inside, detached, ob_locked(&a))
    });
    check!(inside, "the section did not lock the object");
    check!(!detached, "the section was not suspended while the thread state was detached");
    check!(back, "the section was not resumed when the thread state was re-attached");
    check!(!ob_locked(&a), "the object stayed locked");
    Ok(none::none_ref())
}

fn test_critical_section1_reacquisition(_m: &PyObject) -> PyResult {
    let a = dict::new_dict();
    let (mut cs1, mut cs2) = (zero_cs(), zero_cs());
    unsafe {
        pt::PyCriticalSection_Begin(&mut cs1, a.as_ptr());
        let first = ob_locked(&a) && cs_head() == &raw mut cs1 as usize;
        pt::PyCriticalSection_Begin(&mut cs2, a.as_ptr());
        let again = ob_locked(&a) && cs_head() == &raw mut cs1 as usize;
        pt::PyCriticalSection_End(&mut cs2);
        let after_inner = ob_locked(&a) && cs_head() == &raw mut cs1 as usize;
        pt::PyCriticalSection_End(&mut cs1);
        check!(first, "first section is not the active one");
        check!(again, "re-acquiring a held object pushed a new section");
        check!(after_inner, "ending the no-op section changed the active section");
        check!(!ob_locked(&a) && cs_head() == 0, "the object stayed locked");
    }
    Ok(none::none_ref())
}

fn test_critical_section2_reacquisition(_m: &PyObject) -> PyResult {
    let (a, b) = (dict::new_dict(), dict::new_dict());
    let mut cs = zero_cs2();
    let mask = 3usize; // record-kind and suspended tags on chain links
    unsafe {
        pt::PyCriticalSection2_Begin(&mut cs, a.as_ptr(), b.as_ptr());
        let me = &raw mut cs as usize;
        let ok0 = ob_locked(&a) && ob_locked(&b) && cs_head() & !mask == me;
        let mut results = vec![];
        for o in [&a, &b] {
            let mut c1 = zero_cs();
            pt::PyCriticalSection_Begin(&mut c1, o.as_ptr());
            let inside = ob_locked(&a) && ob_locked(&b) && cs_head() & !mask == me;
            pt::PyCriticalSection_End(&mut c1);
            let after = ob_locked(&a) && ob_locked(&b) && cs_head() & !mask == me;
            results.push((inside, after));
        }
        pt::PyCriticalSection2_End(&mut cs);
        check!(ok0, "the two-object section is not active with both objects locked");
        for (i, (inside, after)) in results.into_iter().enumerate() {
            check!(inside, "re-acquiring object {} pushed a section or dropped a lock", i + 1);
            check!(after, "ending the no-op section on object {} changed the state", i + 1);
        }
        check!(!ob_locked(&a) && !ob_locked(&b) && cs_head() == 0, "objects stayed locked");
    }
    Ok(none::none_ref())
}

fn test_critical_sections_threads(_m: &PyObject) -> PyResult {
    let objs: Vec<PyRef> = (0..3).map(|_| dict::new_dict()).collect();
    let ptrs: Vec<SendPtr> = objs.iter().map(|o| SendPtr(o.as_ptr() as usize)).collect();
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let p = ptrs.clone();
            spawn_py(move || {
                let o = |i: usize| unsafe { &*(p[i].0 as *const PyObject) };
                for _ in 0..200 {
                    with_cs(o(0), || {});
                    with_cs(o(1), || nest1(o(0), 1));
                    with_cs2(o(2), o(0), || nest1(o(1), 2));
                    with_cs(o(2), || unsafe {
                        let ts = crate::capi::pystate::PyEval_SaveThread();
                        crate::capi::pystate::PyEval_RestoreThread(ts);
                    });
                }
            })
        })
        .collect();
    for h in hs {
        join(h);
    }
    check!(objs.iter().all(|o| !ob_locked(o)), "an object stayed locked after the threads finished");
    Ok(none::none_ref())
}

fn test_critical_sections_gc(_m: &PyObject) -> PyResult {
    // a thread collecting garbage inside a contended section must not deadlock with
    // the threads waiting for that section
    let obj = dict::new_dict();
    let p = SendPtr(obj.as_ptr() as usize);
    let ready = Arc::new(AtomicUsize::new(0));
    let hs: Vec<_> = (0..3)
        .map(|id| {
            let ready = ready.clone();
            spawn_py(move || {
                let o = unsafe { &*(p.0 as *const PyObject) };
                ready.fetch_add(1, SeqCst);
                crate::threads::blocking(|| {
                    while ready.load(SeqCst) < 3 {
                        sleep_ms(1)
                    }
                });
                match id {
                    0 => with_cs(o, || {
                        sleep_ms(5);
                        unsafe { crate::capi::mem::PyGC_Collect() };
                    }),
                    1 => {
                        sleep_ms(1);
                        with_cs(o, || sleep_ms(1));
                    }
                    _ => {
                        sleep_ms(6);
                        with_cs(o, || sleep_ms(1));
                    }
                }
            })
        })
        .collect();
    for h in hs {
        join(h);
    }
    check!(!ob_locked(&obj), "the object stayed locked");
    Ok(none::none_ref())
}

fn test_critical_sections_stw(_m: &PyObject) -> PyResult {
    // threads hand a section to each other while this thread stops the world and then
    // takes the same section
    let obj = dict::new_dict();
    let p = SendPtr(obj.as_ptr() as usize);
    let started = Arc::new(AtomicUsize::new(0));
    let hs: Vec<_> = (0..2)
        .map(|_| {
            let started = started.clone();
            spawn_py(move || {
                let o = unsafe { &*(p.0 as *const PyObject) };
                started.fetch_add(1, SeqCst);
                with_cs(o, || sleep_ms(10));
            })
        })
        .collect();
    check!(wait_for(5000, || started.load(SeqCst) == 2), "threads did not start");
    crate::threads::blocking(|| sleep_ms(5));
    let ok = crate::gc::with_world_stopped(|| with_cs(&obj, || ob_locked(&obj)));
    for h in hs {
        join(h);
    }
    check!(ok, "the section taken with the world stopped did not lock the object");
    check!(!ob_locked(&obj), "the object stayed locked");
    Ok(none::none_ref())
}

/// Take a new reference to `o` and give it back through the deferred-release queue
/// (QSBR), which the collector drains with the world stopped.
fn incref_decref_delayed(_m: &PyObject, o: &PyObject) -> PyResult {
    unsafe { crate::qsbr::decref_deferred(o.to_ref().into_raw()) };
    Ok(none::none_ref())
}

pub fn methods() -> *mut PyMethodDef {
    crate::mdefs![
        ("incref_decref_delayed", O, incref_decref_delayed),
        ("benchmark_locks", FAST, benchmark_locks),
        ("test_bswap", NOARGS, test_bswap),
        ("test_popcount", NOARGS, test_popcount),
        ("test_bit_length", NOARGS, test_bit_length),
        ("test_hashtable", NOARGS, test_hashtable),
        ("test_edit_cost", NOARGS, test_edit_cost),
        ("test_bytes_find", NOARGS, test_bytes_find),
        ("test_tstate_capi", NOARGS, test_tstate_capi),
        ("test_threadstate_set_stack_protection", NOARGS, test_threadstate_set_stack_protection),
        ("test_interp_guard_countdown", NOARGS, test_interp_guard_countdown),
        ("test_interp_view_countdown", NOARGS, test_interp_view_countdown),
        ("test_thread_state_ensure_from_view_interp_switch", NOARGS, test_thread_state_ensure_from_view_interp_switch),
        ("test_lock_basic", NOARGS, test_lock_basic),
        ("test_lock_two_threads", NOARGS, test_lock_two_threads),
        ("test_lock_counter", NOARGS, test_lock_counter),
        ("test_lock_counter_slow", NOARGS, test_lock_counter_slow),
        ("test_lock_benchmark", NOARGS, test_lock_benchmark),
        ("test_lock_once", NOARGS, test_lock_once),
        ("test_lock_rwlock", NOARGS, test_lock_rwlock),
        ("test_lock_recursive", NOARGS, test_lock_recursive),
        ("test_critical_sections", NOARGS, test_critical_sections),
        ("test_critical_sections_nest", NOARGS, test_critical_sections_nest),
        ("test_critical_sections_suspend", NOARGS, test_critical_sections_suspend),
        ("test_critical_section1_reacquisition", NOARGS, test_critical_section1_reacquisition),
        ("test_critical_section2_reacquisition", NOARGS, test_critical_section2_reacquisition),
        ("test_critical_sections_threads", NOARGS, test_critical_sections_threads),
        ("test_critical_sections_gc", NOARGS, test_critical_sections_gc),
        ("test_critical_sections_stw", NOARGS, test_critical_sections_stw),
        ("check_pyobject_null_is_freed", NOARGS, check_pyobject_null_is_freed),
        ("check_pyobject_uninitialized_is_freed", NOARGS, check_pyobject_uninitialized_is_freed),
        ("check_pyobject_forbidden_bytes_is_freed", NOARGS, check_pyobject_forbidden_bytes_is_freed),
        ("check_pyobject_freed_is_freed", NOARGS, check_pyobject_freed_is_freed),
    ]
}
