//! The dunder <-> slot bridge.
//! * `add_operators`: for a type whose C slots are filled, expose `__add__` etc. as
//!   wrapper descriptors (`wrapper_descriptor`).
//! * `fixup_slots` / `update_slot`: for heap types, point slots at generic `slot_*`
//!   functions that look the dunder up and call it.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{c_int, c_void};

pub type WrapperFn = unsafe fn(&PyObject, &[&PyObject], Option<&PyObject>, *mut c_void) -> PyResult;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    TpGetattro,
    TpSetattro,
    TpRepr,
    TpHash,
    TpCall,
    TpStr,
    TpRichcompare,
    TpIter,
    TpIternext,
    TpDescrGet,
    TpDescrSet,
    TpInit,
    TpNew,
    TpFinalize,
    AmAwait,
    AmAiter,
    AmAnext,
    NbAdd,
    NbSubtract,
    NbMultiply,
    NbRemainder,
    NbDivmod,
    NbPower,
    NbNegative,
    NbPositive,
    NbAbsolute,
    NbBool,
    NbInvert,
    NbLshift,
    NbRshift,
    NbAnd,
    NbXor,
    NbOr,
    NbInt,
    NbFloat,
    NbInplaceAdd,
    NbInplaceSubtract,
    NbInplaceMultiply,
    NbInplaceRemainder,
    NbInplacePower,
    NbInplaceLshift,
    NbInplaceRshift,
    NbInplaceAnd,
    NbInplaceXor,
    NbInplaceOr,
    NbFloorDivide,
    NbTrueDivide,
    NbInplaceFloorDivide,
    NbInplaceTrueDivide,
    NbIndex,
    NbMatrixMultiply,
    NbInplaceMatrixMultiply,
    MpLength,
    MpSubscript,
    MpAssSubscript,
    SqLength,
    SqConcat,
    SqRepeat,
    SqItem,
    SqAssItem,
    SqContains,
    SqInplaceConcat,
    SqInplaceRepeat,
    BfGetbuffer,
    BfReleasebuffer,
}

pub struct SlotDef {
    pub name: &'static str,
    pub slot: Slot,
    pub wrapper: Option<WrapperFn>,
    pub doc: &'static str,
    /// compare op for richcompare wrappers
    pub op: c_int,
    pub kw: bool,
}

macro_rules! sd {
    ($n:expr, $s:ident, $w:expr, $doc:expr) => {
        SlotDef { name: $n, slot: Slot::$s, wrapper: $w, doc: $doc, op: 0, kw: false }
    };
    ($n:expr, $s:ident, $w:expr, $doc:expr, op=$op:expr) => {
        SlotDef { name: $n, slot: Slot::$s, wrapper: $w, doc: $doc, op: $op, kw: false }
    };
    ($n:expr, $s:ident, $w:expr, $doc:expr, kw) => {
        SlotDef { name: $n, slot: Slot::$s, wrapper: $w, doc: $doc, op: 0, kw: true }
    };
}

