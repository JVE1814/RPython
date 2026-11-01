# devtools workstream notes

## Modules
Rust (crates/runtime/src/modules/, registered from `devtools.rs`):
marshal, pwd, grp (pwd.rs), resource, fcntl, termios, syslog + _posixshmem (syslog.rs), mmap.
Python (Lib/): getopt, colorsys, graphlib, sched, this, antigravity, timeit, trace,
ntpath, pathlib/ (package), py_compile, compileall, filecmp, ...

## Decisions
### marshal: code objects
Format version 6 (Py_MARSHAL_VERSION 6 in spec/Include/cpython/marshal.h). All type codes,
FLAG_REF back-references, 15-bit long digits, text/binary floats, small tuples, short ASCII
strings, slices (v5) and frozendicts (v6) are byte-compatible with CPython.
Code objects (`c`) use CPython's field order with OUR code units:
`argcount posonly kwonly stacksize flags` (int32 each), then objects
`co_code` (our 32-bit little-endian instruction words: opcode in bits 0..8, arg in 8..32),
`co_consts`, `co_names`, `co_localsplusnames`, `co_localspluskinds` (bytes, CO_FAST_* per
slot), `co_filename`, `co_name`, `co_qualname`, int32 `co_firstlineno`, `co_linetable`
(per instruction: lineno, end_lineno, col, end_col as u32 LE; u32::MAX = unknown column),
`co_exceptiontable` (per entry: start, end, target, depth, lasti as u32 LE).
Loading validates sizes and opcode range (ValueError "bad marshal data (invalid code object)").
`-X no_debug_ranges`/PYTHONNODEBUGRANGES drop the columns on load.
Immutable containers (frozenset, slice, code, frozendict) get their FLAG_REF index when their
type byte is written but may only be back-referenced once complete; a self-reference is written
again, which recurses into the depth limit (ValueError), matching CPython's test expectations.
The writer always uses refs for version >= 3 (no refcount==1 shortcut: biased refcounts).
Sets are written sorted by the marshalled bytes of their elements (deterministic output).

### .pyc files
py_compile/compileall write importlib's standard 16-byte header (magic 3999 from
importlib._bootstrap_external, flags, mtime/size or SipHash source hash) + marshal of our code
object. The magic number keeps them apart from CPython's caches; cache tag is whatever
sys.implementation.cache_tag says.

### pathlib
Own design: `pathlib/_glob.py` translates patterns per segment to regexes and globs on path
*strings*, so the same Globber serves Path (os.scandir) and pathlib.types._ReadablePath
(iterdir()+info).  Path/PurePath methods take their docstrings from the pathlib.types ABCs
(test_matches_writablepath_docstrings).  `pathlib._local` re-exports the classes so 3.13
pickles load.

### D19 clean-room
All my Python files were rewritten in my own structure/wording after D19 (getopt, colorsys,
graphlib, sched, antigravity, timeit, trace, ntpath, pathlib); Rust docstrings rewritten.

## Cross-workstream issues found
- int::as_i128 (types/int.rs) loses high bits for 5-digit values (2**128 -> 0). Worked around
  with `devtools::int_as_i128` (rejects > 4 digits).
- pymain: `-m` ran runpy without creating `__main__` (fixed, one line in pymain.rs).
- genericpath.ALLOW_MISSING.__reduce__ returns the class name instead of 'ALLOW_MISSING'
  (pickle test in test_genericpath/test_ntpath fails) — osio.
- sys.settrace: the audit hook runs (and is traced) after the trace function is installed;
  line events: `if []:` reports 2 hits, generator lines off by one (system/core).
- slice objects: `==` not implemented (containers).
- compile(): "unknown encoding: U7" (utf-7 coding cookie) — text/compiler.
