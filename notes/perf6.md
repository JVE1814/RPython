# perf6 workstream — start-up image, XML engine, dict reads, printf formatting

Branch: the perf6 worktree (based on `core` a1ff9d4 = master + perf5, merged with the D29
rustflags change). Everything written from scratch. Method as perf3-perf5: callgrind
instruction counts are the primary metric (`tools/bench/perloop.sh`,
`tools/bench/micro/percall.sh`), wall clock through `tools/bench/run_bench.sh` pinned to one
core, back to back with the baselines, is indicative only (D22: other agents running; the
same binary varies by 10-15% between runs, see "Wall clock").

New tooling:
* `tools/bench/micro/xmlparse.py` (percall kinds `fromstring`, `iterparse`, `tostring`,
  `build`: instructions per element of xml_etree's generated document).
* `tools/bench/micro/b64.py` (base64_small's building blocks: `enc`, `dec`, `enc_raw`,
  `dec_raw`).
* `tools/bench/startup_mem.sh` also prints the start-up RSS split (`startup_vmrss_kb`,
  `startup_rssanon_kb`, `startup_rssfile_kb`), so the baselines can be compared through the
  harness.
* `tools/jit_tests/t_late_methods.py` (LoadMethod sites that first run after compilation,
  polymorphic receivers, a method replaced on the class mid-loop, instance shadowing).

## Start-up image (item 1)

HotSpot's CDS archive is the model: the classes every JVM start needs are kept in a mapped
archive in their loaded form. Here:

* `crates/runtime/build/main.rs` (new build script of the runtime crate) compiles every
  frozen module listed in `src/frozen.rs` (importlib's `_bootstrap`/`_bootstrap_external`,
  zipimport, abc, codecs, io, `_collections_abc`, `_sitebuiltins`, genericpath, ntpath,
  posixpath, os, site, stat, importlib.util/machinery, runpy and the `__hello__` test
  modules) with our own parser/compiler crates (build-dependencies), and runs the same
  translator the runtime uses on first execution (`src/interp/translate.rs` and `wc.rs` are
  compiled into the build script through `#[path]`). It writes `OUT_DIR/startup_image.bin`
  (format in `src/startup_image/fmt.rs`): a string table, the module table, the code records
  (constants inline, nested code objects inline), and the translated forms out of line in
  two areas (module/class bodies first, functions after), start-up modules first. A source
  that needs the Unicode database to compile (non-ASCII identifiers, `\N{...}`) is left out
  and compiled at run time as before; none is today.
* `src/startup_image/mod.rs` turns a record into code objects directly (strings interned
  once per process and cached by id; `types::code::new_code_from_image` skips validation
  and re-interning). The translated form stays in the image until the code first runs:
  `CodeMeta::image_tx` points at it and `build_exec` decodes it instead of translating.
* `_imp.get_frozen_object` / `frozen_code` use the image for every frozen module;
  `install_importlib` takes the two bootstrap modules from the image when frozen modules are
  in use (no file access at all; the `.rpython-boot` cache next to the source is only used
  when they are off).
* Frozen modules are now on by default, as in CPython (`use_frozen_modules = not
  Py_DEBUG`; `-X frozen_modules`, `PYTHON_FROZEN_MODULES` as before), except when the
  standard library is the source tree's `Lib/` (no installation found; new
  `PathConfig::source_tree`): there the files on disk stay authoritative, like a CPython
  build directory. So the agents' workflow (edit Lib, run target/release/python3) is
  unchanged; installed snapshots run the frozen copies. Consequence for agents: a snapshot
  must be rebuilt (cargo build) after changing one of the frozen modules' sources, as with
  CPython's `make regen-frozen`; cargo tracks the sources (`rerun-if-changed`).
* `Lib/os.py`: the `__all__` dedup added by a recent merge scanned the list per name
  (quadratic, ~8M instructions at every start); a set now.
* `CodeMeta` keeps views of the co_consts / co_names / co_localsplusnames tuples instead
  of three copied vectors per code object (`types::code::ItemsView`; the one place that
  swaps co_consts for an equal shared tuple re-points the view, `replace_consts`).
