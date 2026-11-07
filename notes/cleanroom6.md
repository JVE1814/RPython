# cleanroom6 — continuation of the D19 audit (see notes/cleanroom1..5.md)

## Method
- As in cleanroom5: a recalled module is rewritten from its list of public names, the
  documented behaviour, the spec tests and a grep of Lib/, crates/ and spec/ for every
  private name other code or the tests use.  The old files (base b9124d2) were kept in an
  untracked `.scratch6/old/` and swapped into an installed snapshot
  (target/cleanroom6/snap) for "before -> after" runs with the same binary.
- Differential fuzzers (`.scratch6/fuzz_*.py`, untracked): seeded random inputs run through
  the old and the new module, every observable compared.
- Speed: own micro-benchmarks (`.scratch6/bench_*.py`) interleaved old/new on the same
  binary (`.scratch6/ab.sh`); pyperformance has nothing for these modules except
  concurrent_imap, whose second benchmark the shim cannot report (it prints the first only).
  The box is noisy (load 5-13): differences under ~15% are noise.
- None of the rewritten files is frozen into the start-up image (D30): frozen.rs lists no
  multiprocessing/xml/email module.  The binary was rebuilt for the Rust change.

## _asyncio Future / test_sslproto `*_failed_certificate_no_cycles` (fixed: 2 -> 0 failures)
The cause was not the frame of `_asyncio.Future.result()`/`__await__` (that code already
drops `self` before raising, and a reachability search found no reference from any frame).
Two separate things kept the transport alive:
1. **QSBR-deferred release of dict values** (crates/runtime/src/types/dict.rs).  With a
   second thread alive (the test's TLS server thread) every value replaced in a dict — so
   every `obj.attr = None` on an instance with a `__dict__` — was released through QSBR, i.e.
   only after a grace period (in practice at the next collection or blocking wait).  The
   failed waiter future (from `SSLProtocol._waiter = None` etc.) stayed alive, and with it
   the exception, its traceback and the transport.  CPython releases at once.  Fix
   (`release_replaced`): the table's `shared_read` flag (set under the dict lock by any
   non-owner before it reads the table lock-free; scale1/scale2 already rely on it for
   freeing data blocks, and the JIT's lock-free dict reads take the slow path for a
   non-owner while it is clear) is read under the lock; a table never marked shared releases
   the old key/value immediately, a shared one still goes through QSBR.  Same for
   `pop_item_hash`.
2. **A transport torn down without close()** (start_tls/create_connection after a fatal
   TLS error) kept itself alive through `_read_ready_cb`, a bound method of itself.
   `_SelectorSocketTransport._call_connection_lost` now clears `_read_ready_cb` as it
   already cleared `_write_ready` (Lib/asyncio/selector_events.py).
- crates/runtime/src/modules/_asyncio.rs: unchanged.
- test_asyncio (35 files, 2929 tests): 2 failures -> 0 (also after `git merge core`).
- test_dict, test_weakref, test_threading, test_free_threading, test_gc, test_capi,
  tools/stress (19/19) and tools/jit_tests (all ok) pass with the change.

## Rewritten

### multiprocessing/pool.py
- Tests (before -> after): test_multiprocessing_{fork,forkserver,spawn} (all four parts
  each), test_multiprocessing_main_handling, test_concurrent_futures, test_genericalias:
  same results (only test_concurrent_futures.test_init fails, before and after).
- Kept because pinned: `Pool`/`ThreadPool` API, `Pool.Process(ctx, ...)`, `_pool`,
  `_state` + `INIT/RUN/CLOSE/TERMINATE`, `_ctx`, `_cache`, `_processes`, `_initializer`,
  `_initargs`, `_maxtasksperchild`, `_repopulate_pool()`, `_wrap_exception`,
  `worker(inqueue, outqueue, initializer, initargs, maxtasks, wrap_exception)`,
  `_helper_reraises_exception`, `RemoteTraceback`, `ExceptionWithTraceback`, `rebuild_exc`,
  `MaybeEncodingError`, `mapstar`, `starmapstar`, `job_counter`, `ApplyResult`/`AsyncResult`
  (+ `__class_getitem__`), `MapResult`, `IMapIterator`, `IMapUnorderedIterator`, `_PoolCache`,
  `_get_tasks`, all messages and the repr; the "PoolWorker" process names.
