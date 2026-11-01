"""The message classes: Message (the classic API) and MIMEPart /
EmailMessage (the API used with the modern policies)."""

__all__ = ['Message', 'EmailMessage']

import binascii
import re
import quopri
from io import BytesIO, StringIO

from email import utils
from email import errors
from email._policybase import compat32
from email import charset as _charset
from email._encoded_words import decode_b

Charset = _charset.Charset

SEMISPACE = '; '

# Characters that force quoting of a parameter value.
tspecials = re.compile(r'[ \(\)<>@,;:\\"/\[\]\?=]')


def _splitparam(param):
    # 'text/plain; charset=x' -> ('text/plain', 'charset=x')
    head, sep, tail = str(param).partition(';')
    if not sep:
        return head.strip(), None
    return head.strip(), tail.strip()


def _formatparam(param, value=None, quote=True):
    """Format 'param=value' for a structured header.

    A tuple value is (charset, language, text) and is written RFC 2231
    encoded, as is a value that is not ASCII.  With *quote* the value is
    always double-quoted, otherwise only when necessary.
    """
    if value is None or len(value) == 0:
        return param
    if isinstance(value, tuple):
        return '%s*=%s' % (param, utils.encode_rfc2231(value[2], value[0],
                                                        value[1]))
    try:
        value.encode('ascii')
    except UnicodeEncodeError:
        return '%s*=%s' % (param, utils.encode_rfc2231(value, 'utf-8', ''))
    if quote or tspecials.search(value):
        return '%s="%s"' % (param, utils.quote(value))
    return '%s=%s' % (param, value)


def _parseparam(s):
    # Split a parameter list at ';' outside of quoted strings; names are
    # lower-cased and spaces around '=' removed.
    s = str(s)
    result = []
    pos = 0
    n = len(s)
    while True:
        end = s.find(';', pos)
        while end > 0 and (s.count('"', pos, end) - s.count('\\"', pos, end)) % 2:
            end = s.find(';', end + 1)
        if end < 0:
            end = n
        item = s[pos:end]
        if '=' in item:
            name, _, value = item.partition('=')
            item = name.strip().lower() + '=' + value.strip()
        result.append(item.strip())
        if end >= n:
            break
        pos = end + 1
    return result


def _unquotevalue(value):
    if isinstance(value, tuple):
        return value[0], value[1], utils.unquote(value[2])
    return utils.unquote(value)


def _decode_uu(encoded):
    """Decode uuencoded data (between the 'begin' and 'end' lines)."""
    lines = encoded.splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        index += 1
        if line.startswith(b"begin "):
            mode = line[6:].partition(b" ")[0]
            try:
                int(mode, 8)
            except ValueError:
                continue
            break
    else:
        raise ValueError("`begin` line not found")
    out = []
    for line in lines[index:]:
        if not line:
            raise ValueError("Truncated input")
        if line.strip(b' \t\r\n\f') == b'end':
            break
        try:
            out.append(binascii.a2b_uu(line))
        except binascii.Error:
            # some encoders add garbage after the data; use only the bytes
            # announced by the length character
            count = (((line[0] - 32) & 63) * 4 + 5) // 3
            out.append(binascii.a2b_uu(line[:count]))
    return b''.join(out)


_valid_name = re.compile(r"[\041-\071\073-\176]+\Z")


def _check_header_name(name):
    if not _valid_name.match(name):
        raise ValueError(f"Invalid header name {name!r}")


