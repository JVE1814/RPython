"""POP3 client (RFC 1939, with the RFC 2449 CAPA, RFC 2595 STLS and
RFC 6856 UTF8 extensions)."""

import errno
import re
import socket
import sys

try:
    import ssl
    HAVE_SSL = True
except ImportError:
    HAVE_SSL = False

__all__ = ["POP3", "error_proto"]


class error_proto(Exception):
    """A protocol error: an -ERR reply or a malformed response."""


POP3_PORT = 110
POP3_SSL_PORT = 995

CR = b'\r'
LF = b'\n'
CRLF = CR + LF

# RFC 1939 limits lines to 512 octets; be lenient but bounded.
_MAXLINE = 2048

# C0 controls and DEL may not appear inside a command line.
_CONTROL_CHARS = re.compile('[\x00-\x1f\x7f]')


class POP3:
    """A POP3 session: POP3(host, port=110, timeout=...).

    Commands return the server's status line (bytes); multi-line commands
    return (response, [lines], octets)."""

    encoding = 'UTF-8'

    def __init__(self, host, port=POP3_PORT,
                 timeout=socket._GLOBAL_DEFAULT_TIMEOUT):
        self.host = host
        self.port = port
        self._tls_established = False
        sys.audit("poplib.connect", self, host, port)
        self.sock = self._create_socket(timeout)
        self.file = self.sock.makefile('rb')
        self._debugging = 0
        self.welcome = self._getresp()

    def _create_socket(self, timeout):
        if timeout is not None and not timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        return socket.create_connection((self.host, self.port), timeout)

    # low level I/O

    def _putline(self, line):
        if self._debugging > 1:
            print('*put*', repr(line))
        sys.audit("poplib.putline", self, line)
        self.sock.sendall(line + CRLF)

    def _putcmd(self, line):
        if self._debugging:
            print('*cmd*', repr(line))
        if _CONTROL_CHARS.search(line):
            raise ValueError('Control characters not allowed in commands')
        self._putline(bytes(line, self.encoding))

    def _getline(self):
        """One line without its terminator, plus its length on the wire."""
        line = self.file.readline(_MAXLINE + 1)
        if len(line) > _MAXLINE:
            raise error_proto('line too long')
        if self._debugging > 1:
            print('*get*', repr(line))
        if not line:
            raise error_proto('-ERR EOF')
        octets = len(line)
        if line[-2:] == CRLF:
            return line[:-2], octets
        if line[:1] == CR:
            return line[1:-1], octets
        return line[:-1], octets

    def _getresp(self):
        resp, _octets = self._getline()
        if self._debugging > 1:
            print('*resp*', repr(resp))
        if not resp.startswith(b'+'):
            raise error_proto(resp)
        return resp

    def _getlongresp(self):
        resp = self._getresp()
        lines = []
        octets = 0
        line, o = self._getline()
        while line != b'.':
            if line.startswith(b'..'):
                # undo dot-stuffing
                o -= 1
                line = line[1:]
            octets += o
            lines.append(line)
            line, o = self._getline()
        return resp, lines, octets

    def _shortcmd(self, line):
        self._putcmd(line)
        return self._getresp()

    def _longcmd(self, line):
        self._putcmd(line)
        return self._getlongresp()

    # public interface

    def getwelcome(self):
        return self.welcome

    def set_debuglevel(self, level):
        self._debugging = level

    def user(self, user):
        return self._shortcmd('USER %s' % user)

    def pass_(self, pswd):
        return self._shortcmd('PASS %s' % pswd)

    def stat(self):
        """(message count, mailbox size)."""
        retval = self._shortcmd('STAT')
        rets = retval.split()
        if self._debugging:
            print('*stat*', repr(rets))
        try:
            return (int(rets[1]), int(rets[2]))
        except (IndexError, ValueError):
            raise error_proto('-ERR invalid STAT response: %r' % retval) from None

    def list(self, which=None):
        """Scan listing of all messages, or of message *which*."""
        if which is not None:
            return self._shortcmd('LIST %s' % which)
        return self._longcmd('LIST')

    def retr(self, which):
        return self._longcmd('RETR %s' % which)

    def dele(self, which):
        return self._shortcmd('DELE %s' % which)

    def noop(self):
        return self._shortcmd('NOOP')

    def rset(self):
        return self._shortcmd('RSET')

    def quit(self):
        resp = self._shortcmd('QUIT')
        self.close()
        return resp

    def close(self):
        """Close the connection without QUIT."""
        try:
            file = self.file
            self.file = None
            if file is not None:
                file.close()
        finally:
            sock = self.sock
            self.sock = None
            if sock is not None:
                try:
                    sock.shutdown(socket.SHUT_RDWR)
                except OSError as exc:
                    # the peer may already have gone away
                    if (exc.errno != errno.ENOTCONN
                            and getattr(exc, 'winerror', 0) != 10022):
                        raise
                finally:
                    sock.close()

    def rpop(self, user):
        return self._shortcmd('RPOP %s' % user)

    timestamp = re.compile(br'\+OK.[^<]*(<.*>)')

    def apop(self, user, password):
        """APOP authentication with the greeting's timestamp."""
        secret = bytes(password, self.encoding)
        m = self.timestamp.match(self.welcome)
        if not m:
            raise error_proto('-ERR APOP not supported by server')
        import hashlib
        digest = hashlib.md5(m.group(1) + secret).hexdigest()
        return self._shortcmd('APOP %s %s' % (user, digest))

    def top(self, which, howmuch):
        """Headers of message *which* plus *howmuch* body lines."""
        return self._longcmd('TOP %s %s' % (which, howmuch))

    def uidl(self, which=None):
        if which is not None:
            return self._shortcmd('UIDL %s' % which)
        return self._longcmd('UIDL')

    def utf8(self):
        return self._shortcmd('UTF8')

    def capa(self):
        """Server capabilities: {name: [arguments]}."""
        caps = {}
        try:
            resp = self._longcmd('CAPA')
        except error_proto:
            raise error_proto('-ERR CAPA not supported by server') from None
        for line in resp[1]:
            name, *args = line.decode('ascii').split()
            caps[name] = args
        return caps

    def stls(self, context=None):
        """Start a TLS session (STLS)."""
        if not HAVE_SSL:
            raise error_proto('-ERR TLS support missing')
        if self._tls_established:
            raise error_proto('-ERR TLS session already established')
        if 'STLS' not in self.capa():
            raise error_proto('-ERR STLS not supported by server')
        if context is None:
            context = ssl._create_stdlib_context()
        resp = self._shortcmd('STLS')
        self.sock = context.wrap_socket(self.sock, server_hostname=self.host)
        self.file = self.sock.makefile('rb')
        self._tls_established = True
        return resp


if HAVE_SSL:

    class POP3_SSL(POP3):
        """POP3 over an implicit TLS connection (port 995)."""

        def __init__(self, host, port=POP3_SSL_PORT, *,
                     timeout=socket._GLOBAL_DEFAULT_TIMEOUT, context=None):
            if context is None:
                context = ssl._create_stdlib_context()
            self.context = context
            POP3.__init__(self, host, port, timeout)

        def _create_socket(self, timeout):
            sock = POP3._create_socket(self, timeout)
            return self.context.wrap_socket(sock, server_hostname=self.host)

        def stls(self, context=None):
            raise error_proto('-ERR TLS session already established')

    __all__.append("POP3_SSL")
