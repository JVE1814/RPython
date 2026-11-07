"""Command-line front end of the sampling profiler (``python -m profiling.sampling``).

Subcommands: ``attach`` a running pid, ``run`` a script/module under the profiler,
``dump`` one stack snapshot.  Chooses an output collector from the requested format
and writes (or replays) a profile.
"""

import argparse
import os
import sys

from . import constants
from .errors import (
    SamplingModuleNotFoundError,
    SamplingScriptNotFoundError,
    SamplingUnknownProcessError,
)
from .sample import SampleProfiler, dump_stack, sample, _is_process_running

__all__ = (
    'FORMAT_EXTENSIONS',
    'main',
    '_parse_mode',
    '_create_collector',
    '_generate_output_filename',
    '_handle_output',
    '_replay_with_reader',
)

FORMAT_EXTENSIONS = {
    "pstats": "pstats",
    "collapsed": "txt",
    "flamegraph": "html",
    "diff_flamegraph": "html",
    "gecko": "json",
    "jsonl": "jsonl",
    "binary": "bin",
}

_MODE_NAMES = {
    constants.PROFILING_MODE_WALL: "wall",
    constants.PROFILING_MODE_CPU: "cpu",
    constants.PROFILING_MODE_GIL: "gil",
    constants.PROFILING_MODE_ALL: "all",
    constants.PROFILING_MODE_EXCEPTION: "exception",
}
_MODE_VALUES = {v: k for k, v in _MODE_NAMES.items()}

# formats that may be combined with --opcodes
_OPCODES_COMPATIBLE_FORMATS = {"pstats", "collapsed", "flamegraph", "gecko"}


def _parse_mode(value):
    """Turn a --mode argument (name or number) into a PROFILING_MODE_* value."""
    if isinstance(value, int):
        return value
    key = str(value).strip().lower()
    if key in _MODE_VALUES:
        return _MODE_VALUES[key]
    try:
        return int(key)
    except ValueError:
        raise argparse.ArgumentTypeError(f"unknown profiling mode: {value!r}")


def _generate_output_filename(output_format, pid):
    ext = FORMAT_EXTENSIONS.get(output_format, output_format)
    return f"{output_format}_{pid}.{ext}"


def _create_collector(output_format, sample_interval_usec, skip_idle=False,
                      mode=constants.PROFILING_MODE_WALL, output_file=None,
                      compression="none", capture_config=None):
    if output_format == "pstats":
        from .pstats_collector import PstatsCollector
        return PstatsCollector(sample_interval_usec, skip_idle=skip_idle)
    if output_format == "collapsed":
        from .stack_collector import CollapsedStackCollector
        return CollapsedStackCollector(sample_interval_usec, skip_idle=skip_idle)
    if output_format == "flamegraph":
        from .stack_collector import FlamegraphCollector
        return FlamegraphCollector(sample_interval_usec, skip_idle=skip_idle, mode=mode)
    if output_format == "diff_flamegraph":
        from .stack_collector import DiffFlamegraphCollector
        return DiffFlamegraphCollector(sample_interval_usec, skip_idle=skip_idle, mode=mode)
    if output_format == "gecko":
        from .gecko_collector import GeckoCollector
        return GeckoCollector(sample_interval_usec, skip_idle=skip_idle, mode=_MODE_NAMES.get(mode, mode))
    if output_format == "jsonl":
        from .jsonl_collector import JsonlCollector
        return JsonlCollector(sample_interval_usec, skip_idle=skip_idle, mode=_MODE_NAMES.get(mode, mode))
    if output_format == "binary":
        from .binary_collector import BinaryCollector
        return BinaryCollector(output_file, sample_interval_usec, compression=compression,
                               mode=mode, capture_config=capture_config, skip_idle=skip_idle)
    raise ValueError(f"unknown output format: {output_format}")


def _open_in_browser(path):  # pragma: no cover - needs a browser
    import webbrowser
    webbrowser.open(f"file://{os.path.abspath(path)}")


def print_stack_dump(pid, **kwargs):
    """Print a one-shot stack dump of *pid* (thin wrapper over sample.dump_stack)."""
    print(dump_stack(pid, **kwargs))


def _wait_for_ready_signal(sock, timeout=None):  # pragma: no cover - coordinator path
    """Wait for a launched target to report it is ready on *sock*."""
    if timeout is not None:
        sock.settimeout(timeout)
    return sock.recv(4096)


