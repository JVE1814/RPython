//! Socket address conversion (getsockaddrarg / makesockaddr / setipaddr).

use super::{errno, os_error, SockObj};
use crate::abstract_ as ab;
use crate::buffer::{BufferView, PyBUF_SIMPLE};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytearray, bytes, int, none, str as pystr, tuple};
use std::ffi::{c_int, CStr, CString};
use std::sync::atomic::Ordering;

pub struct SockAddr {
    pub st: libc::sockaddr_storage,
    pub len: libc::socklen_t,
}

impl SockAddr {
    pub fn new() -> SockAddr {
        SockAddr { st: unsafe { std::mem::zeroed() }, len: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t }
    }
    pub fn as_ptr(&self) -> *const libc::sockaddr {
        &self.st as *const _ as *const libc::sockaddr
    }
    pub fn as_mut_ptr(&mut self) -> *mut libc::sockaddr {
        &mut self.st as *mut _ as *mut libc::sockaddr
    }
    pub fn family(&self) -> i32 {
        self.st.ss_family as i32
    }
    pub fn from_raw(p: *const libc::sockaddr, len: usize) -> SockAddr {
        let mut s = SockAddr::new();
        let n = len.min(std::mem::size_of::<libc::sockaddr_storage>());
        unsafe { std::ptr::copy_nonoverlapping(p as *const u8, &mut s.st as *mut _ as *mut u8, n) };
        s.len = n as libc::socklen_t;
        s
    }
    fn cast<T>(&self) -> &T {
        unsafe { &*(&self.st as *const _ as *const T) }
    }
    fn cast_mut<T>(&mut self) -> &mut T {
        unsafe { &mut *(&mut self.st as *mut _ as *mut T) }
    }
}

// ------------------------------------------------------------------ errors

