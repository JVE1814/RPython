"""Parser for structured header values (RFC 5322, RFC 2045/2231, RFC 2047).

Header values are parsed into trees of tokens.  A TokenList is a list of
tokens; a Terminal is a str holding a piece of the source text.  Every
token has a token_type, defects, a str() that reproduces the source text
and a value that is the "semantic" content (whitespace runs collapsed,
comments dropped, encoded words decoded).

The get_xxx(value) functions each parse one grammar element at the start of
*value* and return (token, remaining_value), raising HeaderParseError if the
element is not there.  The parse_xxx(value) functions parse a complete
header value and never raise; they record defects instead.

Trees can be serialized again with fold(policy=...), which folds long lines
and RFC 2047/2231-encodes text that cannot be represented as is.
"""

import re
import sys
from string import hexdigits
from operator import itemgetter
from email import _encoded_words as _ew
from email import errors
from email import utils

#
# Character classes of the grammar
#

WSP = set(' \t')
CFWS_LEADER = WSP | set('(')
SPECIALS = set(r'()<>@,:;.\"[]')
ATOM_ENDS = SPECIALS | WSP
DOT_ATOM_ENDS = ATOM_ENDS - set('.')
# '.', '"' and '(' may appear in (obsolete) phrases
PHRASE_ENDS = SPECIALS - set('."(')
TSPECIALS = (SPECIALS | set('/?=')) - set('.')
TOKEN_ENDS = TSPECIALS | WSP
ASPECIALS = TSPECIALS | set("*'%")
ATTRIBUTE_ENDS = ASPECIALS | WSP
EXTENDED_ATTRIBUTE_ENDS = ATTRIBUTE_ENDS - set('%')
NLSET = {'\n', '\r'}
SPECIALSNL = SPECIALS | NLSET


def make_quoted_pairs(value):
    """Escape backslashes and double quotes with backslashes."""
    return str(value).replace('\\', '\\\\').replace('"', '\\"')


def make_parenthesis_pairs(value):
    """Escape backslashes and parentheses with backslashes."""
    return (str(value).replace('\\', '\\\\')
            .replace('(', '\\(').replace(')', '\\)'))


def quote_string(value):
    return '"' + make_quoted_pairs(value) + '"'


# An RFC 2047 encoded word anywhere in a string.
rfc2047_matcher = re.compile(r'=\?[^?]*\?[qQbB]\?.*?\?=', re.MULTILINE)


#
# Token classes
#

class TokenList(list):
    """A list of tokens; str() joins the source text of the elements."""

    token_type = None
    syntactic_break = True
    ew_combine_allowed = True

    def __init__(self, *args, **kw):
        super().__init__(*args, **kw)
        self.defects = []

    def __str__(self):
        return ''.join(str(x) for x in self)

    def __repr__(self):
        return '{}({})'.format(self.__class__.__name__, super().__repr__())

    @property
    def value(self):
        return ''.join(x.value for x in self if x.value)

    @property
    def all_defects(self):
        """The defects of this token and of all tokens below it."""
        result = list(self.defects)
        for token in self:
            result.extend(token.all_defects)
        return result

    def startswith_fws(self):
        return self[0].startswith_fws()

    @property
    def as_ew_allowed(self):
        """True if all elements may be represented as encoded words."""
        return all(part.as_ew_allowed for part in self)

    @property
    def comments(self):
        result = []
        for token in self:
            result.extend(token.comments)
        return result

    def fold(self, *, policy):
        return _refold_parse_tree(self, policy=policy)

    def pprint(self, indent=''):
        print(self.ppstr(indent=indent))

    def ppstr(self, indent=''):
        return '\n'.join(self._pp(indent=indent))

    def _pp(self, indent=''):
        yield '{}{}/{}('.format(indent, self.__class__.__name__,
                                self.token_type)
        for token in self:
            if hasattr(token, '_pp'):
                yield from token._pp(indent + '    ')
            else:
                yield (indent + '    !! invalid element in token list: '
                       '{!r}'.format(token))
        tail = ' Defects: {}'.format(self.defects) if self.defects else ''
        yield '{}){}'.format(indent, tail)


class WhiteSpaceTokenList(TokenList):

    @property
    def value(self):
        return ' '

    @property
    def comments(self):
        return [x.content for x in self if x.token_type == 'comment']


class UnstructuredTokenList(TokenList):
    token_type = 'unstructured'


class Phrase(TokenList):
    token_type = 'phrase'


class Word(TokenList):
    token_type = 'word'


class CFWSList(WhiteSpaceTokenList):
    token_type = 'cfws'


class Atom(TokenList):
    token_type = 'atom'


class Token(TokenList):
    token_type = 'token'
    encode_as_ew = False


class EncodedWord(TokenList):
    token_type = 'encoded-word'
    cte = None
    charset = None
    lang = None


class QuotedString(TokenList):
    token_type = 'quoted-string'

    def _bare(self):
        for token in self:
            if token.token_type == 'bare-quoted-string':
                return token
        return None

    @property
    def content(self):
        bare = self._bare()
        return bare.value if bare is not None else None

    @property
    def quoted_value(self):
        return ''.join(str(x) if x.token_type == 'bare-quoted-string'
                       else x.value for x in self)

    @property
    def stripped_value(self):
        bare = self._bare()
        return bare.value if bare is not None else None


class BareQuotedString(QuotedString):
    token_type = 'bare-quoted-string'

    def __str__(self):
        return quote_string(''.join(str(x) for x in self))

    @property
    def value(self):
        return ''.join(str(x) for x in self)


class Comment(WhiteSpaceTokenList):
    token_type = 'comment'

    def __str__(self):
        return '(' + ''.join(self.quote(x) for x in self) + ')'

    def quote(self, value):
        if value.token_type == 'comment':
            return str(value)
        return make_parenthesis_pairs(value)

    @property
    def content(self):
        return ''.join(str(x) for x in self)

    @property
    def comments(self):
        return [self.content]

    def startswith_fws(self):
        return False


class AddressList(TokenList):
    token_type = 'address-list'

    @property
    def addresses(self):
        return [x for x in self if x.token_type == 'address']

    @property
    def mailboxes(self):
        result = []
        for x in self.addresses:
            result.extend(x.mailboxes)
        return result

    @property
    def all_mailboxes(self):
        result = []
        for x in self.addresses:
            result.extend(x.all_mailboxes)
        return result


class Address(TokenList):
    token_type = 'address'

    @property
    def display_name(self):
        if self[0].token_type == 'group':
            return self[0].display_name

    @property
    def mailboxes(self):
        kind = self[0].token_type
        if kind == 'mailbox':
            return [self[0]]
        if kind == 'invalid-mailbox':
            return []
        return self[0].mailboxes

    @property
    def all_mailboxes(self):
        if self[0].token_type in ('mailbox', 'invalid-mailbox'):
            return [self[0]]
        return self[0].all_mailboxes


class MailboxList(TokenList):
    token_type = 'mailbox-list'

    @property
    def mailboxes(self):
        return [x for x in self if x.token_type == 'mailbox']

    @property
    def all_mailboxes(self):
        return [x for x in self
                if x.token_type in ('mailbox', 'invalid-mailbox')]


class GroupList(TokenList):
    token_type = 'group-list'

    @property
    def mailboxes(self):
        if not self or self[0].token_type != 'mailbox-list':
            return []
        return self[0].mailboxes

    @property
    def all_mailboxes(self):
        if not self or self[0].token_type != 'mailbox-list':
            return []
        return self[0].all_mailboxes


class Group(TokenList):
    token_type = "group"

    @property
    def mailboxes(self):
        if self[2].token_type != 'group-list':
            return []
        return self[2].mailboxes

    @property
    def all_mailboxes(self):
        if self[2].token_type != 'group-list':
            return []
        return self[2].all_mailboxes

    @property
    def display_name(self):
        return self[0].display_name


class NameAddr(TokenList):
    token_type = 'name-addr'

    @property
    def display_name(self):
        if len(self) == 1:
            return None
        return self[0].display_name

    @property
    def local_part(self):
        return self[-1].local_part

    @property
    def domain(self):
        return self[-1].domain

    @property
    def route(self):
        return self[-1].route

    @property
    def addr_spec(self):
        return self[-1].addr_spec


class AngleAddr(TokenList):
    token_type = 'angle-addr'

    def _find(self, kind):
        for x in self:
            if x.token_type == kind:
                return x
        return None

    @property
    def local_part(self):
        spec = self._find('addr-spec')
        return spec.local_part if spec is not None else None

    @property
    def domain(self):
        spec = self._find('addr-spec')
        return spec.domain if spec is not None else None

    @property
    def route(self):
        route = self._find('obs-route')
        return route.domains if route is not None else None

    @property
    def addr_spec(self):
        spec = self._find('addr-spec')
        if spec is None:
            return '<>'
        if spec.local_part:
            return spec.addr_spec
        return quote_string(spec.local_part) + spec.addr_spec


class ObsRoute(TokenList):
    token_type = 'obs-route'

    @property
    def domains(self):
        return [x.domain for x in self if x.token_type == 'domain']


class Mailbox(TokenList):
    token_type = 'mailbox'

    @property
    def display_name(self):
        if self[0].token_type == 'name-addr':
            return self[0].display_name

    @property
    def local_part(self):
        return self[0].local_part

    @property
    def domain(self):
        return self[0].domain

    @property
    def route(self):
        if self[0].token_type == 'name-addr':
            return self[0].route

    @property
    def addr_spec(self):
        return self[0].addr_spec


class InvalidMailbox(TokenList):
    token_type = 'invalid-mailbox'

    @property
    def display_name(self):
        return None

    local_part = domain = route = addr_spec = display_name


class Domain(TokenList):
    token_type = 'domain'
    as_ew_allowed = False

    @property
    def domain(self):
        return ''.join(super().value.split())


class DotAtom(TokenList):
    token_type = 'dot-atom'


class DotAtomText(TokenList):
    token_type = 'dot-atom-text'
    as_ew_allowed = True


class NoFoldLiteral(TokenList):
    token_type = 'no-fold-literal'
    as_ew_allowed = False


class AddrSpec(TokenList):
    token_type = 'addr-spec'
    as_ew_allowed = False

    @property
    def local_part(self):
        return self[0].local_part

    @property
    def domain(self):
        if len(self) < 3:
            return None
        return self[-1].domain

    @property
    def value(self):
        if len(self) < 3:
            return self[0].value
        return self[0].value.rstrip() + self[1].value + self[2].value.lstrip()

    @property
    def addr_spec(self):
        local = self.local_part
        chars = set(local)
        if chars & DOT_ATOM_ENDS:
            local = quote_string(local)
        if self.domain is not None:
            return local + '@' + self.domain
        return local


