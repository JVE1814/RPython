//! Recursion guard for container reprs (Py_ReprEnter / Py_ReprLeave).
use crate::ffi::PyObject;
use std::cell::RefCell;
thread_local! { static ACTIVE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) }; }
/// Returns true if `o` is already being repr'd on this thread.
pub fn enter(o: &PyObject) -> bool {
    ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let p = o.as_ptr() as usize;
        if a.contains(&p) {
            return true;
        }
        a.push(p);
        false
    })
}
pub fn leave(o: &PyObject) {
    ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let p = o.as_ptr() as usize;
        if let Some(i) = a.iter().rposition(|&x| x == p) {
            a.remove(i);
        }
    })
}
