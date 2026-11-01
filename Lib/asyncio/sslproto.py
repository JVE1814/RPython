"""TLS support for asyncio transports.

This interpreter is built without the ssl module, so TLS transports cannot
be created: SSLProtocol refuses construction with RuntimeError, which
create_connection(ssl=...) and friends report to the caller.
"""

import enum

try:
    import ssl
except ImportError:
    ssl = None

from . import constants
from . import protocols


class SSLProtocolState(enum.Enum):
    UNWRAPPED = "UNWRAPPED"
    DO_HANDSHAKE = "DO_HANDSHAKE"
    WRAPPED = "WRAPPED"
    FLUSHING = "FLUSHING"
    SHUTDOWN = "SHUTDOWN"


class AppProtocolState(enum.Enum):
    STATE_INIT = "STATE_INIT"
    STATE_CON_MADE = "STATE_CON_MADE"
    STATE_EOF = "STATE_EOF"
    STATE_CON_LOST = "STATE_CON_LOST"


def _create_transport_context(server_side, server_hostname):
    if server_side:
        raise ValueError('Server side SSL needs a valid SSLContext')
    if ssl is None:
        raise RuntimeError('stdlib ssl module not available')
    context = ssl.create_default_context()
    if not server_hostname:
        context.check_hostname = False
    return context


def add_flowcontrol_defaults(high, low, kb):
    if high is None:
        high = kb * 1024 if low is None else 4 * low
    if low is None:
        low = high // 4
    if not high >= low >= 0:
        raise ValueError(f'high ({high!r}) must be >= low ({low!r}) must be '
                         f'>= 0')
    return high, low


class SSLProtocol(protocols.BufferedProtocol):
    """TLS layer between a transport and the application protocol."""

    def __init__(self, loop, app_protocol, sslcontext, waiter,
                 server_side=False, server_hostname=None,
                 call_connection_made=True,
                 ssl_handshake_timeout=None,
                 ssl_shutdown_timeout=None):
        if ssl is None:
            raise RuntimeError("stdlib ssl module not available")
        if ssl_handshake_timeout is not None and ssl_handshake_timeout <= 0:
            raise ValueError(f"ssl_handshake_timeout should be a positive "
                             f"number, got {ssl_handshake_timeout}")
        if ssl_shutdown_timeout is not None and ssl_shutdown_timeout <= 0:
            raise ValueError(f"ssl_shutdown_timeout should be a positive "
                             f"number, got {ssl_shutdown_timeout}")
        raise RuntimeError("TLS transports are not supported by this "
                           "interpreter")