pub fn gaierror(code: c_int) -> PyErr {
    if code == libc::EAI_SYSTEM {
        return os_error(errno());
    }
    let msg = unsafe { CStr::from_ptr(libc::gai_strerror(code)).to_string_lossy().into_owned() };
    let tp = super::GAIERROR.load(Ordering::Acquire);
    match ab::call(unsafe { &*tp }, &[&int::new_int(code as i64), &pystr::new_str(&msg)]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

pub fn herror(code: c_int) -> PyErr {
    let msg = match code {
        1 => "Unknown host",
        2 => "Host name lookup failure",
        3 => "Unknown server error",
        4 => "No address associated with name",
        _ => "Resolver internal error",
    };
    let tp = super::HERROR.load(Ordering::Acquire);
    match ab::call(unsafe { &*tp }, &[&int::new_int(code as i64), &pystr::new_str(msg)]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

// ------------------------------------------------------------------ host names

/// idna_converter: str (ASCII or IDNA-encoded), bytes or bytearray -> bytes without NUL.
pub fn host_bytes(o: &PyObject) -> PyResult<Vec<u8>> {
    let v: Vec<u8> = if pystr::is_str(o) {
        if let Some(a) = pystr::ascii_str(o) {
            a.as_bytes().to_vec()
        } else {
            let e = ab::call_method_str(o, "encode", &[&pystr::new_str("idna")])?;
            bytes::bytes_data(&e).to_vec()
        }
    } else if bytes::is_bytes(o) {
        bytes::bytes_data(o).to_vec()
    } else if bytearray::is_bytearray(o) {
        bytearray::bytearray_data(o).to_vec()
    } else {
        raise!(TypeError, "str, bytes or bytearray expected, not {}", tname(o));
    };
    if v.contains(&0) {
        raise!(TypeError, "host name must not contain null character");
    }
    Ok(v)
}

fn gai(host: Option<&[u8]>, port: Option<&[u8]>, hints: &libc::addrinfo) -> Result<*mut libc::addrinfo, c_int> {
    let h = host.map(|h| CString::new(h).unwrap());
    let p = port.map(|p| CString::new(p).unwrap());
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let hp = h.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let pp = p.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let hints_p = hints as *const libc::addrinfo as usize;
    let resp = &mut res as *mut *mut libc::addrinfo as usize;
    let (hpu, ppu) = (hp as usize, pp as usize);
    let r = crate::threads::blocking(|| unsafe {
        libc::getaddrinfo(
            hpu as *const libc::c_char,
            ppu as *const libc::c_char,
            hints_p as *const libc::addrinfo,
            resp as *mut *mut libc::addrinfo,
        )
    });
    if r != 0 {
        return Err(r);
    }
    Ok(res)
}

pub fn getaddrinfo_raw(host: Option<&[u8]>, port: Option<&[u8]>, family: c_int, socktype: c_int, proto: c_int, flags: c_int) -> PyResult<Vec<(c_int, c_int, c_int, Option<String>, SockAddr)>> {
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = family;
    hints.ai_socktype = socktype;
    hints.ai_protocol = proto;
    hints.ai_flags = flags;
    let res = match gai(host, port, &hints) {
        Ok(r) => r,
        Err(code) => return Err(gaierror(code)),
    };
    let mut out = Vec::new();
    let mut p = res;
    unsafe {
        while !p.is_null() {
            let ai = &*p;
            let canon = if ai.ai_canonname.is_null() { None } else { Some(CStr::from_ptr(ai.ai_canonname).to_string_lossy().into_owned()) };
            out.push((ai.ai_family, ai.ai_socktype, ai.ai_protocol, canon, SockAddr::from_raw(ai.ai_addr, ai.ai_addrlen as usize)));
            p = ai.ai_next;
        }
        libc::freeaddrinfo(res);
    }
    Ok(out)
}

/// Resolve `name` into an address of family `af` (port 0).
pub fn setipaddr(name: &[u8], af: c_int) -> PyResult<SockAddr> {
    if name.is_empty() {
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_family = af;
        hints.ai_socktype = libc::SOCK_DGRAM;
        hints.ai_flags = libc::AI_PASSIVE;
        let res = match gai(None, Some(b"0"), &hints) {
            Ok(r) => r,
            Err(code) => return Err(gaierror(code)),
        };
        unsafe {
            let ai = &*res;
            let multiple = !ai.ai_next.is_null();
            let sa = SockAddr::from_raw(ai.ai_addr, ai.ai_addrlen as usize);
            libc::freeaddrinfo(res);
            if multiple {
                raise!(OSError, "wildcard resolved to multiple address");
            }
            return Ok(sa);
        }
    }
    if name == b"<broadcast>" || name == b"255.255.255.255" {
        if af != libc::AF_INET && af != libc::AF_UNSPEC {
            raise!(OSError, "address family mismatched");
        }
        let mut sa = SockAddr::new();
        let sin: &mut libc::sockaddr_in = sa.cast_mut();
        sin.sin_family = libc::AF_INET as _;
        sin.sin_addr.s_addr = u32::MAX;
        sa.len = std::mem::size_of::<libc::sockaddr_in>() as _;
        return Ok(sa);
    }
    // numeric fast paths
    let cname = match CString::new(name) {
        Ok(c) => c,
        Err(_) => raise!(TypeError, "host name must not contain null character"),
    };
    if af == libc::AF_INET || af == libc::AF_UNSPEC {
        let mut a: libc::in_addr = unsafe { std::mem::zeroed() };
        if unsafe { inet_pton(libc::AF_INET, cname.as_ptr(), &mut a as *mut _ as *mut libc::c_void) } == 1 {
            let mut sa = SockAddr::new();
            let sin: &mut libc::sockaddr_in = sa.cast_mut();
            sin.sin_family = libc::AF_INET as _;
            sin.sin_addr = a;
            sa.len = std::mem::size_of::<libc::sockaddr_in>() as _;
            return Ok(sa);
        }
    }
    if (af == libc::AF_INET6 || af == libc::AF_UNSPEC) && !name.contains(&b'%') {
        let mut a: libc::in6_addr = unsafe { std::mem::zeroed() };
        if unsafe { inet_pton(libc::AF_INET6, cname.as_ptr(), &mut a as *mut _ as *mut libc::c_void) } == 1 {
            let mut sa = SockAddr::new();
            let sin: &mut libc::sockaddr_in6 = sa.cast_mut();
            sin.sin6_family = libc::AF_INET6 as _;
            sin.sin6_addr = a;
            sa.len = std::mem::size_of::<libc::sockaddr_in6>() as _;
            return Ok(sa);
        }
    }
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = af;
    let res = match gai(Some(name), None, &hints) {
        Ok(r) => r,
        Err(code) => return Err(gaierror(code)),
    };
    unsafe {
        let ai = &*res;
        let sa = SockAddr::from_raw(ai.ai_addr, ai.ai_addrlen as usize);
        libc::freeaddrinfo(res);
        Ok(sa)
    }
}

// ------------------------------------------------------------------ helpers

fn check_tuple(o: &PyObject, caller: &str, fam: &str) -> PyResult<()> {
    if !tuple::is_tuple(o) {
        raise!(TypeError, "{}(): {} address must be tuple, not {}", caller, fam, tname(o));
    }
    Ok(())
}

/// "i" conversion inside a PyArg_ParseTuple with a custom message.
fn parse_int(o: &PyObject, msg: &str) -> PyResult<i64> {
    if crate::types::float::is_float(o) || !ab::has_index(o) {
        raise!(TypeError, "{}", msg);
    }
    let v = ab::index(o)?;
    match int::as_i64(&v) {
        Some(x) if x >= i32::MIN as i64 && x <= i32::MAX as i64 => Ok(x),
        Some(_) => raise!(OverflowError, "signed integer is greater than maximum"),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

/// "I"/"k" conversion (unsigned, no overflow checking: wraps).
fn parse_uint(o: &PyObject, msg: &str) -> PyResult<u64> {
    if crate::types::float::is_float(o) || !ab::has_index(o) {
        raise!(TypeError, "{}", msg);
    }
    let v = ab::index(o)?;
    match int::as_i128(&v) {
        Some(x) => Ok(x as u64),
        None => {
            // wrap modulo 2**64
            let mut mag: u64 = 0;
            for (i, &d) in int::digits(&v).iter().enumerate() {
                let sh = i * 30;
                if sh < 64 {
                    mag |= (d as u64) << sh;
                }
            }
            Ok(if int::sign(&v) < 0 { mag.wrapping_neg() } else { mag })
        }
    }
}

fn port_err(caller: &str) -> PyErr {
    crate::err!(OverflowError, "{}(): port must be 0-65535.", caller)
}

fn ifindex(fd: c_int, name: &[u8]) -> PyResult<c_int> {
    if name.len() >= libc::IFNAMSIZ {
        raise!(OSError, "interface name too long");
    }
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    for (i, &b) in name.iter().enumerate() {
        ifr.ifr_name[i] = b as libc::c_char;
    }
    if unsafe { libc::ioctl(fd, libc::SIOCGIFINDEX, &mut ifr) } < 0 {
        return Err(os_error(errno()));
    }
    Ok(unsafe { ifr.ifr_ifru.ifru_ifindex })
}

fn fs_bytes(o: &PyObject) -> PyResult<Vec<u8>> {
    if pystr::is_str(o) {
        return crate::modules::posix::path::fs_encode_str(o);
    }
    if bytes::is_bytes(o) {
        return Ok(bytes::bytes_data(o).to_vec());
    }
    let p = crate::modules::posix::path::fspath(o)?;
    if pystr::is_str(&p) {
        return crate::modules::posix::path::fs_encode_str(&p);
    }
    Ok(bytes::bytes_data(&p).to_vec())
}

// ------------------------------------------------------------------ getsockaddrarg

pub fn getsockaddrarg(s: &SockObj, a: &PyObject, caller: &str) -> PyResult<SockAddr> {
    let mut sa = SockAddr::new();
    match s.family {
        libc::AF_UNIX => {
            let path: Vec<u8> = if pystr::is_str(a) {
                crate::modules::posix::path::fs_encode_str(a)?
            } else {
                BufferView::get(a, PyBUF_SIMPLE)?.to_vec()
            };
            let sun: &mut libc::sockaddr_un = sa.cast_mut();
            sun.sun_family = libc::AF_UNIX as _;
            let cap = sun.sun_path.len();
            let off = std::mem::size_of::<libc::sa_family_t>();
            if path.is_empty() || path[0] == 0 {
                if path.len() > cap {
                    raise!(OSError, "AF_UNIX path too long");
                }
                for (i, &b) in path.iter().enumerate() {
                    sun.sun_path[i] = b as libc::c_char;
                }
                sa.len = (off + path.len()) as _;
            } else {
                if path.len() >= cap {
                    raise!(OSError, "AF_UNIX path too long");
                }
                for (i, &b) in path.iter().enumerate() {
                    sun.sun_path[i] = b as libc::c_char;
                }
                sun.sun_path[path.len()] = 0;
                sa.len = (off + path.len() + 1) as _;
            }
            Ok(sa)
        }
        libc::AF_INET => {
            check_tuple(a, caller, "AF_INET")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_INET address must be a pair (host, port)";
            if t.len() != 2 {
                raise!(TypeError, "{}", M);
            }
            let host = host_bytes(t[0])?;
            let port = match parse_int(t[1], M) {
                Ok(p) => p,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::OverflowError()) {
                        clear_exc();
                        return Err(port_err(caller));
                    }
                    return Err(e);
                }
            };
            if !(0..=0xffff).contains(&port) {
                return Err(port_err(caller));
            }
            let r = setipaddr(&host, libc::AF_INET)?;
            let sin: &libc::sockaddr_in = r.cast();
            let out: &mut libc::sockaddr_in = sa.cast_mut();
            out.sin_family = libc::AF_INET as _;
            out.sin_addr = sin.sin_addr;
            out.sin_port = (port as u16).to_be();
            sa.len = std::mem::size_of::<libc::sockaddr_in>() as _;
            Ok(sa)
        }
        libc::AF_INET6 => {
            check_tuple(a, caller, "AF_INET6")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_INET6 address must be a tuple (host, port[, flowinfo[, scopeid]])";
            if t.len() < 2 || t.len() > 4 {
                raise!(TypeError, "{}", M);
            }
            let host = host_bytes(t[0])?;
            let port = match parse_int(t[1], M) {
                Ok(p) => p,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::OverflowError()) {
                        clear_exc();
                        return Err(port_err(caller));
                    }
                    return Err(e);
                }
            };
            let flowinfo = if t.len() > 2 { parse_uint(t[2], M)? as u32 } else { 0 };
            let scope_id = if t.len() > 3 { parse_uint(t[3], M)? as u32 } else { 0 };
            if !(0..=0xffff).contains(&port) {
                return Err(port_err(caller));
            }
            if flowinfo > 0xfffff {
                raise!(OverflowError, "{}(): flowinfo must be 0-1048575.", caller);
            }
            let r = setipaddr(&host, libc::AF_INET6)?;
            let sin: &libc::sockaddr_in6 = r.cast();
            let addr6 = sin.sin6_addr;
            let rscope = sin.sin6_scope_id;
            let out: &mut libc::sockaddr_in6 = sa.cast_mut();
            out.sin6_family = libc::AF_INET6 as _;
            out.sin6_addr = addr6;
            out.sin6_port = (port as u16).to_be();
            out.sin6_flowinfo = flowinfo.to_be();
            out.sin6_scope_id = if t.len() > 3 { scope_id } else { rscope };
            sa.len = std::mem::size_of::<libc::sockaddr_in6>() as _;
            Ok(sa)
        }
        libc::AF_NETLINK => {
            check_tuple(a, caller, "AF_NETLINK")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_NETLINK address must be a pair (pid, groups)";
            if t.len() != 2 {
                raise!(TypeError, "{}", M);
            }
            let pid = parse_uint(t[0], M)? as u32;
            let groups = parse_uint(t[1], M)? as u32;
            let nl: &mut libc::sockaddr_nl = sa.cast_mut();
            nl.nl_family = libc::AF_NETLINK as _;
            nl.nl_pid = pid;
            nl.nl_groups = groups;
            sa.len = std::mem::size_of::<libc::sockaddr_nl>() as _;
            Ok(sa)
        }
        libc::AF_VSOCK => {
            check_tuple(a, caller, "AF_VSOCK")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_VSOCK address must be a pair (cid, port)";
            if t.len() != 2 {
                raise!(TypeError, "{}", M);
            }
            let cid = parse_uint(t[0], M)? as u32;
            let port = parse_uint(t[1], M)? as u32;
            let vm: &mut libc::sockaddr_vm = sa.cast_mut();
            vm.svm_family = libc::AF_VSOCK as _;
            vm.svm_cid = cid;
            vm.svm_port = port;
            sa.len = std::mem::size_of::<libc::sockaddr_vm>() as _;
            Ok(sa)
        }
        42 => {
            // AF_QIPCRTR: struct sockaddr_qrtr { sa_family_t; u32 node; u32 port }
            check_tuple(a, caller, "AF_QIPCRTR")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_QIPCRTR address must be a pair (node, port)";
            if t.len() != 2 {
                raise!(TypeError, "{}", M);
            }
            let node = parse_uint(t[0], M)? as u32;
            let port = parse_uint(t[1], M)? as u32;
            let p = sa.as_mut_ptr() as *mut u8;
            unsafe {
                *(p as *mut u16) = 42;
                std::ptr::write_unaligned(p.add(4) as *mut u32, node);
                std::ptr::write_unaligned(p.add(8) as *mut u32, port);
            }
            sa.len = 12;
            Ok(sa)
        }
        libc::AF_PACKET => {
            check_tuple(a, caller, "AF_PACKET")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_PACKET address must be a tuple of two to five elements";
            if t.len() < 2 || t.len() > 5 {
                raise!(TypeError, "{}", M);
            }
            if !pystr::is_str(t[0]) {
                raise!(TypeError, "{}", M);
            }
            let ifname = pystr::as_utf8(t[0])?.as_bytes().to_vec();
            let proto = match parse_int(t[1], M) {
                Ok(p) => p,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::OverflowError()) {
                        clear_exc();
                        raise!(OverflowError, "{}(): proto must be 0-65535.", caller);
                    }
                    return Err(e);
                }
            };
            let pkttype = if t.len() > 2 { parse_int(t[2], M)? } else { 0 };
            let hatype = if t.len() > 3 { parse_int(t[3], M)? } else { 0 };
            let haddr = if t.len() > 4 { BufferView::get(t[4], PyBUF_SIMPLE)?.to_vec() } else { Vec::new() };
            if !(0..=0xffff).contains(&proto) {
                raise!(OverflowError, "{}(): proto must be 0-65535.", caller);
            }
            if haddr.len() > 8 {
                raise!(ValueError, "Hardware address must be 8 bytes or less");
            }
            let idx = ifindex(s.fd.load(Ordering::Relaxed), &ifname)?;
            let ll: &mut libc::sockaddr_ll = sa.cast_mut();
            ll.sll_family = libc::AF_PACKET as _;
            ll.sll_protocol = (proto as u16).to_be();
            ll.sll_ifindex = idx;
            ll.sll_pkttype = pkttype as u8;
            ll.sll_hatype = hatype as u16;
            ll.sll_halen = haddr.len() as u8;
            ll.sll_addr[..haddr.len()].copy_from_slice(&haddr);
            sa.len = std::mem::size_of::<libc::sockaddr_ll>() as _;
            Ok(sa)
        }
        libc::AF_CAN => {
            check_tuple(a, caller, "AF_CAN")?;
            let t = tuple::tuple_items(a);
            let p = sa.as_mut_ptr() as *mut u8;
            // struct sockaddr_can { sa_family_t can_family; int can_ifindex; union {...} can_addr; }
            let fill = |name: &[u8], p: *mut u8| -> PyResult<()> {
                let idx = if name.is_empty() { 0 } else { ifindex(s.fd.load(Ordering::Relaxed), name)? };
                unsafe {
                    *(p as *mut u16) = libc::AF_CAN as u16;
                    std::ptr::write_unaligned(p.add(4) as *mut c_int, idx);
                }
                Ok(())
            };
            match s.proto {
                1 | 2 => {
                    // CAN_RAW, CAN_BCM
                    if t.len() != 1 {
                        raise!(TypeError, "AF_CAN address must be a tuple (interface, )");
                    }
                    let name = fs_bytes(t[0])?;
                    if name.len() >= libc::IFNAMSIZ {
                        raise!(OSError, "interface name too long");
                    }
                    fill(&name, p)?;
                    sa.len = 24;
                    Ok(sa)
                }
                6 => {
                    // CAN_ISOTP
                    const M: &str = "AF_CAN address must be a tuple (interface, rx_addr, tx_addr)";
                    if t.len() != 3 {
                        raise!(TypeError, "{}", M);
                    }
                    let name = fs_bytes(t[0])?;
                    if name.len() >= libc::IFNAMSIZ {
                        raise!(OSError, "interface name too long");
                    }
                    let rx = parse_uint(t[1], M)? as u32;
                    let tx = parse_uint(t[2], M)? as u32;
                    fill(&name, p)?;
                    unsafe {
                        std::ptr::write_unaligned(p.add(8) as *mut u32, rx);
                        std::ptr::write_unaligned(p.add(12) as *mut u32, tx);
                    }
                    sa.len = 24;
                    Ok(sa)
                }
                7 => {
                    // CAN_J1939
                    const M: &str = "AF_CAN address must be a tuple (interface, name, pgn, addr)";
                    if t.len() != 4 {
                        raise!(TypeError, "{}", M);
                    }
                    let name = fs_bytes(t[0])?;
                    if name.len() >= libc::IFNAMSIZ {
                        raise!(OSError, "interface name too long");
                    }
                    let jname = parse_uint(t[1], M)?;
                    let pgn = parse_uint(t[2], M)? as u32;
                    let jaddr = parse_uint(t[3], M)? as u8;
                    fill(&name, p)?;
                    unsafe {
                        std::ptr::write_unaligned(p.add(8) as *mut u64, jname);
                        std::ptr::write_unaligned(p.add(16) as *mut u32, pgn);
                        *p.add(20) = jaddr;
                    }
                    sa.len = 24;
                    Ok(sa)
                }
                _ => raise!(OSError, "{}(): unsupported CAN protocol", caller),
            }
        }
        libc::AF_ALG => {
            check_tuple(a, caller, "AF_ALG")?;
            let t = tuple::tuple_items(a);
            const M: &str = "AF_ALG address must be a tuple (type, name[, feat[, mask]])";
            if t.len() < 2 || t.len() > 4 || !pystr::is_str(t[0]) || !pystr::is_str(t[1]) {
                raise!(TypeError, "{}", M);
            }
            let ty = pystr::as_utf8(t[0])?.as_bytes().to_vec();
            let name = pystr::as_utf8(t[1])?.as_bytes().to_vec();
            let feat = if t.len() > 2 { parse_uint(t[2], M)? as u32 } else { 0 };
            let mask = if t.len() > 3 { parse_uint(t[3], M)? as u32 } else { 0 };
            let alg: &mut libc::sockaddr_alg = sa.cast_mut();
            if ty.len() >= alg.salg_type.len() {
                raise!(ValueError, "AF_ALG type too long.");
            }
            if name.len() >= alg.salg_name.len() {
                raise!(ValueError, "AF_ALG name too long.");
            }
            alg.salg_family = libc::AF_ALG as _;
            alg.salg_type[..ty.len()].copy_from_slice(&ty);
            alg.salg_name[..name.len()].copy_from_slice(&name);
            alg.salg_feat = feat;
            alg.salg_mask = mask;
            sa.len = std::mem::size_of::<libc::sockaddr_alg>() as _;
            Ok(sa)
        }
        _ => raise!(OSError, "{}(): bad family", caller),
    }
}

