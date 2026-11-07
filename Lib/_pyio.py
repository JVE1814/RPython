"""Pure Python implementation of the io module.

This mirrors the behaviour of the _io accelerator closely and is used by the
test suite to check both implementations against each other.
"""

import os
import abc
import codecs
import errno
import stat
import sys
from _thread import allocate_lock as Lock
if sys.platform in {'win32', 'cygwin'}:
    from msvcrt import setmode as _setmode
else:
    _setmode = None

import io
from io import (__all__, SEEK_SET, SEEK_CUR, SEEK_END, Reader, Writer)

valid_seek_flags = {0, 1, 2}
if hasattr(os, 'SEEK_HOLE'):
    valid_seek_flags.add(os.SEEK_HOLE)
    valid_seek_flags.add(os.SEEK_DATA)

DEFAULT_BUFFER_SIZE = 128 * 1024

_IOBASE_EMITS_UNRAISABLE = (hasattr(sys, "gettotalrefcount") or sys.flags.dev_mode)
_CHECK_ERRORS = _IOBASE_EMITS_UNRAISABLE

BlockingIOError = BlockingIOError


def text_encoding(encoding, stacklevel=2):
    """Return the encoding to use for a text stream: encoding itself if not
    None, otherwise "utf-8" in UTF-8 mode or "locale".  Emits EncodingWarning
    when -X warn_default_encoding is active and encoding is None."""
    if encoding is None:
        if sys.flags.utf8_mode:
            encoding = "utf-8"
        else:
            encoding = "locale"
        if sys.flags.warn_default_encoding:
            import warnings
            warnings.warn("'encoding' argument not specified.",
                          EncodingWarning, stacklevel + 1)
    return encoding


# A staticmethod, so that storing it as a class attribute (open = _pyio.open)
# does not turn it into a method.
@staticmethod
def open(file, mode="r", buffering=-1, encoding=None, errors=None,
         newline=None, closefd=True, opener=None):
    r"""Open file and return a stream.  Raise OSError upon failure.

    See the documentation of the builtin open() for the meaning of the
    arguments: mode is a combination of 'r', 'w', 'x', 'a', '+', 'b' and 't';
    buffering 0 means unbuffered (binary only), 1 line buffering (text only)
    and > 1 the buffer size; encoding, errors and newline only apply to text
    mode; closefd=False keeps a passed descriptor open; opener returns a file
    descriptor for (file, flags).
    """
    if not isinstance(file, int):
        file = os.fspath(file)
    if not isinstance(file, (str, bytes, int)):
        raise TypeError("invalid file: %r" % file)
    if not isinstance(mode, str):
        raise TypeError("invalid mode: %r" % mode)
    if not isinstance(buffering, int):
        raise TypeError("invalid buffering: %r" % buffering)
    if encoding is not None and not isinstance(encoding, str):
        raise TypeError("invalid encoding: %r" % encoding)
    if errors is not None and not isinstance(errors, str):
        raise TypeError("invalid errors: %r" % errors)
    modes = set(mode)
    if modes - set("axrwb+t") or len(mode) > len(modes):
        raise ValueError("invalid mode: %r" % mode)
    creating = "x" in modes
    reading = "r" in modes
    writing = "w" in modes
    appending = "a" in modes
    updating = "+" in modes
    text = "t" in modes
    binary = "b" in modes
    if text and binary:
        raise ValueError("can't have text and binary mode at once")
    if creating + reading + writing + appending > 1:
        raise ValueError("must have exactly one of create/read/write/append mode")
    if not (creating or reading or writing or appending):
        raise ValueError("Must have exactly one of create/read/write/append "
                         "mode and at most one plus")
    if binary and encoding is not None:
        raise ValueError("binary mode doesn't take an encoding argument")
    if binary and errors is not None:
        raise ValueError("binary mode doesn't take an errors argument")
    if binary and newline is not None:
        raise ValueError("binary mode doesn't take a newline argument")
    if binary and buffering == 1:
        import warnings
        warnings.warn("line buffering (buffering=1) isn't supported in binary "
                      "mode, the default buffer size will be used",
                      RuntimeWarning, 2)
    rawmode = ((creating and "x" or "") + (reading and "r" or "") +
               (writing and "w" or "") + (appending and "a" or "") +
               (updating and "+" or ""))
    raw = FileIO(file, rawmode, closefd, opener=opener)
    result = raw
    try:
        line_buffering = False
        if buffering == 1 or buffering < 0 and raw._isatty_open_only():
            buffering = -1
            line_buffering = True
        if buffering < 0:
            buffering = max(min(raw._blksize, 8192 * 1024), DEFAULT_BUFFER_SIZE)
        if buffering < 0:
            raise ValueError("invalid buffering size")
        if buffering == 0:
            if binary:
                return result
            raise ValueError("can't have unbuffered text I/O")
        if updating:
            buffer = BufferedRandom(raw, buffering)
        elif creating or writing or appending:
            buffer = BufferedWriter(raw, buffering)
        elif reading:
            buffer = BufferedReader(raw, buffering)
        else:
            raise ValueError("unknown mode: %r" % mode)
        result = buffer
        if binary:
            return result
        encoding = text_encoding(encoding)
        text = TextIOWrapper(buffer, encoding, errors, newline, line_buffering)
        result = text
        text.mode = mode
        return result
    except:
        result.close()
        raise


def _open_code_with_warning(path):
    """Open the file at path for reading as bytes (no hooks are applied).

    Used only when io.open_code is missing: warn that a hook installed with
    PyFile_SetOpenCodeHook() would be bypassed."""
    import warnings
    warnings.warn("_pyio.open_code() may not be using hooks",
                  RuntimeWarning, 2)
    return open(path, "rb")


try:
    open_code = io.open_code
except AttributeError:
    open_code = _open_code_with_warning


try:
    UnsupportedOperation = io.UnsupportedOperation
except AttributeError:
    class UnsupportedOperation(OSError, ValueError):
        pass


