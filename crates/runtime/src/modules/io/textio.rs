//! TextIOWrapper and IncrementalNewlineDecoder (placeholder, replaced below).
use super::*;

pub static mut TEXTIOWRAPPER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut NLDECODER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn textiowrapper_type() -> *mut PyTypeObject {
    &raw mut TEXTIOWRAPPER_TYPE
}
pub fn nldecoder_type() -> *mut PyTypeObject {
    &raw mut NLDECODER_TYPE
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if TEXTIOWRAPPER_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup_type(textiowrapper_type(), c"_io.TextIOWrapper", std::mem::size_of::<IoHeader>(), super::iobase::textiobase_type(), "");
        crate::types::typeobject::type_ready(textiowrapper_type())?;
        let t = &mut *nldecoder_type();
        t.tp_name = c"_io.IncrementalNewlineDecoder".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        crate::types::typeobject::type_ready(nldecoder_type())?;
    }
    Ok(())
}
