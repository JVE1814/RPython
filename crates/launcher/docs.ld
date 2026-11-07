/* (perf7) Docstrings of the builtin method tables (`doc_str!`, crates/runtime/src/methods.rs)
   after the other read-only data: start-up touches the method names, not their docs. */
SECTIONS {
  .rodata.rpydoc : { *(.rodata.rpydoc) }
} INSERT AFTER .rodata;
