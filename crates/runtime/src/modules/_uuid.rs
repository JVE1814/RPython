//! `_uuid`: fast generation of time-based (version 1) UUIDs for `uuid.uuid1()`.
//!
//! Timestamps are made strictly increasing process-wide under a lock, so
//! concurrently generated values never collide; the node id is random (with
//! the multicast bit set, as RFC 9562 requires for non-hardware ids), so it is
//! not reported as an extractable hardware address.

use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::compress_common as cc;
use crate::object::*;
use crate::types::{dict, int, tuple};
use std::sync::Mutex;

/// 100 ns intervals between 1582-10-15 and 1970-01-01.
const GREGORIAN_OFFSET: u64 = 0x01b2_1dd2_1381_4000;

struct State {
    last: u64,
    clock_seq: u16,
    node: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn random_u64() -> u64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut b).is_ok() {
            return u64::from_le_bytes(b);
        }
    }
    // Fallback: mix the clock and the process id.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let mut x = t ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^ (x >> 29)
}

fn now_ticks() -> u64 {
    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    ns / 100 + GREGORIAN_OFFSET
}

/// Return the 16 bytes of a fresh version-1 UUID.
pub fn generate_v1() -> [u8; 16] {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = g.get_or_insert_with(|| State {
        last: 0,
        clock_seq: (random_u64() & 0x3FFF) as u16,
        node: (random_u64() & 0xFFFF_FFFF_FFFF) | (1 << 40),
    });
    let mut ts = now_ticks();
    if ts <= st.last {
        ts = st.last + 1;
    }
    st.last = ts;
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&((ts & 0xFFFF_FFFF) as u32).to_be_bytes());
    b[4..6].copy_from_slice(&(((ts >> 32) & 0xFFFF) as u16).to_be_bytes());
    b[6..8].copy_from_slice(&((((ts >> 48) & 0x0FFF) as u16) | 0x1000).to_be_bytes());
    b[8] = 0x80 | ((st.clock_seq >> 8) as u8 & 0x3F);
    b[9] = st.clock_seq as u8;
    b[10..16].copy_from_slice(&st.node.to_be_bytes()[2..8]);
    b
}

fn m_generate_time_safe(_m: &PyObject) -> PyResult {
    let b = generate_v1();
    // 0 = generated in a way that is safe against concurrent callers.
    Ok(tuple::new_tuple(vec![cc::new_bytes(&b)?, int::new_int(0)]))
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_uuid")?;
    let d = crate::types::module::module_dict(&m);
    cc::set_int(&d, "has_uuid_generate_time_safe", 1)?;
    dict::set_item_str(&d, "has_stable_extractable_node", crate::types::bool_::false_obj())?;
    let methods = crate::mdefs![(
        "generate_time_safe",
        NOARGS,
        m_generate_time_safe,
        "generate_time_safe($module, /)\n--\n\nReturn (16 bytes of a new version 1 UUID, safety flag)."
    )];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    Ok(m)
}
