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
        # Drop CFWS at the ends and around dots.
        pieces = [DOT]
        previous = DOT
        previous_is_list = False
        for tok in self[0] + [DOT]:
            if tok.token_type == 'cfws':
                continue
            if (previous_is_list and tok.token_type == 'dot'
                    and previous[-1].token_type == 'cfws'):
                pieces[-1] = TokenList(previous[:-1])
            is_list = isinstance(tok, TokenList)
            if (is_list and previous.token_type == 'dot'
                    and tok[0].token_type == 'cfws'):
                pieces.append(TokenList(tok[1:]))
            else:
                pieces.append(tok)
            previous = pieces[-1]
            previous_is_list = is_list
        return TokenList(pieces[1:-1]).value


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


def get_qp_ctext(value):
    r"""ctext with quoted pairs: printable text up to '(' or ')' or
    whitespace (the value of a comment's text is a space)."""
    ptext, value, _ = _get_ptext_to_endchars(value, '()')
    ptext = WhiteSpaceTerminal(ptext, 'ptext')
    _validate_xtext(ptext)
    return ptext, value


def get_qcontent(value):
    """qcontent: text of a quoted string up to '"' or whitespace."""
    ptext, value, _ = _get_ptext_to_endchars(value, '"')
    ptext = ValueTerminal(ptext, 'ptext')
    _validate_xtext(ptext)
    return ptext, value


def get_atext(value):
    """atext: a run of characters that are not specials or whitespace."""
    m = _non_atom_end_matcher(value)
    if not m:
        raise errors.HeaderParseError(
            "expected atext but found '{}'".format(value))
    text = m.group()
    atext = ValueTerminal(text, 'atext')
    _validate_xtext(atext)
    return atext, value[len(text):]


def get_bare_quoted_string(value):
    """bare-quoted-string = DQUOTE *([FWS] qcontent) [FWS] DQUOTE"""
    if not value or value[0] != '"':
        raise errors.HeaderParseError(
            "expected '\"' but found '{}'".format(value))
    bqs = BareQuotedString()
    value = value[1:]
    if value and value[0] == '"':
        token, value = get_qcontent(value)
        bqs.append(token)
    while value and value[0] != '"':
        if value[0] in WSP:
            token, value = get_fws(value)
        elif value[:2] == '=?':
            is_ew = False
            try:
                token, value = get_encoded_word(value)
                bqs.defects.append(errors.InvalidHeaderDefect(
                    "encoded word inside quoted string"))
                is_ew = True
            except errors.HeaderParseError:
                token, value = get_qcontent(value)
            if (is_ew and len(bqs) > 1 and bqs[-1].token_type == 'fws'
                    and bqs[-2].token_type == 'encoded-word'):
                bqs[-1] = EWWhiteSpaceTerminal(bqs[-1], 'fws')
        else:
            token, value = get_qcontent(value)
        bqs.append(token)
    if not value:
        bqs.defects.append(errors.InvalidHeaderDefect(
            "end of header inside quoted string"))
        return bqs, value
    return bqs, value[1:]


def get_comment(value):
    """comment = "(" *([FWS] ccontent) [FWS] ")" (comments nest)"""
    if value and value[0] != '(':
        raise errors.HeaderParseError(
            "expected '(' but found '{}'".format(value))
    comment = Comment()
    value = value[1:]
    while value and value[0] != ")":
        if value[0] in WSP:
            token, value = get_fws(value)
        elif value[0] == '(':
            token, value = get_comment(value)
        else:
            token, value = get_qp_ctext(value)
        comment.append(token)
    if not value:
        comment.defects.append(errors.InvalidHeaderDefect(
            "end of header inside comment"))
        return comment, value
    return comment, value[1:]


def get_cfws(value):
    """CFWS = (1*([FWS] comment) [FWS]) / FWS"""
    cfws = CFWSList()
    while value and value[0] in CFWS_LEADER:
        if value[0] in WSP:
            token, value = get_fws(value)
        else:
            token, value = get_comment(value)
        cfws.append(token)
    return cfws, value


def get_quoted_string(value):
    """quoted-string = [CFWS] bare-quoted-string [CFWS]"""
    qs = QuotedString()
    value = _optional_cfws(value, qs)
    token, value = get_bare_quoted_string(value)
    qs.append(token)
    value = _optional_cfws(value, qs)
    return qs, value


def get_atom(value):
    """atom = [CFWS] 1*atext [CFWS]  (an encoded word is also accepted)"""
    atom = Atom()
    value = _optional_cfws(value, atom)
    if value and value[0] in ATOM_ENDS:
        raise errors.HeaderParseError(
            "expected atom but found '{}'".format(value))
    if value.startswith('=?'):
        try:
            token, value = get_encoded_word(value)
        except errors.HeaderParseError:
            token, value = get_atext(value)
    else:
        token, value = get_atext(value)
    atom.append(token)
    value = _optional_cfws(value, atom)
    return atom, value


def get_dot_atom_text(value):
    """dot-atom-text = 1*atext *("." 1*atext)"""
    text = DotAtomText()
    if not value or value[0] in ATOM_ENDS:
        raise errors.HeaderParseError("expected atom at a start of "
                                      "dot-atom-text but found '{}'".format(value))
    while value and value[0] not in ATOM_ENDS:
        token, value = get_atext(value)
        text.append(token)
        if value and value[0] == '.':
            text.append(DOT)
            value = value[1:]
    if text[-1] is DOT:
        raise errors.HeaderParseError("expected atom at end of dot-atom-text "
                                      "but found '{}'".format('.' + value))
    return text, value


