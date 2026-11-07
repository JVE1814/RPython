//! Slot arrays (slots.h / slots_generated.h): one walker shared by PyType_FromSlots,
//! PyType_FromSpec*, PyModule_FromSlotsAndSpec and PyModuleDef-based modules.
//!
//! The walker flattens nested arrays (`Py_slot_subslots` -> PySlot[], `Py_tp_slots` ->
//! PyType_Slot[], `Py_mod_slots` -> PyModuleDef_Slot[]), maps the ambiguous compat ids
//! 1..4 to their per-kind numbers, and validates every entry: unknown or foreign ids,
//! flags on the end marker, NULL values, PySlot_STATIC requirements and repeats. The
//! callers then only see a flat list of `(id, flags, value)` in definition order.

use super::*;
use std::ffi::c_void;

use super::typeobj::PySlot;

pub const PYSLOT_OPTIONAL: u16 = 0x0001;
pub const PYSLOT_STATIC: u16 = 0x0002;

/// Nested arrays may go this many levels below the top-level array.
const MAX_NESTING: u32 = 4;

/// The number of slot ids defined by slots_generated.h (`_Py_slot_COUNT`).
pub const SLOT_COUNT: usize = 111;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Type,
    Module,
}

/// Which public entry point the slots came in through; it decides how lenient the
/// checks are (the older APIs keep accepting what they always accepted).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Api {
    /// PyType_FromSlots / PyModule_FromSlotsAndSpec
    Slots,
    /// PyType_Spec.slots
    Spec,
    /// PyModuleDef.m_slots
    Def,
}

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub id: u16,
    pub flags: u16,
    pub value: *mut c_void,
}

/// Name of a slot id as spelled in slots_generated.h (compat ids 1..4 are named by kind).
pub(crate) fn slot_name(id: u32, kind: Kind) -> Option<&'static str> {
    const NAMES: [&str; SLOT_COUNT] = [
        "Py_slot_end",
        "",
        "",
        "",
        "",
        "Py_mp_subscript",
        "Py_nb_absolute",
        "Py_nb_add",
        "Py_nb_and",
        "Py_nb_bool",
        "Py_nb_divmod",
        "Py_nb_float",
        "Py_nb_floor_divide",
        "Py_nb_index",
        "Py_nb_inplace_add",
        "Py_nb_inplace_and",
        "Py_nb_inplace_floor_divide",
        "Py_nb_inplace_lshift",
        "Py_nb_inplace_multiply",
        "Py_nb_inplace_or",
        "Py_nb_inplace_power",
        "Py_nb_inplace_remainder",
        "Py_nb_inplace_rshift",
        "Py_nb_inplace_subtract",
        "Py_nb_inplace_true_divide",
        "Py_nb_inplace_xor",
        "Py_nb_int",
        "Py_nb_invert",
        "Py_nb_lshift",
        "Py_nb_multiply",
        "Py_nb_negative",
        "Py_nb_or",
        "Py_nb_positive",
        "Py_nb_power",
        "Py_nb_remainder",
        "Py_nb_rshift",
        "Py_nb_subtract",
        "Py_nb_true_divide",
        "Py_nb_xor",
        "Py_sq_ass_item",
        "Py_sq_concat",
        "Py_sq_contains",
        "Py_sq_inplace_concat",
        "Py_sq_inplace_repeat",
        "Py_sq_item",
        "Py_sq_length",
        "Py_sq_repeat",
        "Py_tp_alloc",
        "Py_tp_base",
        "Py_tp_bases",
        "Py_tp_call",
        "Py_tp_clear",
        "Py_tp_dealloc",
        "Py_tp_del",
        "Py_tp_descr_get",
        "Py_tp_descr_set",
        "Py_tp_doc",
        "Py_tp_getattr",
        "Py_tp_getattro",
        "Py_tp_hash",
        "Py_tp_init",
        "Py_tp_is_gc",
        "Py_tp_iter",
        "Py_tp_iternext",
        "Py_tp_methods",
        "Py_tp_new",
        "Py_tp_repr",
        "Py_tp_richcompare",
        "Py_tp_setattr",
        "Py_tp_setattro",
        "Py_tp_str",
        "Py_tp_traverse",
        "Py_tp_members",
        "Py_tp_getset",
        "Py_tp_free",
        "Py_nb_matrix_multiply",
        "Py_nb_inplace_matrix_multiply",
        "Py_am_await",
        "Py_am_aiter",
        "Py_am_anext",
        "Py_tp_finalize",
        "Py_am_send",
        "Py_tp_vectorcall",
        "Py_tp_token",
        "Py_mod_create",
        "Py_mod_exec",
        "Py_mod_multiple_interpreters",
        "Py_mod_gil",
        "Py_bf_getbuffer",
        "Py_bf_releasebuffer",
        "Py_mp_ass_subscript",
        "Py_mp_length",
        "Py_slot_subslots",
        "Py_tp_slots",
        "Py_mod_slots",
        "Py_tp_name",
        "Py_tp_basicsize",
        "Py_tp_extra_basicsize",
        "Py_tp_itemsize",
        "Py_tp_flags",
        "Py_mod_name",
        "Py_mod_doc",
        "Py_mod_state_size",
        "Py_mod_methods",
        "Py_mod_state_traverse",
        "Py_mod_state_clear",
        "Py_mod_state_free",
        "Py_tp_metaclass",
        "Py_tp_module",
        "Py_mod_abi",
        "Py_mod_token",
    ];
    if id == 0xffff {
        return Some("Py_slot_invalid");
    }
    if (1..=4).contains(&id) {
        return Some(NAMES[normalize(id, kind) as usize]);
    }
    NAMES.get(id as usize).copied()
}

