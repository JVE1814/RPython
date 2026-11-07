"""Command-line front end of the sampling profiler (``python -m profiling.sampling``).

Subcommands:

* ``run``     start a script (or ``-m module``) and profile it from its first line;
* ``attach``  profile a running process;
* ``dump``    print one stack snapshot of a running process;
* ``replay``  turn a profile recorded with ``--binary`` into another format.
"""

import argparse
import os
import socket
import subprocess
import sys

from . import constants
from .binary_reader import BinaryReader, apply_recorded_stats
from .errors import (
    SamplingModuleNotFoundError,
    SamplingScriptNotFoundError,
    SamplingUnknownProcessError,
)
from .sample import _is_process_running, dump_stack, sample, sample_live

__all__ = ('main',)

FORMAT_EXTENSIONS = {
    "pstats": "pstats",
    "collapsed": "txt",
    "flamegraph": "html",
    "diff_flamegraph": "html",
    "gecko": "json",
    "jsonl": "jsonl",
    "binary": "bin",
}

# formats written as a directory rather than a file
_DIRECTORY_FORMATS = {"heatmap"}
# formats whose result can be opened in a browser
_BROWSER_FORMATS = {"flamegraph", "diff_flamegraph", "heatmap"}
# formats that can show per-opcode information
_OPCODES_COMPATIBLE_FORMATS = {"pstats", "collapsed", "flamegraph",
                               "diff_flamegraph", "gecko", "heatmap", "binary"}

_MODES = {
    "wall": constants.PROFILING_MODE_WALL,
    "cpu": constants.PROFILING_MODE_CPU,
    "gil": constants.PROFILING_MODE_GIL,
    "exception": constants.PROFILING_MODE_EXCEPTION,
}

_SORT_MODES = {
    "nsamples": constants.SORT_MODE_NSAMPLES,
    "tottime": constants.SORT_MODE_TOTTIME,
    "cumtime": constants.SORT_MODE_CUMTIME,
    "sample-pct": constants.SORT_MODE_SAMPLE_PCT,
    "cumul-pct": constants.SORT_MODE_CUMUL_PCT,
    "nsamples-cumul": constants.SORT_MODE_NSAMPLES_CUMUL,
    "name": -1,
}

_DEFAULT_RATE_HZ = 1000
_DEFAULT_LIMIT = 15
_READY_TIMEOUT = 30.0


def _parse_mode(value):
    """A --mode name as its PROFILING_MODE_* value (KeyError if unknown)."""
    return _MODES[value]


def _parse_rate(text):
    """A sampling rate such as ``1000``, ``250hz`` or ``10khz`` as Hz."""
    value = str(text).strip().lower()
    scale = 1
    for suffix, factor in (("mhz", 1_000_000), ("khz", 1000), ("hz", 1)):
        if value.endswith(suffix):
            value = value[:-len(suffix)]
            scale = factor
            break
    try:
        rate = float(value) * scale
    except ValueError:
        raise argparse.ArgumentTypeError(f"invalid sampling rate: {text!r}") from None
    if rate <= 0:
        raise argparse.ArgumentTypeError("the sampling rate must be positive")
    return rate


def _format_rate(interval_usec):
    return str(max(1, round(1_000_000 / interval_usec)))


def _generate_output_filename(output_format, pid):
    if output_format in _DIRECTORY_FORMATS:
        return f"{output_format}_{pid}"
    ext = FORMAT_EXTENSIONS.get(output_format, output_format)
    return f"{output_format}_{pid}.{ext}"


def _create_collector(output_format, sample_interval_usec, skip_idle=False, *,
                      mode=None, output_file=None, compression="auto",
                      capture_config=None, diff_baseline=None, opcodes=False):
    """The collector writing *output_format*."""
    if output_format == "pstats":
        from .pstats_collector import PstatsCollector
        return PstatsCollector(sample_interval_usec, skip_idle=skip_idle)
    if output_format == "collapsed":
        from .stack_collector import CollapsedStackCollector
        return CollapsedStackCollector(sample_interval_usec, skip_idle=skip_idle)
    if output_format == "flamegraph":
        from .stack_collector import FlamegraphCollector
        return FlamegraphCollector(sample_interval_usec, skip_idle=skip_idle,
                                   mode=mode, opcodes=opcodes)
    if output_format == "diff_flamegraph":
        from .stack_collector import DiffFlamegraphCollector
        return DiffFlamegraphCollector(sample_interval_usec,
                                       baseline_binary_path=diff_baseline,
                                       skip_idle=skip_idle, mode=mode,
                                       capture_config=capture_config,
                                       opcodes=opcodes)
    if output_format == "gecko":
        from .gecko_collector import GeckoCollector
        return GeckoCollector(sample_interval_usec, skip_idle=skip_idle,
                              mode=mode, opcodes=opcodes)
    if output_format == "jsonl":
        from .jsonl_collector import JsonlCollector
        return JsonlCollector(sample_interval_usec, skip_idle=skip_idle, mode=mode)
    if output_format == "heatmap":
        from .heatmap_collector import HeatmapCollector
        return HeatmapCollector(sample_interval_usec, skip_idle=skip_idle)
    if output_format == "binary":
        from .binary_collector import BinaryCollector
        return BinaryCollector(output_file, sample_interval_usec,
                               skip_idle=skip_idle, compression=compression,
                               mode=mode, capture_config=capture_config)
    raise ValueError(f"unknown output format: {output_format}")


