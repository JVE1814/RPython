# rpy-compiler — notes

Bytecode compiler for the CPython 3.15 reimplementation: AST (`rpy_parser::ast`) →
symbol table → IR (basic blocks + pseudo-ops) → `bytecode::CodeUnit`.

## Public API (`src/lib.rs`)

```rust
pub fn compile(module: &ast::Mod, opts: &CompileOptions) -> Result<CompileOutput, CompileError>;
pub fn symtable(module: &ast::Mod, opts: &CompileOptions) -> Result<symtable::SymbolTable, CompileError>;
pub fn optimize_ast(module: &mut ast::Mod, optimize: i32, flags: u32) -> Result<(), CompileError>;
pub fn dis::disassemble(code: &CodeUnit) -> String;
pub fn unparse::unparse_expr(e: &ast::Expr) -> String;   // ast.unparse-like, annotation style
pub fn validate_ast(module: &ast::Mod) -> Result<(), CompileError>;  // for compile(ast_obj): ValueError on
    // invalid positions and malformed nodes (BoolOp < 2 values, empty bodies, ...). compile() itself
    // never panics on malformed trees (returns ValueError) but does not check positions.

pub struct CompileOptions { filename, mode: Mode /*Exec|Eval|Single|FuncType*/, optimize: i32,
                            flags: u32 /* PyCF_* | CO_FUTURE_* */, interactive: bool, max_depth: u32 }
pub struct CompileOutput { code: CodeUnit, warnings: Vec<Warning>, future_flags: u32 }
pub struct CompileError { kind: ErrorKind, msg, lineno, col_offset, end_lineno, end_col_offset }
```

* `optimize` must be >= 0 (runtime resolves -1 from `sys.flags.optimize`).
* `flags`: constants in `cf::` (same numbers as CPython). Future flags are inherited and
  copied into every code object's `co_flags` (`CO_FUTURE_*`). `PYCF_ALLOW_TOP_LEVEL_AWAIT`
  makes module-level await/async for/async with legal; the module code then gets
  `CO_COROUTINE` if it awaits.
* Positions in `CompileError`/`Warning` are AST positions: 1-based line, 0-based UTF-8 **byte**
  columns. `CompileError::offsets(start_line_text, end_line_text)` converts to CPython's
  1-based character `offset`/`end_offset`.
* `ErrorKind` tells the runtime which exception to raise: SyntaxError (most),
  RecursionError ("maximum recursion depth exceeded during compilation", when nesting exceeds
  `max_depth`, default 2000), SystemError (internal compiler error — a bug), ValueError.
* Warnings: SyntaxWarnings, deduplicated by (line, col, msg) — e.g. a `finally` body is compiled
  twice but warns once. Emit with `warnings.warn_explicit(msg, SyntaxWarning, filename, lineno)`;
  if the filter turns it into an error, raise `SyntaxError(msg)` at the warning location.
  Emitted: `"is" with 'int' literal. Did you mean "=="?` (and `is not`), `'tuple' object is not
  callable; perhaps you missed a comma?`, `'... object is not subscriptable; ...'`,
  `'X indices must be integers or slices, not Y; ...'`, `assertion is always true, perhaps
  remove parentheses?`, PEP 765 `'return' in a 'finally' block` / `'break' ...` / `'continue' ...`.
  (`symtable()` returns no warnings — matching CPython, which only reports parser warnings.)
* `symtable()` result: `SymbolTable { scopes: Vec<Scope>, future_flags }`, scope 0 = module
  (`name == "top"`, lineno 0). Per scope: `kind` (`type_name()` gives "module", "class",
  "function", "annotation", "type alias", "type parameters", "type variable"), `name`, `id`
  (unique >0), `lineno`, `children` (inlined comprehensions spliced out, their children
  spliced in, like CPython), `symbols` in insertion order, `varnames` (parameters), `nested`,
  `is_optimized()`, flags. `Symbol::raw_flags()` = `flags | scope << SCOPE_OFFSET`, with
  CPython's bit values: DEF_GLOBAL=1, DEF_LOCAL=2, DEF_PARAM=4, DEF_NONLOCAL=8, USE=16,
  DEF_FREE=32, DEF_FREE_CLASS=64, DEF_IMPORT=128, DEF_ANNOT=256, DEF_COMP_ITER=512,
  DEF_TYPE_PARAM=1024, DEF_COMP_CELL=2048, SCOPE_OFFSET=12, scopes LOCAL=1 GLOBAL_EXPLICIT=2
  GLOBAL_IMPLICIT=3 FREE=4 CELL=5. Every `def` has an `__annotate__` annotation block child
  (before the function block) even without annotations (test_symtable relies on this).
  Lambdas are named `<lambda>`, comprehensions `<listcomp>`/`<genexpr>`/..., implicit params
  `.0`, `.format`, `.type_params`, `.defaults`, `.kwdefaults`, `.generic_base`.

