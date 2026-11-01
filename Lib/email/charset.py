"""Character set descriptions for MIME messages.

A Charset knows how text in a given character set should be encoded in
headers (RFC 2047) and bodies (Content-Transfer-Encoding), and which charset
is used on output.
"""

__all__ = [
    'Charset',
    'add_alias',
    'add_charset',
    'add_codec',
]

from functools import partial

import email.base64mime
import email.quoprimime

from email import errors
from email.encoders import encode_7or8bit


# Header / body encodings
QP = 1          # quoted-printable
BASE64 = 2      # base64
SHORTEST = 3    # the shorter of QP and base64 (headers only)

# Length of the RFC 2047 chrome "=?" "?q?" "?=" without the charset name.
RFC2047_CHROME_LEN = 7

DEFAULT_CHARSET = 'us-ascii'
UNKNOWN8BIT = 'unknown-8bit'
EMPTYSTRING = ''


# charset name -> (header encoding, body encoding, output charset)
CHARSETS = {
    'iso-8859-1': (QP, QP, None),
    'iso-8859-2': (QP, QP, None),
    'iso-8859-3': (QP, QP, None),
    'iso-8859-4': (QP, QP, None),
    'iso-8859-9': (QP, QP, None),
    'iso-8859-10': (QP, QP, None),
    'iso-8859-13': (QP, QP, None),
    'iso-8859-14': (QP, QP, None),
    'iso-8859-15': (QP, QP, None),
    'iso-8859-16': (QP, QP, None),
    'windows-1252': (QP, QP, None),
    'viscii': (QP, QP, None),
    'us-ascii': (None, None, None),
    'big5': (BASE64, BASE64, None),
    'gb2312': (BASE64, BASE64, None),
    'euc-jp': (BASE64, None, 'iso-2022-jp'),
    'shift_jis': (BASE64, None, 'iso-2022-jp'),
    'iso-2022-jp': (BASE64, None, None),
    'koi8-r': (BASE64, BASE64, None),
    'utf-8': (SHORTEST, BASE64, 'utf-8'),
}

# alias -> canonical MIME charset name
ALIASES = {
    'latin_1': 'iso-8859-1',
    'latin-1': 'iso-8859-1',
    'latin_2': 'iso-8859-2',
    'latin-2': 'iso-8859-2',
    'latin_3': 'iso-8859-3',
    'latin-3': 'iso-8859-3',
    'latin_4': 'iso-8859-4',
    'latin-4': 'iso-8859-4',
    'latin_5': 'iso-8859-9',
    'latin-5': 'iso-8859-9',
    'latin_6': 'iso-8859-10',
    'latin-6': 'iso-8859-10',
    'latin_7': 'iso-8859-13',
    'latin-7': 'iso-8859-13',
    'latin_8': 'iso-8859-14',
    'latin-8': 'iso-8859-14',
    'latin_9': 'iso-8859-15',
    'latin-9': 'iso-8859-15',
    'latin_10': 'iso-8859-16',
    'latin-10': 'iso-8859-16',
    'cp949': 'ks_c_5601-1987',
    'euc_jp': 'euc-jp',
    'euc_kr': 'euc-kr',
    'ascii': 'us-ascii',
}

# MIME charset name -> Python codec name (None means no conversion)
CODEC_MAP = {
    'us-ascii': None,
}


# Python codec names whose MIME name is spelled differently.
_CODEC_TO_MIME = {
    'ascii': 'us-ascii',
    'euc_jp': 'euc-jp',
    'euc_kr': 'euc-kr',
    'iso2022_jp': 'iso-2022-jp',
    'iso2022_kr': 'iso-2022-kr',
    'cp949': 'ks_c_5601-1987',
    'latin_1': 'iso-8859-1',
}


_SPELLINGS = {
    'eucjp': 'euc_jp', 'ujis': 'euc_jp',
    'euckr': 'euc_kr',
    'iso2022jp': 'iso2022_jp',
    'iso2022kr': 'iso2022_kr',
}


def _canonical_name(name):
    if name in ALIASES:
        return ALIASES[name]
    if name in CHARSETS or name in CODEC_MAP:
        return name
    import codecs
    try:
        codec = codecs.lookup(name).name
    except (LookupError, ValueError):
        # spellings of codecs that may be missing from this build
        codec = _SPELLINGS.get(name.replace('-', '').replace('_', ''))
        if codec is None:
            return name
    if codec in _CODEC_TO_MIME:
        return _CODEC_TO_MIME[codec]
    if codec.startswith('iso8859-'):
        return 'iso-' + codec[3:]
    if codec.startswith('cp125'):
        return 'windows-' + codec[2:]
    if codec == 'utf-8':
        return 'utf-8'
    return name


def add_charset(charset, header_enc=None, body_enc=None, output_charset=None):
    """Register (or replace) the properties of a character set.

    *header_enc* and *body_enc* are QP, BASE64, SHORTEST (headers only) or
    None; *output_charset* is the charset the text is converted to.
    """
    if body_enc == SHORTEST:
        raise ValueError('SHORTEST not allowed for body_enc')
    CHARSETS[charset] = (header_enc, body_enc, output_charset)


