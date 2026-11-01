"""Turn message object trees back into text or bytes."""

__all__ = ['Generator', 'DecodedGenerator', 'BytesGenerator']

import re
import sys
import time
import random

from copy import deepcopy
from io import StringIO, BytesIO
from email.utils import _has_surrogates
from email.errors import HeaderWriteError

UNDERSCORE = '_'
NL = '\n'

NLCRE = re.compile(r'\r\n|\r|\n')
fcre = re.compile(r'^From ', re.MULTILINE)
# A line break that is not followed by folding white space.
NEWLINE_WITHOUT_FWSP = re.compile(r'\r\n[^ \t]|\r[^ \n\t]|\n[^ \t]')


class Generator:
    """Write a message tree to a text file object."""

    def __init__(self, outfp, mangle_from_=None, maxheaderlen=None, *,
                 policy=None):
        """*mangle_from_*: escape body lines starting with 'From ' (default
        from the policy); *maxheaderlen* overrides the policy's
        max_line_length; *policy* overrides the message's policy."""
        if mangle_from_ is None:
            mangle_from_ = True if policy is None else policy.mangle_from_
        self._fp = outfp
        self._mangle_from_ = mangle_from_
        self.maxheaderlen = maxheaderlen
        self.policy = policy

    def write(self, s):
        self._fp.write(s)

    def flatten(self, msg, unixfrom=False, linesep=None):
        """Write *msg* (and its subparts); with *unixfrom* the envelope
        header is written first."""
        policy = msg.policy if self.policy is None else self.policy
        if linesep is not None:
            policy = policy.clone(linesep=linesep)
        if self.maxheaderlen is not None:
            policy = policy.clone(max_line_length=self.maxheaderlen)
        self._NL = policy.linesep
        self._encoded_NL = self._encode(self._NL)
        self._EMPTY = ''
        self._encoded_EMPTY = self._encode(self._EMPTY)
        saved = (self.policy, msg.policy)
        try:
            self.policy = policy
            msg.policy = policy
            if unixfrom:
                envelope = msg.get_unixfrom()
                if not envelope:
                    envelope = 'From nobody ' + time.ctime(time.time())
                self.write(envelope + self._NL)
            self._write(msg)
        finally:
            self.policy, msg.policy = saved

    def clone(self, fp):
        """A generator like this one, writing to *fp*."""
        return self.__class__(fp, self._mangle_from_, None, policy=self.policy)

    # -- helpers overridden by BytesGenerator -------------------------------------

    def _new_buffer(self):
        return StringIO()

    def _encode(self, s):
        return s

    def _write_lines(self, lines):
        # Write text, normalising its line endings to the policy's linesep.
        if not lines:
            return
        pieces = NLCRE.split(lines)
        for piece in pieces[:-1]:
            self.write(piece)
            self.write(self._NL)
        if pieces[-1]:
            self.write(pieces[-1])

    # -- message writing ----------------------------------------------------------

    def _write(self, msg):
        # The body is generated first (into a buffer) because writing it
        # may change headers, e.g. a newly chosen multipart boundary.
        outer = self._fp
        try:
            self._munge_cte = None
            self._fp = body = self._new_buffer()
            self._dispatch(msg)
        finally:
            self._fp = outer
            munge_cte = self._munge_cte
            del self._munge_cte
        if munge_cte:
            msg = deepcopy(msg)
            if msg.get('content-transfer-encoding') is None:
                msg['Content-Transfer-Encoding'] = munge_cte[0]
            else:
                msg.replace_header('content-transfer-encoding', munge_cte[0])
            msg.replace_header('content-type', munge_cte[1])
        write_headers = getattr(msg, '_write_headers', None)
        if write_headers is None:
            self._write_headers(msg)
        else:
            write_headers(self)
        self._fp.write(body.getvalue())

    def _dispatch(self, msg):
        # _handle_<maintype>_<subtype>, else _handle_<maintype>, else the
        # plain body writer.
        main = msg.get_content_maintype()
        sub = msg.get_content_subtype()
        specific = (main + UNDERSCORE + sub).replace('-', '_')
        handler = getattr(self, '_handle_' + specific, None)
        if handler is None:
            handler = getattr(self, '_handle_' + main.replace('-', '_'), None)
            if handler is None:
                handler = self._writeBody
        handler(msg)

    def _check_folded(self, folded):
        if not self.policy.verify_generated_headers:
            return
        linesep = self.policy.linesep
        if not folded.endswith(linesep):
            raise HeaderWriteError(
                f'folded header does not end with {linesep!r}: {folded!r}')
        if NEWLINE_WITHOUT_FWSP.search(folded.removesuffix(linesep)):
            raise HeaderWriteError(
                f'folded header contains newline: {folded!r}')

    def _write_headers(self, msg):
        for name, value in msg.raw_items():
            folded = self.policy.fold(name, value)
            self._check_folded(folded)
            self.write(folded)
        self.write(self._NL)

    def _handle_text(self, msg):
        payload = msg.get_payload()
        if payload is None:
            return
        if not isinstance(payload, str):
            raise TypeError('string payload expected: %s' % type(payload))
        if _has_surrogates(msg._payload):
            charset = msg.get_param('charset')
            if charset is not None:
                # Re-encode undecodable bytes for output (on a copy).
                msg = deepcopy(msg)
                del msg['content-transfer-encoding']
                msg.set_payload(msg._payload, charset)
                payload = msg.get_payload()
                self._munge_cte = (msg['content-transfer-encoding'],
                                   msg['content-type'])
        if self._mangle_from_:
            payload = fcre.sub('>From ', payload)
        self._write_lines(payload)

    _writeBody = _handle_text

    def _flatten_subpart(self, part):
        buffer = self._new_buffer()
        self.clone(buffer).flatten(part, unixfrom=False, linesep=self._NL)
        return buffer.getvalue()

    def _write_framing_text(self, text):
        # preamble or epilogue
        if self._mangle_from_:
            text = fcre.sub('>From ', text)
        self._write_lines(text)

    def _handle_multipart(self, msg):
        subparts = msg.get_payload()
        if subparts is None:
            subparts = []
        elif isinstance(subparts, str):
            # e.g. no start boundary was found when parsing
            self.write(subparts)
            return
        elif not isinstance(subparts, list):
            subparts = [subparts]
        texts = [self._flatten_subpart(part) for part in subparts]
        boundary = msg.get_boundary()
        if not boundary:
            boundary = self._make_boundary(self._encoded_NL.join(texts))
            msg.set_boundary(boundary)
        if msg.preamble is not None:
            self._write_framing_text(msg.preamble)
            self.write(self._NL)
        self.write('--' + boundary + self._NL)
        for index, text in enumerate(texts):
            if index:
                self.write(self._NL + '--' + boundary + self._NL)
            self._fp.write(text)
        self.write(self._NL + '--' + boundary + '--' + self._NL)
        if msg.epilogue is not None:
            self._write_framing_text(msg.epilogue)

    def _handle_multipart_signed(self, msg):
        # Signed content must not be refolded.
        saved = self.policy
        self.policy = saved.clone(max_line_length=0)
        try:
            self._handle_multipart(msg)
        finally:
            self.policy = saved

    def _handle_message_delivery_status(self, msg):
        # Header blocks separated by one blank line (no blank line at the
        # end).
        blocks = []
        for part in msg.get_payload():
            text = self._flatten_subpart(part)
            lines = text.split(self._encoded_NL)
            if lines and lines[-1] == self._encoded_EMPTY:
                blocks.append(self._encoded_NL.join(lines[:-1]))
            else:
                blocks.append(text)
        self._fp.write(self._encoded_NL.join(blocks))

    def _handle_message(self, msg):
        payload = msg._payload
        if isinstance(payload, list):
            payload = self._flatten_subpart(msg.get_payload(0))
        else:
            # a message/rfc822 body read with the HeaderParser
            payload = self._encode(payload)
        self._fp.write(payload)

    @classmethod
    def _make_boundary(cls, text=None):
        """A random boundary that does not occur as a boundary line in
        *text*."""
        token = random.randrange(sys.maxsize)
        boundary = ('=' * 15) + (_fmt % token) + '=='
        if text is None:
            return boundary
        candidate = boundary
        counter = 0
        while True:
            pattern = cls._compile_re('^--' + re.escape(candidate) + '(--)?$',
                                      re.MULTILINE)
            if not pattern.search(text):
                return candidate
            candidate = boundary + '.' + str(counter)
            counter += 1

    @classmethod
    def _compile_re(cls, s, flags):
        return re.compile(s, flags)


