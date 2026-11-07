# Benchmark harness
* `run_bench.sh PY OUT.tsv [names]` runs pyperformance 1.14.0 benchmarks
  (/opt/bench/dl/pyperformance-1.14.0, downloaded from PyPI) through `shim/pyperf`, a minimal
  pyperf-compatible runner (calibrated loops, warmup, median of BENCH_RUNS runs). The same
  shim is used for the baseline CPython builds, so comparisons are like-for-like.
* Benchmarks needing third-party packages get them via `BENCH_EXTRA_PATH` (site dirs).
* Startup: `hyperfine`-style loop of `PY -c pass` / `PY -S -c pass`; memory: peak RSS via
  /proc/self/status VmHWM; scaling: tools/bench/scaling/run.sh (23 workloads, 1/2/4 threads, interleaved repetitions,
  CPU-time and machine-load checks; see the script header and notes/scale1.md). The old
  tools/bench/scaling.py is a one-workload quick check.
