//! `_random`: the Mersenne Twister core behind Lib/random.py.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::int_methods::{from_big, iref};
use crate::types::typeobject::*;
use crate::types::{dict, float, int, none, str as pystr, tuple};
use std::ffi::c_int;

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908b0df;
const UPPER_MASK: u32 = 0x80000000;
const LOWER_MASK: u32 = 0x7fffffff;

pub struct Mt {
    state: [u32; N],
    index: usize,
}

impl Mt {
    pub fn new() -> Mt {
        let mut m = Mt { state: [0; N], index: N + 1 };
        m.init_genrand(19650218);
        m
    }
    fn init_genrand(&mut self, s: u32) {
        let mt = &mut self.state;
        mt[0] = s;
        for i in 1..N {
            mt[i] = 1812433253u32.wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_add(i as u32);
        }
        self.index = N;
    }
    pub fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19650218);
        let mt = &mut self.state;
        let mut i = 1usize;
        let mut j = 0usize;
        let klen = key.len();
        let mut k = N.max(klen);
        while k > 0 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
            if j >= klen {
                j = 0;
            }
            k -= 1;
        }
        k = N - 1;
        while k > 0 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1566083941)).wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        mt[0] = 0x80000000;
        self.index = N;
    }
    pub fn genrand_u32(&mut self) -> u32 {
        let mag01 = [0u32, MATRIX_A];
        if self.index >= N {
            let mt = &mut self.state;
            let mut kk = 0;
            while kk < N - M {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + M] ^ (y >> 1) ^ mag01[(y & 1) as usize];
                kk += 1;
            }
            while kk < N - 1 {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + M - N] ^ (y >> 1) ^ mag01[(y & 1) as usize];
                kk += 1;
            }
            let y = (mt[N - 1] & UPPER_MASK) | (mt[0] & LOWER_MASK);
            mt[N - 1] = mt[M - 1] ^ (y >> 1) ^ mag01[(y & 1) as usize];
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }
    pub fn random(&mut self) -> f64 {
        let a = (self.genrand_u32() >> 5) as f64;
        let b = (self.genrand_u32() >> 6) as f64;
        (a * 67108864.0 + b) * (1.0 / 9007199254740992.0)
    }
}

#[repr(C)]
pub struct RandomObj {
    ob_base: PyObject,
    mt: *mut Mt,
    lock: std::sync::Mutex<()>,
}

#[unsafe(no_mangle)]
pub static mut _PyRandom_Type: PyTypeObject = PyTypeObject::ZERO;
fn random_type() -> *mut PyTypeObject {
    &raw mut _PyRandom_Type
}

fn mt(o: &PyObject) -> &mut Mt {
    unsafe {
        let r = &mut *(o.as_ptr() as *mut RandomObj);
        if r.mt.is_null() {
            r.mt = Box::into_raw(Box::new(Mt::new()));
        }
        &mut *r.mt
    }
}

fn seed_urandom(m: &mut Mt) {
    let mut key = [0u32; N];
    let mut buf = [0u8; N * 4];
    let ok = unsafe { libc::getrandom(buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) } == buf.len() as isize;
    if ok {
        for i in 0..N {
            key[i] = u32::from_le_bytes([buf[4 * i], buf[4 * i + 1], buf[4 * i + 2], buf[4 * i + 3]]);
        }
        m.init_by_array(&key);
    } else {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let pid = std::process::id();
        let k = [t as u32, (t >> 32) as u32, (t >> 64) as u32, pid];
        m.init_by_array(&k);
    }
}

pub fn seed_obj(m: &mut Mt, arg: Option<&PyObject>) -> PyResult<()> {
    let arg = match arg {
        None => {
            seed_urandom(m);
            return Ok(());
        }
        Some(a) if a.is(none::none()) => {
            seed_urandom(m);
            return Ok(());
        }
        Some(a) => a,
    };
    let n: PyRef = if int::is_int_exact(arg) {
        ab::absolute(arg)?
    } else if int::is_int(arg) {
        crate::types::int_methods::exact_copy(&from_big(rpy_bigint::ops::abs(iref(arg))))
    } else {
        let h = ab::hash(arg)?;
        int::new_int_u128(h as usize as u128)
    };
    if !int::is_int(&n) {
        raise!(TypeError, "absolute value of seed is not an int");
    }
    let mag = int::digits(&n);
    // little-endian 32-bit words of |n|
    let bits = if mag.is_empty() { 0 } else { (mag.len() - 1) * 30 + (32 - mag.last().unwrap().leading_zeros() as usize) };
    let keyused = if bits == 0 { 1 } else { (bits - 1) / 32 + 1 };
    let bytesv = rpy_bigint::conv::to_bytes(iref(&n), keyused * 4, true, false).map_err(crate::types::int_methods::int_error)?;
    let key: Vec<u32> = bytesv.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    m.init_by_array(&key);
    Ok(())
}

fn r_random(o: &PyObject) -> PyResult {
    let _g = lock(o);
    Ok(float::new_float(mt(o).random()))
}

fn lock(o: &PyObject) -> std::sync::MutexGuard<'_, ()> {
    unsafe { (*(o.as_ptr() as *mut RandomObj)).lock.lock().unwrap_or_else(|e| e.into_inner()) }
}