def get_dot_atom(value):
    """dot-atom = [CFWS] dot-atom-text [CFWS]"""
    dot_atom = DotAtom()
    if value[0] in CFWS_LEADER:
        token, value = get_cfws(value)
        dot_atom.append(token)
    if value.startswith('=?'):
        try:
            token, value = get_encoded_word(value)
        except errors.HeaderParseError:
            token, value = get_dot_atom_text(value)
    else:
        token, value = get_dot_atom_text(value)
    dot_atom.append(token)
    value = _optional_cfws(value, dot_atom)
    return dot_atom, value


def get_word(value):
    """word = atom / quoted-string

    Leading CFWS is put into the returned token.
    """
    leader = None
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
    if not value:
        raise errors.HeaderParseError(
            "Expected 'atom' or 'quoted-string' but found nothing.")
    if value[0] == '"':
        token, value = get_quoted_string(value)
    elif value[0] in SPECIALS:
        raise errors.HeaderParseError("Expected 'atom' or 'quoted-string' "
                                      "but found '{}'".format(value))
    else:
        token, value = get_atom(value)
    if leader is not None:
        token[:0] = [leader]
    return token, value


def get_phrase(value):
    """phrase = 1*word / obs-phrase  (obs-phrase allows '.' and CFWS)"""
    phrase = Phrase()
    try:
        token, value = get_word(value)
        phrase.append(token)
    except errors.HeaderParseError:
        phrase.defects.append(errors.InvalidHeaderDefect(
            "phrase does not start with word"))
    while value and value[0] not in PHRASE_ENDS:
        if value[0] == '.':
            phrase.append(DOT)
            phrase.defects.append(errors.ObsoleteHeaderDefect(
                "period in 'phrase'"))
            value = value[1:]
            continue
        try:
            token, value = get_word(value)
        except errors.HeaderParseError:
            if value[0] not in CFWS_LEADER:
                raise
            token, value = get_cfws(value)
            phrase.defects.append(errors.ObsoleteHeaderDefect(
                "comment found without atom"))
        phrase.append(token)
    return phrase, value


def get_local_part(value):
    """local-part = dot-atom / quoted-string / obs-local-part"""
    local_part = LocalPart()
    if not value:
        raise errors.HeaderParseError(
            "expected local-part but found '{}'".format(value))
    try:
        token, value = get_dot_atom(value)
    except errors.HeaderParseError:
        try:
            token, value = get_word(value)
        except errors.HeaderParseError:
            if value[0] != '\\' and value[0] in PHRASE_ENDS:
                raise
            token = TokenList()
    if value and (value[0] == '\\' or value[0] not in PHRASE_ENDS):
        obs, value = get_obs_local_part(str(token) + value)
        if obs.token_type == 'invalid-obs-local-part':
            local_part.defects.append(errors.InvalidHeaderDefect(
                "local-part is not dot-atom, quoted-string, or obs-local-part"))
        else:
            local_part.defects.append(errors.ObsoleteHeaderDefect(
                "local-part is not a dot-atom (contains CFWS)"))
        local_part.append(obs)
    else:
        local_part.append(token)
    try:
        local_part.value.encode('ascii')
    except UnicodeEncodeError:
        local_part.defects.append(errors.NonASCIILocalPartDefect(
            "local-part contains non-ASCII characters)"))
    return local_part, value


def get_obs_local_part(value):
    """obs-local-part = word *("." word)"""
    obs = ObsLocalPart()
    after_dot = False
    while value and (value[0] == '\\' or value[0] not in PHRASE_ENDS):
        ch = value[0]
        if ch == '.':
            if after_dot:
                obs.defects.append(errors.InvalidHeaderDefect(
                    "invalid repeated '.'"))
            obs.append(DOT)
            after_dot = True
            value = value[1:]
            continue
        if ch == '\\':
            obs.append(ValueTerminal(ch, 'misplaced-special'))
            value = value[1:]
            obs.defects.append(errors.InvalidHeaderDefect(
                "'\\' character outside of quoted-string/ccontent"))
            after_dot = False
            continue
        if obs and obs[-1].token_type != 'dot':
            obs.defects.append(errors.InvalidHeaderDefect(
                "missing '.' between words"))
        try:
            token, value = get_word(value)
            after_dot = False
        except errors.HeaderParseError:
            if value[0] not in CFWS_LEADER:
                raise
            token, value = get_cfws(value)
        obs.append(token)
    if not obs:
        raise errors.HeaderParseError(
            "expected obs-local-part but found '{}'".format(value))

    def dot_at(first, second):
        return (obs[first].token_type == 'dot'
                or (obs[first].token_type == 'cfws' and len(obs) > 1
                    and obs[second].token_type == 'dot'))

    if dot_at(0, 1):
        obs.defects.append(errors.InvalidHeaderDefect(
            "Invalid leading '.' in local part"))
    if dot_at(-1, -2):
        obs.defects.append(errors.InvalidHeaderDefect(
            "Invalid trailing '.' in local part"))
    if obs.defects:
        obs.token_type = 'invalid-obs-local-part'
    return obs, value


