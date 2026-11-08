"""Client side of the Simple Mail Transfer Protocol and its extensions.

Covers plain SMTP (RFC 821/5321), the ESMTP extension mechanism (RFC 1869),
SMTP AUTH (RFC 4954), STARTTLS (RFC 3207), SMTPUTF8 (RFC 6531) and LMTP
(RFC 2033).  Typical use::

    with smtplib.SMTP("mail.example.org") as conn:
        conn.sendmail("me@example.org", ["you@example.org"], text)
"""

import base64
import copy
import datetime
import email.generator
import email.message
import email.utils
import hmac
import io
import re
import socket
import sys
from email.base64mime import body_encode as encode_base64

__all__ = ["SMTPException", "SMTPNotSupportedError", "SMTPServerDisconnected",
           "SMTPResponseException", "SMTPSenderRefused",
           "SMTPRecipientsRefused", "SMTPDataError", "SMTPConnectError",
           "SMTPHeloError", "SMTPAuthenticationError", "quoteaddr",
           "quotedata", "SMTP"]

SMTP_PORT = 25
SMTP_SSL_PORT = 465
LMTP_PORT = 2003
CRLF = "\r\n"
bCRLF = b"\r\n"

# A reply line longer than this is treated as a protocol violation.
_MAXLINE = 8192
# How many 334 continuation rounds one AUTH exchange may take.
_MAXCHALLENGE = 5

# Some old servers announce their mechanisms as "250-AUTH=LOGIN PLAIN".
OLDSTYLE_AUTH = re.compile(r"auth=(.*)", re.I)

_FEATURE_LINE = re.compile(r"([A-Za-z0-9][A-Za-z0-9-]*) ?(.*)")
_ANY_NEWLINE = re.compile(r"\r\n|\r|\n")
_LEADING_DOT = re.compile(br"^\.", re.MULTILINE)


# ---------------------------------------------------------------- errors

class SMTPException(OSError):
    """Root of every error this module raises."""


class SMTPNotSupportedError(SMTPException):
    """The server lacks an extension the requested operation needs."""


class SMTPServerDisconnected(SMTPException):
    """There is no usable connection to the server (any more)."""


# The constructors below store their arguments in `args` themselves instead
# of handing them to OSError, which would read a leading number as errno and
# change str() to the "[Errno n] ..." form.

class SMTPResponseException(SMTPException):
    """The server answered with an error code.

    `smtp_code` holds the numeric code and `smtp_error` the text (bytes).
    """

    def __init__(self, code, msg):
        self.smtp_code, self.smtp_error = code, msg
        self.args = (code, msg)


class SMTPSenderRefused(SMTPResponseException):
    """MAIL FROM was rejected; `sender` names the refused address."""

    def __init__(self, code, msg, sender):
        SMTPResponseException.__init__(self, code, msg)
        self.sender = sender
        self.args += (sender,)


class SMTPRecipientsRefused(SMTPException):
    """No recipient was accepted.

    `recipients` maps each refused address to its (code, message) pair.
    """

    def __init__(self, recipients):
        self.recipients = recipients
        self.args = (recipients,)


class SMTPDataError(SMTPResponseException):
    """The message body was not accepted."""


class SMTPConnectError(SMTPResponseException):
    """The greeting after connecting was not a 220."""


class SMTPHeloError(SMTPResponseException):
    """Neither EHLO nor HELO was accepted."""


class SMTPAuthenticationError(SMTPResponseException):
    """The server rejected the credentials (usually code 535)."""


# ------------------------------------------------------- address helpers

def _parsed_address(addrstring):
    """Return the bare address in *addrstring*, or None if unparsable."""
    name, address = email.utils.parseaddr(addrstring)
    if not name and not address:
        return None
    return address


def quoteaddr(addrstring):
    """Return *addrstring* as an RFC 5321 path: ``<local@domain>``."""
    if addrstring.strip() == "<>":
        return "<>"     # the null reverse-path, without its padding
    address = _parsed_address(addrstring)
    if address is not None:
        return "<%s>" % address
    # Not something parseaddr understands (for instance "<>"): pass it on,
    # adding the angle brackets unless it already has them.
    if addrstring.strip().startswith("<"):
        return addrstring
    return "<%s>" % addrstring


def _addr_only(addrstring):
    address = _parsed_address(addrstring)
    return addrstring if address is None else address