def _open_in_browser(path):  # pragma: no cover - needs a browser
    import webbrowser
    target = path
    if os.path.isdir(path):
        target = os.path.join(path, "index.html")
    webbrowser.open(f"file://{os.path.abspath(target)}")


def print_stack_dump(stack_frames, *, pid):
    """Print a snapshot taken by ``dump_stack``."""
    from .dump import format_stack_dump
    import _colorize
    print(format_stack_dump(stack_frames, pid,
                            colorize=_colorize.can_colorize(file=sys.stdout)))


# -- output

def _print_options(args):
    sort = getattr(args, "sort", None)
    limit = getattr(args, "limit", None)
    return {
        "sort": _SORT_MODES[sort] if sort is not None else constants.SORT_MODE_NSAMPLES,
        "limit": limit if limit is not None else _DEFAULT_LIMIT,
        "show_summary": not getattr(args, "no_summary", False),
    }


def _handle_output(collector, args, pid, mode):
    """Print or write the collected profile as *args* asks."""
    fmt = getattr(args, "format", "pstats")
    outfile = getattr(args, "outfile", None)
    if fmt == "pstats" and not outfile:
        collector.print_stats(mode=mode, **_print_options(args))
        return None
    if fmt == "binary":
        collector.export(outfile)
        return outfile
    filename = outfile or _generate_output_filename(fmt, pid)
    result = collector.export(filename)
    if (getattr(args, "browser", False) and fmt in _BROWSER_FORMATS
            and result is not False):
        _open_in_browser(filename)
    return filename


# -- children (--subprocesses)

def _build_child_profiler_args(args):
    """Command-line options that give a child's profiler the same settings."""
    child = ["-r", _format_rate(args.sample_interval_usec)]
    if getattr(args, "duration", None) is not None:
        child += ["-d", str(args.duration)]
    if getattr(args, "all_threads", False):
        child.append("-a")
    if getattr(args, "realtime_stats", False):
        child.append("--realtime-stats")
    if getattr(args, "native", False):
        child.append("--native")
    if not getattr(args, "gc", True):
        child.append("--no-gc")
    if getattr(args, "opcodes", False):
        child.append("--opcodes")
    if getattr(args, "async_aware", False):
        child.append("--async-aware")
        async_mode = getattr(args, "async_mode", None)
        if async_mode:
            child += ["--async-mode", async_mode]
    mode = getattr(args, "mode", "wall") or "wall"
    if mode != "wall":
        child += ["--mode", mode]
    fmt = getattr(args, "format", "pstats")
    if fmt == "diff_flamegraph":
        child += ["--diff-flamegraph", args.diff_baseline]
    else:
        child.append(f"--{fmt}")
    return child


def _build_output_pattern(args):
    """The output file name of a child's profile (``{pid}`` is filled in)."""
    outfile = getattr(args, "outfile", None)
    if outfile:
        root, ext = os.path.splitext(outfile)
        return f"{root}_{{pid}}{ext}"
    return _generate_output_filename(args.format, "{pid}")


def _setup_child_monitor(args, parent_pid):
    from ._child_monitor import ChildProcessMonitor
    monitor = ChildProcessMonitor(
        pid=parent_pid,
        cli_args=_build_child_profiler_args(args),
        output_pattern=_build_output_pattern(args),
    )
    return monitor.__enter__()


# -- the parser

