"""Header classes used by the modern email policies.

A header object is a str (the decoded value) with extra attributes that
depend on the kind of header (addresses, datetime, parameters ...).  The
HeaderRegistry creates them from (name, value) pairs.
"""

from types import MappingProxyType

from email import utils
from email import errors
from email import _header_value_parser as parser


class Address:
    """One email address: display name, username and domain."""

    def __init__(self, display_name='', username='', domain='', addr_spec=None):
        """Either give *username* and *domain*, or a complete *addr_spec*
        (which is parsed; invalid ones raise)."""
        combined = ''.join(filter(None, (display_name, username, domain,
                                         addr_spec)))
        if '\r' in combined or '\n' in combined:
            raise ValueError("invalid arguments; address parts cannot "
                             "contain CR or LF")
        if addr_spec is not None:
            if username or domain:
                raise TypeError("addrspec specified when username and/or "
                                "domain also specified")
            spec, rest = parser.get_addr_spec(addr_spec)
            if rest:
                raise ValueError("Invalid addr_spec; only '{}' could be "
                                 "parsed from '{}'".format(spec, addr_spec))
            if spec.all_defects:
                raise spec.all_defects[0]
            username = spec.local_part
            domain = spec.domain
        self._display_name = display_name
        self._username = username
        self._domain = domain

    @property
    def display_name(self):
        return self._display_name

    @property
    def username(self):
        return self._username

    @property
    def domain(self):
        return self._domain

    @property
    def addr_spec(self):
        """username@domain, with the username quoted if necessary."""
        local = self.username
        if not parser.DOT_ATOM_ENDS.isdisjoint(local):
            local = parser.quote_string(local)
        if self.domain:
            return local + '@' + self.domain
        if not local:
            return '<>'
        return local

    def __repr__(self):
        return "{}(display_name={!r}, username={!r}, domain={!r})".format(
            self.__class__.__name__, self.display_name, self.username,
            self.domain)

    def __str__(self):
        name = self.display_name
        if not parser.SPECIALS.isdisjoint(name):
            name = parser.quote_string(name)
        if name:
            spec = '' if self.addr_spec == '<>' else self.addr_spec
            return "{} <{}>".format(name, spec)
        return self.addr_spec

    def __eq__(self, other):
        if not isinstance(other, Address):
            return NotImplemented
        return (self.display_name == other.display_name
                and self.username == other.username
                and self.domain == other.domain)


class Group:
    """A named group of addresses (display_name None means no group)."""

    def __init__(self, display_name=None, addresses=None):
        self._display_name = display_name
        self._addresses = tuple(addresses) if addresses else tuple()

    @property
    def display_name(self):
        return self._display_name

    @property
    def addresses(self):
        return self._addresses

    def __repr__(self):
        return "{}(display_name={!r}, addresses={!r}".format(
            self.__class__.__name__, self.display_name, self.addresses)

    def __str__(self):
        if self.display_name is None and len(self.addresses) == 1:
            return str(self.addresses[0])
        name = self.display_name
        if name is not None and not parser.SPECIALS.isdisjoint(name):
            name = parser.quote_string(name)
        members = ", ".join(str(x) for x in self.addresses)
        if members:
            members = ' ' + members
        return "{}:{};".format(name, members)

    def __eq__(self, other):
        if not isinstance(other, Group):
            return NotImplemented
        return (self.display_name == other.display_name
                and self.addresses == other.addresses)


# -- header classes -------------------------------------------------------

class BaseHeader(str):
    """Base class of all header objects (mixed with a specific header
    class by the HeaderRegistry).

    The specific class provides parse(value, kwds), which must set
    kwds['decoded'] (the str value) and kwds['parse_tree'], and may add
    other keywords that its init() consumes.
    """

    def __new__(cls, name, value):
        kwds = {'defects': []}
        cls.parse(value, kwds)
        if utils._has_surrogates(kwds['decoded']):
            kwds['decoded'] = utils._sanitize(kwds['decoded'])
        self = str.__new__(cls, kwds['decoded'])
        del kwds['decoded']
        self.init(name, **kwds)
        return self

    def init(self, name, *, parse_tree, defects):
        self._name = name
        self._parse_tree = parse_tree
        self._defects = defects

    @property
    def name(self):
        return self._name

    @property
    def defects(self):
        return tuple(self._defects)

    def __reduce__(self):
        return (_reconstruct_header,
                (self.__class__.__name__, self.__class__.__bases__, str(self)),
                self.__getstate__())

    @classmethod
    def _reconstruct(cls, value):
        return str.__new__(cls, value)

    def fold(self, *, policy):
        """Return 'Name: value' folded according to *policy*, ending with
        policy.linesep."""
        header = parser.Header([
            parser.HeaderLabel([
                parser.ValueTerminal(self.name, 'header-name'),
                parser.ValueTerminal(':', 'header-sep')]),
        ])
        if self._parse_tree:
            header.append(
                parser.CFWSList([parser.WhiteSpaceTerminal(' ', 'fws')]))
        header.append(self._parse_tree)
        return header.fold(policy=policy)