# ------------------------------------------------------ message payloads

def _crlf_lines(text):
    """Turn every kind of line break in *text* into CRLF."""
    return _ANY_NEWLINE.sub(CRLF, text)


def _dot_stuff(payload):
    """Double each period that starts a line (RFC 5321 section 4.5.2)."""
    return _LEADING_DOT.sub(b"..", payload)


def quotedata(data):
    """Prepare a str message body for DATA: CRLF line ends, stuffed dots.

    Kept for compatibility; SMTP.data() does the same on bytes.
    """
    return re.sub(r"(?m)^\.", "..", _crlf_lines(data))


def _wire_body(msg):
    """The bytes to send after DATA, including the final "." line."""
    if isinstance(msg, str):
        msg = _crlf_lines(msg).encode("ascii")
    body = _dot_stuff(msg)
    if not body.endswith(bCRLF):
        body += bCRLF
    return body + b"." + bCRLF


def _ehlo_features(reply):
    """Parse the extension lines of an EHLO reply into a dict.

    Keyword names are lowercased.  AUTH may be announced on several lines
    and in the obsolete ``AUTH=`` form; all its mechanisms are collected
    into one space-separated string.
    """
    features = {}
    for line in reply.decode("latin-1").split("\n")[1:]:
        old_auth = OLDSTYLE_AUTH.match(line)
        if old_auth:
            features["auth"] = features.get("auth", "") + " " + old_auth.group(1)
            continue
        match = _FEATURE_LINE.match(line)
        if match is None:
            continue
        keyword = match.group(1).lower()
        params = match.group(2).strip()
        if keyword == "auth":
            features["auth"] = features.get("auth", "") + " " + params
        else:
            features[keyword] = params
    return features


def _envelope_of(msg):
    """Pick the envelope sender and recipients out of a Message's headers.

    When the message carries a Resent- block the Resent- headers are used;
    more than one such block is ambiguous and rejected.
    """
    resent_dates = msg.get_all("Resent-Date")
    if resent_dates is None:
        prefix = ""
    elif len(resent_dates) == 1:
        prefix = "Resent-"
    else:
        raise ValueError("message has more than one 'Resent-' header block")
    sender_field = prefix + "Sender"
    if sender_field not in msg:
        sender_field = prefix + "From"
    sender = email.utils.getaddresses([msg[sender_field]])[0][1]
    fields = [msg[prefix + name] for name in ("To", "Bcc", "Cc")]
    recipients = [pair[1] for pair in
                  email.utils.getaddresses([f for f in fields if f is not None])]
    return sender, recipients


def _all_ascii(*texts):
    return all(text.isascii() for text in texts)


try:
    import ssl
except ImportError:
    _have_ssl = False
else:
    _have_ssl = True


def _probe_md5():
    # CRAM-MD5 needs HMAC-MD5, which a FIPS-restricted hashlib may refuse.
    try:
        hmac.new(b"k", b"m", "md5")
    except ValueError:
        return False
    return True


_have_cram_md5_support = _probe_md5()


# ----------------------------------------------------------- the client

