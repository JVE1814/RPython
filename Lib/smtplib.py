"""SMTP/ESMTP client (RFC 821, RFC 1869, RFC 2554, RFC 3207, RFC 6531).

    s = smtplib.SMTP("localhost")
    s.sendmail(from_addr, to_addrs, msg)
    s.quit()
"""

import socket
import io
import re
import email.utils
import email.message
import email.generator
import base64
import hmac
import copy
import datetime
import sys
from email.base64mime import body_encode as encode_base64

__all__ = ["SMTPException", "SMTPNotSupportedError", "SMTPServerDisconnected",
           "SMTPResponseException", "SMTPSenderRefused",
           "SMTPRecipientsRefused", "SMTPDataError", "SMTPConnectError",
           "SMTPHeloError", "SMTPAuthenticationError",
           "quoteaddr", "quotedata", "SMTP"]

SMTP_PORT = 25
SMTP_SSL_PORT = 465
CRLF = "\r\n"
bCRLF = b"\r\n"
_MAXLINE = 8192        # longest reply line accepted
_MAXCHALLENGE = 5      # AUTH challenges before giving up

OLDSTYLE_AUTH = re.compile(r"auth=(.*)", re.I)


class SMTPException(OSError):
    """Base class of all smtplib exceptions."""


class SMTPNotSupportedError(SMTPException):
    """The server does not support the requested command or option."""


class SMTPServerDisconnected(SMTPException):
    """The connection was closed unexpectedly or never opened."""


class SMTPResponseException(SMTPException):
    """The server answered with an error code (smtp_code, smtp_error)."""

    def __init__(self, code, msg):
        self.smtp_code = code
        self.smtp_error = msg
        self.args = (code, msg)


class SMTPSenderRefused(SMTPResponseException):
    """The sender address was refused."""

    def __init__(self, code, msg, sender):
        self.smtp_code = code
        self.smtp_error = msg
        self.sender = sender
        self.args = (code, msg, sender)


class SMTPRecipientsRefused(SMTPException):
    """Every recipient was refused; .recipients maps each to its error."""

    def __init__(self, recipients):
        self.recipients = recipients
        self.args = (recipients,)


class SMTPDataError(SMTPResponseException):
    """The message data was refused."""


class SMTPConnectError(SMTPResponseException):
    """The server greeting was not 220."""


class SMTPHeloError(SMTPResponseException):
    """HELO/EHLO was refused."""


class SMTPAuthenticationError(SMTPResponseException):
    """Authentication failed."""


def quoteaddr(addrstring):
    """Put an address in angle brackets for MAIL FROM / RCPT TO."""
    if addrstring.strip() == '<>':
        return "<>"
    displayname, addr = email.utils.parseaddr(addrstring)
    if (displayname, addr) == ('', ''):
        # unparsable: pass it through, bracketed
        if addrstring.strip().startswith('<'):
            return addrstring
        return "<%s>" % addrstring
    return "<%s>" % addr


def _addr_only(addrstring):
    displayname, addr = email.utils.parseaddr(addrstring)
    if (displayname, addr) == ('', ''):
        return addrstring
    return addr


_EOL_RE = re.compile(r'(?:\r\n|\n|\r(?!\n))')


def _fix_eols(data):
    return _EOL_RE.sub(CRLF, data)


def quotedata(data):
    """CRLF line endings and leading dots doubled (RFC 5321 4.5.2)."""
    return re.sub(r'(?m)^\.', '..', _fix_eols(data))


def _quote_periods(bindata):
    return re.sub(br'(?m)^\.', b'..', bindata)


try:
    import ssl
except ImportError:
    _have_ssl = False
else:
    _have_ssl = True