* `crates/launcher/startup.ld` regenerated (tools/bench/perf/gen_order.sh): the old list no
  longer matched most function names after the recent merges; text resident at `-c pass`
  7.4 MB -> 3.4 MB.

## XML (item 3)

`pyexpat/engine.rs` keeps its char-indexed design (rewriting all 2800 lines byte-based was
not needed for parity); the per-element allocations and per-character state machines went:
* element and attribute names are shared `Arc<str>` (`engine::Name`) from a bounded
  per-parser table (`name_at`): start/end events and the open-element stack clone a
  reference instead of allocating three Strings per element;
* ASCII text and attribute values are built with one allocation (`text`), attribute values
  without references or line breaks skip `attr_value` (and its copy of the literal);
  duplicate attributes are checked against the names already collected;
* the ASCII run in `decode` is scanned eight bytes at a time; the char buffer is reserved
  once per feed (it doubled from empty for every document);
* position tracking sums whole ranges (vectorisable) instead of a per-character state
  machine; ElementTree's parser (`lazy_positions`) does not track positions for element,
  text and namespace events at all (it reads them only for errors and entity references;
  the tracker catches up in bulk before compaction and errors);
* namespace processing without any declaration (ElementTree always passes a separator):
  no hashing of the empty prefix per element; DTD attribute-list lookup skipped while the
  DTD has none;
* `_elementtree`: the text buffer keeps its capacity, events are not built when nobody
  listens.

## Calls, dicts, formatting (items 2, 5, 6)

