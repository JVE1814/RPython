"""Mailboxes in the Maildir, mbox, MH, Babyl and MMDF formats.

A mailbox is a dictionary-like collection of messages: keys are chosen by
the mailbox when a message is added, values are message objects (by
default instances of the format's Message subclass).  Each format also has
a Message subclass that carries its format-specific state (flags, labels,
sequences, envelope sender, ...) and converts it when a message moves to
another format.
"""

import os
import time
import calendar
import socket
import errno
import copy
import warnings
import email
import email.message
import email.generator
import io
import contextlib
from types import GenericAlias
try:
    import fcntl
except ImportError:
    fcntl = None

__all__ = ['Mailbox', 'Maildir', 'mbox', 'MH', 'Babyl', 'MMDF',
           'Message', 'MaildirMessage', 'mboxMessage', 'MHMessage',
           'BabylMessage', 'MMDFMessage', 'Error', 'NoSuchMailboxError',
           'NotEmptyError', 'ExternalClashError', 'FormatError']

# Mailbox files use the platform's line ending; messages handed out use \n.
linesep = os.linesep.encode('ascii')


def _to_internal(data):
    """Bytes read from a mailbox file, with \\n line endings."""
    return data.replace(linesep, b'\n')


def _universal_line(line):
    """A line read from a file object, with its ending turned into \\n."""
    if line.endswith(b'\r\n'):
        return line[:-2] + b'\n'
    if line.endswith(b'\r'):
        return line[:-1] + b'\n'
    return line


def _not_implemented(*args, **kwargs):
    raise NotImplementedError('Method must be implemented by subclass')


class Mailbox:
    """The interface every mailbox format implements."""

    def __init__(self, path, factory=None, create=True):
        self._path = os.path.abspath(os.path.expanduser(path))
        self._factory = factory

    # The format classes provide these.
    add = remove = __setitem__ = get_message = get_bytes = get_file = \
        iterkeys = __contains__ = __len__ = flush = lock = unlock = \
        close = _not_implemented


    def __delitem__(self, key):
        self.remove(key)

    def discard(self, key):
        """Remove the message with *key*, if there is one."""
        try:
            self.remove(key)
        except KeyError:
            pass

    def get(self, key, default=None):
        try:
            return self.__getitem__(key)
        except KeyError:
            return default

    def __getitem__(self, key):
        if not self._factory:
            return self.get_message(key)
        with contextlib.closing(self.get_file(key)) as file:
            return self._factory(file)

    def get_string(self, key):
        """The message as a str (8-bit content goes through the email
        package's surrogate handling)."""
        return email.message_from_bytes(self.get_bytes(key)).as_string()

    def keys(self):
        return list(self.iterkeys())

    def itervalues(self):
        for key in self.iterkeys():
            try:
                value = self[key]
            except KeyError:
                continue        # removed meanwhile
            yield value

    def __iter__(self):
        return self.itervalues()

    def values(self):
        return list(self.itervalues())

    def iteritems(self):
        for key in self.iterkeys():
            try:
                value = self[key]
            except KeyError:
                continue
            yield key, value

    def items(self):
        return list(self.iteritems())

    def clear(self):
        for key in self.keys():
            self.discard(key)

    def pop(self, key, default=None):
        try:
            message = self[key]
        except KeyError:
            return default
        self.discard(key)
        return message

    def popitem(self):
        for key in self.iterkeys():
            return key, self.pop(key)
        raise KeyError('No messages in mailbox')

    def update(self, arg=None):
        """Replace messages from a mapping or an iterable of pairs."""
        if hasattr(arg, 'iteritems'):
            pairs = arg.iteritems()
        elif hasattr(arg, 'items'):
            pairs = arg.items()
        else:
            pairs = arg
        missing = False
        for key, message in pairs:
            try:
                self[key] = message
            except KeyError:
                missing = True
        if missing:
            raise KeyError('No message with key(s)')

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()

    # Mailbox files only hold 8-bit data written by us as bytes, but in
    # the past strings with arbitrary characters were accepted; refuse
    # those clearly now.
    def _string_to_bytes(self, message):
        try:
            return message.encode('ascii')
        except UnicodeError:
            raise ValueError("String input must be ASCII-only; "
                             "use bytes or a Message instead") from None

    # mbox wants every message to end with a line ending
    _append_newline = False

    def _dump_message(self, message, target, mangle_from_=False):
        """Write *message* (a Message, str, bytes or binary file) to the
        binary file *target* with the mailbox's line endings."""
        if isinstance(message, email.message.Message):
            out = io.BytesIO()
            email.generator.BytesGenerator(out, mangle_from_, 0).flatten(
                message)
            data = out.getvalue().replace(b'\n', linesep)
            self._write_body(target, data)
            return
        if isinstance(message, (str, bytes, io.StringIO)):
            if isinstance(message, io.StringIO):
                warnings.warn("Use of StringIO input is deprecated, "
                              "use BytesIO instead", DeprecationWarning, 3)
                message = message.getvalue()
            if isinstance(message, str):
                message = self._string_to_bytes(message)
            if mangle_from_:
                message = message.replace(b'\nFrom ', b'\n>From ')
            self._write_body(target, message.replace(b'\n', linesep))
            return
        if hasattr(message, 'read'):
            if hasattr(message, 'buffer'):
                warnings.warn("Use of text mode files is deprecated, "
                              "use a binary mode file instead",
                              DeprecationWarning, 3)
                message = message.buffer
            last = None
            for line in iter(message.readline, b''):
                line = _universal_line(line)
                if mangle_from_ and line.startswith(b'From '):
                    line = b'>From ' + line[5:]
                line = line.replace(b'\n', linesep)
                target.write(line)
                last = line
            if self._append_newline and last and not last.endswith(linesep):
                target.write(linesep)
            return
        raise TypeError('Invalid message type: %s' % type(message))

    def _write_body(self, target, data):
        target.write(data)
        if self._append_newline and not data.endswith(linesep):
            target.write(linesep)

    __class_getitem__ = classmethod(GenericAlias)