class ObsLocalPart(TokenList):
    token_type = 'obs-local-part'
    as_ew_allowed = False


class DisplayName(Phrase):
    token_type = 'display-name'
    ew_combine_allowed = False

    @property
    def display_name(self):
        res = TokenList(self)
        if len(res) == 0:
            return res.value
        if res[0].token_type == 'cfws':
            res.pop(0)
        elif isinstance(res[0], TokenList) and res[0][0].token_type == 'cfws':
            res[0] = TokenList(res[0][1:])
        if res[-1].token_type == 'cfws':
            res.pop()
        elif isinstance(res[-1], TokenList) and res[-1][-1].token_type == 'cfws':
            res[-1] = TokenList(res[-1][:-1])
        return res.value

    @property
    def value(self):
        needs_quotes = bool(self.defects) or any(
            x.token_type == 'quoted-string' for x in self)
        if len(self) == 0 or not needs_quotes:
            return super().value

        def has_edge_cfws(token, index):
            return (token.token_type == 'cfws'
                    or (isinstance(token, TokenList)
                        and token[index].token_type == 'cfws'))

        pre = ' ' if has_edge_cfws(self[0], 0) else ''
        post = ' ' if has_edge_cfws(self[-1], -1) else ''
        return pre + quote_string(self.display_name) + post


class LocalPart(TokenList):
    token_type = 'local-part'
    as_ew_allowed = False

    @property
    def value(self):
        if self[0].token_type == "quoted-string":
            return self[0].quoted_value
        return self[0].value

    @property
    def local_part(self):
        # Whitespace and comments vanish at both ends of the local part
        # and next to dots; elsewhere they keep their (folded) value.
        items = [t for t in self[0] if t.token_type != 'cfws']
        last = len(items) - 1
        kept = []
        for i, tok in enumerate(items):
            if isinstance(tok, TokenList):
                parts = list(tok)
                if (i == 0 or items[i - 1].token_type == 'dot') and \
                        parts and parts[0].token_type == 'cfws':
                    del parts[0]
                if (i == last or items[i + 1].token_type == 'dot') and \
                        parts and parts[-1].token_type == 'cfws':
                    del parts[-1]
                tok = TokenList(parts)
            kept.append(tok)
        return TokenList(kept).value


class DomainLiteral(TokenList):
    token_type = 'domain-literal'
    as_ew_allowed = False

    @property
    def domain(self):
        return ''.join(super().value.split())

    @property
    def ip(self):
        for x in self:
            if x.token_type == 'ptext':
                return x.value


class MIMEVersion(TokenList):
    token_type = 'mime-version'
    major = None
    minor = None


class Parameter(TokenList):
    token_type = 'parameter'
    sectioned = False
    extended = False
    charset = 'us-ascii'

    @property
    def section_number(self):
        # The section, if any, is the second element.
        return self[1].number if self.sectioned else 0

    @property
    def param_value(self):
        # Find the value (inside a quoted string for invalid quoted
        # extended values).
        for token in self:
            if token.token_type == 'value':
                return token.stripped_value
            if token.token_type == 'quoted-string':
                for inner in token:
                    if inner.token_type == 'bare-quoted-string':
                        for leaf in inner:
                            if leaf.token_type == 'value':
                                return leaf.stripped_value
        return ''


class InvalidParameter(Parameter):
    token_type = 'invalid-parameter'


class Attribute(TokenList):
    token_type = 'attribute'

    @property
    def stripped_value(self):
        for token in self:
            if token.token_type.endswith('attrtext'):
                return token.value


class Section(TokenList):
    token_type = 'section'
    number = None


class Value(TokenList):
    token_type = 'value'

    @property
    def stripped_value(self):
        token = self[0]
        if token.token_type == 'cfws':
            token = self[1]
        if token.token_type.endswith(
                ('quoted-string', 'attribute', 'extended-attribute')):
            return token.stripped_value
        return self.value


class MimeParameters(TokenList):
    token_type = 'mime-parameters'
    syntactic_break = False

    @property
    def params(self):
        """Yield (name, value) pairs with RFC 2231 continuations joined and
        decoded, in order of first appearance."""
        by_name = {}
        for token in self:
            if not token.token_type.endswith('parameter'):
                continue
            if token[0].token_type != 'attribute':
                continue
            name = token[0].value.strip()
            by_name.setdefault(name, []).append((token.section_number, token))
        for name, parts in by_name.items():
            parts = sorted(parts, key=itemgetter(0))
            first = parts[0][1]
            charset = first.charset
            if not first.extended and len(parts) > 1 and parts[1][0] == 0:
                parts[1][1].defects.append(errors.InvalidHeaderDefect(
                    'duplicate parameter name; duplicate(s) ignored'))
                parts = parts[:1]
            pieces = []
            expected = 0
            for number, param in parts:
                if number != expected:
                    if not param.extended:
                        param.defects.append(errors.InvalidHeaderDefect(
                            'duplicate parameter name; duplicate ignored'))
                        continue
                    param.defects.append(errors.InvalidHeaderDefect(
                        "inconsistent RFC2231 parameter numbering"))
                expected += 1
                text = param.param_value
                if param.extended:
                    try:
                        raw = utils._percent_decode_to_bytes(text)
                    except UnicodeEncodeError:
                        text = utils._percent_decode(text, "latin-1")
                    else:
                        try:
                            text = raw.decode(charset, 'surrogateescape')
                        except (LookupError, UnicodeEncodeError):
                            text = raw.decode('us-ascii', 'surrogateescape')
                        if utils._has_surrogates(text):
                            param.defects.append(errors.UndecodableBytesDefect())
                pieces.append(text)
            yield name, ''.join(pieces)

    def __str__(self):
        items = []
        for name, value in self.params:
            if value:
                items.append('{}={}'.format(name, quote_string(value)))
            else:
                items.append(name)
        text = '; '.join(items)
        return ' ' + text if text else ''


class ParameterizedHeaderValue(TokenList):
    # Folding of the parameters is done by _fold_mime_parameters.
    syntactic_break = False

    @property
    def params(self):
        for token in reversed(self):
            if token.token_type == 'mime-parameters':
                return token.params
        return {}


class ContentType(ParameterizedHeaderValue):
    token_type = 'content-type'
    as_ew_allowed = False
    maintype = 'text'
    subtype = 'plain'


class ContentDisposition(ParameterizedHeaderValue):
    token_type = 'content-disposition'
    as_ew_allowed = False
    content_disposition = None


class ContentTransferEncoding(TokenList):
    token_type = 'content-transfer-encoding'
    as_ew_allowed = False
    cte = '7bit'


class HeaderLabel(TokenList):
    token_type = 'header-label'
    as_ew_allowed = False


class MsgID(TokenList):
    token_type = 'msg-id'
    as_ew_allowed = False

    def fold(self, policy):
        # A msg-id is never folded.
        return str(self) + policy.linesep


class MessageID(MsgID):
    token_type = 'message-id'


class InvalidMessageID(MessageID):
    token_type = 'invalid-message-id'


class MessageIDList(TokenList):
    token_type = 'message-id-list'

    @property
    def message_ids(self):
        return [x for x in self if x.token_type == 'msg-id']


class Header(TokenList):
    token_type = 'header'


#
# Terminals
#

class Terminal(str):
    """A piece of source text."""

    as_ew_allowed = True
    ew_combine_allowed = True
    syntactic_break = True

    def __new__(cls, value, token_type):
        self = super().__new__(cls, value)
        self.token_type = token_type
        self.defects = []
        return self

    def __repr__(self):
        return "{}({})".format(self.__class__.__name__, super().__repr__())

    def pprint(self):
        print(self.__class__.__name__ + '/' + self.token_type)

    @property
    def all_defects(self):
        return list(self.defects)

    def _pp(self, indent=''):
        tail = ' {}'.format(self.defects) if self.defects else ''
        return ["{}{}/{}({}){}".format(indent, self.__class__.__name__,
                                       self.token_type, super().__repr__(),
                                       tail)]

    def pop_trailing_ws(self):
        return None

    @property
    def comments(self):
        return []

    def __getnewargs__(self):
        return (str(self), self.token_type)


class WhiteSpaceTerminal(Terminal):

    @property
    def value(self):
        return ' '

    def startswith_fws(self):
        return True


class ValueTerminal(Terminal):

    @property
    def value(self):
        return self

    def startswith_fws(self):
        return False


class EWWhiteSpaceTerminal(WhiteSpaceTerminal):
    """Whitespace between encoded words: not part of the value."""

    @property
    def value(self):
        return ''

    def __str__(self):
        return ''


class _InvalidEwError(errors.HeaderParseError):
    """An encoded word was found but could not be decoded."""


# Shared terminals.
DOT = ValueTerminal('.', 'dot')
ListSeparator = ValueTerminal(',', 'list-separator')
ListSeparator.as_ew_allowed = False
ListSeparator.syntactic_break = False
RouteComponentMarker = ValueTerminal('@', 'route-component-marker')


#
# Low level scanners
#

_wsp_splitter = re.compile(r'([{}]+)'.format(''.join(WSP))).split
_non_atom_end_matcher = re.compile(r"[^{}]+".format(
    re.escape(''.join(ATOM_ENDS)))).match
_non_printable_finder = re.compile(r"[\x00-\x20\x7F]").findall
_non_token_end_matcher = re.compile(r"[^{}]+".format(
    re.escape(''.join(TOKEN_ENDS)))).match
_non_attribute_end_matcher = re.compile(r"[^{}]+".format(
    re.escape(''.join(ATTRIBUTE_ENDS)))).match
_non_extended_attribute_end_matcher = re.compile(r"[^{}]+".format(
    re.escape(''.join(EXTENDED_ATTRIBUTE_ENDS)))).match


def _validate_xtext(xtext):
    """Record defects for control characters and undecodable bytes."""
    bad = _non_printable_finder(xtext)
    if bad:
        xtext.defects.append(errors.NonPrintableDefect(bad))
    if utils._has_surrogates(xtext):
        xtext.defects.append(errors.UndecodableBytesDefect(
            "Non-ASCII characters found in header token"))