def _reconstruct_header(cls_name, bases, value):
    return type(cls_name, bases, {})._reconstruct(value)


class UnstructuredHeader:
    """Free text (with RFC 2047 encoded words decoded)."""

    max_count = None
    value_parser = staticmethod(parser.get_unstructured)

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = cls.value_parser(value)
        kwds['decoded'] = str(kwds['parse_tree'])


class UniqueUnstructuredHeader(UnstructuredHeader):
    max_count = 1


class DateHeader:
    """A date; the 'datetime' attribute holds the parsed value.

    A datetime may also be assigned directly.
    """

    max_count = None
    # used for folding only
    value_parser = staticmethod(parser.get_unstructured)

    @classmethod
    def parse(cls, value, kwds):
        if not value:
            kwds['defects'].append(errors.HeaderMissingRequiredValue())
            kwds['datetime'] = None
            kwds['decoded'] = ''
            kwds['parse_tree'] = parser.TokenList()
            return
        if isinstance(value, str):
            kwds['decoded'] = value
            try:
                value = utils.parsedate_to_datetime(value)
            except ValueError:
                kwds['defects'].append(
                    errors.InvalidDateDefect('Invalid date value or format'))
                kwds['datetime'] = None
                kwds['parse_tree'] = parser.TokenList()
                return
        kwds['datetime'] = value
        kwds['decoded'] = utils.format_datetime(value)
        kwds['parse_tree'] = cls.value_parser(kwds['decoded'])

    def init(self, *args, **kw):
        self._datetime = kw.pop('datetime')
        super().init(*args, **kw)

    @property
    def datetime(self):
        return self._datetime


class UniqueDateHeader(DateHeader):
    max_count = 1


class AddressHeader:
    """A list of addresses and groups ('groups' and 'addresses')."""

    max_count = None

    @staticmethod
    def value_parser(value):
        address_list, value = parser.get_address_list(value)
        assert not value, 'this should not happen'
        return address_list

    @classmethod
    def parse(cls, value, kwds):
        if isinstance(value, str):
            kwds['parse_tree'] = tree = cls.value_parser(value)
            groups = []
            for addr in tree.addresses:
                members = [Address(mb.display_name or '', mb.local_part or '',
                                   mb.domain or '')
                           for mb in addr.all_mailboxes]
                groups.append(Group(addr.display_name, members))
            defects = list(tree.all_defects)
        else:
            # Address and Group objects (or an iterable of them)
            if not hasattr(value, '__iter__'):
                value = [value]
            groups = [item if hasattr(item, 'addresses') else Group(None, [item])
                      for item in value]
            defects = []
        kwds['groups'] = groups
        kwds['defects'] = defects
        kwds['decoded'] = ', '.join(str(item) for item in groups)
        if 'parse_tree' not in kwds:
            kwds['parse_tree'] = cls.value_parser(kwds['decoded'])

    def init(self, *args, **kw):
        self._groups = tuple(kw.pop('groups'))
        self._addresses = None
        super().init(*args, **kw)

    @property
    def groups(self):
        return self._groups

    @property
    def addresses(self):
        if self._addresses is None:
            self._addresses = tuple(address for group in self._groups
                                    for address in group.addresses)
        return self._addresses


class UniqueAddressHeader(AddressHeader):
    max_count = 1


class SingleAddressHeader(AddressHeader):

    @property
    def address(self):
        if len(self.addresses) != 1:
            raise ValueError(("value of single address header {} is not "
                              "a single address").format(self.name))
        return self.addresses[0]


class UniqueSingleAddressHeader(SingleAddressHeader):
    max_count = 1


class MIMEVersionHeader:
    """MIME-Version: 'version', 'major' and 'minor' attributes."""

    max_count = 1
    value_parser = staticmethod(parser.parse_mime_version)

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = tree = cls.value_parser(value)
        kwds['decoded'] = str(tree)
        kwds['defects'].extend(tree.all_defects)
        kwds['major'] = None if tree.minor is None else tree.major
        kwds['minor'] = tree.minor
        if tree.minor is not None:
            kwds['version'] = '{}.{}'.format(kwds['major'], kwds['minor'])
        else:
            kwds['version'] = None

    def init(self, *args, **kw):
        self._version = kw.pop('version')
        self._major = kw.pop('major')
        self._minor = kw.pop('minor')
        super().init(*args, **kw)

    @property
    def major(self):
        return self._major

    @property
    def minor(self):
        return self._minor

    @property
    def version(self):
        return self._version


