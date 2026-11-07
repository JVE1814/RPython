"""Redirect Python's standard streams to the Apple unified system log.

On iOS-like platforms an app's stdout/stderr are not visible anywhere, so
the interpreter can route them to the system log (``os_log``) instead.  A
SystemLog is a line-buffered text stream over a LogStream; the LogStream
passes every chunk of bytes it receives to a ``log_write(level, data)``
callable supplied by the embedding code.

The module imports on every platform; only the callable it is given is
platform specific.
"""

import io


class SystemLog(io.TextIOWrapper):
    """Text stream whose complete lines are sent to the system log."""

    def __init__(self, log_write, level, **kwargs):
        kwargs.setdefault("encoding", "UTF-8")
        kwargs.setdefault("line_buffering", True)
        super().__init__(LogStream(log_write, level), **kwargs)

    def __repr__(self):
        return f"<SystemLog (level {self.buffer.level})>"

    def write(self, s):
        if not isinstance(s, str):
            raise TypeError(
                f"write() argument must be str, not {type(s).__name__}")
        # An exact str copy: methods overridden by a subclass are not used.
        s = str.__str__(s)
        total = len(s)
        start = 0
        # A line-buffered wrapper flushes all pending text when a write
        # contains a newline; feeding it line by line keeps a trailing
        # partial line buffered until it is completed or flushed.
        while start < total:
            nl = s.find("\n", start)
            end = total if nl < 0 else nl + 1
            super().write(s[start:end])
            start = end
        return total


class LogStream(io.RawIOBase):
    """Write-only raw stream calling ``log_write(level, data)`` per write."""

    def __init__(self, log_write, level):
        super().__init__()
        self.log_write = log_write
        self.level = level

    def __repr__(self):
        return f"<LogStream (level {self.level!r})>"

    def writable(self):
        return True

    def write(self, b):
        try:
            view = memoryview(b)
        except TypeError:
            raise TypeError(
                f"write() argument must be bytes-like, not {type(b).__name__}"
            ) from None
        data = view.tobytes()
        if data:
            # The log takes C strings: NUL is spelled as in modified UTF-8.
            self.log_write(self.level, data.replace(b"\x00", b"\xc0\x80"))
        return view.nbytes
