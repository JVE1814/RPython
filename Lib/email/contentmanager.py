"""Content managers: get_content()/set_content() support for MIMEPart."""

import binascii
import email.charset
import email.message
import email.errors
from email import quoprimime


class ContentManager:
    """Dispatches get_content/set_content calls to registered handlers.

    Get handlers are keyed by 'maintype/subtype', 'maintype' or '' (any);
    set handlers by the type of the object (class, qualified name or
    name) or None (any).
    """

    def __init__(self):
        self.get_handlers = {}
        self.set_handlers = {}

    def add_get_handler(self, key, handler):
        self.get_handlers[key] = handler

    def get_content(self, msg, *args, **kw):
        ctype = msg.get_content_type()
        for key in (ctype, msg.get_content_maintype(), ''):
            if key in self.get_handlers:
                return self.get_handlers[key](msg, *args, **kw)
        raise KeyError(ctype)

    def add_set_handler(self, typekey, handler):
        self.set_handlers[typekey] = handler

    def set_content(self, msg, obj, *args, **kw):
        if msg.get_content_maintype() == 'multipart':
            raise TypeError("set_content not valid on multipart")
        handler = self._find_set_handler(msg, obj)
        msg.clear_content()
        handler(msg, obj, *args, **kw)

    def _find_set_handler(self, msg, obj):
        first_name = None
        handlers = self.set_handlers
        for typ in type(obj).__mro__:
            if typ in handlers:
                return handlers[typ]
            qualname = typ.__qualname__
            module = getattr(typ, '__module__', '')
            full = module + '.' + qualname if module else qualname
            if first_name is None:
                first_name = full
            for key in (full, qualname, typ.__name__):
                if key in handlers:
                    return handlers[key]
        if None in handlers:
            return handlers[None]
        raise KeyError(first_name)


raw_data_manager = ContentManager()


def get_text_content(msg, errors='replace'):
    """The payload decoded with its charset (default ASCII)."""
    content = msg.get_payload(decode=True)
    charset = msg.get_param('charset', 'ASCII')
    return content.decode(charset, errors=errors)


raw_data_manager.add_get_handler('text', get_text_content)


def get_non_text_content(msg):
    return msg.get_payload(decode=True)


for _maintype in ('audio', 'image', 'video', 'application'):
    raw_data_manager.add_get_handler(_maintype, get_non_text_content)
del _maintype


def get_message_content(msg):
    return msg.get_payload(0)


for _subtype in ('rfc822', 'external-body'):
    raw_data_manager.add_get_handler('message/' + _subtype, get_message_content)
del _subtype


def get_and_fixup_unknown_message_content(msg):
    # unknown message subtypes are treated as application/octet-stream
    return bytes(msg.get_payload(0))


raw_data_manager.add_get_handler('message',
                                 get_and_fixup_unknown_message_content)


def _prepare_set(msg, maintype, subtype, headers):
    msg['Content-Type'] = '/'.join((maintype, subtype))
    if not headers:
        return
    if not hasattr(headers[0], 'name'):
        policy = msg.policy
        headers = [policy.header_factory(*policy.header_source_parse([h]))
                   for h in headers]
    try:
        for header in headers:
            if header.defects:
                raise header.defects[0]
            msg[header.name] = header
    except email.errors.HeaderDefect as exc:
        raise ValueError("Invalid header: {}".format(
            header.fold(policy=msg.policy))) from exc


def _finalize_set(msg, disposition, filename, cid, params):
    if disposition is None and filename is not None:
        disposition = 'attachment'
    if disposition is not None:
        msg['Content-Disposition'] = disposition
    if filename is not None:
        msg.set_param('filename', filename, header='Content-Disposition',
                      replace=True)
    if cid is not None:
        msg['Content-ID'] = cid
    if params is not None:
        for key, value in params.items():
            msg.set_param(key, value)


def _encode_base64(data, max_line_length):
    per_line = max_line_length // 4 * 3
    return ''.join(binascii.b2a_base64(data[i:i + per_line]).decode('ascii')
                   for i in range(0, len(data), per_line))


