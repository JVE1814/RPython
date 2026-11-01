# rpy-parser — notes

Tokenizer, parser and AST for the CPython 3.15.2 reimplementation. No dependencies
(`rpy-unicode` is a *dev*-dependency used only by the tests to provide real Unicode tables).

## Public API

```rust
// lib.rs
pub use parser::{parse, parse_bytes, Mode, ParseOptions, ParseOutput};
pub use error::{ErrorKind, SyntaxError, Warning, WarningCategory};
pub struct UnicodeHooks { lookup_name: fn(&str)->Option<u32>, is_xid_start: fn(char)->bool,
                          is_xid_continue: fn(char)->bool, is_printable: fn(char)->bool,
                          nfkc: fn(&str)->String }          // Default = approximations

pub fn parse(src: &str, opts: &ParseOptions) -> ParseOutput        // decoded text
pub fn parse_bytes(src: &[u8], opts: &ParseOptions) -> ParseOutput // BOM + PEP 263 cookie
pub struct ParseOutput { pub result: Result<ast::Mod, SyntaxError>, pub warnings: Vec<Warning> }
pub enum Mode { Exec, Eval, Single, FuncType }
pub struct ParseOptions {
    mode, type_comments /*PyCF_TYPE_COMMENTS*/, feature_version: Option<u32> /*minor*/,
    barry_as_flufl /*CO_FUTURE_BARRY_AS_BDFL*/, allow_incomplete_input /*PyCF_ALLOW_INCOMPLETE_INPUT*/,
    dont_imply_dedent /*PyCF_DONT_IMPLY_DEDENT*/, hooks: UnicodeHooks,
    filename: String /*only for "Non-UTF-8 code ... in file X"*/,
    decode: Option<tokenizer::DecodeHook> /*codec fallback for parse_bytes*/,
    max_stack_bytes: usize /*default 256 KiB of caller stack*/, grow_stack: bool /*default true*/,
    int_max_str_digits: usize /*sys.get_int_max_str_digits(); default 4300, 0 = unlimited*/,
}
impl ParseOptions { pub fn new(mode: Mode) -> ParseOptions }   // and Default

pub struct SyntaxError { kind: ErrorKind /*Syntax|Indentation|Tab|IncompleteInput|MemoryError|RecursionError*/, msg,
    lineno, col_offset /*0-based UTF-8 byte*/, end_lineno: Option, end_col_offset: Option,
    offset /*1-based chars = SyntaxError.offset*/, end_offset: Option, text: Option<String> }
pub struct Warning { category /*SyntaxWarning|DeprecationWarning*/, msg,
    error_msg /*message to use if the warning is turned into an error*/, lineno, col_offset,
    end_lineno, end_col_offset, offset, end_offset }
```

* `ast` — the AST (`Mod`, `Stmt{node: StmtKind, loc}`, `Expr{node: ExprKind, loc}`, `Pattern`,
  `TypeParam`, `Arguments`, `Arg`, `Keyword`, `Alias`, `WithItem`, `MatchCase`,
  `ExceptHandler`, `Comprehension`, `TypeIgnore`, operator enums). Field names/order/optionality
  mirror the `ast` module (`ExceptHandler.type_` is `type`). `Loc {lineno, col_offset,
  end_lineno, end_col_offset}` uses 1-based lines and UTF-8 byte columns (i32).
  * `Constant`: `None | Bool | Ellipsis | Int(Int::Small(i64) | Int::Big(decimal digits)) |
    Float(f64) | Complex{real, imag} | Str(PyStr) | Bytes(Vec<u8>) | Tuple | FrozenSet`.
  * `PyStr`: `Utf8(String)` or `CodePoints(Vec<u32>)` (strings with lone surrogates).
  * `ExprKind::Constant { kind: Some("u") }` for u-prefixed strings (first piece decides).
  * `Import/ImportFrom.is_lazy` (PEP 810), `Interpolation.str`, `TemplateStr` (PEP 750),
    `DictComp.value: Option` (PEP 798: `{**d for d in x}` = `DictComp(key=d, value=None)`;
    `[*x for x in y]` = `ListComp(elt=Starred)`).
  * `ast::ASDL` (the 3.15 ASDL text) and `ast::node_classes() -> &[NodeClass]`,
    `ast::node_class(name)`: machine-readable class table (name, base, abstract flag, fields
    with `FieldKind::{Single, Optional, Sequence, OptionalSequence}` and ASDL type, attributes,
    CPython-style `__doc__` signature) for building the `_ast` module.
