# cleanroom-sys — clean-room rewrite of inspect, logging, tempfile, shutil

Per D19 / AGENTS rule 9 the previous versions of these modules (written from
recalled CPython source) were deleted unread and replaced by new
implementations written from the documented API and from what the spec tests
observe.  The old files were never opened (not via an editor, `git show` or
grep); the "before" numbers were measured by running an untouched copy of
master's `Lib/` (`git archive`) through the same harness.

Files: `Lib/inspect.py`, `Lib/logging/__init__.py`, `Lib/logging/handlers.py`,
`Lib/logging/config.py` (new: dictConfig, fileConfig, listen/stopListening),
`Lib/tempfile.py`, `Lib/shutil.py`.

## Design notes
- shutil.rmtree: iterative (explicit stack, no recursion limit issues).  The
  fd-based variant (`rmtree.avoids_symlink_attacks`) walks with
  `os.open(..., dir_fd=)` + `os.scandir(fd)` and checks `samestat` before
  descending.  The private `_onexc_kwargs=True` flag passes `direntry=` and
  `dir_fd=` (the descriptor of the containing directory) to the handler.
- tempfile.TemporaryDirectory cleanup recovers from PermissionError through
  those descriptors (`fchmod(dir_fd)` then retry relative to it), so a
  directory swapped for a symlink during recovery is never followed, and
  the parent of the temporary directory is never modified.
- logging.config: handler configs are processed on a converting *copy* so a
  failed attempt (e.g. a reference to a handler configured later) can be
  retried; QueueHandler subclasses get the queue positionally so extra
  config keys reach `**kwargs` unchanged.
- logging.LogRecord.msecs is derived from the float stored in `created`, so
  the two never disagree about the second.
- TimedRotatingFileHandler: MIDNIGHT/W* rollovers are computed in calendar
  terms (date arithmetic + mktime); ambiguous/missing local times prefer the
  DST flag in effect "now" and fall back to the other flag only if it
  reproduces the requested wall time.  An existing backup for a period is
  never overwritten (the current file keeps growing instead).
- inspect: class source lookup uses `__firstlineno__`; text signatures are
  parsed with `ast` and defaults evaluated by a small restricted evaluator.

## Runtime / other-module issues found (not fixed here)
- `list.__getitem__(sub_instance, i)` called unbound on a list *subclass*
  that overrides `__getitem__` recurses into the override (tuple is fine).
  logging.config works around it.
- `string.Template.pattern` on the base class is an uncompiled marker
  (`_TemplatePattern`), so `Template.pattern.finditer` fails; logging uses
  its own regex for '$' style validation.
- `FileIO(..., 'w+').mode` gives 'rb+' while test_tempfile expects 'wb+' for a
  rolled-over SpooledTemporaryFile (test_properties).
- `code.co_positions()` reports no column information, so the
  `positions` checks in test_inspect's stack tests cannot pass.
- Many test modules cannot be imported on master because dependencies are
  missing: pathlib, unittest.mock, asyncio, datetime, socket, socketserver,
  configparser, tarfile, zipfile, py_compile, email, http, urllib,
  multiprocessing.  Hence all four target tests fail at import under the
  real regrtest both before and after this work.

## Measuring
Because of the import failures, pass rates were measured with a local stub
harness (not committed): unimportable modules are replaced by inert stubs so
the remaining tests run; numbers are comparable between before/after only.