// ------------------------------------------------------------------ makesockaddr

pub fn ntop(af: c_int, src: *const libc::c_void) -> String {
    let mut buf = [0 as libc::c_char; 64];
    let r = unsafe { inet_ntop(af, src, buf.as_mut_ptr(), buf.len() as _) };
    if r.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned() }
}

/// Numeric host string of an address (IPv6 with %scope via getnameinfo).
pub fn makeipaddr(sa: &SockAddr) -> PyResult {
    if sa.family() == libc::AF_INET {
        let sin: &libc::sockaddr_in = sa.cast();
        return Ok(pystr::new_str(&ntop(libc::AF_INET, &sin.sin_addr as *const _ as *const libc::c_void)));
    }
    let mut host = [0 as libc::c_char; 1025];
    let r = unsafe {
        libc::getnameinfo(sa.as_ptr(), sa.len, host.as_mut_ptr(), host.len() as _, std::ptr::null_mut(), 0, libc::NI_NUMERICHOST)
    };
    if r != 0 {
        return Err(gaierror(r));
    }
    Ok(pystr::new_str(&unsafe { CStr::from_ptr(host.as_ptr()) }.to_string_lossy()))
}

fn ifname_of(idx: c_int) -> PyResult {
    if idx == 0 {
        return Ok(pystr::new_str(""));
    }
    let mut buf = [0 as libc::c_char; libc::IFNAMSIZ];
    let r = unsafe { libc::if_indextoname(idx as u32, buf.as_mut_ptr()) };
    if r.is_null() {
        return Ok(pystr::new_str(""));
    }
    crate::modules::posix::path::fs_decode(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_bytes())
}

