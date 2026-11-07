//! Cross-interpreter data: how a value of one interpreter becomes an equal (but
//! distinct) value of another.
//!
//! A value is first turned into an `Xi` while its own interpreter is current.
//! An `Xi` holds no reference into that interpreter's object graph (except for
//! buffers, whose memory is shared on purpose, and which are kept alive by the
//! `Xi`).  It is then turned back into objects with the target interpreter
//! current.
//!
//! Kinds, in the order they are tried:
//! * plain immutable values (None, bool, int, float, str, bytes and tuples of
//!   them): copied through the marshal format;
//! * buffers (memoryview): a new view on the same memory;
//! * queues and channel ends: re-created from their id in the target, as an
//!   instance of the type the target registered for them;
//! * (with fallback) stateless functions: their code is copied;
//! * (with fallback) anything picklable: pickled.  An object pickled by
//!   reference to `__main__` is found in the target by running the source
//!   interpreter's main script there under the name `<fake __main__>`.

use super::module::not_shareable_type;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{bool_, bytes, dict, float, int, list, module as pymodule, none, str as pystr, tuple};

/// How hard to try making an object shareable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Only the natively shareable kinds.
    Default,
    /// Also stateless functions and pickle.
    Fallback,
}

pub enum Xi {
    /// marshal data of a plain immutable value
    Plain(Vec<u8>),
    Tuple(Vec<Xi>),
    /// A memoryview's exporter; the target gets memoryview(exporter).
    Buffer(PyRef),
    /// marshal data of anything marshal supports (code included)
    Marshal(Vec<u8>),
    /// marshal data of (code, name, qualname)
    Func(Vec<u8>),
    Pickled { data: Vec<u8>, main_file: Option<String> },
    Queue(i64),
    ChannelEnd { id: i64, send: bool },
    ChannelId { id: i64, end: i32 },
}

// --------------------------------------------------------------------- errors

/// An exception, reduced to text so that it can be re-raised in another
/// interpreter.
#[derive(Clone, Debug, Default)]
pub struct Captured {
    pub name: String,
    pub qualname: String,
    pub module: Option<String>,
    pub msg: Option<String>,
    pub formatted: String,
    pub errdisplay: Option<String>,
    /// pickle of the exception object (when asked for)
    pub pickled: Option<Vec<u8>>,
    pub cause: Option<Box<Captured>>,
}

fn str_attr(o: &PyObject, name: &str) -> Option<String> {
    match ab::getattr_opt(o, &pystr::new_str(name)) {
        Ok(Some(v)) if pystr::is_str(&v) => Some(pystr::to_string_lossy(&v)),
        Ok(_) => None,
        Err(_) => {
            clear_exc();
            None
        }
    }
}

impl Captured {
    /// Describe exception `e` (of the current interpreter).
    pub fn from_exc(e: &PyObject, with_display: bool, with_pickle: bool) -> Captured {
        let saved = take_exc();
        let tp = crate::types::typeobject::type_ref(e.type_ptr());
        let name = str_attr(&tp, "__name__").unwrap_or_else(|| "?".into());
        let qualname = str_attr(&tp, "__qualname__").unwrap_or_else(|| name.clone());
        let module = str_attr(&tp, "__module__");
        let msg = match ab::str_string(e) {
            Ok(s) => Some(s),
            Err(_) => {
                clear_exc();
                None
            }
        };
        let shown = match module.as_deref() {
            Some("builtins") | Some("__main__") | None => qualname.clone(),
            Some(m) => format!("{}.{}", m, qualname),
        };
        let formatted = match &msg {
            Some(m) if !m.is_empty() => format!("{}: {}", shown, m),
            Some(_) => shown,
            None => format!("{}: <exception str() failed>", shown),
        };
        let errdisplay = if with_display { format_traceback(e) } else { None };
        let pickled = if with_pickle { pickle_dumps(e).ok() } else { None };
        clear_exc();
        let cause = match ab::getattr_opt(e, &pystr::new_str("__cause__")) {
            Ok(Some(c)) if !c.is(none::none()) && !c.is(e) => Some(Box::new(Captured::from_exc(&c, false, false))),
            Ok(_) => None,
            Err(_) => {
                clear_exc();
                None
            }
        };
        if let Some(s) = saved {
            set_exc(s);
        }
        Captured { name, qualname, module, msg, formatted, errdisplay, pickled, cause }
    }