def _add_sampling_options(parser):
    group = parser.add_argument_group("sampling options")
    group.add_argument("-r", "--sampling-rate", type=_parse_rate,
                       default=_DEFAULT_RATE_HZ, metavar="RATE",
                       help="samples per second, e.g. 1000, 250hz, 10khz "
                            "(default: 1khz)")
    group.add_argument("-d", "--duration", type=float, default=None,
                       metavar="SECONDS",
                       help="how long to sample (default: until the target ends)")
    group.add_argument("-a", "--all-threads", action="store_true",
                       help="sample every thread, not only the main one")
    group.add_argument("--realtime-stats", action="store_true",
                       help="show sampling statistics while running")
    group.add_argument("--native", action="store_true",
                       help='show "<native>" frames where non-Python code runs')
    group.add_argument("--no-gc", action="store_false", dest="gc",
                       help='do not show "<GC>" frames for garbage collection')
    group.add_argument("--opcodes", action="store_true",
                       help="record the bytecode instruction of every frame")
    group.add_argument("--async-aware", action="store_true",
                       help="reconstruct asyncio task stacks")
    group.add_argument("--async-mode", choices=["running", "all"], default=None,
                       help="with --async-aware: only the running task, or all "
                            "tasks (default: running)")
    group.add_argument("--subprocesses", action="store_true",
                       help="also profile Python child processes")
    group.add_argument("--blocking", action="store_true",
                       help="stop the target while each sample is read")


def _add_mode_option(parser):
    parser.add_argument("--mode", choices=list(_MODES), default="wall",
                        help="which threads to sample (default: wall)")


def _add_output_options(parser):
    formats = parser.add_argument_group("output format")
    exclusive = formats.add_mutually_exclusive_group()
    for name, help_text in (
        ("pstats", "pstats report (the default)"),
        ("collapsed", "collapsed stacks for flame graph tools"),
        ("flamegraph", "interactive HTML flame graph"),
        ("gecko", "Firefox Profiler (Gecko) JSON"),
        ("heatmap", "HTML pages with per-line sample counts"),
        ("jsonl", "JSON lines"),
        ("binary", "compact binary recording (convert later with 'replay')"),
    ):
        exclusive.add_argument(f"--{name}", action="store_const", const=name,
                               dest="format", help=help_text)
    exclusive.add_argument("--diff-flamegraph", metavar="BASELINE",
                           dest="diff_baseline", default=None,
                           help="flame graph compared with a --binary baseline")
    out = parser.add_argument_group("output options")
    out.add_argument("-o", "--outfile", default=None,
                     help="output file (or directory for --heatmap)")
    out.add_argument("--browser", action="store_true",
                     help="open HTML output in a browser")
    out.add_argument("--compression", choices=["auto", "zstd", "none"],
                     default="auto", help="compression of --binary output")
    pstats = parser.add_argument_group("pstats options")
    pstats.add_argument("--sort", choices=list(_SORT_MODES), default=None,
                        help="sort order (default: nsamples)")
    pstats.add_argument("-l", "--limit", type=int, default=None,
                        help=f"rows to show (default: {_DEFAULT_LIMIT})")
    pstats.add_argument("--no-summary", action="store_true",
                        help="omit the summary section")


def _build_parser():
    parser = argparse.ArgumentParser(
        prog="python -m profiling.sampling",
        description="Statistical profiler for Python processes.",
    )
    sub = parser.add_subparsers(dest="command", required=True,
                                metavar="{run,attach,dump,replay}")

    run = sub.add_parser("run", help="run a script or module under the profiler")
    run.add_argument("-m", "--module", action="store_true",
                     help="the target is a module name")
    run.add_argument("target", help="script path (or module name with -m)")
    run.add_argument("args", nargs=argparse.REMAINDER,
                     help="arguments for the target")
    run.add_argument("--live", action="store_true",
                     help="show a live view in the terminal")
    _add_sampling_options(run)
    _add_mode_option(run)
    _add_output_options(run)

    attach = sub.add_parser("attach", help="profile a running process")
    attach.add_argument("pid", type=int)
    attach.add_argument("--live", action="store_true",
                        help="show a live view in the terminal")
    _add_sampling_options(attach)
    _add_mode_option(attach)
    _add_output_options(attach)

    dump = sub.add_parser("dump", help="print one stack snapshot of a process")
    dump.add_argument("pid", type=int)
    dump.add_argument("-a", "--all-threads", action="store_true")
    dump.add_argument("--native", action="store_true")
    dump.add_argument("--no-gc", action="store_false", dest="gc")
    dump.add_argument("--opcodes", action="store_true")
    dump.add_argument("--blocking", action="store_true")
    dump.add_argument("--async-aware", action="store_true")
    dump.add_argument("--async-mode", choices=["running", "all"], default=None)

    replay = sub.add_parser("replay", help="convert a --binary recording")
    replay.add_argument("input_file")
    _add_output_options(replay)
    return parser


