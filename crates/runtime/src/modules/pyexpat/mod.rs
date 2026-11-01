//! `_expat_engine`: the XML parsing engine behind `pyexpat`.
//!
//! The engine (engine.rs) turns input into a list of events; the Python
//! module Lib/pyexpat.py implements the xmlparser API (handlers, buffering,
//! attributes) on top of it.  Each call into the engine returns a list of
//! event tuples `(kind, line, column, byte_index, *payload)`.

pub mod engine;
pub mod xmlparser;

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use engine::{Engine, Ev, Event};
use std::sync::Mutex;

#[repr(C)]
struct EngineObj {
    ob_base: PyObject,
    eng: *mut Mutex<Engine>,
}

static mut ENGINE_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn engine_of(o: &PyObject) -> PyResult<&Mutex<Engine>> {
    unsafe {
        if o.type_ptr() != &raw mut ENGINE_TYPE {
            raise!(TypeError, "expected an _expat_engine.Engine");
        }
        let p = (*(o.as_ptr() as *const EngineObj)).eng;
        if p.is_null() {
            raise!(ValueError, "uninitialized engine");
        }
        Ok(&*p)
    }
}

unsafe extern "C" fn engine_dealloc(op: *mut PyObject) {
    unsafe {
        let e = op as *mut EngineObj;
        if !(*e).eng.is_null() {
            drop(Box::from_raw((*e).eng));
            (*e).eng = std::ptr::null_mut();
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn wrap(eng: Engine) -> PyResult {
    unsafe {
        let tp = &raw mut ENGINE_TYPE;
        let op = ((*tp).tp_alloc.unwrap())(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        (*(op as *mut EngineObj)).eng = Box::into_raw(Box::new(Mutex::new(eng)));
        Ok(PyRef::from_raw(op))
    }
}

fn opt_string(o: Option<&PyObject>) -> PyResult<Option<String>> {
    match o {
        None => Ok(None),
        Some(x) if x.is(none::none()) => Ok(None),
        Some(x) => {
            if !pystr::is_str(x) {
                raise!(TypeError, "expected str or None");
            }
            Ok(Some(pystr::as_utf8(x)?.to_string()))
        }
    }
}

fn ostr(s: &Option<String>) -> PyRef {
    match s {
        Some(s) => pystr::new_str(s),
        None => none::none_ref(),
    }
}

fn s(x: &str) -> PyRef {
    pystr::new_str(x)
}

fn event_tuple(e: Event) -> PyRef {
    let mut items: Vec<PyRef> = Vec::with_capacity(8);
    let kind: i64;
    let mut payload: Vec<PyRef> = Vec::new();
    match e.ev {
        Ev::XmlDecl { version, encoding, standalone } => {
            kind = 0;
            payload = vec![ostr(&version), ostr(&encoding), int::new_int(standalone as i64)];
        }
        Ev::StartDoctype { name, sysid, pubid, internal } => {
            kind = 1;
            payload = vec![s(&name), ostr(&sysid), ostr(&pubid), int::new_int(internal as i64)];
        }
        Ev::EndDoctype => kind = 2,
        Ev::ElementDecl { name, model } => {
            kind = 3;
            let nodes: Vec<PyRef> = model
                .into_iter()
                .map(|n| {
                    let children: Vec<PyRef> = n.children.iter().map(|&c| int::new_int(c as i64)).collect();
                    tuple::new_tuple(vec![
                        int::new_int(n.ctype as i64),
                        int::new_int(n.quant as i64),
                        ostr(&n.name),
                        tuple::new_tuple(children),
                    ])
                })
                .collect();
            payload = vec![s(&name), list::new_list(nodes)];
        }
        Ev::AttlistDecl { elname, attname, atype, default, required } => {
            kind = 4;
            payload = vec![s(&elname), s(&attname), s(&atype), ostr(&default), int::new_int(required as i64)];
        }
        Ev::EntityDecl { name, is_pe, value, base, sysid, pubid, notation } => {
            kind = 5;
            payload = vec![
                s(&name),
                int::new_int(is_pe as i64),
                ostr(&value),
                ostr(&base),
                ostr(&sysid),
                ostr(&pubid),
                ostr(&notation),
            ];
        }
        Ev::NotationDecl { name, base, sysid, pubid } => {
            kind = 6;
            payload = vec![s(&name), ostr(&base), ostr(&sysid), ostr(&pubid)];
        }
        Ev::StartElement { name, attrs, nspecified } => {
            kind = 7;
            let mut flat = Vec::with_capacity(attrs.len() * 2);
            for (n, v) in attrs {
                flat.push(s(&n));
                flat.push(s(&v));
            }
            payload = vec![s(&name), list::new_list(flat), int::new_int(nspecified as i64)];
        }
        Ev::EndElement { name } => {
            kind = 8;
            payload = vec![s(&name)];
        }
        Ev::StartNs { prefix, uri } => {
            kind = 9;
            payload = vec![ostr(&prefix), ostr(&uri)];
        }
        Ev::EndNs { prefix } => {
            kind = 10;
            payload = vec![ostr(&prefix)];
        }
        Ev::CharData(t) => {
            kind = 11;
            payload = vec![s(&t)];
        }
        Ev::Pi { target, data } => {
            kind = 12;
            payload = vec![s(&target), s(&data)];
        }
        Ev::Comment(t) => {
            kind = 13;
            payload = vec![s(&t)];
        }
        Ev::StartCdata => kind = 14,
        Ev::EndCdata => kind = 15,
        Ev::Default(t) => {
            kind = 16;
            payload = vec![s(&t)];
        }
        Ev::SkippedEntity { name, is_pe } => {
            kind = 17;
            payload = vec![s(&name), int::new_int(is_pe as i64)];
        }
        Ev::ExternalEntityRef { context, base, sysid, pubid } => {
            kind = 18;
            payload = vec![ostr(&context), ostr(&base), ostr(&sysid), ostr(&pubid)];
        }
        Ev::NotStandalone => kind = 19,
        Ev::NeedEncoding(name) => {
            kind = 20;
            payload = vec![s(&name)];
        }
        Ev::Error(code) => {
            kind = 21;
            payload = vec![int::new_int(code as i64)];
        }
    }
    items.push(int::new_int(kind));
    items.push(int::new_int(e.line as i64));
    items.push(int::new_int(e.col as i64));
    items.push(int::new_int(e.byte));
    items.extend(payload);
    tuple::new_tuple(items)
}

fn events_list(evs: Vec<Event>) -> PyRef {
    list::new_list(evs.into_iter().map(event_tuple).collect())
}

fn new_engine(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("Engine", &["namespace_separator", "encoding"], 0, 0);
    let [sep, enc] = crate::args::parse::<2>(&P, args, kw)?;
    wrap(Engine::new(opt_string(sep)?, opt_string(enc)?))
}

fn eng_feed(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("feed", &["data", "final"], 0, 2);
    let [data, fin] = crate::args::parse::<2>(&P, args, kw)?;
    let data = data.unwrap();
    let bytes = match crate::buffer::buffer_bytes(data) {
        Some(b) => b,
        None => raise!(TypeError, "a bytes-like object is required"),
    };
    let fin = crate::abstract_::is_true(fin.unwrap())?;
    let m = engine_of(o)?;
    let evs = m.lock().unwrap().feed(&bytes, fin);
    Ok(events_list(evs))
}

fn eng_set_table(o: &PyObject, table: &PyObject) -> PyResult {
    if !pystr::is_str(table) || pystr::str_len(table) != 256 {
        raise!(ValueError, "encoding table must be a str of 256 characters");
    }
    let s = pystr::to_string_lossy(table);
    let t: Vec<u32> = s.chars().map(|c| if c == '\u{FFFD}' { u32::MAX } else { c as u32 }).collect();
    let evs = engine_of(o)?.lock().unwrap().set_encoding_table(t);
    Ok(events_list(evs))
}

fn eng_encoding_failed(o: &PyObject) -> PyResult {
    engine_of(o)?.lock().unwrap().encoding_failed();
    Ok(none::none_ref())
}

fn eng_external(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("external", &["context", "encoding"], 0, 1);
    let [ctx, enc] = crate::args::parse::<2>(&P, args, kw)?;
    let ctx = opt_string(ctx)?;
    let enc = opt_string(enc)?;
    let e = {
        let g = engine_of(o)?.lock().unwrap();
        Engine::new_external(&g, ctx, enc)
    };
    wrap(e)
}

fn eng_position(o: &PyObject) -> PyResult {
    let (l, c, b) = engine_of(o)?.lock().unwrap().current_position();
    Ok(tuple::new_tuple(vec![int::new_int(l as i64), int::new_int(c as i64), int::new_int(b)]))
}

fn eng_force_encoding(o: &PyObject, name: &PyObject) -> PyResult {
    let n = pystr::as_utf8(name)?.to_string();
    engine_of(o)?.lock().unwrap().force_encoding(n);
    Ok(none::none_ref())
}

fn eng_started(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(engine_of(o)?.lock().unwrap().is_started()))
}

fn eng_configure(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new(
        "configure",
        &[
            "ns_triplets",
            "param_entity_parsing",
            "use_foreign_dtd",
            "reparse_deferral",
            "base",
            "ampl_threshold",
            "ampl_max",
        ],
        0,
        0,
    );
    let [trip, pep, foreign, defer, base, thr, mx] = crate::args::parse::<7>(&P, args, kw)?;
    let m = engine_of(o)?;
    let mut g = m.lock().unwrap();
    if let Some(x) = trip {
        g.ns_triplets = crate::abstract_::is_true(x)?;
    }
    if let Some(x) = pep {
        g.param_entity_parsing = crate::abstract_::as_i64(x)? as u8;
    }
    if let Some(x) = foreign {
        g.use_foreign_dtd = crate::abstract_::is_true(x)?;
    }
    if let Some(x) = defer {
        g.reparse_deferral = crate::abstract_::is_true(x)?;
    }
    if let Some(x) = base {
        g.base = opt_string(Some(x))?;
    }
    if let Some(x) = thr {
        g.ampl_threshold = crate::abstract_::as_i64(x)? as u64;
    }
    if let Some(x) = mx {
        g.ampl_max = crate::abstract_::float_value(x)?;
    }
    Ok(none::none_ref())
}

pub fn init() -> PyResult {
    unsafe {
        if ENGINE_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            let t = &mut *(&raw mut ENGINE_TYPE);
            t.tp_name = c"_expat_engine.Engine".as_ptr();
            t.tp_basicsize = std::mem::size_of::<EngineObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT;
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
            t.tp_dealloc = Some(engine_dealloc);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_methods = crate::mdefs![
                ("feed", FASTKW, eng_feed),
                ("set_encoding_table", O, eng_set_table),
                ("encoding_failed", NOARGS, eng_encoding_failed),
                ("external", FASTKW, eng_external),
                ("position", NOARGS, eng_position),
                ("started", NOARGS, eng_started),
                ("force_encoding", O, eng_force_encoding),
                ("configure", FASTKW, eng_configure),
            ];
            crate::types::typeobject::type_ready(&raw mut ENGINE_TYPE)?;
        }
    }
    let m = crate::types::module::new_module("_expat_engine")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "Engine", crate::types::typeobject::type_obj(&raw mut ENGINE_TYPE))?;
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("new_engine", FASTKW, new_engine)], Some(&m))?;
    let names = list::new_list(engine::ERROR_NAMES.iter().map(|x| s(x)).collect());
    let msgs = list::new_list(engine::ERROR_MESSAGES.iter().map(|x| s(x)).collect());
    dict::set_item_str(&d, "ERROR_NAMES", &names)?;
    dict::set_item_str(&d, "ERROR_MESSAGES", &msgs)?;
    Ok(m)
}
