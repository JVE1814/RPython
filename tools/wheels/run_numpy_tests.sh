#!/bin/bash
# Install an unmodified binary wheel (default: numpy) into a venv of our interpreter and run
# the package's own test suite with pytest; optionally run the same suite on the baseline
# free-threaded CPython build (measurement only, DECISIONS D15) and compare the failures.
#
# usage: tools/wheels/run_numpy_tests.sh [options] [-- extra pytest args]
#   -b BIN      our python3 binary (default: $CARGO_TARGET_DIR/release/python3,
#               CARGO_TARGET_DIR defaulting to <repo>/target/numpy)
#   -o OUT      work directory (default: /tmp/rpy-wheels)
#   -w DIR      install from the wheels in DIR only (pip --no-index --find-links DIR);
#               default: download from PyPI
#   -p SPEC     packages to install (default: "numpy==2.5.4 pytest hypothesis")
#   -t PKG      package whose tests run via `pytest --pyargs PKG` (default: numpy)
#   -B          also run the suite on /opt/baseline/py-ft (baseline CPython, free-threaded)
#   -k          reuse an existing snapshot/venv in OUT (skip snapshot + install)
#   -T SECS     timeout for one pytest run (default 14400)
#   -m KB       virtual-memory limit (ulimit -v) for the test runs (default 8000000): numpy has
#               tests that allocate many GB and expect MemoryError; without a limit the
#               kernel's OOM killer ends the whole run instead
# Results: OUT/ours/pytest.log, OUT/ours/junit.xml, OUT/ours/failed.txt (and OUT/baseline/...),
# OUT/only_ours.txt = failures/errors that do not happen on the baseline.
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target/numpy}"
BIN="$TARGET/release/python3"
OUT=/tmp/rpy-wheels
WHEELS=""
PKGS="numpy==2.5.4 pytest hypothesis"
TESTPKG=numpy
BASELINE=0
REUSE=0
TMO=14400
MEMKB=8000000
BASEPY=/opt/baseline/py-ft/bin/python3
while getopts "b:o:w:p:t:BkT:m:" opt; do
    case $opt in
        b) BIN="$OPTARG" ;;
        o) OUT="$OPTARG" ;;
        w) WHEELS="$OPTARG" ;;
        p) PKGS="$OPTARG" ;;
        t) TESTPKG="$OPTARG" ;;
        B) BASELINE=1 ;;
        k) REUSE=1 ;;
        T) TMO="$OPTARG" ;;
        m) MEMKB="$OPTARG" ;;
        *) sed -n '2,25p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ "${1:-}" = "--" ] && shift
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

pip_install() { # venv-python
    local args=(-m pip install -q --disable-pip-version-check)
    if [ -n "$WHEELS" ]; then args+=(--no-index --find-links "$WHEELS"); fi
    # shellcheck disable=SC2086
    "$1" "${args[@]}" $PKGS
}

# run_suite NAME VENV-PYTHON
run_suite() {
    local name="$1" py="$2" dir="$OUT/$1"
    shift 2
    mkdir -p "$dir"
    echo "== $name: pytest --pyargs $TESTPKG"
    local start=$SECONDS
    # run from an empty directory so that nothing local shadows the installed package
    # -v: the log names the running test if the process dies
    # NPY_AVAILABLE_MEM pins numpy's requires_memory() skips (otherwise they depend on what
    # other processes use at that moment). numpy's test_multithreading starts up to 250
    # threads behind a barrier; under `ulimit -v` one can fail to start late in a run and the
    # rest then wait forever (seen on the baseline too), so for numpy it runs separately
    # without the limit.
    local mt=()
    [ "$TESTPKG" = numpy ] && mt=(-k "not test_multithreading")
    (cd "$dir" && export NPY_AVAILABLE_MEM=4GB && ulimit -v "$MEMKB" && timeout "$TMO" "$py" -m pytest --pyargs "$TESTPKG" -v -p no:cacheprovider -rfE \
        --continue-on-collection-errors "${mt[@]}" --junitxml="$dir/junit.xml" "$@" > "$dir/pytest.log" 2>&1)
    local rc=$?
    echo "$rc" > "$dir/exit_code"
    : > "$dir/mt.log"
    if [ "$TESTPKG" = numpy ]; then
        (cd "$dir" && timeout 3600 "$py" -m pytest --pyargs numpy._core.tests.test_multithreading -v -p no:cacheprovider -rfE > "$dir/mt.log" 2>&1)
    fi
    cat "$dir/pytest.log" "$dir/mt.log" | grep -E '^(FAILED|ERROR) ' | sed -E 's/ - .*//' | sort -u > "$dir/failed.txt"
    local summary
    summary="$(grep -E '(passed|failed|error)s? in [0-9.]+s' "$dir/pytest.log" | tail -1)"
    if [ -z "$summary" ]; then
        summary="no pytest summary (exit $rc; crash or timeout) - see $dir/pytest.log"
    fi
    echo "$name: $summary  [exit $rc, $((SECONDS - start))s]" | tee "$dir/summary.txt"
}

if [ "$REUSE" = 0 ]; then
    [ -x "$BIN" ] || { echo "no interpreter at $BIN (build: cargo build --release -p python)"; exit 1; }
    echo "== snapshot $BIN -> $OUT/snap"
    bash "$ROOT/tools/snapshot.sh" "$BIN" "$ROOT" "$OUT/snap" || exit 1
    rm -rf "$OUT/venv"
    echo "== venv (ensurepip) -> $OUT/venv"
    "$OUT/snap/bin/python3" -m venv "$OUT/venv" || exit 1
    echo "== pip install $PKGS"
    pip_install "$OUT/venv/bin/python" || exit 1
    if [ "$BASELINE" = 1 ]; then
        rm -rf "$OUT/basevenv"
        "$BASEPY" -m venv "$OUT/basevenv" || exit 1
        pip_install "$OUT/basevenv/bin/python" || exit 1
    fi
fi

run_suite ours "$OUT/venv/bin/python" "$@"
if [ "$BASELINE" = 1 ]; then
    run_suite baseline "$OUT/basevenv/bin/python" "$@"
    comm -23 "$OUT/ours/failed.txt" "$OUT/baseline/failed.txt" > "$OUT/only_ours.txt"
    comm -13 "$OUT/ours/failed.txt" "$OUT/baseline/failed.txt" > "$OUT/only_baseline.txt"
    echo "== failures/errors only on our interpreter: $(wc -l < "$OUT/only_ours.txt") (list: $OUT/only_ours.txt)"
    echo "== failures/errors only on the baseline:    $(wc -l < "$OUT/only_baseline.txt")"
fi
