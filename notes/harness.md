# Workstream "harness" — notes

## What exists (all written from scratch)

Rust (crates/runtime/src/modules):
* `_warnings.rs` — the `_warnings` accelerator: `warn`, `warn_explicit` (registry/version,
  onceregistry, defaultaction, filters with str-or-regex matching like C, `error` raises,
  module_globals loader check via `linecache._bless_my_loader`, `_showwarnmsg` /
  `WarningMessage` / fallback printer), recursive lock (`_acquire_lock`/`_release_lock`,
  "cannot release un-acquired lock"), `_filters_mutated_lock_held`, `filters`,
  `_onceregistry`, `_defaultaction`, `_warnings_context` (only if `_contextvars` exists).
  `modules/warnings.rs` (runtime `warn`/`warn_explicit` used by the VM/compiler) calls it.
* `_ast.rs` — `_ast` module: node classes generated at init from
  `rpy_parser::ast::node_classes()` (Python prelude defines `AST` with 3.15 constructor
  semantics, `_field_types`/`__annotations__`, `__reduce__`, `__replace__`, depth-limited
  `__repr__`); Rust AST -> Python objects (`mod_to_py`), Python -> Rust AST (`py_to_mod`)
  with CPython error messages, `PyAST_Validate`-style validation (contexts, empty bodies,
  argument defaults, patterns...). `ast_bridge` delegates; builtin `compile()` passes
  `_feature_version`. `compile.rs` gained `parse_to_ast` and `pub compile_error`.
* `_opcode.rs` — opcode table of OUR bytecode (names are the `Op` variants in
  UPPER_SNAKE), has_arg/has_const/has_name/has_jump/has_local/has_free/has_exc,
  `stack_effect` (same table as the compiler's assembler), intrinsic descs, nb ops,
  `_exception_table(code)`, `_localsplusnames(code)`, ENABLE_SPECIALIZATION(_FT)=False.
* `_symtable.rs` — `_symtable.symtable(src, filename, mode)` returning a dict tree built
  from `rpy_compiler::symtable`, plus the DEF_*/scope/TYPE_* constants.
* `traceback_print.rs` — uncaught exceptions are printed through
  `traceback._print_exception_bltin` (falls back to the Rust printer, reporting the
  internal failure through sys.unraisablehook, re-entrancy guarded).
* `types/slots.rs` (core, 3-line fix): `add_operators` no longer wraps generic `slot_*`
  functions (inherited Python `__init__` etc. recursed forever in subclasses).

Lib/: `warnings.py`, `_py_warnings.py` (3.14 design: `_wm`, contexts, `_set_module`,
`_setoption` with /regex/ fields, `deprecated`, `_deprecated`), `linecache.py` (lazycache,
loader blessing, zip archives, encoding cookies), `traceback.py` (PEP 657 carets with
display-width handling, colour themes, exception groups, 3.15 suggestions: `'.attr'`,
NFKC, nested attributes, cross-language hints, stdlib-module hints, keyword typos),
`_colorize.py` (3.15 theme sections), `unittest/` (case incl. 3.14 assert methods,
subTest, cleanups/contexts, loader with -k patterns and discovery, suite fixtures,
runner with colours and durations, main, signals, _log, async_case), `argparse.py` (3.15:
colour with wrapping-independent spans, backtick markup, suggest_on_error, deprecated,
`_warning`), `ast.py` (+ unparser, dump with show_empty/color, compare), `annotationlib.py`,
`inspect.py` (signature machinery incl. `__text_signature__`, getsource family,
getattr_static...), `dis.py`/`opcode.py` (our 4-byte instructions; offsets = 4*index,
jump targets = 4*arg, exception table from `_opcode`), `symtable.py`,
`logging/__init__.py`, `logging/handlers.py`, `tempfile.py`, `shutil.py`, `gettext.py`,
`shlex.py`.

## Decisions
* `_warnings` follows CPython's split: C-level warn path is Rust; the Python `warnings`
  module is the source of truth for filters (`_get_filters()`), `onceregistry`,
  `defaultaction` and display (`_showwarnmsg`).
* If `sys.flags.context_aware_warnings` is set but `_contextvars` is unavailable,
  `_py_warnings` falls back to global (non-context) state.
* AST node classes are Python classes created by the `_ast` module (with
  `__module__ == 'ast'`), instances are allocated directly in Rust for speed.
* dis/opcode describe our own bytecode; dis output format is CPython's, but opcodes and
  offsets are ours (structural tests only).
* `linecache` decodes source itself (BOM/cookie) instead of importing tokenize.

## Missing / known gaps
* `unittest.mock` not written yet; `logging.config` not written.
* `ast` node `__init__` with non-str keyword keys: TypeError comes from the call itself.
* Error message "type object 'ast.AST' has no attribute '_fields'" shows 'AST' (heap type).
* Nothing could be run end-to-end yet: io, contextlib, dataclasses, enum, textwrap,
  difflib, pprint, weakref, socket, subprocess... are owned by other workstreams and not
  in master; `_collections_abc` currently fails (`range(1 << 1000)` unsupported) and
  `_abc._abc_register` crashes on a NULL type in core. Tested with private shims
  (not committed): warnings, linecache, ast/_ast (parse/compile/dump/unparse/
  literal_eval round trips), dis, traceback fallback path, _symtable.
* symtable: the compiler creates an `__annotate__` child scope even for functions
  without annotations (compiler behaviour).
