//! PEP 750 template strings: `string.templatelib.Template`, `Interpolation`
//! and the template iterator.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::str as pystr;
use crate::types::typeobject::tname;
use crate::types::{none, tuple};
use std::sync::Once;

#[repr(C)]
struct TemplateObj {
    ob_base: PyObject,
    strings: *mut PyObject,
    interpolations: *mut PyObject,
}

#[repr(C)]
struct InterpolationObj {
    ob_base: PyObject,
    value: *mut PyObject,
    expression: *mut PyObject,
    conversion: *mut PyObject,
    format_spec: *mut PyObject,
}

#[repr(C)]
struct TemplateIterObj {
    ob_base: PyObject,
    template: *mut PyObject,
    index: usize,
}

pub static mut TEMPLATE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut INTERPOLATION_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut TEMPLATE_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn template_type() -> *mut PyTypeObject {
    ensure_types();
    &raw mut TEMPLATE_TYPE
}
pub fn interpolation_type() -> *mut PyTypeObject {
    ensure_types();
    &raw mut INTERPOLATION_TYPE
}

fn tpl(o: &PyObject) -> &TemplateObj {
    unsafe { &*(o.as_ptr() as *const TemplateObj) }
}
fn itp(o: &PyObject) -> &InterpolationObj {
    unsafe { &*(o.as_ptr() as *const InterpolationObj) }
}
fn field(p: *mut PyObject) -> PyRef {
    unsafe { (*p).to_ref() }
}

fn alloc_template(strings: PyRef, interps: PyRef) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(template_type(), std::mem::size_of::<TemplateObj>());
        let t = &mut *(op as *mut TemplateObj);
        t.strings = strings.into_raw();
        t.interpolations = interps.into_raw();
        PyRef::from_raw(op)
    }
}

fn alloc_interpolation(value: &PyObject, expr: &PyObject, conv: &PyObject, spec: &PyObject) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(interpolation_type(), std::mem::size_of::<InterpolationObj>());
        let i = &mut *(op as *mut InterpolationObj);
        i.value = value.to_ref().into_raw();
        i.expression = expr.to_ref().into_raw();
        i.conversion = conv.to_ref().into_raw();
        i.format_spec = spec.to_ref().into_raw();
        PyRef::from_raw(op)
    }
}

/// BUILD_TEMPLATE: `strings` and `interps` are tuples (len(strings) == len(interps) + 1).
pub fn new_template(strings: &PyObject, interps: &PyObject) -> PyResult {
    let s = if tuple::is_tuple_exact(strings) { strings.to_ref() } else { ab::sequence_tuple(strings)? };
    let i = if tuple::is_tuple_exact(interps) { interps.to_ref() } else { ab::sequence_tuple(interps)? };
    Ok(alloc_template(s, i))
}

/// BUILD_INTERPOLATION: `conv` is 0 (none), 1 ('s'), 2 ('r') or 3 ('a').
pub fn new_interpolation(v: &PyObject, s: &PyObject, conv: u32, spec: Option<&PyObject>) -> PyResult {
    let conv_obj = match conv {
        1 => pystr::new_str("s"),
        2 => pystr::new_str("r"),
        3 => pystr::new_str("a"),
        _ => none::none_ref(),
    };
    let empty = pystr::new_str("");
    Ok(alloc_interpolation(v, s, &conv_obj, spec.unwrap_or(&empty)))
}

// ------------------------------------------------------------------ Template

fn is_interp(o: &PyObject) -> bool {
    o.is_type(interpolation_type())
}

/// Build a Template from an interleaved sequence of str / Interpolation.
fn template_from_parts(parts: &[&PyObject]) -> PyResult {
    let mut strings: Vec<PyRef> = Vec::new();
    let mut interps: Vec<PyRef> = Vec::new();
    let mut cur: Option<PyRef> = None;
    for p in parts {
        if pystr::is_str(p) {
            cur = Some(match cur {
                None => p.to_ref(),
                Some(c) => pystr::concat(&c, p),
            });
        } else if is_interp(p) {
            strings.push(cur.take().unwrap_or_else(|| pystr::new_str("")));
            interps.push(p.to_ref());
        } else {
            raise!(TypeError, "Template.__new__ *args need to be of type 'str' or 'Interpolation', got {}", tname(p));
        }
    }
    strings.push(cur.unwrap_or_else(|| pystr::new_str("")));
    Ok(alloc_template(tuple::new_tuple(strings), tuple::new_tuple(interps)))
}

unsafe extern "C" fn template_new(_t: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "Template.__new__ only accepts *args arguments")));
        }
        let parts = tuple::tuple_items(&*args);
        into_c(template_from_parts(parts))
    }
}

unsafe extern "C" fn template_dealloc(o: *mut PyObject) {
    unsafe {
        let t = &mut *(o as *mut TemplateObj);
        xdecref(t.strings);
        xdecref(t.interpolations);
        crate::alloc::free_object(o);
    }
}