pub fn makesockaddr(sa: &SockAddr, proto: c_int) -> PyResult {
    if sa.len == 0 {
        return Ok(none::none_ref());
    }
    let len = sa.len as usize;
    match sa.family() {
        libc::AF_INET => {
            let sin: &libc::sockaddr_in = sa.cast();
            Ok(tuple::new_tuple(vec![makeipaddr(sa)?, int::new_int(u16::from_be(sin.sin_port) as i64)]))
        }
        libc::AF_INET6 => {
            let sin: &libc::sockaddr_in6 = sa.cast();
            Ok(tuple::new_tuple(vec![
                makeipaddr(sa)?,
                int::new_int(u16::from_be(sin.sin6_port) as i64),
                int::new_int(u32::from_be(sin.sin6_flowinfo) as i64),
                int::new_int(sin.sin6_scope_id as i64),
            ]))
        }
        libc::AF_UNIX => {
            let sun: &libc::sockaddr_un = sa.cast();
            let off = std::mem::size_of::<libc::sa_family_t>();
            let plen = len.saturating_sub(off).min(sun.sun_path.len());
            let path: &[u8] = unsafe { std::slice::from_raw_parts(sun.sun_path.as_ptr() as *const u8, plen) };
            if plen > 0 && path[0] == 0 {
                return Ok(bytes::new_bytes(path));
            }
            let end = path.iter().position(|&c| c == 0).unwrap_or(plen);
            crate::modules::posix::path::fs_decode(&path[..end])
        }
        libc::AF_NETLINK => {
            let nl: &libc::sockaddr_nl = sa.cast();
            Ok(tuple::new_tuple(vec![int::new_int(nl.nl_pid as i64), int::new_int(nl.nl_groups as i64)]))
        }
        libc::AF_VSOCK => {
            let vm: &libc::sockaddr_vm = sa.cast();
            Ok(tuple::new_tuple(vec![int::new_int(vm.svm_cid as i64), int::new_int(vm.svm_port as i64)]))
        }
        42 => {
            let p = sa.as_ptr() as *const u8;
            let (node, port) = unsafe { (std::ptr::read_unaligned(p.add(4) as *const u32), std::ptr::read_unaligned(p.add(8) as *const u32)) };
            Ok(tuple::new_tuple(vec![int::new_int(node as i64), int::new_int(port as i64)]))
        }
        libc::AF_PACKET => {
            let ll: &libc::sockaddr_ll = sa.cast();
            let name = ifname_of(ll.sll_ifindex)?;
            let hl = (ll.sll_halen as usize).min(8);
            Ok(tuple::new_tuple(vec![
                name,
                int::new_int(u16::from_be(ll.sll_protocol) as i64),
                int::new_int(ll.sll_pkttype as i64),
                int::new_int(ll.sll_hatype as i64),
                bytes::new_bytes(&ll.sll_addr[..hl]),
            ]))
        }
        libc::AF_CAN => {
            let p = sa.as_ptr() as *const u8;
            let idx = unsafe { std::ptr::read_unaligned(p.add(4) as *const c_int) };
            let name = ifname_of(idx)?;
            match proto {
                6 => {
                    let (rx, tx) = unsafe { (std::ptr::read_unaligned(p.add(8) as *const u32), std::ptr::read_unaligned(p.add(12) as *const u32)) };
                    Ok(tuple::new_tuple(vec![name, int::new_int(rx as i64), int::new_int(tx as i64)]))
                }
                7 => {
                    let (jn, pgn, ad) = unsafe {
                        (std::ptr::read_unaligned(p.add(8) as *const u64), std::ptr::read_unaligned(p.add(16) as *const u32), *p.add(20))
                    };
                    Ok(tuple::new_tuple(vec![name, int::new_int_u128(jn as u128), int::new_int(pgn as i64), int::new_int(ad as i64)]))
                }
                _ => Ok(tuple::new_tuple(vec![name])),
            }
        }
        f => {
            let p = sa.as_ptr() as *const u8;
            let data = unsafe { std::slice::from_raw_parts(p.add(2), 14) };
            Ok(tuple::new_tuple(vec![int::new_int(f as i64), bytes::new_bytes(data)]))
        }
    }
}

unsafe extern "C" {
    pub fn inet_pton(af: c_int, src: *const libc::c_char, dst: *mut libc::c_void) -> c_int;
    pub fn inet_ntop(af: c_int, src: *const libc::c_void, dst: *mut libc::c_char, size: libc::socklen_t) -> *const libc::c_char;
}
