//! Built-in traceback printing (used when the `traceback` module is unavailable,
//! and by the default sys.excepthook before it is replaced).
use crate::abstract_ as ab;
use crate::ffi::PyObject;
use crate::object::PyRef;
use crate::types::{exceptions as exc, none, str as pystr};

fn format_one(e: &PyObject, out: &mut String) {
    if let Some(tb) = exc::exc_traceback(e) {
        out.push_str("Traceback (most recent call last):\n");
        let mut cur: Option<PyRef> = Some(tb);
        while let Some(t) = cur {
            let tbo = unsafe { &*(t.as_ptr() as *const crate::types::traceback::PyTracebackObject) };
            let fo = unsafe { &*tbo.tb_frame };
            if let Some(f) = crate::types::frameobject::frame_of(fo) {
                let cd = crate::types::code::code_data(f.code());
                let fname = pystr::to_string_lossy(&cd.filename);
                let name = pystr::to_string_lossy(&cd.co_name);
                out.push_str(&format!("  File \"{}\", line {}, in {}\n", fname, tbo.tb_lineno, name));
                if let Some(line) = crate::modules::sys_full::source_line(&fname, tbo.tb_lineno) {
                    let l = line.trim();
                    if !l.is_empty() {
                        out.push_str(&format!("    {}\n", l));
                    }
                }
            }
            cur = if tbo.tb_next.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(tbo.tb_next) }) };
        }
    }
    let tname = crate::types::typeobject::type_fqname(e.type_ptr());
    let msg = match ab::str(e) {
        Ok(s) => pystr::to_string_lossy(&s),
        Err(_) => {
            crate::errors::clear_exc();
            "<exception str() failed>".into()
        }
    };
    if crate::types::typeobject::is_subtype(e.type_ptr(), exc::SyntaxError()) {
        let se = unsafe { &*(e.as_ptr() as *const exc::SyntaxErrorExc) };
        let f = if se.filename.is_null() { "<unknown>".to_string() } else { ab::str_string(unsafe { &*se.filename }).unwrap_or_default() };
        let l = if se.lineno.is_null() { String::new() } else { ab::str_string(unsafe { &*se.lineno }).unwrap_or_default() };
        out.push_str(&format!("  File \"{}\", line {}\n", f, l));
        if !se.text.is_null() && !unsafe { &*se.text }.is(none::none()) {
            out.push_str(&format!("    {}\n", pystr::to_string_lossy(unsafe { &*se.text }).trim_end()));
        }
        let m = if se.msg.is_null() { String::new() } else { ab::str_string(unsafe { &*se.msg }).unwrap_or_default() };
        out.push_str(&format!("{}: {}\n", tname, m));
        return;
    }
    if msg.is_empty() {
        out.push_str(&format!("{}\n", tname));
    } else {
        out.push_str(&format!("{}: {}\n", tname, msg));
    }
}

pub fn format_exception(e: &PyObject) -> String {
    let mut chain: Vec<(PyRef, &'static str)> = Vec::new();
    let mut cur = e.to_ref();
    let mut seen: Vec<usize> = Vec::new();
    loop {
        seen.push(cur.as_ptr() as usize);
        let be = exc::exc(&cur);
        let (next, sep) = if !be.cause.is_null() {
            (Some(unsafe { PyRef::from_borrowed(be.cause) }), "\nThe above exception was the direct cause of the following exception:\n\n")
        } else if !be.context.is_null() && be.suppress_context == 0 {
            (Some(unsafe { PyRef::from_borrowed(be.context) }), "\nDuring handling of the above exception, another exception occurred:\n\n")
        } else {
            (None, "")
        };
        chain.push((cur.clone(), sep));
        match next {
            Some(n) if !seen.contains(&(n.as_ptr() as usize)) => cur = n,
            _ => break,
        }
    }
    let mut out = String::new();
    for i in (0..chain.len()).rev() {
        format_one(&chain[i].0, &mut out);
        if i > 0 {
            out.push_str(chain[i - 1].1);
        }
    }
    out
}

pub fn print_exception(e: &PyObject) {
    let s = format_exception(e);
    crate::modules::sys::write_stderr(&s);
}
