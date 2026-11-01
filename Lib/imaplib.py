"""IMAP4 client (RFC 3501, with IDLE, ENABLE/UTF8=ACCEPT and the ACL,
QUOTA, ANNOTATION, SORT and THREAD extensions).

    M = IMAP4(host)            # or IMAP4_SSL / IMAP4_stream
    M.login(user, password)
    M.select('INBOX')
    typ, data = M.search(None, 'ALL')
    ...
    M.logout()

Every command method returns a (type, data) pair: *type* is the server's
completion status ('OK', 'NO', ...) and *data* a list of response items.
An item is bytes, or a (header, literal) tuple of bytes when the server
sent a counted literal.
"""

import binascii
import calendar
import errno
import random
import re
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone, timedelta
from io import DEFAULT_BUFFER_SIZE

try:
    import ssl
    HAVE_SSL = True
except ImportError:
    HAVE_SSL = False

__all__ = ["IMAP4", "IMAP4_stream", "Internaldate2tuple",
           "Int2AP", "ParseFlags", "Time2Internaldate"]
if HAVE_SSL:
    __all__.append("IMAP4_SSL")

CRLF = b'\r\n'
Debug = 0
IMAP4_PORT = 143
IMAP4_SSL_PORT = 993
AllowedVersions = ('IMAP4REV1', 'IMAP4')

# Longest response line accepted from a server.
_MAXLINE = 1000000

_ALL = ('NONAUTH', 'AUTH', 'SELECTED', 'LOGOUT')
_AUTH_OR_SELECTED = ('AUTH', 'SELECTED')

# Command name -> states in which it may be issued.
Commands = {
    'APPEND': _AUTH_OR_SELECTED,
    'AUTHENTICATE': ('NONAUTH',),
    'CAPABILITY': _ALL,
    'CHECK': ('SELECTED',),
    'CLOSE': ('SELECTED',),
    'COPY': ('SELECTED',),
    'CREATE': _AUTH_OR_SELECTED,
    'DELETE': _AUTH_OR_SELECTED,
    'DELETEACL': _AUTH_OR_SELECTED,
    'ENABLE': ('AUTH',),
    'EXAMINE': _AUTH_OR_SELECTED,
    'EXPUNGE': ('SELECTED',),
    'FETCH': ('SELECTED',),
    'GETACL': _AUTH_OR_SELECTED,
    'GETANNOTATION': _AUTH_OR_SELECTED,
    'GETQUOTA': _AUTH_OR_SELECTED,
    'GETQUOTAROOT': _AUTH_OR_SELECTED,
    'IDLE': _AUTH_OR_SELECTED,
    'LIST': _AUTH_OR_SELECTED,
    'LOGIN': ('NONAUTH',),
    'LOGOUT': _ALL,
    'LSUB': _AUTH_OR_SELECTED,
    'MOVE': ('SELECTED',),
    'MYRIGHTS': _AUTH_OR_SELECTED,
    'NAMESPACE': _AUTH_OR_SELECTED,
    'NOOP': _ALL,
    'PARTIAL': ('SELECTED',),
    'PROXYAUTH': ('AUTH',),
    'RENAME': _AUTH_OR_SELECTED,
    'SEARCH': ('SELECTED',),
    'SELECT': _AUTH_OR_SELECTED,
    'SETACL': _AUTH_OR_SELECTED,
    'SETANNOTATION': _AUTH_OR_SELECTED,
    'SETQUOTA': _AUTH_OR_SELECTED,
    'SORT': ('SELECTED',),
    'STARTTLS': ('NONAUTH',),
    'STATUS': _AUTH_OR_SELECTED,
    'STORE': ('SELECTED',),
    'SUBSCRIBE': _AUTH_OR_SELECTED,
    'THREAD': ('SELECTED',),
    'UID': ('SELECTED',),
    'UNSELECT': ('SELECTED',),
    'UNSUBSCRIBE': _AUTH_OR_SELECTED,
}

# UID sub-commands and the untagged response that carries their result.
_UID_RESULT = {
    'COPY': 'FETCH', 'MOVE': 'FETCH', 'FETCH': 'FETCH', 'STORE': 'FETCH',
    'EXPUNGE': 'FETCH', 'SEARCH': 'SEARCH', 'SORT': 'SORT',
    'THREAD': 'THREAD',
}

_RESP_CODE = re.compile(rb'\[(?P<code>[A-Z-]+)(?: (?P<arg>.*))?\]')
_LITERAL_AT_END = re.compile(rb'\{(?P<size>\d+)\}$')
_FLAGS = re.compile(rb'.*FLAGS \((?P<flags>[^)]*)\)')
_INTERNALDATE = re.compile(
    rb'.*INTERNALDATE "'
    rb'(?P<day>[ 0123][0-9])-(?P<mon>[A-Z][a-z][a-z])-(?P<year>[0-9]{4})'
    rb' (?P<hour>[0-9]{2}):(?P<min>[0-9]{2}):(?P<sec>[0-9]{2})'
    rb' (?P<zonen>[-+])(?P<zoneh>[0-9]{2})(?P<zonem>[0-9]{2})"')
_QUOTED_INTERNALDATE = re.compile(r'^"[^"]*"$')

_MONTHS = ('Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
           'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec')
_MONTH_NUMBER = {m.encode(): i + 1 for i, m in enumerate(_MONTHS)}