class IOBase(metaclass=abc.ABCMeta):

    """The abstract base class for all I/O classes.

    Streams are iterable over lines and usable as context managers.  Methods
    a subclass does not implement raise UnsupportedOperation; operations on a
    closed stream raise ValueError.
    """

    def _unsupported(self, name):
        raise UnsupportedOperation("%s.%s() not supported" %
                                   (self.__class__.__name__, name))

    def seek(self, pos, whence=0):
        """Change the stream position; return the new absolute position."""
        self._unsupported("seek")

    def tell(self):
        """Return the current stream position."""
        return self.seek(0, 1)

    def truncate(self, pos=None):
        """Resize the stream to pos (default: current position)."""
        self._unsupported("truncate")

    def flush(self):
        """Flush write buffers, if applicable."""
        self._checkClosed()

    __closed = False

    def close(self):
        """Flush and close the stream; further operations raise ValueError."""
        if not self.__closed:
            try:
                self.flush()
            finally:
                self.__closed = True

    def __del__(self):
        """Close the stream when the object is destroyed."""
        try:
            closed = self.closed
        except AttributeError:
            return
        if closed:
            return
        # Wrappers forward to the raw file, which warns about the object
        # actually being dropped (this one).
        dealloc_warn = getattr(self, "_dealloc_warn", None)
        if dealloc_warn is not None:
            dealloc_warn(self)
        # an exception from close() is reported as unraisable by the caller
        # of __del__ (like the C implementation does)
        self.close()

    def seekable(self):
        """Return whether random access is supported (default False)."""
        return False

    def _checkSeekable(self, msg=None):
        if not self.seekable():
            raise UnsupportedOperation("File or stream is not seekable."
                                       if msg is None else msg)

    def readable(self):
        """Return whether the stream can be read (default False)."""
        return False

    def _checkReadable(self, msg=None):
        if not self.readable():
            raise UnsupportedOperation("File or stream is not readable."
                                       if msg is None else msg)

    def writable(self):
        """Return whether the stream can be written (default False)."""
        return False

    def _checkWritable(self, msg=None):
        if not self.writable():
            raise UnsupportedOperation("File or stream is not writable."
                                       if msg is None else msg)

    @property
    def closed(self):
        """True if the stream is closed."""
        return self.__closed

    def _checkClosed(self, msg=None):
        if self.closed:
            raise ValueError("I/O operation on closed file."
                             if msg is None else msg)

    def __enter__(self):
        self._checkClosed()
        return self

    def __exit__(self, *args):
        self.close()

    def fileno(self):
        """Return the underlying file descriptor, if there is one."""
        self._unsupported("fileno")

    def isatty(self):
        """Return whether the stream is attached to a terminal."""
        self._checkClosed()
        return False

    def readline(self, size=-1):
        r"""Read and return one line (terminated by b'\n') of at most size bytes."""
        if hasattr(self, "peek"):
            def nreadahead():
                readahead = self.peek(1)
                if not readahead:
                    return 1
                n = (readahead.find(b"\n") + 1) or len(readahead)
                if size >= 0:
                    n = min(n, size - len(res))
                return n
        else:
            def nreadahead():
                return 1
        if size is None:
            size = -1
        else:
            try:
                size_index = size.__index__
            except AttributeError:
                raise TypeError(f"{size!r} is not an integer")
            else:
                size = size_index()
        res = bytearray()
        while size < 0 or len(res) < size:
            b = self.read(nreadahead())
            if not b:
                break
            res += b
            if res.endswith(b"\n"):
                break
        return bytes(res)

    def __iter__(self):
        self._checkClosed()
        return self

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    def readlines(self, hint=None):
        """Return a list of lines, stopping once their total size reaches hint."""
        if hint is None or hint <= 0:
            return list(self)
        n = 0
        lines = []
        for line in self:
            lines.append(line)
            n += len(line)
            if n >= hint:
                break
        return lines

    def writelines(self, lines):
        """Write each item of the iterable lines (no separators are added)."""
        self._checkClosed()
        for line in lines:
            self.write(line)

io.IOBase.register(IOBase)


class RawIOBase(IOBase):

    """Base class for raw binary I/O: read() is built on readinto()."""

    def read(self, size=-1):
        """Read up to size bytes with one system call (all if size < 0).
        Returns None for a non-blocking stream without data."""
        if size is None:
            size = -1
        if size < 0:
            return self.readall()
        b = bytearray(size.__index__())
        n = self.readinto(b)
        if n is None:
            return None
        if n < 0 or n > len(b):
            raise ValueError(f"readinto returned {n} outside buffer size {len(b)}")
        del b[n:]
        return bytes(b)

    def readall(self):
        """Read until EOF using repeated read() calls."""
        res = bytearray()
        while data := self.read(DEFAULT_BUFFER_SIZE):
            res += data
        if res:
            return bytes(res)
        else:
            # b'' or None
            return data

    def readinto(self, b):
        """Read into the bytes-like object b; return the count or None."""
        self._unsupported("readinto")

    def write(self, b):
        """Write b; return the number of bytes written or None."""
        self._unsupported("write")

io.RawIOBase.register(RawIOBase)
from _io import FileIO as _FileIO_c  # noqa: E402  (only for registration checks)
RawIOBase.register(_FileIO_c)
del _FileIO_c


class BufferedIOBase(IOBase):

    """Base class for buffered binary streams."""

    def read(self, size=-1):
        """Read and return up to size bytes (all if size is negative)."""
        self._unsupported("read")

    def read1(self, size=-1):
        """Read up to size bytes with at most one raw read."""
        self._unsupported("read1")

    def readinto(self, b):
        """Read bytes into the pre-allocated buffer b; return the count."""
        return self._readinto(b, read1=False)

    def readinto1(self, b):
        """Like readinto(), using at most one raw read."""
        return self._readinto(b, read1=True)

    def _readinto(self, b, read1):
        if not isinstance(b, memoryview):
            b = memoryview(b)
        b = b.cast('B')
        if read1:
            data = self.read1(len(b))
        else:
            data = self.read(len(b))
        n = len(data)
        b[:n] = data
        return n

    def write(self, b):
        """Write b, returning len(b)."""
        self._unsupported("write")

    def detach(self):
        """Separate the underlying raw stream from the buffer and return it."""
        self._unsupported("detach")

io.BufferedIOBase.register(BufferedIOBase)


class _BufferedIOMixin(BufferedIOBase):

    """Delegates the non-buffering methods to the wrapped raw stream."""

    def __init__(self, raw):
        self._raw = raw

    def seek(self, pos, whence=0):
        new_position = self.raw.seek(pos, whence)
        if new_position < 0:
            raise OSError("seek() returned an invalid position")
        return new_position

    def tell(self):
        pos = self.raw.tell()
        if pos < 0:
            raise OSError("tell() returned an invalid position")
        return pos

    def truncate(self, pos=None):
        self._checkClosed()
        self._checkWritable()
        self.flush()
        if pos is None:
            pos = self.tell()
        return self.raw.truncate(pos)

    def flush(self):
        if self.closed:
            raise ValueError("flush on closed file")
        self.raw.flush()

    def close(self):
        if self.raw is not None and not self.closed:
            try:
                self.flush()
            finally:
                self.raw.close()

    def detach(self):
        if self.raw is None:
            raise ValueError("raw stream already detached")
        self.flush()
        raw = self._raw
        self._raw = None
        return raw

    def seekable(self):
        return self.raw.seekable()

    @property
    def raw(self):
        return self._raw

    def _dealloc_warn(self, source):
        dealloc_warn = getattr(self.raw, "_dealloc_warn", None)
        if dealloc_warn is not None:
            dealloc_warn(source)

    @property
    def closed(self):
        return self.raw.closed

    @property
    def name(self):
        return self.raw.name

    @property
    def mode(self):
        return self.raw.mode

    def __getstate__(self):
        raise TypeError(f"cannot pickle {self.__class__.__name__!r} object")

    def __repr__(self):
        modname = self.__class__.__module__
        clsname = self.__class__.__qualname__
        try:
            name = self.name
        except AttributeError:
            return "<{}.{}>".format(modname, clsname)
        else:
            return "<{}.{} name={!r}>".format(modname, clsname, name)

    def fileno(self):
        return self.raw.fileno()

    def isatty(self):
        return self.raw.isatty()