## Pipeline

1. `codegen::check_future` — `from __future__` validation (`not a chance`, `future feature X is
   not defined`, `from __future__ imports must occur at the beginning of the file`).
2. `fold::fold_module` on a private clone of the AST: constant folding + AST-level warnings.
3. `symtable::build` — two passes (build, analyze) as in CPython's symtable.c, incl. PEP 709
   inlining, PEP 695 scopes, PEP 649 annotation scopes, `__class__`/`__classdict__`/
   `__conditional_annotations__` implicit cells.
4. `codegen` — one `Unit` per code object; emits IR with pseudo-ops.
5. `assemble` — dead block removal, location propagation, NOP removal, stack depth, exception
   table, unbound-local analysis (LoadFastCheck → LoadFast), jump resolution, ExtendedArg.

AST nodes are mapped to symbol-table scopes by node address (+ a tag), so symtable and codegen
must run on the same (folded) AST.

## Additions to the bytecode contract

* `Intrinsic1::AsyncGenWrap` (= 11): `[value] -> [wrapped]`, emitted right before `YieldValue`
  for every `yield` in an async generator (and for `(*x async for ...)` items). Values yielded
  by await/yield-from loops are not wrapped.
* No new opcodes.

## Conventions the VM must follow (beyond the doc comments in bytecode.rs)

Agreed with the VM author:
* Exception table: entries never overlap; each instruction has at most one handler (the
  innermost). On entry: truncate stack to `depth`, push lasti (int) if `lasti`, push exc.
* `CopyFreeVars n` copies the closure tuple into the LAST n localsplus slots; `MakeCell i` wraps
  slot i (value or NULL) in a fresh cell.
* Generators: created at call time; first execution starts at instruction 0; nothing pushed on
  first resume; `YieldValue` pushes the sent value on resume.