    /// Take the pending exception of the current interpreter.
    pub fn take_pending(with_display: bool, with_pickle: bool) -> Captured {
        match take_exc() {
            Some(e) => Captured::from_exc(&e, with_display, with_pickle),
            None => Captured { name: "SystemError".into(), qualname: "SystemError".into(), module: Some("builtins".into()), msg: Some("error without exception".into()), formatted: "SystemError: error without exception".into(), ..Default::default() },
        }
    }

    /// The excinfo namespace `_interpreters` hands out.
    pub fn to_namespace(&self) -> PyResult {
        let opt = |s: &Option<String>| s.as_deref().map(pystr::new_str).unwrap_or_else(none::none_ref);
        let t = namespace(&[
            ("__name__", pystr::new_str(&self.name)),
            ("__qualname__", pystr::new_str(&self.qualname)),
            ("__module__", opt(&self.module)),
        ])?;
        namespace(&[
            ("type", t),
            ("msg", opt(&self.msg)),
            ("formatted", pystr::new_str(&self.formatted)),
            ("errdisplay", opt(&self.errdisplay)),
        ])
    }

    /// Re-create the exception in the current interpreter: builtin exception
    /// types are rebuilt as themselves, anything else as a RuntimeError naming it.
    pub fn to_exception(&self) -> PyRef {
        let tp = if self.module.as_deref() == Some("builtins") {
            crate::builtins::builtins_dict()
        } else {
            dict::new_dict()
        };
        let exc_type = match dict::get_item_rs(&tp, &self.name) {
            Some(t) if crate::types::typeobject::is_type(&t) => Some(t),
            _ => None,
        };
        let not_shareable = self.name == "NotShareableError";
        let made = if not_shareable {
            ab::call(crate::types::typeobject::type_obj(not_shareable_type()), &[&pystr::new_str(self.msg.as_deref().unwrap_or(""))])
        } else if let Some(t) = exc_type {
            ab::call(&t, &[&pystr::new_str(self.msg.as_deref().unwrap_or(""))])
        } else {
            ab::call(crate::types::typeobject::type_obj(crate::types::exceptions::RuntimeError()), &[&pystr::new_str(&self.formatted)])
        };
        let e = match made {
            Ok(e) => e,
            Err(_) => {
                clear_exc();
                crate::types::exceptions::make_exception(crate::types::exceptions::RuntimeError(), &[pystr::new_str(&self.formatted)]).unwrap()
            }
        };
        if let Some(c) = &self.cause {
            let ce = c.to_exception();
            if ab::setattr(&e, &pystr::new_str("__cause__"), &ce).is_err() {
                clear_exc();
            }
        }
        e
    }

    /// Raise it (re-created) in the current interpreter.
    pub fn raise(&self) -> PyErr {
        set_exc(self.to_exception());
        PyErr
    }
}

/// NotShareableError(msg), optionally caused by the pending exception.
pub fn not_shareable(msg: &str, chain: bool) -> PyErr {
    let cause = if chain { take_exc() } else { None };
    let e = match ab::call(crate::types::typeobject::type_obj(not_shareable_type()), &[&pystr::new_str(msg)]) {
        Ok(e) => e,
        Err(_) => return PyErr,
    };
    if let Some(c) = cause {
        if ab::setattr(&e, &pystr::new_str("__cause__"), &c).is_err() {
            clear_exc();
        }
    }
    set_exc(e);
    PyErr
}