/// Compat ids 1..4 mean buffer/mapping slots for types and the first module slots for
/// modules.
#[inline]
pub(crate) fn normalize(id: u32, kind: Kind) -> u32 {
    if (1..=4).contains(&id) {
        match kind {
            Kind::Type => id + 87,
            Kind::Module => id + 83,
        }
    } else {
        id
    }
}

fn valid_for(id: u32, kind: Kind) -> bool {
    match kind {
        Kind::Type => matches!(id, 5..=83 | 88..=91 | 95..=99 | 107 | 108),
        Kind::Module => matches!(id, 84..=87 | 100..=106 | 109 | 110),
    }
}

/// How the walker treats a NULL (or zero) value.
enum NullRule {
    Allowed,
    /// deprecated: warn under the new API, skip the entry either way
    Deprecated,
    Error,
}

fn null_rule(id: u32, kind: Kind) -> NullRule {
    match kind {
        Kind::Type => match id {
            56 | 83 | 99 => NullRule::Allowed,
            72 | 95..=98 | 107 | 108 => NullRule::Error,
            _ => NullRule::Deprecated,
        },
        Kind::Module => match id {
            86 | 87 | 102 => NullRule::Allowed,
            84 => NullRule::Deprecated,
            _ => NullRule::Error,
        },
    }
}

/// Slots whose repetition is an error (others: the last one wins, with a warning under
/// the new API).
fn repeat_is_error(id: u32, kind: Kind, api: Api) -> bool {
    match kind {
        Kind::Type => matches!(id, 56 | 72 | 83 | 95..=99 | 107 | 108),
        Kind::Module => match id {
            85 => api != Api::Def, // a PyModuleDef may run several exec functions
            109 => false,           // several ABI descriptors may be checked
            _ => true,
        },
    }
}

/// Slots holding pointers to data the runtime keeps referring to after creation.
fn needs_static(id: u32, kind: Kind, api: Api) -> bool {
    match api {
        Api::Slots => matches!((kind, id), (Kind::Type, 72) | (Kind::Module, 103)),
        // a static PyModuleDef may only point at static data
        Api::Def => matches!(id, 92 | 93 | 94 | 100 | 101 | 103),
        Api::Spec => false,
    }
}

pub(crate) struct Walker {
    pub kind: Kind,
    pub api: Api,
    /// "type X" / "module X" for messages
    pub owner: String,
    pub out: Vec<Entry>,
    seen: [bool; SLOT_COUNT],
}

impl Walker {
    pub(crate) fn new(kind: Kind, api: Api, owner: String) -> Walker {
        Walker { kind, api, owner, out: Vec::new(), seen: [false; SLOT_COUNT] }
    }

