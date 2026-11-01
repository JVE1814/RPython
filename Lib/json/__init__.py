"""Reading and writing JSON (RFC 8259, ECMA-404).

The four entry points mirror pickle/marshal:

* ``dumps(obj)`` / ``dump(obj, fp)`` turn Python data into JSON text,
* ``loads(text)`` / ``load(fp)`` turn JSON text into Python data.

Mapping between the two worlds (decoding direction in brackets)::

    dict, frozendict       <->  object
    list, tuple            <->  array    [list]
    str                    <->  string
    int, float             <->  number
    True / False / None    <->  true / false / null

    >>> import json
    >>> json.dumps({"id": 7, "tags": ("a", "b")}, sort_keys=True)
    '{"id": 7, "tags": ["a", "b"]}'
    >>> json.loads('[1.5, null, {"k": false}]')
    [1.5, None, {'k': False}]
    >>> print(json.dumps([1, {"x": [2]}], indent=2))
    [
      1,
      {
        "x": [
          2
        ]
      }
    ]

Customisation goes through ``JSONEncoder``/``JSONDecoder`` subclasses (the
``cls`` argument) or through the hook arguments (``default`` when encoding;
``object_hook``, ``object_pairs_hook``, ``array_hook``, ``parse_float``,
``parse_int`` and ``parse_constant`` when decoding).

``python -m json`` validates and pretty-prints JSON from the command line.
"""

__all__ = [
    'dump', 'dumps', 'load', 'loads',
    'JSONDecoder', 'JSONDecodeError', 'JSONEncoder',
]

from .decoder import JSONDecoder, JSONDecodeError
from .encoder import JSONEncoder
import codecs


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings.warn(
            "'__version__' is deprecated and slated for removal in "
            "Python 3.20",
            DeprecationWarning, stacklevel=2)
        return '2.0.9'
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


# --------------------------------------------------------------------------
# encoding

_ENCODER_DEFAULTS = dict(
    skipkeys=False, ensure_ascii=True, check_circular=True, allow_nan=True,
    indent=None, separators=None, default=None, sort_keys=False,
)

_shared_encoder = JSONEncoder()


def _pick_encoder(cls, options, extra):
    """The shared default encoder when nothing is customised, else a new
    instance of *cls* (JSONEncoder by default)."""
    if cls is None and not extra and options == _ENCODER_DEFAULTS:
        return _shared_encoder
    if cls is None:
        cls = JSONEncoder
    return cls(**options, **extra)


def dump(obj, fp, *, skipkeys=False, ensure_ascii=True, check_circular=True,
         allow_nan=True, cls=None, indent=None, separators=None,
         default=None, sort_keys=False, **kw):
    """Write *obj* as JSON to the text stream *fp* (anything with a
    ``write(str)`` method).

    The keyword arguments have the meaning documented for JSONEncoder;
    *cls* selects a JSONEncoder subclass and extra keyword arguments are
    passed to its constructor.
    """
    options = dict(skipkeys=skipkeys, ensure_ascii=ensure_ascii,
                   check_circular=check_circular, allow_nan=allow_nan,
                   indent=indent, separators=separators, default=default,
                   sort_keys=sort_keys)
    write = fp.write
    for piece in _pick_encoder(cls, options, kw).iterencode(obj):
        write(piece)


def dumps(obj, *, skipkeys=False, ensure_ascii=True, check_circular=True,
          allow_nan=True, cls=None, indent=None, separators=None,
          default=None, sort_keys=False, **kw):
    """Return *obj* serialised as a JSON ``str``.

    The keyword arguments are those of dump().
    """
    options = dict(skipkeys=skipkeys, ensure_ascii=ensure_ascii,
                   check_circular=check_circular, allow_nan=allow_nan,
                   indent=indent, separators=separators, default=default,
                   sort_keys=sort_keys)
    return _pick_encoder(cls, options, kw).encode(obj)


# --------------------------------------------------------------------------
# decoding

_shared_decoder = JSONDecoder()

_DECODER_HOOKS = ('object_hook', 'parse_float', 'parse_int',
                  'parse_constant', 'object_pairs_hook', 'array_hook')


def detect_encoding(b):
    """Guess the Unicode encoding of the JSON document in the bytes *b*.

    A byte order mark wins; otherwise the pattern of zero bytes among the
    first four bytes tells UTF-16/UTF-32 and their byte order apart (JSON
    text starts with an ASCII character).
    """
    head = bytes(b[:4])
    if head.startswith((codecs.BOM_UTF32_LE, codecs.BOM_UTF32_BE)):
        return 'utf-32'
    if head.startswith((codecs.BOM_UTF16_LE, codecs.BOM_UTF16_BE)):
        return 'utf-16'
    if head.startswith(codecs.BOM_UTF8):
        return 'utf-8-sig'
    zeros = tuple(byte == 0 for byte in head)
    if len(head) >= 4:
        if zeros[0]:
            # 00 xx: big endian; 00 00 00 xx: UTF-32
            return 'utf-32-be' if zeros[1] else 'utf-16-be'
        if zeros[1]:
            # xx 00 00 00: UTF-32 little endian, xx 00 yy ..: UTF-16
            return 'utf-32-le' if zeros[2] and zeros[3] else 'utf-16-le'
    elif len(head) == 2:
        if zeros[0]:
            return 'utf-16-be'
        if zeros[1]:
            return 'utf-16-le'
    return 'utf-8'


def load(fp, *, cls=None, object_hook=None, parse_float=None,
         parse_int=None, parse_constant=None, object_pairs_hook=None,
         array_hook=None, **kw):
    """Read a JSON document from the file-like *fp* (``fp.read()`` may
    return str or bytes) and return the Python value.

    Arguments are those of loads().
    """
    return loads(fp.read(), cls=cls, object_hook=object_hook,
                 parse_float=parse_float, parse_int=parse_int,
                 parse_constant=parse_constant,
                 object_pairs_hook=object_pairs_hook,
                 array_hook=array_hook, **kw)


def loads(s, *, cls=None, object_hook=None, parse_float=None,
          parse_int=None, parse_constant=None, object_pairs_hook=None,
          array_hook=None, **kw):
    """Parse the JSON document *s* (str, bytes or bytearray) into Python
    data.

    * object_hook(dict) replaces each decoded object;
    * object_pairs_hook(list of pairs) does the same from the ordered key/
      value pairs and takes precedence over object_hook;
    * array_hook(list) replaces each decoded array;
    * parse_float(str) / parse_int(str) build numbers (float / int by
      default);
    * parse_constant(str) is called for 'NaN', 'Infinity' and '-Infinity'.

    *cls* selects a JSONDecoder subclass; extra keyword arguments go to its
    constructor.  Bytes are decoded with the encoding detect_encoding()
    reports.
    """
    if isinstance(s, str):
        if s.startswith('﻿'):
            raise JSONDecodeError(
                "Unexpected UTF-8 BOM (decode using utf-8-sig)", s, 0)
    elif isinstance(s, (bytes, bytearray)):
        s = s.decode(detect_encoding(s), 'surrogatepass')
    else:
        raise TypeError(f'the JSON object must be str, bytes or bytearray, '
                        f'not {type(s).__name__}')

    hooks = dict(object_hook=object_hook, parse_float=parse_float,
                 parse_int=parse_int, parse_constant=parse_constant,
                 object_pairs_hook=object_pairs_hook, array_hook=array_hook)
    given = {name: value for name, value in hooks.items()
             if value is not None}
    if cls is None and not given and not kw:
        return _shared_decoder.decode(s)
    if cls is None:
        cls = JSONDecoder
    return cls(**given, **kw).decode(s)
