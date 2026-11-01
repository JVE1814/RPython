"""TLS support for sockets and in-memory transports.

The protocol engine lives in the _ssl extension (OpenSSL).  This module adds
the user-facing classes (SSLContext, SSLSocket, SSLObject), the enumerations
used for their attributes and helper functions such as
create_default_context() and get_server_certificate().
"""

import base64
import errno
import os
import sys
import warnings
from collections import namedtuple
from enum import Enum as _Enum, IntEnum as _IntEnum, IntFlag as _IntFlag
from enum import _simple_enum

import _ssl
from _ssl import OPENSSL_VERSION_NUMBER, OPENSSL_VERSION_INFO, OPENSSL_VERSION
from _ssl import _SSLContext, MemoryBIO, SSLSession
from _ssl import (SSLError, SSLZeroReturnError, SSLWantReadError,
                  SSLWantWriteError, SSLSyscallError, SSLEOFError,
                  SSLCertVerificationError)
from _ssl import txt2obj as _txt2obj, nid2obj as _nid2obj
from _ssl import RAND_status, RAND_add, RAND_bytes
from _ssl import (HAS_SNI, HAS_ECDH, HAS_NPN, HAS_ALPN,
                  HAS_NEVER_CHECK_COMMON_NAME, HAS_SSLv2, HAS_SSLv3,
                  HAS_TLSv1, HAS_TLSv1_1, HAS_TLSv1_2, HAS_TLSv1_3,
                  HAS_PSK, HAS_PSK_TLS13, HAS_PHA)
from _ssl import _DEFAULT_CIPHERS, _OPENSSL_API_VERSION

import socket as _socket
from socket import socket, SOCK_STREAM, SOL_SOCKET, SO_TYPE
from socket import create_connection, _GLOBAL_DEFAULT_TIMEOUT


# --------------------------------------------------------------- enumerations
# The _ssl constants are exposed both as enum members and module globals.

def _enum_from_ssl(kind, name, prefix, exclude=()):
    kind._convert_(name, __name__,
                   lambda n: n.startswith(prefix) and n not in exclude,
                   source=_ssl)


_enum_from_ssl(_IntEnum, '_SSLMethod', 'PROTOCOL_', ('PROTOCOL_SSLv23',))
_enum_from_ssl(_IntFlag, 'Options', 'OP_')
_enum_from_ssl(_IntEnum, 'AlertDescription', 'ALERT_DESCRIPTION_')
_enum_from_ssl(_IntEnum, 'SSLErrorNumber', 'SSL_ERROR_')
_enum_from_ssl(_IntFlag, 'VerifyFlags', 'VERIFY_')
_enum_from_ssl(_IntEnum, 'VerifyMode', 'CERT_')

PROTOCOL_SSLv23 = _SSLMethod.PROTOCOL_SSLv23 = _SSLMethod.PROTOCOL_TLS
_PROTOCOL_NAMES = {member: name
                   for name, member in _SSLMethod.__members__.items()}


@_simple_enum(_IntEnum)
class TLSVersion:
    MINIMUM_SUPPORTED = _ssl.PROTO_MINIMUM_SUPPORTED
    SSLv3 = _ssl.PROTO_SSLv3
    TLSv1 = _ssl.PROTO_TLSv1
    TLSv1_1 = _ssl.PROTO_TLSv1_1
    TLSv1_2 = _ssl.PROTO_TLSv1_2
    TLSv1_3 = _ssl.PROTO_TLSv1_3
    MAXIMUM_SUPPORTED = _ssl.PROTO_MAXIMUM_SUPPORTED


# The docstrings of the three enumerations below are compared by the test-suite.

@_simple_enum(_IntEnum)
class _TLSContentType:
    """Content types (record layer)

    See RFC 8446, section B.1
    """
    CHANGE_CIPHER_SPEC = 20
    ALERT = 21
    HANDSHAKE = 22
    APPLICATION_DATA = 23
    HEADER = 0x100
    INNER_CONTENT_TYPE = 0x101


