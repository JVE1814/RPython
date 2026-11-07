"""Internationalization and localization support (GNU gettext API).

Message catalogs are GNU .mo files found under
<localedir>/<language>/LC_MESSAGES/<domain>.mo.  The class-based API
(translation(), GNUTranslations, NullTranslations) is recommended; the
module-level functions use a global current domain.
"""

import operator
import os
import sys


__all__ = ['NullTranslations', 'GNUTranslations', 'Catalog',
           'bindtextdomain', 'find', 'translation', 'install',
           'textdomain', 'dgettext', 'dngettext', 'gettext',
           'ngettext', 'pgettext', 'dpgettext', 'npgettext',
           'dnpgettext'
           ]

_default_localedir = os.path.join(sys.base_prefix, 'share', 'locale')


# ---------------------------------------------------------------------------
# Plural-Forms expressions
#
# The C expression of a Plural-Forms header is parsed by a small recursive
# descent parser into nested closures; nothing is ever passed to eval().

_MAX_PLURAL_LENGTH = 1000
_MAX_PLURAL_DEPTH = 20


def _plural_tokens(plural):
    """Split a plural expression into tokens (numbers, 'n', operators)."""
    i, size = 0, len(plural)
    ops2 = ('==', '!=', '<=', '>=', '&&', '||')
    while i < size:
        c = plural[i]
        if c in ' \t':
            i += 1
            continue
        if c.isascii() and c.isdigit():
            j = i
            while j < size and plural[j].isascii() and plural[j].isdigit():
                j += 1
            if j < size and (plural[j].isalnum() or plural[j] in '._'):
                raise ValueError('invalid token in plural form: %s' % plural[i:j + 1])
            yield int(plural[i:j], 10)
            i = j
            continue
        if c == 'n':
            if i + 1 < size and (plural[i + 1].isalnum() or plural[i + 1] == '_'):
                raise ValueError('invalid token in plural form: %s' % plural[i:i + 2])
            yield 'n'
            i += 1
            continue
        if plural[i:i + 2] in ops2:
            yield plural[i:i + 2]
            i += 2
            continue
        if c in '<>!*/%+-?:()':
            yield c
            i += 1
            continue
        raise ValueError('invalid token in plural form: %s' % c)


def _c_div(a, b):
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b >= 0) else -q


def _c_mod(a, b):
    return a - b * _c_div(a, b)


_BINARY = {
    '||': (1, None), '&&': (2, None),
    '==': (3, lambda a, b: int(a == b)), '!=': (3, lambda a, b: int(a != b)),
    '<': (4, lambda a, b: int(a < b)), '>': (4, lambda a, b: int(a > b)),
    '<=': (4, lambda a, b: int(a <= b)), '>=': (4, lambda a, b: int(a >= b)),
    '+': (5, operator.add), '-': (5, operator.sub),
    '*': (6, operator.mul), '/': (6, _c_div), '%': (6, _c_mod),
}


class _PluralParser:
    def __init__(self, plural):
        self.tokens = list(_plural_tokens(plural))
        self.pos = 0
        self.depth = 0

    def peek(self):
        if self.pos < len(self.tokens):
            return self.tokens[self.pos]
        return None

    def take(self):
        tok = self.peek()
        self.pos += 1
        return tok

    def expect(self, tok):
        if self.take() != tok:
            raise ValueError('unbalanced parenthesis or conditional in plural form')

    def parse(self):
        expr = self.conditional()
        if self.pos != len(self.tokens):
            raise ValueError('unexpected token in plural form: %s' % (self.peek(),))
        return expr

    def conditional(self):
        cond = self.binary(1)
        if self.peek() == '?':
            self.take()
            if_true = self.conditional()
            self.expect(':')
            if_false = self.conditional()
            return lambda n: if_true(n) if cond(n) else if_false(n)
        return cond

    def binary(self, min_prec):
        left = self.unary()
        while True:
            op = self.peek()
            if not isinstance(op, str) or op not in _BINARY:
                return left
            prec, fn = _BINARY[op]
            if prec < min_prec:
                return left
            self.take()
            right = self.binary(prec + 1)
            left = self._combine(op, fn, left, right)

    @staticmethod
    def _combine(op, fn, left, right):
        if op == '||':
            return lambda n: int(bool(left(n)) or bool(right(n)))
        if op == '&&':
            return lambda n: int(bool(left(n)) and bool(right(n)))
        return lambda n: fn(left(n), right(n))

    def unary(self):
        tok = self.take()
        if tok == '!':
            operand = self.unary()
            return lambda n: int(not operand(n))
        if tok == 'n':
            return lambda n: n
        if isinstance(tok, int):
            return lambda n, v=tok: v
        if tok == '(':
            self.depth += 1
            if self.depth > _MAX_PLURAL_DEPTH:
                raise ValueError('plural form expression is too complex')
            expr = self.conditional()
            self.expect(')')
            self.depth -= 1
            return expr
        raise ValueError('unexpected token in plural form: %s' % (tok,))