class SMTP:
    """A connection to an SMTP server.

    Creating the object with a host connects at once (see connect()).
    Commands map onto methods of the same name; sendmail() and
    send_message() run a whole mail transaction.  Interesting attributes:

    helo_resp, ehlo_resp
        The server's reply to the last HELO/EHLO, or None.
    does_esmtp
        Whether the server accepted EHLO.
    esmtp_features
        Extension keyword (lowercase) -> parameter string, from EHLO.
    """

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
        if local_hostname is None:
            local_hostname = self._guess_local_hostname()
        self.local_hostname = local_hostname

    @staticmethod
    def _guess_local_hostname():
        # RFC 5321 wants a fully qualified name, or else an address literal.
        fqdn = socket.getfqdn()
        if '.' in fqdn:
            return fqdn
        address = '127.0.0.1'
        try:
            address = socket.gethostbyname(socket.gethostname())
        except socket.gaierror:
            pass
        return '[%s]' % address

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        try:
            code, text = self.docmd("QUIT")
            if code != 221:
                raise SMTPResponseException(code, text)
        except SMTPServerDisconnected:
            pass
        finally:
            self.close()

    # -- diagnostics

    def set_debuglevel(self, debuglevel):
        """1 traces the conversation on stderr; 2 adds timestamps."""
        self.debuglevel = debuglevel

    def _print_debug(self, *args):
        if self.debuglevel > 1:
            stamp = datetime.datetime.now().time().isoformat("microseconds")
            print(stamp, *args, file=sys.stderr)
        else:
            print(*args, file=sys.stderr)

    # -- the connection

    def _get_socket(self, host, port, timeout):
        if timeout is not None and not timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        if self.debuglevel > 0:
            self._print_debug('connect: to', (host, port), self.source_address)
        return socket.create_connection((host, port), timeout,
                                        self.source_address)

    @staticmethod
    def _split_host_port(host, port):
        # "host:port" is accepted when no port was given; a host with more
        # than one colon is an IPv6 literal and is left alone.
        if port or host.count(':') != 1:
            return host, port
        host, _, port = host.partition(':')
        try:
            return host, int(port)
        except ValueError:
            raise OSError("nonnumeric port") from None

    def connect(self, host='localhost', port=0, source_address=None):
        """Open a connection and return the server's greeting (code, msg).

        *host* may carry the port as "host:port".  Without a port the
        class's default_port is used.
        """
        if source_address:
            self.source_address = source_address
        host, port = self._split_host_port(host, port)
        if not port:
            port = self.default_port
        sys.audit("smtplib.connect", self, host, port)
        self.sock = self._get_socket(host, port, self.timeout)
        self.file = None
        code, msg = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('connect:', repr(msg))
        return code, msg

    def send(self, s):
        """Write *s* (str or bytes) to the server unchanged."""
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
        """Send one command line; CR and LF inside it are refused."""
        line = "%s %s" % (cmd, args) if args else cmd
        if "\r" in line or "\n" in line:
            shown = line.replace("\r", "\\r").replace("\n", "\\n")
            raise ValueError("command and arguments contain prohibited "
                             "newline characters: %s" % shown)
        self.send(line + CRLF)

    def _reply_line(self):
        if self.file is None:
            self.file = self.sock.makefile('rb')
        try:
            line = self.file.readline(_MAXLINE + 1)
        except OSError as exc:
            self.close()
            raise SMTPServerDisconnected(
                "Connection unexpectedly closed: " + str(exc))
        if not line:
            self.close()
            raise SMTPServerDisconnected("Connection unexpectedly closed")
        if self.debuglevel > 0:
            self._print_debug('reply:', repr(line))
        if len(line) > _MAXLINE:
            self.close()
            raise SMTPResponseException(500, "Line too long.")
        return line

    def getreply(self):
        """Read one (possibly multi-line) reply and return (code, text).

        The text of continuation lines is joined with b"\\n".  A reply
        whose code is not a number gives the code -1.
        """
        texts = []
        while True:
            line = self._reply_line()
            texts.append(line[4:].strip(b" \t\r\n"))
            try:
                code = int(line[:3])
            except ValueError:
                code = -1
                break
            if line[3:4] != b"-":
                break
        text = b"\n".join(texts)
        if self.debuglevel > 0:
            self._print_debug('reply: retcode (%s); Msg: %a' % (code, text))
        return code, text

    def docmd(self, cmd, args=""):
        """Send a command and return the reply."""
        self.putcmd(cmd, args)
        return self.getreply()

    # -- greeting

    def helo(self, name=''):
        """Greet with HELO; *name* defaults to local_hostname."""
        self.putcmd("helo", name or self.local_hostname)
        code, msg = self.getreply()
        self.helo_resp = msg
        return code, msg

    def ehlo(self, name=''):
        """Greet with EHLO and record the extensions the server offers."""
        self.esmtp_features = {}
        self.putcmd(self.ehlo_msg, name or self.local_hostname)
        code, msg = self.getreply()
        if code == -1 and not msg:
            # Some servers drop the connection on an unknown command.
            self.close()
            raise SMTPServerDisconnected("Server not connected")
        self.ehlo_resp = msg
        if code == 250:
            self.does_esmtp = True
            self.esmtp_features = _ehlo_features(msg)
        return code, msg

    def ehlo_or_helo_if_needed(self):
        """Greet the server unless that already happened.

        EHLO is tried first; if it fails, HELO.  SMTPHeloError if neither
        gets a 2xx reply.
        """
        if self.helo_resp is not None or self.ehlo_resp is not None:
            return
        if 200 <= self.ehlo()[0] <= 299:
            return
        code, msg = self.helo()
        if not 200 <= code <= 299:
            raise SMTPHeloError(code, msg)

    def has_extn(self, opt):
        """Whether the last EHLO announced extension *opt*."""
        return opt.lower() in self.esmtp_features

    # -- simple commands

    def help(self, args=''):
        """Return the text of the server's HELP reply."""
        self.putcmd("help", args)
        return self.getreply()[1]

    def rset(self):
        """Abort the current mail transaction."""
        self.command_encoding = 'ascii'
        return self.docmd("rset")

    def _rset(self):
        # RSET after a failure, where a vanished server is not an error.
        try:
            self.rset()
        except SMTPServerDisconnected:
            pass

    def noop(self):
        return self.docmd("noop")

    def verify(self, address):
        """Ask the server whether *address* is a valid mailbox (VRFY)."""
        self.putcmd("vrfy", _addr_only(address))
        return self.getreply()

    vrfy = verify

    def expn(self, address):
        """Ask the server to expand a mailing list (EXPN)."""
        self.putcmd("expn", _addr_only(address))
        return self.getreply()

    # -- the mail transaction

    def _option_suffix(self, options):
        # ESMTP parameters only make sense after a successful EHLO.
        if options and self.does_esmtp:
            return " " + " ".join(options)
        return ""

    def mail(self, sender, options=()):
        """Send MAIL FROM; *options* are ESMTP parameters.

        Asking for SMTPUTF8 switches commands to UTF-8, or raises
        SMTPNotSupportedError when the server did not offer it.
        """
        suffix = self._option_suffix(options)
        if suffix and any(opt.lower() == 'smtputf8' for opt in options):
            if not self.has_extn('smtputf8'):
                raise SMTPNotSupportedError('SMTPUTF8 not supported by server')
            self.command_encoding = 'utf-8'
        self.putcmd("mail", "from:%s%s" % (quoteaddr(sender), suffix))
        return self.getreply()

    def rcpt(self, recip, options=()):
        """Send RCPT TO for one recipient."""
        suffix = self._option_suffix(options)
        self.putcmd("rcpt", "to:%s%s" % (quoteaddr(recip), suffix))
        return self.getreply()

    def data(self, msg):
        """Send DATA followed by the message.

        *msg* may be str (ASCII only) or bytes; line ends are normalised for
        str, periods are stuffed and the terminating "." line is added.
        Raises SMTPDataError if the server does not invite the body.
        """
        self.putcmd("data")
        code, repl = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('data:', (code, repl))
        if code != 354:
            raise SMTPDataError(code, repl)
        self.send(_wire_body(msg))
        code, msg = self.getreply()
        if self.debuglevel > 0:
            self._print_debug('data:', (code, msg))
        return code, msg

    def _give_up(self, code):
        # After a refused step: a 421 means the server is going away, so drop
        # the connection; otherwise reset the transaction and stay connected.
        if code == 421:
            self.close()
        else:
            self._rset()

    def sendmail(self, from_addr, to_addrs, msg, mail_options=(),
                 rcpt_options=()):
        """Run a complete mail transaction.

        *to_addrs* is one address or a list of them; *msg* is str (ASCII,
        line ends are converted to CRLF) or bytes.  Returns a dict with an
        entry (code, message) for every recipient that was refused while
        others were accepted.  Raises SMTPHeloError, SMTPSenderRefused,
        SMTPRecipientsRefused (nobody accepted), SMTPDataError or
        SMTPNotSupportedError.
        """
        self.ehlo_or_helo_if_needed()
        if isinstance(msg, str):
            msg = _crlf_lines(msg).encode('ascii')
        options = []
        if self.does_esmtp:
            if self.has_extn('size'):
                options.append("size=%d" % len(msg))
            options.extend(mail_options)

        code, resp = self.mail(from_addr, options)
        if code != 250:
            self._give_up(code)
            raise SMTPSenderRefused(code, resp, from_addr)

        if isinstance(to_addrs, str):
            to_addrs = [to_addrs]
        refused = {}
        for recipient in to_addrs:
            code, resp = self.rcpt(recipient, rcpt_options)
            if code not in (250, 251):
                refused[recipient] = (code, resp)
            if code == 421:
                self.close()
                raise SMTPRecipientsRefused(refused)
        if len(refused) == len(to_addrs):
            self._rset()
            raise SMTPRecipientsRefused(refused)

        code, resp = self.data(msg)
        if code != 250:
            self._give_up(code)
            raise SMTPDataError(code, resp)
        return refused

    def send_message(self, msg, from_addr=None, to_addrs=None,
                     mail_options=(), rcpt_options=()):
        """Send an email.message.Message through sendmail().

        Missing envelope addresses are taken from the headers (Sender or
        From, and To/Cc/Bcc; the Resent- variants if the message has one
        Resent- block).  Bcc and Resent-Bcc are not transmitted.  When an
        address needs more than ASCII the server must offer SMTPUTF8, and
        the message is then sent with SMTPUTF8 and BODY=8BITMIME.
        """
        self.ehlo_or_helo_if_needed()
        if from_addr is None or to_addrs is None:
            header_from, header_to = _envelope_of(msg)
            if from_addr is None:
                from_addr = header_from
            if to_addrs is None:
                to_addrs = header_to

        outgoing = copy.copy(msg)
        del outgoing['Bcc']
        del outgoing['Resent-Bcc']

        policy = None
        if not _all_ascii(from_addr, *to_addrs):
            if not self.has_extn('smtputf8'):
                raise SMTPNotSupportedError(
                    "One or more source or delivery addresses require "
                    "internationalized email support, but the server "
                    "does not advertise the required SMTPUTF8 capability")
            policy = msg.policy.clone(utf8=True)
            mail_options = (*mail_options, 'SMTPUTF8', 'BODY=8BITMIME')

        with io.BytesIO() as buffer:
            if policy is None:
                writer = email.generator.BytesGenerator(buffer)
            else:
                writer = email.generator.BytesGenerator(buffer, policy=policy)
            writer.flatten(outgoing, linesep='\r\n')
            flat = buffer.getvalue()
        return self.sendmail(from_addr, to_addrs, flat, mail_options,
                             rcpt_options)

    # -- authentication

    def auth(self, mechanism, authobject, *, initial_response_ok=True):
        """Run one SMTP AUTH exchange.

        *authobject* is called with the decoded server challenge (bytes) and
        returns the answer as a str; for the optional initial response
        (RFC 4954) it is called with no argument and may return None to
        decline.  Returns (code, msg) on success, raises
        SMTPAuthenticationError otherwise.
        """
        mechanism = mechanism.upper()
        first = authobject() if initial_response_ok else None
        if first is None:
            self._auth_challenge_count = 0
            code, resp = self.docmd("AUTH", mechanism)
        else:
            self._auth_challenge_count = 1
            code, resp = self.docmd(
                "AUTH", mechanism + " " + self._b64(first))
        while code == 334:
            self._auth_challenge_count += 1
            answer = authobject(base64.decodebytes(resp))
            code, resp = self.docmd(self._b64(answer))
            if self._auth_challenge_count > _MAXCHALLENGE:
                raise SMTPException(
                    "Server AUTH mechanism infinite loop. Last response: "
                    + repr((code, resp)))
        if code in (235, 503):
            return code, resp
        raise SMTPAuthenticationError(code, resp)

    @staticmethod
    def _b64(text):
        return encode_base64(text.encode('ascii'), eol='')

    def auth_cram_md5(self, challenge=None):
        """CRAM-MD5 answer: the user name and the HMAC-MD5 of the challenge."""
        if challenge is None:
            return None     # no initial response for this mechanism
        if not _have_cram_md5_support:
            raise SMTPException("CRAM-MD5 is not supported")
        digest = hmac.HMAC(self.password.encode('ascii'), challenge, 'md5')
        return "%s %s" % (self.user, digest.hexdigest())

    def auth_plain(self, challenge=None):
        """PLAIN answer: NUL user NUL password."""
        return "\0%s\0%s" % (self.user, self.password)

    def auth_login(self, challenge=None):
        """LOGIN answers: the user name first, the password after that."""
        if challenge is None or self._auth_challenge_count < 2:
            return self.user
        return self.password

    def login(self, user, password, *, initial_response_ok=True):
        """Authenticate with the best mechanism both sides support.

        Tried in the order CRAM-MD5 (when MD5 is available), PLAIN, LOGIN;
        the next one is tried when the server rejects the credentials.
        Raises SMTPHeloError, SMTPNotSupportedError (no AUTH), SMTPException
        (no common mechanism) or the last SMTPAuthenticationError.
        """
        self.ehlo_or_helo_if_needed()
        if not self.has_extn("auth"):
            raise SMTPNotSupportedError(
                "SMTP AUTH extension not supported by server.")
        offered = self.esmtp_features["auth"].split()
        ours = ("CRAM-MD5", "PLAIN", "LOGIN") if _have_cram_md5_support \
            else ("PLAIN", "LOGIN")
        candidates = [name for name in ours if name in offered]
        if not candidates:
            raise SMTPException("No suitable authentication method found.")

        self.user, self.password = user, password
        failure = None
        for name in candidates:
            handler = getattr(self, "auth_" + name.lower().replace("-", "_"))
            try:
                code, resp = self.auth(name, handler,
                                       initial_response_ok=initial_response_ok)
            except SMTPAuthenticationError as exc:
                failure = exc
                continue
            if code in (235, 503):
                return code, resp
        raise failure

    # -- TLS

    def starttls(self, *, context=None):
        """Upgrade the connection to TLS.

        Greets first if needed; afterwards the greeting state is cleared,
        as RFC 3207 requires a fresh EHLO.  Raises SMTPNotSupportedError
        when STARTTLS is not offered and SMTPResponseException when it is
        refused.
        """
        self.ehlo_or_helo_if_needed()
        if not self.has_extn("starttls"):
            raise SMTPNotSupportedError(
                "STARTTLS extension not supported by server.")
        code, reply = self.docmd("STARTTLS")
        if code != 220:
            raise SMTPResponseException(code, reply)
        if not _have_ssl:
            raise RuntimeError("No SSL support included in this Python")
        if context is None:
            context = ssl._create_stdlib_context()
        self.sock = context.wrap_socket(self.sock, server_hostname=self._host)
        self.file = None
        self._forget_greeting()
        return code, reply

    # -- shutting down

    def _forget_greeting(self):
        self.helo_resp = None
        self.ehlo_resp = None
        self.esmtp_features = {}
        self.does_esmtp = False

    def close(self):
        """Drop the connection without saying goodbye."""
        reader, self.file = self.file, None
        try:
            if reader:
                reader.close()
        finally:
            sock, self.sock = self.sock, None
            if sock:
                sock.close()

    def quit(self):
        """Send QUIT and close; returns the server's reply."""
        reply = self.docmd("quit")
        self._forget_greeting()
        self.close()
        return reply


