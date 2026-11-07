# core5 workstream notes

Branch `worktree-agent-a09599b93a591e888` (started from core4's branch `core` + master). Owner of the core
runtime (interp/ except jit + cache.rs + specialize.rs, types/ core, compiler crate, import system, sys*,
pymain, launcher, Lib/importlib, Lib/traceback.py, Lib/threading.py).

## Proposed decision for DECISIONS.md

### D25 — CPython's pyc magic number (pyc files are interchangeable with CPython 3.15's)
Finding: since D24 a code object's public form is CPython 3.15 wordcode, and marshal writes code objects in
CPython 3.15's layout ('c', five int32 counts/flags, co_code with zeroed caches, co_consts, co_names,
co_localsplusnames, co_localspluskinds, co_filename, co_name, co_qualname, int32 co_firstlineno, co_linetable,
co_exceptiontable). The pyc header (magic, flags, mtime+size or source hash) was already CPython's. The runtime
translator accepts every base 3.15 opcode (checked against spec/Include/opcode_ids.h; only pseudo/specialised/
INTERPRETER_EXIT are absent, and CPython never marshals those). So the formats are identical; only the magic
differed (we used 3690..3699, derived from a hash of the compiler sources so that a changed compiler invalidated
old caches).
Decision: `importlib.util.MAGIC_NUMBER` / `_imp.pyc_magic_number_token` / `PyImport_GetMagicNumber()` are
CPython 3.15's 3666 + b'\r\n' (test_importlib test_magic_number, test_capi test_getmagicnumber,
test_import test_magic_number_endianness). A .pyc we write is a CPython 3.15 .pyc and vice versa.
Invalidating our own stale caches: the header can no longer say "written by an older build of our compiler",
so the compiler crate's build script records `rpy_compiler::COMPILER_EPOCH` (the time the compiler/parser
sources last changed, i.e. when build.rs last re-ran), exposed as `_imp._rpy_compiler_epoch`;
`SourceFileLoader` treats a cache file whose file mtime is older than that as stale (recompiles and rewrites it).
One extra stat per cached import. The rule is ours only (CPython has no such check); consequences:
* CPython-written pycs older than our build are recompiled once (and rewritten in CPython's format, so CPython
  keeps loading them). In read-only directories they are recompiled in memory on every import.
* Sourceless .pyc files (SourcelessFileLoader) and zipimport are trusted as-is.
* `cp -r` resets mtimes, so a snapshot that copies a worktree's `Lib/**/__pycache__` would make stale caches
  look new. **Coordinator: tools/snapshot.sh / install.sh should not copy `__pycache__` directories**
  (e.g. `find "$DEST.tmp/lib" -name __pycache__ -prune -exec rm -rf {} +`).
Risk not verifiable here (rule 2 forbids running CPython): a pyc written by our compiler is executed by CPython's
VM if CPython finds it; bugs in our stack-depth/exception-table computation would surface there.

## Done

### pyc / marshal (item 1)
* D25 above. `marshal::CODE_FORMAT` removed; marshal module doc updated.
* Constant NaNs are never merged (compiler const keys, CFG `same_const`, runtime `ConstMerger`/shared const
  tuples): `{(1e999-1e999)*1j, (1e999-1e999)*2j, 1.5}` folds to a 3-element frozenset like CPython's
  (test_marshal test_deterministic_sets... the nan set test). test_marshal passes.