def get_dtext(value):
    """dtext: domain literal text up to '[' or ']' or whitespace."""
    ptext, value, had_qp = _get_ptext_to_endchars(value, '[]')
    ptext = ValueTerminal(ptext, 'ptext')
    if had_qp:
        ptext.defects.append(errors.ObsoleteHeaderDefect(
            "quoted printable found in domain-literal"))
    _validate_xtext(ptext)
    return ptext, value


def _domain_literal_ended_early(value, domain_literal):
    if value:
        return False
    domain_literal.defects.append(errors.InvalidHeaderDefect(
        "end of input inside domain-literal"))
    domain_literal.append(ValueTerminal(']', 'domain-literal-end'))
    return True


def get_domain_literal(value):
    """domain-literal = [CFWS] "[" *([FWS] dtext) [FWS] "]" [CFWS]"""
    dl = DomainLiteral()
    if value[0] in CFWS_LEADER:
        token, value = get_cfws(value)
        dl.append(token)
    if not value:
        raise errors.HeaderParseError("expected domain-literal")
    if value[0] != '[':
        raise errors.HeaderParseError("expected '[' at start of domain-literal "
                                      "but found '{}'".format(value))
    value = value[1:]
    dl.append(ValueTerminal('[', 'domain-literal-start'))
    if _domain_literal_ended_early(value, dl):
        return dl, value
    if value[0] in WSP:
        token, value = get_fws(value)
        dl.append(token)
    token, value = get_dtext(value)
    dl.append(token)
    if _domain_literal_ended_early(value, dl):
        return dl, value
    if value[0] in WSP:
        token, value = get_fws(value)
        dl.append(token)
    if _domain_literal_ended_early(value, dl):
        return dl, value
    if value[0] != ']':
        raise errors.HeaderParseError("expected ']' at end of domain-literal "
                                      "but found '{}'".format(value))
    dl.append(ValueTerminal(']', 'domain-literal-end'))
    value = _optional_cfws(value[1:], dl)
    return dl, value


def get_domain(value):
    """domain = dot-atom / domain-literal / obs-domain"""
    domain = Domain()
    leader = None
    if not value:
        raise errors.HeaderParseError(
            "expected domain but found '{}'".format(value))
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
    if not value:
        raise errors.HeaderParseError(
            "expected domain but found '{}'".format(value))
    if value[0] == '[':
        token, value = get_domain_literal(value)
        if leader is not None:
            token[:0] = [leader]
        domain.append(token)
        return domain, value
    try:
        token, value = get_dot_atom(value)
    except errors.HeaderParseError:
        token, value = get_atom(value)
    if value and value[0] == '@':
        raise errors.HeaderParseError('Invalid Domain')
    if leader is not None:
        token[:0] = [leader]
    domain.append(token)
    if value and value[0] == '.':
        domain.defects.append(errors.ObsoleteHeaderDefect(
            "domain is not a dot-atom (contains CFWS)"))
        if domain[0].token_type == 'dot-atom':
            domain[:] = domain[0]
        while value and value[0] == '.':
            domain.append(DOT)
            token, value = get_atom(value[1:])
            domain.append(token)
    return domain, value


def get_addr_spec(value):
    """addr-spec = local-part "@" domain"""
    spec = AddrSpec()
    token, value = get_local_part(value)
    spec.append(token)
    if not value or value[0] != '@':
        spec.defects.append(errors.InvalidHeaderDefect(
            "addr-spec local part with no domain"))
        return spec, value
    spec.append(ValueTerminal('@', 'address-at-symbol'))
    token, value = get_domain(value[1:])
    spec.append(token)
    return spec, value


def get_obs_route(value):
    """obs-route = obs-domain-list ":"
    obs-domain-list = *(CFWS / ",") "@" domain *("," [CFWS] ["@" domain])
    """
    route = ObsRoute()
    while value and (value[0] == ',' or value[0] in CFWS_LEADER):
        if value[0] in CFWS_LEADER:
            token, value = get_cfws(value)
            route.append(token)
        else:
            route.append(ListSeparator)
            value = value[1:]
    if not value or value[0] != '@':
        raise errors.HeaderParseError(
            "expected obs-route domain but found '{}'".format(value))
    route.append(RouteComponentMarker)
    token, value = get_domain(value[1:])
    route.append(token)
    while value and value[0] == ',':
        route.append(ListSeparator)
        value = value[1:]
        if not value:
            break
        value = _optional_cfws(value, route)
        if not value:
            break
        if value[0] == '@':
            route.append(RouteComponentMarker)
            token, value = get_domain(value[1:])
            route.append(token)
    if not value:
        raise errors.HeaderParseError("end of header while parsing obs-route")
    if value[0] != ':':
        raise errors.HeaderParseError("expected ':' marking end of "
                                      "obs-route but found '{}'".format(value))
    route.append(ValueTerminal(':', 'end-of-obs-route-marker'))
    return route, value[1:]


