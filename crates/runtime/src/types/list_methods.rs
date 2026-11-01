//! list methods (stub level): sort.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::{PyObject, Py_LT};
use crate::object::PyRef;
use crate::types::list;

/// Stable merge sort with optional key, honoring reverse; uses __lt__.
pub fn sort_list(l: &PyObject, key: Option<&PyObject>, reverse: bool) -> PyResult<()> {
    let items = list::list_to_vec(l);
    list::set_contents(l, Vec::new());
    let keys: Vec<PyRef> = match key {
        Some(k) => {
            let mut v = Vec::with_capacity(items.len());
            for it in &items {
                v.push(ab::call(k, &[it])?);
            }
            v
        }
        None => items.clone(),
    };
    let mut idx: Vec<usize> = (0..items.len()).collect();
    let mut err = None;
    let lt = |a: &PyRef, b: &PyRef| -> PyResult<bool> { ab::rich_compare_bool(a, b, Py_LT) };
    // insertion-based merge sort to propagate errors
    merge_sort(&mut idx, &mut |a, b| {
        if err.is_some() {
            return false;
        }
        let r = if reverse { lt(&keys[b], &keys[a]) } else { lt(&keys[a], &keys[b]) };
        match r {
            Ok(x) => x,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    });
    let sorted: Vec<PyRef> = idx.into_iter().map(|i| items[i].clone()).collect();
    if list::list_len(l) != 0 {
        list::set_contents(l, sorted);
        raise!(ValueError, "list modified during sort");
    }
    list::set_contents(l, sorted);
    if let Some(e) = err {
        return Err(e);
    }
    Ok(())
}

fn merge_sort(v: &mut Vec<usize>, less: &mut dyn FnMut(usize, usize) -> bool) {
    let n = v.len();
    if n <= 1 {
        return;
    }
    let mut buf = v.clone();
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut a, mut b, mut k) = (i, mid, i);
            while a < mid && b < hi {
                if less(v[b], v[a]) {
                    buf[k] = v[b];
                    b += 1;
                } else {
                    buf[k] = v[a];
                    a += 1;
                }
                k += 1;
            }
            while a < mid {
                buf[k] = v[a];
                a += 1;
                k += 1;
            }
            while b < hi {
                buf[k] = v[b];
                b += 1;
                k += 1;
            }
            i += 2 * width;
        }
        std::mem::swap(v, &mut buf);
        width *= 2;
    }
}