fn t_strings(o: &PyObject) -> PyResult {
    Ok(field(tpl(o).strings))
}
fn t_interpolations(o: &PyObject) -> PyResult {
    Ok(field(tpl(o).interpolations))
}
fn t_values(o: &PyObject) -> PyResult {
    let it = field(tpl(o).interpolations);
    let v: Vec<PyRef> = tuple::tuple_items(&it).iter().map(|i| field(itp(i).value)).collect();
    Ok(tuple::new_tuple(v))
}

unsafe extern "C" fn template_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    into_c((|| {
        let s = ab::repr_string(&field(tpl(o).strings))?;
        let i = ab::repr_string(&field(tpl(o).interpolations))?;
        Ok(pystr::new_str(&format!("Template(strings={}, interpolations={})", s, i)))
    })())
}

unsafe extern "C" fn template_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut TEMPLATE_ITER_TYPE, std::mem::size_of::<TemplateIterObj>());
        let it = &mut *(op as *mut TemplateIterObj);
        it.template = (*o).to_ref().into_raw();
        it.index = 0;
        op
    }
}

unsafe extern "C" fn titer_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut TemplateIterObj);
        if it.template.is_null() {
            return std::ptr::null_mut();
        }
        let t = tpl(&*it.template);
        let strings = tuple::tuple_items(&*t.strings);
        let interps = tuple::tuple_items(&*t.interpolations);
        // Positions: 2k = strings[k], 2k+1 = interpolations[k]; empty strings are skipped.
        loop {
            let k = it.index / 2;
            let is_str = it.index % 2 == 0;
            if is_str {
                if k >= strings.len() {
                    let tp = std::mem::replace(&mut it.template, std::ptr::null_mut());
                    decref(tp);
                    return std::ptr::null_mut();
                }
                it.index += 1;
                let s = strings[k];
                if pystr::str_len(s) > 0 {
                    return s.to_ref().into_raw();
                }
            } else {
                it.index += 1;
                if k < interps.len() {
                    return interps[k].to_ref().into_raw();
                }
            }
        }
    }
}

unsafe extern "C" fn titer_dealloc(o: *mut PyObject) {
    unsafe {
        xdecref((*(o as *mut TemplateIterObj)).template);
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

fn concat_templates(a: &PyObject, b: &PyObject) -> PyResult {
    let (ta, tb) = (tpl(a), tpl(b));
    let sa = tuple::tuple_items(unsafe { &*ta.strings });
    let sb = tuple::tuple_items(unsafe { &*tb.strings });
    let mut strings: Vec<PyRef> = sa[..sa.len() - 1].iter().map(|s| s.to_ref()).collect();
    strings.push(pystr::concat(sa[sa.len() - 1], sb[0]));
    strings.extend(sb[1..].iter().map(|s| s.to_ref()));
    let mut interps: Vec<PyRef> = tuple::tuple_to_vec(unsafe { &*ta.interpolations });
    interps.extend(tuple::tuple_to_vec(unsafe { &*tb.interpolations }));
    Ok(alloc_template(tuple::new_tuple(strings), tuple::new_tuple(interps)))
}

unsafe extern "C" fn template_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let tt = template_type();
        if a.is_type(tt) && b.is_type(tt) {
            return into_c(concat_templates(a, b));
        }
        if a.is_type(tt) {
            return into_c(Err(err!(
                TypeError,
                "can only concatenate string.templatelib.Template (not \"{}\") to string.templatelib.Template",
                tname(b)
            )));
        }
        if b.is_type(tt) && pystr::is_str(a) {
            return into_c(Err(err!(
                TypeError,
                "can only concatenate str (not \"string.templatelib.Template\") to str"
            )));
        }
        none::not_implemented().to_ref().into_raw()
    }
}

fn template_reduce(o: &PyObject) -> PyResult {
    let m = crate::import::import_absolute("string.templatelib")?;
    let f = ab::getattr_str(&m, "_template_unpickle")?;
    let args = tuple::new_tuple(vec![field(tpl(o).strings), field(tpl(o).interpolations)]);
    Ok(tuple::new_tuple(vec![f, args]))
}

fn class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::modules::text_util::generic_alias(cls, item)
}

// ------------------------------------------------------------------ Interpolation

fn check_conversion(c: &PyObject) -> PyResult<()> {
    if none::is_none(c) {
        return Ok(());
    }
    if pystr::is_str(c) {
        if let Some(s) = pystr::ascii_str(c) {
            if matches!(s, "s" | "r" | "a") {
                return Ok(());
            }
        }
    }
    raise!(ValueError, "Interpolation() argument 'conversion' must be one of 's', 'a' or 'r'")
}