def _as_int(n):
    """n as an int for a plural formula: numbers that are not ints are
    accepted with a DeprecationWarning, anything else is a TypeError."""
    try:
        round(n)
    except TypeError:
        raise TypeError('Plural value must be an integer, got %s' %
                        (n.__class__.__name__,)) from None
    return _as_int_lenient(n)


def _as_int_lenient(n):
    """Like _as_int, but any object is accepted (with a DeprecationWarning
    when it is not an int): used where n is only compared with 1."""
    try:
        return operator.index(n)
    except TypeError:
        pass
    import warnings
    # Point the warning at the first caller outside this module.
    frame = sys._getframe(1)
    stacklevel = 2
    while frame.f_back is not None and frame.f_globals.get('__name__') == __name__:
        stacklevel += 1
        frame = frame.f_back
    warnings.warn('Plural value must be an integer, got %s' %
                  (n.__class__.__name__,),
                  DeprecationWarning,
                  stacklevel)
    return n


def c2py(plural):
    """Return a function computing the plural form index for n from the
    C expression of a Plural-Forms header."""
    if len(plural) > _MAX_PLURAL_LENGTH:
        raise ValueError('plural form expression is too long')
    try:
        expr = _PluralParser(plural).parse()
    except RecursionError:
        raise ValueError('plural form expression is too complex')

    def func(n):
        n = _as_int(n)
        return int(expr(n))
    return func


# ---------------------------------------------------------------------------
# Locale name expansion

def _expand_lang(loc):
    import locale
    loc = locale.normalize(loc)
    COMPONENT_CODESET = 1 << 0
    COMPONENT_TERRITORY = 1 << 1
    COMPONENT_MODIFIER = 1 << 2
    # Locale name parts: language[_territory][.codeset][@modifier]
    mask = 0
    pos = loc.find('@')
    if pos >= 0:
        modifier = loc[pos:]
        loc = loc[:pos]
        mask |= COMPONENT_MODIFIER
    else:
        modifier = ''
    pos = loc.find('.')
    if pos >= 0:
        codeset = loc[pos:]
        loc = loc[:pos]
        mask |= COMPONENT_CODESET
    else:
        codeset = ''
    pos = loc.find('_')
    if pos >= 0:
        territory = loc[pos:]
        loc = loc[:pos]
        mask |= COMPONENT_TERRITORY
    else:
        territory = ''
    language = loc
    ret = []
    # All subsets of the present components, most specific first.
    for i in range(mask + 1):
        if not (i & ~mask):
            val = language
            if i & COMPONENT_TERRITORY:
                val += territory
            if i & COMPONENT_CODESET:
                val += codeset
            if i & COMPONENT_MODIFIER:
                val += modifier
            ret.append(val)
    ret.reverse()
    return ret


# ---------------------------------------------------------------------------
# Translation classes

class NullTranslations:
    def __init__(self, fp=None):
        self._info = {}
        self._charset = None
        self._fallback = None
        if fp is not None:
            self._parse(fp)

    def _parse(self, fp):
        pass

    def add_fallback(self, fallback):
        if self._fallback:
            self._fallback.add_fallback(fallback)
        else:
            self._fallback = fallback

    def gettext(self, message):
        if self._fallback:
            return self._fallback.gettext(message)
        return message

    def ngettext(self, msgid1, msgid2, n):
        if self._fallback:
            return self._fallback.ngettext(msgid1, msgid2, n)
        n = _as_int_lenient(n)
        if n == 1:
            return msgid1
        else:
            return msgid2

    def pgettext(self, context, message):
        if self._fallback:
            return self._fallback.pgettext(context, message)
        return message

    def npgettext(self, context, msgid1, msgid2, n):
        if self._fallback:
            return self._fallback.npgettext(context, msgid1, msgid2, n)
        n = _as_int_lenient(n)
        if n == 1:
            return msgid1
        else:
            return msgid2

    def info(self):
        return self._info

    def charset(self):
        return self._charset

    def install(self, names=None):
        import builtins
        builtins.__dict__['_'] = self.gettext
        if names is not None:
            allowed = {'gettext', 'ngettext', 'npgettext', 'pgettext'}
            for name in allowed & set(names):
                builtins.__dict__[name] = getattr(self, name)