/// NotShareableError(msg) caused by a captured error of another interpreter.
pub fn not_shareable_from(msg: &str, cause: &Captured) -> PyErr {
    let c = cause.to_exception();
    let e = match ab::call(crate::types::typeobject::type_obj(not_shareable_type()), &[&pystr::new_str(msg)]) {
        Ok(e) => e,
        Err(_) => return PyErr,
    };
    if ab::setattr(&e, &pystr::new_str("__cause__"), &c).is_err() {
        clear_exc();
    }
    set_exc(e);
    PyErr
}

fn format_traceback(e: &PyObject) -> Option<String> {
    let r = (|| -> PyResult<String> {
        let tb = crate::import::import_absolute("traceback")?;
        let f = ab::getattr_str(&tb, "format_exception")?;
        let kw = tuple::new_tuple(vec![pystr::new_str("colorize")]);
        let lines = match ab::call_kw(&f, &[e, bool_::false_obj()], Some(&kw)) {
            Ok(l) => l,
            Err(_) => {
                clear_exc();
                ab::call(&f, &[e])?
            }
        };
        let mut s = String::new();
        for l in ab::collect_iterable(&lines)? {
            s.push_str(&pystr::to_string_lossy(&l));
        }
        Ok(s)
    })();
    match r {
        Ok(s) => Some(s),
        Err(_) => {
            clear_exc();
            None
        }
    }
}

pub fn namespace(items: &[(&str, PyRef)]) -> PyResult {
    let d = dict::new_dict();
    for (k, v) in items {
        dict::set_item_str(&d, k, v)?;
    }
    crate::types::namespace::new_namespace(Some(&d))
}

// --------------------------------------------------------------------- conversion

fn is_plain(o: &PyObject) -> bool {
    o.is(none::none())
        || o.is(bool_::true_obj())
        || o.is(bool_::false_obj())
        || (int::is_int_exact(o) && int::as_i64(o).is_some())
        || float::is_float_exact(o)
        || pystr::is_str_exact(o)
        || bytes::is_bytes_exact(o)
}

/// Turn `o` (of the current interpreter) into cross-interpreter data.
pub fn get(o: &PyObject, mode: Mode) -> PyResult<Xi> {
    match get_native(o)? {
        Some(x) => Ok(x),
        None if mode == Mode::Default => Err(not_shareable(&format!("{} does not support cross-interpreter data", type_desc(o)), false)),
        None => get_fallback(o),
    }
}

fn type_desc(o: &PyObject) -> String {
    format!("'{}' object", crate::types::typeobject::tname(o))
}

/// The natively shareable kinds; Ok(None) if `o` is none of them.
fn get_native(o: &PyObject) -> PyResult<Option<Xi>> {
    if is_plain(o) {
        return Ok(Some(Xi::Plain(crate::modules::marshal::dumps_vec(o, 4, false)?)));
    }
    if tuple::is_tuple_exact(o) {
        let mut items = Vec::new();
        let mut all_plain = true;
        for it in tuple::tuple_items(o) {
            match get_native(it)? {
                Some(x) => {
                    all_plain &= matches!(x, Xi::Plain(_));
                    items.push(x);
                }
                None => return Ok(None),
            }
        }
        if all_plain {
            return Ok(Some(Xi::Plain(crate::modules::marshal::dumps_vec(o, 4, false)?)));
        }
        return Ok(Some(Xi::Tuple(items)));
    }
    if crate::types::memoryview::is_memoryview(o) {
        return Ok(Some(Xi::Buffer(o.to_ref())));
    }
    if let Some(id) = super::queues::queue_id_of(o)? {
        return Ok(Some(Xi::Queue(id)));
    }
    if let Some((id, send)) = super::channels::end_id_of(o)? {
        return Ok(Some(Xi::ChannelEnd { id, send }));
    }
    if let Some((id, end)) = super::channels::channel_id_of(o) {
        return Ok(Some(Xi::ChannelId { id, end }));
    }
    Ok(None)
}

