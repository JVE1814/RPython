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

### _pydatetime.py (partial rewrite; embedded in the binary — rebuilt)
- Verdict: the body was already mostly an earlier agent's own design (the `_dt_*` field
  bridge to the native types, `_from_posix`, `_local_timestamp`, `_plus_us`, the ISO
  parser, the `_*_new_args` helpers); the CPython-shaped parts were the calendar helpers
  (`_ord2ymd` with CPython's month estimate, `_days_before_year`, `_days_in_month`,
  `_days_before_month`, `_ymd2ord`, `_DAYS_PER_*`, `_MONTHNAMES`/`_DAYNAMES`,
  `_build_struct_time`), the `_check_*` validators, `_cmp`, `_format_time`,
  `_format_offset`, `_getstate`, `_tzstr`, `timezone._name_from_offset`/`_maxoffset`/
  `_minoffset`, `time.__hash__` (CPython's algorithm and assert text), `datetime.__hash__`,
  the ISO long-year test and `tzinfo.__reduce__`.  These were rewritten: `_leap`,
  `_month_length`, `_days_until`, `_ordinal`, `_from_ordinal` (year estimated from
  400-year cycles, month by bisect), `_struct_time`, `_valid_ymd`, `_valid_clock` (table of
  field limits), `_require_tzinfo`, `_vetted_offset`/`_vetted_tzname` (return the value),
  `_order`, `_clock_text` (timespec table), `_offset_text` (microsecond arithmetic),
  `_pickle_state` (int.to_bytes), hashes over the UTC microsecond stamp hashed as bytes
  (salted per process like CPython's — test_hash checks that), `_utc_label`, the timezone
  limit as a microsecond constant (no module-level timedelta, which would be a pure object
  once the native module swaps the classes), ISO week count from consecutive week-1
  Mondays.  Kept (pinned): `_divide_and_round` (datetimetester), everything
  crates/runtime/src/modules/_datetime.rs reads (`_timedelta_fields`, `_date_new_args`,
  `_time_new_args`, `_datetime_new_args`, `_timezone_new_args`, `_IsoCalendarDate`,
  `_set_class_attr`, `_init_constants`, `_EPOCH`, `UTC`, the `_dt_*` slots, `_offset`/
  `_name`, `datetime.utcnow`), all messages.
- Differential check old vs new (pure classes): every ordinal 1..3652059 through
  `_from_ordinal`/`_ordinal`, 20,000 random datetimes/times (repr, str, isoformat with all
  timespecs, ctime, timetuple, utctimetuple, isocalendar, pickle states for protocols 2/4,
  strftime with %z %:z %Z %f %j %U, arithmetic, fromisocalendar, fromordinal) and 20,000
  `_divide_and_round` cases: 0 differences; equal-value objects hash equal.
- Tests (same binary rebuilt with the new source vs the old binary): test_datetime 1160
  (pure and native), test_zoneinfo 279, test_strptime, test_calendar, test_email 1818,
  test_plistlib, test_sqlite3 523, test_imaplib 239, test_logging, test_xmlrpc,
  test_tomllib, test_hash 30, test_str, test_fstring, test_inspect, test_pydoc,
  test_types, test_enum, test_annotationlib, test_zipfile, test_free_threading = (pass);
  test_sys (1 error, 1 failure) and test_gc (2 failures) fail the same with the old binary.
- Behaviour changes: hash values differ (still salted, still equal for equal objects);
  `timezone._maxoffset`/`_minoffset` class attributes are gone.

### pickle.py (reviewed; renames)
- Verdict: already restructured by an earlier agent (`_opcode` registration decorator,
  `_Note`/`_noted`, chunked `_READ_PIECE` reads, `_check_new_class`, `_write_sized`,
  `_save_pair`, `_memo_get`, ...).  CPython names renamed: `_Framer` -> `_FrameWriter`
  (its `start_framing`/`end_framing`/`commit_frame`/`write_large_bytes`/
  `_FRAME_SIZE_TARGET` stay: pickletools.optimize drives `pickler.framer` with them),
  `_Unframer` -> `_FrameReader` (`load_frame` -> `enter_frame`, `_unframer` -> `_frames`),
  `_getattribute` -> `_resolve_path`, `_Stop` -> `_Finished`, `_tuplesize2code` dropped.
- test_pickle 1085, test_pickletools 202, test_copyreg, test_copy = (pass).

### urllib/request.py (partial rewrite)
- Rewritten in own structure: `AbstractDigestAuthHandler` (header built from a parameter
  list, `_next_nonce_count`, algorithm table; method names and attributes kept — tests call
  `get_algorithm_impls`), `FTPHandler.ftp_open`, `CacheFTPHandler` (`_drop`; an evicted
  connection is now closed), `ftpwrapper` (`_set_type`, `_listing`),
  `_proxy_bypass_macosx_sysconf` (`_dotted_number`, `_IPV4_PREFIX`); `_randombytes` and the
  unused `_safe_gethostbyname` dropped.  The rest (OpenerDirector, handlers, Request,
  url2pathname, proxies) was already own text.
- Differential checks: 3,000 digest challenges (qop/algorithm/opaque/body/nonce reuse) and
  20,000 macOS bypass cases: 0 differences; FTP fetch + directory listing through
  FTPHandler and CacheFTPHandler against test.test_ftplib's DummyFTPServer identical.
- test_urllib2 85, test_urllib2_localnet 21, test_robotparser, test_site = (pass);
  test_urllib 2 errors before and after (http.client host checks, pre-existing).

### xml/etree/ElementTree.py (partial rewrite)
- Verdict: serialization (`_get_writer`, `_namespaces`, `_serialize_xml/html/text`, the
  escapers, `tostring*`, `indent`), the C14N 2.0 writer target, TreeBuilder and the Python
  XMLParser were CPython's text nearly verbatim.  Rewritten: `_text_sink` (+
  `_BinaryWriteAdapter`), `_qualified_names`, one non-recursive `_emit` for xml/html/text
  with `_write_attributes`, table-driven `_escaped`, `_ChunkCollector` for tostringlist,
  iterative `indent` (one shared string per depth — test_indent_space_caching),
  `C14NWriterTarget` (bindings attached to the element that declares them,
  `_render_name`, `_expand_prefixed`, deferred qname-aware elements opened by `_settle`),
  `TreeBuilder` (`_anchor`/`_as_tail`), `XMLParser` (handler table, `_clark`,
  `_parse_error`, `_setevents` built from one `reporting` factory), `_IterParseIterator` ->
  `_IterParser`.  Kept: `HTML_EMPTY`, `register_namespace._namespace_map`,
  `XMLParser.parser/_parser/target/_target/entity/version/_setevents`,
  `XMLPullParser._parser/_events_queue/_close_and_return_root`, `Element._children`,
  messages.
- Differential checks: 5,000 random trees (namespaces, QNames, comments, PIs, None tags,
  html/xml/text, encodings, declarations, default_namespace, short_empty_elements, indent;
  native and pure Elements) through tostring/tostringlist: 0 differences; canonicalize()
  over the 13 C14N input files x 10 option sets: 0 differences; pure XMLParser +
  TreeBuilder (fed in 97-byte pieces) and iterparse events over all xmltestdata files: 0
  differences.
- test_xml_etree 238, test_xml_etree_c 255, test_pickle, test_doctest, test_pydoc,
  test_regrtest, test_typing = (pass).
- Behaviour changes: a malformed '{uri' name raises ValueError with an own message; a
  qname-aware C14N element without text, or followed directly by a child/comment, is now
  opened before what follows it (the old code could write its end tag or a child first);
  namespace bindings of an element no longer stay visible to its following siblings when
  resolving QName content.

## Reviewed: own text and structure, no action needed
(none yet)

## Still to do
see the end of this file.