unsafe extern "C" fn interpolation_new(_t: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| {
            let (v, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = v.iter().map(|r| &**r).collect();
            let [value, expr, conv, spec] = crate::args::parse::<4>(
                &crate::args::Params::new("Interpolation", &["value", "expression", "conversion", "format_spec"], 0, 1),
                &refs,
                kwn.as_deref(),
            )?;
            let empty = pystr::new_str("");
            let expr = expr.unwrap_or(&empty);
            if !pystr::is_str(expr) {
                raise!(TypeError, "Interpolation() argument 'expression' must be str, not {}", tname(expr));
            }
            let conv = conv.unwrap_or(none::none());
            check_conversion(conv)?;
            let spec = spec.unwrap_or(&empty);
            if !pystr::is_str(spec) {
                raise!(TypeError, "Interpolation() argument 'format_spec' must be str, not {}", tname(spec));
            }
            Ok(alloc_interpolation(value.unwrap(), expr, conv, spec))
        })())
    }
}

unsafe extern "C" fn interpolation_dealloc(o: *mut PyObject) {
    unsafe {
        let i = &mut *(o as *mut InterpolationObj);
        xdecref(i.value);
        xdecref(i.expression);
        xdecref(i.conversion);
        xdecref(i.format_spec);
        crate::alloc::free_object(o);
    }
}

fn i_value(o: &PyObject) -> PyResult {
    Ok(field(itp(o).value))
}
fn i_expression(o: &PyObject) -> PyResult {
    Ok(field(itp(o).expression))
}
fn i_conversion(o: &PyObject) -> PyResult {
    Ok(field(itp(o).conversion))
}
fn i_format_spec(o: &PyObject) -> PyResult {
    Ok(field(itp(o).format_spec))
}

unsafe extern "C" fn interpolation_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    into_c((|| {
        let i = itp(o);
        Ok(pystr::new_str(&format!(
            "Interpolation({}, {}, {}, {})",
            ab::repr_string(&field(i.value))?,
            ab::repr_string(&field(i.expression))?,
            ab::repr_string(&field(i.conversion))?,
            ab::repr_string(&field(i.format_spec))?
        )))
    })())
}

fn interpolation_reduce(o: &PyObject) -> PyResult {
    let i = itp(o);
    let args = tuple::new_tuple(vec![field(i.value), field(i.expression), field(i.conversion), field(i.format_spec)]);
    Ok(tuple::new_tuple(vec![crate::types::typeobject::type_ref(interpolation_type()), args]))
}

// ------------------------------------------------------------------ type setup

static mut NUMBER: PyNumberMethods = PyNumberMethods::ZERO;

fn ensure_types() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let t = &mut *(&raw mut TEMPLATE_TYPE);
        t.tp_name = c"string.templatelib.Template".as_ptr();
        t.tp_basicsize = std::mem::size_of::<TemplateObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_new = Some(template_new);
        t.tp_dealloc = Some(template_dealloc);
        t.tp_repr = Some(template_repr);
        t.tp_iter = Some(template_iter);
        let nb = &mut *(&raw mut NUMBER);
        nb.nb_add = Some(template_add);
        t.tp_as_number = nb;
        t.tp_doc = c"Template object".as_ptr();
        t.tp_getset = crate::getset_table![
            ("strings", t_strings; "Strings"),
            ("interpolations", t_interpolations; "Interpolations"),
            ("values", t_values; "Values of interpolations"),
        ];
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, template_reduce),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];

        let t = &mut *(&raw mut INTERPOLATION_TYPE);
        t.tp_name = c"string.templatelib.Interpolation".as_ptr();
        t.tp_basicsize = std::mem::size_of::<InterpolationObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_new = Some(interpolation_new);
        t.tp_dealloc = Some(interpolation_dealloc);
        t.tp_repr = Some(interpolation_repr);
        t.tp_doc = c"Interpolation object".as_ptr();
        t.tp_getset = crate::getset_table![
            ("value", i_value),
            ("expression", i_expression),
            ("conversion", i_conversion),
            ("format_spec", i_format_spec),
        ];
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, interpolation_reduce),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];

        let t = &mut *(&raw mut TEMPLATE_ITER_TYPE);
        t.tp_name = c"string.templatelib.TemplateIter".as_ptr();
        t.tp_basicsize = std::mem::size_of::<TemplateIterObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
        t.tp_dealloc = Some(titer_dealloc);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(titer_next);

        for t in [&raw mut TEMPLATE_TYPE, &raw mut INTERPOLATION_TYPE, &raw mut TEMPLATE_ITER_TYPE] {
            if crate::modules::text_util::ready_type(t).is_err() {
                clear_exc();
            }
        }
        // __match_args__ for structural pattern matching.
        let ma = tuple::new_tuple(vec![
            pystr::new_str("value"),
            pystr::new_str("expression"),
            pystr::new_str("conversion"),
            pystr::new_str("format_spec"),
        ]);
        crate::types::typeobject::type_dict_set(&raw mut INTERPOLATION_TYPE, "__match_args__", &ma);
        crate::types::typeobject::type_dict_set(&raw mut TEMPLATE_TYPE, "__module__", &pystr::new_str("string.templatelib"));
        crate::types::typeobject::type_dict_set(&raw mut INTERPOLATION_TYPE, "__module__", &pystr::new_str("string.templatelib"));
    });
}
