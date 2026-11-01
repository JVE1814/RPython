# Workstream "bytes": bytes, bytearray, memoryview, buffer protocol

## Files
* `crates/runtime/src/buffer.rs`: PEP 3118 helpers (Rust side of the C API):
  `get_buffer` (PyObject_GetBuffer), `release_buffer` (PyBuffer_Release),
  `fill_info` (PyBuffer_FillInfo), `is_contiguous` (PyBuffer_IsContiguous),
  `to_contiguous` / `from_contiguous` (PyBuffer_ToContiguous / FromContiguous),
  `get_pointer`, `fill_contiguous_strides`, `size_from_format` (via struct.calcsize),
  `object_copy_data` (PyObject_CopyData), `check_buffer`, `PyBUF_*` constants,
  RAII `BufferView` (the Py_buffer is boxed: exporters store pointers into the view
  itself, e.g. `shape = &view.len`), `buffer_bytes`, `with_buffer`.
* `types/bytes.rs`: allocation (`try_alloc_bytes`, `new_bytes`, `new_bytes_of_type`,
  `resize_unique` = _PyBytes_Resize), data access, cached hash.
* `types/bytes_methods.rs`: the bytes type (slots, constructor, iterator, `%`),
  every bytes method (shared with bytearray through `common_methods()`; results are
  built with the family of `self`), two-way string search (`search` module, generic
  over `T: Ord + Copy`, usable by str), `percent_format`, `bytes_from_object`
  (PyBytes_FromObject), `bytes_repr_string`, `strhex_raw`/`hex_sep`, `parse_fromhex`.
* `types/bytearray.rs`: bytearray (+ iterator), `new_bytearray`, `bytearray_from_object`
  (PyByteArray_FromObject), `resize` (PyByteArray_Resize).
* `types/memoryview.rs`: memoryview, `managedbuffer`, `memory_iterator`,
  `_buffer_wrapper`; `from_object[_flags]`, `from_buffer`, `from_memory`,
  `get_contiguous` (PyMemoryView_*), `from_bufferproc` (used by `__buffer__` wrappers),
  `slot_getbuffer_via_dunder` / `slot_releasebuffer_via_dunder` / `release_buffer_wrapper`
  (PEP 688).

## Decisions
* bytearray storage is a uniquely owned exact `bytes` object in `ob_bytes_object`
  (as the 3.15 header describes); `ob_alloc` = its size (capacity), `ob_start` the
  logical start (front deletions just advance it), data always NUL terminated.
  An empty bytearray without storage points `ob_start` at a static aligned zero
  buffer (never NULL, also for `bytearray.__new__` without `__init__`).
  `__alloc__()` = 0 without storage, else capacity + sizeof(b'') (this matches
  test_take_bytes_optimization and `sys.getsizeof == basicsize + __alloc__`).
  `take_bytes(n)`: n <= size/2 copies out and advances the start; otherwise the
  storage is handed over zero-copy (resized in place) and the rest copied out.
* Non-mutating bytearray methods hold an export on `self` (`SelfGuard`) while they
  may run Python code (argument conversion, `__len__` of sep, `%` arguments), so
  re-entrant resizes raise BufferError like CPython.
* bytes.tp_basicsize = offsetof(ob_sval)+1 (PyBytesObject_SIZE), as CPython.
* `bytes.partition` returns the separator object itself (CPython stringlib quirk).
* memoryview item comparison for native formats is done in Rust (Python `==`
  semantics, NaN-aware, mixed formats); only non-native formats use `struct`.
* `%`-format error messages follow 3.15 ("format argument N: ..."); a bare spec after
  a keyed spec with a mapping argument raises "format requires a parenthesised
  mapping key".

## Changes outside my files (minimal)
* `types/slots.rs` `wrap_buffer`: passes the wrapped getbufferproc to
  `memoryview::from_bufferproc` (CPython's _PyMemoryView_FromBufferProc) and checks
  flags overflow.
* `types/typeobject.rs` `type_ready`: for heap types `add_operators` now runs before
  slot inheritance (as in CPython). Before, `class C(Mixin, bytearray)` got wrapper
  descriptors for slots inherited from `object` via Mixin (C.__init__ was object's).
* `types/core_slots.rs`: removed the old bytes/bytearray code; fixed `i += step`
  overflow in list/tuple extended slicing (sys.maxsize steps).

## Missing / known gaps
* No per-object locking for bytearray under free threading (phase 5).
* `PyBytes_FromFormat` and the exported `extern "C"` API are for the C API workstream.
* Not runnable yet here: tests needing array, pickle, copy, struct, _testcapi,
  _testbuffer, unittest. Verified with scratch scripts porting most of test_bytes,
  test_memoryview, the PEP 688 part of test_buffer and string_tests search tests.
* Observed core bugs (not mine): `list.index` ignores start/stop; `reversed` builtin
  missing at the time; str `%` missing; uncaught exceptions crash in
  `sys_excepthook` (NULL self); non-builtin codecs return "SystemError ... result with
  an exception set".
