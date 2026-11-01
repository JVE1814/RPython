//! JIT tiers (see notes/jit.md). Tier 1: baseline template JIT emitting x86-64
//! machine code per bytecode (runtime helpers + inline fast paths), sharing the
//! interpreter's frame layout so it can fall back to tier 0 at any instruction
//! boundary.

/// Machine code installed for a code object.
pub struct Compiled {
    _private: (),
}

/// Free compiled code of a dying code object.
pub fn release_compiled(p: *mut Compiled) {
    unsafe { drop(Box::from_raw(p)) };
}