* `dump::to_tuple(&Mod) -> String` — `test_ast.utils.to_tuple` rendered with Python `repr`
  (also `repr_str`, `repr_bytes`, `repr_float`, `repr_constant`).
* `tokenizer`:
  * `TokenIter::new(src, extra_tokens, hooks)` yields `Result<PyToken, SyntaxError>`;
    `PyToken {ty /*generic: OP for operators in extra mode*/, exact_type, string,
    start: (line, char col), end, line}` — exactly what `_tokenize.TokenizerIter` yields.
    `extra_tokens = true` is the `tokenize.tokenize/generate_tokens` stream (COMMENT, NL,
    real NEWLINE strings, INDENT/DEDENT positions, `<>` split, no bracket-matching errors);
    `false` is the raw C tokenizer stream (`_generate_tokens_from_c_tokenizer`).
    ENCODING is produced by Lib/tokenize.py, not here.
  * `Lexer` (low level), `LexOptions`, `tokenize_all(src) -> Vec<Token>`.
  * `detect_encoding(bytes) -> DetectedEncoding {encoding, bom, lines_consumed, from_cookie}`,
    `decode_source(bytes)`, `decode_source_with(bytes, hook, filename)`,
    `translate_newlines`, `normalize_encoding_name`.
    Native decoders: utf-8, ascii, latin-1/iso-8859-1, iso-8859-2/-5/-7/-15, cp1251, cp1252,
    koi8-r; everything else through `ParseOptions.decode` (or "unknown encoding").
* `token`: `TokenType` (numbered like `token.py` of 3.14/3.15: ... `EXCLAMATION=54, OP=55,
  TYPE_IGNORE, TYPE_COMMENT, SOFT_KEYWORD, FSTRING_START..END=59..61, TSTRING_*=62..64,
  COMMENT=65, NL=66, ERRORTOKEN=67, ENCODING=68`, `N_TOKENS=69`, `NT_OFFSET=256`),
  `TOKEN_NAMES`, `EXACT_TOKEN_TYPES`, `KEYWORDS`, `SOFT_KEYWORDS` (for keyword.py/token.py).
* `string`: escape decoding helpers.

## How the runtime should use it

* `compile(str)`: `parse(src, opts)`; `compile(bytes)`: `parse_bytes`. For `PyCF_IGNORE_COOKIE`
  decode yourself and call `parse` (a cookie in text is ignored).
* Warnings: emit `out.warnings` in order (they are deduplicated and sorted). If the warning
  filter turns one into an error, raise `SyntaxError(w.error_msg)` at
  `(w.lineno, w.offset, w.end_lineno, w.end_offset)` instead of the parse result (this is what
  CPython does; note the error message differs from the warning message for invalid escapes).
* `ErrorKind::IncompleteInput` -> `_IncompleteInputError` (msg "incomplete input").
* `\N{...}` needs `hooks.lookup_name`; identifiers need `is_xid_*`, `nfkc` (identifiers are
  NFKC-normalized when non-ASCII), `is_printable` for "invalid character" messages.

## Design

* Tokenizer: hand-written port of CPython's tokenizer state machine (indentation stack with
  tab/alt-col checks, bracket stack, f/t-string mode stack per PEP 701 with replacement-field
  and format-spec states). Number lexing mirrors CPython's cursor handling so error offsets
  match. In parser mode the whole file is tokenized up front into a `Vec<Token>` (Copy, 32
  bytes, no per-token heap allocation); a tokenizer error becomes a trailing ERRORTOKEN that is
  raised when the parser reaches it (or, as in CPython, replaces a later generic parser error).
