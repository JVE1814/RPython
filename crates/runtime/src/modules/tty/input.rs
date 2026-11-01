//! The interactive-terminal path of `input()`: when a line-editing hook is
//! installed (`PyOS_ReadlineFunctionPointer`, set by importing `readline`) and
//! sys.stdin / sys.stdout are the process's terminal, the prompt is handed to the
//! hook instead of being written through sys.stdout.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{bytes, none, str as pystr};
use std::ffi::{CStr, c_char};

unsafe extern "C" {
    static mut stdin: *mut libc::FILE;
    static mut stdout: *mut libc::FILE;
}

fn fileno_is(stream: &PyObject, want: i32) -> bool {
    match ab::call_method_str(stream, "fileno", &[]) {
        Ok(fd) => match crate::types::int::as_i64(&fd) {
            Some(v) => v == want as i64 && unsafe { libc::isatty(want) } != 0,
            None => false,
        },
        Err(_) => {
            clear_exc();
            false
        }
    }
}

fn attr_str(o: &PyObject, name: &str, default: &str) -> String {
    match ab::getattr_str(o, name) {
        Ok(v) if pystr::is_str(&v) => pystr::to_string_lossy(&v),
        Ok(_) => default.to_string(),
        Err(_) => {
            clear_exc();
            default.to_string()
        }
    }
}

/// Returns Ok(None) when the terminal path does not apply (caller falls back
/// to sys.stdout.write + sys.stdin.readline).
pub fn tty_input(prompt: Option<&PyObject>) -> PyResult<Option<PyRef>> {
    let hook_set = unsafe { (*(&raw const crate::capi::misc::PyOS_ReadlineFunctionPointer)).is_some() };
    if !hook_set {
        return Ok(None);
    }
    let sys_get = |n: &str| crate::modules::sys::get_sys_attr(n).filter(|o| !o.is(none::none()));
    let Some(inp) = sys_get("stdin") else { return Ok(None) };
    let Some(out) = sys_get("stdout") else { return Ok(None) };
    if !fileno_is(&inp, 0) || !fileno_is(&out, 1) {
        return Ok(None);
    }
    if let Some(err) = sys_get("stderr") {
        if ab::call_method_str(&err, "flush", &[]).is_err() {
            clear_exc();
        }
    }
    ab::call_method_str(&out, "flush", &[])?;
    let out_enc = attr_str(&out, "encoding", "utf-8");
    let out_err = attr_str(&out, "errors", "strict");
    let prompt_bytes: Vec<u8> = match prompt {
        Some(p) if !p.is(none::none()) => {
            let s = ab::str(p)?;
            let b = crate::modules::codecs_registry::encode(&s, &out_enc, &out_err)?;
            if !bytes::is_bytes(&b) {
                crate::raise!(TypeError, "encoder did not return bytes");
            }
            bytes::bytes_data(&b).to_vec()
        }
        _ => Vec::new(),
    };
    if prompt_bytes.contains(&0) {
        crate::raise!(ValueError, "input: prompt string cannot contain null characters");
    }
    let mut cprompt = prompt_bytes;
    cprompt.push(0);
    let line = unsafe { crate::capi::misc::PyOS_Readline(stdin, stdout, cprompt.as_ptr() as *const c_char) };
    if line.is_null() {
        if !err_occurred() {
            return Err(set_err_noargs(crate::types::exceptions::KeyboardInterrupt()));
        }
        return Err(PyErr);
    }
    let raw = unsafe { CStr::from_ptr(line) }.to_bytes().to_vec();
    unsafe { crate::capi::mem::PyMem_RawFree(line as *mut std::ffi::c_void) };
    if raw.is_empty() {
        return Err(set_err_noargs(crate::types::exceptions::EOFError()));
    }
    let body = raw.strip_suffix(b"\n").unwrap_or(&raw);
    let in_enc = attr_str(&inp, "encoding", "utf-8");
    let in_err = attr_str(&inp, "errors", "strict");
    let b = bytes::new_bytes(body);
    let s = ab::call_method_str(&b, "decode", &[&pystr::new_str(&in_enc), &pystr::new_str(&in_err)])?;
    Ok(Some(s))
}
