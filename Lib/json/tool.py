"""Validate and pretty-print JSON from the command line.

    $ echo '{"a": [1, 2]}' | python -m json
    {
        "a": [
            1,
            2
        ]
    }

Run ``python -m json -h`` for the options.
"""
import argparse
import errno
import json
import os
import re
import sys

from _colorize import can_colorize, get_theme

# one lexical token of already-serialised JSON; a string followed by ':' is
# an object key
_TOKEN = re.compile(r'''
    (?P<key>"(?:[^"\\]|\\.)*")(?=[ \t\n\r]*:)
  | (?P<string>"(?:[^"\\]|\\.)*")
  | (?P<number>NaN|-?Infinity|-?[0-9]+(?:\.[0-9]+)?(?:[eE][-+]?[0-9]+)?)
  | (?P<keyword>true|false|null)
''', re.VERBOSE)


def _paint(text, syntax):
    """Wrap the tokens of the JSON text in the theme's escape sequences."""
    palette = {
        'key': syntax.definition,
        'string': syntax.string,
        'number': syntax.number,
        'keyword': syntax.keyword,
    }

    def recolour(match):
        kind = match.lastgroup
        return f'{palette[kind]}{match.group()}{syntax.reset}'

    return _TOKEN.sub(recolour, text)


def _build_parser():
    parser = argparse.ArgumentParser(
        prog='python -m json',
        description='Validate JSON input and write it back pretty-printed.',
        color=True)
    parser.add_argument('infile', nargs='?', default=None,
                        help='file to read (default: standard input)')
    parser.add_argument('outfile', nargs='?', default=None,
                        help='file to write (default: standard output); '
                             'may be the same as infile')
    parser.add_argument('--sort-keys', action='store_true',
                        help='order object members by key')
    parser.add_argument('--no-ensure-ascii', dest='ensure_ascii',
                        action='store_false',
                        help='write non-ASCII characters as they are')
    parser.add_argument('--json-lines', action='store_true',
                        help='treat every input line as a separate document')
    layout = parser.add_mutually_exclusive_group()
    layout.add_argument('--indent', type=int, default=4,
                        help='spaces per nesting level (default: 4)')
    layout.add_argument('--tab', dest='indent', action='store_const',
                        const='\t', help='indent with one tab per level')
    layout.add_argument('--no-indent', dest='indent', action='store_const',
                        const=None, help='write each document on one line')
    layout.add_argument('--compact', action='store_true',
                        help='single line without optional whitespace')
    return parser


def _read_documents(text, json_lines):
    if json_lines:
        return [json.loads(line) for line in text.splitlines()
                if line.strip()]
    return [json.loads(text)]


def main():
    options = _build_parser().parse_args()
    dump_args = dict(sort_keys=options.sort_keys,
                     ensure_ascii=options.ensure_ascii,
                     indent=options.indent)
    if options.compact:
        dump_args.update(indent=None, separators=(',', ':'))

    try:
        if options.infile is None or options.infile == '-':
            text = sys.stdin.read()
        else:
            with open(options.infile, encoding='utf-8') as source:
                text = source.read()
        documents = _read_documents(text, options.json_lines)
    except (OSError, ValueError) as exc:
        raise SystemExit(exc)

    try:
        if options.outfile is None:
            target = sys.stdout
            syntax = None
            if can_colorize(file=target):
                syntax = get_theme(tty_file=target).syntax
            for document in documents:
                rendered = json.dumps(document, **dump_args)
                if syntax is not None:
                    rendered = _paint(rendered, syntax)
                target.write(rendered + '\n')
            target.flush()
        else:
            with open(options.outfile, 'w', encoding='utf-8') as target:
                for document in documents:
                    json.dump(document, target, **dump_args)
                    target.write("\n")
    except BrokenPipeError:
        raise
    except OSError as exc:
        raise SystemExit(exc)


if __name__ == '__main__':
    try:
        main()
    except BrokenPipeError as exc:
        # nobody reads our output any more: silence the flush at exit
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        sys.exit(exc.errno)