Compiler-side conventions:
* **Prologue** of every code unit: `CopyFreeVars n` (if free vars), `MakeCell i` for every cell
  slot (including cells sharing an argument's slot), `Resume 0`. Module code also starts with
  `Resume 0`. `Resume 1/2/3` also follows every `YieldValue` (1 = yield, 2 = yield from,
  3 = await) — no-ops, markers for tracing.
* **localsplus** order: parameters (posonly, args, kwonly, *args, **kwargs), other fast locals
  in order of first use, cells not sharing an argument slot (sorted by name, then hidden
  comprehension cells), free vars (sorted by name — closure tuples use this order).
  Kinds: `LOCAL` (+`ARG_POS`/`ARG_KW`/`ARG_VAR` for parameters: posonly=POS, normal=POS|KW,
  kwonly=KW, *args=VAR|POS, **kw=VAR|KW), `CELL` (args that are cells: LOCAL|CELL|ARG_*),
  `FREE`, `HIDDEN` for inlined-comprehension variables in module/class code (these frames
  have fast slots too!) and for comprehension variables shadowing a `global` name.
  The parameter of `__annotate__` and of lazily evaluated type-param/alias functions is named
  `format` in localsplus (`.format` in the symbol table).
* **Closures**: `LoadFast i` on a cell or free slot must push the **cell object itself**; it is
  only used to build closure tuples: `LoadFast <cells...>; BuildTuple n; LoadConst code;
  MakeFunction; SetFunctionAttribute CLOSURE`. All other cell access uses
  LoadDeref/StoreDeref/DeleteDeref with the localsplus index.
* **Function creation**: attribute values are pushed in the order defaults (tuple), kwdefaults
  (dict), annotate (function), closure (tuple); then `LoadConst code; MakeFunction`; then one
  `SetFunctionAttribute` per flag in the order CLOSURE, ANNOTATE, KWDEFAULTS, DEFAULTS.
  `__doc__` = `consts[0]` iff `CO_HAS_DOCSTRING` (cleaned like CPython 3.13+, stripped at -OO),
  else None. `CO_METHOD` is set for functions defined directly in a class body.
* **Calls**: `[callable, self_or_null, args...]`. Plain call: `<func>; PushNull; args; Call n`.
  Method call: `<obj>; LoadMethod name; args; Call n`. Keywords: values then
  `LoadConst ('k1','k2')`; `CallKw n_total`. Star/double-star calls:
  `[callable, NULL]`, args tuple (built via `BuildList`/`ListAppend`/`ListExtend` +
  `CallIntrinsic1 ListToTuple`, or `BuildTuple`), kwargs dict (`BuildMap`, `DictMerge 1` with
  the dict at depth 1 after popping, callable at depth 4) or `PushNull`, `CallFunctionEx`.
  LoadMethod is only used for non-star calls.
* **Decorators**: `<deco>; PushNull` for each decorator (top to bottom), then the function or
  class, then `Call 1` once per decorator.
* **super()**: `super().x` / `super().m(...)` / `super(C, o).x` compile to
  `LoadGlobal super; LoadDeref __class__ (or arg1); LoadFast self (or arg2); LoadSuperAttr`,
  only when `super` is an unshadowed global and (zero-arg form) the function has a first
  positional parameter and a `__class__` free var.
* **Conditions**: `PopJumpIfTrue/False` always receive an exact bool (the compiler emits
  `ToBool` unless the producer is CompareOp|16, IsOp, ContainsOp, UnaryNot or ToBool).
  `JumpIfFalseOrPop`/`JumpIfTrueOrPop` (used for `and`/`or` and chained comparisons) receive
  arbitrary objects and must truth-test them. `x is None` tests use `PopJumpIfNone/NotNone`.
  `not x` = `ToBool; UnaryNot`.
* **await / yield from loop** (receiver on the stack, then the value to send):
  ```
  send:  Send exit          [recv, v] -> [recv, res]   | finished: -> [retval], jump exit
         YieldValue 1       handler: depth = receiver level (stack [.., recv]), no lasti
         Resume 2|3
         Jump send
  fail:  LoadConst None; Swap 2; CleanupThrow; Swap 2; PopTop   ([recv, exc] -> [value])
  exit:  [result]
  ```
  Because the handler depth keeps only the receiver, it does not matter whether the VM pushes
  a value when `throw()` is called on a frame suspended at that YieldValue.
  `await x`: `<x>; GetAwaitable 0; LoadConst None; <loop>`. `yield from x`:
  `<x>; GetYieldFromIter; LoadConst None; <loop>`.
* **Generators / coroutines / async generators**: the whole body is protected by a handler
  (depth 0, lasti): `CallIntrinsic1 StopIterationError; Reraise 1`. `yield v`:
  `<v>; [CallIntrinsic1 AsyncGenWrap]; YieldValue 0; Resume 1`. Generator bodies end with
  `LoadConst None; ReturnValue`.
* **for**: `<iter>; GetIter; L: ForIter end; <store>; body; Jump L; end:`.
  **async for**: `<iter>; GetAiter; L: [SetupFinally H] GetAnext; LoadConst None; <await loop>;
  [PopBlock] <store>; body; Jump L; H: EndAsyncFor` (handler stack `[aiter, exc]`, no lasti).
* **try/except** (as CPython 3.11+): body under handler H (depth d, no lasti).
  `H: [exc]` → (cleanup handler C covers this, depth d+1, lasti) `PushExcInfo` → `[prev, exc]`;
  per clause `<type>; CheckExcMatch; PopJumpIfFalse next`; `StoreX name` or `PopTop`; body under
  handler `CE` (depth d+1, lasti) which does `name = None; del name; Reraise 1`; success path:
  `PopExcept; name = None; del name; Jump end`. No match: `Reraise 0`.
  `C: Copy 3; PopExcept; Reraise 1` (stack `[prev, lasti, exc]`).
* **try/finally**: body under handler F (depth d, no lasti); finally body inlined on the normal
  path and before return/break/continue; `F: [exc]` (cleanup C, depth d+1, lasti)
  `PushExcInfo; <finally body>; Reraise 0`.
* **except\***: `[prev, orig]`; `BuildList 0; Copy 2` → `[prev, orig, list, rest]`; per clause
  `<type>; CheckEgMatch; Copy 1; PopJumpIfNone no_match`; raised exceptions are appended with
  `ListAppend 3`; finally `ListAppend 1; CallIntrinsic2 PrepReraiseStar` →
  re-raise if not None (`Swap 2; PopExcept; Reraise 0`).
* **with**: `<mgr>; BeforeWith` → `[exit, sn, enter_res]`, body under handler W (depth = base+2,
  i.e. `[exit, sn]`, lasti); normal exit `LoadConst None ×3; Call 3; PopTop`. Handler:
  `[exit, sn, lasti, exc]` → (cleanup C, depth base+4, lasti) `PushExcInfo; WithExceptStart;
  ToBool; PopJumpIfTrue suppress; Copy 3; Swap 2; Reraise 1` (re-raises exc restoring lasti);
  `suppress: PopTop; PopExcept; PopTop; PopTop; PopTop`.
  **async with**: `BeforeAsyncWith; GetAwaitable 1; <await loop>`, exit:
  `Call 3; GetAwaitable 2; <await loop>`, handler: `WithExceptStart; GetAwaitable 2;
  <await loop>; ToBool ...`.
* **return/break/continue** through with-blocks call `__exit__(None, None, None)`, through
  finally-blocks inline the finally body, through handlers `PopExcept`.
* **Comprehensions** (PEP 709): list/set/dict comprehensions are inlined (except inside
  class-visible annotation scopes, where they are separate `<listcomp>` etc. functions):
  ```
  <outermost iter>; GetIter (GetAiter if async)
  LoadFastAndClear x [; MakeCell x]   for every name bound in the comprehension
  Swap k+1                            (iterator back on top)
  [SetupFinally CLEANUP]              (only if k > 0; handler: Swap 2; PopTop; restore; Reraise 0)
  BuildList 0; Swap 2                 [saved.., list, iter]
  L: ForIter E; <store>; <ifs>; <elt>; ListAppend depth+1; Jump L; E:
  [PopBlock]
  Swap k+1; StoreFast x...            (restore; StoreFast may store NULL → slot becomes unbound)
  ```
  Generator expressions are functions `<genexpr>` with one positional parameter `.0` (already
  an iterator: the caller does `MakeFunction; PushNull; <iter>; GetIter; Call 1`).
  PEP 798 (`[*x for ...]`, `{**d for ...}`, `(*x for ...)`) uses ListExtend/SetUpdate/DictUpdate
  and `yield from`.
* **Classes**: `LoadBuildClass; PushNull; <body function>; LoadConst name; bases...; [kw...];
  Call/CallKw/CallFunctionEx`. Body code (no OPTIMIZED/NEWLOCALS flags):
  `[LoadLocals; StoreDeref __classdict__]` (if needed), `__module__ = __name__`,
  `__qualname__`, `__firstlineno__`, `[__type_params__ = .type_params]`,
  `[__conditional_annotations__ = set()]` (cell), `[__doc__]`, body,
  `[__annotate_func__ = <annotate fn>]`, `__static_attributes__ = (sorted names assigned as
  self.X in methods)`, `[__classdictcell__ = <cell>]`, then
  `LoadFast <__class__ cell>; Copy 1; StoreName __classcell__; ReturnValue` or
  `LoadConst None; ReturnValue`. Free variables are read in class bodies with
  `LoadLocals; LoadFromDictOrDeref i`.
* **PEP 649 annotations**: `__annotate__(format, /)` functions start with
  `if format > 2: raise NotImplementedError` (`LoadFast 0; LoadConst 2; CompareOp Gt|16;
  PopJumpIfFalse; LoadCommonConstant NotImplementedError; RaiseVarargs 1`) and return a dict.
  Function annotations → SetFunctionAttribute ANNOTATE. Module: `__annotate__` stored with
  StoreName at the start of the module; class: stored as `__annotate_func__` at the end of
  the body. Module and (conditionally defined) class annotations are tracked in
  `__conditional_annotations__` (a set of indices; `SetAdd 1` must raise TypeError if it is
  not a set); the annotate function checks `idx in __conditional_annotations__` (LoadGlobal in
  modules, LoadDeref of the class cell in classes). Annotation scopes inside classes read names
  via `LoadDeref __classdict__; LoadFromDictOrGlobals name` (or `LoadFromDictOrDeref`).
  `from __future__ import annotations`: function annotate functions return strings; module/
  class use `SetupAnnotations` + `__annotations__[name] = 'string'`.
* **PEP 695**: generic functions/classes/aliases compile a `<generic parameters of X>` function
  (positional params `.defaults`/`.kwdefaults` if the function has defaults) that creates the
  type params (`LoadConst name; CallIntrinsic1 TypeVar|ParamSpec|TypeVarTuple` or
  `<lazy bound fn>; CallIntrinsic2 TypeVarWithBound|WithConstraints`, defaults via
  `CallIntrinsic2 SetTypeparamDefault`), binds them, builds the tuple, then builds the
  function (`CallIntrinsic2 SetFunctionTypeParams` with [func, tuple]) or the class (bases +
  `CallIntrinsic1 SubscriptGeneric(.type_params)` appended) and returns it; the outer code
  calls it. Type alias: `CallIntrinsic1 TypeAlias` on `(name, type_params or None, value_fn)`.
  Lazy values are functions with the `format` parameter and the NotImplementedError check.
* **match**: CPython's algorithm (captures kept on the stack until the whole pattern matches).
  Uses MatchSequence/MatchMapping/MatchKeys/MatchClass/GetLen, `CompareOp Eq|16` for values,
  `IsOp` for singletons.
* **Imports**: `LoadConst level; LoadConst fromlist|None; ImportName name (| 1<<23 if lazy)`;
  `import a.b as c` = ImportName + `ImportFrom b; Swap 2; PopTop` per component.
  `from m import *` → `CallIntrinsic1 ImportStar; PopTop`. The "lazy import only allowed at
  module level" check for `exec()` inside functions/classes is the VM's job (it depends on
  locals != globals at runtime).
