//! `_tokenize.TokenizerIter`: the tokenize-module token stream produced by the
//! rpy-parser tokenizer.  The source is read through `readline` (str lines, or
//! bytes lines decoded with `encoding`) on the first iteration.

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::text_util as tu;
use crate::object::*;
use crate::types::str as pystr;
use crate::types::{bytes, int, none, tuple};
use rpy_parser::tokenizer::TokenIter;

struct State {
    // Field order matters: `iter` borrows `src` and must be dropped first.
    iter: Option<TokenIter<'static>>,
    src: Option<Box<str>>,
    readline: PyRef,
    encoding: Option<String>,
    extra: bool,
    done: bool,
}

#[repr(C)]
struct TokIterObj {
    ob_base: PyObject,
    state: *mut State,
}

static mut TOKITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn read_source(st: &State) -> PyResult<String> {
    let mut src = String::new();
    loop {
        let line = match ab::call(&st.readline, &[]) {
            Ok(l) => l,
            Err(e) => {
                if exc_matches(crate::types::exceptions::StopIteration()) {
                    clear_exc();
                    break;
                }
                return Err(e);
            }
        };
        let text: PyRef = match &st.encoding {
            None => {
                if !pystr::is_str(&line) {
                    raise!(TypeError, "readline() returned a non-string object");
                }
                line
            }
            Some(enc) => {
                if !bytes::is_bytes(&line) {
                    raise!(TypeError, "readline() returned a non-bytes object");
                }
                crate::modules::_codecs::decode_text(bytes::bytes_data(&line), Some(&line), enc, "strict")?
            }
        };
        if pystr::str_len(&text) == 0 {
            break;
        }
        src.push_str(&pystr::to_string_lossy(&text));
    }
    Ok(src)
}

fn syntax_error(e: &rpy_parser::SyntaxError) -> PyErr {
    let class = match e.kind.class_name() {
        "IndentationError" => crate::types::exceptions::IndentationError(),
        "TabError" => crate::types::exceptions::TabError(),
        _ => crate::types::exceptions::SyntaxError(),
    };
    crate::compile::raise_syntax_error(class, &e.msg, "<string>", e.lineno, e.offset, e.text.as_deref(), e.end_lineno, e.end_offset)
}

unsafe extern "C" fn tokiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let st = &mut *(*(o as *mut TokIterObj)).state;
        into_c_opt((|| -> PyResult<Option<PyRef>> {
            if st.done {
                return Ok(None);
            }
            if st.iter.is_none() {
                let src = read_source(st)?.into_boxed_str();
                let hooks = crate::compile::unicode_hooks();
                let r: &'static str = std::mem::transmute::<&str, &'static str>(&src);
                st.src = Some(src);
                st.iter = Some(TokenIter::new(r, st.extra, hooks));
            }
            match st.iter.as_mut().unwrap().next() {
                None => {
                    st.done = true;
                    Ok(None)
                }
                Some(Err(e)) => {
                    st.done = true;
                    Err(syntax_error(&e))
                }
                Some(Ok(t)) => {
                    let pos = |p: (u32, i32)| tuple::new_tuple(vec![int::new_int(p.0 as i64), int::new_int(p.1 as i64)]);
                    Ok(Some(tuple::new_tuple(vec![
                        int::new_int(t.ty as u8 as i64),
                        pystr::new_str(&t.string),
                        pos(t.start),
                        pos(t.end),
                        pystr::new_str(&t.line),
                    ])))
                }
            }
        })())
    }
}

fn into_c_opt(r: PyResult<Option<PyRef>>) -> *mut PyObject {
    match r {
        Ok(Some(v)) => v.into_raw(),
        Ok(None) | Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn tokiter_dealloc(o: *mut PyObject) {
    unsafe {
        let p = (*(o as *mut TokIterObj)).state;
        if !p.is_null() {
            let mut b = Box::from_raw(p);
            b.iter = None;
            drop(b);
        }
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

unsafe extern "C" fn tokiter_new(_t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kwn) = args::kwdict_to_vectorcall(&*a, kw);
            let refs: Vec<&PyObject> = v.iter().map(|r| &**r).collect();
            let [readline, extra, enc] = args::parse::<3>(
                &Params::new("tokenizeriter", &["readline", "extra_tokens", "encoding"], 1, 1).kwonly(2),
                &refs,
                kwn.as_deref(),
            )?;
            let Some(extra) = extra else {
                raise!(TypeError, "tokenizeriter() missing required keyword-only argument 'extra_tokens'");
            };
            let encoding = tu::opt_str_arg(enc, "tokenizeriter", "encoding")?;
            let st = Box::new(State {
                iter: None,
                src: None,
                readline: readline.unwrap().to_ref(),
                encoding,
                extra: ab::is_true(extra)?,
                done: false,
            });
            let op = crate::alloc::alloc_object(&raw mut TOKITER_TYPE, std::mem::size_of::<TokIterObj>());
            (*(op as *mut TokIterObj)).state = Box::into_raw(st);
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

pub fn init_module() -> PyResult {
    unsafe {
        let t = &mut *(&raw mut TOKITER_TYPE);
        if t.tp_name.is_null() {
            t.tp_name = c"_tokenize.TokenizerIter".as_ptr();
            t.tp_basicsize = std::mem::size_of::<TokIterObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
            t.tp_new = Some(tokiter_new);
            t.tp_dealloc = Some(tokiter_dealloc);
            t.tp_iter = Some(self_iter);
            t.tp_iternext = Some(tokiter_next);
            tu::ready_type(&raw mut TOKITER_TYPE)?;
        }
    }
    let m = crate::types::module::new_module("_tokenize")?;
    tu::dict_set(&crate::types::module::module_dict(&m), "TokenizerIter", crate::types::typeobject::type_obj(&raw mut TOKITER_TYPE))?;
    let _ = none::none;
    Ok(m)
}
