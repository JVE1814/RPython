/* Read-only data that start-up does not read, or reads only in part, after the other
   read-only data, so that the kernel's fault-around (64-128 KB windows of the page cache
   mapped on each fault) does not pull it in together with the constants start-up uses.
   (perf7) Docstrings of the builtin method tables (`doc_str!`, crates/runtime/src/methods.rs):
   start-up touches the method names, not their docs.
   (mem1) The start-up image (crates/runtime/src/startup_image: its front is read at every
   start, the rest piecemeal), then blobs no normal start reads (`cold_bytes!`/`cold_str!`:
   frozen module sources, Unicode name and normalization tables, codec tables). */
SECTIONS {
  .rodata.rpyimg : { *(.rodata.rpyimg) }
  .rodata.rpydoc : { *(.rodata.rpydoc) }
  .rodata.rpycold : { *(.rodata.rpycold) }
} INSERT AFTER .rodata;