class Maildir(Mailbox):
    """A directory with tmp/, new/ and cur/ subdirectories, one file per
    message; extra info (flags) follows a colon in the file name."""

    colon = ':'

    # serial number in names of files created by this process
    _count = 1

    def __init__(self, dirname, factory=None, create=True):
        Mailbox.__init__(self, dirname, factory, create)
        self._paths = {sub: os.path.join(self._path, sub)
                       for sub in ('tmp', 'new', 'cur')}
        if not os.path.exists(self._path):
            if not create:
                raise NoSuchMailboxError(self._path)
            os.mkdir(self._path, 0o700)
            for sub in self._paths.values():
                os.mkdir(sub, 0o700)
        self._toc = {}                          # key -> "new/name" etc.
        self._toc_mtimes = {'cur': 0, 'new': 0}
        self._last_read = 0
        # Directory mtimes may be coarse (2 s on FAT): within this window
        # after a scan, rescan regardless of mtimes.
        self._skewfactor = 0.1

    def _full(self, subpath):
        return os.path.join(self._path, subpath)

    @staticmethod
    def _info_of(message):
        """(subdirectory, file name suffix) for a message being stored."""
        if not isinstance(message, MaildirMessage):
            return 'new', ''
        info = message.get_info()
        return message.get_subdir(), (Maildir.colon + info if info else '')

    def add(self, message):
        tmp = self._create_tmp()
        try:
            self._dump_message(message, tmp)
        except BaseException:
            tmp.close()
            os.remove(tmp.name)
            raise
        _fsync_close(tmp)

        subdir, suffix = self._info_of(message)
        key = os.path.basename(tmp.name).split(self.colon)[0]
        dest = os.path.join(self._path, subdir, key + suffix)
        if isinstance(message, MaildirMessage):
            os.utime(tmp.name,
                     (os.path.getatime(tmp.name), message.get_date()))
        # link() refuses to replace an existing file, rename() would not
        try:
            try:
                os.link(tmp.name, dest)
            except (AttributeError, PermissionError):
                os.rename(tmp.name, dest)
            else:
                os.remove(tmp.name)
        except OSError as err:
            os.remove(tmp.name)
            if err.errno == errno.EEXIST:
                raise ExternalClashError(
                    'Name clash with existing message: %s' % dest)
            raise
        return key

    def remove(self, key):
        os.remove(self._full(self._lookup(key)))

    def discard(self, key):
        try:
            self.remove(key)
        except (KeyError, FileNotFoundError):
            pass

    def __setitem__(self, key, message):
        old_subpath = self._lookup(key)
        new_key = self.add(message)
        new_subpath = self._lookup(new_key)
        # The new message's own subdirectory/info wins only if it carries
        # Maildir state; otherwise the old file's placement is kept.
        model = (new_subpath if isinstance(message, MaildirMessage)
                 else old_subpath)
        subdir = os.path.dirname(model)
        name = os.path.basename(model)
        suffix = (self.colon + name.split(self.colon)[-1]
                  if self.colon in name else '')
        self.discard(key)
        source = self._full(new_subpath)
        if isinstance(message, MaildirMessage):
            os.utime(source, (os.path.getatime(source), message.get_date()))
        os.rename(source, os.path.join(self._path, subdir, key + suffix))

    def get_message(self, key):
        subpath = self._lookup(key)
        path = self._full(subpath)
        with open(path, 'rb') as f:
            msg = (self._factory or MaildirMessage)(f)
        subdir, name = os.path.split(subpath)
        msg.set_subdir(subdir)
        if self.colon in name:
            msg.set_info(name.split(self.colon)[-1])
        msg.set_date(os.path.getmtime(path))
        return msg

    def get_bytes(self, key):
        with open(self._full(self._lookup(key)), 'rb') as f:
            return _to_internal(f.read())

    def get_file(self, key):
        return _ProxyFile(open(self._full(self._lookup(key)), 'rb'))

    def get_info(self, key):
        """The text after the colon in the message's file name."""
        name = self._lookup(key)
        return name.split(self.colon)[-1] if self.colon in name else ''

    def set_info(self, key, info):
        if not isinstance(info, str):
            raise TypeError(f'info must be a string: {type(info)}')
        old_subpath = self._lookup(key)
        new_subpath = old_subpath.split(self.colon)[0]
        if info:
            new_subpath += self.colon + info
        if new_subpath != old_subpath:
            os.rename(self._full(old_subpath), self._full(new_subpath))
            self._toc[key] = new_subpath

    def get_flags(self, key):
        info = self.get_info(key)
        return info[2:] if info.startswith('2,') else ''

    def set_flags(self, key, flags):
        if not isinstance(flags, str):
            raise TypeError(f'flags must be a string: {type(flags)}')
        self.set_info(key, '2,' + ''.join(sorted(set(flags))))

    def add_flag(self, key, flag):
        self.set_flags(key, ''.join(set(self.get_flags(key)) | set(flag)))

    def remove_flag(self, key, flag):
        current = self.get_flags(key)
        if current:
            self.set_flags(key, ''.join(set(current) - set(flag)))

    def iterkeys(self):
        self._refresh()
        for key in list(self._toc):
            try:
                self._lookup(key)
            except KeyError:
                continue
            yield key

    def __contains__(self, key):
        self._refresh()
        return key in self._toc

    def __len__(self):
        self._refresh()
        return len(self._toc)

    # Maildir needs neither locking nor flushing.
    def flush(self):
        pass

    def lock(self):
        pass

    def unlock(self):
        pass

    def close(self):
        pass

    def list_folders(self):
        """Names of the subfolders (".name" directories)."""
        return [entry[1:] for entry in os.listdir(self._path)
                if len(entry) > 1 and entry.startswith('.')
                and os.path.isdir(os.path.join(self._path, entry))]

    def get_folder(self, folder):
        return Maildir(os.path.join(self._path, '.' + folder),
                       factory=self._factory, create=False)

    def add_folder(self, folder):
        path = os.path.join(self._path, '.' + folder)
        result = Maildir(path, factory=self._factory)
        marker = os.path.join(path, 'maildirfolder')
        if not os.path.exists(marker):
            os.close(os.open(marker, os.O_CREAT | os.O_WRONLY, 0o666))
        return result

    def remove_folder(self, folder):
        path = os.path.join(self._path, '.' + folder)
        for sub in ('new', 'cur'):
            for entry in os.listdir(os.path.join(path, sub)):
                if not entry.startswith('.'):
                    raise NotEmptyError('Folder contains message(s): %s'
                                        % folder)
        for entry in os.listdir(path):
            if (entry not in ('new', 'cur', 'tmp')
                    and os.path.isdir(os.path.join(path, entry))):
                raise NotEmptyError("Folder contains subdirectory '%s': %s"
                                    % (folder, entry))
        for root, dirs, files in os.walk(path, topdown=False):
            for name in files:
                os.remove(os.path.join(root, name))
            for name in dirs:
                os.rmdir(os.path.join(root, name))
        os.rmdir(path)

    def clean(self):
        """Delete files in tmp/ not accessed for 36 hours."""
        cutoff = time.time() - 36 * 60 * 60
        tmp = self._paths['tmp']
        for entry in os.listdir(tmp):
            path = os.path.join(tmp, entry)
            if os.path.getatime(path) < cutoff:
                os.remove(path)

    def _create_tmp(self):
        """Create a uniquely named file in tmp/ and open it."""
        now = time.time()
        host = socket.gethostname().replace('/', r'\057').replace(':', r'\072')
        name = '%d.M%dP%dQ%d.%s' % (int(now), int(now % 1 * 1e6),
                                     os.getpid(), Maildir._count, host)
        path = os.path.join(self._paths['tmp'], name)
        if not os.path.exists(path):
            Maildir._count += 1
            try:
                return _open_new_file(path)
            except FileExistsError:
                pass
        # Two deliveries this close together mean another process uses
        # the same name: refuse rather than guess.
        raise ExternalClashError('Name clash prevented file creation: %s'
                                 % path)

    def _refresh(self):
        """Rescan new/ and cur/ when they may have changed."""
        if time.time() - self._last_read > 2 + self._skewfactor:
            changed = False
            for sub in self._toc_mtimes:
                mtime = os.path.getmtime(self._paths[sub])
                if mtime > self._toc_mtimes[sub]:
                    changed = True
                    self._toc_mtimes[sub] = mtime
            if not changed:
                return
        toc = {}
        for sub in self._toc_mtimes:
            folder = self._paths[sub]
            for entry in os.listdir(folder):
                # dot files are not messages (editors, sync tools, ...)
                if (entry.startswith('.')
                        or os.path.isdir(os.path.join(folder, entry))):
                    continue
                toc[entry.split(self.colon)[0]] = os.path.join(sub, entry)
        self._toc = toc
        self._last_read = time.time()

    def _lookup(self, key):
        """The subpath ("new/name") of *key*'s file; rescans if needed."""
        subpath = self._toc.get(key)
        if subpath is not None and os.path.exists(self._full(subpath)):
            return subpath
        self._refresh()
        try:
            return self._toc[key]
        except KeyError:
            raise KeyError('No message with key: %s' % key) from None

    def next(self):
        """The next message in a one-pass iteration (None at the end)."""
        if not hasattr(self, '_onetime_keys'):
            self._onetime_keys = self.iterkeys()
        for key in self._onetime_keys:
            try:
                return self[key]
            except KeyError:
                continue
        return None


