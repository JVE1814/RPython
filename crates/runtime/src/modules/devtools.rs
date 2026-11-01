//! Registration of the "devtools" workstream's builtin modules.
use crate::import::register_builtin;

pub fn register() {
    register_builtin("marshal", super::marshal::init);
    register_builtin("pwd", super::pwd::init_pwd);
    register_builtin("grp", super::pwd::init_grp);
    register_builtin("resource", super::resource::init);
    register_builtin("fcntl", super::fcntl::init);
    register_builtin("termios", super::termios::init);
    register_builtin("syslog", super::syslog::init_syslog);
    register_builtin("_posixshmem", super::syslog::init_posixshmem);
    register_builtin("mmap", super::mmap::init);
}

/// `int::as_i128` restricted to |v| < 2**120 (values with more digits give None).
pub fn int_as_i128(o: &crate::ffi::PyObject) -> Option<i128> {
    if crate::types::int::ndigits(o) > 4 {
        return None;
    }
    crate::types::int::as_i128(o)
}

/// Create a new exception class `module.name` deriving from `base` (PyErr_NewException).
pub fn new_exception(module: &str, name: &str, base: *mut crate::ffi::PyTypeObject) -> crate::errors::PyResult {
    use crate::types::{dict, str as pystr, tuple, typeobject};
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str(module))?;
    let bases = tuple::new_tuple(vec![typeobject::type_ref(base)]);
    let meta = typeobject::type_ref(&raw mut typeobject::PyType_Type);
    crate::abstract_::call(&meta, &[&pystr::new_str(name), &bases, &ns])
}
