"""python -m sysconfig: print the configuration, or (with
--generate-posix-vars) write the build-time configuration data files."""

import os
import re
import sys
from sysconfig import (
    _ALWAYS_STR,
    _PYTHON_BUILD,
    _get_sysconfigdata_name,
    get_config_h_filename,
    get_config_var,
    get_config_vars,
    get_default_scheme,
    get_makefile_filename,
    get_paths,
    get_platform,
    get_python_version,
    parse_config_h,
)

# Variables that the Makefile defines as PY_<name>; references to <name>
# fall back to them.
_RENAMED_VARIABLES = ('CFLAGS', 'LDFLAGS', 'CPPFLAGS')

_ASSIGNMENT = re.compile(r'([a-zA-Z][a-zA-Z0-9_]+)\s*=\s*(.*)')
_REFERENCE = re.compile(r'\$(?:\(([A-Za-z][A-Za-z0-9_]*)\)|\{([A-Za-z][A-Za-z0-9_]*)\})')


def _tokenize_value(text):
    """Split a Makefile value into parts: ('lit', str) and ('ref', name).
    Return (parts, has_stray_dollar)."""
    parts = []
    stray = False
    pos = 0
    literal = []
    while pos < len(text):
        char = text[pos]
        if char != '$':
            literal.append(char)
            pos += 1
            continue
        if text.startswith('$$', pos):
            literal.append('$')
            pos += 2
            continue
        match = _REFERENCE.match(text, pos)
        if match is None:
            literal.append('$')
            stray = True
            pos += 1
            continue
        if literal:
            parts.append(('lit', ''.join(literal)))
            literal = []
        parts.append(('ref', match.group(1) or match.group(2)))
        pos = match.end()
    if literal:
        parts.append(('lit', ''.join(literal)))
    return parts, stray


def _convert(value, name):
    if name in _ALWAYS_STR:
        return value
    try:
        return int(value)
    except ValueError:
        return value


def _parse_makefile(filename, vars=None, keep_unresolved=True):
    """Parse the variable assignments of a Makefile into a dict.

    References ``$(NAME)``/``${NAME}`` are expanded (from the file, then
    the environment; unknown names expand to '').  ``$$`` is a literal
    '$'.  Values that keep a stray '$' are kept as they are, or dropped if
    *keep_unresolved* is false.  Integer-looking values become ints.
    """
    if vars is None:
        vars = {}

    raw = {}
    with open(filename, encoding='utf-8', errors='surrogateescape') as f:
        for line in f:
            if line.startswith('#') or not line.strip():
                continue
            match = _ASSIGNMENT.match(line)
            if match:
                raw[match.group(1)] = match.group(2).strip()

    parsed = {name: _tokenize_value(value) for name, value in raw.items()}
    resolved = {}
    in_progress = set()

    def resolve(name):
        if name in resolved:
            return resolved[name]
        if name in in_progress:
            return ''
        in_progress.add(name)
        parts, stray = parsed[name]
        if stray and not keep_unresolved:
            result = None
        else:
            pieces = []
            for kind, data in parts:
                if kind == 'lit':
                    pieces.append(data)
                else:
                    pieces.append(lookup(data))
            result = ''.join(pieces)
        in_progress.discard(name)
        resolved[name] = result
        return result

    def lookup(ref):
        if ref in parsed:
            value = resolve(ref)
            return '' if value is None else value
        if ref in vars:
            return str(vars[ref])
        if ref in os.environ:
            return os.environ[ref]
        if ref in _RENAMED_VARIABLES and 'PY_' + ref in parsed:
            value = resolve('PY_' + ref)
            return '' if value is None else value
        return ''

    for name in parsed:
        value = resolve(name)
        if value is None:
            continue
        vars[name] = _convert(value.strip(), name)
        if name.startswith('PY_') and name[3:] in _RENAMED_VARIABLES:
            plain = name[3:]
            if plain not in parsed:
                vars[plain] = vars[name]
    return vars


def _get_pybuilddir():
    """The build directory of extension modules, relative to the source
    tree (as written to pybuilddir.txt)."""
    pybuilddir = f'build/lib.{get_platform()}-{get_python_version()}'
    if get_config_var('Py_DEBUG') == '1':
        pybuilddir += '-pydebug'
    return pybuilddir


def _get_json_data_name():
    """File name of the JSON dump of the configuration variables."""
    name = _get_sysconfigdata_name()
    assert name.startswith('_sysconfigdata')
    return name.replace('_sysconfigdata', '_sysconfig_vars') + '.json'


def _generate_posix_vars():
    """Write _sysconfigdata_*.py and _sysconfig_vars_*.json into the build
    directory (python -m sysconfig --generate-posix-vars)."""
    import json
    import pprint

    variables = {}
    makefile = get_makefile_filename()
    if os.path.isfile(makefile):
        _parse_makefile(makefile, variables)
    config_h = get_config_h_filename()
    if os.path.isfile(config_h):
        with open(config_h, encoding='utf-8') as f:
            parse_config_h(f, variables)
    if not variables:
        variables = dict(get_config_vars())

    pybuilddir = _get_pybuilddir()
    os.makedirs(pybuilddir, exist_ok=True)
    name = _get_sysconfigdata_name()
    destfile = os.path.join(pybuilddir, name + '.py')
    with open(destfile, 'w', encoding='utf-8') as f:
        f.write('# system configuration generated and used by'
                ' the sysconfig module\n')
        f.write('build_time_vars = ')
        f.write(pprint.pformat(variables, width=100))
        f.write('\n')
    jsonfile = os.path.join(pybuilddir, _get_json_data_name())
    with open(jsonfile, 'w', encoding='utf-8') as f:
        json.dump(dict(get_config_vars()), f, indent=2)
    with open('pybuilddir.txt', 'w', encoding='utf-8') as f:
        f.write(pybuilddir)


def _print_dict(title, data):
    for index, (key, value) in enumerate(sorted(data.items())):
        if index == 0:
            print(f'{title}: ')
        print(f'\t{key} = "{value}"')


def _main():
    """Display all information sysconfig contains."""
    if '--generate-posix-vars' in sys.argv:
        _generate_posix_vars()
        return
    print(f'Platform: "{get_platform()}"')
    print(f'Python version: "{get_python_version()}"')
    print(f'Current installation scheme: "{get_default_scheme()}"')
    print()
    _print_dict('Paths', get_paths())
    print()
    _print_dict('Variables', get_config_vars())


if __name__ == '__main__':
    _main()