@_simple_enum(_IntEnum)
class _TLSAlertType:
    """Alert types for TLSContentType.ALERT messages

    See RFC 8446, section B.2
    """
    CLOSE_NOTIFY = 0
    UNEXPECTED_MESSAGE = 10
    BAD_RECORD_MAC = 20
    DECRYPTION_FAILED = 21
    RECORD_OVERFLOW = 22
    DECOMPRESSION_FAILURE = 30
    HANDSHAKE_FAILURE = 40
    NO_CERTIFICATE = 41
    BAD_CERTIFICATE = 42
    UNSUPPORTED_CERTIFICATE = 43
    CERTIFICATE_REVOKED = 44
    CERTIFICATE_EXPIRED = 45
    CERTIFICATE_UNKNOWN = 46
    ILLEGAL_PARAMETER = 47
    UNKNOWN_CA = 48
    ACCESS_DENIED = 49
    DECODE_ERROR = 50
    DECRYPT_ERROR = 51
    EXPORT_RESTRICTION = 60
    PROTOCOL_VERSION = 70
    INSUFFICIENT_SECURITY = 71
    INTERNAL_ERROR = 80
    INAPPROPRIATE_FALLBACK = 86
    USER_CANCELED = 90
    NO_RENEGOTIATION = 100
    MISSING_EXTENSION = 109
    UNSUPPORTED_EXTENSION = 110
    CERTIFICATE_UNOBTAINABLE = 111
    UNRECOGNIZED_NAME = 112
    BAD_CERTIFICATE_STATUS_RESPONSE = 113
    BAD_CERTIFICATE_HASH_VALUE = 114
    UNKNOWN_PSK_IDENTITY = 115
    CERTIFICATE_REQUIRED = 116
    NO_APPLICATION_PROTOCOL = 120


@_simple_enum(_IntEnum)
class _TLSMessageType:
    """Message types (handshake protocol)

    See RFC 8446, section B.3
    """
    HELLO_REQUEST = 0
    CLIENT_HELLO = 1
    SERVER_HELLO = 2
    HELLO_VERIFY_REQUEST = 3
    NEWSESSION_TICKET = 4
    END_OF_EARLY_DATA = 5
    HELLO_RETRY_REQUEST = 6
    ENCRYPTED_EXTENSIONS = 8
    CERTIFICATE = 11
    SERVER_KEY_EXCHANGE = 12
    CERTIFICATE_REQUEST = 13
    SERVER_DONE = 14
    CERTIFICATE_VERIFY = 15
    CLIENT_KEY_EXCHANGE = 16
    FINISHED = 20
    CERTIFICATE_URL = 21
    CERTIFICATE_STATUS = 22
    SUPPLEMENTAL_DATA = 23
    KEY_UPDATE = 24
    NEXT_PROTO = 67
    MESSAGE_HASH = 254
    CHANGE_CIPHER_SPEC = 0x0101


if sys.platform == "win32":
    from _ssl import enum_certificates, enum_crls

socket_error = OSError
CertificateError = SSLCertVerificationError
CHANNEL_BINDING_TYPES = ['tls-unique']
_RESTRICTED_SERVER_CIPHERS = _DEFAULT_CIPHERS


# --------------------------------------------------------------- certificates

PEM_HEADER = "-----BEGIN CERTIFICATE-----"
PEM_FOOTER = "-----END CERTIFICATE-----"


def DER_cert_to_PEM_cert(der_cert_bytes):
    """Return the PEM text for a DER encoded certificate."""
    text = base64.standard_b64encode(der_cert_bytes).decode('ascii')
    body = '\n'.join(text[pos:pos + 64] for pos in range(0, len(text), 64))
    return f"{PEM_HEADER}\n{body}\n{PEM_FOOTER}\n"


def PEM_cert_to_DER_cert(pem_cert_string):
    """Return the DER bytes for a PEM encoded certificate."""
    if not pem_cert_string.startswith(PEM_HEADER):
        raise ValueError(f"Invalid PEM encoding; must start with {PEM_HEADER}")
    stripped = pem_cert_string.strip()
    if not stripped.endswith(PEM_FOOTER):
        raise ValueError(f"Invalid PEM encoding; must end with {PEM_FOOTER}")
    payload = stripped[len(PEM_HEADER):-len(PEM_FOOTER)]
    return base64.decodebytes(payload.encode('ASCII', 'strict'))


_MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
           "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")


def cert_time_to_seconds(cert_time):
    """Convert an ASN.1 certificate time such as "Jan  5 09:34:43 2018 GMT"
    to seconds since the Epoch."""
    from calendar import timegm
    from time import strptime

    pattern = ' %d %H:%M:%S %Y GMT'
    month = cert_time[:3].title()
    if month not in _MONTHS:
        raise ValueError(f'time data {cert_time!r} does not match '
                         f'format "%b{pattern}"')
    parsed = strptime(cert_time[3:], pattern)
    return timegm((parsed[0], _MONTHS.index(month) + 1) + parsed[2:6])