# -- validation

def _validate_async(args, parser):
    if getattr(args, "async_mode", None) and not args.async_aware:
        parser.error("--async-mode requires --async-aware")
    if not getattr(args, "async_aware", False):
        return
    bad = []
    if getattr(args, "native", False):
        bad.append("--native")
    if not getattr(args, "gc", True):
        bad.append("--no-gc")
    mode = getattr(args, "mode", "wall")
    if mode not in (None, "wall"):
        bad.append(f"--mode={mode}")
    if getattr(args, "all_threads", False):
        bad.append("--all-threads")
    if getattr(args, "format", None) == "binary":
        bad.append("--binary")
    if bad:
        verb = "is" if len(bad) == 1 else "are"
        parser.error(f"{', '.join(bad)} {verb} incompatible with --async-aware")


def _validate_args(args, parser):
    _validate_async(args, parser)
    fmt = getattr(args, "format", None) or "pstats"
    pstats_only = []
    if getattr(args, "sort", None) is not None:
        pstats_only.append("--sort")
    if getattr(args, "limit", None) is not None:
        pstats_only.append("--limit")
    if getattr(args, "no_summary", False):
        pstats_only.append("--no-summary")
    if getattr(args, "live", False):
        if pstats_only:
            parser.error(f"{', '.join(pstats_only)} cannot be used with --live")
        if getattr(args, "subprocesses", False):
            parser.error("--subprocesses is incompatible with --live")
    elif pstats_only and fmt != "pstats":
        parser.error(f"{', '.join(pstats_only)} only valid with --pstats")
    if getattr(args, "opcodes", False) and fmt not in _OPCODES_COMPATIBLE_FORMATS \
            and not getattr(args, "live", False):
        parser.error(f"--opcodes is not compatible with --{fmt}")


def _finish_args(args):
    if getattr(args, "diff_baseline", None) is not None:
        args.format = "diff_flamegraph"
    if getattr(args, "format", None) is None:
        args.format = "pstats"
    if hasattr(args, "sampling_rate"):
        args.sample_interval_usec = max(1, int(round(1_000_000 / args.sampling_rate)))
    if (getattr(args, "async_aware", False) and not args.async_mode
            and args.command != "dump"):
        args.async_mode = "running"


# -- running

def _capture_config(args):
    return {
        "all_threads": bool(getattr(args, "all_threads", False)),
        "native": bool(getattr(args, "native", False)),
        "gc": bool(getattr(args, "gc", True)),
        "opcodes": bool(getattr(args, "opcodes", False)),
        "blocking": bool(getattr(args, "blocking", False)),
    }


def _collector_for(args, pid, mode):
    output_file = None
    if args.format == "binary":
        output_file = args.outfile or _generate_output_filename("binary", pid)
        args.outfile = output_file
    skip_idle = mode != constants.PROFILING_MODE_WALL
    if getattr(args, "live", False):
        from .live_collector import LiveStatsCollector
        return LiveStatsCollector(args.sample_interval_usec, skip_idle=skip_idle)
    return _create_collector(
        args.format, args.sample_interval_usec, skip_idle,
        mode=mode, output_file=output_file, compression=args.compression,
        capture_config=_capture_config(args), diff_baseline=args.diff_baseline,
        opcodes=args.opcodes,
    )


def _profile(args, pid, mode):
    collector = _collector_for(args, pid, mode)
    sampler = sample_live if getattr(args, "live", False) else sample
    monitor = None
    if getattr(args, "subprocesses", False):
        monitor = _setup_child_monitor(args, pid)
    try:
        sampler(
            pid,
            collector,
            duration_sec=args.duration,
            all_threads=args.all_threads,
            realtime_stats=args.realtime_stats,
            mode=mode,
            async_aware=args.async_mode if args.async_aware else None,
            native=args.native,
            gc=args.gc,
            opcodes=args.opcodes,
            blocking=args.blocking,
        )
    finally:
        if monitor is not None:
            monitor.__exit__(None, None, None)
    if not getattr(args, "live", False):
        _handle_output(collector, args, pid, mode)
    if monitor is not None:
        monitor.wait_for_profilers()
    return collector


def _wait_for_ready_signal(sock, process=None, timeout=_READY_TIMEOUT):
    """Wait until the coordinator started by ``_run_with_sync`` says b"ready"."""
    sock.settimeout(timeout)
    conn = sock.accept()[0]
    with conn:
        data = b""
        while b"ready" not in data:
            chunk = conn.recv(1024)
            if not chunk:
                code = process.poll() if process is not None else None
                raise RuntimeError(
                    "the target exited before it was ready"
                    + (f" (exit code {code})" if code is not None else ""))
            data += chunk