def _get_ptext_to_endchars(value, endchars):
    """Scan text up to whitespace or an unescaped character of *endchars*.

    Backslash escapes are removed.  Returns (text, rest, had_qp) where
    had_qp tells whether an escaped backslash was seen.
    """
    fragment, *rest = _wsp_splitter(value, 1)
    chars = []
    escaped = False
    had_qp = False
    stop = len(fragment)
    for pos, ch in enumerate(fragment):
        if ch == '\\':
            if escaped:
                escaped = False
                had_qp = True
            else:
                escaped = True
                continue
        elif escaped:
            escaped = False
        elif ch in endchars:
            stop = pos
            break
        chars.append(ch)
    return ''.join(chars), ''.join([fragment[stop:]] + rest), had_qp


def _optional_cfws(value, target):
    # Parse leading CFWS (if any) into *target*; return the rest.
    if value and value[0] in CFWS_LEADER:
        token, value = get_cfws(value)
        target.append(token)
    return value


#
# RFC 5322 lexical tokens
#

def get_fws(value):
    """FWS: a run of whitespace (value is a single space)."""
    rest = value.lstrip()
    return WhiteSpaceTerminal(value[:len(value) - len(rest)], 'fws'), rest


def get_encoded_word(value, terminal_type='vtext'):
    """encoded-word = "=?" charset "?" encoding "?" encoded-text "?=" """
    ew = EncodedWord()
    if not value.startswith('=?'):
        raise errors.HeaderParseError(
            "expected encoded word but found {}".format(value))
    body, *rest = value[2:].split('?=', 1)
    if body == value[2:]:
        raise errors.HeaderParseError(
            "expected encoded word but found {}".format(value))
    tail = ''.join(rest)
    if (len(tail) > 1 and tail[0] in hexdigits and tail[1] in hexdigits
            and body.count('?') < 2):
        # The '?=' just found was really '?' followed by a '=XX' escape at
        # the start of the encoded text.
        more, *rest = tail.split('?=', 1)
        body = body + '?=' + more
    if len(body.split()) > 1:
        ew.defects.append(errors.InvalidHeaderDefect(
            "whitespace inside encoded word"))
    ew.cte = value
    value = ''.join(rest)
    try:
        text, charset, lang, defects = _ew.decode('=?' + body + '?=')
    except (ValueError, KeyError):
        raise _InvalidEwError(
            "encoded word format invalid: '{}'".format(ew.cte))
    ew.charset = charset
    ew.lang = lang
    ew.defects.extend(defects)
    while text:
        if text[0] in WSP:
            token, text = get_fws(text)
            ew.append(token)
            continue
        chunk, *rest_text = _wsp_splitter(text, 1)
        vtext = ValueTerminal(chunk, terminal_type)
        _validate_xtext(vtext)
        ew.append(vtext)
        text = ''.join(rest_text)
    if value and value[0] not in WSP:
        ew.defects.append(errors.InvalidHeaderDefect(
            "missing trailing whitespace after encoded-word"))
    return ew, value


def get_unstructured(value):
    """unstructured = (*([FWS] vchar) *WSP) / obs-unstruct

    Encoded words are decoded; whitespace between two encoded words is
    dropped from the value.  Never raises.
    """
    result = UnstructuredTokenList()
    while value:
        if value[0] in WSP:
            token, value = get_fws(value)
            result.append(token)
            continue
        valid_ew = True
        if value.startswith('=?'):
            try:
                token, value = get_encoded_word(value, 'utext')
            except _InvalidEwError:
                valid_ew = False
            except errors.HeaderParseError:
                pass
            else:
                separated = True
                if result and result[-1].token_type != 'fws':
                    result.defects.append(errors.InvalidHeaderDefect(
                        "missing whitespace before encoded word"))
                    separated = False
                if (separated and len(result) > 1
                        and result[-2].token_type == 'encoded-word'):
                    result[-1] = EWWhiteSpaceTerminal(result[-1], 'fws')
                result.append(token)
                continue
        chunk, *rest = _wsp_splitter(value, 1)
        if valid_ew and rfc2047_matcher.search(chunk):
            # An encoded word glued to other text: split before it.
            chunk, *rest = value.partition('=?')
        vtext = ValueTerminal(chunk, 'utext')
        _validate_xtext(vtext)
        result.append(vtext)
        value = ''.join(rest)
    return result


def _leads_with(value, chars):
    """True if value is not empty and starts with one of chars."""
    return bool(value) and value[0] in chars


def _leading_cfws(value):
    """(CFWSList or None, rest): the CFWS value starts with, if any."""
    if _leads_with(value, CFWS_LEADER):
        return get_cfws(value)
    return None, value


def _put_first(token, leader):
    """Insert leader (when there is one) at the start of token."""
    if leader is not None:
        token[:0] = [leader]


def _parse_error(what, value):
    return errors.HeaderParseError(
        "expected {} but found '{}'".format(what, value))


def get_qp_ctext(value):
    """ctext / quoted-pair inside a comment, up to whitespace or a
    parenthesis.  Its value is whitespace: comments carry no value."""
    text, value, _ = _get_ptext_to_endchars(value, '()')
    ptext = WhiteSpaceTerminal(text, 'ptext')
    _validate_xtext(ptext)
    return ptext, value


def get_qcontent(value):
    """qtext / quoted-pair inside a quoted string, up to whitespace or the
    closing quote."""
    text, value, _ = _get_ptext_to_endchars(value, '"')
    ptext = ValueTerminal(text, 'ptext')
    _validate_xtext(ptext)
    return ptext, value


def get_atext(value):
    """atext: the longest run of characters that may appear in an atom."""
    match = _non_atom_end_matcher(value)
    if match is None:
        raise _parse_error('atext', value)
    text = match.group()
    atext = ValueTerminal(text, 'atext')
    _validate_xtext(atext)
    return atext, value[len(text):]


def _quoted_word_pieces(value, target):
    """Parse the inside of a quoted string up to the closing quote (or the
    end) into target; return the rest, starting at the quote if found."""
    while value and value[0] != '"':
        if value[0] in WSP:
            piece, value = get_fws(value)
        elif value.startswith('=?'):
            try:
                piece, value = get_encoded_word(value)
            except errors.HeaderParseError:
                piece, value = get_qcontent(value)
            else:
                target.defects.append(errors.InvalidHeaderDefect(
                    "encoded word inside quoted string"))
                # whitespace between two encoded words is not text
                if (len(target) > 1 and target[-1].token_type == 'fws'
                        and target[-2].token_type == 'encoded-word'):
                    target[-1] = EWWhiteSpaceTerminal(target[-1], 'fws')
        else:
            piece, value = get_qcontent(value)
        target.append(piece)
    return value


def get_bare_quoted_string(value):
    """bare-quoted-string = DQUOTE *([FWS] qcontent) [FWS] DQUOTE

    A string that is not closed before the end of the value gets a
    defect.  Encoded words inside are decoded (with a defect).
    """
    if not _leads_with(value, '"'):
        raise errors.HeaderParseError(
            "expected '\"' but found '{}'".format(value))
    result = BareQuotedString()
    value = value[1:]
    if value.startswith('"'):
        # "": one empty qcontent
        piece, value = get_qcontent(value)
        result.append(piece)
    value = _quoted_word_pieces(value, result)
    if not value:
        result.defects.append(errors.InvalidHeaderDefect(
            "end of header inside quoted string"))
        return result, value
    return result, value[1:]


def get_comment(value):
    """comment = "(" *([FWS] ccontent) [FWS] ")"; comments nest."""
    if value and value[0] != '(':
        raise errors.HeaderParseError(
            "expected '(' but found '{}'".format(value))
    comment = Comment()
    value = value[1:]
    while value and value[0] != ')':
        if value[0] in WSP:
            piece, value = get_fws(value)
        elif value[0] == '(':
            piece, value = get_comment(value)
        else:
            piece, value = get_qp_ctext(value)
        comment.append(piece)
    if not value:
        comment.defects.append(errors.InvalidHeaderDefect(
            "end of header inside comment"))
        return comment, value
    return comment, value[1:]


def get_cfws(value):
    """CFWS = (1*([FWS] comment) [FWS]) / FWS"""
    cfws = CFWSList()
    while _leads_with(value, CFWS_LEADER):
        if value[0] in WSP:
            piece, value = get_fws(value)
        else:
            piece, value = get_comment(value)
        cfws.append(piece)
    return cfws, value


def get_quoted_string(value):
    """quoted-string = [CFWS] bare-quoted-string [CFWS]"""
    result = QuotedString()
    value = _optional_cfws(value, result)
    piece, value = get_bare_quoted_string(value)
    result.append(piece)
    value = _optional_cfws(value, result)
    return result, value


def get_atom(value):
    """atom = [CFWS] 1*atext [CFWS]; an encoded word may stand in for the
    atext."""
    atom = Atom()
    value = _optional_cfws(value, atom)
    if _leads_with(value, ATOM_ENDS):
        raise _parse_error('atom', value)
    core = None
    if value.startswith('=?'):
        try:
            core, value = get_encoded_word(value)
        except errors.HeaderParseError:
            core = None
    if core is None:
        core, value = get_atext(value)
    atom.append(core)
    value = _optional_cfws(value, atom)
    return atom, value


def get_dot_atom_text(value):
    """dot-atom-text = 1*atext *("." 1*atext)"""
    if not value or value[0] in ATOM_ENDS:
        raise errors.HeaderParseError(
            "expected atom at a start of dot-atom-text but found '{}'".format(
                value))
    text = DotAtomText()
    while value and value[0] not in ATOM_ENDS:
        piece, value = get_atext(value)
        text.append(piece)
        if value.startswith('.'):
            text.append(DOT)
            value = value[1:]
    if text[-1] is DOT:
        raise errors.HeaderParseError(
            "expected atom at end of dot-atom-text but found '{}'".format(
                '.' + value))
    return text, value


def get_dot_atom(value):
    """dot-atom = [CFWS] dot-atom-text [CFWS] (or an encoded word)"""
    dot_atom = DotAtom()
    if value[0] in CFWS_LEADER:
        value = _optional_cfws(value, dot_atom)
    core = None
    if value.startswith('=?'):
        try:
            core, value = get_encoded_word(value)
        except errors.HeaderParseError:
            core = None
    if core is None:
        core, value = get_dot_atom_text(value)
    dot_atom.append(core)
    value = _optional_cfws(value, dot_atom)
    return dot_atom, value


def get_word(value):
    """word = atom / quoted-string; leading CFWS goes into the word."""
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
    else:
        leader = None
    if not value:
        raise errors.HeaderParseError(
            "Expected 'atom' or 'quoted-string' but found nothing.")
    if value[0] == '"':
        word, value = get_quoted_string(value)
    elif value[0] in SPECIALS:
        raise errors.HeaderParseError(
            "Expected 'atom' or 'quoted-string' but found '{}'".format(value))
    else:
        word, value = get_atom(value)
    _put_first(word, leader)
    return word, value