pub static SLOTDEFS: &[SlotDef] = &[
    sd!("__getattribute__", TpGetattro, Some(wrap_getattr), "__getattribute__($self, name, /)\n--\n\nReturn getattr(self, name)."),
    sd!("__getattr__", TpGetattro, None, ""),
    sd!("__setattr__", TpSetattro, Some(wrap_setattr), "__setattr__($self, name, value, /)\n--\n\nImplement setattr(self, name, value)."),
    sd!("__delattr__", TpSetattro, Some(wrap_delattr), "__delattr__($self, name, /)\n--\n\nImplement delattr(self, name)."),
    sd!("__repr__", TpRepr, Some(wrap_unaryfunc), "__repr__($self, /)\n--\n\nReturn repr(self)."),
    sd!("__hash__", TpHash, Some(wrap_hashfunc), "__hash__($self, /)\n--\n\nReturn hash(self)."),
    sd!("__call__", TpCall, Some(wrap_call), "__call__($self, /, *args, **kwargs)\n--\n\nCall self as a function.", kw),
    sd!("__str__", TpStr, Some(wrap_unaryfunc), "__str__($self, /)\n--\n\nReturn str(self)."),
    sd!("__lt__", TpRichcompare, Some(wrap_richcmp), "__lt__($self, value, /)\n--\n\nReturn self<value.", op = Py_LT),
    sd!("__le__", TpRichcompare, Some(wrap_richcmp), "__le__($self, value, /)\n--\n\nReturn self<=value.", op = Py_LE),
    sd!("__eq__", TpRichcompare, Some(wrap_richcmp), "__eq__($self, value, /)\n--\n\nReturn self==value.", op = Py_EQ),
    sd!("__ne__", TpRichcompare, Some(wrap_richcmp), "__ne__($self, value, /)\n--\n\nReturn self!=value.", op = Py_NE),
    sd!("__gt__", TpRichcompare, Some(wrap_richcmp), "__gt__($self, value, /)\n--\n\nReturn self>value.", op = Py_GT),
    sd!("__ge__", TpRichcompare, Some(wrap_richcmp), "__ge__($self, value, /)\n--\n\nReturn self>=value.", op = Py_GE),
    sd!("__iter__", TpIter, Some(wrap_unaryfunc), "__iter__($self, /)\n--\n\nImplement iter(self)."),
    sd!("__next__", TpIternext, Some(wrap_next), "__next__($self, /)\n--\n\nImplement next(self)."),
    sd!("__get__", TpDescrGet, Some(wrap_descr_get), "__get__($self, instance, owner=None, /)\n--\n\nReturn an attribute of instance, which is of type owner."),
    sd!("__set__", TpDescrSet, Some(wrap_descr_set), "__set__($self, instance, value, /)\n--\n\nSet an attribute of instance to value."),
    sd!("__delete__", TpDescrSet, Some(wrap_descr_delete), "__delete__($self, instance, /)\n--\n\nDelete an attribute of instance."),
    sd!("__init__", TpInit, Some(wrap_init), "__init__($self, /, *args, **kwargs)\n--\n\nInitialize self.  See help(type(self)) for accurate signature.", kw),
    sd!("__new__", TpNew, None, ""),
    sd!("__del__", TpFinalize, Some(wrap_del), "__del__($self, /)\n--\n\nCalled when the instance is about to be destroyed."),
    sd!("__await__", AmAwait, Some(wrap_unaryfunc), "__await__($self, /)\n--\n\nReturn an iterator to be used in await expression."),
    sd!("__aiter__", AmAiter, Some(wrap_unaryfunc), "__aiter__($self, /)\n--\n\nReturn an awaitable, that resolves in asynchronous iterator."),
    sd!("__anext__", AmAnext, Some(wrap_unaryfunc), "__anext__($self, /)\n--\n\nReturn a value or raise StopAsyncIteration."),
    sd!("__add__", NbAdd, Some(wrap_binary_l), "__add__($self, value, /)\n--\n\nReturn self+value."),
    sd!("__radd__", NbAdd, Some(wrap_binary_r), "__radd__($self, value, /)\n--\n\nReturn value+self."),
    sd!("__sub__", NbSubtract, Some(wrap_binary_l), "__sub__($self, value, /)\n--\n\nReturn self-value."),
    sd!("__rsub__", NbSubtract, Some(wrap_binary_r), "__rsub__($self, value, /)\n--\n\nReturn value-self."),
    sd!("__mul__", NbMultiply, Some(wrap_binary_l), "__mul__($self, value, /)\n--\n\nReturn self*value."),
    sd!("__rmul__", NbMultiply, Some(wrap_binary_r), "__rmul__($self, value, /)\n--\n\nReturn value*self."),
    sd!("__mod__", NbRemainder, Some(wrap_binary_l), "__mod__($self, value, /)\n--\n\nReturn self%value."),
    sd!("__rmod__", NbRemainder, Some(wrap_binary_r), "__rmod__($self, value, /)\n--\n\nReturn value%self."),
    sd!("__divmod__", NbDivmod, Some(wrap_binary_l), "__divmod__($self, value, /)\n--\n\nReturn divmod(self, value)."),
    sd!("__rdivmod__", NbDivmod, Some(wrap_binary_r), "__rdivmod__($self, value, /)\n--\n\nReturn divmod(value, self)."),
    sd!("__pow__", NbPower, Some(wrap_ternary), "__pow__($self, value, mod=None, /)\n--\n\nReturn pow(self, value, mod)."),
    sd!("__rpow__", NbPower, Some(wrap_ternary_r), "__rpow__($self, value, mod=None, /)\n--\n\nReturn pow(value, self, mod)."),
    sd!("__neg__", NbNegative, Some(wrap_unaryfunc), "__neg__($self, /)\n--\n\n-self"),
    sd!("__pos__", NbPositive, Some(wrap_unaryfunc), "__pos__($self, /)\n--\n\n+self"),
    sd!("__abs__", NbAbsolute, Some(wrap_unaryfunc), "__abs__($self, /)\n--\n\nabs(self)"),
    sd!("__bool__", NbBool, Some(wrap_inquirypred), "__bool__($self, /)\n--\n\nTrue if self else False"),
    sd!("__invert__", NbInvert, Some(wrap_unaryfunc), "__invert__($self, /)\n--\n\n~self"),
    sd!("__lshift__", NbLshift, Some(wrap_binary_l), "__lshift__($self, value, /)\n--\n\nReturn self<<value."),
    sd!("__rlshift__", NbLshift, Some(wrap_binary_r), "__rlshift__($self, value, /)\n--\n\nReturn value<<self."),
    sd!("__rshift__", NbRshift, Some(wrap_binary_l), "__rshift__($self, value, /)\n--\n\nReturn self>>value."),
    sd!("__rrshift__", NbRshift, Some(wrap_binary_r), "__rrshift__($self, value, /)\n--\n\nReturn value>>self."),
    sd!("__and__", NbAnd, Some(wrap_binary_l), "__and__($self, value, /)\n--\n\nReturn self&value."),
    sd!("__rand__", NbAnd, Some(wrap_binary_r), "__rand__($self, value, /)\n--\n\nReturn value&self."),
    sd!("__xor__", NbXor, Some(wrap_binary_l), "__xor__($self, value, /)\n--\n\nReturn self^value."),
    sd!("__rxor__", NbXor, Some(wrap_binary_r), "__rxor__($self, value, /)\n--\n\nReturn value^self."),
    sd!("__or__", NbOr, Some(wrap_binary_l), "__or__($self, value, /)\n--\n\nReturn self|value."),
    sd!("__ror__", NbOr, Some(wrap_binary_r), "__ror__($self, value, /)\n--\n\nReturn value|self."),
    sd!("__int__", NbInt, Some(wrap_unaryfunc), "__int__($self, /)\n--\n\nint(self)"),
    sd!("__float__", NbFloat, Some(wrap_unaryfunc), "__float__($self, /)\n--\n\nfloat(self)"),
    sd!("__iadd__", NbInplaceAdd, Some(wrap_binary_l), "__iadd__($self, value, /)\n--\n\nReturn self+=value."),
    sd!("__isub__", NbInplaceSubtract, Some(wrap_binary_l), "__isub__($self, value, /)\n--\n\nReturn self-=value."),
    sd!("__imul__", NbInplaceMultiply, Some(wrap_binary_l), "__imul__($self, value, /)\n--\n\nReturn self*=value."),
    sd!("__imod__", NbInplaceRemainder, Some(wrap_binary_l), "__imod__($self, value, /)\n--\n\nReturn self%=value."),
    sd!("__ipow__", NbInplacePower, Some(wrap_ternary), "__ipow__($self, value, /)\n--\n\nReturn self**=value."),
    sd!("__ilshift__", NbInplaceLshift, Some(wrap_binary_l), "__ilshift__($self, value, /)\n--\n\nReturn self<<=value."),
    sd!("__irshift__", NbInplaceRshift, Some(wrap_binary_l), "__irshift__($self, value, /)\n--\n\nReturn self>>=value."),
    sd!("__iand__", NbInplaceAnd, Some(wrap_binary_l), "__iand__($self, value, /)\n--\n\nReturn self&=value."),
    sd!("__ixor__", NbInplaceXor, Some(wrap_binary_l), "__ixor__($self, value, /)\n--\n\nReturn self^=value."),
    sd!("__ior__", NbInplaceOr, Some(wrap_binary_l), "__ior__($self, value, /)\n--\n\nReturn self|=value."),
    sd!("__floordiv__", NbFloorDivide, Some(wrap_binary_l), "__floordiv__($self, value, /)\n--\n\nReturn self//value."),
    sd!("__rfloordiv__", NbFloorDivide, Some(wrap_binary_r), "__rfloordiv__($self, value, /)\n--\n\nReturn value//self."),
    sd!("__truediv__", NbTrueDivide, Some(wrap_binary_l), "__truediv__($self, value, /)\n--\n\nReturn self/value."),
    sd!("__rtruediv__", NbTrueDivide, Some(wrap_binary_r), "__rtruediv__($self, value, /)\n--\n\nReturn value/self."),
    sd!("__ifloordiv__", NbInplaceFloorDivide, Some(wrap_binary_l), "__ifloordiv__($self, value, /)\n--\n\nReturn self//=value."),
    sd!("__itruediv__", NbInplaceTrueDivide, Some(wrap_binary_l), "__itruediv__($self, value, /)\n--\n\nReturn self/=value."),
    sd!("__index__", NbIndex, Some(wrap_unaryfunc), "__index__($self, /)\n--\n\nReturn self converted to an integer, if self is suitable for use as an index into a list."),
    sd!("__matmul__", NbMatrixMultiply, Some(wrap_binary_l), "__matmul__($self, value, /)\n--\n\nReturn self@value."),
    sd!("__rmatmul__", NbMatrixMultiply, Some(wrap_binary_r), "__rmatmul__($self, value, /)\n--\n\nReturn value@self."),
    sd!("__imatmul__", NbInplaceMatrixMultiply, Some(wrap_binary_l), "__imatmul__($self, value, /)\n--\n\nReturn self@=value."),
    sd!("__len__", MpLength, Some(wrap_lenfunc), "__len__($self, /)\n--\n\nReturn len(self)."),
    sd!("__getitem__", MpSubscript, Some(wrap_binary_l), "__getitem__($self, key, /)\n--\n\nReturn self[key]."),
    sd!("__setitem__", MpAssSubscript, Some(wrap_objobjargproc), "__setitem__($self, key, value, /)\n--\n\nSet self[key] to value."),
    sd!("__delitem__", MpAssSubscript, Some(wrap_delitem), "__delitem__($self, key, /)\n--\n\nDelete self[key]."),
    sd!("__len__", SqLength, Some(wrap_lenfunc), "__len__($self, /)\n--\n\nReturn len(self)."),
    sd!("__add__", SqConcat, Some(wrap_binary_l), "__add__($self, value, /)\n--\n\nReturn self+value."),
    sd!("__mul__", SqRepeat, Some(wrap_indexargfunc), "__mul__($self, value, /)\n--\n\nReturn self*value."),
    sd!("__rmul__", SqRepeat, Some(wrap_indexargfunc), "__rmul__($self, value, /)\n--\n\nReturn value*self."),
    sd!("__getitem__", SqItem, Some(wrap_sq_item), "__getitem__($self, key, /)\n--\n\nReturn self[key]."),
    sd!("__setitem__", SqAssItem, Some(wrap_sq_setitem), "__setitem__($self, key, value, /)\n--\n\nSet self[key] to value."),
    sd!("__delitem__", SqAssItem, Some(wrap_sq_delitem), "__delitem__($self, key, /)\n--\n\nDelete self[key]."),
    sd!("__contains__", SqContains, Some(wrap_objobjproc), "__contains__($self, key, /)\n--\n\nReturn bool(key in self)."),
    sd!("__iadd__", SqInplaceConcat, Some(wrap_binary_l), "__iadd__($self, value, /)\n--\n\nImplement self+=value."),
    sd!("__imul__", SqInplaceRepeat, Some(wrap_indexargfunc), "__imul__($self, value, /)\n--\n\nImplement self*=value."),
    sd!("__buffer__", BfGetbuffer, Some(wrap_buffer), "__buffer__($self, flags, /)\n--\n\nReturn a buffer object that exposes the underlying memory of the object."),
    sd!("__release_buffer__", BfReleasebuffer, Some(wrap_releasebuffer), "__release_buffer__($self, buffer, /)\n--\n\nRelease the buffer object that exposes the underlying memory of the object."),
];

// ------------------------------------------------------------ slot access

unsafe fn sub_ptr<T>(p: *mut T) -> Option<&'static mut T> {
    unsafe { if p.is_null() { None } else { Some(&mut *p) } }
}

macro_rules! fp {
    ($e:expr) => {
        match $e {
            Some(f) => f as *mut c_void,
            None => std::ptr::null_mut(),
        }
    };
}