DefaultVerifyPaths = namedtuple(
    "DefaultVerifyPaths",
    "cafile capath openssl_cafile_env openssl_cafile "
    "openssl_capath_env openssl_capath")


def get_default_verify_paths():
    """Return the default CA file and directory used by OpenSSL."""
    env_file, default_file, env_dir, default_dir = _ssl.get_default_verify_paths()
    cafile = os.environ.get(env_file, default_file)
    capath = os.environ.get(env_dir, default_dir)
    return DefaultVerifyPaths(
        cafile if os.path.isfile(cafile) else None,
        capath if os.path.isdir(capath) else None,
        env_file, default_file, env_dir, default_dir)


class _ASN1Object(namedtuple("_ASN1Object", "nid shortname longname oid")):
    """An ASN.1 object identifier as known to OpenSSL."""
    __slots__ = ()

    def __new__(cls, oid):
        return super().__new__(cls, *_txt2obj(oid, name=False))

    @classmethod
    def fromnid(cls, nid):
        """Look an object up by its numeric OpenSSL id."""
        return super().__new__(cls, *_nid2obj(nid))

    @classmethod
    def fromname(cls, name):
        """Look an object up by short name, long name or dotted OID."""
        return super().__new__(cls, *_txt2obj(name, name=True))


class Purpose(_ASN1Object, _Enum):
    """Extended key usage of a certificate (and of a default context)."""
    SERVER_AUTH = '1.3.6.1.5.5.7.3.1'
    CLIENT_AUTH = '1.3.6.1.5.5.7.3.2'


# --------------------------------------------------------------- SSLContext

def _as_enum(enum_cls, value):
    try:
        return enum_cls(value)
    except ValueError:
        return value