class BytesIO(BufferedIOBase):

    """Buffered I/O implementation using an in-memory bytes buffer."""

    _buffer = None

    def __init__(self, initial_bytes=None):
        buf = bytearray()
        if initial_bytes is not None:
            buf += initial_bytes
        self._buffer = buf
        self._pos = 0

    def __getstate__(self):
        if self.closed:
            raise ValueError("__getstate__ on closed file")
        return self.__dict__.copy()

    def __setstate__(self, state):
        # Inverse of __getstate__: the instance dict, whose buffer is copied so that
        # the stream never shares a bytearray with the state it was restored from.
        if self.closed:
            raise ValueError("__setstate__ on closed file")
        if not isinstance(state, dict):
            raise TypeError(f"__setstate__ argument should be a dict, "
                            f"got {type(state).__name__}")
        attrs = dict(state)
        buf = attrs.pop('_buffer', None)
        buf = bytearray() if buf is None else bytearray(buf)
        self.__dict__.update(attrs)
        self._buffer = buf

    def getvalue(self):
        """Return the bytes value (contents) of the buffer."""
        if self.closed:
            raise ValueError("getvalue on closed file")
        return bytes(self._buffer)

    def getbuffer(self):
        """Return a readable and writable view of the buffer."""
        if self.closed:
            raise ValueError("getbuffer on closed file")
        return memoryview(self._buffer)

    def close(self):
        if self._buffer is not None:
            self._buffer.clear()
        super().close()

    def read(self, size=-1):
        if self.closed:
            raise ValueError("read from closed file")
        if size is None:
            size = -1
        else:
            try:
                size_index = size.__index__
            except AttributeError:
                raise TypeError(f"{size!r} is not an integer")
            else:
                size = size_index()
        if size < 0:
            size = len(self._buffer)
        if len(self._buffer) <= self._pos:
            return b""
        newpos = min(len(self._buffer), self._pos + size)
        b = self._buffer[self._pos:newpos]
        self._pos = newpos
        return bytes(b)

    def read1(self, size=-1):
        """This is the same as read."""
        return self.read(size)

    def write(self, b):
        if self.closed:
            raise ValueError("write to closed file")
        if isinstance(b, str):
            raise TypeError("can't write str to binary stream")
        # export b once; obtaining the export may run code that closes us
        with memoryview(b) as view:
            if self.closed:
                raise ValueError("write to closed file")
            n = view.nbytes
            if n == 0:
                return 0
            pos = self._pos
            if pos > len(self._buffer):
                self._buffer.resize(pos)
            self._buffer[pos:pos + n] = view
        self._pos += n
        return n

    def seek(self, pos, whence=0):
        if self.closed:
            raise ValueError("seek on closed file")
        try:
            pos_index = pos.__index__
        except AttributeError:
            raise TypeError(f"{pos!r} is not an integer")
        else:
            pos = pos_index()
        if whence == 0:
            if pos < 0:
                raise ValueError("negative seek position %r" % (pos,))
            self._pos = pos
        elif whence == 1:
            self._pos = max(0, self._pos + pos)
        elif whence == 2:
            self._pos = max(0, len(self._buffer) + pos)
        else:
            raise ValueError("unsupported whence value")
        return self._pos

    def tell(self):
        if self.closed:
            raise ValueError("tell on closed file")
        return self._pos

    def truncate(self, pos=None):
        if self.closed:
            raise ValueError("truncate on closed file")
        if pos is None:
            pos = self._pos
        else:
            try:
                pos_index = pos.__index__
            except AttributeError:
                raise TypeError(f"{pos!r} is not an integer")
            else:
                pos = pos_index()
            if pos < 0:
                raise ValueError("negative truncate position %r" % (pos,))
        del self._buffer[pos:]
        return pos

    def readable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return True

    def writable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return True

    def seekable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return True


class BufferedReader(_BufferedIOMixin):

    """Buffer for a readable, sequential raw stream."""

    def __init__(self, raw, buffer_size=DEFAULT_BUFFER_SIZE):
        if not raw.readable():
            raise OSError('"raw" argument must be readable.')
        _BufferedIOMixin.__init__(self, raw)
        if buffer_size <= 0:
            raise ValueError("invalid buffer size")
        self.buffer_size = buffer_size
        self._reset_read_buf()
        self._read_lock = Lock()

    def readable(self):
        return self.raw.readable()

    def _reset_read_buf(self):
        self._read_buf = b""
        self._read_pos = 0

    def read(self, size=None):
        """Read size bytes (until EOF if size is None or negative); fewer only
        at EOF or when a non-blocking raw stream has no more data."""
        if size is not None and size < -1:
            raise ValueError("invalid number of bytes to read")
        with self._read_lock:
            return self._read_unlocked(size)

    def _read_unlocked(self, n=None):
        nodata_val = b""
        empty_values = (b"", None)
        buf = self._read_buf
        pos = self._read_pos

        if n is None or n == -1:
            self._reset_read_buf()
            if hasattr(self.raw, 'readall'):
                chunk = self.raw.readall()
                if chunk is None:
                    return buf[pos:] or None
                else:
                    return buf[pos:] + chunk
            chunks = [buf[pos:]]
            current_size = 0
            while True:
                chunk = self.raw.read()
                if chunk in empty_values:
                    nodata_val = chunk
                    break
                current_size += len(chunk)
                chunks.append(chunk)
            return b"".join(chunks) or nodata_val

        avail = len(buf) - pos
        if n <= avail:
            self._read_pos += n
            return buf[pos:pos + n]
        chunks = [buf[pos:]]
        wanted = max(self.buffer_size, n)
        while avail < n:
            chunk = self.raw.read(wanted)
            if chunk in empty_values:
                nodata_val = chunk
                break
            avail += len(chunk)
            chunks.append(chunk)
        n = min(n, avail)
        out = b"".join(chunks)
        self._read_buf = out[n:]
        self._read_pos = 0
        return out[:n] if out else nodata_val

    def peek(self, size=0):
        """Return buffered bytes without advancing (at most one raw read)."""
        self._checkClosed("peek of closed file")
        with self._read_lock:
            return self._peek_unlocked(size)

    def _peek_unlocked(self, n=0):
        want = min(n, self.buffer_size)
        have = len(self._read_buf) - self._read_pos
        if have < want or have <= 0:
            to_read = self.buffer_size - have
            current = self.raw.read(to_read)
            if current:
                self._read_buf = self._read_buf[self._read_pos:] + current
                self._read_pos = 0
        return self._read_buf[self._read_pos:]

    def read1(self, size=-1):
        """Read up to size bytes with at most one raw read."""
        self._checkClosed("read of closed file")
        if size < 0:
            size = self.buffer_size
        if size == 0:
            return b""
        with self._read_lock:
            self._peek_unlocked(1)
            return self._read_unlocked(
                min(size, len(self._read_buf) - self._read_pos))

    def _readinto(self, buf, read1):
        """Read data into buf with at most one system call."""
        self._checkClosed("readinto of closed file")
        if not isinstance(buf, memoryview):
            buf = memoryview(buf)
        if buf.nbytes == 0:
            return 0
        buf = buf.cast('B')

        written = 0
        with self._read_lock:
            while written < len(buf):
                avail = min(len(self._read_buf) - self._read_pos, len(buf))
                if avail:
                    buf[written:written + avail] = \
                        self._read_buf[self._read_pos:self._read_pos + avail]
                    self._read_pos += avail
                    written += avail
                    if written == len(buf):
                        break

                if len(buf) - written > self.buffer_size:
                    n = self.raw.readinto(buf[written:])
                    if not n:
                        break
                    written += n
                elif not (read1 and written):
                    if not self._peek_unlocked(1):
                        break

                if read1 and written:
                    break

        return written

    def tell(self):
        pos = _BufferedIOMixin.tell(self) - len(self._read_buf) + self._read_pos
        if pos < 0:
            # a raw stream that always reports 0 (character devices)
            pos = 0
        return pos

    def seek(self, pos, whence=0):
        if whence not in valid_seek_flags:
            raise ValueError("invalid whence value")
        self._checkClosed("seek of closed file")
        with self._read_lock:
            if whence == 1:
                pos -= len(self._read_buf) - self._read_pos
            pos = _BufferedIOMixin.seek(self, pos, whence)
            self._reset_read_buf()
            return pos