impl Slot {
    /// Read the slot as an opaque pointer (null if unset).
    pub fn get(self, tp: *mut PyTypeObject) -> *mut c_void {
        unsafe {
            let t = &*tp;
            use Slot::*;
            match self {
                TpGetattro => fp!(t.tp_getattro),
                TpSetattro => fp!(t.tp_setattro),
                TpRepr => fp!(t.tp_repr),
                TpHash => fp!(t.tp_hash),
                TpCall => fp!(t.tp_call),
                TpStr => fp!(t.tp_str),
                TpRichcompare => fp!(t.tp_richcompare),
                TpIter => fp!(t.tp_iter),
                TpIternext => fp!(t.tp_iternext),
                TpDescrGet => fp!(t.tp_descr_get),
                TpDescrSet => fp!(t.tp_descr_set),
                TpInit => fp!(t.tp_init),
                TpNew => fp!(t.tp_new),
                TpFinalize => fp!(t.tp_finalize),
                AmAwait | AmAiter | AmAnext => match sub_ptr(t.tp_as_async) {
                    None => std::ptr::null_mut(),
                    Some(a) => match self {
                        AmAwait => fp!(a.am_await),
                        AmAiter => fp!(a.am_aiter),
                        _ => fp!(a.am_anext),
                    },
                },
                MpLength | MpSubscript | MpAssSubscript => match sub_ptr(t.tp_as_mapping) {
                    None => std::ptr::null_mut(),
                    Some(m) => match self {
                        MpLength => fp!(m.mp_length),
                        MpSubscript => fp!(m.mp_subscript),
                        _ => fp!(m.mp_ass_subscript),
                    },
                },
                SqLength | SqConcat | SqRepeat | SqItem | SqAssItem | SqContains | SqInplaceConcat | SqInplaceRepeat => {
                    match sub_ptr(t.tp_as_sequence) {
                        None => std::ptr::null_mut(),
                        Some(s) => match self {
                            SqLength => fp!(s.sq_length),
                            SqConcat => fp!(s.sq_concat),
                            SqRepeat => fp!(s.sq_repeat),
                            SqItem => fp!(s.sq_item),
                            SqAssItem => fp!(s.sq_ass_item),
                            SqContains => fp!(s.sq_contains),
                            SqInplaceConcat => fp!(s.sq_inplace_concat),
                            _ => fp!(s.sq_inplace_repeat),
                        },
                    }
                }
                BfGetbuffer | BfReleasebuffer => match sub_ptr(t.tp_as_buffer) {
                    None => std::ptr::null_mut(),
                    Some(b) => match self {
                        BfGetbuffer => fp!(b.bf_getbuffer),
                        _ => fp!(b.bf_releasebuffer),
                    },
                },
                _ => match sub_ptr(t.tp_as_number) {
                    None => std::ptr::null_mut(),
                    Some(n) => match self {
                        NbAdd => fp!(n.nb_add),
                        NbSubtract => fp!(n.nb_subtract),
                        NbMultiply => fp!(n.nb_multiply),
                        NbRemainder => fp!(n.nb_remainder),
                        NbDivmod => fp!(n.nb_divmod),
                        NbPower => fp!(n.nb_power),
                        NbNegative => fp!(n.nb_negative),
                        NbPositive => fp!(n.nb_positive),
                        NbAbsolute => fp!(n.nb_absolute),
                        NbBool => fp!(n.nb_bool),
                        NbInvert => fp!(n.nb_invert),
                        NbLshift => fp!(n.nb_lshift),
                        NbRshift => fp!(n.nb_rshift),
                        NbAnd => fp!(n.nb_and),
                        NbXor => fp!(n.nb_xor),
                        NbOr => fp!(n.nb_or),
                        NbInt => fp!(n.nb_int),
                        NbFloat => fp!(n.nb_float),
                        NbInplaceAdd => fp!(n.nb_inplace_add),
                        NbInplaceSubtract => fp!(n.nb_inplace_subtract),
                        NbInplaceMultiply => fp!(n.nb_inplace_multiply),
                        NbInplaceRemainder => fp!(n.nb_inplace_remainder),
                        NbInplacePower => fp!(n.nb_inplace_power),
                        NbInplaceLshift => fp!(n.nb_inplace_lshift),
                        NbInplaceRshift => fp!(n.nb_inplace_rshift),
                        NbInplaceAnd => fp!(n.nb_inplace_and),
                        NbInplaceXor => fp!(n.nb_inplace_xor),
                        NbInplaceOr => fp!(n.nb_inplace_or),
                        NbFloorDivide => fp!(n.nb_floor_divide),
                        NbTrueDivide => fp!(n.nb_true_divide),
                        NbInplaceFloorDivide => fp!(n.nb_inplace_floor_divide),
                        NbInplaceTrueDivide => fp!(n.nb_inplace_true_divide),
                        NbIndex => fp!(n.nb_index),
                        NbMatrixMultiply => fp!(n.nb_matrix_multiply),
                        NbInplaceMatrixMultiply => fp!(n.nb_inplace_matrix_multiply),
                        _ => std::ptr::null_mut(),
                    },
                },
            }
        }
    }

    /// Write the slot (heap types have their sub-structs embedded).
    pub fn set(self, tp: *mut PyTypeObject, f: *mut c_void) {
        unsafe {
            let t = &mut *tp;
            use Slot::*;
            macro_rules! put {
                ($place:expr) => {
                    $place = std::mem::transmute::<*mut c_void, _>(f)
                };
            }
            match self {
                TpGetattro => put!(t.tp_getattro),
                TpSetattro => put!(t.tp_setattro),
                TpRepr => put!(t.tp_repr),
                TpHash => put!(t.tp_hash),
                TpCall => put!(t.tp_call),
                TpStr => put!(t.tp_str),
                TpRichcompare => put!(t.tp_richcompare),
                TpIter => put!(t.tp_iter),
                TpIternext => put!(t.tp_iternext),
                TpDescrGet => put!(t.tp_descr_get),
                TpDescrSet => put!(t.tp_descr_set),
                TpInit => put!(t.tp_init),
                TpNew => put!(t.tp_new),
                TpFinalize => put!(t.tp_finalize),
                AmAwait | AmAiter | AmAnext => {
                    if t.tp_as_async.is_null() {
                        t.tp_as_async = Box::into_raw(Box::new(PyAsyncMethods::ZERO));
                    }
                    let a = &mut *t.tp_as_async;
                    match self {
                        AmAwait => put!(a.am_await),
                        AmAiter => put!(a.am_aiter),
                        _ => put!(a.am_anext),
                    }
                }
                MpLength | MpSubscript | MpAssSubscript => {
                    if t.tp_as_mapping.is_null() {
                        t.tp_as_mapping = Box::into_raw(Box::new(PyMappingMethods::ZERO));
                    }
                    let m = &mut *t.tp_as_mapping;
                    match self {
                        MpLength => put!(m.mp_length),
                        MpSubscript => put!(m.mp_subscript),
                        _ => put!(m.mp_ass_subscript),
                    }
                }
                SqLength | SqConcat | SqRepeat | SqItem | SqAssItem | SqContains | SqInplaceConcat | SqInplaceRepeat => {
                    if t.tp_as_sequence.is_null() {
                        t.tp_as_sequence = Box::into_raw(Box::new(PySequenceMethods::ZERO));
                    }
                    let s = &mut *t.tp_as_sequence;
                    match self {
                        SqLength => put!(s.sq_length),
                        SqConcat => put!(s.sq_concat),
                        SqRepeat => put!(s.sq_repeat),
                        SqItem => put!(s.sq_item),
                        SqAssItem => put!(s.sq_ass_item),
                        SqContains => put!(s.sq_contains),
                        SqInplaceConcat => put!(s.sq_inplace_concat),
                        _ => put!(s.sq_inplace_repeat),
                    }
                }
                BfGetbuffer | BfReleasebuffer => {
                    if t.tp_as_buffer.is_null() {
                        t.tp_as_buffer = Box::into_raw(Box::new(PyBufferProcs::ZERO));
                    }
                    let b = &mut *t.tp_as_buffer;
                    match self {
                        BfGetbuffer => put!(b.bf_getbuffer),
                        _ => put!(b.bf_releasebuffer),
                    }
                }
                _ => {
                    if t.tp_as_number.is_null() {
                        t.tp_as_number = Box::into_raw(Box::new(PyNumberMethods::ZERO));
                    }
                    let n = &mut *t.tp_as_number;
                    match self {
                        NbAdd => put!(n.nb_add),
                        NbSubtract => put!(n.nb_subtract),
                        NbMultiply => put!(n.nb_multiply),
                        NbRemainder => put!(n.nb_remainder),
                        NbDivmod => put!(n.nb_divmod),
                        NbPower => put!(n.nb_power),
                        NbNegative => put!(n.nb_negative),
                        NbPositive => put!(n.nb_positive),
                        NbAbsolute => put!(n.nb_absolute),
                        NbBool => put!(n.nb_bool),
                        NbInvert => put!(n.nb_invert),
                        NbLshift => put!(n.nb_lshift),
                        NbRshift => put!(n.nb_rshift),
                        NbAnd => put!(n.nb_and),
                        NbXor => put!(n.nb_xor),
                        NbOr => put!(n.nb_or),
                        NbInt => put!(n.nb_int),
                        NbFloat => put!(n.nb_float),
                        NbInplaceAdd => put!(n.nb_inplace_add),
                        NbInplaceSubtract => put!(n.nb_inplace_subtract),
                        NbInplaceMultiply => put!(n.nb_inplace_multiply),
                        NbInplaceRemainder => put!(n.nb_inplace_remainder),
                        NbInplacePower => put!(n.nb_inplace_power),
                        NbInplaceLshift => put!(n.nb_inplace_lshift),
                        NbInplaceRshift => put!(n.nb_inplace_rshift),
                        NbInplaceAnd => put!(n.nb_inplace_and),
                        NbInplaceXor => put!(n.nb_inplace_xor),
                        NbInplaceOr => put!(n.nb_inplace_or),
                        NbFloorDivide => put!(n.nb_floor_divide),
                        NbTrueDivide => put!(n.nb_true_divide),
                        NbInplaceFloorDivide => put!(n.nb_inplace_floor_divide),
                        NbInplaceTrueDivide => put!(n.nb_inplace_true_divide),
                        NbIndex => put!(n.nb_index),
                        NbMatrixMultiply => put!(n.nb_matrix_multiply),
                        NbInplaceMatrixMultiply => put!(n.nb_inplace_matrix_multiply),
                        _ => {}
                    }
                }
            }
        }
    }