class Message:
    """A message: headers plus a payload.

    The payload is a str for simple messages or a list of Message objects
    for multipart messages.  Header access is case-insensitive and goes
    through the message's policy.
    """

    def __init__(self, policy=compat32):
        self.policy = policy
        self._headers = []
        self._unixfrom = None
        self._payload = None
        self._charset = None
        self.preamble = self.epilogue = None
        self.defects = []
        self._default_type = 'text/plain'

    def __str__(self):
        return self.as_string()

    def as_string(self, unixfrom=False, maxheaderlen=0, policy=None):
        """Return the whole message flattened as a str."""
        from email.generator import Generator
        if policy is None:
            policy = self.policy
        out = StringIO()
        g = Generator(out, mangle_from_=False, maxheaderlen=maxheaderlen,
                      policy=policy)
        g.flatten(self, unixfrom=unixfrom)
        return out.getvalue()

    def __bytes__(self):
        return self.as_bytes()

    def as_bytes(self, unixfrom=False, policy=None):
        """Return the whole message flattened as bytes."""
        from email.generator import BytesGenerator
        if policy is None:
            policy = self.policy
        out = BytesIO()
        g = BytesGenerator(out, mangle_from_=False, policy=policy)
        g.flatten(self, unixfrom=unixfrom)
        return out.getvalue()

    def is_multipart(self):
        return isinstance(self._payload, list)

    # -- envelope header ---------------------------------------------------

    def set_unixfrom(self, unixfrom):
        self._unixfrom = unixfrom

    def get_unixfrom(self):
        return self._unixfrom

    # -- payload ----------------------------------------------------------

    def attach(self, payload):
        """Add *payload* as a subpart (making the payload a list)."""
        if self._payload is None:
            self._payload = [payload]
            return
        try:
            self._payload.append(payload)
        except AttributeError:
            raise TypeError("Attach is not valid on a message with a"
                            " non-multipart payload")

    def get_payload(self, i=None, decode=False):
        """Return the payload (or its *i*-th part).

        With *decode*, a non-multipart payload is returned as bytes with the
        Content-Transfer-Encoding undone (multiparts give None).
        """
        if self.is_multipart():
            if decode:
                return None
            if i is None:
                return self._payload
            return self._payload[i]
        if i is not None and not isinstance(self._payload, list):
            raise TypeError('Expected list, got %s' % type(self._payload))
        payload = self._payload
        cte = str(self.get('content-transfer-encoding', '')).strip().lower()
        raw = None
        if isinstance(payload, str):
            if utils._has_surrogates(payload):
                raw = payload.encode('ascii', 'surrogateescape')
                if not decode:
                    try:
                        payload = raw.decode(self.get_content_charset('ascii'),
                                             'replace')
                    except LookupError:
                        payload = raw.decode('ascii', 'replace')
            elif decode:
                try:
                    raw = payload.encode('ascii')
                except UnicodeError:
                    raw = payload.encode('raw-unicode-escape')
        if not decode:
            return payload
        if cte == 'quoted-printable':
            return quopri.decodestring(raw)
        if cte == 'base64':
            value, defects = decode_b(b''.join(raw.splitlines()))
            for defect in defects:
                self.policy.handle_defect(self, defect)
            return value
        if cte in ('x-uuencode', 'uuencode', 'uue', 'x-uue'):
            try:
                return _decode_uu(raw)
            except ValueError:
                return raw
        if isinstance(payload, str):
            return raw
        return payload

    def set_payload(self, payload, charset=None):
        """Set the payload; with *charset*, str payloads are encoded and the
        charset is applied (see set_charset())."""
        if hasattr(payload, 'encode'):
            if charset is None:
                self._payload = payload
                return
            if not isinstance(charset, Charset):
                charset = Charset(charset)
            payload = payload.encode(charset.output_charset, 'surrogateescape')
        if hasattr(payload, 'decode'):
            self._payload = payload.decode('ascii', 'surrogateescape')
        else:
            self._payload = payload
        if charset is not None:
            self.set_charset(charset)

    def set_charset(self, charset):
        """Set the charset of the payload, adding MIME-Version,
        Content-Type and Content-Transfer-Encoding headers as needed and
        encoding the payload.  None removes the charset parameter."""
        if charset is None:
            self.del_param('charset')
            self._charset = None
            return
        if not isinstance(charset, Charset):
            charset = Charset(charset)
        self._charset = charset
        if 'MIME-Version' not in self:
            self.add_header('MIME-Version', '1.0')
        if 'Content-Type' not in self:
            self.add_header('Content-Type', 'text/plain',
                            charset=charset.get_output_charset())
        else:
            self.set_param('charset', charset.get_output_charset())
        if charset != charset.get_output_charset():
            self._payload = charset.body_encode(self._payload)
        if 'Content-Transfer-Encoding' not in self:
            cte = charset.get_body_encoding()
            if callable(cte):
                cte(self)
            else:
                payload = self._payload
                if payload:
                    try:
                        payload = payload.encode('ascii', 'surrogateescape')
                    except UnicodeError:
                        payload = payload.encode(charset.output_charset)
                self._payload = charset.body_encode(payload)
                self.add_header('Content-Transfer-Encoding', cte)

    def get_charset(self):
        return self._charset

    # -- mapping interface to the headers -----------------------------------

    def __len__(self):
        return len(self._headers)

    def __getitem__(self, name):
        return self.get(name)

    def __setitem__(self, name, val):
        """Append a header (existing headers of that name are kept)."""
        _check_header_name(name)
        max_count = self.policy.header_max_count(name)
        if max_count:
            lname = name.lower()
            found = sum(1 for k, v in self._headers if k.lower() == lname)
            if found >= max_count:
                raise ValueError("There may be at most {} {} headers "
                                 "in a message".format(max_count, name))
        self._headers.append(self.policy.header_store_parse(name, val))

    def __delitem__(self, name):
        """Delete all headers called *name* (no error if there are none)."""
        lname = name.lower()
        self._headers = [(k, v) for k, v in self._headers
                         if k.lower() != lname]

    def __contains__(self, name):
        lname = name.lower()
        return any(k.lower() == lname for k, v in self._headers)

    def __iter__(self):
        for field, value in self._headers:
            yield field

    def keys(self):
        return [k for k, v in self._headers]

    def values(self):
        return [self.policy.header_fetch_parse(k, v)
                for k, v in self._headers]

    def items(self):
        return [(k, self.policy.header_fetch_parse(k, v))
                for k, v in self._headers]

    def get(self, name, failobj=None):
        """The first header called *name*, or *failobj*."""
        lname = name.lower()
        for k, v in self._headers:
            if k.lower() == lname:
                return self.policy.header_fetch_parse(k, v)
        return failobj

    def set_raw(self, name, value):
        """Store a header without going through the policy."""
        self._headers.append((name, value))

    def raw_items(self):
        """The (name, value) pairs as stored."""
        return iter(self._headers.copy())

    def get_all(self, name, failobj=None):
        """All values of headers called *name*, or *failobj* if none."""
        lname = name.lower()
        values = [self.policy.header_fetch_parse(k, v)
                  for k, v in self._headers if k.lower() == lname]
        return values if values else failobj

    def add_header(self, _name, _value, **_params):
        """Add a header with parameters; underscores in parameter names
        become dashes, a None value gives a bare parameter."""
        parts = []
        for key, value in _params.items():
            key = key.replace('_', '-')
            parts.append(key if value is None else _formatparam(key, value))
        if _value is not None:
            parts.insert(0, _value)
        self[_name] = SEMISPACE.join(parts)

    def replace_header(self, _name, _value):
        """Replace the value of the first header called *_name*, keeping
        its position; KeyError if there is none."""
        lname = _name.lower()
        for index, (k, v) in enumerate(self._headers):
            if k.lower() == lname:
                self._headers[index] = self.policy.header_store_parse(k, _value)
                return
        raise KeyError(_name)

    # -- Content-Type and parameters --------------------------------------------

    def get_content_type(self):
        """The lower-cased maintype/subtype (with defaults applied)."""
        missing = object()
        value = self.get('content-type', missing)
        if value is missing:
            return self.get_default_type()
        ctype = _splitparam(value)[0].lower()
        if ctype.count('/') != 1:
            return 'text/plain'
        return ctype

    def get_content_maintype(self):
        return self.get_content_type().split('/')[0]

    def get_content_subtype(self):
        return self.get_content_type().split('/')[1]

    def get_default_type(self):
        return self._default_type

    def set_default_type(self, ctype):
        """Set the type assumed when there is no Content-Type header."""
        self._default_type = ctype

    def _get_params_preserve(self, failobj, header):
        missing = object()
        value = self.get(header, missing)
        if value is missing:
            return failobj
        params = []
        for item in _parseparam(value):
            name, sep, val = item.partition('=')
            if sep:
                params.append((name.strip(), val.strip()))
            else:
                params.append((item.strip(), ''))
        return utils.decode_params(params)

    def get_params(self, failobj=None, header='content-type', unquote=True):
        """The parameters of *header* as (name, value) pairs; the first pair
        is the main value with an empty value."""
        params = self._get_params_preserve(failobj, header)
        if params is failobj:
            return params
        if unquote:
            return [(k, _unquotevalue(v)) for k, v in params]
        return params

    def get_param(self, param, failobj=None, header='content-type',
                  unquote=True):
        """The value of one parameter of *header* (an RFC 2231 value is a
        (charset, language, value) tuple)."""
        if header not in self:
            return failobj
        lparam = param.lower()
        for k, v in self._get_params_preserve(failobj, header):
            if k.lower() == lparam:
                return _unquotevalue(v) if unquote else v
        return failobj

    def set_param(self, param, value, header='Content-Type', requote=True,
                  charset=None, language='', replace=False):
        """Set (or add) a parameter of *header*."""
        if not isinstance(value, tuple) and charset:
            value = (charset, language, value)
        if header not in self and header.lower() == 'content-type':
            ctype = 'text/plain'
        else:
            ctype = self.get(header)
        if not self.get_param(param, header=header):
            new = _formatparam(param, value, requote)
            ctype = SEMISPACE.join([ctype, new]) if ctype else new
        else:
            pieces = []
            lparam = param.lower()
            for old_param, old_value in self.get_params(header=header,
                                                        unquote=requote):
                if old_param.lower() == lparam:
                    pieces.append(_formatparam(param, value, requote))
                else:
                    pieces.append(_formatparam(old_param, old_value, requote))
            ctype = SEMISPACE.join(pieces)
        if ctype != self.get(header):
            if replace:
                self.replace_header(header, ctype)
            else:
                del self[header]
                self[header] = ctype

    def del_param(self, param, header='content-type', requote=True):
        """Remove a parameter from *header*."""
        if header not in self:
            return
        lparam = param.lower()
        pieces = [_formatparam(p, v, requote)
                  for p, v in self.get_params(header=header, unquote=requote)
                  if p.lower() != lparam]
        new_ctype = SEMISPACE.join(pieces)
        if new_ctype != self.get(header):
            del self[header]
            self[header] = new_ctype

    def set_type(self, type, header='Content-Type', requote=True):
        """Change the main value of *header* (a MIME type), keeping its
        parameters."""
        if type.count('/') != 1:
            raise ValueError
        if header.lower() == 'content-type':
            del self['mime-version']
            self['MIME-Version'] = '1.0'
        if header not in self:
            self[header] = type
            return
        params = self.get_params(header=header, unquote=requote)
        del self[header]
        self[header] = type
        for p, v in params[1:]:
            self.set_param(p, v, header, requote)

    def get_filename(self, failobj=None):
        """The filename parameter of Content-Disposition (or the name
        parameter of Content-Type)."""
        missing = object()
        filename = self.get_param('filename', missing, 'content-disposition')
        if filename is missing:
            filename = self.get_param('name', missing, 'content-type')
        if filename is missing:
            return failobj
        return utils.collapse_rfc2231_value(filename).strip()

    def get_boundary(self, failobj=None):
        """The multipart boundary parameter, or *failobj*."""
        missing = object()
        boundary = self.get_param('boundary', missing)
        if boundary is missing:
            return failobj
        return utils.collapse_rfc2231_value(boundary).rstrip()

    def set_boundary(self, boundary):
        """Set the boundary parameter of the Content-Type header in place;
        HeaderParseError if there is no Content-Type header."""
        missing = object()
        params = self._get_params_preserve(missing, 'content-type')
        if params is missing:
            raise errors.HeaderParseError('No Content-Type header found')
        new_params = []
        found = False
        for key, value in params:
            if key.lower() == 'boundary':
                new_params.append(('boundary', '"%s"' % boundary))
                found = True
            else:
                new_params.append((key, value))
        if not found:
            new_params.append(('boundary', '"%s"' % boundary))
        text = SEMISPACE.join(k if v == '' else '%s=%s' % (k, v)
                              for k, v in new_params)
        self._headers = [
            self.policy.header_store_parse(h, text)
            if h.lower() == 'content-type' else (h, v)
            for h, v in self._headers]

    def get_content_charset(self, failobj=None):
        """The lower-cased charset parameter of Content-Type, or *failobj*."""
        missing = object()
        charset = self.get_param('charset', missing)
        if charset is missing:
            return failobj
        if isinstance(charset, tuple):
            encoding = charset[0] or 'us-ascii'
            try:
                charset = str(charset[2].encode('raw-unicode-escape'), encoding)
            except (LookupError, UnicodeError):
                charset = charset[2]
        try:
            charset.encode('us-ascii')
        except UnicodeError:
            return failobj
        return charset.lower()

    def get_charsets(self, failobj=None):
        """The content charsets of this message and all its subparts."""
        return [part.get_content_charset(failobj) for part in self.walk()]

    def get_content_disposition(self):
        """The lower-cased Content-Disposition value (without parameters),
        or None."""
        value = self.get('content-disposition')
        if value is None:
            return None
        return _splitparam(value)[0].lower()

    from email.iterators import walk