def _handle_output(collector, args, pid, mode):
    outfile = getattr(args, "outfile", None)
    if outfile is None:
        outfile = _generate_output_filename(getattr(args, "format", "pstats"), pid)
    collector.export(outfile)
    if getattr(args, "browser", False):
        if not os.path.isdir(outfile):
            _open_in_browser(outfile)
    return outfile


def _replay_with_reader(args, reader):
    collector = _create_collector(
        args.format,
        sample_interval_usec=reader.sample_interval_usec,
        mode=reader.mode or constants.PROFILING_MODE_WALL,
    )
    if hasattr(collector, "set_replay_stats"):
        collector.set_replay_stats(reader.replay_stats())
    reader.replay_samples(collector)
    collector.export(args.outfile)
    return collector


def _resolve_format(args):
    for fmt in ("jsonl", "collapsed", "flamegraph", "gecko", "binary", "pstats"):
        if getattr(args, fmt, False):
            return fmt
    return getattr(args, "format", None) or "pstats"


def _build_parser():
    parser = argparse.ArgumentParser(prog="profiling.sampling")
    sub = parser.add_subparsers(dest="command")

    def add_common(p):
        p.add_argument("--interval", type=int, default=1000)
        p.add_argument("--duration", "-d", type=float, default=10)
        p.add_argument("-o", "--outfile", dest="outfile", default=None)
        p.add_argument("-a", "--all-threads", action="store_true", dest="all_threads")
        p.add_argument("--mode", type=_parse_mode, default=constants.PROFILING_MODE_WALL)
        p.add_argument("--native", action="store_true")
        p.add_argument("--opcodes", action="store_true")
        p.add_argument("--async-aware", action="store_true", dest="async_aware")
        p.add_argument("--async-mode", dest="async_mode", default="running")
        p.add_argument("--pstats", action="store_true")
        p.add_argument("--collapsed", action="store_true")
        p.add_argument("--flamegraph", action="store_true")
        p.add_argument("--gecko", action="store_true")
        p.add_argument("--jsonl", action="store_true")
        p.add_argument("--binary", action="store_true")
        p.add_argument("--browser", action="store_true")

    attach = sub.add_parser("attach")
    attach.add_argument("pid", type=int)
    add_common(attach)

    run = sub.add_parser("run")
    add_common(run)
    run.add_argument("-m", "--module", dest="module")
    run.add_argument("script", nargs="?")
    run.add_argument("args", nargs=argparse.REMAINDER)

    dump = sub.add_parser("dump")
    dump.add_argument("pid", type=int)
    dump.add_argument("--async-aware", action="store_true", dest="async_aware")
    dump.add_argument("--async-mode", dest="async_mode", default="running")
    dump.add_argument("--mode", type=_parse_mode, default=constants.PROFILING_MODE_WALL)
    dump.add_argument("--blocking", action="store_true")
    return parser


def main(argv=None):
    parser = _build_parser()
    args = parser.parse_args(argv)
    if args.command is None:
        parser.error("a subcommand is required")

    if args.command == "dump":
        text = dump_stack(
            args.pid,
            async_aware=(args.async_mode if args.async_aware else None),
            mode=args.mode,
            blocking=getattr(args, "blocking", False),
        )
        print(text)
        return 0

    fmt = _resolve_format(args)
    if getattr(args, "opcodes", False) and fmt not in _OPCODES_COMPATIBLE_FORMATS:
        parser.error(f"--opcodes is not compatible with --{fmt}")
    args.format = fmt

    if args.command == "attach":
        if not _is_process_running(args.pid):
            raise SamplingUnknownProcessError(args.pid)
        collector = _create_collector(
            fmt, args.interval, mode=args.mode,
            output_file=getattr(args, "outfile", None) or _generate_output_filename(fmt, args.pid),
        )
        profiler = SampleProfiler(
            args.pid, args.interval, all_threads=args.all_threads, mode=args.mode,
            native=args.native,
        )
        profiler.sample(collector, duration_sec=args.duration)
        _handle_output(collector, args, pid=args.pid, mode=args.mode)
        return 0

    if args.command == "run":
        if args.module:
            import importlib.util
            if importlib.util.find_spec(args.module) is None:
                raise SamplingModuleNotFoundError(args.module)
        elif args.script:
            if not os.path.exists(args.script):
                raise SamplingScriptNotFoundError(args.script)
        else:
            parser.error("a script or -m module is required")
        # launching the target and attaching is environment-specific; left to the
        # coordinator script in a full run.
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