    /// The generic slot implementation used by heap types defining the dunder.
    pub fn generic(self) -> *mut c_void {
        use Slot::*;
        macro_rules! g {
            ($f:expr) => {
                $f as *mut c_void
            };
        }
        match self {
            TpGetattro => g!(slot_tp_getattr_hook as unsafe extern "C" fn(_, _) -> _),
            TpSetattro => g!(slot_tp_setattro as unsafe extern "C" fn(_, _, _) -> _),
            TpRepr => g!(slot_tp_repr as unsafe extern "C" fn(_) -> _),
            TpHash => g!(slot_tp_hash as unsafe extern "C" fn(_) -> _),
            TpCall => g!(slot_tp_call as unsafe extern "C" fn(_, _, _) -> _),
            TpStr => g!(slot_tp_str as unsafe extern "C" fn(_) -> _),
            TpRichcompare => g!(slot_tp_richcompare as unsafe extern "C" fn(_, _, _) -> _),
            TpIter => g!(slot_tp_iter as unsafe extern "C" fn(_) -> _),
            TpIternext => g!(slot_tp_iternext as unsafe extern "C" fn(_) -> _),
            TpDescrGet => g!(slot_tp_descr_get as unsafe extern "C" fn(_, _, _) -> _),
            TpDescrSet => g!(slot_tp_descr_set as unsafe extern "C" fn(_, _, _) -> _),
            TpInit => g!(slot_tp_init as unsafe extern "C" fn(_, _, _) -> _),
            TpNew => g!(slot_tp_new as unsafe extern "C" fn(_, _, _) -> _),
            TpFinalize => g!(slot_tp_finalize as unsafe extern "C" fn(_)),
            AmAwait => g!(slot_am_await as unsafe extern "C" fn(_) -> _),
            AmAiter => g!(slot_am_aiter as unsafe extern "C" fn(_) -> _),
            AmAnext => g!(slot_am_anext as unsafe extern "C" fn(_) -> _),
            NbAdd => g!(slot_nb_add as unsafe extern "C" fn(_, _) -> _),
            NbSubtract => g!(slot_nb_subtract as unsafe extern "C" fn(_, _) -> _),
            NbMultiply => g!(slot_nb_multiply as unsafe extern "C" fn(_, _) -> _),
            NbRemainder => g!(slot_nb_remainder as unsafe extern "C" fn(_, _) -> _),
            NbDivmod => g!(slot_nb_divmod as unsafe extern "C" fn(_, _) -> _),
            NbPower => g!(slot_nb_power as unsafe extern "C" fn(_, _, _) -> _),
            NbNegative => g!(slot_nb_negative as unsafe extern "C" fn(_) -> _),
            NbPositive => g!(slot_nb_positive as unsafe extern "C" fn(_) -> _),
            NbAbsolute => g!(slot_nb_absolute as unsafe extern "C" fn(_) -> _),
            NbBool => g!(slot_nb_bool as unsafe extern "C" fn(_) -> _),
            NbInvert => g!(slot_nb_invert as unsafe extern "C" fn(_) -> _),
            NbLshift => g!(slot_nb_lshift as unsafe extern "C" fn(_, _) -> _),
            NbRshift => g!(slot_nb_rshift as unsafe extern "C" fn(_, _) -> _),
            NbAnd => g!(slot_nb_and as unsafe extern "C" fn(_, _) -> _),
            NbXor => g!(slot_nb_xor as unsafe extern "C" fn(_, _) -> _),
            NbOr => g!(slot_nb_or as unsafe extern "C" fn(_, _) -> _),
            NbInt => g!(slot_nb_int as unsafe extern "C" fn(_) -> _),
            NbFloat => g!(slot_nb_float as unsafe extern "C" fn(_) -> _),
            NbInplaceAdd => g!(slot_nb_inplace_add as unsafe extern "C" fn(_, _) -> _),
            NbInplaceSubtract => g!(slot_nb_inplace_subtract as unsafe extern "C" fn(_, _) -> _),
            NbInplaceMultiply => g!(slot_nb_inplace_multiply as unsafe extern "C" fn(_, _) -> _),
            NbInplaceRemainder => g!(slot_nb_inplace_remainder as unsafe extern "C" fn(_, _) -> _),
            NbInplacePower => g!(slot_nb_inplace_power as unsafe extern "C" fn(_, _, _) -> _),
            NbInplaceLshift => g!(slot_nb_inplace_lshift as unsafe extern "C" fn(_, _) -> _),
            NbInplaceRshift => g!(slot_nb_inplace_rshift as unsafe extern "C" fn(_, _) -> _),
            NbInplaceAnd => g!(slot_nb_inplace_and as unsafe extern "C" fn(_, _) -> _),
            NbInplaceXor => g!(slot_nb_inplace_xor as unsafe extern "C" fn(_, _) -> _),
            NbInplaceOr => g!(slot_nb_inplace_or as unsafe extern "C" fn(_, _) -> _),
            NbFloorDivide => g!(slot_nb_floor_divide as unsafe extern "C" fn(_, _) -> _),
            NbTrueDivide => g!(slot_nb_true_divide as unsafe extern "C" fn(_, _) -> _),
            NbInplaceFloorDivide => g!(slot_nb_inplace_floor_divide as unsafe extern "C" fn(_, _) -> _),
            NbInplaceTrueDivide => g!(slot_nb_inplace_true_divide as unsafe extern "C" fn(_, _) -> _),
            NbIndex => g!(slot_nb_index as unsafe extern "C" fn(_) -> _),
            NbMatrixMultiply => g!(slot_nb_matrix_multiply as unsafe extern "C" fn(_, _) -> _),
            NbInplaceMatrixMultiply => g!(slot_nb_inplace_matrix_multiply as unsafe extern "C" fn(_, _) -> _),
            MpLength => g!(slot_mp_length as unsafe extern "C" fn(_) -> _),
            MpSubscript => g!(slot_mp_subscript as unsafe extern "C" fn(_, _) -> _),
            MpAssSubscript => g!(slot_mp_ass_subscript as unsafe extern "C" fn(_, _, _) -> _),
            SqLength => g!(slot_mp_length as unsafe extern "C" fn(_) -> _),
            SqConcat => std::ptr::null_mut(),
            SqRepeat => std::ptr::null_mut(),
            SqItem => g!(slot_sq_item as unsafe extern "C" fn(_, _) -> _),
            SqAssItem => g!(slot_sq_ass_item as unsafe extern "C" fn(_, _, _) -> _),
            SqContains => g!(slot_sq_contains as unsafe extern "C" fn(_, _) -> _),
            SqInplaceConcat => std::ptr::null_mut(),
            SqInplaceRepeat => std::ptr::null_mut(),
            BfGetbuffer => g!(slot_bf_getbuffer as unsafe extern "C" fn(_, _, _) -> _),
            BfReleasebuffer => g!(slot_bf_releasebuffer as unsafe extern "C" fn(_, _)),
        }
    }
}

// ------------------------------------------------------------ add_operators

pub fn add_operators(tp: *mut PyTypeObject) -> PyResult<()> {
    let d = type_dict(tp);
    for (i, sd) in SLOTDEFS.iter().enumerate() {
        let wrapper = match sd.wrapper {
            Some(w) => w,
            None => continue,
        };
        let f = sd.slot.get(tp);
        if f.is_null() {
            continue;
        }
        let nm = pystr::intern_str(sd.name);
        if dict::get_item_str(d, &nm).is_some() {
            continue;
        }
        // A generic slot function only dispatches to a dunder found on the
        // MRO; wrapping it would shadow that dunder and recurse forever.
        if f == sd.slot.generic() {
            continue;
        }
        if sd.slot == Slot::TpHash && f == hash_not_implemented as *mut c_void {
            dict::set_item(d, &nm, none::none())?;
            continue;
        }
        // only add if this type defines the slot itself (not identical to its base's)
        let base = unsafe { (*tp).tp_base };
        if is_heap_type(tp) {
            // inherited from any base along the MRO (multiple inheritance copies
            // slots from non-primary bases too): the base exposes the wrapper
            let mro = unsafe { (*tp).tp_mro };
            if !mro.is_null() {
                let inherited = crate::types::tuple::tuple_items(unsafe { &*mro })
                    .iter()
                    .skip(1)
                    .any(|b| sd.slot.get(crate::types::typeobject::as_type(b)) == f);
                if inherited {
                    continue;
                }
            }
        }
        if !base.is_null() && sd.slot.get(base) == f && !is_heap_type(tp) {
            // inherited: the base's dict already exposes it
            if crate::types::typeobject::lookup(base, &nm).is_some() {
                continue;
            }
        }
        let _ = wrapper;
        let descr = crate::types::descr::new_wrapper_descr(tp, i, f);
        dict::set_item(d, &nm, &descr)?;
    }
    // __new__ as a builtin staticmethod-like function
    unsafe {
        // Types that cannot be instantiated get no __new__ of their own (they
        // show object.__new__ through the MRO, as in CPython).
        if (*tp).tp_new.is_some()
            && (*tp).tp_flags & crate::ffi::Py_TPFLAGS_DISALLOW_INSTANTIATION == 0
            && dict::get_item_str(d, name!("__new__")).is_none()
        {
            let base = (*tp).tp_base;
            if base.is_null() || (*base).tp_new != (*tp).tp_new || tp == object_type() {
                let f = crate::types::descr::new_tp_new_wrapper(tp);
                dict::set_item(d, name!("__new__"), &f)?;
            }
        }
    }
    Ok(())
}