class BufferedWriter(_BufferedIOMixin):

    """Buffer for a writeable sequential raw stream."""

    def __init__(self, raw, buffer_size=DEFAULT_BUFFER_SIZE):
        if not raw.writable():
            raise OSError('"raw" argument must be writable.')
        _BufferedIOMixin.__init__(self, raw)
        if buffer_size <= 0:
            raise ValueError("invalid buffer size")
        self.buffer_size = buffer_size
        self._write_buf = bytearray()
        self._write_lock = Lock()

    def writable(self):
        return self.raw.writable()

    def write(self, b):
        if isinstance(b, str):
            raise TypeError("can't write str to binary stream")
        with self._write_lock:
            if self.closed:
                raise ValueError("write to closed file")
            if len(self._write_buf) > self.buffer_size:
                # A previous call was interrupted with too much pending data.
                self._flush_unlocked()
            before = len(self._write_buf)
            self._write_buf.extend(b)
            written = len(self._write_buf) - before
            if len(self._write_buf) > self.buffer_size:
                try:
                    self._flush_unlocked()
                except BlockingIOError as e:
                    if len(self._write_buf) > self.buffer_size:
                        # Drop what does not fit and report a partial write.
                        overage = len(self._write_buf) - self.buffer_size
                        written -= overage
                        self._write_buf = self._write_buf[:self.buffer_size]
                        raise BlockingIOError(e.errno, e.strerror, written)
            return written

    def truncate(self, pos=None):
        with self._write_lock:
            self._flush_unlocked()
            if pos is None:
                pos = self.raw.tell()
            return self.raw.truncate(pos)

    def flush(self):
        with self._write_lock:
            self._flush_unlocked()

    def _flush_unlocked(self):
        if self.closed:
            raise ValueError("flush on closed file")
        while self._write_buf:
            try:
                n = self.raw.write(self._write_buf)
            except BlockingIOError:
                raise RuntimeError("self.raw should implement RawIOBase: it "
                                   "should not raise BlockingIOError")
            if n is None:
                raise BlockingIOError(
                    errno.EAGAIN,
                    "write could not complete without blocking", 0)
            if n > len(self._write_buf) or n < 0:
                raise OSError("write() returned incorrect number of bytes")
            del self._write_buf[:n]

    def tell(self):
        return _BufferedIOMixin.tell(self) + len(self._write_buf)

    def seek(self, pos, whence=0):
        if whence not in valid_seek_flags:
            raise ValueError("invalid whence value")
        with self._write_lock:
            self._flush_unlocked()
            return _BufferedIOMixin.seek(self, pos, whence)

    def close(self):
        with self._write_lock:
            if self.raw is None or self.closed:
                return
        # Flush outside the lock: flush() may be overridden.
        try:
            self.flush()
        finally:
            with self._write_lock:
                self.raw.close()


class BufferedRWPair(BufferedIOBase):

    """A reader and a writer joined into one (non-seekable) stream."""

    def __init__(self, reader, writer, buffer_size=DEFAULT_BUFFER_SIZE, /):
        if not reader.readable():
            raise UnsupportedOperation('"reader" argument must be readable.')
        if not writer.writable():
            raise UnsupportedOperation('"writer" argument must be writable.')
        self.reader = BufferedReader(reader, buffer_size)
        self.writer = BufferedWriter(writer, buffer_size)

    def read(self, size=-1):
        if size is None:
            size = -1
        return self.reader.read(size)

    def readinto(self, b):
        return self.reader.readinto(b)

    def write(self, b):
        return self.writer.write(b)

    def peek(self, size=0):
        return self.reader.peek(size)

    def read1(self, size=-1):
        return self.reader.read1(size)

    def readinto1(self, b):
        return self.reader.readinto1(b)

    def readable(self):
        return self.reader.readable()

    def writable(self):
        return self.writer.writable()

    def flush(self):
        return self.writer.flush()

    def close(self):
        try:
            self.writer.close()
        finally:
            self.reader.close()

    def isatty(self):
        return self.reader.isatty() or self.writer.isatty()

    @property
    def closed(self):
        return self.writer.closed


class BufferedRandom(BufferedWriter, BufferedReader):

    """Buffered interface to a seekable raw stream (reading and writing)."""

    def __init__(self, raw, buffer_size=DEFAULT_BUFFER_SIZE):
        raw._checkSeekable()
        BufferedReader.__init__(self, raw, buffer_size)
        BufferedWriter.__init__(self, raw, buffer_size)

    def seek(self, pos, whence=0):
        if whence not in valid_seek_flags:
            raise ValueError("invalid whence value")
        self.flush()
        if self._read_buf:
            with self._read_lock:
                self.raw.seek(self._read_pos - len(self._read_buf), 1)
        pos = self.raw.seek(pos, whence)
        with self._read_lock:
            self._reset_read_buf()
        if pos < 0:
            raise OSError("seek() returned invalid position")
        return pos

    def tell(self):
        if self._write_buf:
            return BufferedWriter.tell(self)
        else:
            return BufferedReader.tell(self)

    def truncate(self, pos=None):
        if pos is None:
            pos = self.tell()
        return BufferedWriter.truncate(self, pos)

    def read(self, size=None):
        if size is None:
            size = -1
        self.flush()
        return BufferedReader.read(self, size)

    def readinto(self, b):
        self.flush()
        return BufferedReader.readinto(self, b)

    def peek(self, size=0):
        self.flush()
        return BufferedReader.peek(self, size)

    def read1(self, size=-1):
        self.flush()
        return BufferedReader.read1(self, size)

    def readinto1(self, b):
        self.flush()
        return BufferedReader.readinto1(self, b)

    def write(self, b):
        if self._read_buf:
            with self._read_lock:
                self.raw.seek(self._read_pos - len(self._read_buf), 1)
                self._reset_read_buf()
        return BufferedWriter.write(self, b)


def _mode_flags(mode):
    """Parse a FileIO mode: return (created, readable, writable, appending,
    flags)."""
    if not isinstance(mode, str):
        raise TypeError('invalid mode: %s' % (mode,))
    if not set(mode) <= set('xrwab+'):
        raise ValueError('invalid mode: %s' % (mode,))
    if sum(c in 'rwax' for c in mode) != 1 or mode.count('+') > 1:
        raise ValueError('Must have exactly one of create/read/write/append '
                         'mode and at most one plus')
    created = readable = writable = appending = False
    if 'x' in mode:
        created = writable = True
        flags = os.O_EXCL | os.O_CREAT
    elif 'r' in mode:
        readable = True
        flags = 0
    elif 'w' in mode:
        writable = True
        flags = os.O_CREAT | os.O_TRUNC
    else:
        writable = appending = True
        flags = os.O_APPEND | os.O_CREAT
    if '+' in mode:
        readable = writable = True
    if readable and writable:
        flags |= os.O_RDWR
    elif readable:
        flags |= os.O_RDONLY
    else:
        flags |= os.O_WRONLY
    flags |= getattr(os, 'O_BINARY', 0)
    flags |= getattr(os, 'O_NOINHERIT', 0) | getattr(os, 'O_CLOEXEC', 0)
    return created, readable, writable, appending, flags


