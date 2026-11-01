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
    crate::types::dict::set_item_str(&d, "__doc__", &crate::types::str::new_str("Rust implementation of decimal arithmetic (accelerator for the decimal module)."))?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("getcontext", NOARGS, py_ctx::getcontext, "getcontext($module, /)\n--\n\nReturn the context of the current thread."),
            ("setcontext", O, py_ctx::setcontext, "setcontext($module, context, /)\n--\n\nMake the given context the current one (templates are copied)."),
            ("localcontext", FASTKW, py_ctx::localcontext, "localcontext($module, /, ctx=None, **kwargs)\n--\n\nContext manager that installs a copy of ctx (or of the current context),\nwith any keyword overrides applied, and restores the previous context on exit."),
            ("IEEEContext", O, py_ctx::ieee_context, "IEEEContext($module, bits, /)\n--\n\nContext for the IEEE 754 interchange format with the given bit width\n(a multiple of 32, at most IEEE_CONTEXT_MAX_BITS)."),
        ],
        Some(&m),
    )?;
    py_ctx::fill_module(&d)?;
    Ok(m)
}

pub fn register() {
    crate::import::register_builtin("_decimal", init_module);
}
