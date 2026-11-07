"""Redirect Python's standard streams to the Android system log.

On Android, file descriptors 1 and 2 go nowhere useful, so at startup the
interpreter replaces sys.stdout and sys.stderr with streams that send each
line to liblog (``__android_log_write``), where ``logcat`` can show it.

The design is two layers:

* BinaryLogStream -- a raw, write-only byte stream; each write() becomes
  one log entry (after a few fix-ups that logcat needs) and is subject to
  a token-bucket rate limit so a chatty program cannot overflow the log.
* TextLogStream -- a TextIOWrapper over it that always line-buffers and
  hands the binary layer one line (or one bounded block) at a time.

This module is importable on every platform; only init_streams() and the
actual log calls need Android.
"""

import io
import sys
from time import sleep, time

# A log entry longer than about 4 KB is truncated by liblog, so text is cut
# into blocks of at most this many characters (4 UTF-8 bytes each at worst)
# and the text layer joins blocks up to this many bytes.
MAX_CHARS_PER_WRITE = 1000
MAX_BYTES_PER_WRITE = 4000

# Token-bucket rate limit, in bytes of log buffer.  The defaults keep the
# default-sized logcat ring buffer from discarding lines that were written
# faster than a reader can collect them.
MAX_BYTES_PER_SECOND = 1024 * 1024
BUCKET_SIZE = 128 * 1024

# Bookkeeping bytes liblog spends on every entry, besides tag and message.
PER_MESSAGE_OVERHEAD = 28

_log_write_func = None


def _native_log_write(prio, tag, message):
    """Send one entry to liblog; *tag* and *message* are bytes."""
    global _log_write_func
    if _log_write_func is None:
        from ctypes import CDLL, c_char_p, c_int
        func = getattr(CDLL("liblog.so"), "__android_log_write")
        func.argtypes = (c_int, c_char_p, c_char_p)
        _log_write_func = func
    _log_write_func(prio, tag, message)


def init_streams(android_log_write=None, stdout_prio=4, stderr_prio=5):
    """Replace sys.stdout and sys.stderr with log streams.

    *android_log_write*, if given, is used instead of loading liblog with
    ctypes; it is called as ``android_log_write(prio, tag, message)``.
    """
    global _log_write_func
    if android_log_write is not None:
        _log_write_func = android_log_write
    sys.stdout = TextLogStream(stdout_prio, "python.stdout", sys.stdout)
    sys.stderr = TextLogStream(stderr_prio, "python.stderr", sys.stderr)


def _fileno_of(target):
    if target is None or isinstance(target, int):
        return target
    try:
        return target.fileno()
    except (AttributeError, OSError, ValueError):
        return None


class TextLogStream(io.TextIOWrapper):
    """Line-buffered text stream whose lines become log entries."""

    def __init__(self, prio, tag, fileno=None, **kwargs):
        kwargs.setdefault("encoding", "UTF-8")
        kwargs.setdefault("errors", "backslashreplace")
        super().__init__(BinaryLogStream(prio, tag, fileno), **kwargs)
        # Let the wrapper collect several blocks into one entry, as long as
        # the entry stays below liblog's limit.
        self._CHUNK_SIZE = MAX_BYTES_PER_WRITE
        self._force_line_buffering()

    def _force_line_buffering(self):
        super().reconfigure(line_buffering=True)

    def __repr__(self):
        return f"<TextLogStream {self.buffer.tag!r}>"

    def reconfigure(self, **kwargs):
        # The log is line based: a newline always ends an entry, so line
        # buffering can never be switched off (write_through can).
        kwargs["line_buffering"] = True
        super().reconfigure(**kwargs)

    def write(self, s):
        if not isinstance(s, str):
            raise TypeError(
                f"write() argument must be str, not {type(s).__name__}")
        # Work on an exact str so overridden methods of a subclass never run.
        s = str.__str__(s)
        total = len(s)
        start = 0
        # Hand the wrapper one line at a time: with line buffering it
        # flushes everything it holds whenever the written text contains a
        # newline, so a partial last line must arrive in a separate call to
        # stay buffered.  Only "\n" ends a line here; other separators are
        # ordinary characters in the log.
        while start < total:
            nl = s.find("\n", start)
            end = total if nl < 0 else nl + 1
            pos = start
            while pos < end:
                block_end = min(end, pos + MAX_CHARS_PER_WRITE)
                super().write(s[pos:block_end])
                pos = block_end
            start = end
        return total


class BinaryLogStream(io.RawIOBase):
    """Write-only raw stream; every write() is one log entry."""

    def __init__(self, prio, tag, fileno=None):
        super().__init__()
        self.prio = prio
        self.tag = tag
        self.fileno_value = _fileno_of(fileno)
        self._bucket_level = None
        self._prev_write_time = None

    def __repr__(self):
        return f"<BinaryLogStream {self.tag!r}>"

    def writable(self):
        return True

    def fileno(self):
        if self.fileno_value is None:
            return super().fileno()
        return self.fileno_value

    def write(self, b):
        try:
            view = memoryview(b)
        except TypeError:
            raise TypeError(
                f"write() argument must be bytes-like, not {type(b).__name__}"
            ) from None
        data = view.tobytes()
        if data:
            # liblog takes a C string: NUL bytes are written the way
            # "modified UTF-8" spells U+0000.
            data = data.replace(b"\x00", b"\xc0\x80")
            # logcat would swallow an entry's leading newline; a space keeps
            # the blank line visible.
            if data.startswith(b"\n"):
                data = b" " + data
            self._throttle(len(data))
            _native_log_write(self.prio, self.tag.encode("UTF-8"), data)
        return view.nbytes

    def _throttle(self, size):
        """Token bucket: sleep while the program writes faster than the
        sustained rate, after an allowance of BUCKET_SIZE bytes."""
        now = time()
        if self._prev_write_time is None:
            level = BUCKET_SIZE
        else:
            level = self._bucket_level + (
                (now - self._prev_write_time) * MAX_BYTES_PER_SECOND)
        level = min(level, BUCKET_SIZE)
        self._prev_write_time = now

        level -= size + len(self.tag) + PER_MESSAGE_OVERHEAD
        if level < 0:
            sleep(-level / MAX_BYTES_PER_SECOND)
            later = time()
            level += (later - now) * MAX_BYTES_PER_SECOND
            self._prev_write_time = later
        self._bucket_level = level