class FileIO(RawIOBase):
    _fd = -1
    _created = False
    _readable = False
    _writable = False
    _appending = False
    _seekable = None
    _closefd = True
    _truncating = False

    def __init__(self, file, mode='r', closefd=True, opener=None):
        """Open a file for raw unbuffered I/O.

        mode is one of 'r', 'w', 'x', 'a' optionally with '+'; file is a path
        or a file descriptor (closefd=False leaves the descriptor open on
        close()); opener(name, flags) may return the descriptor to use.
        """
        if self._fd >= 0:
            self._stat_atopen = None
            try:
                if self._closefd:
                    os.close(self._fd)
            finally:
                self._fd = -1

        if isinstance(file, float):
            raise TypeError('integer argument expected, got float')
        if isinstance(file, int):
            if isinstance(file, bool):
                import warnings
                warnings.warn("bool is used as a file descriptor",
                              RuntimeWarning, 2)
                file = int(file)
            fd = file
            if fd < 0:
                raise ValueError('negative file descriptor')
        else:
            fd = -1

        (self._created, self._readable, self._writable, self._appending,
         flags) = _mode_flags(mode)
        # "w+" files are reported as "wb+" rather than "rb+"
        self._truncating = "w" in mode

        fd_is_own = False
        try:
            if fd < 0:
                if not closefd:
                    raise ValueError('Cannot use closefd=False with file name')
                if opener is None:
                    fd = os.open(file, flags, 0o666)
                else:
                    fd = opener(file, flags)
                    if not isinstance(fd, int):
                        raise TypeError('expected integer from opener')
                    if fd < 0:
                        # (a negative fd without errno would be meaningless)
                        raise ValueError(f'opener returned {fd}')
                    os.set_inheritable(fd, False)
                fd_is_own = True
            self._closefd = closefd
            self._stat_atopen = os.fstat(fd)
            try:
                if stat.S_ISDIR(self._stat_atopen.st_mode):
                    raise IsADirectoryError(errno.EISDIR,
                                            os.strerror(errno.EISDIR), file)
            except AttributeError:
                pass

            if _setmode:
                _setmode(fd, os.O_BINARY)

            self.name = file
            if self._appending:
                try:
                    os.lseek(fd, 0, SEEK_END)
                except OSError as e:
                    if e.errno != errno.ESPIPE:
                        raise
        except:
            self._stat_atopen = None
            if fd_is_own:
                os.close(fd)
            raise
        self._fd = fd

    def _dealloc_warn(self, source):
        if self._fd >= 0 and self._closefd and not self.closed:
            import warnings
            warnings.warn(f'unclosed file {source!r}', ResourceWarning,
                          stacklevel=2, source=self)

    def __del__(self):
        if self._fd >= 0 and self._closefd and not self.closed:
            import warnings
            warnings.warn(f'unclosed file {self!r}', ResourceWarning,
                          stacklevel=2, source=self)
            self.close()

    def __getstate__(self):
        raise TypeError(f"cannot pickle {self.__class__.__name__!r} object")

    def __repr__(self):
        class_name = '%s.%s' % (self.__class__.__module__,
                                self.__class__.__qualname__)
        if self.closed:
            return '<%s [closed]>' % class_name
        try:
            name = self.name
        except AttributeError:
            return ('<%s fd=%d mode=%r closefd=%r>' %
                    (class_name, self._fd, self.mode, self._closefd))
        else:
            return ('<%s name=%r mode=%r closefd=%r>' %
                    (class_name, name, self.mode, self._closefd))

    @property
    def _blksize(self):
        if self._stat_atopen is None:
            return DEFAULT_BUFFER_SIZE
        blksize = getattr(self._stat_atopen, "st_blksize", 0)
        if not blksize:
            return DEFAULT_BUFFER_SIZE
        return blksize

    def _checkReadable(self):
        if not self._readable:
            raise UnsupportedOperation('File not open for reading')

    def _checkWritable(self, msg=None):
        if not self._writable:
            raise UnsupportedOperation('File not open for writing')

    def read(self, size=None):
        """Read at most size bytes (everything if size is None or negative)."""
        self._checkClosed()
        self._checkReadable()
        if size is None or size < 0:
            return self.readall()
        try:
            return os.read(self._fd, size)
        except BlockingIOError:
            return None

    def readall(self):
        """Read until EOF with as few system calls as practical."""
        self._checkClosed()
        self._checkReadable()
        if self._stat_atopen is None or self._stat_atopen.st_size <= 0:
            bufsize = DEFAULT_BUFFER_SIZE
        else:
            bufsize = self._stat_atopen.st_size + 1
            if self._stat_atopen.st_size > 65536:
                try:
                    pos = os.lseek(self._fd, 0, SEEK_CUR)
                    if self._stat_atopen.st_size >= pos:
                        bufsize = self._stat_atopen.st_size - pos + 1
                except OSError:
                    pass

        result = bytearray(bufsize)
        bytes_read = 0
        try:
            while n := os.readinto(self._fd, memoryview(result)[bytes_read:]):
                bytes_read += n
                if bytes_read >= len(result):
                    result.resize(_new_buffersize(bytes_read))
        except BlockingIOError:
            if not bytes_read:
                return None
        assert len(result) - bytes_read >= 1, \
            "os.readinto buffer size 0 will result in erroneous EOF / returns 0"
        result.resize(bytes_read)
        return bytes(result)

    def readinto(self, buffer):
        """Same as RawIOBase.readinto()."""
        self._checkClosed()
        self._checkReadable()
        try:
            return os.readinto(self._fd, buffer)
        except BlockingIOError:
            return None

    def write(self, b):
        """Write b; return the number of bytes written (None if it would block)."""
        self._checkClosed()
        self._checkWritable()
        try:
            return os.write(self._fd, b)
        except BlockingIOError:
            return None

    def seek(self, pos, whence=SEEK_SET):
        """Move to position pos relative to whence (0 start, 1 current, 2 end)."""
        if isinstance(pos, float):
            raise TypeError('an integer is required')
        self._checkClosed()
        return os.lseek(self._fd, pos, whence)

    def tell(self):
        """Return the current file position."""
        self._checkClosed()
        return os.lseek(self._fd, 0, SEEK_CUR)

    def truncate(self, size=None):
        """Resize the file to size bytes (default: current position)."""
        self._checkClosed()
        self._checkWritable()
        if size is None:
            size = self.tell()
        os.ftruncate(self._fd, size)
        self._stat_atopen = None
        return size

    def close(self):
        """Close the file (and its descriptor unless closefd is False)."""
        if not self.closed:
            self._stat_atopen = None
            try:
                if self._closefd and self._fd >= 0:
                    os.close(self._fd)
            finally:
                super().close()

    def seekable(self):
        """True if the file supports random access."""
        self._checkClosed()
        if self._seekable is None:
            try:
                self.tell()
            except OSError:
                self._seekable = False
            else:
                self._seekable = True
        return self._seekable

    def readable(self):
        """True if the file was opened for reading."""
        self._checkClosed()
        return self._readable

    def writable(self):
        """True if the file was opened for writing."""
        self._checkClosed()
        return self._writable

    def fileno(self):
        """Return the underlying file descriptor."""
        self._checkClosed()
        return self._fd

    def isatty(self):
        """True if the file is connected to a TTY device."""
        self._checkClosed()
        return os.isatty(self._fd)

    def _isatty_open_only(self):
        """Like isatty(), but cheap for regular files right after open()."""
        if (self._stat_atopen is not None
                and not stat.S_ISCHR(self._stat_atopen.st_mode)):
            return False
        return os.isatty(self._fd)

    @property
    def closefd(self):
        """True if close() also closes the descriptor."""
        return self._closefd

    @property
    def mode(self):
        """The mode string as seen by the outside world."""
        if self._created:
            if self._readable:
                return 'xb+'
            else:
                return 'xb'
        elif self._appending:
            if self._readable:
                return 'ab+'
            else:
                return 'ab'
        elif self._readable:
            if self._writable:
                return 'wb+' if self._truncating else 'rb+'
            else:
                return 'rb'
        else:
            return 'wb'