- New structure: two helper threads instead of three — a supervisor (reads results, hands
  them to the result objects, replaces exited workers, decides when the pool is done; for
  ThreadPool it blocks on the out-queue and is woken by a marker object) and a feeder
  (task batches -> workers).  Shared state is a `_Phase` object, not thread attributes;
  `_PoolCache` wakes the supervisor only when the last job goes after close().
  Termination kills the workers first, then drains the task pipe until the feeder notices.
- Behaviour changes: an exception raised by a user callback is printed instead of killing
  the result thread (which used to hang the pool); dropped private helpers nothing uses
  (`_handle_workers/_tasks/_results`, `_get_sentinels`, `_get_worker_sentinels`,
  `_join_exited_workers`, `_maintain_pool`, `_repopulate_pool_static`, `_setup_queues`'
  `_quick_put/_quick_get`, `_help_stuff_finish`, `_wait_for_updates`, `_guarded_task_generation`,
  `_map_async` keeps its name).
- Speed (s, best of 5, old vs new, 2 rounds): ThreadPool.imap 0.0027/0.0023 vs
  0.0011/0.0017, ThreadPool apply_async x2000 0.011/0.013 vs 0.010/0.011, Pool.map small
  0.16/0.086 vs 0.073/0.109, Pool.imap 0.024/0.036 vs 0.064/0.028 (noise dominates).

### multiprocessing/connection.py
- Tests: as pool.py (same results).
- Kept because pinned: `Connection(handle, readable, writable)` + `_handle`, `_detach`,
  `Listener`, `Client`, `Pipe`, `wait`, `SocketListener`, `SocketClient`, `XmlListener`,
  `XmlClient`, `ConnectionWrapper`, `arbitrary_address`, `address_type`, `families`,
  `default_family`, `BUFSIZE`, `CONNECTION_TIMEOUT`, `MESSAGE_LENGTH`, `_CHALLENGE`,
  `_WELCOME`, `_FAILURE`, `_ALLOWED_DIGESTS`, `_MAX_HANDSHAKE_LEN`, `_LEGACY_LENGTHS`,
  `_get_digest_name_and_payload`, `_create_response`, `_verify_challenge`,
  `deliver_challenge`, `answer_challenge`, `reduce_connection`/`rebuild_connection`; the
  message framing (`!i`, -1 + `!Q`) and the handshake wire format; all messages.
