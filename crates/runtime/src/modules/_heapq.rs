//! The `_heapq` accelerator (min-heap and max-heap primitives over lists).

use crate::abstract_ as ab;
use crate::args::check_pos;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::list;
use crate::{mdefs, raise};

fn lt(a: &PyObject, b: &PyObject, max: bool) -> PyResult<bool> {
    if max { ab::rich_compare_bool(b, a, Py_LT) } else { ab::rich_compare_bool(a, b, Py_LT) }
}

fn get(heap: &PyObject, i: usize) -> PyResult<PyRef> {
    match list::get(heap, i) {
        Some(v) => Ok(v),
        None => raise!(RuntimeError, "list changed size during iteration"),
    }
}

fn swap(heap: &PyObject, i: usize, j: usize) -> PyResult<()> {
    let n = list::list_len(heap);
    if i >= n || j >= n {
        raise!(RuntimeError, "list changed size during iteration");
    }
    let a = get(heap, i)?;
    let b = get(heap, j)?;
    list::set(heap, i, b);
    list::set(heap, j, a);
    Ok(())
}

/// Move the item at `pos` up towards `startpos` until the heap invariant holds.
fn siftdown(heap: &PyObject, startpos: usize, mut pos: usize, max: bool) -> PyResult<()> {
    let size = list::list_len(heap);
    if pos >= size {
        raise!(RuntimeError, "list changed size during iteration");
    }
    while pos > startpos {
        let parentpos = (pos - 1) >> 1;
        let newitem = get(heap, pos)?;
        let parent = get(heap, parentpos)?;
        let c = lt(&newitem, &parent, max)?;
        if size != list::list_len(heap) {
            raise!(RuntimeError, "list changed size during iteration");
        }
        if !c {
            break;
        }
        swap(heap, pos, parentpos)?;
        pos = parentpos;
    }
    Ok(())
}

/// Bubble the smaller child up from `pos` to a leaf, then sift down.
fn siftup(heap: &PyObject, mut pos: usize, max: bool) -> PyResult<()> {
    let endpos = list::list_len(heap);
    let startpos = pos;
    if pos >= endpos {
        raise!(RuntimeError, "list changed size during iteration");
    }
    let limit = endpos >> 1;
    while pos < limit {
        let mut childpos = 2 * pos + 1;
        if childpos + 1 < endpos {
            let a = get(heap, childpos)?;
            let b = get(heap, childpos + 1)?;
            let c = lt(&a, &b, max)?;
            if endpos != list::list_len(heap) {
                raise!(RuntimeError, "list changed size during iteration");
            }
            if !c {
                childpos += 1;
            }
        }
        swap(heap, pos, childpos)?;
        pos = childpos;
    }
    siftdown(heap, startpos, pos, max)
}

fn check_list(o: &PyObject) -> PyResult<()> {
    if !list::is_list(o) {
        raise!(TypeError, "heap argument must be a list");
    }
    Ok(())
}

fn push_impl(a: &[&PyObject], name: &str, max: bool) -> PyResult {
    check_pos(name, a, 2, 2)?;
    check_list(a[0])?;
    let _whole = crate::objlock::lock(a[0]); // heap operations are atomic w.r.t. each other
    list::append(a[0], a[1].to_ref());
    let n = list::list_len(a[0]);
    siftdown(a[0], 0, n - 1, max)?;
    Ok(fc::none())
}

fn pop_impl(heap: &PyObject, max: bool) -> PyResult {
    check_list(heap)?;
    let _whole = crate::objlock::lock(heap);
    let n = list::list_len(heap);
    if n == 0 {
        raise!(IndexError, "index out of range");
    }
    // pop the current last item atomically (another thread may have shrunk the list
    // since `n` was read; list::pop would then hand back None)
    let last = match list::pop_index(heap, -1) {
        Ok(v) => v,
        Err(_) => raise!(IndexError, "index out of range"),
    };
    if n == 1 {
        return Ok(last);
    }
    let ret = get(heap, 0)?;
    list::set(heap, 0, last);
    siftup(heap, 0, max)?;
    Ok(ret)
}

fn replace_impl(a: &[&PyObject], name: &str, max: bool) -> PyResult {
    check_pos(name, a, 2, 2)?;
    check_list(a[0])?;
    let _whole = crate::objlock::lock(a[0]);
    if list::list_len(a[0]) == 0 {
        raise!(IndexError, "index out of range");
    }
    let ret = get(a[0], 0)?;
    list::set(a[0], 0, a[1].to_ref());
    siftup(a[0], 0, max)?;
    Ok(ret)
}

