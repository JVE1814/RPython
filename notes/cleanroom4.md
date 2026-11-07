# cleanroom4 — continuation of the D19 audit (see notes/cleanroom1..3.md)

## Method
- As before: a module judged recalled is rewritten without reading the old body — only its
  list of top-level names (and, for typing, `__all__`), the documented API, the spec tests,
  and a grep of crates/ + Lib/ + spec/Lib/test (and of third-party typing_extensions, names
  only) for every private name other code uses.
- "before -> after": the module's own tests and every test module that mentions it, on an
  installed snapshot (target/cleanroom4/snap), old file vs new file, same binary.
- Scratch scripts in an untracked `.scratch4/` in the worktree.

## Rewritten

### typing.py
- Tests (before -> after): test_typing 731 -> 733 ok (2 more: own doctest examples in
  get_origin/get_args); test_types 136, test_type_params 109, test_annotationlib 120,
  test_genericalias 37, test_dataclasses 285, test_functools 338, test_type_aliases 34,
  test_isinstance 23, test_grammar 75, test_patma 343, test_copy 84, test_enum 1083,
  test_builtin 157, test_funcattrs 39, test_fractions 50, test_future_stmt 31,
  test_inspect 382, test_pydoc 119, test_free_threading.test_types 3,
  test_unittest.testmock.testhelpers 84, test_pstats, test_syntax, test_getpass,
  test_itertools, test_opcodes, test_ast, test_pyclbr, test_module, test__colorize,
  test_zipfile 388, test_type_annotations, test_decimal, test_tomllib, test_bz2,
  test_zoneinfo, test_datetime 1160, test_remote_pdb = (all pass).  Same pre-existing
  failures before and after: test_descr (1 error, 1 failure), test_importlib (6 errors),
  test_pyrepl (1 failure, test_cursor_back_write), test_warnings (env changed).  libregrtest
  itself imports typing (and calls `typing._cleanups`), so every run above also exercised it.
  typing_extensions 4.16 imports and works on top of it.
- Kept because pinned: the class names and hierarchy (`_Final` with the `_root=True`
  subclassing protocol, `_NotIterable`, `_SpecialForm(getitem)`, `_BaseGenericAlias`,
  `_GenericAlias` (+ `copy_with`, `_determine_new_args`, `_make_substitution`),
  `_SpecialGenericAlias`, `_CallableGenericAlias`, `_CallableType`, `_TupleType`,
  `_LiteralGenericAlias`, `_ConcatenateGenericAlias`, `_UnpackGenericAlias`,
  `_AnnotatedAlias`, `_ProtocolMeta`, `_TypedDictMeta`, `NamedTupleMeta`,
  `_DeprecatedGenericAlias`, `_UnionGenericAlias` + its metaclass, `_AnyMeta`, `_NoExtraItemsType`)
  — tests, pydoc output (`_GenericAlias`, `_SpecialForm`), dataclasses, typing_extensions
  and pydantic use them; functions the Rust runtime calls (`_type_check`, `_typevar_subst`,
  `_paramspec_subst`, `_paramspec_prepare_subst`, `_typevartuple_prepare_subst`,
  `_generic_class_getitem`, `_generic_init_subclass`, `Unpack`); names tests or other modules
  use (`_cleanups`, `_overload_registry` (3-level dict, patched by tests), `_overload_dummy`,
  `_get_protocol_attrs`, `_type_repr`, `_eval_type`, `_collect_parameters` (deprecated
  module `__getattr__`), `_ASSERT_NEVER_REPR_MAX_LENGTH`,
  `_deprecation_warning_for_no_type_params_passed`, `_proto_hook`, `_tp_cache`, `_type_convert`,
  `_is_param_expr`, `_deduplicate`, `_prohibited`, `_special`, `_make_nmtuple`, `_caller`,
  `_TYPING_INTERNALS`, `_SPECIAL_NAMES`, `EXCLUDED_ATTRIBUTES`), every asserted message.
- New structure: special forms are built by small factories (`_plain_form`,
  `_wrapper_form`, `_typeddict_key_form`, `_form`) instead of one decorated function each;
  Literal/Annotated/Concatenate subscriptions go through cached module-level builders
  (`_literal_alias(*values)` with typed caching instead of a `_TypedCacheSpecialForm`
  subclass); TypeForm's call behaviour is a `_call` slot of `_SpecialForm` (subclasses of a
  class with a `__doc__` slot cannot set it); `_tp_cache` only falls back to the uncached
  call when the arguments really are unhashable (a TypeError raised by the function itself
  is no longer swallowed and retried); `_brings_generic` decides whether a later base adds
  Generic; `_respecialize` rebuilds a types.GenericAlias keeping its class and its star;
  lazily created names come from a `_LAZY` table; protocol members/annotation names via
  `_annotation_names` (annotationlib only when the class has annotations, so importing
  typing does not import annotationlib, inspect, re, contextlib or warnings).
- Speed (ns, same binary, interleaved, noisy box): List[int] 225 -> 210, Callable[[int], str]
  ~equal, C[int] ~equal, isinstance(obj, runtime protocol) 530-720 -> 410-530 (the frame
  inspection of `_allow_reckless_class_checks` now only runs for non-runtime protocols),
  isinstance(3, SupportsInt) 450-650 -> 345, NamedTuple() 65-113k -> 45-95k, TypedDict()
  16-21k -> 10-18k, Optional[int] 220 -> ~260 (one extra Python frame).  pyperformance
  typing_runtime_protocols: old 0.144-0.218 ms, new 0.148-0.203 ms (equal within noise).
- Behaviour changes (all tests equal): evaluate_forward_ref() lets a ForwardRef created for a
  module see that module's names even when other globals are passed (they take
  precedence); nested string arguments inherit the parent reference's module;
  `_is_unpacked_typevartuple` requires the attribute to be exactly True (annotationlib's
  stringifier objects answer every attribute); `__name__`/`__qualname__` of an alias without
  `_name` come from the origin's `__name__`; dropped private helpers nothing uses
  (`_TypedCacheSpecialForm`, `_TypeFormForm`, `_TypingEllipsis`, `_LazyAnnotationLib`,
  `_lazy_annotationlib`, `_sentinel`, `_caches`, `_check_generic_specialization`,
  `_class_annotation_names`, `_lazy_load_getattr_static`, `_DeprecatedSpecialAlias`,
  `_warn_byte_string`, `_warn_if_runtime_checkable_inherited`); `_collect_type_parameters`
  lost its unused `validate_all` keyword; untested messages reworded.

## Reviewed: own text and structure, no action needed
(none yet)

## Still to do
see the end of this file.