def _new_buffersize(bytes_read):
    # grow by ~25% with a 64 KiB floor
    addend = bytes_read >> 3
    if addend < 1 << 15:
        addend = 1 << 15
    return bytes_read + addend


class TextIOBase(IOBase):

    """Base class for text I/O (str in, str out)."""

    def read(self, size=-1):
        """Read at most size characters (until EOF if size is negative)."""
        self._unsupported("read")

    def write(self, s):
        """Write the str s; return the number of characters written."""
        self._unsupported("write")

    def truncate(self, pos=None):
        """Truncate the size of the stream to pos (default: current position)."""
        self._unsupported("truncate")

    def readline(self):
        """Read until newline or EOF."""
        self._unsupported("readline")

    def detach(self):
        """Separate the underlying buffer from the TextIOBase and return it."""
        self._unsupported("detach")

    @property
    def encoding(self):
        """Subclasses should override."""
        return None

    @property
    def newlines(self):
        """Line endings translated so far (None, a str or a tuple of str)."""
        return None

    @property
    def errors(self):
        """Error setting of the decoder or encoder."""
        return None

io.TextIOBase.register(TextIOBase)


class IncrementalNewlineDecoder(codecs.IncrementalDecoder):
    r"""Wrap a decoder translating \r\n and \r into \n (if translate) and
    recording which line endings were seen."""

    def __init__(self, decoder, translate, errors='strict'):
        codecs.IncrementalDecoder.__init__(self, errors=errors)
        self.translate = translate
        self.decoder = decoder
        self.seennl = 0
        self.pendingcr = False

    def decode(self, input, final=False):
        if self.decoder is None:
            output = input
        else:
            output = self.decoder.decode(input, final=final)
        if self.pendingcr and (output or final):
            output = "\r" + output
            self.pendingcr = False

        # retain a trailing \r: it may start a \r\n in the next chunk
        if output.endswith("\r") and not final:
            output = output[:-1]
            self.pendingcr = True

        crlf = output.count('\r\n')
        cr = output.count('\r') - crlf
        lf = output.count('\n') - crlf
        self.seennl |= (lf and self._LF) | (cr and self._CR) \
                    | (crlf and self._CRLF)

        if self.translate:
            if crlf:
                output = output.replace("\r\n", "\n")
            if cr:
                output = output.replace("\r", "\n")

        return output

    def getstate(self):
        if self.decoder is None:
            buf = b""
            flag = 0
        else:
            buf, flag = self.decoder.getstate()
        flag <<= 1
        if self.pendingcr:
            flag |= 1
        return buf, flag

    def setstate(self, state):
        buf, flag = state
        self.pendingcr = bool(flag & 1)
        if self.decoder is not None:
            self.decoder.setstate((buf, flag >> 1))

    def reset(self):
        self.seennl = 0
        self.pendingcr = False
        if self.decoder is not None:
            self.decoder.reset()

    _LF = 1
    _CR = 2
    _CRLF = 4

    @property
    def newlines(self):
        return (None,
                "\n",
                "\r",
                ("\r", "\n"),
                "\r\n",
                ("\n", "\r\n"),
                ("\r", "\r\n"),
                ("\r", "\n", "\r\n")
               )[self.seennl]