class SMTP:
    """An SMTP connection.

    SMTP(host='', port=0, local_hostname=None, timeout=..., source_address=None)
    connects at once when *host* is given."""

    debuglevel = 0
    sock = None
    file = None
    helo_resp = None
    ehlo_msg = "ehlo"
    ehlo_resp = None
    does_esmtp = False
    default_port = SMTP_PORT

    def __init__(self, host='', port=0, local_hostname=None,
                 timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                 source_address=None):
        self._host = host
        self.timeout = timeout
        self.esmtp_features = {}
        self.command_encoding = 'ascii'
        self.source_address = source_address
        self._auth_challenge_count = 0

        if host:
            code, msg = self.connect(host, port)
            if code != 220:
                self.close()
                raise SMTPConnectError(code, msg)
        if local_hostname is not None:
            self.local_hostname = local_hostname
        else:
            # RFC 5321 wants a FQDN or an address literal
            fqdn = socket.getfqdn()
            if '.' in fqdn:
                self.local_hostname = fqdn
            else:
                addr = '127.0.0.1'
                try:
                    addr = socket.gethostbyname(socket.gethostname())
                except socket.gaierror:
                    pass
                self.local_hostname = '[%s]' % addr

    def __enter__(self):
        return self

    def __exit__(self, *args):
        try:
            code, message = self.docmd("QUIT")
            if code != 221:
                raise SMTPResponseException(code, message)
        except SMTPServerDisconnected:
            pass
        finally:
            self.close()

    def set_debuglevel(self, debuglevel):
        """1: print the dialogue; 2: also timestamps."""
        self.debuglevel = debuglevel

    def _print_debug(self, *args):
        if self.debuglevel > 1:
            print(datetime.datetime.now().time(), *args, file=sys.stderr)
        else:
            print(*args, file=sys.stderr)

    def _get_socket(self, host, port, timeout):
        if timeout is not None and not timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        if self.debuglevel > 0:
            self._print_debug('connect: to', (host, port), self.source_address)
        return socket.create_connection((host, port), timeout,
                                        self.source_address)

    def connect(self, host='localhost', port=0, source_address=None):
        """Connect to host:port (the port may be given as 'host:port');
        returns the greeting (code, message)."""
        if source_address:
            self.source_address = source_address
        if not port and host.find(':') == host.rfind(':'):
            i = host.rfind(':')
            if i >= 0:
                host, port = host[:i], host[i + 1:]
                try:
                    port = int(port)
                except ValueError:
                    raise OSError("nonnumeric port")
        if not port:
            port = self.default_port
        sys.audit("smtplib.connect", self, host, port)
        self.sock = self._get_socket(host, port, self.timeout)
        self.file = None
        code, msg = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('connect:', repr(msg))
        return (code, msg)

    def send(self, s):
        """Send *s* (str encoded with command_encoding, or bytes)."""
        if self.debuglevel > 0:
            self._print_debug('send:', repr(s))
        if not self.sock:
            raise SMTPServerDisconnected('please run connect() first')
        if isinstance(s, str):
            s = s.encode(self.command_encoding)
        sys.audit("smtplib.send", self, s)
        try:
            self.sock.sendall(s)
        except OSError:
            self.close()
            raise SMTPServerDisconnected('Server not connected')

    def putcmd(self, cmd, args=""):
        """Send one command line."""
        s = cmd if args == "" else f'{cmd} {args}'
        if '\r' in s or '\n' in s:
            s = s.replace('\n', '\\n').replace('\r', '\\r')
            raise ValueError(
                f'command and arguments contain prohibited newline '
                f'characters: {s}')
        self.send(f'{s}{CRLF}')

    def getreply(self):
        """Read a (possibly multi-line) reply: (code, message bytes);
        code is -1 when it is not a number."""
        lines = []
        if self.file is None:
            self.file = self.sock.makefile('rb')
        while True:
            try:
                line = self.file.readline(_MAXLINE + 1)
            except OSError as e:
                self.close()
                raise SMTPServerDisconnected("Connection unexpectedly closed: "
                                             + str(e))
            if not line:
                self.close()
                raise SMTPServerDisconnected("Connection unexpectedly closed")
            if self.debuglevel > 0:
                self._print_debug('reply:', repr(line))
            if len(line) > _MAXLINE:
                self.close()
                raise SMTPResponseException(500, "Line too long.")
            lines.append(line[4:].strip(b' \t\r\n'))
            try:
                errcode = int(line[:3])
            except ValueError:
                errcode = -1
                break
            if line[3:4] != b"-":
                break
        errmsg = b"\n".join(lines)
        if self.debuglevel > 0:
            self._print_debug('reply: retcode (%s); Msg: %a' % (errcode, errmsg))
        return errcode, errmsg

    def docmd(self, cmd, args=""):
        """Send a command and return its reply."""
        self.putcmd(cmd, args)
        return self.getreply()

    # standard commands

    def helo(self, name=''):
        self.putcmd("helo", name or self.local_hostname)
        code, msg = self.getreply()
        self.helo_resp = msg
        return code, msg

    def ehlo(self, name=''):
        """EHLO; on success the advertised extensions go into
        esmtp_features (lowercase name -> parameters)."""
        self.esmtp_features = {}
        self.putcmd(self.ehlo_msg, name or self.local_hostname)
        code, msg = self.getreply()
        if code == -1 and len(msg) == 0:
            self.close()
            raise SMTPServerDisconnected("Server not connected")
        self.ehlo_resp = msg
        if code != 250:
            return code, msg
        self.does_esmtp = True
        assert isinstance(self.ehlo_resp, bytes), repr(self.ehlo_resp)
        for line in self.ehlo_resp.decode("latin-1").split('\n')[1:]:
            # old servers announce "AUTH=method ..."
            old_auth = OLDSTYLE_AUTH.match(line)
            if old_auth:
                self.esmtp_features["auth"] = (
                    self.esmtp_features.get("auth", "") + " "
                    + old_auth.groups(0)[0])
                continue
            m = re.match(r'(?P<feature>[A-Za-z0-9][A-Za-z0-9\-]*) ?', line)
            if m:
                feature = m.group("feature").lower()
                params = m.string[m.end("feature"):].strip()
                if feature == "auth":
                    self.esmtp_features[feature] = (
                        self.esmtp_features.get(feature, "") + " " + params)
                else:
                    self.esmtp_features[feature] = params
        return code, msg

    def has_extn(self, opt):
        return opt.lower() in self.esmtp_features

    def help(self, args=''):
        self.putcmd("help", args)
        return self.getreply()[1]

    def rset(self):
        self.command_encoding = 'ascii'
        return self.docmd("rset")

    def _rset(self):
        # RSET where the connection may already be gone
        try:
            self.rset()
        except SMTPServerDisconnected:
            pass

    def noop(self):
        return self.docmd("noop")

    def mail(self, sender, options=()):
        optionlist = ''
        if options and self.does_esmtp:
            if any(x.lower() == 'smtputf8' for x in options):
                if self.has_extn('smtputf8'):
                    self.command_encoding = 'utf-8'
                else:
                    raise SMTPNotSupportedError(
                        'SMTPUTF8 not supported by server')
            optionlist = ' ' + ' '.join(options)
        self.putcmd("mail", "from:%s%s" % (quoteaddr(sender), optionlist))
        return self.getreply()

    def rcpt(self, recip, options=()):
        optionlist = ''
        if options and self.does_esmtp:
            optionlist = ' ' + ' '.join(options)
        self.putcmd("rcpt", "to:%s%s" % (quoteaddr(recip), optionlist))
        return self.getreply()

    def data(self, msg):
        """DATA: send *msg* (str or bytes) with dot-stuffing."""
        self.putcmd("data")
        code, repl = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('data:', (code, repl))
        if code != 354:
            raise SMTPDataError(code, repl)
        if isinstance(msg, str):
            msg = _fix_eols(msg).encode('ascii')
        q = _quote_periods(msg)
        if q[-2:] != bCRLF:
            q = q + bCRLF
        q = q + b"." + bCRLF
        self.send(q)
        code, msg = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('data:', (code, msg))
        return code, msg

    def verify(self, address):
        self.putcmd("vrfy", _addr_only(address))
        return self.getreply()

    vrfy = verify

    def expn(self, address):
        self.putcmd("expn", _addr_only(address))
        return self.getreply()

    # higher level

    def ehlo_or_helo_if_needed(self):
        """EHLO (falling back to HELO) unless already done."""
        if self.helo_resp is None and self.ehlo_resp is None:
            if not (200 <= self.ehlo()[0] <= 299):
                code, resp = self.helo()
                if not (200 <= code <= 299):
                    raise SMTPHeloError(code, resp)

    def auth(self, mechanism, authobject, *, initial_response_ok=True):
        """Run the AUTH exchange; *authobject(challenge)* produces each
        response (called with None for the initial response)."""
        mechanism = mechanism.upper()
        initial = authobject() if initial_response_ok else None
        if initial is not None:
            response = encode_base64(initial.encode('ascii'), eol='')
            code, resp = self.docmd("AUTH", mechanism + " " + response)
            self._auth_challenge_count = 1
        else:
            code, resp = self.docmd("AUTH", mechanism)
            self._auth_challenge_count = 0
        while code == 334:
            self._auth_challenge_count += 1
            challenge = base64.decodebytes(resp)
            response = encode_base64(authobject(challenge).encode('ascii'),
                                     eol='')
            code, resp = self.docmd(response)
            if self._auth_challenge_count > _MAXCHALLENGE:
                raise SMTPException(
                    "Server AUTH mechanism infinite loop. Last response: "
                    + repr((code, resp)))
        if code in (235, 503):
            return code, resp
        raise SMTPAuthenticationError(code, resp)

    def auth_cram_md5(self, challenge=None):
        if challenge is None:
            return None
        if not _have_cram_md5_support:
            raise SMTPException("CRAM-MD5 is not supported")
        digest = hmac.HMAC(self.password.encode('ascii'), challenge,
                           'md5').hexdigest()
        return self.user + " " + digest

    def auth_plain(self, challenge=None):
        return "\0%s\0%s" % (self.user, self.password)

    def auth_login(self, challenge=None):
        if challenge is None or self._auth_challenge_count < 2:
            return self.user
        return self.password

    def login(self, user, password, *, initial_response_ok=True):
        """Authenticate with the best mechanism both sides support."""
        self.ehlo_or_helo_if_needed()
        if not self.has_extn("auth"):
            raise SMTPNotSupportedError(
                "SMTP AUTH extension not supported by server.")
        advertised = self.esmtp_features["auth"].split()
        preferred = ['PLAIN', 'LOGIN']
        if _have_cram_md5_support:
            preferred.insert(0, 'CRAM-MD5')
        authlist = [auth for auth in preferred if auth in advertised]
        if not authlist:
            raise SMTPException("No suitable authentication method found.")
        self.user, self.password = user, password
        last_exception = None
        for method in authlist:
            handler = getattr(self,
                              'auth_' + method.lower().replace('-', '_'))
            try:
                code, resp = self.auth(method, handler,
                                       initial_response_ok=initial_response_ok)
                if code in (235, 503):
                    return code, resp
            except SMTPAuthenticationError as e:
                last_exception = e
        raise last_exception

    def starttls(self, *, context=None):
        """Switch the connection to TLS."""
        self.ehlo_or_helo_if_needed()
        if not self.has_extn("starttls"):
            raise SMTPNotSupportedError(
                "STARTTLS extension not supported by server.")
        resp, reply = self.docmd("STARTTLS")
        if resp != 220:
            raise SMTPResponseException(resp, reply)
        if not _have_ssl:
            raise RuntimeError("No SSL support included in this Python")
        if context is None:
            context = ssl._create_stdlib_context()
        self.sock = context.wrap_socket(self.sock, server_hostname=self._host)
        self.file = None
        # RFC 3207: forget everything learned before TLS
        self.helo_resp = None
        self.ehlo_resp = None
        self.esmtp_features = {}
        self.does_esmtp = False
        return resp, reply

    def sendmail(self, from_addr, to_addrs, msg, mail_options=(),
                 rcpt_options=()):
        """Send *msg* (str or bytes) from *from_addr* to *to_addrs*.

        Returns a dict of the refused recipients; raises when all are
        refused or the sender or data is refused."""
        self.ehlo_or_helo_if_needed()
        esmtp_opts = []
        if isinstance(msg, str):
            msg = _fix_eols(msg).encode('ascii')
        if self.does_esmtp:
            if self.has_extn('size'):
                esmtp_opts.append("size=%d" % len(msg))
            esmtp_opts.extend(mail_options)
        code, resp = self.mail(from_addr, esmtp_opts)
        if code != 250:
            if code == 421:
                self.close()
            else:
                self._rset()
            raise SMTPSenderRefused(code, resp, from_addr)
        refused = {}
        if isinstance(to_addrs, str):
            to_addrs = [to_addrs]
        for each in to_addrs:
            code, resp = self.rcpt(each, rcpt_options)
            if code != 250 and code != 251:
                refused[each] = (code, resp)
            if code == 421:
                self.close()
                raise SMTPRecipientsRefused(refused)
        if len(refused) == len(to_addrs):
            self._rset()
            raise SMTPRecipientsRefused(refused)
        code, resp = self.data(msg)
        if code != 250:
            if code == 421:
                self.close()
            else:
                self._rset()
            raise SMTPDataError(code, resp)
        return refused

    def send_message(self, msg, from_addr=None, to_addrs=None,
                     mail_options=(), rcpt_options=()):
        """Send an email.message.Message; addresses default to the
        (Resent-)Sender/From and To/Cc/Bcc headers.  Bcc headers are not
        transmitted."""
        self.ehlo_or_helo_if_needed()
        resent = msg.get_all('Resent-Date')
        if resent is None:
            prefix = ''
        elif len(resent) == 1:
            prefix = 'Resent-'
        else:
            raise ValueError("message has more than one 'Resent-' header block")
        if from_addr is None:
            if prefix + 'Sender' in msg:
                from_addr = msg[prefix + 'Sender']
            else:
                from_addr = msg[prefix + 'From']
            from_addr = email.utils.getaddresses([from_addr])[0][1]
        if to_addrs is None:
            fields = [f for f in (msg[prefix + 'To'], msg[prefix + 'Bcc'],
                                  msg[prefix + 'Cc']) if f is not None]
            to_addrs = [a[1] for a in email.utils.getaddresses(fields)]
        msg_copy = copy.copy(msg)
        del msg_copy['Bcc']
        del msg_copy['Resent-Bcc']
        international = False
        try:
            ''.join([from_addr, *to_addrs]).encode('ascii')
        except UnicodeEncodeError:
            if not self.has_extn('smtputf8'):
                raise SMTPNotSupportedError(
                    "One or more source or delivery addresses require "
                    "internationalized email support, but the server "
                    "does not advertise the required SMTPUTF8 capability")
            international = True
        with io.BytesIO() as out:
            if international:
                gen = email.generator.BytesGenerator(
                    out, policy=msg.policy.clone(utf8=True))
                mail_options = (*mail_options, 'SMTPUTF8', 'BODY=8BITMIME')
            else:
                gen = email.generator.BytesGenerator(out)
            gen.flatten(msg_copy, linesep='\r\n')
            flatmsg = out.getvalue()
        return self.sendmail(from_addr, to_addrs, flatmsg, mail_options,
                             rcpt_options)

    def close(self):
        """Close the connection (without QUIT)."""
        try:
            file = self.file
            self.file = None
            if file:
                file.close()
        finally:
            sock = self.sock
            self.sock = None
            if sock:
                sock.close()

    def quit(self):
        """QUIT and close."""
        res = self.docmd("quit")
        self.ehlo_resp = self.helo_resp = None
        self.esmtp_features = {}
        self.does_esmtp = False
        self.close()
        return res