* Implicit final newline: added for 'exec' (and the tokenize module) like CPython's
  `translate_newlines(exec_input=1)`; 'single'/'eval' get CPython's parser behaviour (inserted
  NEWLINE + implied DEDENTs at a mid-line EOF, none with `dont_imply_dedent`).
* Parser: recursive descent following python.gram (3.14/3.15) rule by rule, with
  deterministic choices instead of backtracking where the grammar's alternatives share a
  prefix (tuple/group/genexp, call args vs. bare genexp, dict/set/comprehensions, assignment
  targets are parsed as expressions and validated/converted). The six binary-operator levels
  use precedence climbing (same trees/locations). Node locations follow CPython's EXTRA rule
  (start = first token of the rule, end = last non-NEWLINE/INDENT/DEDENT token consumed).
* Two passes like CPython: the second pass (only after a failure) enables the `invalid_*`
  rules (src/parser/invalid.rs and inline `if self.invalid` blocks) to produce the specific
  messages; generic "invalid syntax" is reported at the furthest token of the first pass;
  a later tokenizer error overrides a parser error (except inside f-strings), "'(' was never
  closed" only if the error is on a later line, "unexpected indent/unindent" for INDENT/DEDENT.
* Strings: concatenation/flattening exactly like `_PyPegen_concatenate_strings` (constant
  folding of adjacent parts with merged locations; empty parts dropped in f-strings),
  f-string debug text (`f"{x=}"`) and t-string `Interpolation.str` from the source with
  comments removed, empty FSTRING_MIDDLE tokens emitted where CPython emits them.
* Ints: `i64` when it fits, else exact decimal string (hex/oct/bin converted). Decimal
  literals over 4300 digits raise CPython's int-max-str-digits SyntaxError.
* Performance: ~30 MB/s end-to-end (decode + tokenize + parse + drop AST) on the 27 MB of
  `spec/Lib/test` (`cargo run --release --example bench`); tokenizer alone ~170-200 MB/s.
  Allocation (Box per child node, String per identifier) is ~25-30% of the parse time; an
  arena-based AST would be the next step but would change the API used by the runtime.

## Tests (all in `tests/`)

| test | what | result |
|------|------|--------|
| `snippets.rs` | every expected entry of `test_ast/snippets.py` via `to_tuple` | 219/219 |
| `parse_all.rs` | every `.py` under `spec/Lib/test` (1302 files, 27 MB) | 1298 parse, 4 expected failures |
| `error_messages.rs` | 1703 expectations extracted from test_syntax (doctests + `_check_error`), test_exceptions (`check` offsets, `ckmsg`), test_fstring/test_tstring, test_grammar, test_patma, test_type_params, test_positional_only_arg, test_genexps, test_unpack_ex, test_setcomps, test_listcomps, test_extcall, test_generators, generic `assertRaises(SyntaxError, eval/exec/compile, "...")` in ~20 files | 1514 pass, 189 are compiler/symtable errors (parse OK), 0 mismatches |
| `tokenize.rs` | every constant `check_tokenize` in test_tokenize.py (tokenize + C tokenizer streams) | 182/182 |
| `type_comments.rs` | port of test_type_comments.py incl. feature_version | pass |
| `codeop.rs` | av/ai lists of test_codeop.py with `_maybe_compile` emulation | 174/174 |
| `misc.rs` | test_compile lists (leading zeros, None, imports), int max str digits, 200 nested brackets, recursion limits (RecursionError / MemoryError "too complex"), class table | pass |

### Files expected not to parse (`EXPECTED_INVALID` in tests/parse_all.rs)