# Characters that force an astring to be sent quoted: SP, CTL, the quoted
# specials, the list wildcards and the other atom specials ("(", ")", "{").
_ATOM_SPECIALS = frozenset(b' (){%*"\\') | frozenset(range(0x20)) | {0x7f}
_FORBIDDEN = frozenset(b'\0\r\n')


class _IdleTimeout(Exception):
    """Internal: no complete line arrived before an IDLE deadline."""


class IMAP4:
    """An IMAP4 client connection.

    IMAP4(host='', port=IMAP4_PORT, timeout=None)

    The server's greeting is read and its capabilities determined on
    connection.  Unless documented otherwise, command arguments that are
    mailbox names or similar strings are quoted when the protocol requires.
    """

    class error(Exception):
        """Command failed or protocol violation."""

    class abort(error):
        """The connection is unusable (server error, EOF); reconnect."""

    class readonly(abort):
        """The mailbox status changed to read-only."""

    def __init__(self, host='', port=IMAP4_PORT, timeout=None):
        self.debug = Debug
        self.state = 'LOGOUT'
        self.literal = None
        self.tagged_commands = {}
        self.untagged_responses = {}
        self.continuation_response = ''
        self.is_readonly = False
        self.tagnum = 0
        self._tls_established = False
        self._encoding = 'ascii'
        self._rbuf = bytearray()
        self._pending = []
        self._idle_queue = None
        self.tagpre = Int2AP(random.randint(4096, 65535))
        self.open(host, port, timeout)
        try:
            self._connect()
        except Exception:
            try:
                self.shutdown()
            except OSError:
                pass
            raise

    # -- connection set-up --------------------------------------------------

    def _connect(self):
        self.welcome = self._read_greeting()
        if 'CAPABILITY' in self.untagged_responses:
            caps = self.untagged_responses.pop('CAPABILITY')[-1]
            self._set_capabilities(caps)
        else:
            self._get_capabilities()
        if self.debug >= 3:
            self._mesg('CAPABILITIES: %r' % (self.capabilities,))
        for version in AllowedVersions:
            if version in self.capabilities:
                self.PROTOCOL_VERSION = version
                return
        raise self.error('server not IMAP4 compliant')

    def _read_greeting(self):
        kind, line = self._read_response()
        if kind == 'untagged' and line[0] in ('OK', 'PREAUTH'):
            self.state = 'AUTH' if line[0] == 'PREAUTH' else 'NONAUTH'
            self.untagged_responses.pop(line[0], None)
            return line[2]
        raise self.error('invalid greeting: %s'
                         % line[2].decode('ascii', 'replace'))

    def _set_capabilities(self, data):
        text = data.decode('ascii', 'replace') if isinstance(data, bytes) else data
        self.capabilities = tuple(text.upper().split())

    def _get_capabilities(self):
        typ, dat = self.capability()
        if dat == [None]:
            raise self.error('no CAPABILITY response from server')
        self._set_capabilities(dat[-1])

    def _refresh_capabilities(self):
        """After authentication the server may advertise more."""
        caps = self.untagged_responses.get('CAPABILITY')
        if caps:
            self._set_capabilities(caps[-1])
        else:
            self._get_capabilities()

    def _create_socket(self, timeout):
        if timeout is not None and not timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        host = self.host or None
        address = (host, self.port)
        if timeout is not None:
            return socket.create_connection(address, timeout)
        return socket.create_connection(address)

    def open(self, host='', port=IMAP4_PORT, timeout=None):
        """Connect to host:port (called by the constructor)."""
        self.host = host
        self.port = port
        self.sock = self._create_socket(timeout)
        self._imaplib_file = self.sock.makefile('rb')

    @property
    def file(self):
        import warnings
        warnings.warn("IMAP4.file is unsupported, can cause errors, "
                      "and may be removed.", DeprecationWarning, stacklevel=2)
        return self._imaplib_file

    @file.setter
    def file(self, value):
        import warnings
        warnings.warn("IMAP4.file is unsupported, can cause errors, "
                      "and may be removed.", DeprecationWarning, stacklevel=2)
        self._imaplib_file = value

    def __getattr__(self, attr):
        # Upper-case command names are aliases of the methods.
        if attr in Commands:
            return getattr(self, attr.lower())
        raise AttributeError("'%s' object has no attribute '%s'"
                             % (type(self).__name__, attr))

    def __enter__(self):
        return self

    def __exit__(self, *args):
        if self.state == 'LOGOUT':
            return
        try:
            self.logout()
        except OSError:
            pass

    @property
    def utf8_enabled(self):
        """True once UTF8=ACCEPT has been enabled."""
        return self._encoding == 'utf-8'

    # -- raw I/O --------------------------------------------------------------

    def _recv_more(self, deadline=None):
        """Append received bytes to the read buffer; False at EOF."""
        if deadline is not None and not self._wait_readable(deadline):
            raise _IdleTimeout
        chunk = self.sock.recv(DEFAULT_BUFFER_SIZE)
        if not chunk:
            return False
        self._rbuf += chunk
        return True

    def _wait_readable(self, deadline):
        pending = getattr(self.sock, 'pending', None)
        if pending is not None and pending() > 0:
            return True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        import select
        ready, _, _ = select.select([self.sock], [], [], remaining)
        return bool(ready)

    def _take_line(self, deadline=None):
        buf = self._rbuf
        start = 0
        while True:
            end = buf.find(b'\n', start)
            if end >= 0:
                line = bytes(buf[:end + 1])
                del buf[:end + 1]
                break
            if len(buf) > _MAXLINE:
                raise self.error('got more than %d bytes' % _MAXLINE)
            start = len(buf)
            if not self._recv_more(deadline):
                line = bytes(buf)
                buf.clear()
                break
        if len(line) > _MAXLINE:
            raise self.error('got more than %d bytes' % _MAXLINE)
        return line

    def read(self, size):
        """Read up to *size* bytes (fewer only at end of file)."""
        buf = self._rbuf
        while len(buf) < size:
            if not self._recv_more():
                break
        data = bytes(buf[:size])
        del buf[:size]
        return data

    def readline(self):
        """Read one line, including its terminator."""
        return self._take_line()

    def send(self, data):
        """Send bytes to the server."""
        self.sock.sendall(data)

    def shutdown(self):
        """Close the connection."""
        try:
            self._imaplib_file.close()
        except Exception:
            pass
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError as exc:
            # The server may already have closed its side.
            if (exc.errno != errno.ENOTCONN
                    and getattr(exc, 'winerror', 0) != 10022):
                raise
        finally:
            self.sock.close()

    def socket(self):
        """The socket used for the connection."""
        return self.sock

    # -- response reading -----------------------------------------------------

    def _get_line(self, deadline=None):
        if self._pending:
            return self._pending.pop()
        if deadline is None:
            line = self.readline()
        else:
            line = self._take_line(deadline)
        if not line:
            raise self.abort('socket error: EOF')
        if not line.endswith(CRLF):
            raise self.abort('socket error: unterminated line: %r' % line)
        line = line[:-2]
        if self.debug >= 4:
            self._mesg('< %r' % line)
        return line

    def _unread_line(self, line):
        self._pending.append(line)

    def _read_literal(self, size):
        data = b''
        # read in bounded pieces: a bogus size must not allocate memory
        while len(data) < size:
            piece = self.read(min(size - len(data), 1 << 20))
            if not piece:
                break
            data += piece
        if len(data) != size:
            raise self.abort('socket error: EOF while reading a literal '
                             'of %d bytes' % size)
        return data

    def _is_response_start(self, line):
        return (line.startswith(b'* ') or line.startswith(b'+')
                or line.startswith(self.tagpre))

    def _collect_literals(self, text):
        """Items for a response whose text may end with literal markers."""
        items = []
        while True:
            m = _LITERAL_AT_END.search(text)
            if m is None:
                items.append(text)
                return items
            literal = self._read_literal(int(m.group('size')))
            items.append((text, literal))
            text = self._get_line()
            if text == b'':
                # Either the response really ends with the literal, or a
                # buggy server sent a blank line before the rest of it.
                following = self._get_line()
                if following == b'' or self._is_response_start(following):
                    self._unread_line(following)
                else:
                    text = following

    def _store_untagged(self, typ, items):
        self.untagged_responses.setdefault(typ, []).extend(items)
        if self._idle_queue is not None:
            self._idle_queue.append((typ, items))

    def _store_code(self, text):
        m = _RESP_CODE.match(text)
        if m is not None:
            code = m.group('code').decode('ascii')
            self.untagged_responses.setdefault(code, []).append(
                m.group('arg') or b'')

    def _read_response(self, deadline=None):
        """Read one complete response.

        Returns ('tagged', (tag, typ, data)), ('untagged', (typ, items,
        text)) or ('continuation', text).
        """
        line = self._get_line(deadline)
        while line == b'':
            line = self._get_line(deadline)
        if line.startswith(b'+'):
            text = line[2:] if line.startswith(b'+ ') else line[1:]
            self.continuation_response = text
            return 'continuation', (None, [text], line)
        if line.startswith(b'* '):
            first, _, rest = line[2:].partition(b' ')
            if first.isdigit():
                name, _, tail = rest.partition(b' ')
                text = first + b' ' + tail if tail else first
            else:
                name, text = first, rest
            typ = name.decode('ascii', 'replace').upper()
            if typ in ('OK', 'NO', 'BAD', 'BYE', 'PREAUTH'):
                self._store_code(text)
            items = self._collect_literals(text)
            self._store_untagged(typ, items)
            return 'untagged', (typ, items, line)
        tag, _, rest = line.partition(b' ')
        if tag not in self.tagged_commands:
            raise self.abort('unexpected response: %r' % line)
        name, _, text = rest.partition(b' ')
        typ = name.decode('ascii', 'replace').upper()
        if typ not in ('OK', 'NO', 'BAD'):
            raise self.abort('unexpected tagged response: %r' % line)
        self._store_code(text)
        items = self._collect_literals(text)
        self.tagged_commands[tag] = (typ, items)
        return 'tagged', (tag, typ, items)

    # -- commands -------------------------------------------------------------

    def _new_tag(self):
        tag = self.tagpre + str(self.tagnum).encode('ascii')
        self.tagnum += 1
        self.tagged_commands[tag] = None
        return tag

    def _check_state(self, name):
        states = Commands.get(name)
        if states is None:
            raise self.error('unknown IMAP4 command: %s' % name)
        if self.state not in states:
            self.literal = None
            raise self.error('command %s illegal in state %s, '
                             'only allowed in states %s'
                             % (name, self.state, ', '.join(states)))

    def _encode(self, arg):
        if isinstance(arg, str):
            return arg.encode(self._encoding)
        return arg

    def _send_command(self, name, *args, literal=None, literal_end=CRLF):
        """Send a command; returns its tag once the server accepted it
        (and any literal or authentication exchange is done)."""
        self._check_state(name)
        for typ in ('OK', 'NO', 'BAD'):
            self.untagged_responses.pop(typ, None)
        if 'READ-ONLY' in self.untagged_responses and not self.is_readonly:
            raise self.readonly('mailbox status changed to READ-ONLY')
        parts = [name.encode('ascii')]
        for arg in args:
            if arg is not None:
                data = self._encode(arg)
                if _FORBIDDEN.intersection(data):
                    # a line break would end the command early
                    raise ValueError('NUL, CR and LF are not allowed in '
                                     'IMAP arguments: %r' % (arg,))
                parts.append(data)
        tag = self._new_tag()
        parts.insert(0, tag)
        if literal is not None and not callable(literal):
            if self.utf8_enabled:
                parts.append(b'UTF8 (~{%d}' % len(literal))
                literal_end = b')' + CRLF
            else:
                parts.append(b'{%d}' % len(literal))
        line = b' '.join(parts)
        if self.debug >= 4:
            self._mesg('> %r' % line)
        self.send(line + CRLF)
        if literal is None:
            return tag
        while True:
            # wait for the server's go-ahead (or a refusal)
            while True:
                kind, _ = self._read_response()
                if kind == 'continuation':
                    break
                if self.tagged_commands.get(tag) is not None:
                    return tag
            if callable(literal):
                answer = literal(self.continuation_response)
                self.send(answer + CRLF)
                continue
            self.send(literal)
            self.send(literal_end)
            return tag

    def _await_completion(self, name, tag, expect_bye=False):
        while self.tagged_commands.get(tag) is None:
            kind, value = self._read_response()
            if (kind == 'untagged' and value[0] == 'BYE'
                    and not expect_bye):
                raise self.abort(value[1][-1].decode('ascii', 'replace'))
        typ, data = self.tagged_commands.pop(tag)
        if typ == 'BAD':
            raise self.error('%s command error: %s %s' % (name, typ, data))
        return typ, data

    def _simple_command(self, name, *args):
        tag = self._send_command(name, *args)
        return self._await_completion(name, tag)

    def _untagged_response(self, typ, data, name):
        if typ == 'NO':
            return typ, data
        if name not in self.untagged_responses:
            return typ, [None]
        return typ, self.untagged_responses.pop(name)

    # -- argument quoting -----------------------------------------------------

    def _as_bytes(self, arg):
        if isinstance(arg, str):
            return arg.encode(self._encoding)
        return bytes(arg)

    @staticmethod
    def _is_quoted(data):
        """True if *data* is one complete quoted string."""
        if len(data) < 2 or data[:1] != b'"' or data[-1:] != b'"':
            return False
        i, end = 1, len(data) - 1
        while i < end:
            c = data[i]
            if c == 0x5c:          # backslash escapes the next byte
                i += 2
                continue
            if c == 0x22:
                return False
            i += 1
        return i == end

    @staticmethod
    def _quoted(data):
        return b'"' + data.replace(b'\\', b'\\\\').replace(b'"', b'\\"') + b'"'

    def _quote_if_needed(self, arg, allowed=frozenset()):
        data = self._as_bytes(arg)
        if self._is_quoted(data):
            return data
        if (not data or any(b >= 0x80 for b in data)
                or (_ATOM_SPECIALS - allowed).intersection(data)):
            return self._quoted(data)
        return data

    def _astring(self, arg):
        """An astring argument: an atom if possible, else quoted."""
        return self._quote_if_needed(arg)

    def _list_mailbox(self, arg):
        """A LIST/LSUB pattern: like an astring, but wildcards stay bare."""
        return self._quote_if_needed(arg, frozenset(b'%*'))

    def _quote(self, arg):
        """Always quote (used for passwords)."""
        data = self._as_bytes(arg)
        return self._quoted(data)

    @staticmethod
    def _parenthesized(arg):
        if isinstance(arg, bytes):
            if arg[:1] == b'(' and arg[-1:] == b')':
                return arg
            return b'(' + arg + b')'
        if arg[:1] == '(' and arg[-1:] == ')':
            return arg
        return '(' + arg + ')'

    # -- public commands ------------------------------------------------------

    def append(self, mailbox, flags, date_time, message):
        """Append *message* (bytes) to *mailbox* (default 'INBOX').

        Bare CR and LF line ends in the message are converted to CRLF.
        """
        mailbox = self._astring(mailbox if mailbox else 'INBOX')
        if flags:
            flags = self._parenthesized(flags)
        else:
            flags = None
        date = Time2Internaldate(date_time) if date_time else None
        literal = re.sub(br'\r\n|\r|\n', CRLF, message)
        tag = self._send_command('APPEND', mailbox, flags, date,
                                 literal=literal)
        return self._await_completion('APPEND', tag)

    def authenticate(self, mechanism, authobject):
        """Authenticate with SASL *mechanism*.

        *authobject(challenge)* is called with each decoded server
        challenge and returns the response (bytes or str), or None to
        abort the exchange.
        """
        mech = mechanism.upper()

        def respond(challenge):
            try:
                decoded = binascii.a2b_base64(challenge)
            except binascii.Error:
                decoded = b''
            answer = authobject(decoded)
            if answer is None:
                return b'*'
            if isinstance(answer, str):
                answer = answer.encode('utf-8')
            return binascii.b2a_base64(answer, newline=False)

        self.untagged_responses.pop('CAPABILITY', None)
        tag = self._send_command('AUTHENTICATE', mech, literal=respond)
        typ, dat = self._await_completion('AUTHENTICATE', tag)
        if typ != 'OK':
            raise self.error(dat[-1].decode('utf-8', 'replace'))
        self.state = 'AUTH'
        self._refresh_capabilities()
        return typ, dat

    def capability(self):
        """Request the server's capabilities."""
        typ, dat = self._simple_command('CAPABILITY')
        return self._untagged_response(typ, dat, 'CAPABILITY')

    def check(self):
        """Checkpoint the selected mailbox."""
        return self._simple_command('CHECK')

    def close(self):
        """Close the selected mailbox (deleted messages are removed)."""
        try:
            return self._simple_command('CLOSE')
        finally:
            self.state = 'AUTH'

    def copy(self, message_set, new_mailbox):
        """Copy messages to *new_mailbox*."""
        return self._simple_command('COPY', message_set,
                                    self._astring(new_mailbox))

    def create(self, mailbox):
        """Create a mailbox."""
        return self._simple_command('CREATE', self._astring(mailbox))

    def delete(self, mailbox):
        """Delete a mailbox."""
        return self._simple_command('DELETE', self._astring(mailbox))

    def deleteacl(self, mailbox, who):
        """Remove the access rights of *who* on *mailbox*."""
        return self._simple_command('DELETEACL', self._astring(mailbox),
                                    self._astring(who))

    def enable(self, capability):
        """Enable a server extension (RFC 5161)."""
        if 'ENABLE' not in self.capabilities:
            raise self.error('Server does not support ENABLE')
        typ, data = self._simple_command('ENABLE', capability)
        if typ == 'OK' and 'UTF8=ACCEPT' in capability.upper():
            self._encoding = 'utf-8'
        return typ, data

    def expunge(self):
        """Permanently remove deleted messages; returns their numbers."""
        typ, dat = self._simple_command('EXPUNGE')
        return self._untagged_response(typ, dat, 'EXPUNGE')

    def fetch(self, message_set, message_parts):
        """Fetch data items of the messages in *message_set*."""
        typ, dat = self._simple_command(
            'FETCH', message_set, self._fetch_items(message_parts))
        return self._untagged_response(typ, dat, 'FETCH')

    @classmethod
    def _fetch_items(cls, parts):
        text = parts.decode('ascii') if isinstance(parts, bytes) else parts
        if text.upper() in ('ALL', 'FAST', 'FULL'):
            return parts
        return cls._parenthesized(parts)

    def getacl(self, mailbox):
        """Get the access control list of *mailbox*."""
        typ, dat = self._simple_command('GETACL', self._astring(mailbox))
        return self._untagged_response(typ, dat, 'ACL')

    def getannotation(self, mailbox, entry, attribute):
        """Retrieve annotations (RFC 5464 draft syntax)."""
        typ, dat = self._simple_command('GETANNOTATION',
                                        self._astring(mailbox),
                                        entry, attribute)
        return self._untagged_response(typ, dat, 'ANNOTATION')

    def getquota(self, root):
        """Get the quota *root*'s usage and limits."""
        typ, dat = self._simple_command('GETQUOTA', self._astring(root))
        return self._untagged_response(typ, dat, 'QUOTA')

    def getquotaroot(self, mailbox):
        """Get the quota roots of *mailbox*: [[roots...], [quotas...]]."""
        typ, dat = self._simple_command('GETQUOTAROOT',
                                        self._astring(mailbox))
        typ, quota = self._untagged_response(typ, dat, 'QUOTA')
        typ, roots = self._untagged_response(typ, dat, 'QUOTAROOT')
        return typ, [roots, quota]

    def idle(self, duration=None):
        """Return an Idler: a context manager that keeps the connection in
        IDLE mode and yields (type, data) for each untagged response.
        *duration* bounds the whole idle period in seconds."""
        return Idler(self, duration)

    def list(self, directory='""', pattern='*'):
        """List mailbox names matching *pattern* in *directory*."""
        typ, dat = self._simple_command('LIST', self._astring(directory),
                                        self._list_mailbox(pattern))
        return self._untagged_response(typ, dat, 'LIST')

    def login(self, user, password):
        """Log in with a plain-text password."""
        self.untagged_responses.pop('CAPABILITY', None)
        typ, dat = self._simple_command('LOGIN', self._astring(user),
                                        self._quote(password))
        if typ != 'OK':
            raise self.error(dat[-1])
        self.state = 'AUTH'
        self._refresh_capabilities()
        return typ, dat

    def login_cram_md5(self, user, password):
        """Log in with CRAM-MD5 (the password is not sent in clear)."""
        if isinstance(password, str):
            password = password.encode('utf-8')

        def answer(challenge):
            import hmac
            try:
                digest = hmac.HMAC(password, challenge, 'md5').hexdigest()
            except ValueError:
                raise self.error('CRAM-MD5 authentication is not supported')
            return user + ' ' + digest

        try:
            import hashlib
            hashlib.md5(b'')
        except ValueError:
            raise self.error('CRAM-MD5 authentication is not supported')
        return self.authenticate('CRAM-MD5', answer)

    def logout(self):
        """End the session and close the connection."""
        self.state = 'LOGOUT'
        try:
            tag = self._send_command('LOGOUT')
            typ, dat = self._await_completion('LOGOUT', tag, expect_bye=True)
        except BaseException as exc:
            typ, dat = 'NO', ['%s: %s' % (type(exc), exc)]
        self.shutdown()
        if 'BYE' in self.untagged_responses:
            return 'BYE', self.untagged_responses.pop('BYE')
        return typ, dat

    def lsub(self, directory='""', pattern='*'):
        """List subscribed mailbox names."""
        typ, dat = self._simple_command('LSUB', self._astring(directory),
                                        self._list_mailbox(pattern))
        return self._untagged_response(typ, dat, 'LSUB')

    def myrights(self, mailbox):
        """Show the rights the user has on *mailbox*."""
        typ, dat = self._simple_command('MYRIGHTS', self._astring(mailbox))
        return self._untagged_response(typ, dat, 'MYRIGHTS')

    def namespace(self):
        """Return the IMAP namespaces (RFC 2342)."""
        typ, dat = self._simple_command('NAMESPACE')
        return self._untagged_response(typ, dat, 'NAMESPACE')

    def noop(self):
        """Do nothing (lets the server push pending untagged responses)."""
        if self.debug >= 3:
            self._dump_ur(self.untagged_responses)
        return self._simple_command('NOOP')

    def partial(self, message_num, message_part, start, length):
        """Fetch part of a message (obsolete PARTIAL command)."""
        typ, dat = self._simple_command('PARTIAL', message_num,
                                        message_part, start, length)
        return self._untagged_response(typ, dat, 'FETCH')

    def proxyauth(self, user):
        """Assume the authentication of *user* (privileged)."""
        return self._simple_command('PROXYAUTH', self._astring(user))

    def recent(self):
        """The RECENT count (prodding the server with NOOP if needed)."""
        if 'RECENT' in self.untagged_responses:
            return self._untagged_response('OK', [None], 'RECENT')
        typ, dat = self.noop()
        return self._untagged_response(typ, dat, 'RECENT')

    def rename(self, oldmailbox, newmailbox):
        """Rename a mailbox."""
        return self._simple_command('RENAME', self._astring(oldmailbox),
                                    self._astring(newmailbox))

    def response(self, code):
        """Return (and forget) the untagged data received for *code*."""
        return self._untagged_response(code, [None], code.upper())

    def search(self, charset, *criteria):
        """Search the selected mailbox; returns matching message numbers."""
        if charset:
            if self.utf8_enabled:
                raise self.error('Non-None charset not valid in UTF8 mode')
            typ, dat = self._simple_command('SEARCH', 'CHARSET',
                                            self._astring(charset), *criteria)
        else:
            typ, dat = self._simple_command('SEARCH', *criteria)
        return self._untagged_response(typ, dat, 'SEARCH')

    def select(self, mailbox='INBOX', readonly=False):
        """Select a mailbox (EXAMINE it if *readonly*); returns the
        message count."""
        self.untagged_responses = {}
        self.is_readonly = readonly
        name = 'EXAMINE' if readonly else 'SELECT'
        typ, dat = self._simple_command(name, self._astring(mailbox))
        if typ != 'OK':
            self.state = 'AUTH'
            return typ, dat
        self.state = 'SELECTED'
        if 'READ-ONLY' in self.untagged_responses and not readonly:
            raise self.readonly('%r is not writable' % (mailbox,))
        return typ, self.untagged_responses.get('EXISTS', [None])

    def setacl(self, mailbox, who, what):
        """Set the rights of *who* on *mailbox*."""
        return self._simple_command('SETACL', self._astring(mailbox),
                                    self._astring(who), self._astring(what))

    def setannotation(self, *args):
        """Set annotations: setannotation(mailbox, entry, attributes...)."""
        if args:
            args = (self._astring(args[0]),) + args[1:]
        typ, dat = self._simple_command('SETANNOTATION', *args)
        return self._untagged_response(typ, dat, 'ANNOTATION')

    def setquota(self, root, limits):
        """Set the resource limits of a quota root."""
        typ, dat = self._simple_command('SETQUOTA', self._astring(root),
                                        self._parenthesized(limits))
        return self._untagged_response(typ, dat, 'QUOTA')

    def sort(self, sort_criteria, charset, *search_criteria):
        """Sorted search (SORT extension)."""
        typ, dat = self._simple_command('SORT',
                                        self._parenthesized(sort_criteria),
                                        self._astring(charset),
                                        *search_criteria)
        return self._untagged_response(typ, dat, 'SORT')

    def starttls(self, ssl_context=None):
        """Switch the connection to TLS (STARTTLS)."""
        if not HAVE_SSL:
            raise self.error('SSL support missing')
        if self._tls_established:
            raise self.abort('TLS session already established')
        if 'STARTTLS' not in self.capabilities:
            raise self.abort('TLS not supported by server')
        if ssl_context is None:
            ssl_context = ssl._create_stdlib_context()
        typ, dat = self._simple_command('STARTTLS')
        if typ != 'OK':
            raise self.error("Couldn't establish TLS session")
        self.sock = ssl_context.wrap_socket(self.sock,
                                            server_hostname=self.host)
        self._imaplib_file = self.sock.makefile('rb')
        self._tls_established = True
        self._get_capabilities()
        return typ, dat

    def status(self, mailbox, names):
        """Request status items (e.g. '(MESSAGES UIDNEXT)') of a mailbox."""
        typ, dat = self._simple_command('STATUS', self._astring(mailbox),
                                        self._parenthesized(names))
        return self._untagged_response(typ, dat, 'STATUS')

    def store(self, message_set, command, flags):
        """Alter message flags, e.g. store('1:3', '+FLAGS', r'\\Seen')."""
        typ, dat = self._simple_command('STORE', message_set, command,
                                        self._parenthesized(flags))
        return self._untagged_response(typ, dat, 'FETCH')

    def subscribe(self, mailbox):
        """Subscribe to a mailbox."""
        return self._simple_command('SUBSCRIBE', self._astring(mailbox))

    def thread(self, threading_algorithm, charset, *search_criteria):
        """Threaded search (THREAD extension)."""
        typ, dat = self._simple_command('THREAD', threading_algorithm,
                                        self._astring(charset),
                                        *search_criteria)
        return self._untagged_response(typ, dat, 'THREAD')

    def uid(self, command, *args):
        """Run a command taking UIDs instead of message numbers:
        uid('FETCH', ...), uid('SEARCH', ...), ..."""
        if isinstance(command, bytes):
            key = command.decode('ascii', 'replace').upper()
        else:
            key = command.upper()
        if key not in Commands:
            raise self.error('Unknown IMAP4 UID command: %r' % (command,))
        if self.state not in Commands[key]:
            raise self.error('command %s illegal in state %s, '
                             'only allowed in states %s'
                             % (key, self.state, ', '.join(Commands[key])))
        args = self._uid_arguments(key, args)
        typ, dat = self._simple_command('UID', key, *args)
        return self._untagged_response(typ, dat, _UID_RESULT.get(key, 'FETCH'))

    def _uid_arguments(self, key, args):
        args = list(args)
        if key == 'FETCH' and len(args) >= 2:
            args[1] = self._fetch_items(args[1])
        elif key == 'STORE' and len(args) >= 3:
            args[2] = self._parenthesized(args[2])
        elif key in ('COPY', 'MOVE') and len(args) >= 2:
            args[1] = self._astring(args[1])
        elif key in ('SORT', 'THREAD') and len(args) >= 2:
            if key == 'SORT':
                args[0] = self._parenthesized(args[0])
            args[1] = self._astring(args[1])
        return args

    def unselect(self):
        """Leave the selected state without expunging."""
        try:
            return self._simple_command('UNSELECT')
        finally:
            self.state = 'AUTH'

    def unsubscribe(self, mailbox):
        """Unsubscribe from a mailbox."""
        return self._simple_command('UNSUBSCRIBE', self._astring(mailbox))

    def xatom(self, name, *args):
        """Send an extension command the class does not know."""
        name = name.upper()
        if name not in Commands:
            Commands[name] = (self.state,)
        return self._simple_command(name, *args)

    # -- debugging ------------------------------------------------------------

    def _mesg(self, s, secs=None):
        if secs is None:
            secs = time.time()
        stamp = time.strftime('%M:%S', time.localtime(secs))
        sys.stderr.write('  %s.%02d %s\n'
                         % (stamp, int((secs * 100) % 100), s))
        sys.stderr.flush()

    def _dump_ur(self, untagged_resp_dict):
        if not untagged_resp_dict:
            return
        entries = ['%s: %r' % (key, value)
                   for key, value in untagged_resp_dict.items()]
        self._mesg('untagged responses dump:' + '\n\t\t'.join(entries))


