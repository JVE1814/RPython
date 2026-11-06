//! Name resolution and address-text conversion functions of `_socket`.

use super::addr::{self, SockAddr};
use super::{errno, os_error};
use crate::abstract_ as ab;
use crate::buffer::{BufferView, PyBUF_SIMPLE};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytes, int, list, none, str as pystr, tuple};
use std::ffi::{c_int, CStr, CString};

unsafe extern "C" {
    fn gethostbyname_r(
        name: *const libc::c_char,
        ret: *mut libc::hostent,
        buf: *mut libc::c_char,
        buflen: libc::size_t,
        result: *mut *mut libc::hostent,
        h_errnop: *mut c_int,
    ) -> c_int;
    fn gethostbyaddr_r(
        addr: *const libc::c_void,
        len: libc::socklen_t,
        ty: c_int,
        ret: *mut libc::hostent,
        buf: *mut libc::c_char,
        buflen: libc::size_t,
        result: *mut *mut libc::hostent,
        h_errnop: *mut c_int,
    ) -> c_int;
    fn inet_aton(cp: *const libc::c_char, inp: *mut libc::in_addr) -> c_int;
    fn sethostname(name: *const libc::c_char, len: libc::size_t) -> c_int;
}

static NETDB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn int_arg(o: &PyObject) -> PyResult<i32> {
    crate::modules::posix::path::c_int(o)
}

fn str_arg<'a>(o: &'a PyObject, fname: &str, n: usize) -> PyResult<&'a str> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument {} must be str, not {}", fname, n, tname(o));
    }
    let s = pystr::as_utf8(o)?;
    if s.contains('\0') {
        raise!(ValueError, "embedded null character");
    }
    Ok(s)
}

fn f_gethostname(_m: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("socket.gethostname", &[])?;
    let mut buf = [0u8; 1024];
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len() - 1) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    crate::modules::posix::path::fs_decode(&buf[..end])
}