def _run_with_sync(original_cmd, suppress_output=False):
    """Start *original_cmd* (``python script ...`` or ``python -m mod ...``)
    through the synchronising coordinator; return the process once the
    target is about to run."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", 0))
        sock.listen(1)
        port = sock.getsockname()[1]
        cmd = (sys.executable, "-m", "profiling.sampling._sync_coordinator",
               str(port), os.getcwd()) + tuple(original_cmd[1:])
        quiet = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL} \
            if suppress_output else {}
        process = subprocess.Popen(cmd, **quiet)
        try:
            _wait_for_ready_signal(sock, process)
        except BaseException:
            process.kill()
            process.wait()
            raise
    return process


def _stop_process(process):
    try:
        if process.poll() is not None:
            return
    except Exception:
        return
    process.terminate()
    try:
        process.wait(timeout=2.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def _handle_run(args):
    if args.module:
        import importlib.util
        # the target runs with the current directory first on its path
        sys.path.insert(0, os.getcwd())
        try:
            spec = importlib.util.find_spec(args.target)
        except (ImportError, ValueError):
            spec = None
        finally:
            try:
                sys.path.remove(os.getcwd())
            except ValueError:
                pass
        if spec is None:
            raise SamplingModuleNotFoundError(args.target)
        cmd = (sys.executable, "-m", args.target, *args.args)
    else:
        if not os.path.exists(args.target):
            raise SamplingScriptNotFoundError(args.target)
        cmd = (sys.executable, args.target, *args.args)
    process = _run_with_sync(cmd, suppress_output=False)
    mode = _parse_mode(args.mode)
    try:
        _profile(args, process.pid, mode)
    finally:
        _stop_process(process)


def _handle_attach(args):
    if not _is_process_running(args.pid):
        raise SamplingUnknownProcessError(args.pid)
    _profile(args, args.pid, _parse_mode(args.mode))


def _handle_dump(args):
    if not _is_process_running(args.pid):
        raise SamplingUnknownProcessError(args.pid)
    async_aware = (args.async_mode or "all") if args.async_aware else None
    mode = constants.PROFILING_MODE_WALL if async_aware else constants.PROFILING_MODE_ALL
    try:
        stack_frames = dump_stack(
            args.pid,
            all_threads=args.all_threads,
            async_aware=async_aware,
            native=args.native,
            gc=args.gc,
            opcodes=args.opcodes,
            blocking=args.blocking,
            mode=mode,
        )
    except ProcessLookupError:
        sys.exit(f"No stack dump collected - process {args.pid} exited "
                 f"before the snapshot was taken")
    print_stack_dump(stack_frames, pid=args.pid)


_MAGIC = (0x54414348).to_bytes(4, sys.byteorder)


def _replay_with_reader(args, reader):
    info = reader.get_info()
    options = {}
    if args.format == "diff_flamegraph":
        options["diff_baseline"] = args.diff_baseline
    collector = _create_collector(
        args.format,
        info["sample_interval_us"],
        skip_idle=False,
        mode=info.get("mode"),
        **options,
        capture_config=info.get("capture_config"),
    )
    reader.replay_samples(collector)
    apply_recorded_stats(collector, info)
    name = os.path.splitext(os.path.basename(getattr(args, "input_file", "replay")))[0]
    _handle_output(collector, args, name, info.get("mode"))
    return collector


def _handle_replay(args):
    try:
        with open(args.input_file, "rb") as f:
            head = f.read(4)
    except OSError as exc:
        sys.exit(f"Error: {exc}")
    if head != _MAGIC:
        sys.exit(f"Error: {args.input_file} is not a binary sampling profile "
                 f"(record one with --binary)")
    if args.format == "binary":
        sys.exit("Error: replay cannot write --binary output")
    try:
        with BinaryReader(args.input_file) as reader:
            _replay_with_reader(args, reader)
    except (OSError, ValueError) as exc:
        sys.exit(f"Error: {exc}")


def main(argv=None):
    parser = _build_parser()
    args = parser.parse_args(argv)
    _finish_args(args)
    if args.command == "dump":
        _validate_async(args, parser)
        return _handle_dump(args)
    if args.command == "replay":
        return _handle_replay(args)
    _validate_args(args, parser)
    if args.command == "run":
        return _handle_run(args)
    return _handle_attach(args)


if __name__ == "__main__":
    main()
