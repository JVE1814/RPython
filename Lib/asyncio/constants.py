"""Tunables used across the asyncio implementation."""

import enum

# Warn after this many write() calls on a transport whose connection is lost.
LOG_THRESHOLD_FOR_CONNLOST_WRITES = 5

# Back-off (seconds) after accept() failed for lack of resources.
ACCEPT_RETRY_DELAY = 1

# Depth of the creation stacks recorded in debug mode.
DEBUG_STACK_DEPTH = 10

# TLS handshake and shutdown time limits (seconds).
SSL_HANDSHAKE_TIMEOUT = 60.0
SSL_SHUTDOWN_TIMEOUT = 30.0

# Chunk size of the read()/write() based sendfile emulation.
SENDFILE_FALLBACK_READBUFFER_SIZE = 256 * 1024

# Flow control limits of the TLS layer (in KiB).
FLOW_CONTROL_HIGH_WATER_SSL_READ = 256
FLOW_CONTROL_HIGH_WATER_SSL_WRITE = 512

# How long loop.shutdown_default_executor() waits for the worker threads.
THREAD_JOIN_TIMEOUT = 300


class _SendfileMode(enum.Enum):
    UNSUPPORTED = enum.auto()
    TRY_NATIVE = enum.auto()
    FALLBACK = enum.auto()