class Idler:
    """IDLE session of an IMAP4 connection; see IMAP4.idle()."""

    def __init__(self, imap, duration=None):
        if 'IDLE' not in imap.capabilities:
            raise imap.error('Server does not support IMAP4 IDLE')
        self._imap = imap
        self._duration = duration
        self._deadline = None
        self._tag = None
        self._done = False

    def __enter__(self):
        imap = self._imap
        imap._idle_queue = []
        try:
            self._tag = imap._send_command('IDLE')
            while True:
                kind, _ = imap._read_response()
                if kind == 'continuation':
                    break
                result = imap.tagged_commands.get(self._tag)
                if result is not None:
                    imap.tagged_commands.pop(self._tag)
                    raise imap.error('IDLE command error: %s %s' % result)
        except BaseException:
            imap._idle_queue = None
            raise
        if self._duration is not None:
            self._deadline = time.monotonic() + self._duration
        return self

    def __exit__(self, *exc_info):
        imap = self._imap
        imap._idle_queue = None
        if self._tag is None:
            return
        tag, self._tag = self._tag, None
        if imap.tagged_commands.get(tag) is None:
            imap.send(b'DONE' + CRLF)
        imap._await_completion('IDLE', tag)

    def __iter__(self):
        return self

    def __next__(self):
        item = self._next_response(self._deadline)
        if item is None:
            raise StopIteration
        return item

    def burst(self, interval=0.1):
        """Yield the responses that arrive without a pause longer than
        *interval* seconds."""
        while True:
            deadline = time.monotonic() + interval
            if self._deadline is not None:
                deadline = min(deadline, self._deadline)
            item = self._next_response(deadline)
            if item is None:
                return
            yield item

    def _next_response(self, deadline):
        imap = self._imap
        queue = imap._idle_queue
        while queue is not None:
            if queue:
                typ, items = queue.pop(0)
                self._consume(typ, items)
                return typ, items
            if self._done:
                return None
            try:
                kind, _ = imap._read_response(deadline)
            except _IdleTimeout:
                return None
            if kind == 'tagged' and imap.tagged_commands.get(self._tag):
                # the server ended IDLE on its own
                self._done = True
        return None

    def _consume(self, typ, items):
        stored = self._imap.untagged_responses.get(typ)
        if not stored:
            return
        for item in items:
            for i, x in enumerate(stored):
                if x is item:
                    del stored[i]
                    break
        if not stored:
            del self._imap.untagged_responses[typ]