class _FlatFileMailbox(Mailbox):
    """Formats that keep every message in one file.

    The table of contents maps keys to (start, stop) byte offsets.  Adding
    appends to the file; removing or replacing only marks the file for a
    rewrite, which flush() performs through a temporary file.
    """

    def __init__(self, path, factory=None, create=True):
        Mailbox.__init__(self, path, factory, create)
        try:
            f = open(self._path, 'rb+')
        except FileNotFoundError:
            if not create:
                raise NoSuchMailboxError(self._path)
            f = open(self._path, 'wb+')
        except OSError as err:
            if err.errno not in (errno.EACCES, errno.EROFS):
                raise
            f = open(self._path, 'rb')      # read-only mailbox
        self._file = f
        self._toc = None            # built on first use
        self._next_key = 0
        self._pending = False       # removals/replacements need a rewrite
        self._pending_sync = False  # appends need an fsync
        self._locked = False
        self._file_length = None    # size when last read or written

    def add(self, message):
        self._lookup()
        self._toc[self._next_key] = self._append_message(message)
        self._next_key += 1
        self._pending_sync = True
        return self._next_key - 1

    def remove(self, key):
        self._lookup(key)
        del self._toc[key]
        self._pending = True

    def __setitem__(self, key, message):
        self._lookup(key)
        self._toc[key] = self._append_message(message)
        self._pending = True

    def iterkeys(self):
        self._lookup()
        yield from list(self._toc)

    def __contains__(self, key):
        self._lookup()
        return key in self._toc

    def __len__(self):
        self._lookup()
        return len(self._toc)

    def lock(self):
        if not self._locked:
            _lock(self._file)
            self._locked = True

    def unlock(self):
        if self._locked:
            _unlock(self._file)
            self._locked = False

    def flush(self):
        """Write changes to disk."""
        if not self._pending:
            if self._pending_sync:
                # only appends: the file itself is already complete
                _fsync(self._file)
                self._pending_sync = False
            return

        self._file.seek(0, 2)
        size = self._file.tell()
        if size != self._file_length:
            raise ExternalClashError('Size of mailbox file changed '
                                     '(expected %i, found %i)'
                                     % (self._file_length, size))

        # Copy the surviving messages into a new file next to the old one,
        # then move it into place, keeping the old file's mode and owner.
        new_file = _open_side_file(self._path)
        try:
            new_toc = self._copy_messages_to(new_file)
            self._file_length = new_file.tell()
        except BaseException:
            new_file.close()
            os.remove(new_file.name)
            raise
        _fsync_close(new_file)
        self._file.close()
        info = os.stat(self._path)
        os.chmod(new_file.name, info.st_mode)
        try:
            os.chown(new_file.name, info.st_uid, info.st_gid)
        except (AttributeError, OSError):
            pass
        try:
            os.rename(new_file.name, self._path)
        except FileExistsError:
            os.remove(self._path)
            os.rename(new_file.name, self._path)
        self._file = open(self._path, 'rb+')
        self._toc = new_toc
        self._pending = False
        self._pending_sync = False
        if self._locked:
            _lock(self._file, dotlock=False)

    def _copy_messages_to(self, target):
        toc = {}
        self._pre_mailbox_hook(target)
        for key in sorted(self._toc):
            start, stop = self._toc[key]
            self._file.seek(start)
            self._pre_message_hook(target)
            new_start = target.tell()
            remaining = stop - start
            while remaining > 0:
                chunk = self._file.read(min(4096, remaining))
                if not chunk:
                    break
                target.write(chunk)
                remaining -= len(chunk)
            toc[key] = (new_start, target.tell())
            self._post_message_hook(target)
        return toc

    # Format hooks around the mailbox and each message.
    def _pre_mailbox_hook(self, f):
        pass

    def _pre_message_hook(self, f):
        pass

    def _post_message_hook(self, f):
        pass

    def __enter__(self):
        self.lock()
        return self

    def close(self):
        """Flush, unlock and close the mailbox file."""
        try:
            self.flush()
        finally:
            try:
                if self._locked:
                    self.unlock()
            finally:
                self._file.close()

    def _lookup(self, key=None):
        """Build the table of contents if needed; return *key*'s offsets."""
        if self._toc is None:
            self._generate_toc()
        if key is None:
            return None
        try:
            return self._toc[key]
        except KeyError:
            raise KeyError('No message with key: %s' % key) from None

    def _append_message(self, message):
        """Append *message* at the end of the file; return its offsets."""
        self._file.seek(0, 2)
        before = self._file.tell()
        if not self._toc and not self._pending:
            # An empty mailbox may still lack its header (Babyl).
            self._pre_mailbox_hook(self._file)
        try:
            self._pre_message_hook(self._file)
            offsets = self._install_message(message)
            self._post_message_hook(self._file)
        except BaseException:
            self._file.truncate(before)
            raise
        self._file.flush()
        self._file_length = self._file.tell()
        return offsets

    def _scan_lines(self):
        """Yield (offset, line) for every line of the file, from the top."""
        self._file.seek(0)
        while True:
            offset = self._file.tell()
            line = self._file.readline()
            yield offset, line
            if not line:
                return

    def _set_toc(self, spans):
        self._toc = dict(enumerate(spans))
        self._next_key = len(self._toc)
        self._file_length = self._file.tell()