fn f_sethostname(_m: &PyObject, a: &PyObject) -> PyResult {
    let b: Vec<u8> = if pystr::is_str(a) {
        crate::modules::posix::path::fs_encode_str(a)?
    } else {
        BufferView::get(a, PyBUF_SIMPLE)?.to_vec()
    };
    crate::modules::sys_full::audit("socket.sethostname", &[a])?;
    let r = unsafe { sethostname(b.as_ptr() as *const libc::c_char, b.len()) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn f_gethostbyname(_m: &PyObject, a: &PyObject) -> PyResult {
    let name = addr::host_bytes(a)?;
    crate::modules::sys_full::audit("socket.gethostbyname", &[a])?;
    let sa = addr::setipaddr(&name, libc::AF_INET)?;
    addr::makeipaddr(&sa)
}

/// Build (name, aliases, addresses) from a hostent.
unsafe fn gethost_common(h: *const libc::hostent) -> PyResult {
    unsafe {
        let h = &*h;
        let name = if h.h_name.is_null() { String::new() } else { CStr::from_ptr(h.h_name).to_string_lossy().into_owned() };
        let aliases = list::new_list_empty();
        if !h.h_aliases.is_null() {
            let mut p = h.h_aliases;
            while !(*p).is_null() {
                list::append(&aliases, pystr::new_str(&CStr::from_ptr(*p).to_string_lossy()));
                p = p.add(1);
            }
        }
        let addrs = list::new_list_empty();
        if !h.h_addr_list.is_null() {
            let mut p = h.h_addr_list;
            while !(*p).is_null() {
                let s = addr::ntop(h.h_addrtype, *p as *const libc::c_void);
                list::append(&addrs, pystr::new_str(&s));
                p = p.add(1);
            }
        }
        Ok(tuple::new_tuple(vec![pystr::new_str(&name), aliases, addrs]))
    }
}

fn h_err(code: c_int) -> PyErr {
    addr::herror(code)
}

fn f_gethostbyname_ex(_m: &PyObject, a: &PyObject) -> PyResult {
    let name = addr::host_bytes(a)?;
    crate::modules::sys_full::audit("socket.gethostbyname", &[a])?;
    addr::setipaddr(&name, libc::AF_INET)?;
    let cname = CString::new(name).unwrap();
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut he: libc::hostent = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::hostent = std::ptr::null_mut();
    let mut herr: c_int = 0;
    let _g = NETDB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = crate::threads::blocking(|| unsafe { gethostbyname_r(cname.as_ptr(), &mut he, buf.as_mut_ptr(), buf.len(), &mut res, &mut herr) });
    if r != 0 || res.is_null() {
        return Err(h_err(if herr == 0 { 1 } else { herr }));
    }
    unsafe { gethost_common(res) }
}

fn f_gethostbyaddr(_m: &PyObject, a: &PyObject) -> PyResult {
    let name = addr::host_bytes(a)?;
    crate::modules::sys_full::audit("socket.gethostbyaddr", &[a])?;
    let sa = addr::setipaddr(&name, libc::AF_UNSPEC)?;
    let (ap, alen, af): (*const libc::c_void, usize, c_int) = unsafe {
        if sa.family() == libc::AF_INET6 {
            let s6 = &*(sa.as_ptr() as *const libc::sockaddr_in6);
            (&s6.sin6_addr as *const _ as *const libc::c_void, 16, libc::AF_INET6)
        } else {
            let s4 = &*(sa.as_ptr() as *const libc::sockaddr_in);
            (&s4.sin_addr as *const _ as *const libc::c_void, 4, libc::AF_INET)
        }
    };
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut he: libc::hostent = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::hostent = std::ptr::null_mut();
    let mut herr: c_int = 0;
    let apu = ap as usize;
    let r = crate::threads::blocking(|| unsafe {
        gethostbyaddr_r(apu as *const libc::c_void, alen as _, af, &mut he, buf.as_mut_ptr(), buf.len(), &mut res, &mut herr)
    });
    if r != 0 || res.is_null() {
        return Err(h_err(if herr == 0 { 1 } else { herr }));
    }
    unsafe { gethost_common(res) }
}

fn f_getservbyname(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getservbyname", args, 1, 2)?;
    let name = str_arg(args[0], "getservbyname", 1)?;
    let proto = match args.get(1) {
        Some(p) => Some(str_arg(p, "getservbyname", 2)?),
        None => None,
    };
    crate::modules::sys_full::audit(
        "socket.getservbyname",
        &[args[0], args.get(1).copied().unwrap_or(none::none())],
    )?;
    let cn = CString::new(name).unwrap();
    let cp = proto.map(|p| CString::new(p).unwrap());
    let _g = NETDB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sp = unsafe { libc::getservbyname(cn.as_ptr(), cp.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if sp.is_null() {
        raise!(OSError, "service/proto not found");
    }
    let port = unsafe { (*sp).s_port };
    Ok(int::new_int(u16::from_be(port as u16) as i64))
}

fn f_getservbyport(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getservbyport", args, 1, 2)?;
    let port = int_arg(args[0])?;
    let proto = match args.get(1) {
        Some(p) => Some(str_arg(p, "getservbyport", 2)?),
        None => None,
    };
    if !(0..=0xffff).contains(&port) {
        raise!(OverflowError, "getservbyport: port must be 0-65535.");
    }
    crate::modules::sys_full::audit(
        "socket.getservbyport",
        &[args[0], args.get(1).copied().unwrap_or(none::none())],
    )?;
    let cp = proto.map(|p| CString::new(p).unwrap());
    let _g = NETDB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sp = unsafe { libc::getservbyport((port as u16).to_be() as c_int, cp.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())) };
    if sp.is_null() {
        raise!(OSError, "port/proto not found");
    }
    let name = unsafe { CStr::from_ptr((*sp).s_name).to_string_lossy().into_owned() };
    Ok(pystr::new_str(&name))
}

fn f_getprotobyname(_m: &PyObject, a: &PyObject) -> PyResult {
    let name = str_arg(a, "getprotobyname", 1)?;
    let cn = CString::new(name).unwrap();
    let _g = NETDB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let sp = unsafe { libc::getprotobyname(cn.as_ptr()) };
    if sp.is_null() {
        raise!(OSError, "protocol not found");
    }
    Ok(int::new_int(unsafe { (*sp).p_proto } as i64))
}

fn f_inet_aton(_m: &PyObject, a: &PyObject) -> PyResult {
    let s = str_arg(a, "inet_aton", 1)?;
    let c = CString::new(s).unwrap();
    let mut ia: libc::in_addr = unsafe { std::mem::zeroed() };
    if unsafe { inet_aton(c.as_ptr(), &mut ia) } == 0 {
        raise!(OSError, "illegal IP address string passed to inet_aton");
    }
    Ok(bytes::new_bytes(&ia.s_addr.to_ne_bytes()))
}

fn f_inet_ntoa(_m: &PyObject, a: &PyObject) -> PyResult {
    let b = BufferView::get(a, PyBUF_SIMPLE)?;
    if b.len() != 4 {
        raise!(OSError, "packed IP wrong length for inet_ntoa");
    }
    let d = b.as_bytes();
    Ok(pystr::new_str(&format!("{}.{}.{}.{}", d[0], d[1], d[2], d[3])))
}

fn f_inet_pton(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("inet_pton", args, 2, 2)?;
    let af = int_arg(args[0])?;
    let s = str_arg(args[1], "inet_pton", 2)?;
    let c = CString::new(s).unwrap();
    let mut buf = [0u8; 16];
    let r = unsafe { addr::inet_pton(af, c.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    if r == 0 {
        raise!(OSError, "illegal IP address string passed to inet_pton");
    }
    match af {
        libc::AF_INET => Ok(bytes::new_bytes(&buf[..4])),
        libc::AF_INET6 => Ok(bytes::new_bytes(&buf[..16])),
        _ => raise!(OSError, "unknown address family"),
    }
}

fn f_inet_ntop(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("inet_ntop", args, 2, 2)?;
    let af = int_arg(args[0])?;
    let b = BufferView::get(args[1], PyBUF_SIMPLE)?;
    match af {
        libc::AF_INET => {
            if b.len() != 4 {
                raise!(ValueError, "invalid length of packed IP address string");
            }
        }
        libc::AF_INET6 => {
            if b.len() != 16 {
                raise!(ValueError, "invalid length of packed IP address string");
            }
        }
        _ => raise!(ValueError, "unknown address family {}", af),
    }
    let mut out = [0 as libc::c_char; 64];
    let r = unsafe { addr::inet_ntop(af, b.view.buf, out.as_mut_ptr(), out.len() as _) };
    if r.is_null() {
        return Err(os_error(errno()));
    }
    Ok(pystr::new_str(&unsafe { CStr::from_ptr(out.as_ptr()) }.to_string_lossy()))
}

fn f_getaddrinfo(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<6>("getaddrinfo", &["host", "port", "family", "type", "proto", "flags"], 0, 2, 0, args, kw)?;
    let hobj = a[0].unwrap();
    let pobj = a[1].unwrap();
    let host: Option<Vec<u8>> = if none::is_none(hobj) {
        None
    } else if pystr::is_str(hobj) {
        Some(addr::host_bytes(hobj)?)
    } else if bytes::is_bytes(hobj) {
        Some(bytes::bytes_data(hobj).to_vec())
    } else {
        raise!(TypeError, "getaddrinfo() argument 1 must be string or None");
    };
    let port: Option<Vec<u8>> = if none::is_none(pobj) {
        None
    } else if int::is_int(pobj) {
        // any int goes to the C library as its decimal text; range errors
        // are the resolver's business
        Some(crate::types::int_methods::to_decimal_string(pobj)?.into_bytes())
    } else if pystr::is_str(pobj) {
        let s = pystr::as_utf8(pobj)?;
        Some(s.as_bytes().to_vec())
    } else if bytes::is_bytes(pobj) {
        Some(bytes::bytes_data(pobj).to_vec())
    } else {
        raise!(OSError, "Int or String expected");
    };
    let family = match a[2] { Some(o) => int_arg(o)?, None => 0 };
    let socktype = match a[3] { Some(o) => int_arg(o)?, None => 0 };
    let proto = match a[4] { Some(o) => int_arg(o)?, None => 0 };
    let flags = match a[5] { Some(o) => int_arg(o)?, None => 0 };
    if let Some(h) = &host {
        if h.contains(&0) {
            raise!(ValueError, "embedded null character");
        }
    }
    if let Some(p) = &port {
        if p.contains(&0) {
            raise!(ValueError, "embedded null character");
        }
    }
    crate::modules::sys_full::audit(
        "socket.getaddrinfo",
        &[hobj, pobj, &int::new_int(family as i64), &int::new_int(socktype as i64), &int::new_int(proto as i64), &int::new_int(flags as i64)],
    )?;
    let res = addr::getaddrinfo_raw(host.as_deref(), port.as_deref(), family, socktype, proto, flags)?;
    let out = list::new_list_empty();
    for (fam, st, pr, canon, sa) in res {
        let addr = addr::makesockaddr(&sa, pr)?;
        list::append(
            &out,
            tuple::new_tuple(vec![
                int::new_int(fam as i64),
                int::new_int(st as i64),
                int::new_int(pr as i64),
                pystr::new_str(canon.as_deref().unwrap_or("")),
                addr,
            ]),
        );
    }
    Ok(out)
}

fn f_getnameinfo(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getnameinfo", args, 2, 2)?;
    let sa = args[0];
    let flags = int_arg(args[1])?;
    if !tuple::is_tuple(sa) {
        raise!(TypeError, "getnameinfo() argument 1 must be a tuple");
    }
    const M: &str = "getnameinfo(): illegal sockaddr argument";
    let t = tuple::tuple_items(sa);
    if t.len() < 2 || t.len() > 4 || !pystr::is_str(t[0]) {
        raise!(TypeError, "{}", M);
    }
    let host = pystr::as_utf8(t[0])?.to_string();
    if !int::is_int(t[1]) && !(ab::has_index(t[1]) && !crate::types::float::is_float(t[1])) {
        raise!(TypeError, "{}", M);
    }
    let port = int_arg(t[1])?;
    let uarg = |o: &PyObject| -> PyResult<u32> {
        if crate::types::float::is_float(o) || !ab::has_index(o) {
            raise!(TypeError, "{}", M);
        }
        let v = ab::index(o)?;
        Ok(int::as_i128(&v).unwrap_or(0) as u32)
    };
    let flowinfo = if t.len() > 2 { uarg(t[2])? } else { 0 };
    let scope_id = if t.len() > 3 { uarg(t[3])? } else { 0 };
    if flowinfo > 0xfffff {
        raise!(OverflowError, "getnameinfo(): flowinfo must be 0-1048575.");
    }
    crate::modules::sys_full::audit("socket.getnameinfo", &[sa])?;
    let pbuf = port.to_string();
    let res = addr::getaddrinfo_raw(Some(host.as_bytes()), Some(pbuf.as_bytes()), libc::AF_UNSPEC, libc::SOCK_DGRAM, 0, libc::AI_NUMERICHOST)?;
    if res.len() > 1 {
        raise!(OSError, "sockaddr resolved to multiple addresses");
    }
    let Some((fam, _, _, _, mut addr)) = res.into_iter().next() else {
        raise!(OSError, "sockaddr resolved to multiple addresses");
    };
    match fam {
        libc::AF_INET => {
            if t.len() != 2 {
                raise!(OSError, "IPv4 sockaddr must be 2 tuple");
            }
        }
        libc::AF_INET6 => unsafe {
            let s6 = &mut *(addr.as_mut_ptr() as *mut libc::sockaddr_in6);
            s6.sin6_flowinfo = flowinfo.to_be();
            s6.sin6_scope_id = scope_id;
        },
        _ => {}
    }
    let mut hbuf = [0 as libc::c_char; 1025];
    let mut sbuf = [0 as libc::c_char; 32];
    let ap = &addr as *const SockAddr as usize;
    let (hp, sp) = (hbuf.as_mut_ptr() as usize, sbuf.as_mut_ptr() as usize);
    let r = crate::threads::blocking(|| unsafe {
        let a = &*(ap as *const SockAddr);
        libc::getnameinfo(a.as_ptr(), a.len, hp as *mut libc::c_char, 1025, sp as *mut libc::c_char, 32, flags)
    });
    if r != 0 {
        return Err(addr::gaierror(r));
    }
    let h = unsafe { CStr::from_ptr(hbuf.as_ptr()) }.to_string_lossy().into_owned();
    let s = unsafe { CStr::from_ptr(sbuf.as_ptr()) }.to_string_lossy().into_owned();
    Ok(tuple::new_tuple(vec![pystr::new_str(&h), pystr::new_str(&s)]))
}

fn f_if_nameindex(_m: &PyObject) -> PyResult {
    let ni = unsafe { libc::if_nameindex() };
    if ni.is_null() {
        return Err(os_error(errno()));
    }
    let out = list::new_list_empty();
    let mut p = ni;
    let r = (|| -> PyResult<()> {
        unsafe {
            while (*p).if_index != 0 {
                let name = crate::modules::posix::path::fs_decode(CStr::from_ptr((*p).if_name).to_bytes())?;
                list::append(&out, tuple::new_tuple(vec![int::new_int((*p).if_index as i64), name]));
                p = p.add(1);
            }
        }
        Ok(())
    })();
    unsafe { libc::if_freenameindex(ni) };
    r?;
    Ok(out)
}

fn f_if_nametoindex(_m: &PyObject, a: &PyObject) -> PyResult {
    let name: Vec<u8> = if pystr::is_str(a) {
        crate::modules::posix::path::fs_encode_str(a)?
    } else if bytes::is_bytes(a) {
        bytes::bytes_data(a).to_vec()
    } else {
        let p = crate::modules::posix::path::fspath(a).map_err(|e| {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                crate::err!(TypeError, "expected str, bytes or os.PathLike object, not {}", tname(a))
            } else {
                e
            }
        })?;
        if pystr::is_str(&p) { crate::modules::posix::path::fs_encode_str(&p)? } else { bytes::bytes_data(&p).to_vec() }
    };
    let c = match CString::new(name) {
        Ok(c) => c,
        Err(_) => raise!(ValueError, "embedded null byte"),
    };
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if idx == 0 {
        let e = errno();
        if e == 0 {
            raise!(OSError, "no interface with this name");
        }
        return Err(os_error(e));
    }
    Ok(int::new_int(idx as i64))
}

fn f_if_indextoname(_m: &PyObject, a: &PyObject) -> PyResult {
    if !int::is_int(a) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(a));
    }
    if int::sign(a) < 0 {
        raise!(ValueError, "index must be non-negative");
    }
    let idx = match int::as_i128(a) {
        Some(v) if v <= u32::MAX as i128 => v as u32,
        _ => raise!(OverflowError, "index is too large"),
    };
    let mut buf = [0 as libc::c_char; libc::IFNAMSIZ];
    let r = unsafe { libc::if_indextoname(idx, buf.as_mut_ptr()) };
    if r.is_null() {
        return Err(os_error(errno()));
    }
    crate::modules::posix::path::fs_decode(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_bytes())
}

pub fn add_functions(d: &PyObject, m: &PyObject) -> PyResult<()> {
    crate::builtins::add_methods_to_dict(
        d,
        crate::mdefs![
            ("gethostname", NOARGS, f_gethostname, "gethostname() -> string\n\nReturn the current host name."),
            ("sethostname", O, f_sethostname, "sethostname(name)\n\nSets the hostname to name."),
            ("gethostbyname", O, f_gethostbyname, "gethostbyname(host) -> address\n\nReturn the IP address (a string of the form '255.255.255.255') for a host."),
            ("gethostbyname_ex", O, f_gethostbyname_ex, "gethostbyname_ex(host) -> (name, aliaslist, addresslist)\n\nReturn the true host name, a list of aliases, and a list of IP addresses,\nfor a host.  The host argument is a string giving a host name or IP number."),
            ("gethostbyaddr", O, f_gethostbyaddr, "gethostbyaddr(host) -> (name, aliaslist, addresslist)\n\nReturn the true host name, a list of aliases, and a list of IP addresses,\nfor a host.  The host argument is a string giving a host name or IP number."),
            ("getservbyname", FAST, f_getservbyname, "getservbyname(servicename[, protocolname]) -> integer"),
            ("getservbyport", FAST, f_getservbyport, "getservbyport(port[, protocolname]) -> string"),
            ("getprotobyname", O, f_getprotobyname, "getprotobyname(name) -> integer"),
            ("inet_aton", O, f_inet_aton, "inet_aton(string) -> bytes giving packed 32-bit IP representation"),
            ("inet_ntoa", O, f_inet_ntoa, "inet_ntoa($module, packed_ip, /)\n--\n\nConvert an IP address from 32-bit packed binary format to string format."),
            ("inet_pton", FAST, f_inet_pton, "inet_pton(af, ip) -> packed IP address string"),
            ("inet_ntop", FAST, f_inet_ntop, "inet_ntop(af, packed_ip) -> string formatted IP address"),
            ("getaddrinfo", FASTKW, f_getaddrinfo, "getaddrinfo(host, port [, family, type, proto, flags])\n    -> list of (family, type, proto, canonname, sockaddr)"),
            ("getnameinfo", FAST, f_getnameinfo, "getnameinfo(sockaddr, flags) --> (host, port)"),
            ("if_nameindex", NOARGS, f_if_nameindex, "if_nameindex()\n\nReturns a list of network interface information (index, name) tuples."),
            ("if_nametoindex", O, f_if_nametoindex, "if_nametoindex($module, oname, /)\n--\n\nReturns the interface index corresponding to the interface name if_name."),
            ("if_indextoname", O, f_if_indextoname, "if_indextoname($module, index, /)\n--\n\nReturns the interface name corresponding to the interface index if_index."),
        ],
        Some(m),
    )
}