class GNUTranslations(NullTranslations):
    # Magic number of .mo files
    LE_MAGIC = 0x950412de
    BE_MAGIC = 0xde120495

    # The encoding of a msgctxt and a msgid in a .mo file is
    # msgctxt + "\x04" + msgid (gettext version >= 0.15)
    CONTEXT = "%s\x04%s"

    # Acceptable .mo versions
    VERSIONS = (0, 1)

    def _get_versions(self, version):
        """Returns a tuple of major version, minor version"""
        return (version >> 16, version & 0xffff)

    def _parse(self, fp):
        """Read a GNU .mo file into the catalog."""
        from struct import unpack
        filename = getattr(fp, 'name', '')
        self._catalog = catalog = {}
        self.plural = lambda n: int(n != 1)  # germanic plural by default
        buf = fp.read()
        buflen = len(buf)
        magic = unpack('<I', buf[:4])[0]
        if magic == self.LE_MAGIC:
            version, msgcount, masteridx, transidx = unpack('<4I', buf[4:20])
            ii = '<II'
        elif magic == self.BE_MAGIC:
            version, msgcount, masteridx, transidx = unpack('>4I', buf[4:20])
            ii = '>II'
        else:
            raise OSError(0, 'Bad magic number', filename)

        major_version, minor_version = self._get_versions(version)

        if major_version not in self.VERSIONS:
            raise OSError(0, 'Bad version number ' + str(major_version), filename)

        # The string tables: msgcount (length, offset) pairs each.
        for i in range(0, msgcount):
            mlen, moff = unpack(ii, buf[masteridx:masteridx + 8])
            mend = moff + mlen
            tlen, toff = unpack(ii, buf[transidx:transidx + 8])
            tend = toff + tlen
            if mend < buflen and tend < buflen:
                msg = buf[moff:mend]
                tmsg = buf[toff:tend]
            else:
                raise OSError(0, 'File is corrupt', filename)
            # The empty msgid holds the metadata (RFC 822 style headers).
            if mlen == 0:
                lastk = None
                for b_item in tmsg.split(b'\n'):
                    item = b_item.decode().strip()
                    if not item:
                        continue
                    # Skip over comment lines:
                    if item.startswith('#-#-#-#-#') and item.endswith('#-#-#-#-#'):
                        continue
                    k = v = None
                    if ':' in item:
                        k, v = item.split(':', 1)
                        k = k.strip().lower()
                        v = v.strip()
                        self._info[k] = v
                        lastk = k
                    elif lastk:
                        self._info[lastk] += '\n' + item
                    if k == 'content-type':
                        self._charset = v.split('charset=')[1]
                    elif k == 'plural-forms':
                        v = v.split(';')
                        plural = v[1].split('plural=')[1]
                        self.plural = c2py(plural)
            # Plural entries hold msgid1\0msgid2 and msgstr[0]\0msgstr[1]...
            charset = self._charset or 'ascii'
            if b'\x00' in msg:
                msgid1, msgid2 = msg.split(b'\x00')
                tmsg = tmsg.split(b'\x00')
                msgid1 = str(msgid1, charset)
                for i, x in enumerate(tmsg):
                    catalog[(msgid1, i)] = str(x, charset)
            else:
                catalog[str(msg, charset)] = str(tmsg, charset)
            # advance to next entry in the seek tables
            masteridx += 8
            transidx += 8

    def gettext(self, message):
        missing = object()
        tmsg = self._catalog.get(message, missing)
        if tmsg is missing:
            tmsg = self._catalog.get((message, self.plural(1)), missing)
        if tmsg is not missing:
            return tmsg
        if self._fallback:
            return self._fallback.gettext(message)
        return message

    def ngettext(self, msgid1, msgid2, n):
        try:
            tmsg = self._catalog[(msgid1, self.plural(n))]
        except KeyError:
            if self._fallback:
                return self._fallback.ngettext(msgid1, msgid2, n)
            if n == 1:
                tmsg = msgid1
            else:
                tmsg = msgid2
        return tmsg

    def pgettext(self, context, message):
        ctxt_msg_id = self.CONTEXT % (context, message)
        missing = object()
        tmsg = self._catalog.get(ctxt_msg_id, missing)
        if tmsg is missing:
            tmsg = self._catalog.get((ctxt_msg_id, self.plural(1)), missing)
        if tmsg is not missing:
            return tmsg
        if self._fallback:
            return self._fallback.pgettext(context, message)
        return message

    def npgettext(self, context, msgid1, msgid2, n):
        ctxt_msg_id = self.CONTEXT % (context, msgid1)
        try:
            tmsg = self._catalog[ctxt_msg_id, self.plural(n)]
        except KeyError:
            if self._fallback:
                return self._fallback.npgettext(context, msgid1, msgid2, n)
            if n == 1:
                tmsg = msgid1
            else:
                tmsg = msgid2
        return tmsg