* **Interactive** (`Single` + interactive): expression statements → `CallIntrinsic1 Print; PopTop`.
  **Eval**: expression; ReturnValue.
* `assert` is omitted when optimize > 0; `__debug__` loads fold to `optimize == 0`.

## Constant folding (fold.rs)

Done on a private AST copy before symtable/codegen: unary -,+,~,not on constants; binary ops
on numbers (ints up to 128 bits via i128, CPython's MAX_INT_SIZE; floats; complex +,-,*),
str/bytes concatenation and repetition (≤ 4096), tuples (≤ 256 items), constant tuples,
`x in [consts]` → tuple, `x in {consts}` → frozenset, same for `for` iterables; `__debug__`.
Lists/sets of ≥ 3 constants compile to `BuildList 0; LoadConst tuple; ListExtend 1`.
Not folded (yet): `%`-formatting to f-strings, subscripts of constants.

## Recursion / performance

All recursive passes count depth and fail with RecursionError beyond `max_depth` (2000).
Everything is linear except: name lookups use hash maps; analysis copies the bound/global
sets per child scope (as CPython). Compiling all 1276 parseable files of spec/Lib/test (with
parsing) takes ~3 s in release mode.

## Tests

* `tests/corpus.rs`: compiles every `.py` under spec/Lib/test the parser accepts (expected
  failures: `badsyntax*` files). Status: 1269/1269 compile, 0 unexpected errors (~3 s release,
  including parsing).
* `tests/syntax_doctests.rs`: SyntaxError doctests of test_syntax.py & co. that pass the parser
  must produce exactly the expected message from the compiler (88/88).
* `tests/syntax_calls.rs`: `check_syntax_error(self, "...", "msg")`, `self._check_error(...)`,
  `assert_syntax_error(...)` calls with literal sources extracted from 20 test files; message
  and (when given) lineno/offset/end_offset must match (107/107).
* `tests/named_expr.rs`, `tests/async_scope.rs`: cases from test_named_expressions.py and
  test_coroutines.test_async_comprehension_scope.
* `tests/symtable_api.rs`: mirrors test_symtable.py assertions on the Rust data structure.
* `tests/unparse_future.rs`: the 170 `eq(...)` cases of test_future.py (string annotations).
* `tests/units.rs`, `tests/smoke.rs`, `tests/depth.rs`, `src/assemble.rs` unit tests.
* `e2e/run.sh <python3>`: runs `e2e/*.py` through the real runtime and diffs against verified
  `*.expected` outputs (comprehension scoping in module/class/function scope, control flow,
  annotations, match ...). All pass with the runtime at the time of writing.
* `cargo run --example dis -- -c 'source'` (or a file) prints a disassembly.

## Locations / tracing

* Every instruction has a `Location`; artificial instructions inherit the previous location in
  their block (and along fallthrough edges), except handler entry blocks and the prologue
  (lineno 0 = "no location").
* NOPs are kept only when they carry the sole instruction of a line (`pass`, `global`, constant
  expression statements, the `while` header, `try` header, `return <const>` on its own line).
* Small location-less exit blocks (`LoadConst None; ReturnValue`) reached by jumps are
  duplicated per jumping predecessor with that predecessor's location (CPython's
  duplicate_exits_without_lineno), so the implicit return after a loop reports the loop line.
* Multi-line attribute chains/method calls: LoadAttr/LoadMethod/Call locations start at the
  attribute name (CPython's update_start_location_to_match_attr).
* Jump threading retargets jumps to location-less unconditional jumps.
* Unused constants are dropped (consts[0] kept when it is the docstring).

## Known gaps

* Line-event behaviour could not be validated end to end (runtime tracing is stubbed); the
  shapes follow CPython's rules but `test_sys_settrace` details may differ.
* No constant folding of subscripts or slices (there is no slice constant in the contract).
* Exact `co_consts` layouts checked by CPython-only tests (e.g. test_folding_type_param) are
  not reproduced.