def get_angle_addr(value):
    """angle-addr = [CFWS] "<" addr-spec ">" [CFWS] / obs-angle-addr"""
    angle = AngleAddr()
    value = _optional_cfws(value, angle)
    if not value or value[0] != '<':
        raise errors.HeaderParseError(
            "expected angle-addr but found '{}'".format(value))
    angle.append(ValueTerminal('<', 'angle-addr-start'))
    value = value[1:]
    if value and value[0] == '>':
        # '<>' is used by SMTP
        angle.append(ValueTerminal('>', 'angle-addr-end'))
        angle.defects.append(errors.InvalidHeaderDefect(
            "null addr-spec in angle-addr"))
        return angle, value[1:]
    try:
        token, value = get_addr_spec(value)
    except errors.HeaderParseError:
        try:
            token, value = get_obs_route(value)
            angle.defects.append(errors.ObsoleteHeaderDefect(
                "obsolete route specification in angle-addr"))
        except errors.HeaderParseError:
            raise errors.HeaderParseError(
                "expected addr-spec or obs-route but found '{}'".format(value))
        angle.append(token)
        token, value = get_addr_spec(value)
    angle.append(token)
    if value and value[0] == '>':
        value = value[1:]
    else:
        angle.defects.append(errors.InvalidHeaderDefect(
            "missing trailing '>' on angle-addr"))
    angle.append(ValueTerminal('>', 'angle-addr-end'))
    value = _optional_cfws(value, angle)
    return angle, value


def get_display_name(value):
    """display-name = phrase"""
    name = DisplayName()
    token, value = get_phrase(value)
    name.extend(token[:])
    name.defects = token.defects[:]
    return name, value


def get_name_addr(value):
    """name-addr = [display-name] angle-addr"""
    name_addr = NameAddr()
    leader = None
    if not value:
        raise errors.HeaderParseError(
            "expected name-addr but found '{}'".format(value))
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
        if not value:
            raise errors.HeaderParseError(
                "expected name-addr but found '{}'".format(leader))
    if value[0] != '<':
        if value[0] in PHRASE_ENDS:
            raise errors.HeaderParseError(
                "expected name-addr but found '{}'".format(value))
        token, value = get_display_name(value)
        if not value:
            raise errors.HeaderParseError(
                "expected name-addr but found '{}'".format(token))
        if leader is not None:
            if isinstance(token[0], TokenList):
                token[0][:0] = [leader]
            else:
                token[:0] = [leader]
            leader = None
        name_addr.append(token)
    token, value = get_angle_addr(value)
    if leader is not None:
        token[:0] = [leader]
    name_addr.append(token)
    return name_addr, value


def get_mailbox(value):
    """mailbox = name-addr / addr-spec"""
    mailbox = Mailbox()
    try:
        token, value = get_name_addr(value)
    except errors.HeaderParseError:
        try:
            token, value = get_addr_spec(value)
        except errors.HeaderParseError:
            raise errors.HeaderParseError(
                "expected mailbox but found '{}'".format(value))
    if any(isinstance(x, errors.InvalidHeaderDefect)
           for x in token.all_defects):
        mailbox.token_type = 'invalid-mailbox'
    mailbox.append(token)
    return mailbox, value


def get_invalid_mailbox(value, endchars):
    """Collect everything up to one of *endchars* as an invalid mailbox."""
    invalid = InvalidMailbox()
    while value and value[0] not in endchars:
        if value[0] in PHRASE_ENDS:
            invalid.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            token, value = get_phrase(value)
            invalid.append(token)
    return invalid, value


def get_mailbox_list(value):
    """mailbox-list = (mailbox *("," mailbox)) / obs-mbox-list

    Invalid entries become invalid-mailbox tokens (with defects).
    """
    mlist = MailboxList()
    while value and value[0] != ';':
        try:
            token, value = get_mailbox(value)
            mlist.append(token)
        except errors.HeaderParseError:
            leader = None
            if value[0] in CFWS_LEADER:
                leader, value = get_cfws(value)
                if not value or value[0] in ',;':
                    mlist.append(leader)
                    mlist.defects.append(errors.ObsoleteHeaderDefect(
                        "empty element in mailbox-list"))
                else:
                    token, value = get_invalid_mailbox(value, ',;')
                    token[:0] = [leader]
                    mlist.append(token)
                    mlist.defects.append(errors.InvalidHeaderDefect(
                        "invalid mailbox in mailbox-list"))
            elif value[0] == ',':
                mlist.defects.append(errors.ObsoleteHeaderDefect(
                    "empty element in mailbox-list"))
            else:
                token, value = get_invalid_mailbox(value, ',;')
                mlist.append(token)
                mlist.defects.append(errors.InvalidHeaderDefect(
                    "invalid mailbox in mailbox-list"))
        if value and value[0] not in ',;':
            # trailing junk: the mailbox becomes invalid
            mailbox = mlist[-1]
            mailbox.token_type = 'invalid-mailbox'
            token, value = get_invalid_mailbox(value, ',;')
            mailbox.extend(token)
            mlist.defects.append(errors.InvalidHeaderDefect(
                "invalid mailbox in mailbox-list"))
        if value and value[0] == ',':
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
    leader = None
    if value[0] in CFWS_LEADER:
        leader, value = get_cfws(value)
        if not value:
            glist.defects.append(errors.InvalidHeaderDefect(
                "end of header in group-list"))
            glist.append(leader)
            return glist, value
        if value[0] == ';':
            glist.append(leader)
            return glist, value
    token, value = get_mailbox_list(value)
    if len(token.all_mailboxes) == 0:
        if leader is not None:
            glist.append(leader)
        glist.extend(token)
        glist.defects.append(errors.ObsoleteHeaderDefect(
            "group-list with empty entries"))
        return glist, value
    if leader is not None:
        token[:0] = [leader]
    glist.append(token)
    return glist, value