def find(domain, localedir=None, languages=None, all=False):
    """Return the path of the .mo file for domain (or all of them with
    all=True), searching the given or environment-derived languages."""
    if localedir is None:
        localedir = _default_localedir
    if languages is None:
        languages = []
        for envar in ('LANGUAGE', 'LC_ALL', 'LC_MESSAGES', 'LANG'):
            val = os.environ.get(envar)
            if val:
                languages = val.split(':')
                break
        if 'C' not in languages:
            languages.append('C')
    # Expand the languages, dropping duplicates and stopping at 'C'.
    nelangs = []
    for lang in languages:
        for nelang in _expand_lang(lang):
            if nelang not in nelangs:
                nelangs.append(nelang)
    if all:
        result = []
    else:
        result = None
    for lang in nelangs:
        if lang == 'C':
            break
        mofile = os.path.join(localedir, lang, 'LC_MESSAGES', '%s.mo' % domain)
        if os.path.exists(mofile):
            if all:
                result.append(mofile)
            else:
                return mofile
    return result


# Cache of parsed catalogs keyed by (class, absolute .mo path).
_translations = {}


def translation(domain, localedir=None, languages=None,
                class_=None, fallback=False):
    """Return a translations object for domain (an instance of class_,
    default GNUTranslations), chaining fallbacks for every catalog found.
    Raises FileNotFoundError when nothing is found unless fallback is
    true (then a NullTranslations is returned)."""
    if class_ is None:
        class_ = GNUTranslations
    mofiles = find(domain, localedir, languages, all=True)
    if not mofiles:
        if fallback:
            return NullTranslations()
        from errno import ENOENT
        raise FileNotFoundError(ENOENT,
                                'No translation file found for domain', domain)
    result = None
    for mofile in mofiles:
        key = (class_, os.path.abspath(mofile))
        t = _translations.get(key)
        if t is None:
            with open(mofile, 'rb') as fp:
                t = _translations.setdefault(key, class_(fp))
        # Each returned object is a shallow copy so that fallbacks added to
        # it do not affect the cached one.
        import copy
        t = copy.copy(t)
        if result is None:
            result = t
        else:
            result.add_fallback(t)
    return result


def install(domain, localedir=None, *, names=None):
    t = translation(domain, localedir, fallback=True)
    t.install(names)


# Locale directories bound to domains.
_localedirs = {}
# The current global domain (GNU gettext default: "messages").
_current_domain = 'messages'


def textdomain(domain=None):
    global _current_domain
    if domain is not None:
        _current_domain = domain
    return _current_domain


def bindtextdomain(domain, localedir=None):
    global _localedirs
    if localedir is not None:
        _localedirs[domain] = localedir
    return _localedirs.get(domain, _default_localedir)


def dgettext(domain, message):
    try:
        t = translation(domain, _localedirs.get(domain, None))
    except OSError:
        return message
    return t.gettext(message)


def dngettext(domain, msgid1, msgid2, n):
    try:
        t = translation(domain, _localedirs.get(domain, None))
    except OSError:
        n = _as_int_lenient(n)
        if n == 1:
            return msgid1
        else:
            return msgid2
    return t.ngettext(msgid1, msgid2, n)


def dpgettext(domain, context, message):
    try:
        t = translation(domain, _localedirs.get(domain, None))
    except OSError:
        return message
    return t.pgettext(context, message)


def dnpgettext(domain, context, msgid1, msgid2, n):
    try:
        t = translation(domain, _localedirs.get(domain, None))
    except OSError:
        n = _as_int_lenient(n)
        if n == 1:
            return msgid1
        else:
            return msgid2
    return t.npgettext(context, msgid1, msgid2, n)


def gettext(message):
    return dgettext(_current_domain, message)


def ngettext(msgid1, msgid2, n):
    return dngettext(_current_domain, msgid1, msgid2, n)


def pgettext(context, message):
    return dpgettext(_current_domain, context, message)


def npgettext(context, msgid1, msgid2, n):
    return dnpgettext(_current_domain, context, msgid1, msgid2, n)


# Older name of GNUTranslations construction via translation().
Catalog = translation