def _encode_text(string, charset, cte, policy):
    """Choose (if *cte* is None) and apply a transfer encoding; returns
    (cte, payload)."""
    lines = string.encode(charset).splitlines()
    linesep = policy.linesep.encode('ascii')

    def with_linesep(lines):
        return linesep.join(lines) + linesep

    def with_newline(lines):
        return b'\n'.join(lines) + b'\n'

    if cte is None:
        if max((len(x) for x in lines), default=0) <= policy.max_line_length:
            try:
                return '7bit', with_newline(lines).decode('ascii')
            except UnicodeDecodeError:
                pass
            if policy.cte_type == '8bit':
                return '8bit', with_newline(lines).decode('ascii',
                                                          'surrogateescape')
        # Compare the encodings on (at most) the first ten lines.
        sample = with_linesep(lines[:10])
        sample_qp = quoprimime.body_encode(sample.decode('latin-1'),
                                           policy.max_line_length)
        sample_b64 = binascii.b2a_base64(sample)
        if len(sample_qp) > len(sample_b64):
            cte = 'base64'
        else:
            cte = 'quoted-printable'
            if len(lines) <= 10:
                return cte, sample_qp
    if cte == '7bit':
        data = with_newline(lines).decode('ascii')
    elif cte == '8bit':
        data = with_newline(lines).decode('ascii', 'surrogateescape')
    elif cte == 'quoted-printable':
        data = quoprimime.body_encode(with_newline(lines).decode('latin-1'),
                                      policy.max_line_length)
    elif cte == 'base64':
        data = _encode_base64(with_linesep(lines), policy.max_line_length)
    else:
        raise ValueError("Unknown content transfer encoding {}".format(cte))
    return cte, data


def set_text_content(msg, string, subtype="plain", charset='utf-8', cte=None,
                     disposition=None, filename=None, cid=None,
                     params=None, headers=None):
    _prepare_set(msg, 'text', subtype, headers)
    cte, payload = _encode_text(string, charset, cte, msg.policy)
    msg.set_payload(payload)
    msg.set_param('charset', email.charset.ALIASES.get(charset, charset),
                  replace=True)
    msg['Content-Transfer-Encoding'] = cte
    _finalize_set(msg, disposition, filename, cid, params)


raw_data_manager.add_set_handler(str, set_text_content)


def set_message_content(msg, message, subtype="rfc822", cte=None,
                        disposition=None, filename=None, cid=None,
                        params=None, headers=None):
    if subtype == 'partial':
        raise ValueError("message/partial is not supported for Message objects")
    if subtype == 'rfc822':
        if cte not in (None, '7bit', '8bit', 'binary'):
            raise ValueError(
                "message/rfc822 parts do not support cte={}".format(cte))
        if cte is None:
            cte = '8bit'
    elif subtype == 'external-body':
        if cte not in (None, '7bit'):
            raise ValueError(
                "message/external-body parts do not support cte={}".format(cte))
        cte = '7bit'
    elif cte is None:
        cte = '7bit'
    _prepare_set(msg, 'message', subtype, headers)
    msg.set_payload([message])
    msg['Content-Transfer-Encoding'] = cte
    _finalize_set(msg, disposition, filename, cid, params)


raw_data_manager.add_set_handler(email.message.Message, set_message_content)


def set_bytes_content(msg, data, maintype, subtype, cte='base64',
                      disposition=None, filename=None, cid=None,
                      params=None, headers=None):
    _prepare_set(msg, maintype, subtype, headers)
    if cte == 'base64':
        data = _encode_base64(data, max_line_length=msg.policy.max_line_length)
    elif cte == 'quoted-printable':
        data = binascii.b2a_qp(data, istext=False, header=False,
                               quotetabs=True).decode('ascii')
    elif cte == '7bit':
        data = data.decode('ascii')
    elif cte in ('8bit', 'binary'):
        data = data.decode('ascii', 'surrogateescape')
    msg.set_payload(data)
    msg['Content-Transfer-Encoding'] = cte
    _finalize_set(msg, disposition, filename, cid, params)


for _typ in (bytes, bytearray, memoryview):
    raw_data_manager.add_set_handler(_typ, set_bytes_content)
del _typ
