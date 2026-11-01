# cleanroom-lang — clean-room rewrite of enum, dataclasses, contextlib, _collections_abc

Per D19 / AGENTS rule 9 the four files were deleted and rewritten without opening the
previous versions (not even via git show/grep). Sources of truth: the documented public
API, the spec tests (test_enum, test_dataclasses/, test_contextlib(_async),
test_collections, test_abc, plus test_typing/test_pprint/test_inspect expectations), and
the other stdlib modules in this repo that import these names.

## Design notes (our own structure)

### Lib/_collections_abc.py
* One-trick ABCs get their `__subclasshook__` from `_structural(*methods)`: a classmethod
  that answers only for the ABC whose namespace holds it (identity check on
  `cls.__dict__['__subclasshook__']`), so explicit subclasses fall back to normal rules.
* `_check_methods` kept (io/os import it). `Set._hash` mirrors the runtime's frozenset hash
  (crates/runtime/src/types/set.rs `frozenset_hash_of`).
* `_CallableGenericAlias`: args stored flat; `__getitem__` lets GenericAlias substitute and
  folds the result back into `(params, result)` form. Module-level helpers
  (`_is_param_expr`, `_has_param_expr`) because GenericAlias forwards unknown attribute
  lookups to `__origin__`.
* `ByteString` is only reachable as `_ByteString` / via module `__getattr__` (and
  collections/abc.py's), with a metaclass warning on subclassing and isinstance().
* Built-in registrations in `_register_builtins()`; frozendict registered as Mapping.

### Lib/contextlib.py
* `ContextDecorator`/`AsyncContextDecorator` pick a wrapper by function kind (sync,
  generator, coroutine, async generator; `inspect` imported lazily at decoration time).
* `_GeneratorContextManager(func, args, kwds)` (test_with subclasses it); call arguments are
  dropped on `__enter__` (test_nokeepref).
* ExitStack: `_exit_callbacks` deque of `(is_sync, callback)`. Special methods are bound
  through the type's descriptor (`_bound_special_method`) so classmethod/staticmethod/slot
  `__exit__` work. Context chaining of exceptions raised by callbacks is repaired by
  `_relink_context` (an explicitly cleared `__context__` is respected). The source lines
  `if cb(*exc_details):`, `cb_suppress = cb(*exc_details)`, `raise exc` and
  `callback(*args, **kwds)` in `_exit_wrapper` are pinned by test_exit_exception_traceback.

### Lib/dataclasses.py
* Methods are generated as source and compiled together by `_MethodSource`, which wraps
  them in a factory function so helper objects are closure variables and the methods'
  globals are the defining module. The factory is named `__create_fn__` because
  Lib/pprint.py recognises a generated `__repr__` by that name in
  `__repr__.__wrapped__.__qualname__`.
* `__eq__` compares field by field (`a == b and ...`, NaN-correct, identity shortcut);
  hash action table in `_hash_action`.
* Class annotations are read in FORWARDREF format from the class's own annotate function
  (`_own_annotations`), so `make_dataclass` does not import typing.
* Generated `__init__` gets an `__annotate__` (marked `__generated_by_dataclasses__`).
* `__doc__` is a lazy descriptor (`_SignatureDoc`) computing the signature on first access
  (test_docstring_recursive needs names bound after the class statement; also keeps
  `inspect` out of class creation).
* String ClassVar/InitVar/KW_ONLY detection resolves the leading dotted name of the string
  in the class's module (`proxy.Nested.ClassVar` works).
* slots=True: new class; closure cells referring to the old class are re-pointed (incl.
  `__wrapped__` chains and property parts, empty cells tolerated); forward refs in field
  types are re-owned; `_forget_old_namespace` (see runtime note 4) lets the old class die.
* metadata: given mapping -> MappingProxyType (live view); default -> empty frozendict.

### Lib/enum.py
* Members are created from a single `_MemberBuilder` placed last in the class namespace:
  type.__new__ calls its `__set_name__` before `__init_subclass__`, which builds all
  members (`_build_members`) and fixes inherited methods first.
* A member whose name shadows an inherited descriptor (Enum.name/value, str.title,
  int.numerator, a mixin property, ...) is installed as an `enum.property` proxy
  (`_install_member`); otherwise the member object itself is the class attribute.
* Value lookup `_find_by_value` (hash table, then linear search for unhashable values);
  `Enum.__new__` detects `super().__new__()` from member constructors via
  `_UNDER_CONSTRUCTION`; raised exceptions are not kept in frame locals (no ref cycles).
* Flag: `_flag_mask_` = OR of all member values, `_singles_mask_` = single-bit members,
  `_all_bits_`; negative values: `& _singles_mask_` (STRICT/CONFORM) or
  `& all-bits-wide-enough` (KEEP/EJECT); members with negative values (e.g. `~A` in the
  class body) are resolved after all other members. Pseudo-member names list single bits
  in definition order, then multi-bit members, then leftover bits via `_numeric_repr_`
  (this reproduces e.g. `<Simple.SINGLE|<Iron.TWO: 2>: 3>`).
* Pickling is by value (`(cls, (value,))`); `pickle_by_enum_name` / `pickle_by_global_name`
  are opt-in.
* `_simple_enum` builds the class through the normal metaclass path, so
  `_test_simple_enum` (our own comparison) sees identical class dicts.
* Docstrings of EnumType.__contains__/__getitem__/__iter__/__len__/__members__ and of
  Enum.name/value are the exact text test_enum.test_pydoc compares (allowed by D19).

## Runtime fixes made on this branch (small, local)
1. `interp/eval.rs` + `threads.rs`: an exception that leaves a frame unhandled was chained
   (`__context__`) again in every caller frame it propagated through, overriding an
   explicitly cleared `__context__` (broke contextlib explicit-None tests and nested-with
   semantics). Now chaining happens only where the exception is raised: the frame that
   lets an exception escape records its (exception, traceback) identity; the receiving
   frame skips `chain_context` for that exception.
2. `types/slots.rs` `slot_tp_hash`: an int returned by a Python `__hash__` that fits in
   Py_hash_t is used as is (was always reduced with the int hash, so
   `hash(obj) != obj.__hash__()` for large values; broke dataclass hash tests).

## Runtime bugs found, not fixed (listed for the runtime owners)
3. Accessing an item of the StackSummary returned by `traceback.extract_tb(e.__traceback__)`
   after the `except` block ends overflows the Rust stack (segfault) — see
   /tmp-style repro: `try: 1/0 / except ZeroDivisionError as e: frames = extract_tb(...)`
   then `frames[0]` outside the handler. Crashes test_contextlib(_async)
   test_contextmanager_traceback (independent of contextlib; crashed before too).
4. A class's compiler-generated annotate function keeps the class's *own* dict (tp_dict) in
   its `__classdict__` cell (CPython keeps the class-body namespace, which holds no
   `__dict__`/`__weakref__` descriptors). This creates class <-> dict references that kept
   replaced dataclasses alive; worked around in dataclasses (`_forget_old_namespace`).
5. `type.__name__`/`__qualname__` return a new str object on each access, so
   `inspect.classify_class_attrs` cannot find their home class (test_enum
   test_inspect_classify_class_attrs).
6. Rare segfault under thread contention in test_enum OldTestIntFlag.test_unique_composite
   (8 threads creating pseudo-members; crash inside plain int arithmetic); not
   reproducible in isolation (passes 6/6 runs), seen once under heavy machine load.
7. `lazy from ... import` (PEP 810) not supported by the compiler
   (test_dataclasses test_lazy_import_annotations).

## Test results
See the report in the final message of this workstream (before/after numbers, measured
with identical binary + measurement-only stubs for missing stdlib modules).