class _FromLineMailbox(_FlatFileMailbox):
    """mbox and MMDF: each message starts with a "From " envelope line."""

    _mangle_from_ = True

    def get_message(self, key):
        start, stop = self._lookup(key)
        self._file.seek(start)
        from_line = self._file.readline().replace(linesep, b'')
        body = self._file.read(stop - self._file.tell())
        msg = self._message_factory(_to_internal(body))
        from_line = from_line.decode('ascii')
        msg.set_unixfrom(from_line)
        msg.set_from(from_line[5:])
        return msg

    def get_string(self, key, from_=False):
        return email.message_from_bytes(
            self.get_bytes(key, from_)).as_string(unixfrom=from_)

    def get_bytes(self, key, from_=False):
        start, stop = self._lookup(key)
        self._file.seek(start)
        if not from_:
            self._file.readline()
        return _to_internal(self._file.read(stop - self._file.tell()))

    def get_file(self, key, from_=False):
        start, stop = self._lookup(key)
        self._file.seek(start)
        if not from_:
            self._file.readline()
        return _PartialFile(self._file, self._file.tell(), stop)

    def _envelope_of(self, message):
        """Split off the "From " line: (from line or None, rest)."""
        if isinstance(message, str):
            message = self._string_to_bytes(message)
        if isinstance(message, bytes):
            if message.startswith(b'From '):
                head, newline, rest = message.partition(b'\n')
                return head, rest
            return None, message
        if isinstance(message, _mboxMMDFMessage):
            return b'From ' + message.get_from().encode('ascii'), message
        if isinstance(message, email.message.Message):
            unixfrom = message.get_unixfrom()
            if unixfrom is not None:
                return unixfrom.encode('ascii'), message
        return None, message

    def _install_message(self, message):
        from_line, message = self._envelope_of(message)
        if from_line is None:
            from_line = (b'From MAILER-DAEMON '
                         + time.asctime(time.gmtime()).encode())
        start = self._file.tell()
        self._file.write(from_line + linesep)
        self._dump_message(message, self._file, self._mangle_from_)
        return start, self._file.tell()


class mbox(_FromLineMailbox):
    """The classic Unix mailbox: messages separated by "From " lines."""

    _mangle_from_ = True
    # each message ends with a line ending, then one empty line follows
    _append_newline = True

    def __init__(self, path, factory=None, create=True):
        self._message_factory = mboxMessage
        _FromLineMailbox.__init__(self, path, factory, create)

    def _post_message_hook(self, f):
        f.write(linesep)

    def _generate_toc(self):
        spans = []
        start = None
        blank_before = False
        for offset, line in self._scan_lines():
            if line.startswith(b'From ') or not line:
                if start is not None:
                    # the empty separator line belongs to no message
                    end = offset - len(linesep) if blank_before else offset
                    spans.append((start, end))
                if not line:
                    break
                start = offset
                blank_before = False
            else:
                blank_before = line == linesep
        self._set_toc(spans)


_MMDF_SEP = b'\001\001\001\001'


class MMDF(_FromLineMailbox):
    """Messages framed by lines of four Control-A characters."""

    def __init__(self, path, factory=None, create=True):
        self._message_factory = MMDFMessage
        _FromLineMailbox.__init__(self, path, factory, create)

    def _pre_message_hook(self, f):
        f.write(_MMDF_SEP + linesep)

    def _post_message_hook(self, f):
        f.write(linesep + _MMDF_SEP + linesep)

    def _generate_toc(self):
        spans = []
        frame = _MMDF_SEP + linesep
        lines = self._scan_lines()
        for offset, line in lines:
            if not line:
                break
            if not line.startswith(frame):
                continue
            start = offset + len(line)
            for offset, line in lines:
                if line == frame:
                    spans.append((start, offset - len(linesep)))
                    break
                if not line:
                    spans.append((start, offset))
                    break
            if not line:
                break
        self._set_toc(spans)