class MIMEPart(Message):
    """A MIME part using the modern API (default policy)."""

    def __init__(self, policy=None):
        if policy is None:
            from email.policy import default
            policy = default
        super().__init__(policy)

    def as_string(self, unixfrom=False, maxheaderlen=None, policy=None):
        if policy is None:
            policy = self.policy
        if maxheaderlen is None:
            maxheaderlen = policy.max_line_length
        return super().as_string(unixfrom, maxheaderlen, policy)

    def __str__(self):
        return self.as_string(policy=self.policy.clone(utf8=True))

    def is_attachment(self):
        disposition = self.get('content-disposition')
        if disposition is None:
            return False
        return disposition.content_disposition == 'attachment'

    def _find_body(self, part, preferencelist):
        # Yield (preference rank, part) candidates for get_body().
        if part.is_attachment():
            return
        maintype, subtype = part.get_content_type().split('/')
        if maintype == 'text':
            if subtype in preferencelist:
                yield (preferencelist.index(subtype), part)
            return
        if maintype != 'multipart' or not self.is_multipart():
            return
        if subtype != 'related':
            for subpart in part.iter_parts():
                yield from self._find_body(subpart, preferencelist)
            return
        if 'related' in preferencelist:
            yield (preferencelist.index('related'), part)
        root = None
        start = part.get_param('start')
        if start:
            for subpart in part.iter_parts():
                if subpart['content-id'] == start:
                    root = subpart
                    break
        if root is None:
            subparts = part.get_payload()
            root = subparts[0] if subparts else None
        if root is not None:
            yield from self._find_body(root, preferencelist)

    def get_body(self, preferencelist=('related', 'html', 'plain')):
        """The best candidate for the body, by *preferencelist* order."""
        best = len(preferencelist)
        body = None
        for rank, part in self._find_body(self, preferencelist):
            if rank < best:
                best = rank
                body = part
                if rank == 0:
                    break
        return body

    _body_types = {('text', 'plain'),
                   ('text', 'html'),
                   ('multipart', 'related'),
                   ('multipart', 'alternative')}

    def iter_attachments(self):
        """The subparts that are not candidates for the body."""
        maintype, subtype = self.get_content_type().split('/')
        if maintype != 'multipart' or subtype == 'alternative':
            return
        payload = self.get_payload()
        try:
            parts = payload.copy()
        except AttributeError:
            return
        if subtype == 'related':
            start = self.get_param('start')
            if start:
                others = [p for p in parts if p.get('content-id') != start]
                if len(others) != len(parts):
                    yield from others
                    return
            yield from parts[1:]
            return
        seen = set()
        for part in parts:
            key = tuple(part.get_content_type().split('/'))
            if (key in self._body_types and not part.is_attachment()
                    and key[1] not in seen):
                seen.add(key[1])
                continue
            yield part

    def iter_parts(self):
        if self.is_multipart():
            yield from self.get_payload()

    def get_content(self, *args, content_manager=None, **kw):
        if content_manager is None:
            content_manager = self.policy.content_manager
        return content_manager.get_content(self, *args, **kw)

    def set_content(self, *args, content_manager=None, **kw):
        if content_manager is None:
            content_manager = self.policy.content_manager
        content_manager.set_content(self, *args, **kw)

    def _make_multipart(self, subtype, disallowed_subtypes, boundary):
        if self.get_content_maintype() == 'multipart':
            existing = self.get_content_subtype()
            if existing in disallowed_subtypes + (subtype,):
                raise ValueError("Cannot convert {} to {}".format(existing,
                                                                  subtype))
        keep = []
        content = []
        for name, value in self._headers:
            if name.lower().startswith('content-'):
                content.append((name, value))
            else:
                keep.append((name, value))
        if content:
            part = type(self)(policy=self.policy)
            part._headers = content
            part._payload = self._payload
            self._payload = [part]
        else:
            self._payload = []
        self._headers = keep
        self['Content-Type'] = 'multipart/' + subtype
        if boundary is not None:
            self.set_param('boundary', boundary)

    def make_related(self, boundary=None):
        self._make_multipart('related', ('alternative', 'mixed'), boundary)

    def make_alternative(self, boundary=None):
        self._make_multipart('alternative', ('mixed',), boundary)

    def make_mixed(self, boundary=None):
        self._make_multipart('mixed', (), boundary)

    def _add_multipart(self, _subtype, *args, _disp=None, **kw):
        if (self.get_content_maintype() != 'multipart'
                or self.get_content_subtype() != _subtype):
            getattr(self, 'make_' + _subtype)()
        part = type(self)(policy=self.policy)
        part.set_content(*args, **kw)
        if _disp and 'content-disposition' not in part:
            part['Content-Disposition'] = _disp
        self.attach(part)

    def add_related(self, *args, **kw):
        self._add_multipart('related', *args, _disp='inline', **kw)

    def add_alternative(self, *args, **kw):
        self._add_multipart('alternative', *args, **kw)

    def add_attachment(self, *args, **kw):
        self._add_multipart('mixed', *args, _disp='attachment', **kw)

    def clear(self):
        self._headers = []
        self._payload = None

    def clear_content(self):
        self._headers = [(n, v) for n, v in self._headers
                         if not n.lower().startswith('content-')]
        self._payload = None


class EmailMessage(MIMEPart):
    """A complete message (adds MIME-Version when content is set)."""

    def set_content(self, *args, **kw):
        super().set_content(*args, **kw)
        if 'MIME-Version' not in self:
            self['MIME-Version'] = '1.0'