if _have_ssl:

    class SMTP_SSL(SMTP):
        """SMTP over a connection that is TLS from the first byte.

        *context* is an ssl.SSLContext; the default one verifies the server
        certificate.
        """

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
            plain = super()._get_socket(host, port, timeout)
            return self.context.wrap_socket(plain, server_hostname=self._host)

    __all__.append("SMTP_SSL")


class LMTP(SMTP):
    """Local Mail Transfer Protocol client (RFC 2033).

    The greeting is LHLO.  Besides host names, *host* may be the path of
    a Unix domain socket (anything starting with "/").
    """

    ehlo_msg = "lhlo"

    def __init__(self, host='', port=LMTP_PORT, local_hostname=None,
                 source_address=None, timeout=socket._GLOBAL_DEFAULT_TIMEOUT):
        super().__init__(host, port, local_hostname=local_hostname,
                         source_address=source_address, timeout=timeout)

    def connect(self, host='localhost', port=0, source_address=None):
        if not host.startswith('/'):
            return super().connect(host, port, source_address=source_address)
        if self.timeout is not None and not self.timeout:
            raise ValueError('Non-blocking socket (timeout=0) is not supported')
        self.file = None
        try:
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            if self.timeout is not socket._GLOBAL_DEFAULT_TIMEOUT:
                self.sock.settimeout(self.timeout)
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


def _interactive():
    # Minimal manual test: read headers and body from stdin, send via
    # localhost.
    sender = input("From: ")
    recipients = input("To: ").split(",")
    print("Message body, end with end-of-file:")
    body = sys.stdin.read()
    print("Message length is %d" % len(body))
    with SMTP("localhost") as conn:
        conn.set_debuglevel(1)
        conn.sendmail(sender, recipients, body)


if __name__ == '__main__':
    _interactive()