def add_alias(alias, canonical):
    """Make *alias* another name for the charset *canonical*."""
    ALIASES[alias] = canonical


def add_codec(charset, codecname):
    """Use the Python codec *codecname* for the charset *charset*."""
    CODEC_MAP[charset] = codecname


def _encode(string, codec):
    """Encode *string* with *codec*; unknown-8bit means the surrogate
    escaped original bytes."""
    if codec == UNKNOWN8BIT:
        return string.encode('ascii', 'surrogateescape')
    return string.encode(codec)


_STATEFUL_CODECS = ('iso-2022', 'iso2022', 'utf-16', 'utf_16', 'utf-32',
                    'utf_32', 'utf-7', 'utf_7', 'hz')


class Charset:
    """Encoding properties of a character set.

    Attributes: input_charset, header_encoding, body_encoding,
    output_charset, input_codec and output_codec (see the module tables).
    """

    def __init__(self, input_charset=DEFAULT_CHARSET):
        try:
            if isinstance(input_charset, str):
                input_charset.encode('ascii')
            else:
                input_charset = str(input_charset, 'ascii')
        except UnicodeError:
            raise errors.CharsetError(input_charset)
        name = _canonical_name(input_charset.lower())
        self.input_charset = name
        header_enc, body_enc, conv = CHARSETS.get(name, (SHORTEST, BASE64, None))
        self.header_encoding = header_enc
        self.body_encoding = body_enc
        if conv is None:
            conv = name
        self.output_charset = ALIASES.get(conv, conv)
        self.input_codec = CODEC_MAP.get(self.input_charset, self.input_charset)
        self.output_codec = CODEC_MAP.get(self.output_charset,
                                          self.output_charset)

    def __repr__(self):
        return self.input_charset.lower()

    __str__ = __repr__

    def __eq__(self, other):
        return str(self) == str(other).lower()

    def __hash__(self):
        return hash(str(self))

    def get_body_encoding(self):
        """The Content-Transfer-Encoding to use for bodies: the string
        'quoted-printable' or 'base64', or the encode_7or8bit function."""
        assert self.body_encoding != SHORTEST
        if self.body_encoding == QP:
            return 'quoted-printable'
        if self.body_encoding == BASE64:
            return 'base64'
        return encode_7or8bit

    def get_output_charset(self):
        return self.output_charset or self.input_charset

    def _encoder_module(self, header_bytes):
        if self.header_encoding == BASE64:
            return email.base64mime
        if self.header_encoding == QP:
            return email.quoprimime
        if self.header_encoding == SHORTEST:
            b_len = email.base64mime.header_length(header_bytes)
            q_len = email.quoprimime.header_length(header_bytes)
            return email.base64mime if b_len < q_len else email.quoprimime
        return None

    def header_encode(self, string):
        """Return *string* as one RFC 2047 encoded word (or unchanged if the
        charset has no header encoding)."""
        codec = self.output_codec or 'us-ascii'
        header_bytes = _encode(string, codec)
        module = self._encoder_module(header_bytes)
        if module is None:
            return string
        return module.header_encode(header_bytes, codec)

    def header_encode_lines(self, string, maxlengths):
        """Encode *string* as a list of encoded words, each fitting into the
        lengths produced by the iterator *maxlengths* (including the RFC 2047
        chrome).  A leading None means that not even one character fit on
        the first line."""
        codec = self.output_codec or 'us-ascii'
        module = self._encoder_module(_encode(string, codec))
        encode_word = partial(module.header_encode, charset=codec)
        out_charset = self.get_output_charset() or 'us-ascii'
        chrome = len(out_charset) + RFC2047_CHROME_LEN
        stateful = codec.lower().startswith(_STATEFUL_CODECS)

        words = []
        start = 0
        line_bytes = b''
        limit = next(maxlengths) - chrome
        for index, ch in enumerate(string):
            if stateful:
                candidate = _encode(string[start:index + 1], codec)
            else:
                candidate = line_bytes + _encode(ch, codec)
            if module.header_length(candidate) > limit:
                if not words and index == start:
                    words.append(None)
                else:
                    words.append(encode_word(_encode(string[start:index], codec)))
                start = index
                candidate = _encode(ch, codec)
                limit = next(maxlengths) - chrome
            line_bytes = candidate
        words.append(encode_word(_encode(string[start:], codec)))
        return words

    def body_encode(self, string):
        """Encode a body (str or bytes) with the body encoding; str input is
        first converted to the output charset."""
        if not string:
            return string
        if self.body_encoding is BASE64:
            if isinstance(string, str):
                string = string.encode(self.output_charset)
            return email.base64mime.body_encode(string)
        if self.body_encoding is QP:
            if isinstance(string, str):
                string = string.encode(self.output_charset)
            return email.quoprimime.body_encode(string.decode('latin1'))
        if isinstance(string, str):
            string = string.encode(self.output_charset).decode('ascii')
        return string