class SSLContext(_SSLContext):
    """TLS configuration shared by the connections created from it."""

    _windows_cert_stores = ("CA", "ROOT")
    sslsocket_class = None
    sslobject_class = None

    def __new__(cls, protocol=None, *args, **kwargs):
        if protocol is None:
            warnings.warn("ssl.SSLContext() without protocol argument is "
                          "deprecated.", DeprecationWarning, stacklevel=2)
            protocol = PROTOCOL_TLS
        return _SSLContext.__new__(cls, protocol)

    @staticmethod
    def _encode_hostname(hostname):
        if hostname is None:
            return None
        if isinstance(hostname, str):
            return hostname.encode('idna').decode('ascii')
        return hostname.decode('ascii')

    # -- connection factories

    def wrap_socket(self, sock, server_side=False, do_handshake_on_connect=True,
                    suppress_ragged_eofs=True, server_hostname=None,
                    session=None):
        return self.sslsocket_class._create(
            sock=sock, server_side=server_side,
            do_handshake_on_connect=do_handshake_on_connect,
            suppress_ragged_eofs=suppress_ragged_eofs,
            server_hostname=server_hostname, context=self, session=session)

    def wrap_bio(self, incoming, outgoing, server_side=False,
                 server_hostname=None, session=None):
        return self.sslobject_class._create(
            incoming, outgoing, server_side=server_side,
            server_hostname=self._encode_hostname(server_hostname),
            session=session, context=self)

    # -- configuration helpers

    def set_npn_protocols(self, npn_protocols):
        warnings.warn("ssl NPN is deprecated, use ALPN instead",
                      DeprecationWarning, stacklevel=2)

    def set_servername_callback(self, server_name_callback):
        if server_name_callback is None:
            self.sni_callback = None
            return
        if not callable(server_name_callback):
            raise TypeError("not a callable object")

        def call_with_text_name(sslobj, servername, sslctx):
            return server_name_callback(
                sslobj, self._encode_hostname(servername), sslctx)

        self.sni_callback = call_with_text_name

    def set_alpn_protocols(self, alpn_protocols):
        wire = bytearray()
        for proto in alpn_protocols:
            encoded = bytes(proto, 'ascii')
            if not 0 < len(encoded) <= 255:
                raise SSLError('ALPN protocols must be 1 to 255 in length')
            wire += bytes((len(encoded),)) + encoded
        self._set_alpn_protocols(wire)

    def _load_windows_store_certs(self, storename, purpose):
        try:
            for cert, encoding, trust in enum_certificates(storename):
                if encoding != "x509_asn":
                    continue
                if trust is True or purpose.oid in trust:
                    try:
                        self.load_verify_locations(cadata=cert)
                    except SSLError as exc:
                        warnings.warn(f"Bad certificate in Windows certificate store: {exc!s}")
        except PermissionError:
            warnings.warn("unable to enumerate Windows certificate store")

    def load_default_certs(self, purpose=Purpose.SERVER_AUTH):
        if not isinstance(purpose, _ASN1Object):
            raise TypeError(purpose)
        if sys.platform == "win32":
            for storename in self._windows_cert_stores:
                self._load_windows_store_certs(storename, purpose)
        self.set_default_verify_paths()

    # -- enum-typed views of the _SSLContext attributes

    @property
    def protocol(self):
        return _SSLMethod(super().protocol)

    @property
    def options(self):
        return Options(super().options)

    @options.setter
    def options(self, value):
        _SSLContext.options.__set__(self, value)

    @property
    def verify_flags(self):
        return VerifyFlags(super().verify_flags)

    @verify_flags.setter
    def verify_flags(self, value):
        _SSLContext.verify_flags.__set__(self, value)

    @property
    def verify_mode(self):
        return _as_enum(VerifyMode, super().verify_mode)

    @verify_mode.setter
    def verify_mode(self, value):
        _SSLContext.verify_mode.__set__(self, value)

    @property
    def minimum_version(self):
        return TLSVersion(super().minimum_version)

    @minimum_version.setter
    def minimum_version(self, value):
        if value == TLSVersion.SSLv3:
            self.options &= ~Options.OP_NO_SSLv3
        _SSLContext.minimum_version.__set__(self, value)

    @property
    def maximum_version(self):
        return TLSVersion(super().maximum_version)

    @maximum_version.setter
    def maximum_version(self, value):
        _SSLContext.maximum_version.__set__(self, value)

    @property
    def hostname_checks_common_name(self):
        never = _ssl.HOSTFLAG_NEVER_CHECK_SUBJECT
        return (self._host_flags & never) != never

    @hostname_checks_common_name.setter
    def hostname_checks_common_name(self, value):
        if value:
            self._host_flags &= ~_ssl.HOSTFLAG_NEVER_CHECK_SUBJECT
        else:
            self._host_flags |= _ssl.HOSTFLAG_NEVER_CHECK_SUBJECT

    @property
    def _msg_callback(self):
        """Debugging hook called for every TLS protocol message.

        The callback receives (conn, direction, version, content_type,
        msg_type, data); version, content_type and msg_type are converted to
        the TLSVersion/_TLSContentType/_TLSAlertType/_TLSMessageType enums
        when the value is known.  Exceptions raised by the callback are
        re-raised after the TLS operation that triggered it.
        """
        wrapper = super()._msg_callback
        return None if wrapper is None else wrapper.user_function

    @_msg_callback.setter
    def _msg_callback(self, callback):
        if callback is None:
            _SSLContext._msg_callback.__set__(self, None)
            return
        if not hasattr(callback, '__call__'):
            raise TypeError(f"{callback} is not callable.")

        def wrapper(conn, direction, version, content_type, msg_type, data):
            content_type = _as_enum(_TLSContentType, content_type)
            if content_type == _TLSContentType.HEADER:
                msg_enum = _TLSContentType
            elif content_type == _TLSContentType.ALERT:
                msg_enum = _TLSAlertType
            else:
                msg_enum = _TLSMessageType
            return callback(conn, direction, _as_enum(TLSVersion, version),
                            content_type, _as_enum(msg_enum, msg_type), data)

        wrapper.user_function = callback
        _SSLContext._msg_callback.__set__(self, wrapper)


def _apply_keylog_env(context):
    path = os.environ.get('SSLKEYLOGFILE')
    if path and not sys.flags.ignore_environment:
        context.keylog_filename = path