if HAVE_SSL:

    class IMAP4_SSL(IMAP4):
        """IMAP4 over an SSL/TLS connection.

        IMAP4_SSL(host='', port=IMAP4_SSL_PORT, *, ssl_context=None,
                  timeout=None)
        """

        def __init__(self, host='', port=IMAP4_SSL_PORT, *,
                     ssl_context=None, timeout=None):
            if ssl_context is None:
                ssl_context = ssl._create_stdlib_context()
            self.ssl_context = ssl_context
            IMAP4.__init__(self, host, port, timeout)

        def _create_socket(self, timeout):
            sock = IMAP4._create_socket(self, timeout)
            return self.ssl_context.wrap_socket(sock,
                                                server_hostname=self.host)

        def open(self, host='', port=IMAP4_SSL_PORT, timeout=None):
            IMAP4.open(self, host, port, timeout)


class IMAP4_stream(IMAP4):
    """IMAP4 over the standard streams of a command (e.g. an ssh tunnel).

    IMAP4_stream(command)
    """

    def __init__(self, command):
        self.command = command
        IMAP4.__init__(self)

    def open(self, host=None, port=None, timeout=None):
        self.host = None
        self.port = None
        self.sock = None
        self._imaplib_file = None
        self.process = subprocess.Popen(
            self.command, bufsize=DEFAULT_BUFFER_SIZE,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            shell=True, close_fds=True)
        self.writefile = self.process.stdin
        self.readfile = self.process.stdout

    def read(self, size):
        return self.readfile.read(size)

    def readline(self):
        line = self.readfile.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise self.error('got more than %d bytes' % _MAXLINE)
        return line

    def send(self, data):
        self.writefile.write(data)
        self.writefile.flush()

    def shutdown(self):
        self.readfile.close()
        self.writefile.close()
        self.process.wait()


