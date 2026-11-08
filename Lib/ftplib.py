"""FTP client: RFC 959, plus EPRT/EPSV (RFC 2428), MLSD and SIZE (RFC 3659)
and explicit FTP over TLS (RFC 4217, class FTP_TLS).

    with FTP('ftp.example.org') as ftp:
        ftp.login()                     # anonymous
        ftp.retrlines('LIST')
        with open('file', 'wb') as out:
            ftp.retrbinary('RETR file', out.write)

Server replies come back as text.  A 4xx reply raises error_temp, a 5xx
one error_perm, anything else unexpected error_reply or error_proto.
"""

import re
import socket
import sys
from socket import _GLOBAL_DEFAULT_TIMEOUT

__all__ = ["FTP", "error_reply", "error_temp", "error_perm", "error_proto",
           "all_errors"]

MSG_OOB = 0x1           # send ABOR as urgent (out-of-band) data
FTP_PORT = 21
MAXLINE = 8192          # longest line accepted from the server

CRLF = '\r\n'
B_CRLF = b'\r\n'


class Error(Exception):
    pass


class error_reply(Error):
    """A reply with a code the command did not expect."""


class error_temp(Error):
    """A 4xx (transient failure) reply."""


class error_perm(Error):
    """A 5xx (permanent failure) reply."""


class error_proto(Error):
    """A reply that does not follow the protocol."""


# Everything an FTP session can raise for a failed operation.
all_errors = (Error, OSError, EOFError)

# What the first digit of a reply code means for the caller.
_REPLY_FAILURES = {'4': error_temp, '5': error_perm}

_TRANSFER_ABORTED = ('426', '225', '226')    # good answers to ABOR