fn pushpop_impl(a: &[&PyObject], name: &str, max: bool) -> PyResult {
    check_pos(name, a, 2, 2)?;
    let (heap, item) = (a[0], a[1]);
    check_list(heap)?;
    let _whole = crate::objlock::lock(heap);
    if list::list_len(heap) == 0 {
        return Ok(item.to_ref());
    }
    let top = get(heap, 0)?;
    let c = lt(&top, item, max)?;
    if !c {
        return Ok(item.to_ref());
    }
    if list::list_len(heap) == 0 {
        raise!(IndexError, "index out of range");
    }
    let ret = get(heap, 0)?;
    list::set(heap, 0, item.to_ref());
    siftup(heap, 0, max)?;
    Ok(ret)
}

fn heapify_impl(heap: &PyObject, max: bool) -> PyResult {
    check_list(heap)?;
    let _whole = crate::objlock::lock(heap);
    let n = list::list_len(heap);
    for i in (0..n / 2).rev() {
        siftup(heap, i, max)?;
    }
    Ok(fc::none())
}

fn heappush(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    push_impl(a, "heappush", false)
}
fn heappush_max(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    push_impl(a, "heappush_max", true)
}
fn heappop(_m: &PyObject, h: &PyObject) -> PyResult {
    pop_impl(h, false)
}
fn heappop_max(_m: &PyObject, h: &PyObject) -> PyResult {
    pop_impl(h, true)
}
fn heapreplace(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    replace_impl(a, "heapreplace", false)
}
fn heapreplace_max(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    replace_impl(a, "heapreplace_max", true)
}
fn heappushpop(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    pushpop_impl(a, "heappushpop", false)
}
fn heappushpop_max(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    pushpop_impl(a, "heappushpop_max", true)
}
fn heapify(_m: &PyObject, h: &PyObject) -> PyResult {
    heapify_impl(h, false)
}
fn heapify_max(_m: &PyObject, h: &PyObject) -> PyResult {
    heapify_impl(h, true)
}

pub fn init_module() -> PyResult {
    let methods = mdefs![
        ("heappush", FAST, heappush, "heappush($module, heap, item, /)\n--\n\nPush item onto heap, maintaining the heap invariant."),
        ("heappushpop", FAST, heappushpop, "heappushpop($module, heap, item, /)\n--\n\nPush item on the heap, then pop and return the smallest item from the heap.\n\nThe combined action runs more efficiently than heappush() followed by\na separate call to heappop()."),
        ("heappop", O, heappop, "heappop($module, heap, /)\n--\n\nPop the smallest item off the heap, maintaining the heap invariant."),
        ("heapreplace", FAST, heapreplace, "heapreplace($module, heap, item, /)\n--\n\nPop and return the current smallest value, and add the new item.\n\nThis is more efficient than heappop() followed by heappush(), and can be\nmore appropriate when using a fixed-size heap.  Note that the value\nreturned may be larger than item!  That constrains reasonable uses of\nthis routine unless written as part of a conditional replacement:\n\n    if item > heap[0]:\n        item = heapreplace(heap, item)"),
        ("heapify", O, heapify, "heapify($module, heap, /)\n--\n\nTransform list into a heap, in-place, in O(len(heap)) time."),
        ("heappush_max", FAST, heappush_max, "heappush_max($module, heap, item, /)\n--\n\nPush item onto max heap, maintaining the heap invariant."),
        ("heappushpop_max", FAST, heappushpop_max, "heappushpop_max($module, heap, item, /)\n--\n\nMaxheap variant of heappushpop.\n\nThe combined action runs more efficiently than heappush_max() followed by\na separate call to heappop_max()."),
        ("heappop_max", O, heappop_max, "heappop_max($module, heap, /)\n--\n\nMaxheap variant of heappop."),
        ("heapreplace_max", FAST, heapreplace_max, "heapreplace_max($module, heap, item, /)\n--\n\nMaxheap variant of heapreplace."),
        ("heapify_max", O, heapify_max, "heapify_max($module, heap, /)\n--\n\nMaxheap variant of heapify."),
    ];
    let (m, d) = fc::new_module("_heapq", methods, Some("Heap queue algorithm (a.k.a. priority queue)."))?;
    crate::types::dict::set_item_str(&d, "__about__", &crate::types::str::new_str("Heap queues"))?;
    Ok(m)
}