def create_default_context(purpose=Purpose.SERVER_AUTH, *, cafile=None,
                           capath=None, cadata=None):
    """Return a new SSLContext with secure default settings for `purpose`."""
    if not isinstance(purpose, _ASN1Object):
        raise TypeError(purpose)
    if purpose == Purpose.SERVER_AUTH:
        context = SSLContext(PROTOCOL_TLS_CLIENT)
        context.verify_mode = CERT_REQUIRED
        context.check_hostname = True
    elif purpose == Purpose.CLIENT_AUTH:
        context = SSLContext(PROTOCOL_TLS_SERVER)
    else:
        raise ValueError(purpose)
    context.verify_flags |= VERIFY_X509_PARTIAL_CHAIN | VERIFY_X509_STRICT
    if cafile or capath or cadata:
        context.load_verify_locations(cafile, capath, cadata)
    elif context.verify_mode != CERT_NONE:
        context.load_default_certs(purpose)
    _apply_keylog_env(context)
    return context


def _create_unverified_context(protocol=None, *, cert_reqs=CERT_NONE,
                               check_hostname=False,
                               purpose=Purpose.SERVER_AUTH, certfile=None,
                               keyfile=None, cafile=None, capath=None,
                               cadata=None):
    """Context factory used by stdlib modules (lenient defaults)."""
    if not isinstance(purpose, _ASN1Object):
        raise TypeError(purpose)
    if purpose == Purpose.SERVER_AUTH:
        protocol = PROTOCOL_TLS_CLIENT if protocol is None else protocol
    elif purpose == Purpose.CLIENT_AUTH:
        protocol = PROTOCOL_TLS_SERVER if protocol is None else protocol
    else:
        raise ValueError(purpose)
    context = SSLContext(protocol)
    context.check_hostname = check_hostname
    if cert_reqs is not None:
        context.verify_mode = cert_reqs
    if check_hostname:
        context.check_hostname = True
    if keyfile and not certfile:
        raise ValueError("certfile must be specified")
    if certfile or keyfile:
        context.load_cert_chain(certfile, keyfile)
    if cafile or capath or cadata:
        context.load_verify_locations(cafile, capath, cadata)
    elif context.verify_mode != CERT_NONE:
        context.load_default_certs(purpose)
    _apply_keylog_env(context)
    return context


_create_default_https_context = create_default_context
_create_stdlib_context = _create_unverified_context


# --------------------------------------------------------------- connections

class _ChainMixin:
    def _chain_bytes(self, chain):
        if chain is None:
            return []
        return [cert.public_bytes(_ssl.ENCODING_DER) for cert in chain]


class SSLObject(_ChainMixin):
    """TLS state machine without its own I/O: data is exchanged through the
    MemoryBIO objects given to SSLContext.wrap_bio()."""

    def __init__(self, *args, **kwargs):
        raise TypeError(
            f"{self.__class__.__name__} does not have a public "
            f"constructor. Instances are returned by SSLContext.wrap_bio().")

    @classmethod
    def _create(cls, incoming, outgoing, server_side=False,
                server_hostname=None, session=None, context=None):
        if server_side:
            if server_hostname:
                raise ValueError("server_hostname can only be specified "
                                 "in client mode")
            if session is not None:
                raise ValueError("session can only be specified in "
                                 "client mode")
        if context.check_hostname and not server_hostname:
            raise ValueError("check_hostname requires server_hostname")
        obj = cls.__new__(cls)
        obj._sslobj = context._wrap_bio(
            incoming, outgoing, server_side=server_side,
            server_hostname=server_hostname, owner=obj, session=session)
        return obj

    context = property(lambda self: self._sslobj.context,
                       lambda self, ctx: setattr(self._sslobj, 'context', ctx),
                       doc="The SSLContext in use.")
    session = property(lambda self: self._sslobj.session,
                       lambda self, s: setattr(self._sslobj, 'session', s),
                       doc="The SSLSession of a client connection.")
    session_reused = property(lambda self: self._sslobj.session_reused,
                              doc="Whether the session was resumed.")
    server_side = property(lambda self: self._sslobj.server_side,
                           doc="True for the server end of a connection.")
    server_hostname = property(lambda self: self._sslobj.server_hostname,
                               doc="The SNI host name, or None.")

    def read(self, len=1024, buffer=None):
        """Read decrypted data (into `buffer` if given)."""
        if buffer is None:
            return self._sslobj.read(len)
        return self._sslobj.read(len, buffer)

    def write(self, data):
        """Encrypt `data`; returns the number of bytes consumed."""
        return self._sslobj.write(data)

    def getpeercert(self, binary_form=False):
        """The peer certificate (dict, DER bytes, {} if unverified, or None)."""
        return self._sslobj.getpeercert(binary_form)

    def get_verified_chain(self):
        return self._chain_bytes(self._sslobj.get_verified_chain())

    def get_unverified_chain(self):
        return self._chain_bytes(self._sslobj.get_unverified_chain())

    def selected_npn_protocol(self):
        warnings.warn("ssl NPN is deprecated, use ALPN instead",
                      DeprecationWarning, stacklevel=2)

    def selected_alpn_protocol(self):
        """The negotiated ALPN protocol, or None."""
        return self._sslobj.selected_alpn_protocol()

    def cipher(self):
        """(name, protocol version, secret bits) of the cipher in use."""
        return self._sslobj.cipher()

    def shared_ciphers(self):
        """Ciphers offered by the client (server side), or None."""
        return self._sslobj.shared_ciphers()

    def compression(self):
        """Compression in use (always None with OpenSSL 3 defaults)."""
        return self._sslobj.compression()

    def group(self):
        """Name of the negotiated key exchange group, or None."""
        return self._sslobj.group()

    def pending(self):
        """Number of decrypted bytes ready to be read."""
        return self._sslobj.pending()

    def do_handshake(self):
        """Perform (or continue) the TLS handshake."""
        self._sslobj.do_handshake()

    def unwrap(self):
        """Run the TLS closing handshake."""
        return self._sslobj.shutdown()

    def get_channel_binding(self, cb_type="tls-unique"):
        """Channel binding data (bytes or None before the handshake)."""
        return self._sslobj.get_channel_binding(cb_type)

    def version(self):
        """Negotiated protocol version string, or None."""
        return self._sslobj.version()

    def verify_client_post_handshake(self):
        return self._sslobj.verify_client_post_handshake()