    fn what(&self) -> &'static str {
        match self.kind {
            Kind::Type => "type",
            Kind::Module => "module",
        }
    }

    fn label(&self, id: u32) -> String {
        match slot_name(id, self.kind) {
            Some(n) if !n.is_empty() => format!("{} ({})", id, n),
            _ => format!("{}", id),
        }
    }

    fn name_of(&self, id: u32) -> String {
        match slot_name(id, self.kind) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => format!("slot {}", id),
        }
    }

    /// Walk a PySlot array.
    pub(crate) unsafe fn walk_slots(&mut self, mut p: *const PySlot, depth: u32) -> PyResult<()> {
        unsafe {
            if p.is_null() {
                return Err(system_error(&format!("NULL slot array for a {}", self.what())));
            }
            loop {
                let s = &*p;
                if s.sl_id == 0 {
                    if s.sl_flags != 0 {
                        return Err(system_error(&format!("invalid flags 0x{:x} for Py_slot_end", s.sl_flags)));
                    }
                    return Ok(());
                }
                self.entry(s.sl_id as u32, s.sl_flags, s.sl_ptr, true, depth)?;
                p = p.add(1);
            }
        }
    }

    /// Walk a PyType_Slot array.
    pub(crate) unsafe fn walk_type_slots(&mut self, mut p: *const PyType_Slot, depth: u32) -> PyResult<()> {
        unsafe {
            if p.is_null() {
                return Ok(());
            }
            while (*p).slot != 0 {
                let id = (*p).slot;
                let id = if id < 0 || id > 0xffff { 0xfffe_u32.max(id as u32) } else { id as u32 };
                self.entry(id, 0, (*p).pfunc, false, depth)?;
                p = p.add(1);
            }
            Ok(())
        }
    }

    /// Walk a PyModuleDef_Slot array.
    pub(crate) unsafe fn walk_mod_slots(&mut self, mut p: *const PyModuleDef_Slot, depth: u32) -> PyResult<()> {
        unsafe {
            if p.is_null() {
                return Ok(());
            }
            while (*p).slot != 0 {
                let id = (*p).slot;
                let id = if id < 0 || id > 0xffff { 0xfffe_u32.max(id as u32) } else { id as u32 };
                self.entry(id, 0, (*p).value, false, depth)?;
                p = p.add(1);
            }
            Ok(())
        }
    }

    unsafe fn entry(&mut self, raw: u32, flags: u16, value: *mut c_void, is_pyslot: bool, depth: u32) -> PyResult<()> {
        unsafe {
            let optional = flags & PYSLOT_OPTIONAL != 0;
            // nested arrays
            if matches!(raw, 92 | 93 | 94) {
                let foreign = (raw == 93 && self.kind == Kind::Module) || (raw == 94 && self.kind == Kind::Type);
                if foreign {
                    if optional {
                        return Ok(());
                    }
                    return Err(system_error(&format!("invalid slot ID {} for a {}", self.label(raw), self.what())));
                }
                if value.is_null() {
                    return Err(system_error(&format!("NULL not allowed for {}", self.name_of(raw))));
                }
                if is_pyslot && flags & PYSLOT_STATIC == 0 && needs_static(raw, self.kind, self.api) {
                    return Err(system_error(&format!("{} slot in a static definition must be static (PySlot_STATIC)", self.name_of(raw))));
                }
                if depth >= MAX_NESTING {
                    return Err(system_error(&format!("{}: too many levels of nested slot arrays", self.owner)));
                }
                return match raw {
                    92 => self.walk_slots(value as *const PySlot, depth + 1),
                    93 => self.walk_type_slots(value as *const PyType_Slot, depth + 1),
                    _ => self.walk_mod_slots(value as *const PyModuleDef_Slot, depth + 1),
                };
            }
            let id = normalize(raw, self.kind);
            if !valid_for(id, self.kind) {
                if optional {
                    return Ok(());
                }
                return Err(match slot_name(id, self.kind) {
                    Some(n) if !n.is_empty() => system_error(&format!("invalid slot ID {} ({}) for a {}", id, n, self.what())),
                    _ => system_error(&format!("unknown slot ID {} for a {}", raw, self.what())),
                });
            }
            if value.is_null() {
                match null_rule(id, self.kind) {
                    NullRule::Allowed => {}
                    NullRule::Error => {
                        let what = if matches!(id, 96..=98) { "must be positive" } else { "NULL not allowed" };
                        return Err(system_error(&format!("{}: {} for {} slot", self.owner, what, self.name_of(id))));
                    }
                    NullRule::Deprecated => {
                        if self.api == Api::Slots {
                            crate::modules::warnings::warn(
                                crate::types::exceptions::DeprecationWarning(),
                                &format!("{}: NULL value in {} slot is deprecated and ignored", self.owner, self.name_of(id)),
                                1,
                            )?;
                        }
                        return Ok(());
                    }
                }
            }
            if self.api == Api::Slots && self.kind == Kind::Type && matches!(id, 96..=98) && (value as isize) < 0 {
                return Err(system_error(&format!("{}: {} must be positive", self.owner, self.name_of(id))));
            }
            if is_pyslot && flags & PYSLOT_STATIC == 0 && needs_static(id, self.kind, self.api) {
                return Err(system_error(&format!("{} requires the PySlot_STATIC flag", self.name_of(id))));
            }
            let idx = id as usize;
            if self.seen[idx] {
                if repeat_is_error(id, self.kind, self.api) {
                    return Err(system_error(&format!("{} has multiple {} slots", self.owner, self.name_of(id))));
                }
                if self.api == Api::Slots {
                    crate::modules::warnings::warn(
                        crate::types::exceptions::DeprecationWarning(),
                        &format!("{} has multiple {} slots; the last one is used", self.owner, self.name_of(id)),
                        1,
                    )?;
                }
                // for types the last one wins; repeated module slots (exec, abi) all apply
                if self.kind == Kind::Type {
                    self.out.retain(|e| e.id as u32 != id);
                }
            }
            self.seen[idx] = true;
            self.out.push(Entry { id: id as u16, flags, value });
            Ok(())
        }
    }
}