fn get_fallback(o: &PyObject) -> PyResult<Xi> {
    if crate::types::funcobject::is_function(o) && is_stateless_function(o)? {
        let code = ab::getattr_str(o, "__code__")?;
        let name = ab::getattr_str(o, "__name__")?;
        let qualname = ab::getattr_str(o, "__qualname__")?;
        let t = tuple::new_tuple(vec![code, name, qualname]);
        return Ok(Xi::Func(crate::modules::marshal::dumps_vec(&t, 4, true)?));
    }
    pickled(o)
}

/// Pickle `o` (NotShareableError if that fails).
pub fn pickled(o: &PyObject) -> PyResult<Xi> {
    let data = match pickle_dumps(o) {
        Ok(d) => d,
        Err(_) => return Err(not_shareable("object could not be pickled", true)),
    };
    let main_file = if contains(&data, b"__main__") { main_script_file() } else { None };
    Ok(Xi::Pickled { data, main_file })
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn pickle_dumps(o: &PyObject) -> PyResult<Vec<u8>> {
    let pickle = crate::import::import_absolute("pickle")?;
    let f = ab::getattr_str(&pickle, "dumps")?;
    let r = ab::call(&f, &[o])?;
    if !bytes::is_bytes(&r) {
        raise!(TypeError, "pickle.dumps() did not return bytes");
    }
    Ok(bytes::bytes_data(&r).to_vec())
}

fn pickle_loads(data: &[u8]) -> PyResult {
    let pickle = crate::import::import_absolute("pickle")?;
    let f = ab::getattr_str(&pickle, "loads")?;
    ab::call(&f, &[&bytes::new_bytes(data)])
}

/// The file the current interpreter's `__main__` was run from, if it is a script.
fn main_script_file() -> Option<String> {
    let m = crate::import::get_loaded_module("__main__")?;
    let d = pymodule::module_dict(&m);
    // a module run with -m is found by its real name
    if dict::get_item_rs(&d, "__spec__").is_some_and(|s| !s.is(none::none())) {
        return None;
    }
    let f = dict::get_item_rs(&d, "__file__")?;
    if !pystr::is_str(&f) {
        return None;
    }
    let s = pystr::to_string_lossy(&f);
    if std::path::Path::new(&s).is_file() { Some(s) } else { None }
}

impl Xi {
    /// Build the value in the current interpreter.
    pub fn to_object(&self) -> PyResult {
        match self {
            Xi::Plain(d) => crate::modules::marshal::loads_slice(d, false),
            Xi::Marshal(d) => crate::modules::marshal::loads_slice(d, true),
            Xi::Tuple(items) => {
                let mut v = Vec::with_capacity(items.len());
                for it in items {
                    v.push(it.to_object()?);
                }
                Ok(tuple::new_tuple(v))
            }
            Xi::Buffer(src) => {
                let mv = crate::types::typeobject::type_obj(crate::types::memoryview::memoryview_type());
                ab::call(mv, &[src])
            }
            Xi::Func(d) => {
                let t = crate::modules::marshal::loads_slice(d, true)?;
                let items = tuple::tuple_items(&t);
                let main = super::main_module()?;
                let g = pymodule::module_dict(&main);
                let f = crate::types::funcobject::new_function(items[0], &g, Some(items[2]))?;
                ab::setattr(&f, &pystr::new_str("__name__"), items[1])?;
                Ok(f)
            }
            Xi::Pickled { data, main_file } => match unpickle(data, main_file.as_deref()) {
                Ok(o) => Ok(o),
                Err(_) => Err(not_shareable("object could not be unpickled", true)),
            },
            Xi::Queue(id) => super::queues::queue_object(*id),
            Xi::ChannelEnd { id, send } => super::channels::end_object(*id, *send),
            Xi::ChannelId { id, end } => super::channels::channel_id_object(*id, *end),
        }
    }
}

/// Unpickle with `sys.modules['__main__']` temporarily replaced by the source
/// interpreter's main script, loaded (once per interpreter) as `<fake __main__>`.
fn unpickle(data: &[u8], main_file: Option<&str>) -> PyResult {
    let Some(file) = main_file else { return pickle_loads(data) };
    let fake = fake_main(file)?;
    let md = crate::modules::sys::modules_dict();
    let real = dict::get_item_rs(&md, "__main__");
    dict::set_item_str(&md, "__main__", &fake)?;
    let r = pickle_loads(data);
    let saved = take_exc();
    match real {
        Some(m) => {
            let _ = dict::set_item_str(&md, "__main__", &m);
        }
        None => {
            let _ = dict::del_item(&md, &pystr::new_str("__main__"));
        }
    }
    clear_exc();
    if let Some(e) = saved {
        set_exc(e);
    }
    r
}

const FAKE_MAIN: &str = "<fake __main__>";

fn fake_main(file: &str) -> PyResult {
    let md = crate::modules::sys::modules_dict();
    let key = format!("{} {}", FAKE_MAIN, file);
    if let Some(m) = dict::get_item_rs(&md, &key) {
        return Ok(m);
    }
    let m = pymodule::new_module(FAKE_MAIN)?;
    let d = pymodule::module_dict(&m);
    dict::set_item_str(&d, "__file__", &pystr::new_str(file))?;
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    let src = match std::fs::read(file) {
        Ok(s) => s,
        Err(e) => return Err(crate::types::exceptions::os_error_from_errno(e.raw_os_error().unwrap_or(0), Some(&pystr::new_str(file)), None)),
    };
    let code = crate::compile::compile_bytes(&src, file, crate::compile::Mode::Exec, 0, -1)?;
    // registered first so that the script can import itself under that name
    dict::set_item_str(&md, &key, &m)?;
    let md2 = md.clone();
    let real = dict::get_item_rs(&md2, "__main__");
    dict::set_item_str(&md2, "__main__", &m)?;
    let r = crate::interp::eval_code(&code.code, &d, Some(&d));
    match real {
        Some(x) => {
            let _ = dict::set_item_str(&md2, "__main__", &x);
        }
        None => {}
    }
    if r.is_err() {
        let saved = take_exc();
        let _ = dict::del_item(&md, &pystr::new_str(&key));
        clear_exc();
        if let Some(e) = saved {
            set_exc(e);
        }
        return Err(PyErr);
    }
    Ok(m)
}

// --------------------------------------------------------------------- code analysis

const OP_CACHE: u8 = 0;
const OP_RETURN_VALUE: u8 = 33;
const OP_DELETE_GLOBAL: u8 = 62;
const OP_DELETE_NAME: u8 = 63;
const OP_EXTENDED_ARG: u8 = 67;
const OP_LOAD_CONST: u8 = 81;
const OP_LOAD_COMMON_CONSTANT: u8 = 80;
/// LOAD_COMMON_CONSTANT operand for None
const COMMON_CONST_NONE: u32 = 7;
const OP_LOAD_FROM_DICT_OR_GLOBALS: u8 = 90;
const OP_LOAD_GLOBAL: u8 = 91;
const OP_LOAD_NAME: u8 = 92;
const OP_STORE_GLOBAL: u8 = 114;
const OP_STORE_NAME: u8 = 115;

/// (opcode, full argument) of every instruction (inline caches skipped).
fn instructions(code: &PyObject) -> PyResult<Vec<(u8, u32)>> {
    let co = ab::getattr_str(code, "co_code")?;
    let b = bytes::bytes_data(&co);
    let mut out = Vec::new();
    let mut ext: u32 = 0;
    let mut i = 0;
    while i + 1 < b.len() {
        let (op, arg) = (b[i], b[i + 1] as u32);
        i += 2;
        if op == OP_CACHE {
            continue;
        }
        let full = (ext << 8) | arg;
        if op == OP_EXTENDED_ARG {
            ext = full;
            continue;
        }
        ext = 0;
        out.push((op, full));
    }
    Ok(out)
}

/// Names a code object (and the code objects nested in it) looks up as globals.
fn global_names(code: &PyObject, out: &mut Vec<String>) -> PyResult<()> {
    let names = ab::getattr_str(code, "co_names")?;
    let names = tuple::tuple_items(&names);
    for (op, arg) in instructions(code)? {
        let idx = match op {
            OP_LOAD_GLOBAL => (arg >> 1) as usize,
            OP_STORE_GLOBAL | OP_DELETE_GLOBAL | OP_LOAD_NAME | OP_STORE_NAME | OP_DELETE_NAME | OP_LOAD_FROM_DICT_OR_GLOBALS => arg as usize,
            _ => continue,
        };
        if let Some(n) = names.get(idx) {
            out.push(pystr::to_string_lossy(n));
        }
    }
    let consts = ab::getattr_str(code, "co_consts")?;
    for c in tuple::tuple_items(&consts) {
        if crate::types::code::is_code(c) {
            global_names(c, out)?;
        }
    }
    Ok(())
}

/// No free variables, no defaults, and only builtins looked up as globals.
pub fn is_stateless_function(f: &PyObject) -> PyResult<bool> {
    for attr in ["__closure__", "__defaults__", "__kwdefaults__"] {
        let v = ab::getattr_str(f, attr)?;
        if !v.is(none::none()) {
            return Ok(false);
        }
    }
    let code = ab::getattr_str(f, "__code__")?;
    let free = ab::getattr_str(&code, "co_freevars")?;
    if tuple::tuple_len(&free) != 0 {
        return Ok(false);
    }
    let mut names = Vec::new();
    global_names(&code, &mut names)?;
    let b = crate::builtins::builtins_dict();
    Ok(names.iter().all(|n| is_builtin_name(&b, n)))
}

/// A global name that resolves to a builtin wherever the code runs (dunder
/// names such as `__name__` are module attributes first).
fn is_builtin_name(builtins: &PyObject, n: &str) -> bool {
    !(n.starts_with("__") && n.ends_with("__")) && dict::get_item_rs(builtins, n).is_some()
}

/// Can `code` run as a script: no parameters, no free variables, and no return
/// value other than None?
pub fn check_script_code(code: &PyObject, what: &str) -> PyResult<()> {
    let argc = int::as_i64(&*ab::getattr_str(code, "co_argcount")?).unwrap_or(0)
        + int::as_i64(&*ab::getattr_str(code, "co_posonlyargcount")?).unwrap_or(0)
        + int::as_i64(&*ab::getattr_str(code, "co_kwonlyargcount")?).unwrap_or(0);
    let flags = int::as_i64(&*ab::getattr_str(code, "co_flags")?).unwrap_or(0);
    const CO_VARARGS: i64 = 0x4;
    const CO_VARKEYWORDS: i64 = 0x8;
    if argc != 0 || flags & (CO_VARARGS | CO_VARKEYWORDS) != 0 {
        raise!(ValueError, "{} must not take any arguments", what);
    }
    let free = ab::getattr_str(code, "co_freevars")?;
    if tuple::tuple_len(&free) != 0 {
        raise!(ValueError, "{} must not be a closure", what);
    }
    let consts = ab::getattr_str(code, "co_consts")?;
    let consts = tuple::tuple_items(&consts);
    let mut prev: Option<(u8, u32)> = None;
    for (op, arg) in instructions(code)? {
        if op == OP_RETURN_VALUE {
            let ok = matches!(prev, Some((OP_LOAD_CONST, k)) if consts.get(k as usize).is_some_and(|c| c.is(none::none())))
                || matches!(prev, Some((OP_LOAD_COMMON_CONSTANT, COMMON_CONST_NONE)));
            if !ok {
                raise!(ValueError, "{} must not return a value", what);
            }
        }
        prev = Some((op, arg));
    }
    Ok(())
}

/// The data `_interpreters.exec` & co. send: a script as a code object.
pub fn script_code(o: &PyObject, what: &str) -> PyResult<Vec<u8>> {
    let code = if pystr::is_str(o) {
        let s = pystr::as_utf8(o)?;
        crate::compile::compile_str(s, "<string>", crate::compile::Mode::Exec, 0, -1)?.code
    } else if crate::types::code::is_code(o) {
        check_script_code(o, what)?;
        o.to_ref()
    } else if crate::types::funcobject::is_function(o) {
        let c = ab::getattr_str(o, "__code__")?;
        for attr in ["__closure__", "__defaults__", "__kwdefaults__"] {
            let v = ab::getattr_str(o, attr)?;
            if !v.is(none::none()) && !(attr != "__closure__" && is_empty(&v)) {
                raise!(ValueError, "{} must not have {}", what, attr.trim_matches('_'));
            }
        }
        check_script_code(&c, what)?;
        c
    } else {
        raise!(TypeError, "unsupported {} {}", what, ab::repr_string(o).unwrap_or_default());
    };
    crate::modules::marshal::dumps_vec(&code, 4, true)
}

/// A script (source text, or a function or code object that can run as a
/// script) as marshalled code; `pure`: it may not use globals other than
/// builtins.
pub fn script_data(o: &PyObject, pure: bool) -> PyResult<Vec<u8>> {
    let code = if pystr::is_str(o) || bytes::is_bytes(o) {
        let src = if pystr::is_str(o) { pystr::as_utf8(o)?.as_bytes().to_vec() } else { bytes::bytes_data(o).to_vec() };
        crate::compile::compile_bytes(&src, "<string>", crate::compile::Mode::Exec, 0, -1)?.code
    } else if crate::types::code::is_code(o) {
        check_script_code(o, "code")?;
        o.to_ref()
    } else if crate::types::funcobject::is_function(o) {
        for attr in ["__closure__", "__defaults__", "__kwdefaults__"] {
            if !ab::getattr_str(o, attr)?.is(none::none()) {
                raise!(ValueError, "function must not have {}", attr.trim_matches('_'));
            }
        }
        let c = ab::getattr_str(o, "__code__")?;
        check_script_code(&c, "function")?;
        c
    } else {
        raise!(TypeError, "unsupported script {}", ab::repr_string(o).unwrap_or_default());
    };
    if pure {
        let mut names = Vec::new();
        global_names(&code, &mut names)?;
        let b = crate::builtins::builtins_dict();
        if let Some(n) = names.iter().find(|n| !is_builtin_name(&b, n)) {
            raise!(ValueError, "script uses global {:?}", n);
        }
    }
    crate::modules::marshal::dumps_vec(&code, 4, true)
}

fn is_empty(o: &PyObject) -> bool {
    ab::length(o).map(|n| n == 0).unwrap_or_else(|_| {
        clear_exc();
        false
    })
}

/// `dict` of str keys -> default-shareable values, as cross-interpreter data.
pub fn shared_dict(d: &PyObject) -> PyResult<Vec<(String, Xi)>> {
    let mut out = Vec::new();
    for (k, v) in dict::items_vec(d) {
        if !pystr::is_str(&k) {
            raise!(TypeError, "expected str keys, got {}", crate::types::typeobject::tname(&k));
        }
        // keys must survive the trip as text (no lone surrogates, no NUL)
        let ks = match pystr::to_string_strict(&k) {
            Some(s) => s,
            None => {
                let enc = ab::call_method_str(&k, "encode", &[&pystr::new_str("utf-8")]);
                enc?;
                raise!(UnicodeEncodeError, "surrogates not allowed");
            }
        };
        if ks.contains('\0') {
            raise!(ValueError, "embedded null character");
        }
        out.push((ks, get(&v, Mode::Default)?));
    }
    Ok(out)
}

/// Store shared values into a namespace dict of the current interpreter.
pub fn apply_shared(ns: &PyObject, shared: &[(String, Xi)]) -> PyResult<()> {
    for (k, x) in shared {
        let v = x.to_object()?;
        dict::set_item_str(ns, k, &v)?;
    }
    Ok(())
}

#[allow(dead_code)]
fn unused(_: &PyObject) -> PyRef {
    list::new_list_empty()
}
