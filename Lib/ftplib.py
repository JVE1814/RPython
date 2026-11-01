"""FTP client (RFC 959, with RFC 2428 EPRT/EPSV, RFC 3659 MLSD/SIZE and
RFC 4217 FTP over TLS).

    ftp = FTP('ftp.example.org')
    ftp.login()
    ftp.retrlines('LIST')
    ftp.quit()
"""

import sys
import socket
from socket import _GLOBAL_DEFAULT_TIMEOUT

__all__ = ["FTP", "error_reply", "error_temp", "error_perm", "error_proto",
           "all_errors"]

MSG_OOB = 0x1          # urgent data flag for ABOR
FTP_PORT = 21
MAXLINE = 8192         # longest line accepted from the server

CRLF = '\r\n'
B_CRLF = b'\r\n'


class Error(Exception):
    pass


class error_reply(Error):
    """Unexpected reply code (not 1xx-3xx where expected)."""


class error_temp(Error):
    """4xx reply."""


class error_perm(Error):
    """5xx reply."""


class error_proto(Error):
    """Reply that does not start with a digit 1-5, or malformed data."""


all_errors = (Error, OSError, EOFError)


class FTP:
    """An FTP session.

    FTP(host='', user='', passwd='', acct='', timeout=..., source_address=None,
        *, encoding='utf-8') connects and logs in when host / user are given.
    """

    debugging = 0
    host = ''
    port = FTP_PORT
    maxline = MAXLINE
    sock = None
    file = None
    welcome = None
    passiveserver = True
    # By default the address in a PASV reply is ignored in favour of the
    # control connection's peer (it may point elsewhere).
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

    def __exit__(self, *args):
        if self.sock is not None:
            try:
                self.quit()
            except (OSError, EOFError):
                pass
            finally:
                if self.sock is not None:
                    self.close()

    def connect(self, host='', port=0, timeout=-999, source_address=None):
        """Open the control connection; returns the welcome message."""
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
        if self.debugging:
            print('*welcome*', self.sanitize(self.welcome))
        return self.welcome

    def set_debuglevel(self, level):
        """0: silent; 1: commands and replies; 2: every line."""
        self.debugging = level

    debug = set_debuglevel

    def set_pasv(self, val):
        """Use passive (True) or active (False) data connections."""
        self.passiveserver = val

    def sanitize(self, s):
        # never print passwords
        if s[:5] in {'pass ', 'PASS '}:
            i = len(s.rstrip('\r\n'))
            s = s[:5] + '*' * (i - 5) + s[i:]
        return repr(s)

    # control connection I/O

    def putline(self, line):
        if '\r' in line or '\n' in line:
            raise ValueError('an illegal newline character should not be '
                             'contained')
        sys.audit("ftplib.sendcmd", self, line)
        line = line + CRLF
        if self.debugging > 1:
            print('*put*', self.sanitize(line))
        self.sock.sendall(line.encode(self.encoding))

    def putcmd(self, line):
        if self.debugging:
            print('*cmd*', self.sanitize(line))
        self.putline(line)

    def getline(self):
        line = self.file.readline(self.maxline + 1)
        if len(line) > self.maxline:
            raise Error("got more than %d bytes" % self.maxline)
        if self.debugging > 1:
            print('*get*', self.sanitize(line))
        if not line:
            raise EOFError
        if line[-2:] == CRLF:
            line = line[:-2]
        elif line[-1:] in CRLF:
            line = line[:-1]
        return line

    def getmultiline(self):
        """A reply, joining continuation lines ('xyz-' ... 'xyz ')."""
        line = self.getline()
        if line[3:4] == '-':
            code = line[:3]
            while True:
                nextline = self.getline()
                line = line + '\n' + nextline
                if nextline[:3] == code and nextline[3:4] != '-':
                    break
        return line

    def getresp(self):
        resp = self.getmultiline()
        if self.debugging:
            print('*resp*', self.sanitize(resp))
        self.lastresp = resp[:3]
        c = resp[:1]
        if c in {'1', '2', '3'}:
            return resp
        if c == '4':
            raise error_temp(resp)
        if c == '5':
            raise error_perm(resp)
        raise error_proto(resp)

    def voidresp(self):
        """A reply that must be 2xx."""
        resp = self.getresp()
        if resp[:1] != '2':
            raise error_reply(resp)
        return resp

    def abort(self):
        """Abort the running transfer (ABOR sent as urgent data)."""
        line = b'ABOR' + B_CRLF
        if self.debugging > 1:
            print('*put urgent*', self.sanitize(line))
        self.sock.sendall(line, MSG_OOB)
        resp = self.getmultiline()
        if resp[:3] not in {'426', '225', '226'}:
            raise error_proto(resp)
        return resp

    def sendcmd(self, cmd):
        self.putcmd(cmd)
        return self.getresp()

    def voidcmd(self, cmd):
        self.putcmd(cmd)
        return self.voidresp()

    # data connections

    def sendport(self, host, port):
        """PORT h1,h2,h3,h4,p1,p2"""
        fields = host.split('.') + [repr(port // 256), repr(port % 256)]
        return self.voidcmd('PORT ' + ','.join(fields))

    def sendeprt(self, host, port):
        """EPRT |af|host|port|"""
        if self.af == socket.AF_INET:
            af = 1
        elif self.af == socket.AF_INET6:
            af = 2
        else:
            raise error_proto('unsupported address family')
        fields = ['', repr(af), host, repr(port), '']
        return self.voidcmd('EPRT ' + '|'.join(fields))

    def makeport(self):
        """Listen on a new port and tell the server about it."""
        sock = socket.create_server(("", 0), family=self.af, backlog=1)
        port = sock.getsockname()[1]
        host = self.sock.getsockname()[0]
        if self.af == socket.AF_INET:
            self.sendport(host, port)
        else:
            self.sendeprt(host, port)
        if self.timeout is not _GLOBAL_DEFAULT_TIMEOUT:
            sock.settimeout(self.timeout)
        return sock

    def makepasv(self):
        """Ask for a passive data port: (host, port)."""
        if self.af == socket.AF_INET:
            untrusted_host, port = parse227(self.sendcmd('PASV'))
            if self.trust_server_pasv_ipv4_address:
                host = untrusted_host
            else:
                host = self.sock.getpeername()[0]
        else:
            host, port = parse229(self.sendcmd('EPSV'),
                                  self.sock.getpeername())
        return host, port

    def ntransfercmd(self, cmd, rest=None):
        """Open a data connection for *cmd*; returns (socket, size or
        None), the size taken from a '150 ... (N bytes)' reply."""
        size = None
        if self.passiveserver:
            host, port = self.makepasv()
            conn = socket.create_connection((host, port), self.timeout,
                                            source_address=self.source_address)
            try:
                if rest is not None:
                    self.sendcmd("REST %s" % rest)
                resp = self.sendcmd(cmd)
                # some servers reply 2xx first; the 1xx follows
                if resp[0] == '2':
                    resp = self.getresp()
                if resp[0] != '1':
                    raise error_reply(resp)
            except:
                conn.close()
                raise
        else:
            with self.makeport() as sock:
                if rest is not None:
                    self.sendcmd("REST %s" % rest)
                resp = self.sendcmd(cmd)
                if resp[0] == '2':
                    resp = self.getresp()
                if resp[0] != '1':
                    raise error_reply(resp)
                conn, _addr = sock.accept()
                if self.timeout is not _GLOBAL_DEFAULT_TIMEOUT:
                    conn.settimeout(self.timeout)
        if resp[:3] == '150':
            size = parse150(resp)
        return conn, size

    def transfercmd(self, cmd, rest=None):
        return self.ntransfercmd(cmd, rest)[0]

    def login(self, user='', passwd='', acct=''):
        """USER / PASS / ACCT; anonymous login when *user* is empty."""
        user = user or 'anonymous'
        passwd = passwd or ''
        acct = acct or ''
        if user == 'anonymous' and passwd in {'', '-'}:
            # a '-' asks the server to be terse
            passwd = passwd + 'anonymous@'
        resp = self.sendcmd('USER ' + user)
        if resp[0] == '3':
            resp = self.sendcmd('PASS ' + passwd)
        if resp[0] == '3':
            resp = self.sendcmd('ACCT ' + acct)
        if resp[0] != '2':
            raise error_reply(resp)
        return resp

    def _end_transfer(self, conn):
        if _SSLSocket is not None and isinstance(conn, _SSLSocket):
            conn.unwrap()

    def retrbinary(self, cmd, callback, blocksize=8192, rest=None):
        """Binary retrieval: *callback(block)* for each received block."""
        self.voidcmd('TYPE I')
        with self.transfercmd(cmd, rest) as conn:
            while data := conn.recv(blocksize):
                callback(data)
            self._end_transfer(conn)
        return self.voidresp()

    def retrlines(self, cmd, callback=None):
        """ASCII retrieval: *callback(line)* per line, without EOL
        (default: print it)."""
        if callback is None:
            callback = print_line
        self.sendcmd('TYPE A')
        with self.transfercmd(cmd) as conn, \
                conn.makefile('r', encoding=self.encoding) as fp:
            while True:
                line = fp.readline(self.maxline + 1)
                if len(line) > self.maxline:
                    raise Error("got more than %d bytes" % self.maxline)
                if self.debugging > 2:
                    print('*retr*', repr(line))
                if not line:
                    break
                if line[-2:] == CRLF:
                    line = line[:-2]
                elif line[-1:] == '\n':
                    line = line[:-1]
                callback(line)
            self._end_transfer(conn)
        return self.voidresp()

    def storbinary(self, cmd, fp, blocksize=8192, callback=None, rest=None):
        """Binary upload of the binary file *fp*."""
        self.voidcmd('TYPE I')
        with self.transfercmd(cmd, rest) as conn:
            while buf := fp.read(blocksize):
                conn.sendall(buf)
                if callback:
                    callback(buf)
            self._end_transfer(conn)
        return self.voidresp()

    def storlines(self, cmd, fp, callback=None):
        """ASCII upload of the binary file *fp* with CRLF line ends."""
        self.voidcmd('TYPE A')
        with self.transfercmd(cmd) as conn:
            while True:
                buf = fp.readline(self.maxline + 1)
                if len(buf) > self.maxline:
                    raise Error("got more than %d bytes" % self.maxline)
                if not buf:
                    break
                if buf[-2:] != B_CRLF:
                    if buf[-1] in B_CRLF:
                        buf = buf[:-1]
                    buf = buf + B_CRLF
                conn.sendall(buf)
                if callback:
                    callback(buf)
            self._end_transfer(conn)
        return self.voidresp()

    # convenience commands

    def acct(self, password):
        return self.voidcmd('ACCT ' + password)

    def nlst(self, *args):
        """Names in a directory (NLST)."""
        cmd = 'NLST'
        for arg in args:
            cmd = cmd + (' ' + arg)
        files = []
        self.retrlines(cmd, files.append)
        return files

    def dir(self, *args):
        """LIST, printed or passed to a callback given as last argument."""
        cmd = 'LIST'
        func = None
        if args[-1:] and not isinstance(args[-1], str):
            args, func = args[:-1], args[-1]
        for arg in args:
            if arg:
                cmd = cmd + (' ' + arg)
        self.retrlines(cmd, func)

    def mlsd(self, path="", facts=[]):
        """Iterate over (name, {fact: value}) from MLSD."""
        if facts:
            self.sendcmd("OPTS MLST " + ";".join(facts) + ";")
        cmd = "MLSD %s" % path if path else "MLSD"
        lines = []
        self.retrlines(cmd, lines.append)
        for line in lines:
            found, _, name = line.rstrip(CRLF).partition(' ')
            entry = {}
            for fact in found[:-1].split(";"):
                key, _, value = fact.partition("=")
                entry[key.lower()] = value
            yield (name, entry)

    def rename(self, fromname, toname):
        resp = self.sendcmd('RNFR ' + fromname)
        if resp[0] != '3':
            raise error_reply(resp)
        return self.voidcmd('RNTO ' + toname)

    def delete(self, filename):
        resp = self.sendcmd('DELE ' + filename)
        if resp[:3] in {'250', '200'}:
            return resp
        raise error_reply(resp)

    def cwd(self, dirname):
        if dirname == '..':
            try:
                return self.voidcmd('CDUP')
            except error_perm as msg:
                # CDUP unknown: fall back to CWD ..
                if msg.args[0][:3] != '500':
                    raise
        elif dirname == '':
            dirname = '.'
        return self.voidcmd('CWD ' + dirname)

    def size(self, filename):
        """File size via SIZE (None if the reply is not 213)."""
        resp = self.sendcmd('SIZE ' + filename)
        if resp[:3] == '213':
            return int(resp[3:].strip())

    def mkd(self, dirname):
        resp = self.voidcmd('MKD ' + dirname)
        if not resp.startswith('257'):
            return ''
        return parse257(resp)

    def rmd(self, dirname):
        return self.voidcmd('RMD ' + dirname)

    def pwd(self):
        resp = self.voidcmd('PWD')
        if not resp.startswith('257'):
            return ''
        return parse257(resp)

    def quit(self):
        resp = self.voidcmd('QUIT')
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
                sock.close()


try:
    import ssl
except ImportError:
    _SSLSocket = None
else:
    _SSLSocket = ssl.SSLSocket

    class FTP_TLS(FTP):
        """FTP over explicit TLS (RFC 4217): login() secures the control
        connection, prot_p() the data connections."""

        def __init__(self, host='', user='', passwd='', acct='', *,
                     context=None, timeout=_GLOBAL_DEFAULT_TIMEOUT,
                     source_address=None, encoding='utf-8'):
            if context is None:
                context = ssl._create_stdlib_context()
            self.context = context
            self._prot_p = False
            super().__init__(host, user, passwd, acct, timeout,
                             source_address, encoding=encoding)

        def login(self, user='', passwd='', acct='', secure=True):
            if secure and not isinstance(self.sock, ssl.SSLSocket):
                self.auth()
            return super().login(user, passwd, acct)

        def auth(self):
            """Secure the control connection (AUTH TLS)."""
            if isinstance(self.sock, ssl.SSLSocket):
                raise ValueError("Already using TLS")
            if self.context.protocol >= ssl.PROTOCOL_TLS:
                resp = self.voidcmd('AUTH TLS')
            else:
                resp = self.voidcmd('AUTH SSL')
            self.sock = self.context.wrap_socket(self.sock,
                                                 server_hostname=self.host)
            self.file = self.sock.makefile(mode='r', encoding=self.encoding)
            return resp

        def ccc(self):
            """Return the control connection to plain text."""
            if not isinstance(self.sock, ssl.SSLSocket):
                raise ValueError("not using TLS")
            resp = self.voidcmd('CCC')
            self.sock = self.sock.unwrap()
            return resp

        def prot_p(self):
            """Protect data connections with TLS."""
            self.voidcmd('PBSZ 0')
            resp = self.voidcmd('PROT P')
            self._prot_p = True
            return resp

        def prot_c(self):
            """Plain-text data connections."""
            resp = self.voidcmd('PROT C')
            self._prot_p = False
            return resp

        def ntransfercmd(self, cmd, rest=None):
            conn, size = super().ntransfercmd(cmd, rest)
            if self._prot_p:
                conn = self.context.wrap_socket(conn,
                                                server_hostname=self.host)
            return conn, size

        def abort(self):
            # urgent data cannot be sent over TLS
            line = b'ABOR' + B_CRLF
            self.sock.sendall(line)
            resp = self.getmultiline()
            if resp[:3] not in {'426', '225', '226'}:
                raise error_proto(resp)
            return resp

    __all__.append('FTP_TLS')
    all_errors = (Error, OSError, EOFError, ssl.SSLError)


def parse150(resp):
    """The size from a '150 ... (N bytes)' reply, or None."""
    if resp[:3] != '150':
        raise error_reply(resp)
    global _150_re
    if _150_re is None:
        import re
        _150_re = re.compile(r"150 .* \((\d+) bytes\)",
                             re.IGNORECASE | re.ASCII)
    m = _150_re.match(resp)
    if not m:
        return None
    return int(m.group(1))


_150_re = None
_227_re = None


def parse227(resp):
    """(host, port) from a '227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)'
    reply."""
    if resp[:3] != '227':
        raise error_reply(resp)
    global _227_re
    if _227_re is None:
        import re
        _227_re = re.compile(r'(\d+),(\d+),(\d+),(\d+),(\d+),(\d+)', re.ASCII)
    m = _227_re.search(resp)
    if not m:
        raise error_proto(resp)
    numbers = m.groups()
    host = '.'.join(numbers[:4])
    port = (int(numbers[4]) << 8) + int(numbers[5])
    return host, port


def parse229(resp, peer):
    """(host, port) from a '229 ... (|||port|)' reply; the host is the
    control connection's peer."""
    if resp[:3] != '229':
        raise error_reply(resp)
    left = resp.find('(')
    if left < 0:
        raise error_proto(resp)
    right = resp.find(')', left + 1)
    if right < 0:
        raise error_proto(resp)
    if resp[left + 1] != resp[right - 1]:
        raise error_proto(resp)
    parts = resp[left + 1:right].split(resp[left + 1])
    if len(parts) != 5:
        raise error_proto(resp)
    return peer[0], int(parts[3])


def parse257(resp):
    """The directory name from a '257 "name" ...' reply ("" doubled
    inside)."""
    if resp[:3] != '257':
        raise error_reply(resp)
    if resp[3:5] != ' "':
        return ''
    out = []
    i = 5
    n = len(resp)
    while i < n:
        c = resp[i]
        i += 1
        if c == '"':
            if i >= n or resp[i] != '"':
                break
            i += 1
        out.append(c)
    return ''.join(out)


def print_line(line):
    """Default retrlines() callback."""
    print(line)


def ftpcp(source, sourcename, target, targetname='', type='I'):
    """Copy a file directly between two servers."""
    if not targetname:
        targetname = sourcename
    type = 'TYPE ' + type
    source.voidcmd(type)
    target.voidcmd(type)
    sourcehost, sourceport = parse227(source.sendcmd('PASV'))
    if not source.trust_server_pasv_ipv4_address:
        # as in makepasv(): the advertised host may point anywhere
        sourcehost = source.sock.getpeername()[0]
    target.sendport(sourcehost, sourceport)
    treply = target.sendcmd('STOR ' + targetname)
    if treply[:3] not in {'125', '150'}:
        raise error_proto
    sreply = source.sendcmd('RETR ' + sourcename)
    if sreply[:3] not in {'125', '150'}:
        raise error_proto
    source.voidresp()
    target.voidresp()