def get_phrase(value):
    """phrase = 1*word / obs-phrase

    The obsolete form also allows periods and comments without words
    (each with a defect).
    """
    phrase = Phrase()
    try:
        word, value = get_word(value)
    except errors.HeaderParseError:
        phrase.defects.append(errors.InvalidHeaderDefect(
            "phrase does not start with word"))
    else:
        phrase.append(word)
    while value and value[0] not in PHRASE_ENDS:
        if value[0] == '.':
            phrase.append(DOT)
            phrase.defects.append(errors.ObsoleteHeaderDefect(
                "period in 'phrase'"))
            value = value[1:]
            continue
        try:
            word, value = get_word(value)
        except errors.HeaderParseError:
            if value[0] not in CFWS_LEADER:
                raise
            word, value = get_cfws(value)
            phrase.defects.append(errors.ObsoleteHeaderDefect(
                "comment found without atom"))
        _collapse_ew_gap(phrase, word)
        phrase.append(word)
    return phrase, value


def _starts_with_ew(token):
    """True if token's first piece is an encoded word (no CFWS before)."""
    if not isinstance(token, TokenList) or not token:
        return False
    return token[0].token_type == 'encoded-word'


def _collapse_ew_gap(phrase, word):
    """RFC 2047 6.2: whitespace between two encoded words is not part of
    the text.  If the last word of phrase ends with an encoded word and
    plain whitespace, and word starts with an encoded word, the whitespace
    stops counting."""
    if not phrase or not _starts_with_ew(word):
        return
    previous = phrase[-1]
    if not isinstance(previous, TokenList) or len(previous) < 2:
        return
    gap = previous[-1]
    if (gap.token_type == 'cfws' and not gap.comments
            and previous[-2].token_type == 'encoded-word'):
        previous[-1] = EWWhiteSpaceTerminal(str(gap), 'fws')


def get_local_part(value):
    """local-part = dot-atom / quoted-string / obs-local-part

    Non-ASCII local parts are accepted (RFC 6532); the policy decides when
    the header is written whether they can be serialized.
    """
    if not value:
        raise _parse_error('local-part', value)
    try:
        first, value = get_dot_atom(value)
    except errors.HeaderParseError:
        try:
            first, value = get_word(value)
        except errors.HeaderParseError:
            if value[0] != '\\' and value[0] in PHRASE_ENDS:
                raise
            first = TokenList()
    local_part = LocalPart()
    if not value or (value[0] != '\\' and value[0] in PHRASE_ENDS):
        local_part.append(first)
        return local_part, value
    # more than a dot-atom or quoted-string: reparse it all as the
    # obsolete form
    obsolete, value = get_obs_local_part(str(first) + value)
    if all(piece.token_type == 'cfws' for piece in obsolete):
        raise errors.HeaderParseError(
            "expected local-part but found only CFWS")
    if obsolete.token_type == 'invalid-obs-local-part':
        local_part.defects.append(errors.InvalidHeaderDefect(
            "local-part is not dot-atom, quoted-string, or obs-local-part"))
    else:
        local_part.defects.append(errors.ObsoleteHeaderDefect(
            "local-part is not a dot-atom (contains CFWS)"))
    local_part.append(obsolete)
    return local_part, value


def _is_dot_maybe_after_cfws(tokens, first, second):
    """True if tokens[first] is a dot, or is CFWS with a dot at
    tokens[second]."""
    if tokens[first].token_type == 'dot':
        return True
    return (tokens[first].token_type == 'cfws' and len(tokens) > 1
            and tokens[second].token_type == 'dot')


def get_obs_local_part(value):
    """obs-local-part = word *("." word)

    Repeated, leading and trailing periods, missing periods and stray
    backslashes are accepted with defects; with any defect the token type
    is 'invalid-obs-local-part'.
    """
    obsolete = ObsLocalPart()
    after_dot = False
    while value and (value[0] == '\\' or value[0] not in PHRASE_ENDS):
        head = value[0]
        if head == '.':
            if after_dot:
                obsolete.defects.append(errors.InvalidHeaderDefect(
                    "invalid repeated '.'"))
            obsolete.append(DOT)
            after_dot = True
            value = value[1:]
            continue
        if head == '\\':
            obsolete.append(ValueTerminal(head, 'misplaced-special'))
            value = value[1:]
            obsolete.defects.append(errors.InvalidHeaderDefect(
                "'\\' character outside of quoted-string/ccontent"))
            after_dot = False
            continue
        if obsolete and obsolete[-1].token_type != 'dot':
            obsolete.defects.append(errors.InvalidHeaderDefect(
                "missing '.' between words"))
        try:
            piece, value = get_word(value)
        except errors.HeaderParseError:
            if value[0] not in CFWS_LEADER:
                raise
            piece, value = get_cfws(value)
        else:
            after_dot = False
        obsolete.append(piece)
    if not obsolete:
        raise _parse_error('obs-local-part', value)
    if _is_dot_maybe_after_cfws(obsolete, 0, 1):
        obsolete.defects.append(errors.InvalidHeaderDefect(
            "Invalid leading '.' in local part"))
    if _is_dot_maybe_after_cfws(obsolete, -1, -2):
        obsolete.defects.append(errors.InvalidHeaderDefect(
            "Invalid trailing '.' in local part"))
    if obsolete.defects:
        obsolete.token_type = 'invalid-obs-local-part'
    return obsolete, value


def get_dtext(value):
    """dtext / quoted-pair (obsolete) up to whitespace or a bracket."""
    text, value, had_qp = _get_ptext_to_endchars(value, '[]')
    ptext = ValueTerminal(text, 'ptext')
    if had_qp:
        ptext.defects.append(errors.ObsoleteHeaderDefect(
            "quoted printable found in domain-literal"))
    _validate_xtext(ptext)
    return ptext, value


def _domain_literal_ended_early(value, domain_literal):
    """At the end of the value inside a domain literal: close it (with a
    defect) and return True."""
    if value:
        return False
    domain_literal.defects.append(errors.InvalidHeaderDefect(
        "end of input inside domain-literal"))
    domain_literal.append(ValueTerminal(']', 'domain-literal-end'))
    return True


def _optional_fws(value):
    if value[0] in WSP:
        return get_fws(value)
    return None, value


def get_domain_literal(value):
    """domain-literal = [CFWS] "[" *([FWS] dtext) [FWS] "]" [CFWS]"""
    literal = DomainLiteral()
    if value[0] in CFWS_LEADER:
        value = _optional_cfws(value, literal)
    if not value:
        raise errors.HeaderParseError("expected domain-literal")
    if value[0] != '[':
        raise errors.HeaderParseError(
            "expected '[' at start of domain-literal but found '{}'".format(
                value))
    literal.append(ValueTerminal('[', 'domain-literal-start'))
    value = value[1:]
    # [FWS] dtext [FWS]; the value may end after each part
    for parser in (_optional_fws, get_dtext, _optional_fws):
        if parser is _optional_fws and _domain_literal_ended_early(
                value, literal):
            return literal, value
        piece, value = parser(value)
        if piece is not None:
            literal.append(piece)
    if _domain_literal_ended_early(value, literal):
        return literal, value
    if value[0] != ']':
        raise errors.HeaderParseError(
            "expected ']' at end of domain-literal but found '{}'".format(
                value))
    literal.append(ValueTerminal(']', 'domain-literal-end'))
    value = _optional_cfws(value[1:], literal)
    return literal, value


def get_domain(value):
    """domain = dot-atom / domain-literal / obs-domain"""
    domain = Domain()
    leader, value = _leading_cfws(value)
    if not value:
        raise _parse_error('domain', value)
    if value[0] == '[':
        literal, value = get_domain_literal(value)
        _put_first(literal, leader)
        domain.append(literal)
        return domain, value
    try:
        first, value = get_dot_atom(value)
    except errors.HeaderParseError:
        first, value = get_atom(value)
    if value.startswith('@'):
        raise errors.HeaderParseError('Invalid Domain')
    _put_first(first, leader)
    domain.append(first)
    if value.startswith('.'):
        # obs-domain: atoms separated by periods, CFWS allowed
        domain.defects.append(errors.ObsoleteHeaderDefect(
            "domain is not a dot-atom (contains CFWS)"))
        if domain[0].token_type == 'dot-atom':
            domain[:] = domain[0]
        while value.startswith('.'):
            domain.append(DOT)
            atom, value = get_atom(value[1:])
            domain.append(atom)
    return domain, value


def get_addr_spec(value):
    """addr-spec = local-part "@" domain"""
    spec = AddrSpec()
    local_part, value = get_local_part(value)
    spec.append(local_part)
    if not value.startswith('@'):
        spec.defects.append(errors.InvalidHeaderDefect(
            "addr-spec local part with no domain"))
        return spec, value
    spec.append(ValueTerminal('@', 'address-at-symbol'))
    domain, value = get_domain(value[1:])
    spec.append(domain)
    return spec, value


def _route_domain(value, route):
    """'@' domain, appended to route; returns the rest."""
    route.append(RouteComponentMarker)
    domain, value = get_domain(value[1:])
    route.append(domain)
    return value


def get_obs_route(value):
    """obs-route = obs-domain-list ":"

    obs-domain-list = *(CFWS / ",") "@" domain
                      *("," [CFWS] ["@" domain])
    """
    route = ObsRoute()
    while value and (value[0] == ',' or value[0] in CFWS_LEADER):
        if value[0] == ',':
            route.append(ListSeparator)
            value = value[1:]
        else:
            value = _optional_cfws(value, route)
    if not value.startswith('@'):
        raise _parse_error('obs-route domain', value)
    value = _route_domain(value, route)
    while value.startswith(','):
        route.append(ListSeparator)
        value = _optional_cfws(value[1:], route)
        if value.startswith('@'):
            value = _route_domain(value, route)
    if not value:
        raise errors.HeaderParseError("end of header while parsing obs-route")
    if value[0] != ':':
        raise errors.HeaderParseError(
            "expected ':' marking end of obs-route but found '{}'".format(
                value))
    route.append(ValueTerminal(':', 'end-of-obs-route-marker'))
    return route, value[1:]