class BytesGenerator(Generator):
    """Write a message tree to a binary file object.

    Payloads containing undecodable bytes (surrogate escapes) are written
    back as the original bytes unless the policy's cte_type is '7bit'.
    """

    def write(self, s):
        self._fp.write(s.encode('ascii', 'surrogateescape'))

    def _new_buffer(self):
        return BytesIO()

    def _encode(self, s):
        return s.encode('ascii')

    def _write_headers(self, msg):
        for name, value in msg.raw_items():
            folded = self.policy.fold_binary(name, value)
            if self.policy.verify_generated_headers:
                self._check_folded(folded.decode('ascii', 'surrogateescape'))
            self._fp.write(folded)
        self.write(self._NL)

    def _handle_text(self, msg):
        if msg._payload is None:
            return
        if _has_surrogates(msg._payload) and not self.policy.cte_type == '7bit':
            if self._mangle_from_:
                msg._payload = fcre.sub(">From ", msg._payload)
            self._write_lines(msg._payload)
        else:
            super()._handle_text(msg)

    _writeBody = _handle_text

    @classmethod
    def _compile_re(cls, s, flags):
        return re.compile(s.encode('ascii'), flags)


_FMT = '[Non-text (%(type)s) part of message omitted, filename %(filename)s]'


class DecodedGenerator(Generator):
    """Write the text parts of a message, replacing other parts by a line
    produced from *fmt* (keys: type, maintype, subtype, filename,
    description, encoding)."""

    def __init__(self, outfp, mangle_from_=None, maxheaderlen=None, fmt=None,
                 *, policy=None):
        Generator.__init__(self, outfp, mangle_from_, maxheaderlen,
                           policy=policy)
        self._fmt = _FMT if fmt is None else fmt

    def _dispatch(self, msg):
        for part in msg.walk():
            maintype = part.get_content_maintype()
            if maintype == 'text':
                print(part.get_payload(decode=False), file=self)
            elif maintype == 'multipart':
                continue
            else:
                print(self._fmt % {
                    'type': part.get_content_type(),
                    'maintype': part.get_content_maintype(),
                    'subtype': part.get_content_subtype(),
                    'filename': part.get_filename('[no filename]'),
                    'description': part.get('Content-Description',
                                            '[no description]'),
                    'encoding': part.get('Content-Transfer-Encoding',
                                         '[no encoding]'),
                }, file=self)


_width = len(repr(sys.maxsize - 1))
_fmt = '%%0%dd' % _width

_make_boundary = Generator._make_boundary
