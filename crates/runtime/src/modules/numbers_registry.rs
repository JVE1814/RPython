//! Registration of the numeric builtin modules (numbers workstream).
use crate::import::register_builtin;

pub fn register() {
    register_builtin("math", super::math::init_math);
    register_builtin("math.integer", super::math::init_math_integer);
    register_builtin("cmath", super::cmath::init_cmath);
    register_builtin("_struct", super::_struct::init_module);
}
