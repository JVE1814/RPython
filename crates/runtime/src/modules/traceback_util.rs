//! Helpers used when decorating exceptions.
pub fn add_note_to_current(note: &str) {
    let e = match crate::errors::take_exc() {
        Some(e) => e,
        None => return,
    };
    let n = crate::types::str::new_str(note);
    let _ = crate::abstract_::call_method_str(&e, "add_note", &[&n]);
    crate::errors::clear_exc();
    crate::errors::set_exc(e);
}
