# ast workstream

Owner of: `crates/parser` (bug fixes), `crates/compiler` (after the compiler author finished),
`crates/runtime/src/modules/_ast/` (`mod.rs`, `to_py.rs`, `from_py.rs`),
`crates/runtime/src/modules/ast_bridge.rs` (builtin `compile()`), `crates/runtime/src/compile.rs`,
`crates/runtime/src/modules/_symtable.rs`, `crates/runtime/src/modules/_tokenize.rs`,
`Lib/ast.py`, `Lib/_ast_unparse.py`, `Lib/tokenize.py`, `Lib/token.py`, `Lib/symtable.py`,
`Lib/codeop.py`.

## Design

* `_ast` node classes are process-global (created on first use by `compile()` or the first
  `import _ast`; `compile()` never imports `_ast`).  `ast.AST` is a static, *mutable* type
  (no IMMUTABLETYPE, so `del ast.AST._fields` works) with an instance `__dict__`; every ASDL
  class is a heap type made with `type()` whose `tp_name` is patched to `"ast.<Name>"` (error
  messages use it).  Class attributes: `_fields`, `__match_args__`, `_attributes`,
  `_field_types` / `__annotations__` (`X | None`, `list[X]`, the node classes, `str`, `int`,
  `object` for `constant`), `None` defaults for optional fields and optional attributes.
* `AST.__init__` (Rust): positional args fill `_fields`; keywords must be fields or
  attributes (TypeError otherwise); missing fields get `None` / `[]` / `Load()` from
  `_field_types`, anything else missing is a TypeError listing the names.  Abstract classes
  (`ast.AST`, the sum bases) warn with DeprecationWarning.  `_ast._is_abstract(cls)`.
* `__reduce__` returns `(cls, (None,)*k, __dict__)` where k = number of leading fields present,
  so unpickling/deepcopy never trips the required-field check and never recurses into args.
  `__replace__` re-calls the class with the old fields/attributes plus the changes.
  `__repr__` is depth limited (3 levels, `Name(...)` below; lists show first, `...`, last).
* Conversion: `to_py.rs` (ast2obj; identifiers interned; Load/Store/Del and operators are
  shared singletons) and `from_py.rs` (obj2ast, CPython's messages: `required field "x" missing
  from Y`, `field 'x' is required for Y`, `expected some sort of expr, but got ...`,
  `Y field "x" must be a list, not a ...`, `invalid integer value: ...`, `got an invalid type in
  Constant: ...`, `None disallowed in ... list`).  Missing `end_*` positions default to the
  start position.  Depth/stack guarded (RecursionError "... during ast construction").
* Validation: `rpy_compiler::validate_ast` is now the full `_PyAST_Validate` (contexts,
  empty bodies, arity mismatches, patterns, `identifier field can't represent 'True' constant`,
  positions).
* `compile()` lives in `ast_bridge.rs`: flag/optimize/mode checks, future-flag inheritance
  (`dont_inherit`), `module=` for warning filters, `_feature_version`, str sources ignore the
  coding cookie, AST input (validate + compile, or return as is / optimized with
  PyCF_ONLY_AST / PyCF_OPTIMIZED_AST, root type checked against the mode).
* `compile.rs` API: `Source::{Str,Bytes}`, `Opts`, `parse_mod`, `compile_source`,
  `compile_mod`, `optimize_mod`, `decode_source`; old `compile_str/compile_bytes/compile_ast`
  kept.  Compile-time warnings get a `module` derived from the file's location under
  `sys.path` when compile() got none; SyntaxErrors raised from warnings carry end positions.
  Non-ASCII identifiers are NFKC-normalized via `unicodedata.normalize` when parsing on a
  Python thread (TypeError if it returns a non-str, like CPython).
* `_symtable.symtable(source, filename, compile_type, *, module=None)` returns
  `symtable entry` objects (attributes id/name/type/lineno/nested/symbols/varnames/children).
* Python modules were written from scratch in our own structure (DECISIONS D19):
  `_ast_unparse` renders expressions as strings with an explicit "required precedence"
  argument; `ast.dump` uses a `_Dumper` class; `tokenize` keeps only the documented/test-used
  names (`TokenInfo`, `Untokenizer`, `detect_encoding`, `open`, `untokenize`,
  `_generate_tokens_from_c_tokenizer`, `StringPrefix`, `_main`, ...).

## Changes outside the owned files (minimal)
* `builtins.rs`: compile() delegates to `ast_bridge`; `_IncompleteInputError` exported;
  eval()/exec() of str sources ignore the coding cookie and inherit future flags.
* `types/typeobject.rs`: `type object 'X' has no attribute` uses `tp_name` (CPython does).
* `types/object_type.rs`: `__dict__` descriptor only added by the class that adds the dict.
* `abstract_.rs`: `f(**{non_str: v})` reaches `tp_call` of non-vectorcall callables.
* `types/int_methods.rs`: "Exceeds the limit (N digits)" messages.
* `interp/eval.rs`: `lazy import` executed with locals != globals raises SyntaxError.

## Test status (debug build, local stubs for missing pathlib/unittest.mock)
test_ast 216/220 (4 failures are runtime issues, see below), test_unparse 81/81,
test_type_comments 18/18 (1 skip), test_codeop 13/13, test_symtable 42/42,
test_type_params 109 (only pickling/weakref runtime failures).

## Known gaps / bugs
* `b''` constants are not the empty-bytes singleton (bytes workstream).
* `node.__replace__(**{object(): ...})`: method calls still reject non-str keywords before
  reaching `__replace__`.
* `.pyc` files are not invalidated when the compiler changes (import workstream): delete
  `__pycache__` after compiler changes.
* Lazy imports (PEP 810) are compiled with the lazy bit but executed eagerly by the VM.