def get_angle_addr(value):
    """angle-addr = [CFWS] "<" addr-spec ">" [CFWS] / obs-angle-addr

    "<>" (used by SMTP) is accepted with a defect, as is a missing ">".
    """
    angle = AngleAddr()
    value = _optional_cfws(value, angle)
    if not value.startswith('<'):
        raise _parse_error('angle-addr', value)
    angle.append(ValueTerminal('<', 'angle-addr-start'))
    value = value[1:]
    if value.startswith('>'):
        angle.append(ValueTerminal('>', 'angle-addr-end'))
        angle.defects.append(errors.InvalidHeaderDefect(
            "null addr-spec in angle-addr"))
        return angle, value[1:]
    try:
        spec, value = get_addr_spec(value)
    except errors.HeaderParseError:
        try:
            route, value = get_obs_route(value)
        except errors.HeaderParseError:
            raise errors.HeaderParseError(
                "expected addr-spec or obs-route but found '{}'".format(value))
        angle.defects.append(errors.ObsoleteHeaderDefect(
            "obsolete route specification in angle-addr"))
        angle.append(route)
        spec, value = get_addr_spec(value)
    angle.append(spec)
    # the refolder reports a non-ASCII address here as a whole addr-spec
    spec._in_angle_addr = True
    if value.startswith('>'):
        value = value[1:]
    else:
        angle.defects.append(errors.InvalidHeaderDefect(
            "missing trailing '>' on angle-addr"))
    angle.append(ValueTerminal('>', 'angle-addr-end'))
    value = _optional_cfws(value, angle)
    return angle, value


def get_display_name(value):
    """display-name = phrase"""
    phrase, value = get_phrase(value)
    name = DisplayName()
    name.extend(phrase[:])
    name.defects = phrase.defects[:]
    return name, value


def get_name_addr(value):
    """name-addr = [display-name] angle-addr"""
    if not value:
        raise _parse_error('name-addr', value)
    result = NameAddr()
    # the display name and the angle-addr may both start with CFWS
    leader, value = _leading_cfws(value)
    if not value:
        raise _parse_error('name-addr', leader)
    if value[0] != '<':
        if value[0] in PHRASE_ENDS:
            raise _parse_error('name-addr', value)
        name, value = get_display_name(value)
        if not value:
            raise _parse_error('name-addr', name)
        if leader is not None:
            if isinstance(name[0], TokenList):
                name[0][:0] = [leader]
            else:
                name[:0] = [leader]
            leader = None
        result.append(name)
    angle, value = get_angle_addr(value)
    _put_first(angle, leader)
    result.append(angle)
    return result, value


def get_mailbox(value):
    """mailbox = name-addr / addr-spec"""
    # only trying tells the two forms apart
    mailbox = Mailbox()
    try:
        inner, value = get_name_addr(value)
    except errors.HeaderParseError:
        try:
            inner, value = get_addr_spec(value)
        except errors.HeaderParseError:
            raise _parse_error('mailbox', value)
    if any(isinstance(d, errors.InvalidHeaderDefect)
           for d in inner.all_defects):
        mailbox.token_type = 'invalid-mailbox'
    mailbox.append(inner)
    return mailbox, value


def get_invalid_mailbox(value, endchars):
    """Everything up to one of endchars, as an invalid mailbox (phrases
    and misplaced specials)."""
    junk = InvalidMailbox()
    while value and value[0] not in endchars:
        if value[0] in PHRASE_ENDS:
            junk.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            phrase, value = get_phrase(value)
            junk.append(phrase)
    return junk, value


def _list_entry_failed(value, container, endchars, wrap, cfws_only_message,
                       empty_message, invalid_message):
    """Handle a list entry that is not a valid mailbox/address: an empty
    entry (only CFWS, or nothing before the separator) or junk, kept as an
    invalid mailbox.  Returns the rest of the value."""
    leader, value = _leading_cfws(value)
    if leader is not None and (not value or value[0] in endchars):
        container.append(leader)
        container.defects.append(
            errors.ObsoleteHeaderDefect(cfws_only_message))
    elif leader is None and value[0] == ',':
        container.defects.append(errors.ObsoleteHeaderDefect(empty_message))
    else:
        junk, value = get_invalid_mailbox(value, endchars)
        _put_first(junk, leader)
        container.append(wrap(junk) if wrap else junk)
        container.defects.append(errors.InvalidHeaderDefect(invalid_message))
    return value


def get_mailbox_list(value):
    """mailbox-list = (mailbox *("," mailbox)) / obs-mbox-list

    Empty and invalid entries are kept with defects; parsing stops at ';'
    (the end of a group).
    """
    mlist = MailboxList()
    while value and value[0] != ';':
        try:
            mailbox, value = get_mailbox(value)
        except errors.HeaderParseError:
            value = _list_entry_failed(
                value, mlist, ',;', None, "empty element in mailbox-list",
                "empty element in mailbox-list",
                "invalid mailbox in mailbox-list")
        else:
            mlist.append(mailbox)
        if value and value[0] not in ',;':
            # junk after a mailbox: it becomes part of an invalid mailbox
            mailbox = mlist[-1]
            mailbox.token_type = 'invalid-mailbox'
            junk, value = get_invalid_mailbox(value, ',;')
            mailbox.extend(junk)
            mlist.defects.append(errors.InvalidHeaderDefect(
                "invalid mailbox in mailbox-list"))
        if value.startswith(','):
            mlist.append(ListSeparator)
            value = value[1:]
    return mlist, value


def get_group_list(value):
    """group-list = mailbox-list / CFWS / obs-group-list"""
    glist = GroupList()
    if not value:
        glist.defects.append(errors.InvalidHeaderDefect(
            "end of header before group-list"))
        return glist, value
    leader, value = _leading_cfws(value)
    if leader is not None:
        if not value:
            # cannot happen inside a group, where CFWS alone is allowed
            glist.defects.append(errors.InvalidHeaderDefect(
                "end of header in group-list"))
            glist.append(leader)
            return glist, value
        if value[0] == ';':
            glist.append(leader)
            return glist, value
    mlist, value = get_mailbox_list(value)
    if not mlist.all_mailboxes:
        if leader is not None:
            glist.append(leader)
        glist.extend(mlist)
        glist.defects.append(errors.ObsoleteHeaderDefect(
            "group-list with empty entries"))
        return glist, value
    _put_first(mlist, leader)
    glist.append(mlist)
    return glist, value


def get_group(value):
    """group = display-name ":" [group-list] ";" [CFWS]"""
    group = Group()
    name, value = get_display_name(value)
    if not value.startswith(':'):
        raise errors.HeaderParseError(
            "expected ':' at end of group display name but found '{}'".format(
                value))
    group.append(name)
    group.append(ValueTerminal(':', 'group-display-name-terminator'))
    value = value[1:]
    if value.startswith(';'):
        group.append(ValueTerminal(';', 'group-terminator'))
        return group, value[1:]
    glist, value = get_group_list(value)
    group.append(glist)
    if not value:
        group.defects.append(errors.InvalidHeaderDefect(
            "end of header in group"))
    elif value[0] != ';':
        raise errors.HeaderParseError(
            "expected ';' at end of group but found {}".format(value))
    group.append(ValueTerminal(';', 'group-terminator'))
    value = _optional_cfws(value[1:], group)
    return group, value


def get_address(value):
    """address = mailbox / group"""
    # A group and a mailbox look alike until ':', '<' or '@' shows up;
    # try the group first.
    address = Address()
    try:
        inner, value = get_group(value)
    except errors.HeaderParseError:
        try:
            inner, value = get_mailbox(value)
        except errors.HeaderParseError:
            raise _parse_error('address', value)
    address.append(inner)
    return address, value


def get_address_list(value):
    """address-list = (address *("," address)) / obs-addr-list

    Empty and invalid entries are kept with defects.
    """
    alist = AddressList()
    while value:
        try:
            address, value = get_address(value)
        except errors.HeaderParseError:
            value = _list_entry_failed(
                value, alist, ',', lambda junk: Address([junk]),
                "address-list entry with no content",
                "empty element in address-list",
                "invalid address in address-list")
        else:
            alist.append(address)
        if value and value[0] != ',':
            # junk after an address is an invalid address of its own
            junk, value = get_invalid_mailbox(value, ',')
            alist.append(Address([junk]))
            alist.defects.append(errors.InvalidHeaderDefect(
                "invalid address in address-list"))
        if value:
            # a ',' by now
            alist.append(ListSeparator)
            value = value[1:]
    return alist, value


def get_no_fold_literal(value):
    """no-fold-literal = "[" *dtext "]" """
    if not value:
        raise _parse_error('no-fold-literal', value)
    if value[0] != '[':
        raise errors.HeaderParseError(
            "expected '[' at the start of no-fold-literal "
            "but found '{}'".format(value))
    literal = NoFoldLiteral()
    literal.append(ValueTerminal('[', 'no-fold-literal-start'))
    text, value = get_dtext(value[1:])
    literal.append(text)
    if not value.startswith(']'):
        raise errors.HeaderParseError(
            "expected ']' at the end of no-fold-literal "
            "but found '{}'".format(value))
    literal.append(ValueTerminal(']', 'no-fold-literal-end'))
    return literal, value[1:]


def _id_left(value, msg_id):
    """id-left = dot-atom-text / obs-id-left (a local-part)"""
    try:
        return get_dot_atom_text(value)
    except errors.HeaderParseError:
        pass
    try:
        token, value = get_obs_local_part(value)
    except errors.HeaderParseError:
        raise errors.HeaderParseError(
            "expected dot-atom-text or obs-id-left"
            " but found '{}'".format(value))
    msg_id.defects.append(errors.ObsoleteHeaderDefect(
        "obsolete id-left in msg-id"))
    return token, value


def _id_right(value, msg_id):
    """id-right = dot-atom-text / no-fold-literal / obs-id-right (a
    domain)"""
    for getter in (get_dot_atom_text, get_no_fold_literal):
        try:
            return getter(value)
        except errors.HeaderParseError:
            pass
    try:
        token, value = get_domain(value)
    except errors.HeaderParseError:
        raise errors.HeaderParseError(
            "expected dot-atom-text, no-fold-literal or obs-id-right"
            " but found '{}'".format(value))
    msg_id.defects.append(errors.ObsoleteHeaderDefect(
        "obsolete id-right in msg-id"))
    return token, value