class MH(Mailbox):
    """A directory of numbered message files plus a .mh_sequences file."""

    def __init__(self, path, factory=None, create=True):
        Mailbox.__init__(self, path, factory, create)
        if not os.path.exists(self._path):
            if not create:
                raise NoSuchMailboxError(self._path)
            os.mkdir(self._path, 0o700)
            os.close(os.open(self._sequences_path(),
                             os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
        self._locked = False

    def _msg_path(self, key):
        return os.path.join(self._path, str(key))

    def _sequences_path(self):
        return os.path.join(self._path, '.mh_sequences')

    def _open_message(self, key, mode):
        try:
            return open(self._msg_path(key), mode)
        except FileNotFoundError:
            raise KeyError('No message with key: %s' % key) from None

    @contextlib.contextmanager
    def _file_lock(self, f):
        # While the folder is locked, individual message files are locked
        # around each access as well.
        if self._locked:
            _lock(f)
        try:
            yield
        finally:
            if self._locked and not f.closed:
                _unlock(f)

    def add(self, message):
        keys = self.keys()
        key = keys[-1] + 1 if keys else 1
        path = self._msg_path(key)
        f = _open_new_file(path)
        try:
            with self._file_lock(f):
                try:
                    self._dump_message(message, f)
                except BaseException:
                    # unlock and close first: Windows cannot delete it open
                    if self._locked:
                        _unlock(f)
                    _fsync_close(f)
                    os.remove(path)
                    raise
                if isinstance(message, MHMessage):
                    self._dump_sequences(message, key)
        finally:
            if not f.closed:
                _fsync_close(f)
        return key

    def remove(self, key):
        self._open_message(key, 'rb+').close()
        os.remove(self._msg_path(key))

    def __setitem__(self, key, message):
        f = self._open_message(key, 'rb+')
        try:
            with self._file_lock(f):
                os.close(os.open(self._msg_path(key),
                                 os.O_WRONLY | os.O_TRUNC))
                self._dump_message(message, f)
                if isinstance(message, MHMessage):
                    self._dump_sequences(message, key)
        finally:
            _fsync_close(f)

    def get_message(self, key):
        with self._open_message(key, 'rb+' if self._locked else 'rb') as f:
            with self._file_lock(f):
                msg = MHMessage(f)
        for name, members in self.get_sequences().items():
            if key in members:
                msg.add_sequence(name)
        return msg

    def get_bytes(self, key):
        with self._open_message(key, 'rb+' if self._locked else 'rb') as f:
            with self._file_lock(f):
                return _to_internal(f.read())

    def get_file(self, key):
        return _ProxyFile(self._open_message(key, 'rb'))

    def iterkeys(self):
        return iter(sorted(int(name) for name in os.listdir(self._path)
                           if name.isdigit()))

    def __contains__(self, key):
        return os.path.exists(self._msg_path(key))

    def __len__(self):
        return len(list(self.iterkeys()))

    def lock(self):
        """Lock the folder (through its .mh_sequences file)."""
        if not self._locked:
            path = self._sequences_path()
            os.close(os.open(path, os.O_CREAT | os.O_WRONLY, 0o600))
            self._file = open(path, 'rb+')
            _lock(self._file)
            self._locked = True

    def unlock(self):
        if self._locked:
            _unlock(self._file)
            _fsync_close(self._file)
            del self._file
            self._locked = False

    def flush(self):
        pass

    def close(self):
        if self._locked:
            self.unlock()

    def list_folders(self):
        return [name for name in os.listdir(self._path)
                if os.path.isdir(os.path.join(self._path, name))]

    def get_folder(self, folder):
        return MH(os.path.join(self._path, folder),
                  factory=self._factory, create=False)

    def add_folder(self, folder):
        return MH(os.path.join(self._path, folder), factory=self._factory)

    def remove_folder(self, folder):
        path = os.path.join(self._path, folder)
        entries = os.listdir(path)
        if entries == ['.mh_sequences']:
            os.remove(os.path.join(path, '.mh_sequences'))
        elif entries:
            raise NotEmptyError('Folder not empty: %s' % self._path)
        os.rmdir(path)

    def get_sequences(self):
        """A dict mapping sequence names to sorted lists of existing keys."""
        existing = set(self.keys())
        result = {}
        try:
            f = open(self._sequences_path(), 'r', encoding='ASCII')
        except FileNotFoundError:
            return result       # a folder made by another program
        with f:
            for line in f:
                try:
                    name, spec = line.split(':')
                    members = _parse_ranges(spec)
                except ValueError:
                    raise FormatError('Invalid sequence specification: %s'
                                      % line.rstrip())
                present = [key for key in sorted(members) if key in existing]
                if present:
                    result[name] = present
        return result

    def set_sequences(self, sequences):
        """Replace the .mh_sequences file (empty sequences are dropped)."""
        # truncated in place (created if missing): a lock held on the
        # file stays valid
        f = open(self._sequences_path(), 'w', encoding='ASCII')
        try:
            for name, keys in sequences.items():
                if keys:
                    f.write('%s: %s\n' % (name, _format_ranges(keys)))
        finally:
            _fsync_close(f)

    def pack(self):
        """Renumber the messages 1..n, keeping sequences consistent."""
        sequences = self.get_sequences()
        renamed = {}
        for new, old in enumerate(self.iterkeys(), 1):
            if new == old:
                continue
            renamed[old] = new
            src, dst = self._msg_path(old), self._msg_path(new)
            try:
                os.link(src, dst)
            except (AttributeError, PermissionError):
                os.rename(src, dst)
            else:
                os.unlink(src)
        self._next_key = len(self) + 1
        if not renamed:
            return
        for members in sequences.values():
            members[:] = [renamed.get(key, key) for key in members]
        self.set_sequences(sequences)

    def _dump_sequences(self, message, key):
        """Record in .mh_sequences which sequences *key* belongs to."""
        wanted = message.get_sequences()
        sequences = self.get_sequences()
        for name, members in sequences.items():
            if name in wanted:
                members.append(key)
            elif key in members:
                members.remove(key)
        for name in wanted:
            sequences.setdefault(name, [key])
        self.set_sequences(sequences)


def _parse_ranges(spec):
    """Keys named by an MH sequence specification such as "1-3 7"."""
    keys = set()
    for item in spec.split():
        if item.isdigit():
            keys.add(int(item))
        else:
            low, high = (int(part) for part in item.split('-'))
            keys.update(range(low, high + 1))
    return keys


def _format_ranges(keys):
    """The MH notation for *keys*: runs of consecutive numbers as "a-b"."""
    runs = []
    for key in sorted(set(keys)):
        if runs and runs[-1][1] == key - 1:
            runs[-1][1] = key
        else:
            runs.append([key, key])
    return ' '.join(str(a) if a == b else '%d-%d' % (a, b) for a, b in runs)


_BABYL_EOOH = b'*** EOOH ***'


class Babyl(_FlatFileMailbox):
    """The Emacs RMAIL format: messages with labels and a copy of the
    headers as last displayed ("visible" headers)."""

    _special_labels = frozenset({'unseen', 'deleted', 'filed', 'answered',
                                 'forwarded', 'edited', 'resent'})

    def __init__(self, path, factory=None, create=True):
        _FlatFileMailbox.__init__(self, path, factory, create)
        self._labels = {}

    def add(self, message):
        key = _FlatFileMailbox.add(self, message)
        if isinstance(message, BabylMessage):
            self._labels[key] = message.get_labels()
        return key

    def remove(self, key):
        _FlatFileMailbox.remove(self, key)
        self._labels.pop(key, None)

    def __setitem__(self, key, message):
        _FlatFileMailbox.__setitem__(self, key, message)
        if isinstance(message, BabylMessage):
            self._labels[key] = message.get_labels()

    def _read_until(self, stop_lines):
        """Read lines (converted to \\n endings) up to, not including, the
        first one in *stop_lines* or the end of the file."""
        out = []
        for line in iter(self._file.readline, b''):
            if line in stop_lines:
                break
            out.append(_to_internal(line))
        return b''.join(out)

    def _read_parts(self, key):
        """(original headers, visible headers, body) of message *key*."""
        start, stop = self._lookup(key)
        self._file.seek(start)
        self._file.readline()           # the "1,..." labels line
        original = self._read_until((_BABYL_EOOH + linesep,))
        visible = self._read_until((linesep,))
        body = self._file.read(max(stop - self._file.tell(), 0))
        return original, visible, _to_internal(body)

    def get_message(self, key):
        original, visible, body = self._read_parts(key)
        msg = BabylMessage(original + body)
        msg.set_visible(visible)
        if key in self._labels:
            msg.set_labels(self._labels[key])
        return msg

    def get_bytes(self, key):
        original, visible, body = self._read_parts(key)
        return original + body

    def get_file(self, key):
        return io.BytesIO(self.get_bytes(key).replace(b'\n', linesep))

    def get_labels(self):
        """Every user-defined label used in the mailbox."""
        self._lookup()
        labels = set()
        for names in self._labels.values():
            labels.update(names)
        return list(labels - self._special_labels)

    def _generate_toc(self):
        spans = []
        labels = []
        start = None
        for offset, line in self._scan_lines():
            if line == b'\037\014' + linesep or line in (b'\037',
                                                         b'\037' + linesep):
                if start is not None:
                    spans.append((start, offset - len(linesep)))
                    start = None
                if line.endswith(b'\014' + linesep):
                    start = offset + len(line)
                    names = self._file.readline()[1:].split(b',')
                    labels.append([name.strip().decode()
                                   for name in names if name.strip()])
            elif not line:
                if start is not None:
                    spans.append((start, offset - len(linesep)))
                break
        self._toc = dict(enumerate(spans))
        self._labels = dict(enumerate(labels))
        self._next_key = len(self._toc)
        self._file.seek(0, 2)
        self._file_length = self._file.tell()

    def _pre_mailbox_hook(self, f):
        names = b','.join(label.encode() for label in self.get_labels())
        f.write(b'BABYL OPTIONS:' + linesep + b'Version: 5' + linesep
                + b'Labels:' + names + linesep + b'\037')

    def _pre_message_hook(self, f):
        f.write(b'\014' + linesep)

    def _post_message_hook(self, f):
        f.write(linesep + b'\037')

    def _labels_line(self, message):
        if not isinstance(message, BabylMessage):
            return b'1,,'
        labels = message.get_labels()
        special = [l for l in labels if l in self._special_labels]
        user = [l for l in labels if l not in self._special_labels]
        return (b'1' + b''.join(b', ' + l.encode() for l in special) + b',,'
                + b''.join(b' ' + l.encode() + b',' for l in user))

    def _write_header_block(self, data):
        """Write header lines from *data* (a binary file) up to and
        including the empty line that ends them."""
        for line in iter(data.readline, b''):
            self._file.write(line.replace(b'\n', linesep))
            if line == b'\n':
                break

    def _install_message(self, message):
        start = self._file.tell()
        self._file.write(self._labels_line(message) + linesep)

        if isinstance(message, email.message.Message):
            flat = io.BytesIO()
            email.generator.BytesGenerator(flat, False, 0).flatten(message)
            flat.seek(0)
            self._write_header_block(flat)
            self._file.write(_BABYL_EOOH + linesep)
            if isinstance(message, BabylMessage):
                shown = io.BytesIO()
                email.generator.BytesGenerator(shown, False, 0).flatten(
                    message.get_visible())
                shown.seek(0)
                self._write_header_block(shown)
            else:
                body_at = flat.tell()
                flat.seek(0)
                self._write_header_block(flat)
                flat.seek(body_at)
            self._file.write(flat.read().replace(b'\n', linesep))
            return start, self._file.tell()

        if isinstance(message, (bytes, str, io.StringIO)):
            if isinstance(message, io.StringIO):
                warnings.warn("Use of StringIO input is deprecated, "
                              "use BytesIO instead", DeprecationWarning, 3)
                message = message.getvalue()
            if isinstance(message, str):
                message = self._string_to_bytes(message)
            split = message.find(b'\n\n')
            if split == -1:
                self._file.write(_BABYL_EOOH + linesep + linesep)
                self._file.write(message.replace(b'\n', linesep))
            else:
                headers = message[:split + 2].replace(b'\n', linesep)
                self._file.write(headers + _BABYL_EOOH + linesep + headers)
                self._file.write(message[split + 2:].replace(b'\n', linesep))
            return start, self._file.tell()

        if hasattr(message, 'readline'):
            if hasattr(message, 'buffer'):
                warnings.warn("Use of text mode files is deprecated, "
                              "use a binary mode file instead",
                              DeprecationWarning, 3)
                message = message.buffer
            header_at = message.tell()
            headers = []
            for line in iter(message.readline, b''):
                line = _universal_line(line)
                headers.append(line)
                if line == b'\n':
                    break
            block = b''.join(headers).replace(b'\n', linesep)
            self._file.write(block + _BABYL_EOOH + linesep)
            message.seek(header_at)
            # the same headers again as the visible copy, then the body
            for line in iter(message.readline, b''):
                self._file.write(_universal_line(line).replace(b'\n', linesep))
            return start, self._file.tell()

        raise TypeError('Invalid message type: %s' % type(message))


# ---------------------------------------------------------------------------
# Messages

class Message(email.message.Message):
    """An email message that also carries mailbox-format state."""

    def __init__(self, message=None):
        if isinstance(message, email.message.Message):
            self._become_message(copy.deepcopy(message))
            if isinstance(message, Message):
                message._explain_to(self)
        elif isinstance(message, bytes):
            self._become_message(email.message_from_bytes(message))
        elif isinstance(message, str):
            self._become_message(email.message_from_string(message))
        elif isinstance(message, io.TextIOWrapper):
            self._become_message(email.message_from_file(message))
        elif hasattr(message, "read"):
            self._become_message(email.message_from_binary_file(message))
        elif message is None:
            email.message.Message.__init__(self)
        else:
            raise TypeError('Invalid message type: %s' % type(message))

    def _become_message(self, message):
        """Take over *message*'s email state (not its format state)."""
        own = getattr(message, '_type_specific_attributes', ())
        for name, value in message.__dict__.items():
            if name not in own:
                self.__dict__[name] = value

    # Conversion between two formats goes through a neutral summary of the
    # message's status (see _status()); within a format the state is
    # copied as is.  A plain Message has no state to give or take.
    _family = None           # the class whose instances share our state
    _takes_status = False    # whether _absorb() means anything

    def _status(self):
        """Keys: seen, cur (no longer new), deleted, flagged, answered,
        passed (forwarded/resent), date (an mbox envelope date, for a
        Maildir delivery date), delivered (a Maildir delivery date, for an
        mbox envelope)."""
        raise NotImplementedError

    def _absorb(self, status):
        pass

    def _copy_state_to(self, other):
        pass

    def _after_export(self, message):
        pass

    def _explain_to(self, message):
        """Pass this message's format state on to *message*."""
        if not isinstance(message, Message):
            raise TypeError('Cannot convert to specified type: %s'
                            % type(message))
        if self._family is None:
            return
        if isinstance(message, self._family):
            self._copy_state_to(message)
        elif message._takes_status:
            message._absorb(self._status())
            self._after_export(message)


class MaildirMessage(Message):
    """A message with Maildir state: subdirectory, info/flags, date."""

    _type_specific_attributes = ['_subdir', '_info', '_date']

    def __init__(self, message=None):
        self._subdir = 'new'
        self._info = ''
        self._date = time.time()
        Message.__init__(self, message)

    def get_subdir(self):
        return self._subdir

    def set_subdir(self, subdir):
        if subdir not in ('new', 'cur'):
            raise ValueError("subdir must be 'new' or 'cur': %s" % subdir)
        self._subdir = subdir

    def get_flags(self):
        return self._info[2:] if self._info.startswith('2,') else ''

    def set_flags(self, flags):
        self._info = '2,' + ''.join(sorted(flags))

    def add_flag(self, flag):
        self.set_flags(''.join(set(self.get_flags()) | set(flag)))

    def remove_flag(self, flag):
        if self.get_flags():
            self.set_flags(''.join(set(self.get_flags()) - set(flag)))

    def get_date(self):
        return self._date

    def set_date(self, date):
        try:
            self._date = float(date)
        except ValueError:
            raise TypeError("can't convert to float: %s" % date) from None

    def get_info(self):
        return self._info

    def set_info(self, info):
        if not isinstance(info, str):
            raise TypeError('info must be a string: %s' % type(info))
        self._info = info

    def _copy_state_to(self, other):
        other.set_flags(self.get_flags())
        other.set_subdir(self.get_subdir())
        other.set_date(self.get_date())

    def _status(self):
        flags = self.get_flags()
        return dict(seen='S' in flags, cur=self._subdir == 'cur',
                    deleted='T' in flags, flagged='F' in flags,
                    answered='R' in flags, passed='P' in flags,
                    date=None, delivered=self.get_date())

    def _absorb(self, status):
        if status['cur']:
            self.set_subdir('cur')
        wanted = ''.join(flag for flag, key in (('S', 'seen'),
                                                ('T', 'deleted'),
                                                ('F', 'flagged'),
                                                ('R', 'answered'),
                                                ('P', 'passed'))
                         if status[key])
        if wanted:
            self.add_flag(wanted)
        if status['date'] is not None:
            self.set_date(status['date'])


class _mboxMMDFMessage(Message):
    """State shared by mbox and MMDF messages: the envelope sender line
    and the Status/X-Status flag headers."""

    _type_specific_attributes = ['_from']

    def __init__(self, message=None):
        self.set_from('MAILER-DAEMON', True)
        if isinstance(message, email.message.Message):
            unixfrom = message.get_unixfrom()
            if unixfrom is not None and unixfrom.startswith('From '):
                self.set_from(unixfrom[5:])
        Message.__init__(self, message)

    def get_from(self):
        return self._from

    def set_from(self, from_, time_=None):
        """Set the envelope text; *time_* (a struct_time, or True for now)
        appends a date."""
        if time_ is not None:
            if time_ is True:
                time_ = time.gmtime()
            from_ += ' ' + time.asctime(time_)
        self._from = from_

    def get_flags(self):
        return self.get('Status', '') + self.get('X-Status', '')

    def set_flags(self, flags):
        flags = set(flags)
        status = ''.join(f for f in 'RO' if f in flags)
        xstatus = ''.join(f for f in 'DFA' if f in flags)
        xstatus += ''.join(sorted(flags - set('RODFA')))
        for header, value in (('Status', status), ('X-Status', xstatus)):
            try:
                self.replace_header(header, value)
            except KeyError:
                self.add_header(header, value)

    def add_flag(self, flag):
        self.set_flags(''.join(set(self.get_flags()) | set(flag)))

    def remove_flag(self, flag):
        if 'Status' in self or 'X-Status' in self:
            self.set_flags(''.join(set(self.get_flags()) - set(flag)))

    def _copy_state_to(self, other):
        other.set_flags(self.get_flags())
        other.set_from(self.get_from())

    def _envelope_date(self):
        stamp = ' '.join(self.get_from().split()[-5:])
        try:
            return calendar.timegm(time.strptime(stamp,
                                                 '%a %b %d %H:%M:%S %Y'))
        except (ValueError, OverflowError):
            return None

    def _status(self):
        flags = self.get_flags()
        return dict(seen='R' in flags, cur='O' in flags,
                    deleted='D' in flags, flagged='F' in flags,
                    answered='A' in flags, passed=False,
                    date=self._envelope_date(), delivered=None)

    def _after_export(self, message):
        # the flag headers mean nothing in the other formats
        del message['status']
        del message['x-status']

    def _absorb(self, status):
        wanted = ''.join(flag for flag, key in (('R', 'seen'), ('O', 'cur'),
                                                ('D', 'deleted'),
                                                ('F', 'flagged'),
                                                ('A', 'answered'))
                         if status[key])
        if wanted:
            self.add_flag(wanted)
        if status['delivered'] is not None:
            self.set_from('MAILER-DAEMON', time.gmtime(status['delivered']))


class mboxMessage(_mboxMMDFMessage):
    """A message from an mbox mailbox."""


class MHMessage(Message):
    """A message with the list of MH sequences it belongs to."""

    _type_specific_attributes = ['_sequences']

    def __init__(self, message=None):
        self._sequences = []
        Message.__init__(self, message)

    def get_sequences(self):
        return self._sequences[:]

    def set_sequences(self, sequences):
        self._sequences = list(sequences)

    def add_sequence(self, sequence):
        if not isinstance(sequence, str):
            raise TypeError('sequence type must be str: %s' % type(sequence))
        if sequence not in self._sequences:
            self._sequences.append(sequence)

    def remove_sequence(self, sequence):
        if sequence in self._sequences:
            self._sequences.remove(sequence)

    def _copy_state_to(self, other):
        for name in self.get_sequences():
            other.add_sequence(name)

    def _status(self):
        names = self._sequences
        return dict(seen='unseen' not in names, cur=True, deleted=False,
                    flagged='flagged' in names, answered='replied' in names,
                    passed=False, date=None, delivered=None)

    def _absorb(self, status):
        for name, key, value in (('unseen', 'seen', False),
                                 ('replied', 'answered', True),
                                 ('flagged', 'flagged', True)):
            if status[key] == value:
                self.add_sequence(name)


class BabylMessage(Message):
    """A message with Babyl labels and visible headers."""

    _type_specific_attributes = ['_labels', '_visible']

    def __init__(self, message=None):
        self._labels = []
        self._visible = Message()
        Message.__init__(self, message)

    def get_labels(self):
        return self._labels[:]

    def set_labels(self, labels):
        self._labels = list(labels)

    def add_label(self, label):
        if not isinstance(label, str):
            raise TypeError('label must be a string: %s' % type(label))
        if label not in self._labels:
            self._labels.append(label)

    def remove_label(self, label):
        if label in self._labels:
            self._labels.remove(label)

    def get_visible(self):
        return Message(self._visible)

    def set_visible(self, visible):
        self._visible = Message(visible)

    def update_visible(self):
        """Refresh the visible headers from the message's own headers and
        add the usual ones (Date, From, Reply-To, To, CC, Subject)."""
        for header in self._visible.keys():
            if header in self:
                self._visible.replace_header(header, self[header])
            else:
                del self._visible[header]
        for header in ('Date', 'From', 'Reply-To', 'To', 'CC', 'Subject'):
            if header in self and header not in self._visible:
                self._visible[header] = self[header]

    def _copy_state_to(self, other):
        other.set_visible(self.get_visible())
        for label in self.get_labels():
            other.add_label(label)

    def _status(self):
        labels = self._labels
        return dict(seen='unseen' not in labels, cur=True,
                    deleted='deleted' in labels, flagged=False,
                    answered='answered' in labels,
                    passed='forwarded' in labels or 'resent' in labels,
                    date=None, delivered=None)

    def _absorb(self, status):
        for label, key, value in (('unseen', 'seen', False),
                                  ('deleted', 'deleted', True),
                                  ('answered', 'answered', True),
                                  ('forwarded', 'passed', True)):
            if status[key] == value:
                self.add_label(label)


class MMDFMessage(_mboxMMDFMessage):
    """A message from an MMDF mailbox."""


# Each format class shares its state with its own subclasses only.
for _cls in (MaildirMessage, _mboxMMDFMessage, MHMessage, BabylMessage):
    _cls._family = _cls
    _cls._takes_status = True
del _cls




# ---------------------------------------------------------------------------
# File views handed out by get_file()

class _ProxyFile:
    """A read-only view of an open binary file that keeps its own
    position, so several views (and the mailbox) can share the file."""

    def __init__(self, f, pos=None):
        self._file = f
        self._pos = f.tell() if pos is None else pos

    def _limit(self, size):
        """How much a read of *size* (None/negative: all) may return."""
        return -1 if size is None else size

    def _read(self, size, read_method):
        size = self._limit(size)
        if size == 0:
            return b''
        self._file.seek(self._pos)
        data = read_method(size)
        self._pos = self._file.tell()
        return data

    def read(self, size=None):
        return self._read(size, self._file.read)

    def read1(self, size=None):
        return self._read(size, self._file.read1)

    def readline(self, size=None):
        return self._read(size, self._file.readline)

    def readlines(self, sizehint=None):
        lines = []
        for line in self:
            lines.append(line)
            if sizehint is not None:
                sizehint -= len(line)
                if sizehint <= 0:
                    break
        return lines

    def __iter__(self):
        return iter(self.readline, b'')

    def tell(self):
        return self._pos

    def seek(self, offset, whence=0):
        if whence == 1:
            self._file.seek(self._pos)
        self._file.seek(offset, whence)
        self._pos = self._file.tell()

    def close(self):
        if hasattr(self, '_file'):
            try:
                if hasattr(self._file, 'close'):
                    self._file.close()
            finally:
                del self._file

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()

    def readable(self):
        return self._file.readable()

    def writable(self):
        return self._file.writable()

    def seekable(self):
        return self._file.seekable()

    def flush(self):
        return self._file.flush()

    @property
    def closed(self):
        if not hasattr(self, '_file'):
            return True
        return getattr(self._file, 'closed', False)

    __class_getitem__ = classmethod(GenericAlias)


class _PartialFile(_ProxyFile):
    """A view of the byte range [start, stop) of a shared file."""

    def __init__(self, f, start=None, stop=None):
        _ProxyFile.__init__(self, f, start)
        self._start = start
        self._stop = stop

    def _limit(self, size):
        left = max(self._stop - self._pos, 0)
        if size is None or size < 0 or size > left:
            return left
        return size

    def tell(self):
        return self._pos - self._start

    def seek(self, offset, whence=0):
        base = {0: self._start, 1: self._pos, 2: self._stop}[whence]
        self._pos = base
        _ProxyFile.seek(self, offset, 1)

    def close(self):
        # the underlying file belongs to the mailbox: only forget it
        if hasattr(self, '_file'):
            del self._file


# ---------------------------------------------------------------------------
# Locking and file helpers

def _lock(f, dotlock=True):
    """Lock *f* with lockf() (where available) and a "<name>.lock" file."""
    made_dotlock = False
    try:
        if fcntl:
            try:
                fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as err:
                if err.errno in (errno.EAGAIN, errno.EACCES, errno.EROFS):
                    raise ExternalClashError('lockf: lock unavailable: %s'
                                             % f.name)
                raise
        if dotlock:
            lock_name = f.name + '.lock'
            try:
                scratch = _open_side_file(lock_name)
                scratch.close()
            except OSError as err:
                if err.errno in (errno.EACCES, errno.EROFS):
                    return      # no write access: skip dot-locking
                raise
            # link() fails if the lock exists; rename() is the fallback for
            # file systems without hard links
            try:
                try:
                    os.link(scratch.name, lock_name)
                    made_dotlock = True
                except (AttributeError, PermissionError):
                    os.rename(scratch.name, lock_name)
                    made_dotlock = True
                else:
                    os.unlink(scratch.name)
            except FileExistsError:
                os.remove(scratch.name)
                raise ExternalClashError('dot lock unavailable: %s' % f.name)
    except BaseException:
        if fcntl:
            fcntl.lockf(f, fcntl.LOCK_UN)
        if made_dotlock:
            os.remove(f.name + '.lock')
        raise


def _unlock(f):
    if fcntl:
        fcntl.lockf(f, fcntl.LOCK_UN)
    if os.path.exists(f.name + '.lock'):
        os.remove(f.name + '.lock')


def _open_new_file(path):
    """Create *path* (it must not exist) and open it for binary update."""
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o666)
    try:
        return open(path, 'rb+')
    finally:
        os.close(fd)


def _open_side_file(path):
    """Create a file next to *path* with a name unique to this moment,
    host and process."""
    return _open_new_file('%s.%s.%s.%s' % (path, int(time.time()),
                                           socket.gethostname(), os.getpid()))


def _fsync(f):
    f.flush()
    if hasattr(os, 'fsync'):
        os.fsync(f.fileno())


def _fsync_close(f):
    _fsync(f)
    f.close()


class Error(Exception):
    """Base class for this module's errors."""


class NoSuchMailboxError(Error):
    """The mailbox does not exist (and was not to be created)."""


class NotEmptyError(Error):
    """The folder still contains messages."""


class ExternalClashError(Error):
    """Another program got in the way (lock held, name taken, file size
    changed behind our back)."""


class FormatError(Error):
    """A file could not be parsed."""