fn r_seed(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seed", a, 0, 1)?;
    let _g = lock(o);
    seed_obj(mt(o), a.first().copied())?;
    Ok(none::none_ref())
}

fn r_getstate(o: &PyObject) -> PyResult {
    let _g = lock(o);
    let m = mt(o);
    let mut v: Vec<PyRef> = m.state.iter().map(|&x| int::new_int(x as i64)).collect();
    v.push(int::new_int(m.index as i64));
    Ok(tuple::new_tuple(v))
}

fn r_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if !tuple::is_tuple(state) {
        raise!(TypeError, "state vector must be a tuple");
    }
    let items = tuple::tuple_items(state);
    if items.len() != N + 1 {
        raise!(ValueError, "state vector is the wrong size");
    }
    let mut new = [0u32; N];
    for i in 0..N {
        let x = items[i];
        if !int::is_int(x) {
            raise!(TypeError, "state vector items must be integers");
        }
        if int::sign(x) < 0 {
            raise!(OverflowError, "can't convert negative int to unsigned");
        }
        let v = match rpy_bigint::conv::to_u64(iref(x)) {
            Some(v) => v,
            None => raise!(OverflowError, "Python int too large to convert to C unsigned long"),
        };
        new[i] = v as u32;
    }
    let idx = items[N];
    if !int::is_int(idx) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(idx));
    }
    let index = match int::as_i64(idx) {
        Some(v) if (0..=N as i64).contains(&v) => v as usize,
        _ => raise!(ValueError, "invalid state"),
    };
    let _g = lock(o);
    let m = mt(o);
    m.state = new;
    m.index = index;
    Ok(none::none_ref())
}

fn r_getrandbits(o: &PyObject, k: &PyObject) -> PyResult {
    let kk = ab::index(k)?;
    let k = match int::as_i64(&kk) {
        Some(v) => v,
        None => {
            if int::sign(&kk) < 0 {
                raise!(ValueError, "number of bits must be non-negative");
            }
            raise!(OverflowError, "Python int too large to convert to C int64_t");
        }
    };
    if k < 0 {
        raise!(ValueError, "number of bits must be non-negative");
    }
    // the result must be representable: at most isize::MAX bytes of digits
    if k / 8 >= isize::MAX as i64 / 2 {
        return Err(memory_error());
    }
    let _g = lock(o);
    let m = mt(o);
    if k == 0 {
        return Ok(int::new_int(0));
    }
    if k <= 32 {
        return Ok(int::new_int((m.genrand_u32() >> (32 - k)) as i64));
    }
    let words = ((k - 1) / 32 + 1) as usize;
    let mut bytes: Vec<u8> = crate::errors::try_vec_with_capacity(words * 4)?;
    let mut rem = k;
    for _ in 0..words {
        let mut r = m.genrand_u32();
        if rem < 32 {
            r >>= 32 - rem;
        }
        bytes.extend_from_slice(&r.to_le_bytes());
        rem -= 32;
    }
    Ok(from_big(rpy_bigint::conv::from_bytes(&bytes, true, false)))
}

fn r_reduce(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", tname(o));
}

unsafe extern "C" fn random_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            let ro = &mut *(op as *mut RandomObj);
            ro.mt = Box::into_raw(Box::new(Mt::new()));
            std::ptr::write(&mut ro.lock, std::sync::Mutex::new(()));
            let obj = PyRef::from_raw(op);

            Ok(obj)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn random_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let r = &mut *(o as *mut RandomObj);
        if !r.mt.is_null() {
            drop(Box::from_raw(r.mt));
            r.mt = std::ptr::null_mut();
        }
        std::ptr::drop_in_place(&mut r.lock);
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn random_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let o = &*o;
            if o.is_type(random_type()) && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "Random() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "Random() requires 0 or 1 argument");
            }
            let _g = lock(o);
            seed_obj(mt(o), a.first().copied())
        })();
        int_into_c(r)
    }
}

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *random_type();
        t.tp_name = c"_random.Random".as_ptr();
        t.tp_basicsize = std::mem::size_of::<RandomObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_new = Some(random_new);
        t.tp_init = Some(random_init);
        t.tp_dealloc = Some(random_dealloc);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Random()\n--\n\nRandom() -> create a random number generator with its own internal state.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("random", NOARGS, r_random, "random($self, /)\n--\n\nrandom() -> x in the interval [0, 1)."),
            ("seed", FAST, r_seed, "seed($self, n=None, /)\n--\n\nseed([n]) -> None.\n\nDefaults to use urandom and falls back to a combination\nof the current time and the process identifier."),
            ("getstate", NOARGS, r_getstate, "getstate($self, /)\n--\n\ngetstate() -> tuple containing the current state."),
            ("setstate", O, r_setstate, "setstate($self, state, /)\n--\n\nsetstate(state) -> None.  Restores generator state."),
            ("getrandbits", O, r_getrandbits, "getrandbits($self, k, /)\n--\n\ngetrandbits(k) -> x.  Generates an int with k random bits."),
            ("__reduce__", NOARGS, r_reduce),
        ];
        res = type_ready(random_type());
    });
    res?;
    let m = crate::types::module::new_module("_random")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Module implements the Mersenne Twister random number generator."))?;
    dict::set_item_str(&d, "Random", &type_ref(random_type()))?;
    Ok(m)
}