def get_msg_id(value):
    """msg-id = [CFWS] "<" id-left '@' id-right  ">" [CFWS]"""
    msg_id = MsgID()
    value = _optional_cfws(value, msg_id)
    if not value.startswith('<'):
        raise _parse_error('msg_id', value)
    msg_id.append(ValueTerminal('<', 'msg-id-start'))
    left, value = _id_left(value[1:], msg_id)
    msg_id.append(left)
    if not value.startswith('@'):
        msg_id.defects.append(errors.InvalidHeaderDefect(
            "msg-id with no id-right"))
        # still take a closing '>' that follows directly
        if value.startswith('>'):
            msg_id.append(ValueTerminal('>', 'msg-id-end'))
            value = value[1:]
        return msg_id, value
    msg_id.append(ValueTerminal('@', 'address-at-symbol'))
    right, value = _id_right(value[1:], msg_id)
    msg_id.append(right)
    if value.startswith('>'):
        value = value[1:]
    else:
        msg_id.defects.append(errors.InvalidHeaderDefect(
            "missing trailing '>' on msg-id"))
    msg_id.append(ValueTerminal('>', 'msg-id-end'))
    value = _optional_cfws(value, msg_id)
    return msg_id, value


def parse_message_id(value):
    """message-id = "Message-ID:" msg-id CRLF"""
    message_id = MessageID()
    try:
        token, value = get_msg_id(value)
        message_id.append(token)
    except errors.HeaderParseError as ex:
        token = get_unstructured(value)
        message_id = InvalidMessageID(token)
        message_id.defects.append(errors.InvalidHeaderDefect(
            "Invalid msg-id: {!r}".format(ex)))
    else:
        if value:
            message_id.defects.append(errors.InvalidHeaderDefect(
                "Unexpected {!r}".format(value)))
    return message_id


def parse_message_ids(value):
    """A list of msg-ids (References, In-Reply-To): 1*msg-id

    Commas between ids are tolerated (with a defect); text that is not a
    msg-id is kept as invalid text.
    """
    ids = MessageIDList()
    while value:
        try:
            token, value = get_msg_id(value)
            ids.append(token)
            continue
        except errors.HeaderParseError:
            pass
        if value[0] in CFWS_LEADER:
            token, value = get_cfws(value)
            ids.append(token)
        elif value[0] == ',':
            ids.append(WhiteSpaceTerminal(' ', 'fws'))
            ids.defects.append(errors.InvalidHeaderDefect(
                "comma used as msg-id separator"))
            value = value[1:]
        else:
            end = value.find('<', 1)
            if end < 0:
                end = len(value)
            junk = ValueTerminal(value[:end], 'invalid-msg-id')
            junk.defects.append(errors.InvalidHeaderDefect(
                "invalid msg-id: {!r}".format(value[:end])))
            ids.append(junk)
            value = value[end:]
    if not ids.message_ids and not ids.all_defects:
        ids.defects.append(errors.InvalidHeaderDefect("no msg-id found"))
    return ids


#
# MIME headers (RFC 2045, 2231)
#

def _scan(value, keep_going):
    # Split *value* after the longest prefix whose characters all satisfy
    # keep_going(ch).
    end = 0
    while end < len(value) and keep_going(value[end]):
        end += 1
    return value[:end], value[end:]


def _absorb_until_semicolon(value, container):
    # Move everything up to the next ';' into *container*: phrases, and
    # specials that cannot start one as 'misplaced-special' terminals.
    while value and value[0] != ';':
        if value[0] in PHRASE_ENDS:
            container.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            token, value = get_phrase(value)
            container.append(token)
    return value


def _mime_version_number(value, mv, which, stops):
    # One of the two numbers of a MIME-Version; *which* is 'major' or
    # 'minor'.  Whatever stands there is kept, flagged when not digits.
    text, value = _scan(value, lambda ch: ch not in stops)
    if text.isdigit():
        setattr(mv, which, int(text))
        mv.append(ValueTerminal(text, 'digits'))
    else:
        mv.defects.append(errors.InvalidHeaderDefect(
            "Expected MIME {} version number but found {!r}".format(which, text)))
        mv.append(ValueTerminal(text, 'xtext'))
    return value


def _incomplete_version(mv):
    if mv.major is not None:
        mv.defects.append(errors.InvalidHeaderDefect(
            "Incomplete MIME version; found only major number"))


def parse_mime_version(value):
    """mime-version = [CFWS] 1*digit [CFWS] "." [CFWS] 1*digit [CFWS]"""
    mv = MIMEVersion()
    if not value:
        mv.defects.append(errors.HeaderMissingRequiredValue(
            "Missing MIME version number (eg: 1.0)"))
        return mv
    if value[0] in CFWS_LEADER:
        token, value = get_cfws(value)
        mv.append(token)
        if not value:
            mv.defects.append(errors.HeaderMissingRequiredValue(
                "Expected MIME version number but found only CFWS"))
    value = _mime_version_number(value, mv, 'major', CFWS_LEADER | {'.'})
    value = _optional_cfws(value, mv)
    if value[:1] != '.':
        _incomplete_version(mv)
        if value:
            mv.append(ValueTerminal(value, 'xtext'))
        return mv
    mv.append(ValueTerminal('.', 'version-separator'))
    value = _optional_cfws(value[1:], mv)
    if not value:
        _incomplete_version(mv)
        return mv
    value = _mime_version_number(value, mv, 'minor', CFWS_LEADER)
    value = _optional_cfws(value, mv)
    if value:
        mv.defects.append(errors.InvalidHeaderDefect(
            "Excess non-CFWS text after MIME version"))
        mv.append(ValueTerminal(value, 'xtext'))
    return mv


def get_invalid_parameter(value):
    """Everything up to the next ';', as an invalid parameter."""
    invalid = InvalidParameter()
    value = _absorb_until_semicolon(value, invalid)
    return invalid, value


def _get_run(value, matcher, kind):
    m = matcher(value)
    if not m:
        raise errors.HeaderParseError(
            "expected {} but found '{}'".format(kind, value))
    text = m.group()
    terminal = ValueTerminal(text, kind)
    _validate_xtext(terminal)
    return terminal, value[len(text):]


def get_ttext(value):
    """ttext: characters that are not tspecials or whitespace."""
    return _get_run(value, _non_token_end_matcher, 'ttext')


def get_attrtext(value):
    """attrtext: token characters except '*', "'" and '%'."""
    return _get_run(value, _non_attribute_end_matcher, 'attrtext')


def get_extended_attrtext(value):
    """attrtext plus '%' (for RFC 2231 encoded values)."""
    return _get_run(value, _non_extended_attribute_end_matcher,
                    'extended-attrtext')


def _get_cfws_wrapped(value, container, ends, getter):
    value = _optional_cfws(value, container)
    if value and value[0] in ends:
        raise errors.HeaderParseError(
            "expected token but found '{}'".format(value))
    token, value = getter(value)
    container.append(token)
    value = _optional_cfws(value, container)
    return container, value


def get_token(value):
    """token = [CFWS] 1*ttext [CFWS]"""
    return _get_cfws_wrapped(value, Token(), TOKEN_ENDS, get_ttext)


def get_attribute(value):
    """attribute = [CFWS] 1*attrtext [CFWS]"""
    return _get_cfws_wrapped(value, Attribute(), ATTRIBUTE_ENDS, get_attrtext)


def get_extended_attribute(value):
    """[CFWS] 1*extended_attrtext [CFWS]"""
    return _get_cfws_wrapped(value, Attribute(), EXTENDED_ATTRIBUTE_ENDS,
                             get_extended_attrtext)


def get_section(value):
    """section = "*" digits   (RFC 2231 continuation number)"""
    if value[:1] != '*':
        raise errors.HeaderParseError(
            "Expected section but found {}".format(value))
    value = value[1:]
    digits, rest = _scan(value, str.isdigit)
    if not digits:
        raise errors.HeaderParseError(
            "Expected section number but found {}".format(value))
    section = Section()
    section.append(ValueTerminal('*', 'section-marker'))
    if len(digits) > 1 and digits[0] == '0':
        section.defects.append(errors.InvalidHeaderDefect(
            "section number has an invalid leading 0"))
    section.number = int(digits)
    section.append(ValueTerminal(digits, 'digits'))
    return section, rest


def get_value(value):
    """value = token / quoted-string   (with optional leading CFWS)"""
    if not value:
        raise errors.HeaderParseError("Expected value but found end of string")
    leader = None
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
        if not value:
            raise errors.HeaderParseError(
                "Expected value but found only {}".format(leader))
    if value[0] == '"':
        token, value = get_quoted_string(value)
    else:
        token, value = get_extended_attribute(value)
    if leader is not None:
        token[:0] = [leader]
    wrapper = Value()
    wrapper.append(token)
    return wrapper, value


def _parameter_markers(value, param):
    # After the attribute: an optional "*N" section and an optional "*"
    # marking an RFC 2231 extended (charset'lang'-prefixed) value.
    if value[0] != '*':
        return value
    try:
        section, value = get_section(value)
    except errors.HeaderParseError:
        pass
    else:
        param.sectioned = True
        param.append(section)
    if value[:1] == '*':
        param.append(ValueTerminal('*', 'extended-parameter-marker'))
        param.extended = True
        value = value[1:]
    if not value:
        raise errors.HeaderParseError("Incomplete parameter")
    return value


def _looks_encoded(text, section_number):
    # Could *text* (the inside of a quoted string) be an extended value?
    # An initial section must start with "charset'"; a later one must be
    # nothing but extended attrtext.
    try:
        if section_number == 0:
            if text[:1] == "'":
                return True
            _, rest = get_attrtext(text)
            return rest[:1] == "'"
        _, rest = get_extended_attrtext(text)
        return not rest
    except errors.HeaderParseError:
        return False


def _open_quoted_extended(value, param):
    # An extended value given as a quoted string is invalid but occurs in
    # the wild.  When its contents look encoded, they are parsed as if
    # unquoted, into the (emptied) bare-quoted-string of the parse tree.
    # Returns (container, text to parse, text after the quoted string).
    qstring, after = get_quoted_string(value)
    inner = qstring.stripped_value
    if not _looks_encoded(inner, param.section_number):
        param.defects.append(errors.InvalidHeaderDefect(
            "Parameter marked as extended but appears to have a "
            "quoted string value that is non-encoded"))
        return param, value, None
    param.defects.append(errors.InvalidHeaderDefect(
        "Quoted string value for extended parameter is invalid"))
    param.append(qstring)
    container = param
    for part in qstring:
        if part.token_type == 'bare-quoted-string':
            part[:] = []
            container = part
            break
    return container, inner, after