class ParameterizedMIMEHeader:
    """A MIME header with parameters ('params' mapping, names lowercased)."""

    max_count = 1

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = tree = cls.value_parser(value)
        kwds['decoded'] = str(tree)
        kwds['defects'].extend(tree.all_defects)
        if tree.params is None:
            kwds['params'] = {}
        else:
            kwds['params'] = {utils._sanitize(name).lower():
                              utils._sanitize(value)
                              for name, value in tree.params}

    def init(self, *args, **kw):
        self._params = kw.pop('params')
        super().init(*args, **kw)

    @property
    def params(self):
        return MappingProxyType(self._params)


class ContentTypeHeader(ParameterizedMIMEHeader):

    value_parser = staticmethod(parser.parse_content_type_header)

    def init(self, *args, **kw):
        super().init(*args, **kw)
        self._maintype = utils._sanitize(self._parse_tree.maintype)
        self._subtype = utils._sanitize(self._parse_tree.subtype)

    @property
    def maintype(self):
        return self._maintype

    @property
    def subtype(self):
        return self._subtype

    @property
    def content_type(self):
        return self.maintype + '/' + self.subtype


class ContentDispositionHeader(ParameterizedMIMEHeader):

    value_parser = staticmethod(parser.parse_content_disposition_header)

    def init(self, *args, **kw):
        super().init(*args, **kw)
        disposition = self._parse_tree.content_disposition
        self._content_disposition = (disposition if disposition is None
                                     else utils._sanitize(disposition))

    @property
    def content_disposition(self):
        return self._content_disposition


class ContentTransferEncodingHeader:

    max_count = 1
    value_parser = staticmethod(parser.parse_content_transfer_encoding_header)

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = tree = cls.value_parser(value)
        kwds['decoded'] = str(tree)
        kwds['defects'].extend(tree.all_defects)

    def init(self, *args, **kw):
        super().init(*args, **kw)
        self._cte = utils._sanitize(self._parse_tree.cte)

    @property
    def cte(self):
        return self._cte


class MessageIDHeader:

    max_count = 1
    value_parser = staticmethod(parser.parse_message_id)

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = tree = cls.value_parser(value)
        kwds['decoded'] = str(tree)
        kwds['defects'].extend(tree.all_defects)


class MessageIDsHeader:
    """A list of message ids (References, In-Reply-To)."""

    max_count = 1
    value_parser = staticmethod(parser.parse_message_ids)

    @classmethod
    def parse(cls, value, kwds):
        kwds['parse_tree'] = tree = cls.value_parser(value)
        kwds['decoded'] = str(tree)
        kwds['defects'].extend(tree.all_defects)


_default_header_map = {
    'subject': UniqueUnstructuredHeader,
    'date': UniqueDateHeader,
    'resent-date': DateHeader,
    'orig-date': UniqueDateHeader,
    'sender': UniqueSingleAddressHeader,
    'resent-sender': SingleAddressHeader,
    'to': UniqueAddressHeader,
    'resent-to': AddressHeader,
    'cc': UniqueAddressHeader,
    'resent-cc': AddressHeader,
    'bcc': UniqueAddressHeader,
    'resent-bcc': AddressHeader,
    'from': UniqueAddressHeader,
    'resent-from': AddressHeader,
    'reply-to': UniqueAddressHeader,
    'mime-version': MIMEVersionHeader,
    'content-type': ContentTypeHeader,
    'content-disposition': ContentDispositionHeader,
    'content-transfer-encoding': ContentTransferEncodingHeader,
    'message-id': MessageIDHeader,
    'references': MessageIDsHeader,
    'in-reply-to': MessageIDsHeader,
}


class HeaderRegistry:
    """A factory turning (name, value) into header objects."""

    def __init__(self, base_class=BaseHeader, default_class=UnstructuredHeader,
                 use_default_map=True):
        """*base_class* is mixed into every created class; *default_class*
        is used for names without a registered class."""
        self.registry = {}
        self.base_class = base_class
        self.default_class = default_class
        if use_default_map:
            self.registry.update(_default_header_map)

    def map_to_type(self, name, cls):
        """Use *cls* for headers called *name*."""
        self.registry[name.lower()] = cls

    def __getitem__(self, name):
        cls = self.registry.get(name.lower(), self.default_class)
        return type('_' + cls.__name__, (cls, self.base_class), {})

    def __call__(self, name, value):
        """Create a header object for *name* from *value*."""
        return self[name](name, value)