/// Slot setup for a class just created by `type_new` (after `type_ready`).
/// perf-runtime (startup): with a single base, every slot whose dunder names do not
/// appear in the class's own dict was just inherited from the (already fixed-up) base
/// by type_ready, so only the slots named in the dict need recomputing.
pub fn fixup_slots_new_type(tp: *mut PyTypeObject) {
    unsafe {
        let bases = (*tp).tp_bases;
        let d = (*tp).tp_dict;
        if !bases.is_null() && tuple::tuple_len(&*bases) == 1 && !d.is_null() && dict::is_dict_exact(&*d) {
            static BY_NAME: std::sync::OnceLock<std::collections::HashMap<usize, Vec<Slot>>> = std::sync::OnceLock::new();
            let by_name = BY_NAME.get_or_init(|| {
                let mut m: std::collections::HashMap<usize, Vec<Slot>> = std::collections::HashMap::new();
                for sd in SLOTDEFS.iter() {
                    let nm = pystr::intern_str(sd.name).into_raw() as usize; // immortal
                    let e = m.entry(nm).or_default();
                    if !e.contains(&sd.slot) {
                        e.push(sd.slot);
                    }
                }
                m
            });
            let mut todo: Vec<Slot> = Vec::new();
            let mut fallback = false;
            for k in dict::keys_vec(&*d) {
                if !pystr::is_str_exact(&k) {
                    if pystr::is_str(&k) {
                        fallback = true; // str-subclass key: recompute everything
                        break;
                    }
                    continue;
                }
                // a name without an interned twin cannot be a slot name
                let Some(kp) = pystr::lookup_interned(&k) else { continue };
                if let Some(slots) = by_name.get(&(kp as usize)) {
                    for &s in slots {
                        if !todo.contains(&s) {
                            todo.push(s);
                        }
                    }
                }
            }
            if !fallback {
                // tp_richcompare and tp_hash are inherited as a pair: if the dict
                // defines only one of them, type_ready inherited neither, so the
                // other must be recomputed from the MRO as well (grind-lib).
                for (a, b) in [(Slot::TpHash, Slot::TpRichcompare), (Slot::TpRichcompare, Slot::TpHash)] {
                    if todo.contains(&a) && !todo.contains(&b) {
                        todo.push(b);
                    }
                }
                for s in todo {
                    update_one_slot(tp, s);
                }
                return;
            }
        }
    }
    fixup_slots(tp);
}

/// For a new heap type: set every slot from the dunders found along the MRO.
pub fn fixup_slots(tp: *mut PyTypeObject) {
    let mut done: Vec<Slot> = Vec::new();
    for sd in SLOTDEFS.iter() {
        if done.contains(&sd.slot) {
            continue;
        }
        done.push(sd.slot);
        update_one_slot(tp, sd.slot);
    }
}

/// Recompute slots affected by assigning `name` on `tp` (and its subclasses).
pub fn update_slot(tp: *mut PyTypeObject, attr: &PyObject) {
    let mut slots: Vec<Slot> = Vec::new();
    for sd in SLOTDEFS.iter() {
        if pystr::str_eq_ascii(attr, sd.name) && !slots.contains(&sd.slot) {
            slots.push(sd.slot);
        }
    }
    if slots.is_empty() {
        return;
    }
    fn rec(tp: *mut PyTypeObject, slots: &[Slot]) {
        for &s in slots {
            update_one_slot(tp, s);
        }
        for sub in subclasses(tp) {
            if is_heap_type(sub) {
                rec(sub, slots);
            }
        }
    }
    rec(tp, &slots);
}

/// SLOTDEFS grouped by slot with their interned names, built once (class creation
/// runs update_one_slot for every slot; rebuilding these lists dominated it).
fn slot_names(slot: Slot) -> &'static [(&'static SlotDef, usize)] {
    static T: std::sync::OnceLock<Vec<(Slot, Vec<(&'static SlotDef, usize)>)>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut v: Vec<(Slot, Vec<(&'static SlotDef, usize)>)> = Vec::new();
        for sd in SLOTDEFS.iter() {
            let nm = pystr::intern_str(sd.name).into_raw() as usize; // interned: immortal
            match v.iter_mut().find(|(s, _)| *s == sd.slot) {
                Some((_, l)) => l.push((sd, nm)),
                None => v.push((sd.slot, vec![(sd, nm)])),
            }
        }
        v
    });
    t.iter().find(|(s, _)| *s == slot).map(|(_, l)| l.as_slice()).unwrap_or(&[])
}

fn update_one_slot(tp: *mut PyTypeObject, slot: Slot) {
    // Find the first dunder (in MRO order, considering all names mapping to this slot).
    let names = slot_names(slot);
    let mut chosen: *mut c_void = std::ptr::null_mut();
    let mut use_generic = false;
    let mut any = false;
    for &(sd, nmp) in names {
        let _ = sd;
        let nm: &PyObject = unsafe { &*(nmp as *mut PyObject) };
        let descr = match lookup(tp, nm) {
            Some(d) => d,
            None => continue,
        };
        any = true;
        if slot == Slot::TpHash && descr.is(none::none()) {
            if chosen.is_null() && !use_generic {
                chosen = hash_not_implemented as *mut c_void;
            }
            continue;
        }
        if slot == Slot::TpNew {
            // __new__ is a staticmethod wrapping tp_new_wrapper of a builtin type
            if let Some(t) = crate::types::descr::tp_new_wrapper_type(&descr) {
                if !use_generic && (chosen.is_null() || chosen == unsafe { (*t).tp_new.map_or(std::ptr::null_mut(), |f| f as *mut c_void) }) {
                    chosen = unsafe { (*t).tp_new.map_or(std::ptr::null_mut(), |f| f as *mut c_void) };
                    continue;
                }
            }
            use_generic = true;
            continue;
        }
        if let Some((wslot, wrapped, dtype)) = crate::types::descr::wrapper_descr_info(&descr) {
            if wslot == slot && is_subtype(tp, dtype) {
                if chosen.is_null() && !use_generic {
                    chosen = wrapped;
                } else if chosen != wrapped {
                    use_generic = true;
                }
                continue;
            }
        }
        use_generic = true;
    }
    let f = if use_generic {
        slot.generic()
    } else if any {
        chosen
    } else {
        std::ptr::null_mut()
    };
    // Sequence slots that have no generic version: keep whatever (null).
    if use_generic && f.is_null() {
        slot.set(tp, std::ptr::null_mut());
        return;
    }
    slot.set(tp, f);
}

/// Invalidate any per-type caches (method cache, version tag).
pub fn type_modified(tp: *mut PyTypeObject) {
    unsafe {
        crate::interp::cache::TYPE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        (*tp).tp_version_tag = 0;
        (*tp).tp_flags &= !Py_TPFLAGS_VALID_VERSION_TAG;
        for s in subclasses(tp) {
            type_modified(s);
        }
    }
}

pub unsafe extern "C" fn hash_not_implemented(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let _ = err!(TypeError, "unhashable type: '{}'", type_name((*o).ty()));
    }
    -1
}

// ------------------------------------------------------------ wrappers

fn check_nargs(args: &[&PyObject], n: usize, name: &str) -> PyResult<()> {
    if args.len() != n {
        raise!(TypeError, "expected {} argument{}, got {}", n, if n == 1 { "" } else { "s" }, args.len());
    }
    let _ = name;
    Ok(())
}

fn no_kwargs(kw: Option<&PyObject>, name: &str) -> PyResult<()> {
    if let Some(k) = kw {
        if crate::abstract_::object_len_or_zero(k) > 0 {
            raise!(TypeError, "wrapper {}() takes no keyword arguments", name);
        }
    }
    Ok(())
}

macro_rules! cast {
    ($w:expr, $t:ty) => {
        std::mem::transmute::<*mut c_void, $t>($w)
    };
}

