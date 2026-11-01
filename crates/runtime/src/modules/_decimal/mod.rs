//! `_decimal`: decimal floating point arithmetic (General Decimal Arithmetic
//! Specification / IEEE 754-2008), the accelerator behind Lib/decimal.py.
//!
//! Engine (no runtime dependency, testable with run_engine_tests.sh):
//!   nat     base-10^19 naturals (Karatsuba), ntt: NTT multiplication / Newton division
//!   dec     Dec/Ctx types, rounding/finalisation, parsing, to-sci/eng strings
//!   ops     arithmetic, comparison, quantize, logical/shift ops
//!   transc  sqrt, exp, ln, log10, power (correctly rounded)
//!   format  __format__ mini-language
//! Python binding: py_dec (Decimal, Context arithmetic methods), py_ctx (Context,
//! SignalDict, current context, localcontext, exceptions, module state).

pub mod dec;
pub mod format;
pub mod nat;
pub mod ntt;
pub mod ops;
pub mod transc;
pub mod py_ctx;
pub mod py_dec;
#[cfg(test)]
mod dectest;

use crate::errors::*;
use crate::object::*;

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res: PyResult<()> = Ok(());
    INIT.call_once(|| {
        res = (|| {
            py_dec::init_types()?;
            py_ctx::init_types()?;
            Ok(())
        })();
    });
    res?;
    py_ctx::init_state()?;
    let m = crate::types::module::new_module("_decimal")?;
    let d = crate::types::module::module_dict(&m);
    crate::types::dict::set_item_str(&d, "__doc__", &crate::types::str::new_str("C decimal arithmetic module"))?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("getcontext", NOARGS, py_ctx::getcontext, "getcontext($module, /)\n--\n\nGet the current default context."),
            ("setcontext", O, py_ctx::setcontext, "setcontext($module, context, /)\n--\n\nSet a new default context."),
            ("localcontext", FASTKW, py_ctx::localcontext, "localcontext($module, /, ctx=None, **kwargs)\n--\n\nReturn a context manager that will set the default context to a copy of ctx\non entry to the with-statement and restore the previous default context when\nexiting the with-statement. If no context is specified, a copy of the current\ndefault context is used.\n"),
            ("IEEEContext", O, py_ctx::ieee_context, "IEEEContext($module, bits, /)\n--\n\nReturn a context object initialized to the proper values for one of the\nIEEE interchange formats.  The argument must be a multiple of 32 and less\nthan IEEE_CONTEXT_MAX_BITS.\n"),
        ],
        Some(&m),
    )?;
    py_ctx::fill_module(&d)?;
    Ok(m)
}

pub fn register() {
    crate::import::register_builtin("_decimal", init_module);
}
