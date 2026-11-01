"""Command line front end shared by profiling.tracing and profile.

    [-o output_file] [-s sort] [-m module | scriptfile] [arg ...]

Options are only recognised before the target; everything after the script
name (or the module name of -m) is handed to the profiled program.
"""

import os
import sys


def _usage(prog):
    return ("usage: %s [-o output_file_path] [-s sort] "
            "[-m module | scriptfile] [arg] ..." % prog)


def _fail(prog, message):
    print(_usage(prog), file=sys.stderr)
    print("%s: error: %s" % (prog, message), file=sys.stderr)
    sys.exit(2)


def _sort_choices():
    import pstats
    return sorted(pstats.Stats.sort_arg_dict_default)


def parse(argv, prog):
    """Return (outfile, sort, module, target, target_args)."""
    outfile = None
    sort = -1
    module = None
    index = 0

    def value_of(option, index):
        arg = argv[index]
        if len(arg) > 2 and not arg.startswith('--'):
            return arg[2:], index + 1
        if '=' in arg and arg.startswith('--'):
            return arg.split('=', 1)[1], index + 1
        if index + 1 >= len(argv):
            _fail(prog, "%s option requires 1 argument" % option)
        return argv[index + 1], index + 2

    while index < len(argv):
        arg = argv[index]
        if arg == '--':
            index += 1
            break
        if arg in ('-h', '--help'):
            print(_usage(prog))
            print()
            print("options:")
            print("  -h, --help            show this help message and exit")
            print("  -o, --outfile FILE    save the statistics to FILE")
            print("  -s, --sort KEY        sort order of the printed report")
            print("  -m MODULE             profile a library module")
            sys.exit(0)
        if arg.startswith('-o') or arg.startswith('--outfile'):
            outfile, index = value_of('-o', index)
            continue
        if arg.startswith('-s') or arg.startswith('--sort'):
            sort, index = value_of('-s', index)
            if sort not in _sort_choices():
                _fail(prog, "option -s: invalid choice: %r (choose from %s)"
                      % (sort, ", ".join(repr(c) for c in _sort_choices())))
            continue
        if arg.startswith('-m'):
            module, index = value_of('-m', index)
            break
        if arg.startswith('-') and arg != '-':
            _fail(prog, "no such option: %s" % arg)
        break
    rest = argv[index:]
    if module is not None:
        return outfile, sort, module, module, rest
    if not rest:
        return outfile, sort, None, None, []
    return outfile, sort, None, rest[0], rest[1:]


def main(runctx, prog, argv=None):
    """Run the command line.  `runctx(code, globals, locals, filename, sort)`
    profiles `code` and reports or saves the statistics."""
    import runpy
    if argv is None:
        argv = sys.argv[1:]
    outfile, sort, module, target, args = parse(argv, prog)
    if target is None:
        print(_usage(prog), file=sys.stderr)
        sys.exit(2)
    if outfile is not None:
        # the program may change directory; keep the file where it was asked
        outfile = os.path.abspath(outfile)
    if module is not None:
        sys.argv[:] = [module] + args
        code = "run_module(modname, run_name='__main__', alter_sys=True)"
        namespace = {'run_module': runpy.run_module, 'modname': module}
    else:
        script = target
        if not os.path.exists(script):
            print("%s: error: file not found: %s" % (prog, script), file=sys.stderr)
            sys.exit(1)
        sys.argv[:] = [script] + args
        sys.path.insert(0, os.path.dirname(os.path.abspath(script)))
        code = "run_path(script, run_name='__main__')"
        namespace = {'run_path': runpy.run_path, 'script': script}
    try:
        runctx(code, namespace, None, outfile, sort)
    except BrokenPipeError:
        sys.stdout = None
        sys.exit(1)