unsafe fn wrap_unaryfunc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject) -> *mut PyObject)(s.as_ptr())) }
}
unsafe fn wrap_binary_l(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject)(s.as_ptr(), a[0].as_ptr())) }
}
unsafe fn wrap_binary_r(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject)(a[0].as_ptr(), s.as_ptr())) }
}
unsafe fn wrap_ternary(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    if a.is_empty() || a.len() > 2 {
        raise!(TypeError, "expected 1 or 2 arguments, got {}", a.len());
    }
    let third = if a.len() == 2 { a[1].as_ptr() } else { none::none().as_ptr() };
    unsafe {
        from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject)(
            s.as_ptr(),
            a[0].as_ptr(),
            third,
        ))
    }
}
unsafe fn wrap_ternary_r(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    if a.is_empty() || a.len() > 2 {
        raise!(TypeError, "expected 1 or 2 arguments, got {}", a.len());
    }
    let third = if a.len() == 2 { a[1].as_ptr() } else { none::none().as_ptr() };
    unsafe {
        from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject)(
            a[0].as_ptr(),
            s.as_ptr(),
            third,
        ))
    }
}
unsafe fn wrap_inquirypred(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject) -> c_int)(s.as_ptr()) };
    if r < 0 {
        return Err(PyErr);
    }
    Ok(crate::types::bool_::new_bool(r != 0))
}
unsafe fn wrap_lenfunc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject) -> Py_ssize_t)(s.as_ptr()) };
    if r < 0 && err_occurred() {
        return Err(PyErr);
    }
    Ok(int::new_int(r as i64))
}
unsafe fn wrap_hashfunc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject) -> Py_hash_t)(s.as_ptr()) };
    if r == -1 && err_occurred() {
        return Err(PyErr);
    }
    Ok(int::new_int(r as i64))
}
unsafe fn wrap_call(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    let args = unsafe { tuple::new_tuple_from_ptrs(&a.iter().map(|x| x.as_ptr()).collect::<Vec<_>>()) };
    unsafe {
        from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject)(
            s.as_ptr(),
            args.as_ptr(),
            kw.map_or(std::ptr::null_mut(), |k| k.as_ptr()),
        ))
    }
}
unsafe fn wrap_init(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    let args = unsafe { tuple::new_tuple_from_ptrs(&a.iter().map(|x| x.as_ptr()).collect::<Vec<_>>()) };
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(
            s.as_ptr(),
            args.as_ptr(),
            kw.map_or(std::ptr::null_mut(), |k| k.as_ptr()),
        )
    };
    if r < 0 {
        return Err(PyErr);
    }
    Ok(none::none_ref())
}
unsafe fn wrap_richcmp_op(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void, op: c_int) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    unsafe {
        from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject)(
            s.as_ptr(),
            a[0].as_ptr(),
            op,
        ))
    }
}
/// Placeholder; the descriptor passes the op through `wrap_richcmp_dispatch`.
unsafe fn wrap_richcmp(_s: &PyObject, _a: &[&PyObject], _kw: Option<&PyObject>, _w: *mut c_void) -> PyResult {
    unreachable!("richcmp wrappers are dispatched with their op")
}
pub unsafe fn call_wrapper(idx: usize, s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    let sd = &SLOTDEFS[idx];
    unsafe {
        if sd.slot == Slot::TpRichcompare {
            return wrap_richcmp_op(s, a, kw, w, sd.op);
        }
        (sd.wrapper.unwrap())(s, a, kw, w)
    }
}
unsafe fn wrap_next(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject) -> *mut PyObject)(s.as_ptr()) };
    if r.is_null() {
        if !err_occurred() {
            return Err(set_err_noargs(crate::types::exceptions::StopIteration()));
        }
        return Err(PyErr);
    }
    Ok(unsafe { PyRef::from_raw(r) })
}
unsafe fn wrap_descr_get(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    if a.is_empty() || a.len() > 2 {
        raise!(TypeError, "expected 1 or 2 arguments, got {}", a.len());
    }
    let obj = if a[0].is(none::none()) { std::ptr::null_mut() } else { a[0].as_ptr() };
    let tp = if a.len() == 2 && !a[1].is(none::none()) { a[1].as_ptr() } else { std::ptr::null_mut() };
    if obj.is_null() && tp.is_null() {
        raise!(TypeError, "__get__(None, None) is invalid");
    }
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject)(s.as_ptr(), obj, tp)) }
}
unsafe fn wrap_descr_set(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 2, "")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), a[1].as_ptr())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_descr_delete(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), std::ptr::null_mut())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_del(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 0, "")?;
    unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject))(s.as_ptr()) };
    if err_occurred() { Err(PyErr) } else { Ok(none::none_ref()) }
}
/// Check that a slot wrapper is applied to a compatible object (the "Carlo Verre hack").
fn hackcheck(s: &PyObject, w: *mut c_void, what: &str) -> PyResult<()> {
    let tp = s.type_ptr();
    // Find the nearest base with a C-level (non-generic) setattro
    let mut t = tp;
    unsafe {
        while !t.is_null() && is_heap_type(t) {
            t = (*t).tp_base;
        }
        if !t.is_null() {
            let f = (*t).tp_setattro.map_or(std::ptr::null_mut(), |f| f as *mut c_void);
            if f != w {
                raise!(TypeError, "can't apply this {} to {} object", what, type_name(&*t));
            }
        }
    }
    Ok(())
}
unsafe fn wrap_setattr(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 2, "")?;
    hackcheck(s, w, "__setattr__")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), a[1].as_ptr())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_delattr(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    hackcheck(s, w, "__delattr__")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), std::ptr::null_mut())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_getattr(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject)(s.as_ptr(), a[0].as_ptr())) }
}
unsafe fn wrap_objobjargproc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 2, "")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), a[1].as_ptr())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_delitem(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr(), std::ptr::null_mut())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_objobjproc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int)(s.as_ptr(), a[0].as_ptr()) };
    if r < 0 { Err(PyErr) } else { Ok(crate::types::bool_::new_bool(r != 0)) }
}
fn seq_index(s: &PyObject, i: &PyObject) -> PyResult<isize> {
    let mut i = ab::index_as_ssize(i, Some(crate::types::exceptions::OverflowError()))?;
    if i < 0 {
        let sq = s.ty().tp_as_sequence;
        if !sq.is_null() {
            if let Some(len) = unsafe { (*sq).sq_length } {
                let n = unsafe { len(s.as_ptr()) };
                if n < 0 {
                    return Err(PyErr);
                }
                i += n;
            }
        }
    }
    Ok(i)
}
unsafe fn wrap_sq_item(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let i = seq_index(s, a[0])?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut PyObject)(s.as_ptr(), i)) }
}
unsafe fn wrap_sq_setitem(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 2, "")?;
    let i = seq_index(s, a[0])?;
    let r = unsafe { cast!(w, unsafe extern "C" fn(*mut PyObject, Py_ssize_t, *mut PyObject) -> c_int)(s.as_ptr(), i, a[1].as_ptr()) };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_sq_delitem(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let i = seq_index(s, a[0])?;
    let r = unsafe {
        cast!(w, unsafe extern "C" fn(*mut PyObject, Py_ssize_t, *mut PyObject) -> c_int)(s.as_ptr(), i, std::ptr::null_mut())
    };
    if r < 0 { Err(PyErr) } else { Ok(none::none_ref()) }
}
unsafe fn wrap_indexargfunc(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let i = ab::index_as_ssize(a[0], Some(crate::types::exceptions::OverflowError()))?;
    unsafe { from_c(cast!(w, unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut PyObject)(s.as_ptr(), i)) }
}
unsafe fn wrap_buffer(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    let flags = ab::index_as_ssize(a[0], Some(crate::types::exceptions::OverflowError()))?;
    let proc: getbufferproc = unsafe { cast!(w, getbufferproc) };
    crate::types::memoryview::from_bufferproc(s, flags, proc)
}
unsafe fn wrap_releasebuffer(s: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, _w: *mut c_void) -> PyResult {
    no_kwargs(kw, "")?;
    check_nargs(a, 1, "")?;
    crate::types::memoryview::release_buffer_wrapper(s, a[0])
}

// ------------------------------------------------------------ generic slot functions

/// Look up a dunder on type(obj) and call it bound to obj. Returns None if not found.
pub fn call_special(obj: &PyObject, nm: &PyObject, args: &[&PyObject]) -> PyResult<Option<PyRef>> {
    let f = match lookup(obj.type_ptr(), nm) {
        Some(f) => f,
        None => return Ok(None),
    };
    ab::call_unbound(&f, obj, args).map(Some)
}

fn call_special_required(obj: &PyObject, nm: &PyObject, args: &[&PyObject]) -> PyResult {
    match call_special(obj, nm, args)? {
        Some(r) => Ok(r),
        None => Err(attribute_error_for(obj, nm)),
    }
}