def get_group(value):
    """group = display-name ":" [group-list] ";" [CFWS]"""
    group = Group()
    token, value = get_display_name(value)
    if not value or value[0] != ':':
        raise errors.HeaderParseError("expected ':' at end of group "
                                      "display name but found '{}'".format(value))
    group.append(token)
    group.append(ValueTerminal(':', 'group-display-name-terminator'))
    value = value[1:]
    if value and value[0] == ';':
        group.append(ValueTerminal(';', 'group-terminator'))
        return group, value[1:]
    token, value = get_group_list(value)
    group.append(token)
    if not value:
        group.defects.append(errors.InvalidHeaderDefect("end of header in group"))
    elif value[0] != ';':
        raise errors.HeaderParseError(
            "expected ';' at end of group but found {}".format(value))
    group.append(ValueTerminal(';', 'group-terminator'))
    value = _optional_cfws(value[1:], group)
    return group, value


def get_address(value):
    """address = mailbox / group"""
    address = Address()
    try:
        token, value = get_group(value)
    except errors.HeaderParseError:
        try:
            token, value = get_mailbox(value)
        except errors.HeaderParseError:
            raise errors.HeaderParseError(
                "expected address but found '{}'".format(value))
    address.append(token)
    return address, value


def get_address_list(value):
    """address-list = (address *("," address)) / obs-addr-list

    Returns (AddressList, rest).  Invalid entries become addresses holding
    an invalid-mailbox.
    """
    alist = AddressList()
    while value:
        try:
            token, value = get_address(value)
            alist.append(token)
        except errors.HeaderParseError:
            if value[0] in CFWS_LEADER:
                leader, value = get_cfws(value)
                if not value or value[0] == ',':
                    alist.append(leader)
                    alist.defects.append(errors.ObsoleteHeaderDefect(
                        "address-list entry with no content"))
                else:
                    token, value = get_invalid_mailbox(value, ',')
                    token[:0] = [leader]
                    alist.append(Address([token]))
                    alist.defects.append(errors.InvalidHeaderDefect(
                        "invalid address in address-list"))
            elif value[0] == ',':
                alist.defects.append(errors.ObsoleteHeaderDefect(
                    "empty element in address-list"))
            else:
                token, value = get_invalid_mailbox(value, ',')
                alist.append(Address([token]))
                alist.defects.append(errors.InvalidHeaderDefect(
                    "invalid address in address-list"))
        if value and value[0] != ',':
            # trailing junk: the mailbox becomes invalid
            mailbox = alist[-1][0]
            mailbox.token_type = 'invalid-mailbox'
            token, value = get_invalid_mailbox(value, ',')
            mailbox.extend(token)
            alist.defects.append(errors.InvalidHeaderDefect(
                "invalid address in address-list"))
        if value:
            alist.append(ListSeparator)
            value = value[1:]
    return alist, value


def get_no_fold_literal(value):
    """no-fold-literal = "[" *dtext "]" """
    literal = NoFoldLiteral()
    if not value:
        raise errors.HeaderParseError(
            "expected no-fold-literal but found '{}'".format(value))
    if value[0] != '[':
        raise errors.HeaderParseError("expected '[' at the start of "
                                      "no-fold-literal but found '{}'".format(value))
    literal.append(ValueTerminal('[', 'no-fold-literal-start'))
    token, value = get_dtext(value[1:])
    literal.append(token)
    if not value or value[0] != ']':
        raise errors.HeaderParseError("expected ']' at the end of "
                                      "no-fold-literal but found '{}'".format(value))
    literal.append(ValueTerminal(']', 'no-fold-literal-end'))
    return literal, value[1:]