def _rfc2231_prefix(value, first, container, param):
    # charset "'" [language] "'" : *first* is what was parsed before the
    # first quote (the charset), *value* starts at that quote.
    if first is not None:
        charset = first[0]
        container.append(charset)
        param.charset = charset.value
    if value[0] != "'":
        raise errors.HeaderParseError("Expected RFC2231 char/lang encoding "
                                      "delimiter, but found {!r}".format(value))
    container.append(ValueTerminal("'", 'RFC2231-delimiter'))
    value = value[1:]
    if value and value[0] != "'":
        lang, value = get_attrtext(value)
        container.append(lang)
        param.lang = lang.value
        if value[:1] != "'":
            raise errors.HeaderParseError("Expected RFC2231 char/lang "
                                          "encoding delimiter, but found {}".format(value))
    # A missing closing quote at the very end is let through.
    container.append(ValueTerminal("'", 'RFC2231-delimiter'))
    return value[1:]


def _raw_quoted_contents(value):
    # The remaining inside of a quoted extended value, kept as qcontent,
    # whitespace and stray quotes (it is percent-decoded later).
    contents = Value()
    while value:
        if value[0] in WSP:
            token, value = get_fws(value)
        elif value[0] == '"':
            token, value = ValueTerminal('"', 'DQUOTE'), value[1:]
        else:
            token, value = get_qcontent(value)
        contents.append(token)
    return contents


def get_parameter(value):
    """attribute [section] ["*"] [CFWS] "=" value

    The value of an extended parameter (RFC 2231) may carry a
    charset'language' prefix.  Some invalid but common spellings (quoted
    extended values, a prefix on a non-extended parameter) are accepted
    with defects.
    """
    param = Parameter()
    attribute, value = get_attribute(value)
    param.append(attribute)
    if not value or value[0] == ';':
        param.defects.append(errors.InvalidHeaderDefect(
            "Parameter contains name ({}) but no value".format(attribute)))
        return param, value
    value = _parameter_markers(value, param)
    if value[0] != '=':
        raise errors.HeaderParseError("Parameter not followed by '='")
    param.append(ValueTerminal('=', 'parameter-separator'))
    value = _optional_cfws(value[1:], param)

    container, after_quotes = param, None
    if param.extended and value[:1] == '"':
        container, value, after_quotes = _open_quoted_extended(value, param)

    first = None
    if value[:1] != "'":
        first, value = get_value(value)
    initial_extended = param.extended and param.section_number == 0
    if not initial_extended:
        if value[:1] != "'":
            # A plain value (or a continuation section): done.
            container.append(first)
            return param, (value if after_quotes is None else after_quotes)
        param.defects.append(errors.InvalidHeaderDefect(
            "Apparent initial-extended-value but attribute "
            "was not marked as extended or was not initial section"))

    if value:
        value = _rfc2231_prefix(value, first, container, param)
    else:
        param.defects.append(errors.InvalidHeaderDefect(
            "Missing required charset/lang delimiters"))
        container.append(first)
        if after_quotes is None:
            return param, value

    if after_quotes is None:
        encoded, value = get_value(value)
    else:
        encoded, value = _raw_quoted_contents(value), after_quotes
    container.append(encoded)
    return param, value


def parse_mime_parameters(value):
    """parameter *( ";" parameter )

    Never raises: unparsable entries become invalid-parameter tokens with
    a defect.
    """
    params = MimeParameters()
    while value:
        try:
            param, value = get_parameter(value)
        except errors.HeaderParseError:
            value = _recover_parameter(value, params)
            if value is None:
                return params
        else:
            params.append(param)
            if value and value[0] != ';':
                # Junk after a good parameter spoils it.
                param.token_type = 'invalid-parameter'
                junk, value = get_invalid_parameter(value)
                param.extend(junk)
                params.defects.append(errors.InvalidHeaderDefect(
                    "parameter with invalid trailing text {!r}".format(junk)))
        if value:
            params.append(ValueTerminal(';', 'parameter-separator'))
            value = value[1:]
    return params


def _recover_parameter(value, params):
    # get_parameter() failed at *value*: keep its text as an invalid
    # parameter (or note an empty entry).  Returns the rest, or None when
    # only CFWS remained.
    leader = None
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
        if not value:
            params.append(leader)
            return None
    if value[0] == ';':
        if leader is not None:
            params.append(leader)
        params.defects.append(errors.InvalidHeaderDefect(
            "parameter entry with no content"))
        return value
    invalid, value = get_invalid_parameter(value)
    if leader:
        invalid[:0] = [leader]
    params.append(invalid)
    params.defects.append(errors.InvalidHeaderDefect(
        "invalid parameter {!r}".format(invalid)))
    return value


def _find_mime_parameters(tokenlist, value):
    # Keep whatever precedes the first ';' as junk; parse what follows as
    # parameters.
    value = _absorb_until_semicolon(value, tokenlist)
    if value:
        tokenlist.append(ValueTerminal(';', 'parameter-separator'))
        tokenlist.append(parse_mime_parameters(value[1:]))


def _leading_token(header, value, missing_message):
    # The first token of a MIME header, appended to *header*; None (after
    # recording a defect and salvaging parameters) if there is none.
    try:
        token, value = get_token(value)
    except errors.HeaderParseError:
        header.defects.append(errors.InvalidHeaderDefect(
            "{} but found {!r}".format(missing_message, value)))
        _find_mime_parameters(header, value)
        return None, value
    header.append(token)
    return token, value


def _trailing_parameters(header, value, what):
    # ';' parameters after the main value; False if something else follows.
    if not value:
        return True
    if value[0] != ';':
        header.defects.append(errors.InvalidHeaderDefect(
            "Only parameters are valid after {}, but "
            "found {!r}".format(what, value)))
        _find_mime_parameters(header, value)
        return False
    header.append(ValueTerminal(';', 'parameter-separator'))
    header.append(parse_mime_parameters(value[1:]))
    return True


def parse_content_type_header(value):
    """maintype "/" subtype *( ";" parameter )"""
    ctype = ContentType()
    if not value:
        ctype.defects.append(errors.HeaderMissingRequiredValue(
            "Missing content type specification"))
        return ctype
    maintype, value = _leading_token(ctype, value,
                                     "Expected content maintype")
    if maintype is None:
        return ctype
    if value[:1] != '/':
        ctype.defects.append(errors.InvalidHeaderDefect(
            "Invalid content type"))
        if value:
            _find_mime_parameters(ctype, value)
        return ctype
    ctype.maintype = maintype.value.strip().lower()
    ctype.append(ValueTerminal('/', 'content-type-separator'))
    subtype, value = _leading_token(ctype, value[1:],
                                    "Expected content subtype")
    if subtype is None:
        return ctype
    ctype.subtype = subtype.value.strip().lower()
    if not _trailing_parameters(ctype, value, 'content type'):
        # A content type followed by junk is not trusted: fall back to the
        # default (text/plain).
        del ctype.maintype, ctype.subtype
    return ctype


def parse_content_disposition_header(value):
    """disposition-type *( ";" parameter )"""
    disp = ContentDisposition()
    if not value:
        disp.defects.append(errors.HeaderMissingRequiredValue(
            "Missing content disposition"))
        return disp
    kind, value = _leading_token(disp, value,
                                 "Expected content disposition")
    if kind is None:
        return disp
    disp.content_disposition = kind.value.strip().lower()
    _trailing_parameters(disp, value, 'content disposition')
    return disp


def parse_content_transfer_encoding_header(value):
    """mechanism   (a single token)"""
    cte = ContentTransferEncoding()
    if not value:
        cte.defects.append(errors.HeaderMissingRequiredValue(
            "Missing content transfer encoding"))
        return cte
    try:
        token, value = get_token(value)
    except errors.HeaderParseError:
        cte.defects.append(errors.InvalidHeaderDefect(
            "Expected content transfer encoding but found {!r}".format(value)))
    else:
        cte.append(token)
        cte.cte = token.value.strip().lower()
    # Anything else is kept, with one defect per piece.
    while value:
        cte.defects.append(errors.InvalidHeaderDefect(
            "Extra text after content transfer encoding"))
        if value[0] in PHRASE_ENDS:
            cte.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            token, value = get_phrase(value)
            cte.append(token)
    return cte


#
# Folding
#

def _steal_trailing_WSP_if_exists(lines):
    # Move a trailing blank of the last line to the start of a new line.
    wsp = ''
    if lines and lines[-1] and lines[-1][-1] in WSP:
        wsp = lines[-1][-1]
        lines[-1] = lines[-1][:-1]
    return wsp


def _refold_parse_tree(parse_tree, *, policy):
    """Serialize *parse_tree* folded to policy.max_line_length, encoding
    non-ASCII (or otherwise unrepresentable) text as RFC 2047 encoded words
    and parameters per RFC 2231.  The result ends with policy.linesep."""
    return _Refolder(policy).run(parse_tree)


def _new_line(lines, text):
    # Start a new output line, reusing the last one if stealing its
    # trailing blank left it empty.
    if len(lines) > 1 and lines[-1] == '':
        lines[-1] = text
    else:
        lines.append(text)


def _has_special_ew(part):
    # An encoded word whose decoded text contains specials must stay
    # encoded even if it would fit as is.
    if part.token_type == 'encoded-word':
        return not SPECIALSNL.isdisjoint(str(part))
    if isinstance(part, TokenList):
        return any(_has_special_ew(x) for x in part)
    return False


_ADDRESS_PARTS = frozenset(('addr-spec', 'local-part', 'domain'))


def _address_part_text(part):
    if part.token_type == 'local-part':
        return part.local_part
    if part.token_type == 'domain':
        return part.domain
    return part.addr_spec