class SSLSocket(_ChainMixin, socket):
    """A socket whose traffic is protected by TLS once connected."""

    def __init__(self, *args, **kwargs):
        raise TypeError(
            f"{self.__class__.__name__} does not have a public "
            f"constructor. Instances are returned by "
            f"SSLContext.wrap_socket().")

    @classmethod
    def _create(cls, sock, server_side=False, do_handshake_on_connect=True,
                suppress_ragged_eofs=True, server_hostname=None,
                context=None, session=None):
        if sock.getsockopt(SOL_SOCKET, SO_TYPE) != SOCK_STREAM:
            raise NotImplementedError("only stream sockets are supported")
        if server_side:
            if server_hostname:
                raise ValueError("server_hostname can only be specified "
                                 "in client mode")
            if session is not None:
                raise ValueError("session can only be specified in "
                                 "client mode")
        if context.check_hostname and not server_hostname:
            raise ValueError("check_hostname requires server_hostname")

        timeout = sock.gettimeout()
        params = dict(family=sock.family, type=sock.type, proto=sock.proto,
                      fileno=sock.fileno())
        self = cls.__new__(cls, **params)
        socket.__init__(self, **params)
        sock.detach()
        try:
            self._context = context
            self._session = session
            self._closed = False
            self._sslobj = None
            self.server_side = server_side
            self.server_hostname = context._encode_hostname(server_hostname)
            self.do_handshake_on_connect = do_handshake_on_connect
            self.suppress_ragged_eofs = suppress_ragged_eofs
            self._connected = self._peer_present()
            self.settimeout(timeout)
            if self._connected:
                self._sslobj = context._wrap_socket(
                    self, server_side, self.server_hostname,
                    owner=self, session=self._session)
                if do_handshake_on_connect:
                    if self.gettimeout() == 0.0:
                        raise ValueError("do_handshake_on_connect should not "
                                         "be specified for non-blocking sockets")
                    self.do_handshake()
        except:
            try:
                self.close()
            except OSError:
                pass
            raise
        return self

    def _peer_present(self):
        try:
            self.getpeername()
            return True
        except OSError as exc:
            if exc.errno != errno.ENOTCONN:
                raise
        # Not connected: refuse sockets whose peer already sent (plain) data
        # and closed, since a handshake can never succeed on them.
        was_blocking = self.getblocking()
        self.setblocking(False)
        try:
            early = self.recv(1)
        except OSError as exc:
            if exc.errno not in (errno.ENOTCONN, errno.EINVAL):
                raise
            early = b''
        finally:
            self.setblocking(was_blocking)
        if early:
            reason = "Closed before TLS handshake with data in recv buffer."
            err = SSLError(errno.ENOTCONN, reason)
            err.reason = reason
            err.library = None
            raise err
        return False

    @property
    def context(self):
        """The SSLContext in use."""
        return self._context

    @context.setter
    def context(self, ctx):
        self._context = ctx
        self._sslobj.context = ctx

    @property
    def session(self):
        """The SSLSession of a client connection."""
        if self._sslobj is not None:
            return self._sslobj.session

    @session.setter
    def session(self, session):
        self._session = session
        if self._sslobj is not None:
            self._sslobj.session = session

    @property
    def session_reused(self):
        """Whether the session was resumed."""
        if self._sslobj is not None:
            return self._sslobj.session_reused

    def dup(self):
        raise NotImplementedError(f"Can't dup() {self.__class__.__name__} instances")

    def _checkClosed(self, msg=None):
        pass

    def _check_connected(self):
        if not self._connected:
            # raises ENOTCONN when there is no peer
            self.getpeername()

    def _tls(self, what):
        if self._sslobj is None:
            raise ValueError(f"{what} on closed or unwrapped SSL socket.")
        return self._sslobj

    def _no_flags(self, flags, method):
        if flags != 0:
            raise ValueError(f"non-zero flags not allowed in calls to "
                             f"{method}() on {self.__class__}")

    def read(self, len=1024, buffer=None):
        """Read decrypted data; b'' (or 0) at EOF."""
        tls = self._tls("Read")
        try:
            if buffer is None:
                return tls.read(len)
            return tls.read(len, buffer)
        except SSLError as exc:
            if exc.args[0] == SSL_ERROR_EOF and self.suppress_ragged_eofs:
                return b'' if buffer is None else 0
            raise

    def write(self, data):
        """Send `data` over TLS; returns the number of bytes sent."""
        return self._tls("Write").write(data)

    def getpeercert(self, binary_form=False):
        self._check_connected()
        return self._sslobj.getpeercert(binary_form)

    def get_verified_chain(self):
        return self._chain_bytes(self._sslobj.get_verified_chain())

    def get_unverified_chain(self):
        return self._chain_bytes(self._sslobj.get_unverified_chain())

    def selected_npn_protocol(self):
        warnings.warn("ssl NPN is deprecated, use ALPN instead",
                      DeprecationWarning, stacklevel=2)
        return None

    def selected_alpn_protocol(self):
        if self._sslobj is None:
            return None
        return self._sslobj.selected_alpn_protocol()

    def cipher(self):
        return None if self._sslobj is None else self._sslobj.cipher()

    def shared_ciphers(self):
        return None if self._sslobj is None else self._sslobj.shared_ciphers()

    def compression(self):
        return None if self._sslobj is None else self._sslobj.compression()

    def version(self):
        return None if self._sslobj is None else self._sslobj.version()

    def group(self):
        return None if self._sslobj is None else self._sslobj.group()

    def pending(self):
        return 0 if self._sslobj is None else self._sslobj.pending()

    def send(self, data, flags=0):
        if self._sslobj is None:
            return super().send(data, flags)
        self._no_flags(flags, "send")
        return self._sslobj.write(data)

    def sendto(self, data, flags_or_addr, addr=None):
        if self._sslobj is not None:
            raise ValueError(f"sendto not allowed on instances of {self.__class__}")
        if addr is None:
            return super().sendto(data, flags_or_addr)
        return super().sendto(data, flags_or_addr, addr)

    def sendmsg(self, *args, **kwargs):
        raise NotImplementedError(f"sendmsg not allowed on instances of {self.__class__}")

    def sendall(self, data, flags=0):
        if self._sslobj is None:
            return super().sendall(data, flags)
        self._no_flags(flags, "sendall")
        with memoryview(data) as view, view.cast("B") as raw:
            sent, total = 0, len(raw)
            while sent < total:
                sent += self.send(raw[sent:])

    def sendfile(self, file, offset=0, count=None):
        if self._sslobj is None:
            return super().sendfile(file, offset, count)
        return self._sendfile_use_send(file, offset, count)

    def recv(self, buflen=1024, flags=0):
        if self._sslobj is None:
            return super().recv(buflen, flags)
        self._no_flags(flags, "recv")
        return self.read(buflen)

    def recv_into(self, buffer, nbytes=None, flags=0):
        if nbytes is None:
            nbytes = 1024
            if buffer is not None:
                with memoryview(buffer) as view:
                    nbytes = view.nbytes or 1024
        if self._sslobj is None:
            return super().recv_into(buffer, nbytes, flags)
        self._no_flags(flags, "recv_into")
        return self.read(nbytes, buffer)

    def recvfrom(self, buflen=1024, flags=0):
        if self._sslobj is not None:
            raise ValueError(f"recvfrom not allowed on instances of {self.__class__}")
        return super().recvfrom(buflen, flags)

    def recvfrom_into(self, buffer, nbytes=None, flags=0):
        if self._sslobj is not None:
            raise ValueError(f"recvfrom_into not allowed on instances of {self.__class__}")
        return super().recvfrom_into(buffer, nbytes, flags)

    def recvmsg(self, *args, **kwargs):
        raise NotImplementedError(f"recvmsg not allowed on instances of {self.__class__}")

    def recvmsg_into(self, *args, **kwargs):
        raise NotImplementedError(f"recvmsg_into not allowed on instances of {self.__class__}")

    def shutdown(self, how):
        self._sslobj = None
        super().shutdown(how)

    def unwrap(self):
        if not self._sslobj:
            raise ValueError("No SSL wrapper around " + str(self))
        plain = self._sslobj.shutdown()
        self._sslobj = None
        return plain

    def verify_client_post_handshake(self):
        if not self._sslobj:
            raise ValueError("No SSL wrapper around " + str(self))
        return self._sslobj.verify_client_post_handshake()

    def _real_close(self):
        self._sslobj = None
        super()._real_close()

    def do_handshake(self, block=False):
        self._check_connected()
        timeout = self.gettimeout()
        try:
            if block and timeout == 0.0:
                self.settimeout(None)
            self._sslobj.do_handshake()
        finally:
            self.settimeout(timeout)

    def _real_connect(self, addr, connect_ex):
        if self.server_side:
            raise ValueError("can't connect in server-side mode")
        if self._connected or self._sslobj is not None:
            raise ValueError("attempt to connect already-connected SSLSocket!")
        self._sslobj = self.context._wrap_socket(
            self, False, self.server_hostname, owner=self,
            session=self._session)
        try:
            if connect_ex:
                rc = super().connect_ex(addr)
            else:
                rc = None
                super().connect(addr)
            if not rc:
                self._connected = True
                if self.do_handshake_on_connect:
                    self.do_handshake()
            return rc
        except (OSError, ValueError):
            self._sslobj = None
            raise

    def connect(self, addr):
        """Connect to `addr` and run the TLS handshake."""
        self._real_connect(addr, False)

    def connect_ex(self, addr):
        """Like connect() but returns an error indicator."""
        return self._real_connect(addr, True)

    def accept(self):
        """Accept a connection and wrap it as a server-side SSLSocket."""
        conn, addr = super().accept()
        conn = self.context.wrap_socket(
            conn, do_handshake_on_connect=self.do_handshake_on_connect,
            suppress_ragged_eofs=self.suppress_ragged_eofs, server_side=True)
        return conn, addr

    def get_channel_binding(self, cb_type="tls-unique"):
        if self._sslobj is not None:
            return self._sslobj.get_channel_binding(cb_type)
        if cb_type not in CHANNEL_BINDING_TYPES:
            raise ValueError(f"{cb_type} channel binding type not implemented")
        return None


SSLContext.sslsocket_class = SSLSocket
SSLContext.sslobject_class = SSLObject


def get_server_certificate(addr, ssl_version=PROTOCOL_TLS_CLIENT,
                           ca_certs=None, timeout=_GLOBAL_DEFAULT_TIMEOUT):
    """Fetch the certificate of the server at `addr` as PEM text (verified
    against `ca_certs` when given)."""
    host, _ = addr
    reqs = CERT_REQUIRED if ca_certs is not None else CERT_NONE
    context = _create_stdlib_context(ssl_version, cert_reqs=reqs, cafile=ca_certs)
    with create_connection(addr, timeout=timeout) as sock:
        with context.wrap_socket(sock, server_hostname=host) as tls:
            der = tls.getpeercert(True)
    return DER_cert_to_PEM_cert(der)


def get_protocol_name(protocol_code):
    return _PROTOCOL_NAMES.get(protocol_code, '<unknown>')