* Lock-free dict reads (scale1/scale2's `lookup_lockfree_raw`) decided only identity and
  exact-str keys; any other key with an equal hash fell back to the locked path, so every
  lookup with a fresh tuple key (mdp, bpe_tokeniser's Counter of byte pairs, comprehensions'
  ids above 256) did the probe twice. Exact int/float/bytes/tuple keys are now compared
  with `plain_eq` in the lock-free probe (no Python code can run; same reclamation argument
  as for str keys).
* `plain_eq` (also used by dict find, sets and tuple comparison) handles exact bytes;
  `tuple_hash` hashes leading bytes items without the recursion guard.
* `abstract_::hash`: one-digit ints inline.
* `slot_tp_richcompare` calls a plain function found by its one lookup (it looked the name
  up twice); `slot_mp_ass_subscript` calls an inherited C `__setitem__` (dict subclasses
  that only override `__delitem__`, such as Counter) directly instead of through the
  wrapper descriptor.
* `str % args`: bare `%s`/`%d`/`%i` (no key, flags, width, precision) and `%(key)s` are
  formatted without the spec parser; machine-size ints are written directly
  (`'LEAF-%d-%d' % (c, i)` cost ~2000 instructions, the dominant cost of xml_etree's
  tree building; logging's record formats use `%(name)s`).
* Builtins with keyword arguments: names compared by length and first byte before the full
  compare (a memcmp call per candidate name).
* Tier 1: a LoadMethod site whose cache was empty when the function was compiled (code
  after the loop whose back edges triggered compilation) reads the first cache entry at run
  time and takes a METHOD hit inline (seqlock snapshot of kind/tag/method, type-tag guard),
  instead of calling `h_load_method` on every execution (bpe_train's merge loop: 15M helper
  calls per run).

### Shared design points (for refs1 / scale2)
* JIT pieces touched (all flagged `(perf6)`): tier-1 `attr_op` (run-time read of the first
  LoadMethod cache entry; pushes an owned method reference and moves the receiver into the
  self slot), tier-1 PushExcInfo/PopExcept/CheckExcMatch helpers (`h_push_exc_info`,
  `h_pop_except`, `h_check_exc_match`: owned references in and out, same stack effects as
  `exec_misc`), `jit::run` handler re-entry (`Run::Propagated`: compiled code already
  unwound the exception out of the frame; eval_inner returns the error without unwinding
  again). With borrowed stack slots these helpers must follow whatever ownership tag the
  slot carries, like `exec_misc` does.
* `unwind_compiled` links the frame under `ts.frame` itself; `jit::run`'s caller frame is
  already current, so run() presents the caller and restores both links around it.

## Measurements
"base" = this worktree at the merge (ee46d9d, `core` + D29), installed snapshot; "perf6" =
the branch head. CPython = /opt/baseline/py-default through the bench scripts.

### Start-up and memory
| | base | perf6 | CPython default |
|---|---|---|---|
| `-c pass` instructions | 64.8M (58.3M with the os.py fix alone) | 39.2M | 40.5M (perf2) |
| `-S -c pass` instructions | 28.3M | 19.7M | 27.7M (perf2) |
| `-c pass` wall, best of 40, pinned, interleaved | 13.1 ms | 8.9 ms | 10.1 ms (ft 11.9) |
| `-S -c pass` wall | 7.3 ms | 5.6 ms | 7.4 ms |
| start-up RSS (anon / file) | 13.1 MB (4.1 / 9.0) | 12.0 MB (3.9 / 8.1) | 9.2 MB (3.3 / 5.9) |
| startup_mem.sh peak_rss_kb | 37352 (36.5-38.7 across runs) | 35796 | 32336 (ft 38640, perf4) |

### Instructions per operation (`percall.sh`)
| kind | base | perf6 | CPython default |
|---|---|---|---|
| xmlparse fromstring (per element) | 6703 | 4365 | 4507 |
| xmlparse iterparse | 13429 | 10810 | 9393 |
| xmlparse tostring | 11464 | 11487 | 14622 |
| xmlparse build (SubElement, `'%d' %`) | 6214 | 3942 | 4429 |
| calls.py keyerror (miss + except) | 2396 | 1867 | 1841 |
| calls.py exc (raise + catch one down) | 4210 | 4004 | 3686 |
| calls.py counter (`stats[pair] += 1`) | 2620 | 2213 | 2360 |
| calls.py dunder | 1962 | 1954 | 1452 |
| calls.py kw / init / super / prop / bound | 796 / 762 / 2226 / 400 / 457 | 767 / 759 / 2215 / 401 / 454 | 701 / 1253 / 1911 / 441 / 502 |
| calls.py map / sortkey / getattr_call / alloc / star / kwargs | 684 / 523 / 1125 / 943 / 629 / 2916 | 681 / 522 / 1151 / 937 / 605 / 2932 | 542 / 502 / 1094 / 762 / 856 / 1818 |
| b64.py enc / dec / enc_raw / dec_raw | 2944 / 4073 / 1521 / 1774 | 2958 / 4024 / 1496 / 1780 | 2449 / 4373 / 1560 / 1481 |

### Steady state per benchmark loop (callgrind, millions, `perloop.sh`)
| bench (windows) | base | perf6 | CPython default |
|---|---|---|---|
| typing_runtime_protocols (300/1300) | 1.92 | 1.32 | 1.68 |
| deepcopy (100/400) | 3.29 | 3.24 | 2.83 |
| logging (500/1500) | 1.28 | 1.28 | 1.05 |
| comprehensions (50/300) | 0.22 | 0.22 | 0.19 |
| mdp (1/2) | 12561 | 12412 | - |
| bpe_tokeniser, one loop incl. start-up | 48.66G (before the LoadMethod fix) | 46.80G | - |

(The perloop windows matter: several benchmarks are not linear in the loop count for us,
e.g. typing at 50/300 reads 0.22M; use the windows given.)

### Wall clock
s/loop through run_bench.sh, the four binaries back to back per benchmark, pinned to core 3,
other agents running; ratio = x / best CPython. Noisy benchmarks were re-run three times
interleaved (the ratios shown are from those runs). Single runs of the same binary vary by
10-15% (a three-binary layout experiment showed the same spread for one binary).

| bench | base ratio | perf6 ratio |
|---|---|---|
| python_startup (best of 40) | 1.30 | 0.88 |
| xml_etree | 1.70 | 1.21 |
| mdp | 1.52 | 1.32 |
| bpe_tokeniser | 1.35 | 1.29 |
| typing_runtime_protocols | 1.39 | 1.32 |
| logging | 1.33 | 1.31 |
| comprehensions | 1.30 | 1.27 |
| nqueens | 1.28 | 1.22 |
| deepcopy | 1.47 | 1.48 |
| pathlib | 1.16 | 1.17 |
| base64 | 1.17 | 1.15 |
| async_generators | 1.12 | 1.12 |
| pprint | 1.10 | 0.82 |
| concurrent_imap | 0.90 | 0.97 |

typing_runtime_protocols now runs fewer instructions than CPython (1.32M vs 1.68M) and is
still 1.3x in wall time: not explained by instruction counts (cache/branch behaviour of the
call paths; not investigated further).

## Correctness
tools/jit_tests/run.sh (with the new t_late_methods.py) and tools/stress/run.sh pass on
the final build (after merging `core` 91b8c20). Spec modules through tools/run_tests.sh on
installed-layout snapshots of the starting point and of the head (frozen modules on in the
head's snapshot): no new failure anywhere; the differences are improvements that came with
the `core` merge (test_asyncio 10 -> 2 failures, test_free_threading 2 -> 0, test_import
3 -> 0). Final gate: test_call test_descr test_class test_exceptions test_traceback
test_sys_settrace test_monitoring test_generators test_coroutines test_asyncio test_logging
test_pathlib test_xml_etree test_xml_etree_c test_pyexpat test_sax test_minidom test_copy
test_typing test_gc test_threading test_free_threading test_pickle test_importlib
test_import test_frozen test_cmd_line test_sys test_embed test_fractions test_dict test_str
test_format test_inspect test_dis. Earlier rounds (identical run/failure/error counts to the
starting point): test_site test_runpy test_zipimport test_pkgutil test_os test_posixpath
test_abc test_code test_compile test_linecache test_warnings test_set test_tuple test_bytes
test_collections test_hash test_dictviews test_ordered_dict test_userdict test_float
test_long test_bool test_compare test_marshal test_frame test_funcattrs test_capi
test_except_star test_raise test_with test_exception_group test_asyncgen test_contextlib
test_contextlib_async test_xml_dom_minicompat test_string_literals.

## What is left (measured causes)
* Peak memory (startup_mem.sh ~37 MB vs CPython 32.3 MB; ft CPython 38.6). The workload
  part is within ~1 MB of CPython (bigger ABI headers offset by tighter blocks); the gap is
  start-up RSS: 12.7-13.1 MB vs 9.2 MB, of which ~3 MB file-backed. Measured with
  /proc/self/pagemap: at a small script, .rodata is almost entirely resident (2.1 of
  2.6 MB) because scattered constants pull in whole 64 KB fault-around windows, and 4.5-5 MB
  of text is resident. MADV_RANDOM on the segments changes nothing on this kernel (tried,
  reverted). Next steps: order .rodata like the text (constants touched at start-up first;
  needs an access trace, the symbols are mostly anonymous), keep startup.ld current
  (gen_order.sh after every merge: names drift and the order decays — after merging scale2
  RssFile rose from 8.7 to 9.3 MB until regenerated), smaller code for cold paths.
* Calls not made by the machine-code call routine still cost 250-300 instructions in Rust
  (`h_call` self cost is 100+ per call in typing_runtime_protocols: generic dispatch,
  stack release loop, `vectorcall_raw` for classes/bound methods/method-wrappers);
  typing_runtime_protocols 1.92M vs 1.68M per loop, deepcopy 3.25M vs 2.83M (100/400).
  Keyword-only defaults are dict lookups by name per call (b64encode: ~140 instructions
  each).
* LoadAttr sites that first run after compilation still call the helper forever in that
  compiled code (only LoadMethod METHOD got the run-time entry read); INSTANCE_DICT needs
  the shape hint at run time.
* iterparse (10.8k vs 9.4k per element): Python-level generator plumbing of
  `_IterParseIterator` (`slot_tp_iternext` -> Python `__next__` -> generator) and
  `el.tag` through generic getattr.
* xml_etree's tree building (`build`, 3.9k vs 4.4k) and parse are at or below CPython;
  the engine is still char-indexed (Vec<char> + widths, ~350 instructions per element of
  decoding and scanning).
* `exc` (raise + catch one frame down) 4.0k vs 3.7k: AttributeError's own `__init__`
  through type_call; traceback entries need a frame object each (as in CPython: the public
  `tb_frame` field must be set, so this stays).
* mdp (12.41G vs 12.56G instructions per loop, ~1.4x wall): Fraction arithmetic in Python,
  namedtuple `_replace`, generator expressions resumed from Rust.