unsafe extern "C" fn slot_tp_repr(s: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        match call_special(s, name!("__repr__"), &[]) {
            Ok(Some(r)) => r.into_raw(),
            Ok(None) => into_c(Ok(pystr::new_str(&format!("<{} object at {:p}>", type_fqname(s.type_ptr()), s.as_ptr())))),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn slot_tp_str(s: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(call_special_required(&*s, name!("__str__"), &[])) }
}
unsafe extern "C" fn slot_tp_hash(s: *mut PyObject) -> Py_hash_t {
    unsafe {
        let s = &*s;
        let f = match lookup(s.type_ptr(), name!("__hash__")) {
            Some(f) if !f.is(none::none()) => f,
            _ => return hash_not_implemented(s.as_ptr()),
        };
        let r = match ab::call_unbound(&f, s, &[]) {
            Ok(r) => r,
            Err(_) => return -1,
        };
        if !int::is_int(&r) {
            let _ = err!(TypeError, "__hash__ method should return an integer");
            return -1;
        }
        // a result that fits in Py_hash_t is used as is; only larger ints are
        // reduced with the int hash
        let h = match int::as_i64(&r) {
            Some(v) => v as Py_hash_t,
            None => int::int_hash(&r),
        };
        if h == -1 { -2 } else { h }
    }
}
unsafe extern "C" fn slot_tp_call(s: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        let f = match lookup(s.type_ptr(), name!("__call__")) {
            Some(f) => f,
            None => return into_c(Err(err!(TypeError, "'{}' object is not callable", type_name(s.ty())))),
        };
        let kw = if kw.is_null() { None } else { Some(&*kw) };
        into_c(call_bound_kw(&f, s, tuple::tuple_items(&*args), kw))
    }
}
unsafe extern "C" fn slot_tp_getattr_hook(s: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        let nm = &*nm;
        let tp = s.type_ptr();
        let getattr = lookup(tp, name!("__getattr__"));
        let getattribute = lookup(tp, name!("__getattribute__"));
        let r = match &getattribute {
            Some(ga) if !crate::types::descr::is_wrapper_for(ga, object_generic_getattro as *mut c_void) => {
                ab::call_unbound(ga, s, &[nm])
            }
            _ => generic_getattr(s, nm),
        };
        match (r, getattr) {
            (Ok(v), _) => v.into_raw(),
            (Err(e), Some(ga)) => {
                if exc_matches(crate::types::exceptions::AttributeError()) {
                    clear_exc();
                    into_c(ab::call_unbound(&ga, s, &[nm]))
                } else {
                    let _ = e;
                    std::ptr::null_mut()
                }
            }
            (Err(_), None) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn slot_tp_setattro(s: *mut PyObject, nm: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let s = &*s;
        let r = if v.is_null() {
            call_special_required(s, name!("__delattr__"), &[&*nm])
        } else {
            call_special_required(s, name!("__setattr__"), &[&*nm, &*v])
        };
        if r.is_ok() { 0 } else { -1 }
    }
}
const RICHCMP_NAMES: [&str; 6] = ["__lt__", "__le__", "__eq__", "__ne__", "__gt__", "__ge__"];
unsafe extern "C" fn slot_tp_richcompare(s: *mut PyObject, o: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let s = &*s;
        let nm = pystr::intern_str(RICHCMP_NAMES[op as usize]);
        // a descriptor whose __get__ fails makes the comparison NotImplemented
        if let Some(f) = lookup(s.type_ptr(), &nm) {
            if !crate::types::funcobject::is_function(&f) {
                if let Some(get) = f.ty().tp_descr_get {
                    if f.ty().tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR == 0 {
                        let bound = get(f.as_ptr(), s.as_ptr(), s.type_ptr() as *mut PyObject);
                        if bound.is_null() {
                            // an AttributeError from __get__ means "no such method"
                            if !exc_matches(crate::types::exceptions::AttributeError()) {
                                return std::ptr::null_mut();
                            }
                            clear_exc();
                            return crate::types::none::not_implemented().to_ref().into_raw();
                        }
                        let bound = PyRef::from_raw(bound);
                        return into_c(ab::call(&bound, &[&*o]));
                    }
                }
            }
        }
        match call_special(s, &nm, &[&*o]) {
            Ok(Some(r)) => r.into_raw(),
            Ok(None) => crate::types::none::not_implemented().to_ref().into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn slot_tp_iter(s: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        match lookup(s.type_ptr(), name!("__iter__")) {
            Some(f) if f.is(none::none()) => into_c(Err(err!(TypeError, "'{}' object is not iterable", type_name(s.ty())))),
            Some(f) => into_c(ab::call_unbound(&f, s, &[])),
            None => {
                // fall back to the sequence protocol if __getitem__ exists
                if lookup(s.type_ptr(), name!("__getitem__")).is_some() {
                    into_c(Ok(crate::types::iterobject::new_seq_iter(s.to_ref())))
                } else {
                    into_c(Err(err!(TypeError, "'{}' object is not iterable", type_name(s.ty()))))
                }
            }
        }
    }
}
unsafe extern "C" fn slot_tp_iternext(s: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(call_special_required(&*s, name!("__next__"), &[])) }
}
unsafe extern "C" fn slot_tp_descr_get(s: *mut PyObject, obj: *mut PyObject, tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        let f = match lookup(s.type_ptr(), name!("__get__")) {
            Some(f) => f,
            None => return s.to_ref().into_raw(),
        };
        let o = if obj.is_null() { none::none() } else { &*obj };
        let t = if tp.is_null() { none::none() } else { &*tp };
        into_c(ab::call_unbound(&f, s, &[o, t]))
    }
}
unsafe extern "C" fn slot_tp_descr_set(s: *mut PyObject, obj: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let s = &*s;
        let r = if v.is_null() {
            call_special_required(s, name!("__delete__"), &[&*obj])
        } else {
            call_special_required(s, name!("__set__"), &[&*obj, &*v])
        };
        if r.is_ok() { 0 } else { -1 }
    }
}
/// Call the special method `f` found on the type of `s` with `args`/`kw`: plain
/// functions and method descriptors get `s` prepended, other descriptors are bound
/// through `__get__` first (whose errors propagate), anything else is called as is.
fn call_bound_kw(f: &PyObject, s: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let ft = f.ty();
    if ft.tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR == 0 {
        return match ft.tp_descr_get {
            Some(g) => {
                let bound = unsafe { from_c(g(f.as_ptr(), s.as_ptr(), s.type_ptr() as *mut PyObject))? };
                ab::call_with_kwargs_dict(&bound, args, kw)
            }
            None => ab::call_with_kwargs_dict(f, args, kw),
        };
    }
    let mut v: Vec<&PyObject> = Vec::with_capacity(args.len() + 1);
    v.push(s);
    v.extend_from_slice(args);
    ab::call_with_kwargs_dict(f, &v, kw)
}
pub unsafe extern "C" fn slot_tp_init(s: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let s = &*s;
        let f = match lookup(s.type_ptr(), name!("__init__")) {
            Some(f) => f,
            None => return 0,
        };
        let kw = if kw.is_null() { None } else { Some(&*kw) };
        match call_bound_kw(&f, s, tuple::tuple_items(&*args), kw) {
            Ok(r) => {
                if !r.is(none::none()) {
                    let _ = err!(TypeError, "__init__() should return None, not '{}'", type_name(r.ty()));
                    return -1;
                }
                0
            }
            Err(_) => -1,
        }
    }
}
pub unsafe extern "C" fn slot_tp_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = match crate::abstract_::getattr(type_obj(tp), name!("__new__")) {
            Ok(f) => f,
            Err(_) => return std::ptr::null_mut(),
        };
        let mut v: Vec<&PyObject> = vec![type_obj(tp)];
        v.extend(tuple::tuple_items(&*args).iter().copied());
        let kw = if kw.is_null() { None } else { Some(&*kw) };
        into_c(ab::call_with_kwargs_dict(&f, &v, kw))
    }
}
unsafe extern "C" fn slot_tp_finalize(s: *mut PyObject) {
    unsafe {
        let s = &*s;
        let saved = take_exc();
        if let Some(f) = lookup(s.type_ptr(), name!("__del__")) {
            if ab::call_unbound(&f, s, &[]).is_err() {
                // the report names the __del__ in the message (object: None)
                let exc = take_exc();
                let r = ab::repr_string(&f).unwrap_or_else(|_| {
                    clear_exc();
                    "<object repr() failed>".into()
                });
                if let Some(e) = exc {
                    set_exc(e);
                }
                crate::modules::sys::write_unraisable_with_obj(&format!("Exception ignored while calling deallocator {}", r), None);
            }
        }
        if let Some(e) = saved {
            set_exc(e);
        }
    }
}
macro_rules! unary_slot {
    ($fname:ident, $dunder:expr) => {
        unsafe extern "C" fn $fname(s: *mut PyObject) -> *mut PyObject {
            unsafe { into_c(call_special_required(&*s, name!($dunder), &[])) }
        }
    };
}
unary_slot!(slot_nb_negative, "__neg__");
unary_slot!(slot_nb_positive, "__pos__");
unary_slot!(slot_nb_absolute, "__abs__");
unary_slot!(slot_nb_invert, "__invert__");
unary_slot!(slot_nb_int, "__int__");
unary_slot!(slot_nb_float, "__float__");
unary_slot!(slot_nb_index, "__index__");
unary_slot!(slot_am_await, "__await__");
unary_slot!(slot_am_aiter, "__aiter__");
unary_slot!(slot_am_anext, "__anext__");

macro_rules! inplace_slot {
    ($fname:ident, $dunder:expr) => {
        unsafe extern "C" fn $fname(s: *mut PyObject, o: *mut PyObject) -> *mut PyObject {
            unsafe {
                match call_special(&*s, name!($dunder), &[&*o]) {
                    Ok(Some(r)) => r.into_raw(),
                    Ok(None) => none::not_implemented().to_ref().into_raw(),
                    Err(_) => std::ptr::null_mut(),
                }
            }
        }
    };
}
inplace_slot!(slot_nb_inplace_add, "__iadd__");
inplace_slot!(slot_nb_inplace_subtract, "__isub__");
inplace_slot!(slot_nb_inplace_multiply, "__imul__");
inplace_slot!(slot_nb_inplace_remainder, "__imod__");
inplace_slot!(slot_nb_inplace_lshift, "__ilshift__");
inplace_slot!(slot_nb_inplace_rshift, "__irshift__");
inplace_slot!(slot_nb_inplace_and, "__iand__");
inplace_slot!(slot_nb_inplace_xor, "__ixor__");
inplace_slot!(slot_nb_inplace_or, "__ior__");
inplace_slot!(slot_nb_inplace_floor_divide, "__ifloordiv__");
inplace_slot!(slot_nb_inplace_true_divide, "__itruediv__");
inplace_slot!(slot_nb_inplace_matrix_multiply, "__imatmul__");

