# Runtime contributor guide (read before editing crates/runtime)

## Big picture
* Objects use CPython's **free-threaded C layout** (`ffi.rs`); every object is a raw heap
  allocation with a `PyObject` header. Builtin types are exported `static mut PyXxx_Type`
  symbols (C extensions link against them).
* `PyRef` (object.rs) is an owned reference (Clone = incref, Drop = decref). Borrowed
  references are `&PyObject`. `o.to_ref()` gives a new owned ref from a borrowed one
  (NOT `to_owned()` — that is std's `ToOwned` and returns `&PyObject`).
* Errors follow CPython's convention: the exception lives in the thread state; Rust
  functions return `PyResult<T>` where `Err(PyErr)` means "exception is set".
  - `raise!(TypeError, "fmt {}", x)` returns early with that exception.
  - `err!(ValueError, "...")` builds a `PyErr` (exception set) for `map_err`, etc.
  - `exc_matches(exceptions::KeyError())`, `take_exc()`, `clear_exc()`, `set_exc(e)`.
  - C boundary: `into_c(r)` (PyResult -> *mut PyObject/NULL), `from_c(p)`, `int_into_c`.
* Interned names: `name!("__init__")` gives a `&'static PyObject` interned str.

## Defining a builtin type
Fill a `static mut T: PyTypeObject = PyTypeObject::ZERO;` in an `init_types()` function:
`tp_name`, `tp_basicsize`, `tp_flags`, slots (`extern "C"` fns), `tp_methods` via
`crate::mdefs![("name", O|NOARGS|FAST|FASTKW, rust_fn, "doc"; extra_flags), ...]`,
`tp_getset` via `crate::getset_table![("attr", getter, setter; "doc")]`,
`tp_members` via `crate::member_table![("attr", Py_T_OBJECT, offset, flags)]`.
Register it: call your `init_types()` from `types::init_all()` and add the type to the
ready list there (or call `type_ready` yourself); export builtin names via
`types::builtin_types()`.

Rust method signatures (self is the first arg):
* NOARGS `fn(&PyObject) -> PyResult`
* O      `fn(&PyObject, &PyObject) -> PyResult`
* FAST   `fn(&PyObject, &[&PyObject]) -> PyResult` (positional only)
* FASTKW `fn(&PyObject, &[&PyObject], Option<&PyObject>) -> PyResult` where `args` holds
  positional values followed by keyword values and the third argument is the kwnames
  tuple; parse with `args::parse::<N>(&Params::new(fname, &names, posonly, required), args, kw)`
  or `args::split`.
For METH_CLASS methods, `self` is the class.

Wrapper slots are created automatically: if you set `nb_add`, Python sees `int.__add__`
etc. (slots.rs). Python subclasses overriding dunders get generic slot functions.

## Modules
A builtin module is a `fn() -> PyResult` creating a module (`types::module::new_module`,
`builtins::add_methods_to_dict(&dict, mdefs![...], Some(&module))`), registered with
`import::register_builtin("name", init)` from `modules::register_all()`.
Pure-Python stdlib modules live in `/Lib` (repo root) and are written from scratch.

## Rules
* Never read CPython/other implementations' source (see /AGENTS.md).
* Match CPython error messages exactly where tests check them (grep spec/Lib/test).
* Thread-safety: no GIL. For now keep data structures consistent under single-threaded
  use and avoid global mutable state without a lock.
* Keep each file owned by one workstream; touch shared registration points
  (`types/mod.rs`, `modules/mod.rs`, `types::init_all`, `modules::register_all`) with
  minimal one-line additions.

## Building
`cargo build -p python` produces `target/<dir>/debug/python3`. Set
`RPYTHON_STDLIB=/path/to/repo/Lib` (default `<prefix>/lib/python3.15t`).