def get_msg_id(value):
    """msg-id = [CFWS] "<" id-left "@" id-right ">" [CFWS]"""
    msg_id = MsgID()
    value = _optional_cfws(value, msg_id)
    if not value or value[0] != '<':
        raise errors.HeaderParseError(
            "expected msg_id but found '{}'".format(value))
    msg_id.append(ValueTerminal('<', 'msg-id-start'))
    value = value[1:]
    try:
        token, value = get_dot_atom_text(value)
    except errors.HeaderParseError:
        try:
            token, value = get_obs_local_part(value)
            msg_id.defects.append(errors.ObsoleteHeaderDefect(
                "obsolete id-left in msg-id"))
        except errors.HeaderParseError:
            raise errors.HeaderParseError("expected dot-atom-text or "
                                          "obs-id-left but found '{}'".format(value))
    msg_id.append(token)
    if not value or value[0] != '@':
        msg_id.defects.append(errors.InvalidHeaderDefect(
            "msg-id with no id-right"))
        if value and value[0] == '>':
            msg_id.append(ValueTerminal('>', 'msg-id-end'))
            value = value[1:]
        return msg_id, value
    msg_id.append(ValueTerminal('@', 'address-at-symbol'))
    value = value[1:]
    try:
        token, value = get_dot_atom_text(value)
    except errors.HeaderParseError:
        try:
            token, value = get_no_fold_literal(value)
        except errors.HeaderParseError:
            try:
                token, value = get_domain(value)
                msg_id.defects.append(errors.ObsoleteHeaderDefect(
                    "obsolete id-right in msg-id"))
            except errors.HeaderParseError:
                raise errors.HeaderParseError(
                    "expected dot-atom-text, no-fold-literal or obs-id-right "
                    "but found '{}'".format(value))
    msg_id.append(token)
    if value and value[0] == '>':
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
    digits = ''
    while value and value[0] != '.' and value[0] not in CFWS_LEADER:
        digits += value[0]
        value = value[1:]
    if not digits.isdigit():
        mv.defects.append(errors.InvalidHeaderDefect(
            "Expected MIME major version number but found {!r}".format(digits)))
        mv.append(ValueTerminal(digits, 'xtext'))
    else:
        mv.major = int(digits)
        mv.append(ValueTerminal(digits, 'digits'))
    value = _optional_cfws(value, mv)
    if not value or value[0] != '.':
        if mv.major is not None:
            mv.defects.append(errors.InvalidHeaderDefect(
                "Incomplete MIME version; found only major number"))
        if value:
            mv.append(ValueTerminal(value, 'xtext'))
        return mv
    mv.append(ValueTerminal('.', 'version-separator'))
    value = _optional_cfws(value[1:], mv)
    if not value:
        if mv.major is not None:
            mv.defects.append(errors.InvalidHeaderDefect(
                "Incomplete MIME version; found only major number"))
        return mv
    digits = ''
    while value and value[0] not in CFWS_LEADER:
        digits += value[0]
        value = value[1:]
    if not digits.isdigit():
        mv.defects.append(errors.InvalidHeaderDefect(
            "Expected MIME minor version number but found {!r}".format(digits)))
        mv.append(ValueTerminal(digits, 'xtext'))
    else:
        mv.minor = int(digits)
        mv.append(ValueTerminal(digits, 'digits'))
    value = _optional_cfws(value, mv)
    if value:
        mv.defects.append(errors.InvalidHeaderDefect(
            "Excess non-CFWS text after MIME version"))
        mv.append(ValueTerminal(value, 'xtext'))
    return mv


def get_invalid_parameter(value):
    """Everything up to the next ';' as an invalid parameter."""
    invalid = InvalidParameter()
    while value and value[0] != ';':
        if value[0] in PHRASE_ENDS:
            invalid.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            token, value = get_phrase(value)
            invalid.append(token)
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
    """section = "*" digits  (no leading zeros except for 0 itself)"""
    section = Section()
    if not value or value[0] != '*':
        raise errors.HeaderParseError(
            "Expected section but found {}".format(value))
    section.append(ValueTerminal('*', 'section-marker'))
    value = value[1:]
    if not value or not value[0].isdigit():
        raise errors.HeaderParseError(
            "Expected section number but found {}".format(value))
    end = 0
    while end < len(value) and value[end].isdigit():
        end += 1
    digits = value[:end]
    value = value[end:]
    if digits[0] == '0' and digits != '0':
        section.defects.append(errors.InvalidHeaderDefect(
            "section number has an invalid leading 0"))
    section.number = int(digits)
    section.append(ValueTerminal(digits, 'digits'))
    return section, value


def get_value(value):
    """value = token / quoted-string"""
    v = Value()
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
    v.append(token)
    return v, value


def get_parameter(value):
    """attribute [section] ["*"] [CFWS] "=" value

    Handles RFC 2231 sections and charset'language'value encoding, and
    tolerates (with defects) extended values that are quoted.
    """
    param = Parameter()
    token, value = get_attribute(value)
    param.append(token)
    if not value or value[0] == ';':
        param.defects.append(errors.InvalidHeaderDefect(
            "Parameter contains name ({}) but no value".format(token)))
        return param, value
    if value[0] == '*':
        try:
            token, value = get_section(value)
            param.sectioned = True
            param.append(token)
        except errors.HeaderParseError:
            pass
        if not value:
            raise errors.HeaderParseError("Incomplete parameter")
        if value[0] == '*':
            param.append(ValueTerminal('*', 'extended-parameter-marker'))
            value = value[1:]
            param.extended = True
    if value[0] != '=':
        raise errors.HeaderParseError("Parameter not followed by '='")
    param.append(ValueTerminal('=', 'parameter-separator'))
    value = _optional_cfws(value[1:], param)

    after_quotes = None      # what follows a quoted extended value
    target = param
    if param.extended and value and value[0] == '"':
        # Quoted extended values are invalid, but common; accept them if
        # their content looks like an extended value.
        qstring, rest = get_quoted_string(value)
        inner = qstring.stripped_value
        plausible = False
        if param.section_number == 0:
            if inner and inner[0] == "'":
                plausible = True
            else:
                try:
                    token, tail = get_attrtext(inner)
                except errors.HeaderParseError:
                    pass
                else:
                    if tail and tail[0] == "'":
                        plausible = True
        else:
            try:
                token, tail = get_extended_attrtext(inner)
            except errors.HeaderParseError:
                pass
            else:
                if not tail:
                    plausible = True
        if plausible:
            param.defects.append(errors.InvalidHeaderDefect(
                "Quoted string value for extended parameter is invalid"))
            param.append(qstring)
            for t in qstring:
                if t.token_type == 'bare-quoted-string':
                    t[:] = []
                    target = t
                    break
            value = inner
            after_quotes = rest
        else:
            param.defects.append(errors.InvalidHeaderDefect(
                "Parameter marked as extended but appears to have a "
                "quoted string value that is non-encoded"))
    if value and value[0] == "'":
        token = None
    else:
        token, value = get_value(value)
    if not param.extended or param.section_number > 0:
        if not value or value[0] != "'":
            target.append(token)
            if after_quotes is not None:
                assert not value, value
                value = after_quotes
            return param, value
        param.defects.append(errors.InvalidHeaderDefect(
            "Apparent initial-extended-value but attribute "
            "was not marked as extended or was not initial section"))
    if not value:
        # charset'lang' prefix missing: the token is the value
        param.defects.append(errors.InvalidHeaderDefect(
            "Missing required charset/lang delimiters"))
        target.append(token)
        if after_quotes is None:
            return param, value
    else:
        if token is not None:
            for t in token:
                if t.token_type == 'extended-attrtext':
                    break
            target.append(t)
            param.charset = t.value
        if value[0] != "'":
            raise errors.HeaderParseError("Expected RFC2231 char/lang encoding "
                                          "delimiter, but found {!r}".format(value))
        target.append(ValueTerminal("'", 'RFC2231-delimiter'))
        value = value[1:]
        if value and value[0] != "'":
            token, value = get_attrtext(value)
            target.append(token)
            param.lang = token.value
            if not value or value[0] != "'":
                raise errors.HeaderParseError("Expected RFC2231 char/lang "
                                              "encoding delimiter, but found {}".format(value))
        target.append(ValueTerminal("'", 'RFC2231-delimiter'))
        value = value[1:]
    if after_quotes is not None:
        # the rest of the quoted string is the (percent encoded) value
        v = Value()
        while value:
            if value[0] in WSP:
                token, value = get_fws(value)
            elif value[0] == '"':
                token = ValueTerminal('"', 'DQUOTE')
                value = value[1:]
            else:
                token, value = get_qcontent(value)
            v.append(token)
        token = v
    else:
        token, value = get_value(value)
    target.append(token)
    if after_quotes is not None:
        assert not value, value
        value = after_quotes
    return param, value


