"""Constants shared by the asyncio modules."""

import enum

# Number of consecutive writes after a lost connection before warning.
LOG_THRESHOLD_FOR_CONNLOST_WRITES = 5

# Seconds to wait before retrying accept() after a resource error.
ACCEPT_RETRY_DELAY = 1

# Frames kept by Handle/Future/Task source tracebacks in debug mode.
DEBUG_STACK_DEPTH = 10

# Default SSL handshake / shutdown timeouts (seconds).
SSL_HANDSHAKE_TIMEOUT = 60.0
SSL_SHUTDOWN_TIMEOUT = 30.0

# Read buffer size of the sendfile() fallback implementation.
SENDFILE_FALLBACK_READBUFFER_SIZE = 1024 * 256

# SSL flow control high-water marks (KiB).
FLOW_CONTROL_HIGH_WATER_SSL_READ = 256
FLOW_CONTROL_HIGH_WATER_SSL_WRITE = 512

# Default timeout for joining threads in loop.shutdown_default_executor().
THREAD_JOIN_TIMEOUT = 300


class _SendfileMode(enum.Enum):
    UNSUPPORTED = enum.auto()
    TRY_NATIVE = enum.auto()
    FALLBACK = enum.auto()