# -- helpers ----------------------------------------------------------------

def Internaldate2tuple(resp):
    """Convert an INTERNALDATE in an IMAP response to a local time tuple
    (None if *resp* contains none)."""
    m = _INTERNALDATE.match(resp)
    if not m:
        return None
    month = _MONTH_NUMBER.get(m.group('mon'))
    if month is None:
        return None
    fields = [int(m.group(k)) for k in ('year', 'day', 'hour', 'min', 'sec')]
    year, day, hour, minute, sec = fields
    offset = (int(m.group('zoneh')) * 60 + int(m.group('zonem'))) * 60
    if m.group('zonen') == b'-':
        offset = -offset
    utc = calendar.timegm((year, month, day, hour, minute, sec, -1, -1, -1))
    return time.localtime(utc - offset)


def Int2AP(num):
    """Encode a non-negative integer with the letters A..P (base 16)."""
    digits = b''
    letters = b'ABCDEFGHIJKLMNOP'
    num = int(abs(num))
    while num:
        num, d = divmod(num, 16)
        digits = letters[d:d + 1] + digits
    return digits


def ParseFlags(resp):
    """The flags of a FLAGS response as a tuple of bytes."""
    m = _FLAGS.match(resp)
    if not m:
        return ()
    return tuple(m.group('flags').split())