def parse_mime_parameters(value):
    """parameter *( ";" parameter )   -- never raises"""
    params = MimeParameters()
    while value:
        try:
            token, value = get_parameter(value)
            params.append(token)
        except errors.HeaderParseError:
            leader = None
            if value[0] in CFWS_LEADER:
                leader, value = get_cfws(value)
            if not value:
                params.append(leader)
                return params
            if value[0] == ';':
                if leader is not None:
                    params.append(leader)
                params.defects.append(errors.InvalidHeaderDefect(
                    "parameter entry with no content"))
            else:
                token, value = get_invalid_parameter(value)
                if leader:
                    token[:0] = [leader]
                params.append(token)
                params.defects.append(errors.InvalidHeaderDefect(
                    "invalid parameter {!r}".format(token)))
        if value and value[0] != ';':
            param = params[-1]
            param.token_type = 'invalid-parameter'
            token, value = get_invalid_parameter(value)
            param.extend(token)
            params.defects.append(errors.InvalidHeaderDefect(
                "parameter with invalid trailing text {!r}".format(token)))
        if value:
            params.append(ValueTerminal(';', 'parameter-separator'))
            value = value[1:]
    return params


def _find_mime_parameters(tokenlist, value):
    # Skip junk up to the first ';' and parse what follows as parameters.
    while value and value[0] != ';':
        if value[0] in PHRASE_ENDS:
            tokenlist.append(ValueTerminal(value[0], 'misplaced-special'))
            value = value[1:]
        else:
            token, value = get_phrase(value)
            tokenlist.append(token)
    if not value:
        return
    tokenlist.append(ValueTerminal(';', 'parameter-separator'))
    tokenlist.append(parse_mime_parameters(value[1:]))


def parse_content_type_header(value):
    """maintype "/" subtype *( ";" parameter )"""
    ctype = ContentType()
    if not value:
        ctype.defects.append(errors.HeaderMissingRequiredValue(
            "Missing content type specification"))
        return ctype
    try:
        token, value = get_token(value)
    except errors.HeaderParseError:
        ctype.defects.append(errors.InvalidHeaderDefect(
            "Expected content maintype but found {!r}".format(value)))
        _find_mime_parameters(ctype, value)
        return ctype
    ctype.append(token)
    if not value or value[0] != '/':
        ctype.defects.append(errors.InvalidHeaderDefect("Invalid content type"))
        if value:
            _find_mime_parameters(ctype, value)
        return ctype
    ctype.maintype = token.value.strip().lower()
    ctype.append(ValueTerminal('/', 'content-type-separator'))
    value = value[1:]
    try:
        token, value = get_token(value)
    except errors.HeaderParseError:
        ctype.defects.append(errors.InvalidHeaderDefect(
            "Expected content subtype but found {!r}".format(value)))
        _find_mime_parameters(ctype, value)
        return ctype
    ctype.append(token)
    ctype.subtype = token.value.strip().lower()
    if not value:
        return ctype
    if value[0] != ';':
        ctype.defects.append(errors.InvalidHeaderDefect(
            "Only parameters are valid after content type, but "
            "found {!r}".format(value)))
        # an invalid content type is treated as text/plain
        del ctype.maintype, ctype.subtype
        _find_mime_parameters(ctype, value)
        return ctype
    ctype.append(ValueTerminal(';', 'parameter-separator'))
    ctype.append(parse_mime_parameters(value[1:]))
    return ctype


