# Rules for every agent working in this repo

Project: a from-scratch Rust re-implementation of CPython 3.15.2 (drop-in replacement, free-threaded
C ABI, no GIL, JIT). The spec is the CPython test suite in `spec/Lib/test` and the public headers in
`spec/Include`. Read PLAN.md and DECISIONS.md for architecture.

Hard rules (violating any of these invalidates the project):
1. NEVER read CPython implementation source: nothing under `/opt/baseline/` and nothing from the
   CPython repo except what is in `spec/`. Never read any other Python implementation's source
   (PyPy, RustPython, Jython, GraalPy, MicroPython, ...), including copies in `~/.cargo/registry`.
   Write everything from your own knowledge and from the tests/headers in `spec/`.
2. NEVER run `python3`/`python` or any other Python interpreter, not even for tooling, not even
   `python3 --version` (two agents already did this by reflex — do not).  Tooling is
   bash or Rust. (Exception: `/opt/baseline/py-*` builds are used *only* by the benchmark harness.)
3. NEVER edit anything under `spec/`. Never skip or special-case a test.
4. Only touch the paths you were assigned. Shared files (Cargo.toml at the root, DECISIONS.md,
   PLAN.md, PROGRESS.md) are owned by the coordinator: put your notes/decisions into
   `<your crate>/NOTES.md` and mention them in your final report.
5. Use your own cargo target dir to avoid lock contention:
   `export CARGO_TARGET_DIR=/home/user/RPython/target/<your-crate-name>`.
6. Commit often, only your own paths: `git add <paths> && git commit -m "<msg>" -- <paths>`.
   If `.git/index.lock` exists, wait a few seconds and retry. Do not push. Do not rewrite history.
7. External crates: only general-purpose ones without Python-implementation code, and only when
   the assignment allows it. Default: no dependencies.
8. Your final message must summarise: public API, what is done, what is missing, known bugs.
9. Never reproduce CPython's (or any other Python implementation's) source text from memory —
   not code, not comments, not docstrings. Match the behaviour and the names the tests use; write
   the structure, code and comments yourself. No CPython issue-number comments ("gh-NNNN").
   If you notice you are recalling CPython's text, stop and design it differently. (D19)
