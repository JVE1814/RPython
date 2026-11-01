# Brief for runtime/stdlib workstream agents

You are one of several agents working IN PARALLEL on a from-scratch Rust reimplementation of
CPython 3.15.2 (repo: this git worktree; the main repo is /home/user/RPython). Read, in order:
/home/user/RPython/AGENTS.md (hard rules — e.g. never read CPython/PyPy/RustPython source, never run
any python interpreter, not even `python3 --version`), PLAN.md, DECISIONS.md,
crates/runtime/CONTRIBUTING.md, then skim crates/runtime/src (object model, types, interp).

Workflow
* You are in your own git worktree on your own branch. Commit there often (small commits,
  message prefix = your workstream name). Never push, never commit to master yourself.
* Pull in other work with `git merge master` whenever useful (the coordinator merges finished
  branches into master). Resolve conflicts in shared registration files by keeping both sides.
* Shared files (types/mod.rs, modules/mod.rs, types::init_all, modules::register_all,
  builtins.rs registration lists) — only append the minimal lines you need.
* Build: `export CARGO_TARGET_DIR=/home/user/RPython/target/<your-workstream>`;
  `cargo build -p python` -> `$CARGO_TARGET_DIR/debug/python3`. The interpreter finds the
  stdlib at `<worktree>/Lib` automatically in dev builds.
* If `crates/compiler` does not compile/exist in master yet, you cannot run Python code: write
  your code, keep it compiling (`cargo check -p rpy-runtime` works only if the compiler crate
  builds; if not, run `git merge master` later). Check `git log master -- crates/compiler`.
* Run a spec test module: `cd /tmp && PYTHONPATH=<worktree>/spec/Lib $PY -m test test_xxx` once
  the stdlib can run regrtest; before that, write small scripts exercising your features and
  run the relevant `spec/Lib/test/test_*.py` files directly where possible
  (`$PY -m unittest` needs unittest — another agent writes it; you may run individual test
  functions by importing the test module once possible).
* Use spec/Lib/test as the specification: grep it for the behaviour and exact error messages
  of everything you implement.
* Pure-Python stdlib modules go to `Lib/` (written from scratch from your own knowledge of the
  documented behaviour — do not reproduce CPython's source text). Accelerator/"C" modules are
  Rust in crates/runtime/src/modules/ (follow CPython's Python-vs-C split as you know it).
* Quality bar: behaviour identical to CPython 3.15 as observed by the test suite. Performance
  matters (fast paths for exact builtin types).
* When finished (or when your context is getting full), make sure everything is committed and
  report: branch name, what's done, what's missing, test results, any decisions (also write them
  to `notes/<workstream>.md` in your branch).