def parse_content_disposition_header(value):
    """disposition-type *( ";" parameter )"""
    disp = ContentDisposition()
    if not value:
        disp.defects.append(errors.HeaderMissingRequiredValue(
            "Missing content disposition"))
        return disp
    try:
        token, value = get_token(value)
    except errors.HeaderParseError:
        disp.defects.append(errors.InvalidHeaderDefect(
            "Expected content disposition but found {!r}".format(value)))
        _find_mime_parameters(disp, value)
        return disp
    disp.append(token)
    disp.content_disposition = token.value.strip().lower()
    if not value:
        return disp
    if value[0] != ';':
        disp.defects.append(errors.InvalidHeaderDefect(
            "Only parameters are valid after content disposition, but "
            "found {!r}".format(value)))
        _find_mime_parameters(disp, value)
        return disp
    disp.append(ValueTerminal(';', 'parameter-separator'))
    disp.append(parse_mime_parameters(value[1:]))
    return disp


def parse_content_transfer_encoding_header(value):
    """mechanism"""
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
                    want_encoding = not NLSET.isdisjoint(text)
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
                    lines.append(carry + text)
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
                lines.append(carry + text)
            else:
                lines[-1] += text
        return self.policy.linesep.join(lines) + self.policy.linesep


def _fold_as_ew(to_encode, lines, maxlen, last_ew, ew_combine_allowed,
                charset, leading_whitespace):
    """Append *to_encode* to *lines* as encoded words, wrapping as needed.

    If *last_ew* is the start of an encoded word at the end of the last
    line and combining is allowed, the text is merged into it.  Returns the
    position of the last encoded word for further combining (or None).
    """
    if last_ew is not None and ew_combine_allowed:
        to_encode = str(get_unstructured(lines[-1][last_ew:] + to_encode))
        lines[-1] = lines[-1][:last_ew]
    elif to_encode[0] in WSP:
        # keep a leading blank unencoded
        blank = to_encode[0]
        to_encode = to_encode[1:]
        if len(lines[-1]) == maxlen:
            lines.append(_steal_trailing_WSP_if_exists(lines))
        lines[-1] += blank
    trailing = ''
    if to_encode[-1] in WSP:
        trailing = to_encode[-1]
        to_encode = to_encode[:-1]
    new_last_ew = len(lines[-1]) if last_ew is None else last_ew
    encode_as = 'utf-8' if charset == 'us-ascii' else charset
    chrome_len = len(encode_as) + 7
    if chrome_len + 1 >= maxlen:
        raise errors.HeaderParseError(
            "max_line_length is too small to fit an encoded word")
    while to_encode:
        room = maxlen - len(lines[-1])
        text_room = room - chrome_len - len(leading_whitespace)
        if text_room <= 0:
            lines.append(' ')
            continue
        if len(lines) > 1 and len(lines[-1]) == 1 and leading_whitespace:
            lines[-1] += _ew.encode(leading_whitespace, charset=encode_as)
            leading_whitespace = ''
        chunk = to_encode[:text_room]
        word = _ew.encode(chunk, charset=encode_as)
        while len(word) > room:
            chunk = chunk[:-1]
            word = _ew.encode(chunk, charset=encode_as)
        lines[-1] += word
        to_encode = to_encode[len(chunk):]
        leading_whitespace = ''
        if to_encode:
            lines.append(' ')
            new_last_ew = len(lines[-1])
    lines[-1] += trailing
    return new_last_ew if ew_combine_allowed else None


def _fold_mime_parameters(part, lines, maxlen, encoding):
    """Fold MIME parameters, using RFC 2231 encoding (and continuations)
    where the value needs it."""
    for name, value in part.params:
        if not lines[-1].rstrip().endswith(';'):
            lines[-1] += ';'
        charset = encoding
        errors_handler = 'strict'
        try:
            value.encode(encoding)
            needs_encoding = False
        except UnicodeEncodeError:
            needs_encoding = True
            if utils._has_surrogates(value):
                charset = 'unknown-8bit'
                errors_handler = 'surrogateescape'
            else:
                charset = 'utf-8'
        if needs_encoding:
            encoded = utils._percent_encode(value, "utf-8", errors_handler)
            text = "{}*={}''{}".format(name, charset, encoded)
        else:
            text = '{}={}'.format(name, quote_string(value))
        if len(lines[-1]) + len(text) + 1 < maxlen:
            lines[-1] = lines[-1] + ' ' + text
            continue
        if len(text) + 2 <= maxlen:
            lines.append(' ' + text)
            continue
        # Split into RFC 2231 sections, all of them encoded.
        section = 0
        prefix = charset + "''"
        while value:
            chrome_len = len(name) + len(str(section)) + 3 + len(prefix)
            if maxlen <= chrome_len + 3:
                maxlen = 78
            limit = maxlen - chrome_len - 2
            split = limit
            while True:
                encoded = utils._percent_encode(value[:split], "utf-8", errors_handler)
                if len(encoded) <= limit:
                    break
                split -= 1
            lines.append(" {}*{}*={}{}".format(name, section, prefix, encoded))
            prefix = ''
            section += 1
            value = value[split:]
            if value:
                lines[-1] += ';'