class FTP:
    """A session with an FTP server.

    With host, the constructor connects; with user as well, it logs in.
    Text is sent and received in the given encoding (UTF-8 by default).
    """

    debugging = 0
    host = ''
    port = FTP_PORT
    maxline = MAXLINE
    sock = None
    file = None
    welcome = None
    passiveserver = True
    # The address in a PASV reply is ignored in favour of the server's own
    # address unless this is set: a hostile server could point the client
    # at any host.
    trust_server_pasv_ipv4_address = False

    def __init__(self, host='', user='', passwd='', acct='',
                 timeout=_GLOBAL_DEFAULT_TIMEOUT, source_address=None, *,
                 encoding='utf-8'):
        self.encoding = encoding
        self.source_address = source_address
        self.timeout = timeout
        if host:
            self.connect(host)
            if user:
                self.login(user, passwd, acct)

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        if self.sock is None:
            return
        try:
            self.quit()
        except (OSError, EOFError):
            pass
        finally:
            if self.sock is not None:
                self.close()

    def connect(self, host='', port=0, timeout=-999, source_address=None):
        """Open the control connection and return the server's welcome.

        Arguments left at their defaults keep the values given before.
        """
        if host != '':
            self.host = host
        if port > 0:
            self.port = port
        if timeout != -999:
            self.timeout = timeout
        if self.timeout is not None and not self.timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        if source_address is not None:
            self.source_address = source_address
        sys.audit("ftplib.connect", self, self.host, self.port)
        self.sock = socket.create_connection((self.host, self.port),
                                             self.timeout,
                                             source_address=self.source_address)
        self.af = self.sock.family
        self.file = self.sock.makefile('r', encoding=self.encoding)
        self.welcome = self.getresp()
        return self.welcome

    def getwelcome(self):
        """The welcome message the server sent on connecting."""
        if self.debugging:
            print('*welcome*', self.sanitize(self.welcome))
        return self.welcome

    def set_debuglevel(self, level):
        """0 prints nothing, 1 commands and replies, 2 every line."""
        self.debugging = level

    debug = set_debuglevel

    def set_pasv(self, val):
        """Open data connections passively (the default) or actively."""
        self.passiveserver = val

    def sanitize(self, s):
        """repr() of a line for debug output, with any password hidden."""
        if s[:5] in ('pass ', 'PASS '):
            end = len(s.rstrip('\r\n'))
            s = s[:5] + '*' * (end - 5) + s[end:]
        return repr(s)

    # -- the control connection

    def putline(self, line):
        if '\r' in line or '\n' in line:
            raise ValueError('an illegal newline character should not be contained')
        sys.audit("ftplib.sendcmd", self, line)
        line += CRLF
        if self.debugging > 1:
            print('*put*', self.sanitize(line))
        self.sock.sendall(line.encode(self.encoding))

    def putcmd(self, line):
        if self.debugging:
            print('*cmd*', self.sanitize(line))
        self.putline(line)

    def getline(self):
        """One line from the server, without its line ending."""
        line = self.file.readline(self.maxline + 1)
        if len(line) > self.maxline:
            raise Error("got more than %d bytes" % self.maxline)
        if self.debugging > 1:
            print('*get*', self.sanitize(line))
        if not line:
            raise EOFError
        if line.endswith(CRLF):
            return line[:-2]
        if line[-1:] in ('\r', '\n'):
            return line[:-1]
        return line

    def getmultiline(self):
        """One reply, which may span lines: "123-first ... 123 last"."""
        lines = [self.getline()]
        if lines[0][3:4] == '-':
            code = lines[0][:3]
            while True:
                lines.append(self.getline())
                if lines[-1][:3] == code and lines[-1][3:4] != '-':
                    break
        return '\n'.join(lines)

    def getresp(self):
        """One reply; 4xx and 5xx replies (and garbage) raise."""
        resp = self.getmultiline()
        if self.debugging:
            print('*resp*', self.sanitize(resp))
        self.lastresp = resp[:3]
        kind = resp[:1]
        if kind in ('1', '2', '3'):
            return resp
        raise _REPLY_FAILURES.get(kind, error_proto)(resp)

    def voidresp(self):
        """One reply, which must be 2xx (else error_reply)."""
        resp = self.getresp()
        if not resp.startswith('2'):
            raise error_reply(resp)
        return resp

    def _check_abort_reply(self):
        resp = self.getmultiline()
        if resp[:3] not in _TRANSFER_ABORTED:
            raise error_proto(resp)
        return resp

    def abort(self):
        """Stop a running transfer (ABOR, sent as urgent data)."""
        line = b'ABOR' + B_CRLF
        if self.debugging > 1:
            print('*put urgent*', self.sanitize(line))
        self.sock.sendall(line, MSG_OOB)
        return self._check_abort_reply()

    def sendcmd(self, cmd):
        """Send a command and return the reply."""
        self.putcmd(cmd)
        return self.getresp()

    def voidcmd(self, cmd):
        """Send a command that must be answered with 2xx."""
        self.putcmd(cmd)
        return self.voidresp()

    # -- data connections

    def sendport(self, host, port):
        """Tell the server where to connect for data (PORT, IPv4)."""
        numbers = host.split('.') + [repr(port // 256), repr(port % 256)]
        return self.voidcmd('PORT ' + ','.join(numbers))

    def sendeprt(self, host, port):
        """Tell the server where to connect for data (EPRT, any family)."""
        families = {socket.AF_INET: 1, socket.AF_INET6: 2}
        if self.af not in families:
            raise error_proto('unsupported address family')
        return self.voidcmd('EPRT |%d|%s|%r|' % (families[self.af], host, port))

    def makeport(self):
        """Listen on a fresh port and announce it; returns the listening
        socket."""
        listener = socket.create_server(("", 0), family=self.af, backlog=1)
        port = listener.getsockname()[1]
        host = self.sock.getsockname()[0]
        if self.af == socket.AF_INET:
            self.sendport(host, port)
        else:
            self.sendeprt(host, port)
        if self.timeout is not _GLOBAL_DEFAULT_TIMEOUT:
            listener.settimeout(self.timeout)
        return listener

    def makepasv(self):
        """Ask the server for a port to connect to: (host, port)."""
        if self.af != socket.AF_INET:
            return parse229(self.sendcmd('EPSV'), self.sock.getpeername())
        offered, port = parse227(self.sendcmd('PASV'))
        if self.trust_server_pasv_ipv4_address:
            return offered, port
        return self.sock.getpeername()[0], port

    def _start_transfer(self, cmd, rest):
        # REST (to resume at an offset) and the command; the server must
        # answer 1xx.  Some send a 2xx first and the 1xx after it.
        if rest is not None:
            self.sendcmd("REST %s" % rest)
        resp = self.sendcmd(cmd)
        if resp.startswith('2'):
            resp = self.getresp()
        if not resp.startswith('1'):
            raise error_reply(resp)
        return resp

    def ntransfercmd(self, cmd, rest=None):
        """Open a data connection for cmd; returns (socket, expected size).

        The size comes from a "150 ... (N bytes)" reply, else it is None.
        """
        if self.passiveserver:
            conn = socket.create_connection(self.makepasv(), self.timeout,
                                            source_address=self.source_address)
            try:
                resp = self._start_transfer(cmd, rest)
            except:
                conn.close()
                raise
        else:
            with self.makeport() as listener:
                resp = self._start_transfer(cmd, rest)
                conn, _ = listener.accept()
                if self.timeout is not _GLOBAL_DEFAULT_TIMEOUT:
                    conn.settimeout(self.timeout)
        size = parse150(resp) if resp.startswith('150') else None
        return conn, size

    def transfercmd(self, cmd, rest=None):
        """Like ntransfercmd(), returning only the socket."""
        return self.ntransfercmd(cmd, rest)[0]

    def _end_transfer(self, conn):
        # A TLS data connection is shut down cleanly before it is closed.
        if _SSLSocket is not None and isinstance(conn, _SSLSocket):
            conn.unwrap()

    def _checked_line(self, line):
        if len(line) > self.maxline:
            raise Error("got more than %d bytes" % self.maxline)
        return line

    def retrbinary(self, cmd, callback, blocksize=8192, rest=None):
        """Retrieve in binary mode, calling callback with each block."""
        self.voidcmd('TYPE I')
        with self.transfercmd(cmd, rest) as conn:
            while block := conn.recv(blocksize):
                callback(block)
            self._end_transfer(conn)
        return self.voidresp()

    def retrlines(self, cmd, callback=None):
        """Retrieve in ASCII mode, calling callback with each line (without
        its line ending); by default the lines are printed."""
        if callback is None:
            callback = print_line
        self.sendcmd('TYPE A')
        with self.transfercmd(cmd) as conn, \
                conn.makefile('r', encoding=self.encoding) as lines:
            while True:
                line = self._checked_line(lines.readline(self.maxline + 1))
                if self.debugging > 2:
                    print('*retr*', repr(line))
                if not line:
                    break
                if line.endswith(CRLF):
                    line = line[:-2]
                elif line.endswith('\n'):
                    line = line[:-1]
                callback(line)
            self._end_transfer(conn)
        return self.voidresp()

    def storbinary(self, cmd, fp, blocksize=8192, callback=None, rest=None):
        """Upload the binary file fp; callback, if given, sees each block."""
        self.voidcmd('TYPE I')
        with self.transfercmd(cmd, rest) as conn:
            while block := fp.read(blocksize):
                conn.sendall(block)
                if callback:
                    callback(block)
            self._end_transfer(conn)
        return self.voidresp()

    def storlines(self, cmd, fp, callback=None):
        """Upload the binary file fp line by line, with CRLF line ends."""
        self.voidcmd('TYPE A')
        with self.transfercmd(cmd) as conn:
            while line := self._checked_line(fp.readline(self.maxline + 1)):
                if not line.endswith(B_CRLF):
                    if line[-1] in B_CRLF:
                        line = line[:-1]
                    line += B_CRLF
                conn.sendall(line)
                if callback:
                    callback(line)
            self._end_transfer(conn)
        return self.voidresp()

    # -- login and commands

    def login(self, user='', passwd='', acct=''):
        """Log in; without a user name, anonymously."""
        user = user or 'anonymous'
        passwd = passwd or ''
        acct = acct or ''
        if user == 'anonymous' and passwd in ('', '-'):
            # Anonymous users give a mail address as password; a leading
            # "-" asks the server for short replies.
            passwd += 'anonymous@'
        resp = self.sendcmd('USER ' + user)
        for follow_up in ('PASS ' + passwd, 'ACCT ' + acct):
            if not resp.startswith('3'):
                break
            resp = self.sendcmd(follow_up)
        if not resp.startswith('2'):
            raise error_reply(resp)
        return resp

    def acct(self, password):
        """Send account information (ACCT)."""
        return self.voidcmd('ACCT ' + password)

    def nlst(self, *args):
        """Names of the files in a directory (NLST)."""
        names = []
        self.retrlines(' '.join(('NLST',) + args), names.append)
        return names

    def dir(self, *args):
        """Print a directory listing (LIST); if the last argument is not a
        str, it is called with each line instead."""
        callback = None
        if args and not isinstance(args[-1], str):
            args, callback = args[:-1], args[-1]
        self.retrlines(' '.join(['LIST'] + [arg for arg in args if arg]),
                       callback)

    def mlsd(self, path="", facts=[]):
        """Iterate over a directory listing in the standard MLSD format,
        yielding (name, {fact: value}) with lowercased fact names.  facts
        asks the server for those facts (OPTS MLST)."""
        if facts:
            self.sendcmd("OPTS MLST " + ";".join(facts) + ";")
        listing = []
        self.retrlines("MLSD %s" % path if path else "MLSD", listing.append)
        for entry in listing:
            fact_text, _, name = entry.rstrip(CRLF).partition(' ')
            found = {}
            for fact in fact_text[:-1].split(";"):
                key, _, value = fact.partition("=")
                found[key.lower()] = value
            yield name, found

    def rename(self, fromname, toname):
        """Rename a file on the server."""
        resp = self.sendcmd('RNFR ' + fromname)
        if not resp.startswith('3'):
            raise error_reply(resp)
        return self.voidcmd('RNTO ' + toname)

    def delete(self, filename):
        """Delete a file on the server."""
        resp = self.sendcmd('DELE ' + filename)
        if resp[:3] not in ('250', '200'):
            raise error_reply(resp)
        return resp

    def cwd(self, dirname):
        """Change the working directory on the server."""
        if dirname == '..':
            try:
                return self.voidcmd('CDUP')
            except error_perm as failure:
                # A server without CDUP answers 500: use CWD .. instead.
                if failure.args[0][:3] != '500':
                    raise
        elif dirname == '':
            dirname = '.'
        return self.voidcmd('CWD ' + dirname)

    def size(self, filename):
        """The size of a file (SIZE), or None if the server does not say."""
        resp = self.sendcmd('SIZE ' + filename)
        if resp[:3] == '213':
            return int(resp[3:].strip())
        return None

    def mkd(self, dirname):
        """Create a directory; returns its name as the server reports it."""
        resp = self.voidcmd('MKD ' + dirname)
        return parse257(resp) if resp.startswith('257') else ''

    def rmd(self, dirname):
        """Remove a directory."""
        return self.voidcmd('RMD ' + dirname)

    def pwd(self):
        """The server's current directory."""
        resp = self.voidcmd('PWD')
        return parse257(resp) if resp.startswith('257') else ''

    def quit(self):
        """Say goodbye (QUIT) and close the connection."""
        resp = self.voidcmd('QUIT')
        self.close()
        return resp

    def close(self):
        """Close the connection without saying goodbye."""
        reader, self.file = self.file, None
        try:
            if reader is not None:
                reader.close()
        finally:
            sock, self.sock = self.sock, None
            if sock is not None:
                sock.close()


try:
    import ssl
except ImportError:
    _SSLSocket = None
else:
    _SSLSocket = ssl.SSLSocket

    class FTP_TLS(FTP):
        """FTP secured with TLS (RFC 4217).

        login() first secures the control connection (unless secure is
        false); data connections stay plain until prot_p() is called.
        context is an ssl.SSLContext.
        """

        def __init__(self, host='', user='', passwd='', acct='', *,
                     context=None, timeout=_GLOBAL_DEFAULT_TIMEOUT,
                     source_address=None, encoding='utf-8'):
            self.context = (ssl._create_stdlib_context() if context is None
                            else context)
            self._prot_p = False
            super().__init__(host, user, passwd, acct, timeout,
                             source_address, encoding=encoding)

        def _wrap(self, sock):
            return self.context.wrap_socket(sock, server_hostname=self.host)

        def login(self, user='', passwd='', acct='', secure=True):
            if secure and not isinstance(self.sock, ssl.SSLSocket):
                self.auth()
            return super().login(user, passwd, acct)

        def auth(self):
            """Switch the control connection to TLS (AUTH)."""
            if isinstance(self.sock, ssl.SSLSocket):
                raise ValueError("Already using TLS")
            mechanism = 'TLS' if self.context.protocol >= ssl.PROTOCOL_TLS else 'SSL'
            resp = self.voidcmd('AUTH ' + mechanism)
            self.sock = self._wrap(self.sock)
            self.file = self.sock.makefile(mode='r', encoding=self.encoding)
            return resp

        def ccc(self):
            """Return the control connection to plain text (CCC)."""
            if not isinstance(self.sock, ssl.SSLSocket):
                raise ValueError("not using TLS")
            resp = self.voidcmd('CCC')
            self.sock = self.sock.unwrap()
            return resp

        def prot_p(self):
            """Use TLS for data connections from now on."""
            self.voidcmd('PBSZ 0')
            resp = self.voidcmd('PROT P')
            self._prot_p = True
            return resp

        def prot_c(self):
            """Use plain data connections from now on."""
            resp = self.voidcmd('PROT C')
            self._prot_p = False
            return resp

        def ntransfercmd(self, cmd, rest=None):
            conn, size = super().ntransfercmd(cmd, rest)
            if self._prot_p:
                conn = self._wrap(conn)
            return conn, size

        def abort(self):
            # TLS has no urgent data: ABOR goes out as a normal line.
            self.sock.sendall(b'ABOR' + B_CRLF)
            return self._check_abort_reply()

    __all__.append('FTP_TLS')
    all_errors = (Error, OSError, EOFError, ssl.SSLError)


# ------------------------------------------------------------ reply parsing

_SIZE_IN_150 = re.compile(r"150 .* \((\d+) bytes\)", re.IGNORECASE | re.ASCII)
_ADDRESS_IN_227 = re.compile(r'(\d+),(\d+),(\d+),(\d+),(\d+),(\d+)', re.ASCII)


def parse150(resp):
    """The transfer size in a "150 ... (N bytes)" reply, or None."""
    if not resp.startswith('150'):
        raise error_reply(resp)
    found = _SIZE_IN_150.match(resp)
    return int(found.group(1)) if found else None


def parse227(resp):
    """(host, port) from a "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)"
    reply."""
    if not resp.startswith('227'):
        raise error_reply(resp)
    found = _ADDRESS_IN_227.search(resp)
    if not found:
        raise error_proto(resp)
    numbers = found.groups()
    return '.'.join(numbers[:4]), int(numbers[4]) * 256 + int(numbers[5])


def parse229(resp, peer):
    """(host, port) from a "229 ... (|||port|)" reply: the host is the one
    at the other end of the control connection (peer)."""
    if not resp.startswith('229'):
        raise error_reply(resp)
    opening = resp.find('(')
    closing = resp.find(')', opening + 1) if opening >= 0 else -1
    if closing < 0:
        raise error_proto(resp)
    inside = resp[opening + 1:closing]
    # Five fields separated by one delimiter, which must also end the text.
    if not inside or inside[0] != inside[-1]:
        raise error_proto(resp)
    fields = inside.split(inside[0])
    if len(fields) != 5:
        raise error_proto(resp)
    return peer[0], int(fields[3])


def parse257(resp):
    """The directory name in a '257 "name" ...' reply (a doubled quote
    stands for one quote); '' if the reply has no quoted name."""
    if not resp.startswith('257'):
        raise error_reply(resp)
    if resp[3:5] != ' "':
        return ''
    name = []
    rest = iter(resp[5:])
    for ch in rest:
        if ch == '"' and next(rest, None) != '"':
            break
        name.append(ch)
    return ''.join(name)


def print_line(line):
    """The default retrlines() callback: print the line."""
    print(line)


def ftpcp(source, sourcename, target, targetname='', type='I'):
    """Have server source send a file straight to server target."""
    targetname = targetname or sourcename
    for session in (source, target):
        session.voidcmd('TYPE ' + type)
    host, port = parse227(source.sendcmd('PASV'))
    if not source.trust_server_pasv_ipv4_address:
        host = source.sock.getpeername()[0]    # see FTP.makepasv()
    target.sendport(host, port)
    # Both sides must answer 125 or 150 (data connection about to open).
    if target.sendcmd('STOR ' + targetname)[:3] not in ('125', '150'):
        raise error_proto
    if source.sendcmd('RETR ' + sourcename)[:3] not in ('125', '150'):
        raise error_proto
    source.voidresp()
    target.voidresp()