def _md5_available():
    try:
        hmac.HMAC(b'test', b'test', 'md5')
    except ValueError:
        return False
    return True


_have_cram_md5_support = _md5_available()


if _have_ssl:

    class SMTP_SSL(SMTP):
        """SMTP over an implicit TLS connection (port 465)."""

        default_port = SMTP_SSL_PORT

        def __init__(self, host='', port=0, local_hostname=None, *,
                     timeout=socket._GLOBAL_DEFAULT_TIMEOUT,
                     source_address=None, context=None):
            if context is None:
                context = ssl._create_stdlib_context()
            self.context = context
            SMTP.__init__(self, host, port, local_hostname, timeout,
                          source_address)

        def _get_socket(self, host, port, timeout):
            if self.debuglevel > 0:
                self._print_debug('connect:', (host, port))
            new_socket = super()._get_socket(host, port, timeout)
            return self.context.wrap_socket(new_socket,
                                            server_hostname=self._host)

    __all__.append("SMTP_SSL")


LMTP_PORT = 2003


class LMTP(SMTP):
    """LMTP (RFC 2033): like SMTP with LHLO; the host may be a Unix
    socket path."""

    ehlo_msg = "lhlo"

    def __init__(self, host='', port=LMTP_PORT, local_hostname=None,
                 source_address=None,
                 timeout=socket._GLOBAL_DEFAULT_TIMEOUT):
        super().__init__(host, port, local_hostname=local_hostname,
                         source_address=source_address, timeout=timeout)

    def connect(self, host='localhost', port=0, source_address=None):
        if host[0] != '/':
            return super().connect(host, port, source_address=source_address)
        if self.timeout is not None and not self.timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        try:
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            if self.timeout is not socket._GLOBAL_DEFAULT_TIMEOUT:
                self.sock.settimeout(self.timeout)
            self.file = None
            self.sock.connect(host)
        except OSError:
            if self.debuglevel > 0:
                self._print_debug('connect fail:', host)
            if self.sock:
                self.sock.close()
            self.sock = None
            raise
        code, msg = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('connect:', msg)
        return code, msg