* `tokenizedata/bad_coding.py` — declares the unknown encoding `uft-8` (test_source_encoding /
  test_tokenize expect SyntaxError).
* `tokenizedata/bad_coding2.py` — UTF-8 BOM together with a non-utf-8 cookie
  ("encoding problem: utf8 with BOM").
* `tokenizedata/badsyntax_3131.py` — invalid identifier character `€` (test_unicode_identifiers).
* `tokenizedata/badsyntax_pep3120.py` — non-UTF-8 bytes without a declaration.

Files whose errors are raised by the compiler/symtable, not the parser, parse fine and are not
in the list: `test_future_stmt/badsyntax_future.py`, `test_lazy_import/data/badsyntax/*`
(lazy import in function/class/try, `lazy from ... import *`, `lazy from __future__`).

## Checks that belong to the compiler/symtable (not done here)

'return'/'yield'/'await'/'yield from' outside function, 'break'/'continue' outside loop,
"asynchronous comprehension outside of an asynchronous function", global/nonlocal errors
(parameter and global, used/assigned prior to declaration, no binding for nonlocal, nonlocal at
module level), duplicate parameter / duplicate type parameter, "keyword argument repeated",
"cannot assign to __debug__" / "cannot delete __debug__" (all forms), starred assignment
errors ("starred assignment target must be in a list or tuple", "multiple starred expressions",
"too many expressions in star-unpacking assignment", "can't use starred expression here"),
`from __future__` checks (placement, unknown feature, braces), lazy-import restrictions
("lazy import not allowed inside functions/classes/try/except blocks",
"lazy from ... import * is not allowed", lazy `__future__`), "named/yield/await expression
cannot be used within a TypeVar bound/default/type alias/generic definition",
"non-default type parameter follows default type parameter", pattern semantics (attribute
name repeated in class pattern, irrefutable/wildcard patterns, alternative bindings, mapping
duplicate keys), "'elif' ..." is parser-level (done). Also "invalid syntax. Did you mean
'X'?" keyword-typo suggestions are added by Lib/traceback.py, not by the parser.

## Known gaps

* Unicode: without hooks, identifiers use `char::is_alphabetic/is_alphanumeric`, no NFKC and
  no `\N{}`; the runtime must pass real hooks.
* Error locations: covered where tests assert them; less common invalid_* rules may report a
  slightly different range than CPython. Messages for a few rarely-tested invalid rules
  (e.g. some `invalid_class_def_raw`, `invalid_double_type_comments` variants) are generic.
* `feature_version`: version gates implemented for async/await (5), `@` (5), async
  comprehensions, underscores in numbers and variable annotations (6), walrus and
  positional-only parameters (8), match (10), except* (11), type parameters/`type` (12).
  Other version-dependent behaviour (e.g. PEP 701 f-string rules for < 3.12) is not emulated.
* The tokenize-module stream is produced from the full source (no incremental readline).
* Recursion/stack safety: nesting is limited to 6000 recursion levels (like CPython's
  MAXSTACK), raising `ErrorKind::MemoryError` "Parser stack overflowed - Python source too
  complex to parse" (CPython: MemoryError). The parser uses at most
  `ParseOptions.max_stack_bytes` (default 256 KiB, measured with the real stack pointer) of the
  caller's stack; deeper recursion transparently continues on scoped helper threads with 16 MiB
  stacks (`grow_stack`, default on), so e.g. 200 nested brackets (~5 KB of stack per level)
  parse fine even on small thread stacks. UnicodeHooks may therefore be called from a helper
  thread. Constructs parsed iteratively but producing deep
  ASTs (trailer chains `a()()()...`, binary-operator chains, `elif` chains) are limited to 5000
  and raise `ErrorKind::RecursionError` "maximum recursion depth exceeded during compilation"
  (CPython raises RecursionError for these in ast2obj/the compiler). Brackets: 200 (tokenizer).
  Consumers must still guard their own recursion over the AST.
* Encodings beyond the native list need `ParseOptions.decode`.