class TextIOWrapper(TextIOBase):

    r"""Character and line based layer over a BufferedIOBase object."""

    _CHUNK_SIZE = 8192

    # set early: __del__ -> close() looks at it
    _buffer = None

    def __init__(self, buffer, encoding=None, errors=None, newline=None,
                 line_buffering=False, write_through=False):
        self._check_newline(newline)
        encoding = self._check_encoding(encoding)
        if errors is None:
            errors = "strict"
        elif not isinstance(errors, str):
            raise ValueError("invalid errors: %r" % errors)
        elif _CHECK_ERRORS:
            codecs.lookup_error(errors)
        self._buffer = buffer
        self._decoded_chars = ''
        self._decoded_chars_used = 0
        self._snapshot = None
        self._seekable = self._telling = self.buffer.seekable()
        self._has_read1 = hasattr(self.buffer, 'read1')
        self._configure(encoding, errors, newline,
                        line_buffering, write_through)

    def _check_newline(self, newline):
        if newline is not None and not isinstance(newline, str):
            raise TypeError("illegal newline type: %r" % (type(newline),))
        if newline not in (None, "", "\n", "\r", "\r\n"):
            raise ValueError("illegal newline value: %r" % (newline,))

    def _check_encoding(self, encoding):
        if encoding is None:
            encoding = text_encoding(None, 3)
        if encoding == "locale":
            encoding = self._get_locale_encoding()
        if not isinstance(encoding, str):
            raise ValueError("invalid encoding: %r" % encoding)
        if not codecs.lookup(encoding)._is_text_encoding:
            msg = ("%r is not a text encoding; "
                   "use codecs.open() to handle arbitrary codecs")
            raise LookupError(msg % encoding)
        return encoding

    def _configure(self, encoding=None, errors=None, newline=None,
                   line_buffering=False, write_through=False):
        self._encoding = encoding
        self._errors = errors
        self._encoder = None
        self._decoder = None
        self._b2cratio = 0.0

        self._readuniversal = not newline
        self._readtranslate = newline is None
        self._readnl = newline
        self._writetranslate = newline != ''
        self._writenl = newline or os.linesep

        self._line_buffering = line_buffering
        self._write_through = write_through

        # don't write a BOM in the middle of a file
        if self._seekable and self.writable():
            position = self.buffer.tell()
            if position != 0:
                try:
                    self._get_encoder().setstate(0)
                except LookupError:
                    pass

    def __repr__(self):
        result = "<{}.{}".format(self.__class__.__module__,
                                 self.__class__.__qualname__)
        try:
            name = self.name
        except AttributeError:
            pass
        else:
            result += " name={0!r}".format(name)
        try:
            mode = self.mode
        except AttributeError:
            pass
        else:
            result += " mode={0!r}".format(mode)
        return result + " encoding={0!r}>".format(self.encoding)

    @property
    def encoding(self):
        return self._encoding

    @property
    def errors(self):
        return self._errors

    @property
    def line_buffering(self):
        return self._line_buffering

    @property
    def write_through(self):
        return self._write_through

    @property
    def buffer(self):
        return self._buffer

    def _dealloc_warn(self, source):
        dealloc_warn = getattr(self.buffer, "_dealloc_warn", None)
        if dealloc_warn is not None:
            dealloc_warn(source)

    def reconfigure(self, *,
                    encoding=None, errors=None, newline=Ellipsis,
                    line_buffering=None, write_through=None):
        """Reconfigure the text stream with new parameters.

        This also flushes the stream.
        """
        if (self._decoder is not None
                and (encoding is not None or errors is not None
                     or newline is not Ellipsis)):
            raise UnsupportedOperation(
                "It is not possible to set the encoding or newline of stream "
                "after the first read")

        if errors is None:
            if encoding is None:
                errors = self._errors
            else:
                errors = 'strict'
        elif not isinstance(errors, str):
            raise TypeError("invalid errors: %r" % errors)

        if encoding is None:
            encoding = self._encoding
        else:
            if not isinstance(encoding, str):
                raise TypeError("invalid encoding: %r" % encoding)
            encoding = self._check_encoding(encoding)

        if newline is Ellipsis:
            newline = self._readnl
        self._check_newline(newline)

        if line_buffering is None:
            line_buffering = self.line_buffering
        if write_through is None:
            write_through = self.write_through

        self.flush()
        self._configure(encoding, errors, newline,
                        line_buffering, write_through)

    def seekable(self):
        if self.closed:
            raise ValueError("I/O operation on closed file.")
        return self._seekable

    def readable(self):
        return self.buffer.readable()

    def writable(self):
        return self.buffer.writable()

    def flush(self):
        self.buffer.flush()
        self._telling = self._seekable

    def close(self):
        if self.buffer is not None and not self.closed:
            try:
                self.flush()
            finally:
                self.buffer.close()

    @property
    def closed(self):
        return self.buffer.closed

    @property
    def name(self):
        return self.buffer.name

    def fileno(self):
        return self.buffer.fileno()

    def isatty(self):
        return self.buffer.isatty()

    def write(self, s):
        'Write data, where s is a str'
        if self.closed:
            raise ValueError("write to closed file")
        if not isinstance(s, str):
            raise TypeError("can't write %s to text stream" %
                            s.__class__.__name__)
        length = len(s)
        haslf = (self._writetranslate or self._line_buffering) and "\n" in s
        if haslf and self._writetranslate and self._writenl != "\n":
            s = s.replace("\n", self._writenl)
        encoder = self._encoder or self._get_encoder()
        b = encoder.encode(s)
        self.buffer.write(b)
        if self._line_buffering and (haslf or "\r" in s):
            self.flush()
        if self._snapshot is not None:
            self._set_decoded_chars('')
            self._snapshot = None
        if self._decoder:
            self._decoder.reset()
        return length

    def _get_encoder(self):
        make_encoder = codecs.getincrementalencoder(self._encoding)
        self._encoder = make_encoder(self._errors)
        return self._encoder

    def _get_decoder(self):
        make_decoder = codecs.getincrementaldecoder(self._encoding)
        decoder = make_decoder(self._errors)
        if self._readuniversal:
            decoder = IncrementalNewlineDecoder(decoder, self._readtranslate)
        self._decoder = decoder
        return decoder

    # The snapshot remembers (decoder flags, bytes fed since the snapshot) so
    # that tell() can rebuild an opaque cookie for the current position.

    def _set_decoded_chars(self, chars):
        """Set the _decoded_chars buffer."""
        self._decoded_chars = chars
        self._decoded_chars_used = 0

    def _get_decoded_chars(self, n=None):
        """Advance into the _decoded_chars buffer."""
        offset = self._decoded_chars_used
        if n is None:
            chars = self._decoded_chars[offset:]
        else:
            chars = self._decoded_chars[offset:offset + n]
        self._decoded_chars_used += len(chars)
        return chars

    def _get_locale_encoding(self):
        try:
            import locale
        except ImportError:
            return 'utf-8'
        else:
            return locale.getencoding()

    def _rewind_decoded_chars(self, n):
        """Rewind the _decoded_chars buffer."""
        if self._decoded_chars_used < n:
            raise AssertionError("rewind decoded_chars out of bounds")
        self._decoded_chars_used -= n

    def _read_chunk(self):
        """Read and decode the next chunk; return False at EOF."""
        if self._decoder is None:
            raise ValueError("no decoder")

        if self._telling:
            dec_buffer, dec_flags = self._decoder.getstate()

        if self._has_read1:
            input_chunk = self.buffer.read1(self._CHUNK_SIZE)
        else:
            input_chunk = self.buffer.read(self._CHUNK_SIZE)
        eof = not input_chunk
        decoded_chars = self._decoder.decode(input_chunk, eof)
        self._set_decoded_chars(decoded_chars)
        if decoded_chars:
            self._b2cratio = len(input_chunk) / len(self._decoded_chars)
        else:
            self._b2cratio = 0.0

        if self._telling:
            self._snapshot = (dec_flags, dec_buffer + input_chunk)

        return not eof

    def _pack_cookie(self, position, dec_flags=0,
                     bytes_to_feed=0, need_eof=False, chars_to_skip=0):
        return (position | (dec_flags << 64) | (bytes_to_feed << 128)
                | (chars_to_skip << 192) | bool(need_eof) << 256)

    def _unpack_cookie(self, bigint):
        rest, position = divmod(bigint, 1 << 64)
        rest, dec_flags = divmod(rest, 1 << 64)
        rest, bytes_to_feed = divmod(rest, 1 << 64)
        need_eof, chars_to_skip = divmod(rest, 1 << 64)
        return position, dec_flags, bytes_to_feed, bool(need_eof), chars_to_skip

    def tell(self):
        if not self._seekable:
            raise UnsupportedOperation("underlying stream is not seekable")
        if not self._telling:
            raise OSError("telling position disabled by next() call")
        self.flush()
        position = self.buffer.tell()
        decoder = self._decoder
        if decoder is None or self._snapshot is None:
            if self._decoded_chars:
                raise AssertionError("pending decoded text")
            return position

        dec_flags, next_input = self._snapshot
        position -= len(next_input)

        chars_to_skip = self._decoded_chars_used
        if chars_to_skip == 0:
            return self._pack_cookie(position, dec_flags)

        saved_state = decoder.getstate()
        try:
            # Coarse search: try to skip whole bytes using the b2c ratio.
            skip_bytes = int(self._b2cratio * chars_to_skip)
            skip_back = 1
            while skip_bytes > 0:
                decoder.setstate((b'', dec_flags))
                n = len(decoder.decode(next_input[:skip_bytes]))
                if n <= chars_to_skip:
                    b, d = decoder.getstate()
                    if not b:
                        dec_flags = d
                        chars_to_skip -= n
                        break
                    skip_bytes -= len(b)
                    skip_back = 1
                else:
                    skip_bytes -= skip_back
                    skip_back = skip_back * 2
            else:
                skip_bytes = 0
                decoder.setstate((b'', dec_flags))

            start_pos = position + skip_bytes
            start_flags = dec_flags
            if chars_to_skip == 0:
                return self._pack_cookie(start_pos, start_flags)

            # Fine search: feed byte by byte.
            bytes_fed = 0
            need_eof = False
            chars_decoded = 0
            for i in range(skip_bytes, len(next_input)):
                bytes_fed += 1
                chars_decoded += len(decoder.decode(next_input[i:i+1]))
                dec_buffer, dec_flags = decoder.getstate()
                if not dec_buffer and chars_decoded <= chars_to_skip:
                    start_pos += bytes_fed
                    chars_to_skip -= chars_decoded
                    start_flags, bytes_fed, chars_decoded = dec_flags, 0, 0
                if chars_decoded >= chars_to_skip:
                    break
            else:
                chars_decoded += len(decoder.decode(b'', final=True))
                need_eof = True
                if chars_decoded < chars_to_skip:
                    raise OSError("can't reconstruct logical file position")

            return self._pack_cookie(
                start_pos, start_flags, bytes_fed, need_eof, chars_to_skip)
        finally:
            decoder.setstate(saved_state)

    def truncate(self, pos=None):
        self.flush()
        if pos is None:
            pos = self.tell()
        return self.buffer.truncate(pos)

    def detach(self):
        if self.buffer is None:
            raise ValueError("buffer is already detached")
        self.flush()
        buffer = self._buffer
        self._buffer = None
        return buffer

    def seek(self, cookie, whence=0):
        def _reset_encoder(position):
            """Reset the encoder (merely useful for proper BOM handling)"""
            try:
                encoder = self._encoder or self._get_encoder()
            except LookupError:
                # Sometimes the encoder doesn't exist
                pass
            else:
                if position != 0:
                    encoder.setstate(0)
                else:
                    encoder.reset()

        if self.closed:
            raise ValueError("tell on closed file")
        if not self._seekable:
            raise UnsupportedOperation("underlying stream is not seekable")
        if whence == SEEK_CUR:
            if cookie != 0:
                raise UnsupportedOperation("can't do nonzero cur-relative seeks")
            whence = 0
            cookie = self.tell()
        elif whence == SEEK_END:
            if cookie != 0:
                raise UnsupportedOperation("can't do nonzero end-relative seeks")
            self.flush()
            position = self.buffer.seek(0, whence)
            self._set_decoded_chars('')
            self._snapshot = None
            if self._decoder:
                self._decoder.reset()
            _reset_encoder(position)
            return position
        if whence != 0:
            raise ValueError("unsupported whence (%r)" % (whence,))
        if cookie < 0:
            raise ValueError("negative seek position %r" % (cookie,))
        self.flush()

        start_pos, dec_flags, bytes_to_feed, need_eof, chars_to_skip = \
            self._unpack_cookie(cookie)

        self.buffer.seek(start_pos)
        self._set_decoded_chars('')
        self._snapshot = None

        if cookie == 0 and self._decoder:
            self._decoder.reset()
        elif self._decoder or dec_flags or chars_to_skip:
            self._decoder = self._decoder or self._get_decoder()
            self._decoder.setstate((b'', dec_flags))
            self._snapshot = (dec_flags, b'')

        if chars_to_skip:
            input_chunk = self.buffer.read(bytes_to_feed)
            self._set_decoded_chars(
                self._decoder.decode(input_chunk, need_eof))
            self._snapshot = (dec_flags, input_chunk)

            if len(self._decoded_chars) < chars_to_skip:
                raise OSError("can't restore logical file position")
            self._decoded_chars_used = chars_to_skip

        _reset_encoder(cookie)
        return cookie

    def read(self, size=None):
        self._checkReadable()
        if size is None:
            size = -1
        else:
            try:
                size_index = size.__index__
            except AttributeError:
                raise TypeError(f"{size!r} is not an integer")
            else:
                size = size_index()
        decoder = self._decoder or self._get_decoder()
        if size < 0:
            chunk = self.buffer.read()
            if chunk is None:
                raise BlockingIOError("Read returned None.")
            result = (self._get_decoded_chars() +
                      decoder.decode(chunk, final=True))
            if self._snapshot is not None:
                self._set_decoded_chars('')
                self._snapshot = None
            return result
        else:
            eof = False
            result = self._get_decoded_chars(size)
            while len(result) < size and not eof:
                eof = not self._read_chunk()
                result += self._get_decoded_chars(size - len(result))
            return result

    def __next__(self):
        self._telling = False
        line = self.readline()
        if not line:
            self._snapshot = None
            self._telling = self._seekable
            raise StopIteration
        return line

    def _line_end(self, text, start):
        """Index just past the first line ending in text[start:], or -1.
        Returns -2 when text ends with a lone '\\r' that may begin '\\r\\n'."""
        if self._readtranslate:
            i = text.find('\n', start)
            return i + 1 if i >= 0 else -1
        if not self._readuniversal:
            i = text.find(self._readnl, start)
            return i + len(self._readnl) if i >= 0 else -1
        for i in range(start, len(text)):
            ch = text[i]
            if ch == '\n':
                return i + 1
            if ch == '\r':
                if i + 1 == len(text):
                    return -2
                return i + 2 if text[i + 1] == '\n' else i + 1
        return -1

    def readline(self, size=None):
        if self.closed:
            raise ValueError("read from closed file")
        if size is None:
            size = -1
        else:
            try:
                to_index = size.__index__
            except AttributeError:
                raise TypeError(f"{size!r} is not an integer")
            size = to_index()
        if not self._decoder:
            self._get_decoder()

        text = self._get_decoded_chars()
        scanned = 0
        while True:
            end = self._line_end(text, scanned)
            if end >= 0:
                break
            if end == -2:
                scanned = len(text) - 1
            elif not self._readtranslate and not self._readuniversal:
                # a "\r\n" terminator may straddle the chunk boundary
                scanned = max(0, len(text) - len(self._readnl) + 1)
            else:
                scanned = len(text)
            if 0 <= size <= len(text):
                end = size
                break
            more = ''
            while self._read_chunk():
                if self._decoded_chars:
                    break
            if self._decoded_chars:
                more = self._get_decoded_chars()
            if not more:
                self._set_decoded_chars('')
                self._snapshot = None
                return text
            text += more

        if 0 <= size < end:
            end = size
        self._rewind_decoded_chars(len(text) - end)
        return text[:end]

    @property
    def newlines(self):
        return self._decoder.newlines if self._decoder else None


class StringIO(TextIOWrapper):
    """Text I/O implementation using an in-memory buffer."""

    def __init__(self, initial_value="", newline="\n"):
        super(StringIO, self).__init__(BytesIO(),
                                       encoding="utf-8",
                                       errors="surrogatepass",
                                       newline=newline)
        if newline is None:
            self._writetranslate = False
        if initial_value is not None:
            if not isinstance(initial_value, str):
                raise TypeError("initial_value must be str or None, not {0}"
                                .format(type(initial_value).__name__))
            self.write(initial_value)
            self.seek(0)

    def getvalue(self):
        self.flush()
        decoder = self._decoder or self._get_decoder()
        old_state = decoder.getstate()
        decoder.reset()
        try:
            return decoder.decode(self.buffer.getvalue(), final=True)
        finally:
            decoder.setstate(old_state)

    def __repr__(self):
        return object.__repr__(self)

    @property
    def errors(self):
        return None

    @property
    def encoding(self):
        return None

    def detach(self):
        self._unsupported("detach")