class _Refolder:

    def __init__(self, policy):
        self.policy = policy
        self.maxlen = policy.max_line_length or sys.maxsize
        self.encoding = 'utf-8' if policy.utf8 else 'us-ascii'
        self.lines = ['']
        # whitespace that may have to be encoded at the start of the next
        # encoded word
        self.leading_whitespace = ''
        # start index (in the last line) of the encoded word we could add to
        self.last_ew = None
        self.last_charset = None
        # nesting depth of parts in which encoded words are not allowed
        self.ew_blocked = 0

    def run(self, parse_tree):
        lines = self.lines
        maxlen = self.maxlen
        end_block = Terminal('', 'wrap_as_ew_blocked')
        pending = list(parse_tree)
        want_encoding = False
        while pending:
            part = pending.pop(0)
            if part is end_block:
                self.ew_blocked -= 1
                continue
            text = str(part)
            if not want_encoding:
                if part.token_type in ('ptext', 'vtext'):
                    want_encoding = not SPECIALSNL.isdisjoint(text)
                else:
                    want_encoding = not NLSET.isdisjoint(text) or _has_special_ew(part)
            try:
                text.encode(self.encoding)
                charset = self.encoding
            except UnicodeEncodeError:
                if any(isinstance(d, errors.UndecodableBytesDefect)
                       for d in part.all_defects):
                    charset = 'unknown-8bit'
                else:
                    charset = 'utf-8'
                want_encoding = True

            if part.token_type == 'mime-parameters':
                _fold_mime_parameters(part, lines, maxlen, self.encoding)
                continue

            if (charset != self.encoding
                    and part.token_type in _ADDRESS_PARTS
                    and (part.token_type != 'addr-spec'
                         or getattr(part, '_in_angle_addr', False))):
                if charset != 'unknown-8bit':
                    # Non-ASCII text in an address can only be written
                    # with utf8 (SMTPUTF8); encoded words are not allowed.
                    raise errors.HeaderWriteError(
                        "Non-ASCII {} {!r} cannot be serialized; enable "
                        "policy.utf8 to allow it".format(
                            part.token_type, _address_part_text(part)))
                # Undecodable bytes from a parsed message: keep them as
                # they were.
                want_encoding = False
                self.last_ew = None
                if len(text) <= maxlen - len(lines[-1]):
                    lines[-1] += text
                else:
                    carry = _steal_trailing_WSP_if_exists(lines)
                    if carry or text[:1] in WSP:
                        _new_line(lines, carry + text)
                    else:
                        lines[-1] += text
                continue

            if want_encoding and not self.ew_blocked:
                if not part.as_ew_allowed:
                    want_encoding = False
                    self.last_ew = None
                    if part.syntactic_break:
                        folded = part.fold(policy=self.policy)
                        folded = folded[:-len(self.policy.linesep)]
                        if self.policy.linesep not in folded:
                            if len(folded) > maxlen - len(lines[-1]):
                                lines.append(_steal_trailing_WSP_if_exists(lines))
                            lines[-1] += folded
                            continue
                if not hasattr(part, 'encode'):
                    pending[0:0] = list(part)
                    want_encoding = False
                    continue
                if part.as_ew_allowed:
                    if (self.last_ew is not None and charset != self.last_charset
                            and (self.last_charset == 'unknown-8bit'
                                 or (self.last_charset == 'utf-8'
                                     and charset != 'us-ascii'))):
                        self.last_ew = None
                    self.last_ew = _fold_as_ew(
                        text, lines, maxlen, self.last_ew,
                        part.ew_combine_allowed, charset,
                        self.leading_whitespace)
                    self.leading_whitespace = ''
                    self.last_charset = charset
                    want_encoding = False
                    continue
                # a terminal that must stay as is (e.g. a list separator)
                self.last_ew = None
                want_encoding = False

            if len(text) <= maxlen - len(lines[-1]):
                lines[-1] += text
                continue

            # Too long for the current line.
            self.leading_whitespace = ''
            if part.syntactic_break and len(text) + 1 <= maxlen:
                carry = _steal_trailing_WSP_if_exists(lines)
                if carry or part.startswith_fws():
                    _new_line(lines, carry + text)
                    line = lines[-1]
                    self.leading_whitespace = line[:len(line) - len(line.lstrip(' \t'))]
                    self.last_ew = None
                    continue
            if not hasattr(part, 'encode'):
                subparts = list(part)
                if part.token_type == 'bare-quoted-string':
                    subparts = ([ValueTerminal('"', 'ptext')]
                                + [ValueTerminal(make_quoted_pairs(p), 'ptext')
                                   for p in subparts]
                                + [ValueTerminal('"', 'ptext')])
                elif part.token_type == 'comment':
                    subparts = ([ValueTerminal('(', 'ptext')]
                                + [ValueTerminal(make_parenthesis_pairs(p), 'ptext')
                                   if p.token_type == 'ptext' else p
                                   for p in subparts]
                                + [ValueTerminal(')', 'ptext')])
                if not part.as_ew_allowed:
                    self.ew_blocked += 1
                    subparts.append(end_block)
                pending[0:0] = subparts
                continue
            if part.as_ew_allowed and not self.ew_blocked:
                # encode it anyway, so that it can be split
                pending.insert(0, part)
                want_encoding = True
                continue
            # Unsplittable: put it on a line of its own if possible.
            carry = _steal_trailing_WSP_if_exists(lines)
            if carry or part.startswith_fws():
                _new_line(lines, carry + text)
            else:
                lines[-1] += text
        return self.policy.linesep.join(lines) + self.policy.linesep


def _ew_charset(charset):
    # Plain ASCII text never needs an encoded word; text that does is
    # written as UTF-8.
    return 'utf-8' if charset == 'us-ascii' else charset


def _start_ew_run(to_encode, lines, maxlen, last_ew, ew_combine_allowed):
    # Prepare *to_encode* for encoding.  Text that may join the encoded
    # word ending the last line is merged with it (that word is taken off
    # the line and re-encoded); otherwise a leading blank stays plain.
    # Returns (text, trailing blank, column where the run starts).
    if last_ew is not None and ew_combine_allowed:
        to_encode = str(get_unstructured(lines[-1][last_ew:] + to_encode))
        lines[-1] = lines[-1][:last_ew]
    elif to_encode[0] in WSP:
        if len(lines[-1]) == maxlen:
            lines.append(_steal_trailing_WSP_if_exists(lines))
        lines[-1] += to_encode[0]
        to_encode = to_encode[1:]
    trailing = ''
    if to_encode[-1] in WSP:
        to_encode, trailing = to_encode[:-1], to_encode[-1]
    start = len(lines[-1]) if last_ew is None else last_ew
    return to_encode, trailing, start


def _longest_ew(text, room, charset):
    # The encoded word for the longest prefix of *text* that fits in
    # *room* columns: (prefix length, encoded word).
    take = len(text)
    word = _ew.encode(text, charset=charset)
    while len(word) > room:
        take -= 1
        word = _ew.encode(text[:take], charset=charset)
    return take, word


def _fold_as_ew(to_encode, lines, maxlen, last_ew, ew_combine_allowed,
                charset, leading_whitespace):
    """Write *to_encode* to *lines* as RFC 2047 encoded words, starting new
    lines as needed.

    *last_ew* is the column of an encoded word ending the last line, which
    the new text joins when *ew_combine_allowed*.  *leading_whitespace* is
    whitespace that belonged before the text; at the start of a
    continuation line after an encoded word it is encoded too (a bare blank
    there would be swallowed by the decoder).  Returns the column of the
    last encoded word if later text may join it, else None.
    """
    to_encode, trailing, run_start = _start_ew_run(
        to_encode, lines, maxlen, last_ew, ew_combine_allowed)
    charset = _ew_charset(charset)
    # "=?" charset "?q?" ... "?=" : everything but the payload.
    overhead = len(charset) + 7
    if overhead + 1 >= maxlen:
        raise errors.HeaderParseError(
            "max_line_length is too small to fit an encoded word")
    while to_encode:
        room = maxlen - len(lines[-1])
        if room - overhead - len(leading_whitespace) <= 0:
            lines.append(_steal_trailing_WSP_if_exists(lines) or ' ')
            run_start = len(lines[-1])
            continue
        if (leading_whitespace and len(lines) > 1 and len(lines[-1]) == 1
                and lines[-2].rstrip(" \t").endswith("?=")):
            lines[-1] += _ew.encode(leading_whitespace, charset=charset)
        payload_room = room - overhead - len(leading_whitespace)
        leading_whitespace = ''
        taken, word = _longest_ew(to_encode[:payload_room], room, charset)
        lines[-1] += word
        to_encode = to_encode[taken:]
        if to_encode:
            lines.append(' ')
            run_start = len(lines[-1])
    lines[-1] += trailing
    return run_start if ew_combine_allowed else None


def _parameter_spelling(name, value, encoding):
    # How a parameter is written when it fits on a line: name="value", or
    # in RFC 2231 form when the value cannot be represented in *encoding*.
    # Also returns the charset and error handler a split value is encoded
    # with.
    try:
        value.encode(encoding)
    except UnicodeEncodeError:
        pass
    else:
        return '{}={}'.format(name, quote_string(value)), encoding, 'strict'
    if utils._has_surrogates(value):
        charset, handler = 'unknown-8bit', 'surrogateescape'
    else:
        charset, handler = 'utf-8', 'strict'
    encoded = utils._percent_encode(value, "utf-8", handler)
    return "{}*={}''{}".format(name, charset, encoded), charset, handler


def _rfc2231_continuations(name, value, charset, handler, maxlen):
    # Split a value too long for any line into numbered, percent-encoded
    # sections ( name*0*=charset''..., name*1*=... ), one per line.
    # Returns the lines and the width used, which grows to 78 if *maxlen*
    # leaves no room for the value at all.
    pieces = []
    prefix = charset + "''"
    number = 0
    while value:
        # " name*N*=" + prefix, and the ';' that ends all but the last line
        overhead = len(name) + len(str(number)) + 3 + len(prefix)
        if maxlen <= overhead + 3:
            maxlen = 78
        room = maxlen - overhead - 2
        take = room
        encoded = utils._percent_encode(value[:take], "utf-8", handler)
        while len(encoded) > room and take > 0:
            take -= 1
            encoded = utils._percent_encode(value[:take], "utf-8", handler)
        if take == 0 and not prefix:
            # Not even one character fits and nothing will shrink: force
            # progress with an over-long line rather than loop forever.
            take = 1
            encoded = utils._percent_encode(value[:take], "utf-8", handler)
        # (An empty first section can happen: it only carries the prefix.)
        pieces.append(" {}*{}*={}{}".format(name, number, prefix, encoded))
        prefix = ''
        number += 1
        value = value[take:]
    return [piece + ';' for piece in pieces[:-1]] + pieces[-1:], maxlen


def _fold_mime_parameters(part, lines, maxlen, encoding):
    """Append the parameters of *part* to *lines*: on the current line if
    they fit, else on a line of their own, else split in RFC 2231
    sections."""
    for name, value in part.params:
        if not lines[-1].rstrip().endswith(';'):
            lines[-1] += ';'
        text, charset, handler = _parameter_spelling(name, value, encoding)
        if len(lines[-1]) + len(text) + 1 < maxlen:
            lines[-1] += ' ' + text
        elif len(text) + 2 <= maxlen:
            lines.append(' ' + text)
        else:
            sections, maxlen = _rfc2231_continuations(name, value, charset,
                                                       handler, maxlen)
            lines.extend(sections)
