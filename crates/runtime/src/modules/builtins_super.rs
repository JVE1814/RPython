//! Zero-argument super(): find __class__ cell and first argument of the caller frame.
use crate::errors::*;
use crate::object::PyRef;
use crate::raise;
use rpy_compiler::bytecode as bc;
pub fn zero_arg_super() -> PyResult<(PyRef, Option<PyRef>)> {
    let f = crate::interp::current_frame_ptr();
    if f.is_null() {
        raise!(RuntimeError, "super(): no current frame");
    }
    let f = unsafe { &*f };
    let cd = crate::types::code::code_data(f.code());
    let co = crate::types::code::code(f.code());
    if co.obj.co_argcount == 0 {
        raise!(RuntimeError, "super(): no arguments");
    }
    let mut first = f.slots[0];
    if first.is_null() {
        raise!(RuntimeError, "super(): arg[0] deleted");
    }
    // the first arg may have been turned into a cell
    if cd.localspluskinds[0] & bc::fast::CELL != 0 {
        let c = unsafe { &*first };
        if c.is_type(crate::types::funcobject::cell_type()) {
            match crate::types::funcobject::cell_get(c) {
                Some(v) => first = v.into_raw(),
                None => raise!(RuntimeError, "super(): arg[0] deleted"),
            }
        }
    } else {
        unsafe { crate::object::incref(first) };
    }
    let first = unsafe { PyRef::from_raw(first) };
    for i in 0..f.nlocalsplus {
        if cd.localspluskinds[i] & bc::fast::FREE != 0 && crate::types::str::str_eq_ascii(&cd.localsplusnames[i], "__class__") {
            let cell = f.slots[i];
            if cell.is_null() {
                raise!(RuntimeError, "super(): bad __class__ cell");
            }
            return match crate::types::funcobject::cell_get(unsafe { &*cell }) {
                Some(t) => {
                    if !crate::types::typeobject::is_type(&t) {
                        raise!(RuntimeError, "super(): __class__ is not a type ({})", crate::types::typeobject::tname(&t));
                    }
                    Ok((t, Some(first)))
                }
                None => raise!(RuntimeError, "super(): empty __class__ cell"),
            };
        }
    }
    raise!(RuntimeError, "super(): __class__ cell not found");
}