def Time2Internaldate(date_time):
    """Format a time as an IMAP INTERNALDATE: '"DD-Mmm-YYYY HH:MM:SS +HHMM"'.

    Accepts seconds since the epoch, a local time tuple (struct_time or
    9-tuple), an aware datetime, or an already quoted date string.
    """
    if isinstance(date_time, (int, float)):
        dt = datetime.fromtimestamp(date_time, timezone.utc).astimezone()
    elif isinstance(date_time, tuple):
        gmtoff = getattr(date_time, 'tm_gmtoff', None)
        if gmtoff is not None:
            tz = timezone(timedelta(seconds=gmtoff))
            dt = datetime(*date_time[:6], tzinfo=tz)
        else:
            stamp = time.mktime(tuple(date_time))
            dt = datetime.fromtimestamp(stamp, timezone.utc).astimezone()
    elif isinstance(date_time, datetime):
        if date_time.tzinfo is None:
            raise ValueError('date_time must be aware')
        dt = date_time
    elif isinstance(date_time, str) and _QUOTED_INTERNALDATE.match(date_time):
        return date_time
    else:
        raise ValueError('date_time not of a known type')
    offset = dt.utcoffset()
    minutes = int(offset.total_seconds()) // 60
    sign = '+' if minutes >= 0 else '-'
    minutes = abs(minutes)
    return '"%02d-%s-%04d %02d:%02d:%02d %s%02d%02d"' % (
        dt.day, _MONTHS[dt.month - 1], dt.year, dt.hour, dt.minute,
        dt.second, sign, minutes // 60, minutes % 60)


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings._deprecated('__version__', remove=(3, 20))
        return '2.60'
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')
