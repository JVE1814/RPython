//! Registration of the "devtools" workstream's builtin modules.
use crate::import::register_builtin;

pub fn register() {
    register_builtin("marshal", super::marshal::init);
}