- New structure: no `_ConnectionBase`; framing in `_send_message`/`_read_size`/
  `_read_body` with `os.readinto` straight into the buffer (recv_bytes_into reads into the
  caller's buffer, no copy); `wait()` and `poll()` use `select.poll` directly instead of a
  selector object; one `_require()` check.
- Behaviour changes: the HMAC of a tagged challenge covers the whole challenge including
  the `{digest}` tag (both ends are ours; legacy untagged 20-byte challenges still answer
  with a bare HMAC-MD5); a legacy challenge that happens to start with `{` is still
  understood.
- Speed: Pipe round trips (20k objects, 20k small, 50 x 1 MB): 0.44/0.41 s -> 0.36/0.38 s.

### multiprocessing/managers.py
- Tests: test_multiprocessing_*.test_manager and the manager-type tests of test_processes
  (TestSyncManagerTypes, _TestMyManager, _TestRemoteManager, _TestManagerRestart,
  TestManagerExceptions incl. test_dispatch, shared-memory manager tests): same results.
- Kept because pinned: the wire protocol (`#RETURN`, `#ERROR`, `#TRACEBACK`,
  `#UNSERIALIZABLE`, `#PROXY`, `#GETVALUE`, the RPC method names), `Token`, `dispatch`,
  `convert_to_error`, `RemoteError`, `all_methods`, `public_methods`, `Server` and its public
  methods/attributes (`public`, `registry`, `authkey`, `listener`, `address`, `id_to_obj`,
  `id_to_refcount`, `id_to_local_proxy_obj`, `mutex`, `stop_event`, `fallback_*`),
  `State`, `listener_client`, `BaseManager` (+ `_registry`, `_Server`, `_process`,
  `_address`, `_authkey`, `_state`, `_serializer`, `_Client`, `_create`, `_run_server`,
  `_finalize_manager`, `_debug_info`, `_number_of_objects`), `BaseProxy` (+ `_callmethod`,
  `_getvalue`, `_token`, `_id`, `_manager`, `_tls`, `_idset`, `_close`, `_isauto`,
  `_owned_by_manager`, `_address_to_local`, `_mutex`, `_exposed_`, `_method_to_typeid_`),
  `RebuildProxy`, `MakeProxyType`, `AutoProxy`, every proxy class and exposed list,
  `SyncManager` registrations, the shared-memory classes; proxy repr; messages.
- New structure: `_short_request` for every one-shot connection; `_raise_reply` keeps
  raised replies out of the caller's frame (the test_dispatch no-cycle property);
  `Server._answer_first_request`, `_call_shared`, `_fallback`, `_send_reply`, `_lookup`;
  `_creation_method` builds the registered methods; `_forwarder` instead of exec-built
  methods; `_in_place` for the augmented assignments; SyncManager registered from a table;
  `SharedMemoryManager._tracked`.
- Behaviour changes: a request that cannot be unpickled on the server now gets a
  `#TRACEBACK` reply instead of silently ending the serving thread (the client used to
  hang); `BaseManager.join()` works on a manager that was only connect()ed.

### multiprocessing/queues.py
- Verdict: `Queue._feed` and friends were CPython's text (debug strings included).
- Tests: as pool.py (same results; one run of test_multiprocessing_spawn.test_processes
  reported "env changed" from a GC-timed `ReentrantCallError` in resource_tracker during a
  semaphore finalizer — passes on rerun, not related to the queue code).
- Kept because pinned: `Queue`, `JoinableQueue`, `SimpleQueue` (+ `__class_getitem__`),
  `_reader`, `_writer`, `_rlock`, `_wlock`, `_poll`, `_send_bytes`, `_recv_bytes`, `_sem`,
  `_maxsize`, `_opid`, `_buffer`, `_notempty`, `_thread`, `_jointhread`, `_joincancelled`,
  `_closed`, `_close`, `_ignore_epipe`, `_terminate_broken`, `_on_queue_feeder_error`
  (overridden by concurrent.futures), `_reset`, `_after_fork`, `_start_thread`,
  `_finalize_join`, `_finalize_close`, `_feed`, `_unfinished_tasks`, `_cond`, the pickled
  state layout, messages.
- New structure: the feeder takes all buffered items per wake-up; `_check_open`/`_enqueue`.
- Speed (20k put/get + 5k joinable round trips): 0.20-0.21 s -> 0.19-0.23 s (noise).

### xml/dom: minidom.py, expatbuilder.py, pulldom.py, xmlbuilder.py
- Tests (before -> after): test_minidom 133, test_pulldom 11 (its three expected failures
  still fail), test_xml_dom_minicompat 11, test_xml_dom_xmlbuilder 4, test_sax 192,
  test_xml_etree(_c), test_pyexpat, test_xmlrpc, test_docxmlrpc, test_plistlib = (pass).
- Differential check: 300 random documents (DTD with entities/notations/ATTLIST/ELEMENT,
  namespaces, CDATA, comments, PIs, escapes) parsed with and without namespaces; full tree
  dumps (names, namespaces, attributes with isId/schemaType/specified, wholeText,
  isWhitespaceInElementContent, doctype/entities/notations/internalSubset), toxml/
  toprettyxml in three encodings, pickle and deepcopy, getElementsByTagName(NS),
  getElementById before/after setIdAttribute, cloneNode/importNode, random tree edits +
  normalize, pulldom event streams, DOMBuilder features, a DOMBuilderFilter.  Remaining
  differences are all deliberate (below).
- Kept because pinned: the class hierarchy and every public name, `defproperty`
  properties, `__slots__`, `_child_node_types`, `_nodeTypes_with_children`, `_no_type`,
  `_append_child`, `_set_attribute_node`, `_clear_id_cache`, `_in_document`, `_write_data`,
  `_get_elements_by_tagName(_ns)_helper`, `_nssplit`, `_clone_node`, `_do_pulldom_parse`,
  `_get_containing_element/_entref`, `_ensure_attributes`, `_attrs`/`_attrsNS`,
  `_magic_id_nodes`/`_magic_id_count`, `_id_cache`, `_id_search_stack`, `_elem_info` keyed by
  qualified tag name, `_get_elem_info`, `_create_entity`/`_create_notation`,
  `_identified_mixin_init`, `_seq`, `_user_data`, `_call_user_data_handler`; expatbuilder's
  `ElementInfo` (+ `_attr_info`, `_model`), `ExpatBuilder(NS)`, `FragmentBuilder(NS)`,
  `Namespaces`, `InternalSubsetExtractor`, `ParseEscape`, `FilterVisibilityController`,
  `parse*`, `makeBuilder`, `_intern`, `_parse_ns_name`; pulldom's events, `PullDOM`,
  `SAX2DOM`, `DOMEventStream` (+ `pulldom`, `parser`, `stream`, `reset`, `getEvent`),
  `ErrorHandler`, `default_bufsize`, and the quirks its expected-failure tests pin (no
  COMMENT / END_DOCUMENT events from a real parser; SAX2DOM fails on a PI before the
  root); xmlbuilder's classes, `Options` defaults, feature names and messages.
- New structure: minidom's child/sibling plumbing through `_join`/`_position_of`;
  O(1) id-cache invalidation via ownerDocument (testAppendChildNoQuadraticComplexity);
  getElementById resumes a DFS and caches every ID it passes; elements by tag name via a
  `_descendant_elements` iterator.  expatbuilder: one handler set with state flags
  (`_in_subset`, `_ignoring`, `_created`, `_cdata_open`) instead of swapping parser handlers
  and the FilterCrutch/Rejecter/Skipper classes; the namespace version only overrides
  `_make_element`; fragments are parsed in a wrapper document built from the context's DTD
  and in-scope xmlns attributes, then imported.  pulldom: a deque event queue (no
  `firstEvent`/`lastEvent` linked list).  xmlbuilder: feature settings derived from small
  sets instead of a pair table; `_base_of` for DOMEntityResolver.
- Behaviour changes: DOMBuilderFilter.startContainer() is now consulted in namespace mode
  too (the namespace builder used to skip it); getElementById() finds an element whose
  declared ID attribute comes after other declared attributes (the old search stopped at
  the first non-ID attribute); Document.removeChild() relinks the neighbours' sibling
  pointers; pulldom builds `xml:lang` (not `lang`) for attributes in the XML namespace;
  `ElementInfo.isIdNS()` accepts the two-argument call minidom makes; removed internal
  helper classes FilterCrutch, Rejecter, Skipper, `_FRAGMENT_BUILDER_*`.
- Speed (s, 3000-element document): parse 0.083/0.071 -> 0.082/0.074, toxml 0.032 ->
  0.027-0.032, toprettyxml equal, clone 0.067/0.070 -> 0.077/0.065, pulldom 0.11 -> 0.10-0.11,
  20k createElement+createTextNode+appendChild 0.024 -> 0.030/0.026 (then inlined).

### xml/sax: expatreader.py, saxutils.prepare_input_source
- Tests: test_sax 192, test_pulldom, test_minidom, test_xml_dom_xmlbuilder, test_xmlrpc,
  test_docxmlrpc = (pass).
- Differential check: 300 documents (20% corrupted) x namespaces on/off x parse()/feed()
  in random chunks with random flush(), with lexical handler and string interning at
  random; every content/DTD/lexical event with the locator position, errors with
  line/column, features: identical.
- Kept because pinned: `ExpatParser` and its handler method names, `_parser`, `_source`,
  `_cont_handler` hookup, `_parsing`, `_entity_stack`, `_namespaces`, `_lex_handler_prop`,
  `_interning`, `_external_ges`, `_ClosedParser`, `ExpatLocator`, `create_parser`, the
  deprecated `version` (module `__getattr__`), every message.
- New structure: `_ns_pair`/`_ns_qname` helpers, an `_UNSUPPORTED` feature table,
  `_report` for expat errors; prepare_input_source split into `_input_source_for_file` and
  `_open_system_id`.
- Speed (5000-element document through XMLGenerator): 0.065/0.071 -> 0.060/0.048,
  namespaces 0.046/0.054 -> 0.044/0.066 (noise).

### email/_header_value_parser.py: the RFC 5322 grammar functions
- Rewritten: get_qp_ctext, get_qcontent, get_atext, get_bare_quoted_string, get_comment,
  get_cfws, get_quoted_string, get_atom, get_dot_atom_text, get_dot_atom, get_word,
  get_phrase, get_local_part, get_obs_local_part, get_dtext, get_domain_literal,
  get_domain, get_addr_spec, get_obs_route, get_angle_addr, get_display_name,
  get_name_addr, get_mailbox, get_invalid_mailbox, get_mailbox_list, get_group_list,
  get_group, get_address, get_address_list, get_no_fold_literal, get_msg_id (and the
  `_domain_literal_ended_early` helper).  Earlier agents' versions of the lexical
  helpers, get_encoded_word, get_unstructured, parse_message_id(s), the MIME-parameter
  functions and the refolder are kept.
- Tests (before -> after): test_email 1818 (pass, incl. test__header_value_parser 373),
  test_smtplib, test_mailbox, test_imaplib, test_http_cookies, test_urllib2, test_httplib.
- Differential check: 40,000 random header values (two alphabets: specials/encoded
  words/controls/surrogates, and realistic address pieces) x 33 functions; compared parse
  tree (`ppstr()`), str, value, all defects (type and text), instance attributes of every
  token (this found the `_in_angle_addr` marker the refolder uses), rest or exception
  type and message: 0 differences.
- New structure: helpers `_leading_cfws`, `_put_first`, `_leads_with`, `_parse_error`,
  `_quoted_word_pieces`, `_collapse_ew_gap`/`_starts_with_ew` (RFC 2047 6.2 gap rule),
  `_is_dot_maybe_after_cfws`, `_optional_fws`, `_route_domain`, `_list_entry_failed`
  (shared by mailbox-list and address-list), `_id_left`/`_id_right`.
- Behaviour changes: none found.
- Speed (1200 address lists + message ids): 0.129-0.137 s -> 0.124-0.134 s.

## Still recalled / not done
- multiprocessing: synchronize, process, util, heap, context, forkserver, popen_*,
  reduction, resource_sharer, resource_tracker, shared_memory, sharedctypes, spawn, dummy
  were not rewritten (queues showed CPython's text, so the rest of the package from the same
  commit is presumably recalled too; ~3,900 lines).
- email/_header_value_parser.py: the MIME half (get_parameter, parse_mime_parameters,
  parse_content_type_header, get_section/get_value, parse_mime_version, ...) was not
  reviewed in detail; it carries some earlier restructuring (`_get_run`,
  `_get_cfws_wrapped`) but get_parameter's RFC 2231 logic may still follow CPython.
- xml.sax.handler / xmlreader (API classes) not sampled; asyncio/tasks.gather (cosmetic,
  per cleanroom5).

## Known failures (installed snapshot after `git merge core`)
- test_concurrent_futures.test_init: `_resource_tracker._exitcode` 1 != 0 — fails the same
  with the base files (pre-existing, also noted by cleanroom5).
- Final run (installed snapshot of the merged tree, 141 test files, 11,677 tests): failed
  test_concurrent_futures.test_init (above), test_descr (test_vicious_descriptor_nonsense,
  test_method_get_meth_method_invalid_type) and test_gc (test_get_referents_on_capsule,
  test_tuple_untrack_counts) — identical with core's own binary (target/core), so
  pre-existing.  "env changed" (timing flakes under load ~10; spawn.test_processes and test_ordered_dict
  passed when rerun, the asyncio pair was not rerun):
  test_asyncio.test_events and test_streams (uncaught "Invalid file descriptor: -1" in
  test_utils' HTTP server thread, a race in the test helper also seen by cleanroom5 with the
  old files), test_multiprocessing_spawn.test_processes (ReentrantCallError from a semaphore
  finalizer inside the resource tracker), test_ordered_dict ("generator already executing").
- tools/jit_tests/run.sh: all ok; tools/stress/run.sh: 19/19 PASS.