unsafe extern "C" fn slot_nb_inplace_power(s: *mut PyObject, o: *mut PyObject, _m: *mut PyObject) -> *mut PyObject {
    unsafe {
        match call_special(&*s, name!("__ipow__"), &[&*o]) {
            Ok(Some(r)) => r.into_raw(),
            Ok(None) => none::not_implemented().to_ref().into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

/// Is `name` defined on `tp` by something other than what `other_tp` would use?
fn method_is_overloaded(left: *mut PyTypeObject, right: *mut PyTypeObject, nm: &PyObject) -> bool {
    let b = match lookup(right, nm) {
        Some(b) => b,
        None => return false,
    };
    match lookup(left, nm) {
        Some(a) => !a.is(&b),
        None => true,
    }
}

/// Generic binary slot (SLOT1BINFULL).
fn binary_slot(s: &PyObject, o: &PyObject, slot: Slot, dunder: &PyObject, rdunder: &PyObject) -> PyResult {
    binary_slot_ex(s, o, slot, dunder, rdunder, None)
}

fn args<'a>(x: &'a PyObject, e: Option<&'a PyObject>) -> Vec<&'a PyObject> {
    match e {
        Some(m) => vec![x, m],
        None => vec![x],
    }
}

/// Binary-operator dispatch; `extra` is the modulus of three-argument pow (3.14+ also tries
/// the reflected method for it).
fn binary_slot_ex(s: &PyObject, o: &PyObject, slot: Slot, dunder: &PyObject, rdunder: &PyObject, extra: Option<&PyObject>) -> PyResult {
    let generic = slot.generic();
    let st = s.type_ptr();
    let ot = o.type_ptr();
    let do_other = st != ot && slot.get(ot) == generic;
    if slot.get(st) == generic {
        if do_other && is_subtype(ot, st) && method_is_overloaded(st, ot, rdunder) {
            if let Some(r) = call_special(o, rdunder, &args(s, extra))? {
                if !r.is(none::not_implemented()) {
                    return Ok(r);
                }
            }
            return match call_special(s, dunder, &args(o, extra))? {
                Some(r) => Ok(r),
                None => Ok(none::not_implemented().to_ref()),
            };
        }
        if let Some(r) = call_special(s, dunder, &args(o, extra))? {
            if !r.is(none::not_implemented()) || st == ot {
                return Ok(r);
            }
        }
    }
    if do_other {
        if let Some(r) = call_special(o, rdunder, &args(s, extra))? {
            return Ok(r);
        }
    }
    Ok(none::not_implemented().to_ref())
}

macro_rules! binary_slot_fn {
    ($fname:ident, $slot:ident, $d:expr, $rd:expr) => {
        unsafe extern "C" fn $fname(s: *mut PyObject, o: *mut PyObject) -> *mut PyObject {
            unsafe { into_c(binary_slot(&*s, &*o, Slot::$slot, name!($d), name!($rd))) }
        }
    };
}
binary_slot_fn!(slot_nb_add, NbAdd, "__add__", "__radd__");
binary_slot_fn!(slot_nb_subtract, NbSubtract, "__sub__", "__rsub__");
binary_slot_fn!(slot_nb_multiply, NbMultiply, "__mul__", "__rmul__");
binary_slot_fn!(slot_nb_remainder, NbRemainder, "__mod__", "__rmod__");
binary_slot_fn!(slot_nb_divmod, NbDivmod, "__divmod__", "__rdivmod__");
binary_slot_fn!(slot_nb_lshift, NbLshift, "__lshift__", "__rlshift__");
binary_slot_fn!(slot_nb_rshift, NbRshift, "__rshift__", "__rrshift__");
binary_slot_fn!(slot_nb_and, NbAnd, "__and__", "__rand__");
binary_slot_fn!(slot_nb_xor, NbXor, "__xor__", "__rxor__");
binary_slot_fn!(slot_nb_or, NbOr, "__or__", "__ror__");
binary_slot_fn!(slot_nb_floor_divide, NbFloorDivide, "__floordiv__", "__rfloordiv__");
binary_slot_fn!(slot_nb_true_divide, NbTrueDivide, "__truediv__", "__rtruediv__");
binary_slot_fn!(slot_nb_matrix_multiply, NbMatrixMultiply, "__matmul__", "__rmatmul__");

unsafe extern "C" fn slot_nb_power(s: *mut PyObject, o: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*s;
        let o = &*o;
        if m.is_null() || (*m).is(none::none()) {
            return into_c(binary_slot(s, o, Slot::NbPower, name!("__pow__"), name!("__rpow__")));
        }
        // three-argument pow: since 3.14 the reflected __rpow__ is tried too
        into_c(binary_slot_ex(s, o, Slot::NbPower, name!("__pow__"), name!("__rpow__"), Some(&*m)))
    }
}

unsafe extern "C" fn slot_nb_bool(s: *mut PyObject) -> c_int {
    unsafe {
        let s = &*s;
        let (f, using_len) = match lookup(s.type_ptr(), name!("__bool__")) {
            Some(f) => (f, false),
            None => match lookup(s.type_ptr(), name!("__len__")) {
                Some(f) => (f, true),
                None => return 1,
            },
        };
        if f.is(none::none()) {
            let _ = err!(TypeError, "'{}' object cannot be interpreted as a boolean", type_name(s.ty()));
            return -1;
        }
        let r = match ab::call_unbound(&f, s, &[]) {
            Ok(r) => r,
            Err(_) => return -1,
        };
        if using_len {
            return match ab::len_result_to_ssize(&r) {
                Ok(n) => (n != 0) as c_int,
                Err(_) => -1,
            };
        }
        if crate::types::bool_::is_bool(&r) {
            return r.is(crate::types::bool_::true_obj()) as c_int;
        }
        let _ = err!(TypeError, "__bool__ should return bool, returned {}", type_name(r.ty()));
        -1
    }
}

unsafe extern "C" fn slot_mp_length(s: *mut PyObject) -> Py_ssize_t {
    unsafe {
        let s = &*s;
        let r = match call_special_required(s, name!("__len__"), &[]) {
            Ok(r) => r,
            Err(_) => return -1,
        };
        match ab::len_result_to_ssize(&r) {
            Ok(n) => n,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn slot_mp_subscript(s: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(call_special_required(&*s, name!("__getitem__"), &[&*k])) }
}
unsafe extern "C" fn slot_mp_ass_subscript(s: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let r = if v.is_null() {
            call_special_required(&*s, name!("__delitem__"), &[&*k])
        } else {
            call_special_required(&*s, name!("__setitem__"), &[&*k, &*v])
        };
        if r.is_ok() { 0 } else { -1 }
    }
}
unsafe extern "C" fn slot_sq_item(s: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let idx = int::new_int(i as i64);
        into_c(call_special_required(&*s, name!("__getitem__"), &[&idx]))
    }
}
unsafe extern "C" fn slot_sq_ass_item(s: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let idx = int::new_int(i as i64);
        let r = if v.is_null() {
            call_special_required(&*s, name!("__delitem__"), &[&idx])
        } else {
            call_special_required(&*s, name!("__setitem__"), &[&idx, &*v])
        };
        if r.is_ok() { 0 } else { -1 }
    }
}
unsafe extern "C" fn slot_sq_contains(s: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let s = &*s;
        match lookup(s.type_ptr(), name!("__contains__")) {
            Some(f) if f.is(none::none()) => {
                let _ = err!(TypeError, "'{}' object is not a container", type_name(s.ty()));
                -1
            }
            Some(f) => match ab::call_unbound(&f, s, &[&*v]).and_then(|r| ab::is_true(&r)) {
                Ok(b) => b as c_int,
                Err(_) => -1,
            },
            None => match ab::iter_search_contains(s, &*v) {
                Ok(b) => b as c_int,
                Err(_) => -1,
            },
        }
    }
}
unsafe extern "C" fn slot_bf_getbuffer(s: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe { crate::types::memoryview::slot_getbuffer_via_dunder(&*s, view, flags) }
}
unsafe extern "C" fn slot_bf_releasebuffer(s: *mut PyObject, view: *mut Py_buffer) {
    unsafe { crate::types::memoryview::slot_releasebuffer_via_dunder(&*s, view) }
}

pub fn is_generic_slot(slot: Slot, tp: *mut PyTypeObject) -> bool {
    slot.get(tp) == slot.generic()
}

pub fn len_of_slotdefs() -> usize {
    SLOTDEFS.len()
}
pub fn slotdef(i: usize) -> &'static SlotDef {
    &SLOTDEFS[i]
}
