"""Flexible event logging for applications and libraries.

Loggers are organised in a dotted-name hierarchy below the root logger.
Records created by a logger are filtered, passed to the logger's handlers
and (when the logger propagates) to the handlers of its ancestors.
Handlers format records with Formatter objects and write them somewhere.

Typical use::

    import logging
    log = logging.getLogger(__name__)
    log.warning("disk %s is %d%% full", name, percent)
"""

import collections.abc
import io
import os
import re
import sys
import threading
import time
import traceback
import warnings
import weakref
from string import Template
from types import GenericAlias

__all__ = ['BASIC_FORMAT', 'BufferingFormatter', 'CRITICAL', 'DEBUG', 'ERROR',
           'FATAL', 'FileHandler', 'Filter', 'Formatter', 'Handler', 'INFO',
           'LogRecord', 'Logger', 'LoggerAdapter', 'NOTSET', 'NullHandler',
           'StreamHandler', 'WARN', 'WARNING', 'addLevelName', 'basicConfig',
           'captureWarnings', 'critical', 'debug', 'disable', 'error',
           'exception', 'fatal', 'getLevelName', 'getLogger', 'getLoggerClass',
           'info', 'log', 'makeLogRecord', 'setLoggerClass', 'shutdown',
           'warn', 'warning', 'getLogRecordFactory', 'setLogRecordFactory',
           'lastResort', 'raiseExceptions', 'getLevelNamesMapping',
           'getHandlerByName', 'getHandlerNames']


_DEPRECATED_METADATA = {"__version__": "0.5.1.2", "__date__": "07 February 2010"}

def __getattr__(name):
    if name in _DEPRECATED_METADATA:
        warnings.warn(f"{name!r} is deprecated and slated for removal in "
                      f"Python 3.20", DeprecationWarning, stacklevel=2)
        return _DEPRECATED_METADATA[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


# --------------------------------------------------------------------------
# Module configuration switches

# Reference point for LogRecord.relativeCreated (nanoseconds).
_startTime = time.time_ns()

# If true, errors raised while emitting are reported on stderr (via
# Handler.handleError); production code may want to switch this off.
raiseExceptions = True

# Which (potentially expensive) bits of context a LogRecord collects.
logThreads = True
logMultiprocessing = True
logProcesses = True
logAsyncioTasks = True


# --------------------------------------------------------------------------
# Levels

CRITICAL = 50
FATAL = CRITICAL
ERROR = 40
WARNING = 30
WARN = WARNING
INFO = 20
DEBUG = 10
NOTSET = 0

_levelToName = {
    CRITICAL: 'CRITICAL',
    ERROR: 'ERROR',
    WARNING: 'WARNING',
    INFO: 'INFO',
    DEBUG: 'DEBUG',
    NOTSET: 'NOTSET',
}
_nameToLevel = {
    'CRITICAL': CRITICAL,
    'FATAL': FATAL,
    'ERROR': ERROR,
    'WARN': WARNING,
    'WARNING': WARNING,
    'INFO': INFO,
    'DEBUG': DEBUG,
    'NOTSET': NOTSET,
}


def getLevelNamesMapping():
    """Return a copy of the name -> level mapping."""
    return dict(_nameToLevel)


def getLevelName(level):
    """Return the name of a numeric level, or the level of a name.

    Unknown numeric levels are rendered as "Level <n>".  (The reverse
    lookup exists for backwards compatibility.)
    """
    name = _levelToName.get(level)
    if name is not None:
        return name
    number = _nameToLevel.get(level)
    if number is not None:
        return number
    return "Level %s" % level


def addLevelName(level, levelName):
    """Associate *levelName* with the numeric *level* (both directions)."""
    with _lock:
        _levelToName[level] = levelName
        _nameToLevel[levelName] = level


def _checkLevel(level):
    """Normalise a level given as int or registered name to an int."""
    if isinstance(level, int):
        return level
    if str(level) == level:
        try:
            return _nameToLevel[level]
        except KeyError:
            raise ValueError("Unknown level: %r" % level) from None
    raise TypeError("Level not an integer or a valid string: %r" % (level,))


# --------------------------------------------------------------------------
# Locking
#
# _lock serialises changes to shared state: the logger hierarchy, the
# handler registry and level names.  It is re-entrant because handlers may
# log while being configured.

_lock = threading.RLock()

def _acquireLock():
    _lock.acquire()

def _releaseLock():
    _lock.release()


# Locks (of handlers) that must be reinitialised in a forked child, since
# a thread of the parent may have held them at fork time.
_at_fork_reinit_lock_weakset = weakref.WeakSet()

def _register_at_fork_reinit_lock(instance):
    with _lock:
        _at_fork_reinit_lock_weakset.add(instance)

def _reinit_locks_in_child():
    for handler in list(_at_fork_reinit_lock_weakset):
        handler._at_fork_reinit()
    _lock._at_fork_reinit()

if hasattr(os, "register_at_fork"):
    os.register_at_fork(before=_lock.acquire,
                        after_in_child=_reinit_locks_in_child,
                        after_in_parent=_lock.release)


# --------------------------------------------------------------------------
# Finding the caller

def currentframe():
    """Return the frame of the caller of this function."""
    return sys._getframe(1)

# Source file of this module, used to skip logging's own frames.
_srcfile = os.path.normcase(addLevelName.__code__.co_filename)


def _is_internal_frame(frame):
    """Frames that belong to logging itself or to import machinery."""
    filename = os.path.normcase(frame.f_code.co_filename)
    return filename == _srcfile or (
        "importlib" in filename and "_bootstrap" in filename)


# --------------------------------------------------------------------------
# LogRecord

class LogRecord(object):
    """Everything known about one logged event.

    Created by Logger.makeRecord() (or makeLogRecord()); the message
    template and its arguments are kept separately and merged by
    getMessage() only when the record is actually formatted.
    """

    def __init__(self, name, level, pathname, lineno, msg, args, exc_info,
                 func=None, sinfo=None, **kwargs):
        stamp = time.time_ns()
        self.name = name
        self.msg = msg
        # A single non-empty mapping argument allows "%(key)s" templates.
        if (args and len(args) == 1
                and isinstance(args[0], collections.abc.Mapping) and args[0]):
            args = args[0]
        self.args = args
        self.levelname = getLevelName(level)
        self.levelno = level
        self.pathname = pathname
        try:
            self.filename = os.path.basename(pathname)
            self.module = os.path.splitext(self.filename)[0]
        except (TypeError, ValueError, AttributeError):
            self.filename = pathname
            self.module = "Unknown module"
        self.exc_info = exc_info
        self.exc_text = None
        self.stack_info = sinfo
        self.lineno = lineno
        self.funcName = func

        self.created = stamp / 1e9
        # Derive the milliseconds from the float actually stored in
        # 'created', so the two never disagree about the second.
        whole_second = int(self.created)
        msecs = (stamp - whole_second * 1_000_000_000) // 1_000_000
        self.msecs = float(min(max(msecs, 0), 999))
        self.relativeCreated = (stamp - _startTime) / 1e6

        if logThreads:
            self.thread = threading.get_ident()
            self.threadName = threading.current_thread().name
        else:
            self.thread = None
            self.threadName = None

        if logMultiprocessing:
            self.processName = 'MainProcess'
            mp = sys.modules.get('multiprocessing')
            if mp is not None:
                try:
                    self.processName = mp.current_process().name
                except Exception:
                    pass
        else:
            self.processName = None

        if logProcesses and hasattr(os, 'getpid'):
            self.process = os.getpid()
        else:
            self.process = None

        self.taskName = None
        if logAsyncioTasks:
            asyncio = sys.modules.get('asyncio')
            if asyncio is not None:
                try:
                    self.taskName = asyncio.current_task().get_name()
                except Exception:
                    pass

    def __repr__(self):
        return '<LogRecord: %s, %s, %s, %s, "%s">' % (
            self.name, self.levelno, self.pathname, self.lineno, self.msg)

    def getMessage(self):
        """The message with its arguments merged in."""
        text = str(self.msg)
        if self.args:
            text = text % self.args
        return text


_logRecordFactory = LogRecord

def setLogRecordFactory(factory):
    """Use *factory* (same signature as LogRecord) to create records."""
    global _logRecordFactory
    _logRecordFactory = factory

def getLogRecordFactory():
    """The callable currently used to create LogRecords."""
    return _logRecordFactory


def makeLogRecord(dict):
    """Create a record whose attributes come from *dict* (for example a
    record's __dict__ received over the network)."""
    record = _logRecordFactory(None, None, "", 0, "", (), None, None)
    record.__dict__.update(dict)
    return record


# --------------------------------------------------------------------------
# Format styles

class PercentStyle(object):
    """printf-style templates: "%(levelname)s: %(message)s"."""

    default_format = '%(message)s'
    asctime_format = '%(asctime)s'
    asctime_search = '%(asctime)'
    validation_pattern = re.compile(
        r'%\(\w+\)[#0+ -]*(\*|\d+)?(\.(\*|\d+))?[diouxefgcrsa%]', re.I)

    def __init__(self, fmt, *, defaults=None):
        self._fmt = fmt or self.default_format
        self._defaults = defaults

    def usesTime(self):
        return self._fmt.find(self.asctime_search) >= 0

    def validate(self):
        """Raise ValueError unless the template contains a usable field."""
        if not self.validation_pattern.search(self._fmt):
            raise ValueError("Invalid format '%s' for '%s' style"
                             % (self._fmt, self.default_format[0]))

    def _values(self, record):
        if self._defaults:
            return {**self._defaults, **record.__dict__}
        return record.__dict__

    def _format(self, record):
        return self._fmt % self._values(record)

    def format(self, record):
        try:
            return self._format(record)
        except KeyError as exc:
            raise ValueError('Formatting field not found in record: %s' % exc)


class StrFormatStyle(PercentStyle):
    """str.format() templates: "{levelname}: {message}"."""

    default_format = '{message}'
    asctime_format = '{asctime}'
    asctime_search = '{asctime'

    _spec_pattern = re.compile(
        r'^(.?[<>=^])?[+ -]?z?#?0?(\d+|{\w+})?[,_]?(\.(\d+|{\w+}))?'
        r'[bcdefgnosxX%]?$', re.I)
    _field_pattern = re.compile(r'^(\d+|\w+)(\.\w+|\[[^]]+\])*$')

    def _format(self, record):
        return self._fmt.format(**self._values(record))

    def validate(self):
        """Check every replacement field of the template."""
        import string
        seen = set()
        try:
            for _literal, field, spec, conversion in string.Formatter().parse(self._fmt):
                if field:
                    if not self._field_pattern.match(field):
                        raise ValueError('invalid field name/expression: %r' % field)
                    seen.add(field)
                if conversion and conversion not in 'rsa':
                    raise ValueError('invalid conversion: %r' % conversion)
                if spec and not self._spec_pattern.match(spec):
                    raise ValueError('bad specifier: %r' % spec)
        except ValueError as exc:
            raise ValueError('invalid format: %s' % exc)
        if not seen:
            raise ValueError('invalid format: no fields')


class StringTemplateStyle(PercentStyle):
    """string.Template templates: "$levelname: ${message}"."""

    default_format = '${message}'
    asctime_format = '${asctime}'
    asctime_search = '${asctime}'

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._tpl = Template(self._fmt)

    def usesTime(self):
        fmt = self._fmt
        return fmt.find('$asctime') >= 0 or fmt.find(self.asctime_search) >= 0

    def validate(self):
        seen = set()
        for match in Template.pattern.finditer(self._fmt):
            groups = match.groupdict()
            if groups['named']:
                seen.add(groups['named'])
            elif groups['braced']:
                seen.add(groups['braced'])
            elif match.group(0) == '$':
                raise ValueError("invalid format: bare '$' not allowed")
        if not seen:
            raise ValueError('invalid format: no fields')

    def _format(self, record):
        return self._tpl.substitute(**self._values(record))


BASIC_FORMAT = "%(levelname)s:%(name)s:%(message)s"

_STYLES = {
    '%': (PercentStyle, BASIC_FORMAT),
    '{': (StrFormatStyle, '{levelname}:{name}:{message}'),
    '$': (StringTemplateStyle, '${levelname}:${name}:${message}'),
}


# --------------------------------------------------------------------------
# Formatters

class Formatter(object):
    """Turns a LogRecord into text.

    fmt is a template in one of three styles ('%', '{' or '$'); the
    record's attributes (plus 'message' and, if used, 'asctime') are
    available to it.  datefmt is a time.strftime() format for asctime.
    """

    converter = time.localtime
    default_time_format = '%Y-%m-%d %H:%M:%S'
    default_msec_format = '%s,%03d'

    def __init__(self, fmt=None, datefmt=None, style='%', validate=True, *,
                 defaults=None):
        if style not in _STYLES:
            raise ValueError('Style must be one of: %s' % ','.join(_STYLES.keys()))
        self._style = _STYLES[style][0](fmt, defaults=defaults)
        if validate:
            self._style.validate()
        self._fmt = self._style._fmt
        self.datefmt = datefmt

    def __repr__(self):
        return '<%s (%s)>' % (self.__class__.__name__, self._fmt)

    def formatTime(self, record, datefmt=None):
        """Render record.created, by default as "2003-07-08 16:49:45,896"."""
        moment = self.converter(record.created)
        if datefmt:
            return time.strftime(datefmt, moment)
        text = time.strftime(self.default_time_format, moment)
        if self.default_msec_format:
            text = self.default_msec_format % (text, record.msecs)
        return text

    def formatException(self, ei):
        """Render an exc_info triple like the interpreter's traceback."""
        out = io.StringIO()
        traceback.print_exception(ei[0], ei[1], ei[2], None, out)
        text = out.getvalue()
        out.close()
        return text[:-1] if text.endswith("\n") else text

    def usesTime(self):
        return self._style.usesTime()

    def formatMessage(self, record):
        return self._style.format(record)

    def formatStack(self, stack_info):
        """Hook for rendering record.stack_info (returned unchanged)."""
        return stack_info

    def format(self, record):
        """Produce the final text: message, then exception and stack."""
        record.message = record.getMessage()
        if self.usesTime():
            record.asctime = self.formatTime(record, self.datefmt)
        text = self.formatMessage(record)
        if record.exc_info and not record.exc_text:
            # Cached so several handlers do not format it again.
            record.exc_text = self.formatException(record.exc_info)
        if record.exc_text:
            if not text.endswith("\n"):
                text += "\n"
            text += record.exc_text
        if record.stack_info:
            if not text.endswith("\n"):
                text += "\n"
            text += self.formatStack(record.stack_info)
        return text


_defaultFormatter = Formatter()


class BufferingFormatter(object):
    """Formats a batch of records: header, each record, footer."""

    def __init__(self, linefmt=None):
        self.linefmt = linefmt if linefmt else _defaultFormatter

    def formatHeader(self, records):
        return ""

    def formatFooter(self, records):
        return ""

    def format(self, records):
        if not records:
            return ""
        body = "".join(self.linefmt.format(record) for record in records)
        return self.formatHeader(records) + body + self.formatFooter(records)


# --------------------------------------------------------------------------
# Filters

class Filter(object):
    """Lets through records from the logger called *name* and its
    descendants ("A.B" passes "A.B" and "A.B.C" but not "A.BB")."""

    def __init__(self, name=''):
        self.name = name
        self.nlen = len(name)

    def __repr__(self):
        return '<%s (%s)>' % (self.__class__.__name__, self.name)

    def filter(self, record):
        """True if the record should be processed."""
        if not self.nlen or self.name == record.name:
            return True
        if not record.name.startswith(self.name):
            return False
        return record.name[self.nlen] == "."


class Filterer(object):
    """Base for objects (loggers, handlers) that hold a list of filters.

    A filter is a Filter-like object with a filter(record) method or a
    plain callable; a false result drops the record, a LogRecord result
    replaces it.
    """

    def __init__(self):
        self.filters = []

    def addFilter(self, filter):
        if filter not in self.filters:
            self.filters.append(filter)

    def removeFilter(self, filter):
        if filter in self.filters:
            self.filters.remove(filter)

    def filter(self, record):
        """Run all filters; return False or the (possibly replaced) record."""
        for item in self.filters:
            check = getattr(item, 'filter', None)
            verdict = check(record) if check is not None else item(record)
            if not verdict:
                return False
            if isinstance(verdict, LogRecord):
                record = verdict
        return record


# --------------------------------------------------------------------------
# Handler registry

# name -> handler, for handlers that have a name.
_handlers = weakref.WeakValueDictionary()
# Weak references to every handler, in creation order; used at shutdown.
_handlerList = []


def _forgetHandlerRef(ref):
    """Weakref callback: drop a dead handler from _handlerList."""
    lock, handler_list = _lock, _handlerList
    if lock and handler_list is not None:
        with lock:
            try:
                handler_list.remove(ref)
            except ValueError:
                pass


def _addHandlerRef(handler):
    with _lock:
        _handlerList.append(weakref.ref(handler, _forgetHandlerRef))


def getHandlerByName(name):
    """The handler registered under *name*, or None."""
    return _handlers.get(name)


def getHandlerNames():
    """Immutable set of all registered handler names."""
    return frozenset(_handlers)


# --------------------------------------------------------------------------
# Handlers

class Handler(Filterer):
    """Base class for handlers: dispatches records to a destination.

    Subclasses implement emit(); handle() applies the handler's filters and
    serialises emit() calls with the handler's lock.
    """

    def __init__(self, level=NOTSET):
        Filterer.__init__(self)
        self._name = None
        self.level = _checkLevel(level)
        self.formatter = None
        self._closed = False
        _addHandlerRef(self)
        self.createLock()

    def get_name(self):
        return self._name

    def set_name(self, name):
        with _lock:
            if self._name in _handlers:
                del _handlers[self._name]
            self._name = name
            if name:
                _handlers[name] = self

    name = property(get_name, set_name)

    def createLock(self):
        self.lock = threading.RLock()
        _register_at_fork_reinit_lock(self)

    def _at_fork_reinit(self):
        self.lock._at_fork_reinit()

    def acquire(self):
        if self.lock:
            self.lock.acquire()

    def release(self):
        if self.lock:
            self.lock.release()

    def setLevel(self, level):
        self.level = _checkLevel(level)

    def format(self, record):
        formatter = self.formatter if self.formatter else _defaultFormatter
        return formatter.format(record)

    def emit(self, record):
        raise NotImplementedError('emit must be implemented '
                                  'by Handler subclasses')

    def handle(self, record):
        """Filter, then emit under the lock.  Returns the filter result."""
        verdict = self.filter(record)
        if isinstance(verdict, LogRecord):
            record = verdict
        if verdict:
            with self.lock:
                self.emit(record)
        return verdict

    def setFormatter(self, fmt):
        self.formatter = fmt

    def flush(self):
        """Make sure output has been written (no-op by default)."""

    def close(self):
        """Release resources; also removes the handler from the registry."""
        with _lock:
            self._closed = True
            if self._name and self._name in _handlers:
                del _handlers[self._name]

    def handleError(self, record):
        """Report an exception raised during emit() on stderr (only when
        raiseExceptions is true), then carry on."""
        if not (raiseExceptions and sys.stderr):
            return
        exc = sys.exception()
        try:
            err = sys.stderr
            err.write('--- Logging error ---\n')
            traceback.print_exception(exc, limit=None, file=err)
            err.write('Call stack:\n')
            frame = exc.__traceback__.tb_frame if exc.__traceback__ else None
            package_dir = os.path.dirname(__file__)
            while frame and os.path.dirname(frame.f_code.co_filename) == package_dir:
                frame = frame.f_back
            if frame:
                traceback.print_stack(frame, file=err)
            else:
                err.write('Logged from file %s, line %s\n'
                          % (record.filename, record.lineno))
            try:
                err.write('Message: %r\nArguments: %s\n'
                          % (record.msg, record.args))
            except RecursionError:
                raise
            except Exception:
                err.write('Unable to print the message and arguments'
                          ' - possible formatting error.\nUse the'
                          ' traceback above to help find the error.\n')
        except OSError:
            pass
        finally:
            del exc

    def __repr__(self):
        return '<%s (%s)>' % (self.__class__.__name__, getLevelName(self.level))


class StreamHandler(Handler):
    """Writes formatted records to a stream (default: sys.stderr)."""

    terminator = '\n'

    def __init__(self, stream=None):
        Handler.__init__(self)
        if stream is None:
            stream = sys.stderr
        self.stream = stream

    def flush(self):
        with self.lock:
            stream = self.stream
            if stream and hasattr(stream, "flush"):
                stream.flush()

    def emit(self, record):
        try:
            text = self.format(record)
            self.stream.write(text + self.terminator)
            self.flush()
        except RecursionError:
            raise
        except Exception:
            self.handleError(record)

    def setStream(self, stream):
        """Switch to *stream*; return the old one (None if unchanged)."""
        if stream is self.stream:
            return None
        previous = self.stream
        with self.lock:
            self.flush()
            self.stream = stream
        return previous

    def __repr__(self):
        name = str(getattr(self.stream, 'name', ''))
        if name:
            name += ' '
        return '<%s %s(%s)>' % (self.__class__.__name__, name,
                                getLevelName(self.level))

    __class_getitem__ = classmethod(GenericAlias)


class FileHandler(StreamHandler):
    """Writes formatted records to a file.

    With delay=True the file is only opened when the first record is
    emitted.
    """

    def __init__(self, filename, mode='a', encoding=None, delay=False,
                 errors=None):
        filename = os.fspath(filename)
        self.baseFilename = os.path.abspath(filename)
        self.mode = mode
        self.encoding = encoding
        if "b" not in mode:
            self.encoding = io.text_encoding(encoding)
        self.errors = errors
        self.delay = delay
        # Kept so the file can still be (re)opened during interpreter
        # shutdown, when builtins may already have been torn down.
        self._builtin_open = open
        if delay:
            Handler.__init__(self)
            self.stream = None
        else:
            StreamHandler.__init__(self, self._open())

    def close(self):
        with self.lock:
            try:
                stream = self.stream
                if stream:
                    try:
                        self.flush()
                    finally:
                        self.stream = None
                        if hasattr(stream, "close"):
                            stream.close()
            finally:
                StreamHandler.close(self)

    def _open(self):
        return self._builtin_open(self.baseFilename, self.mode,
                                  encoding=self.encoding, errors=self.errors)

    def emit(self, record):
        if self.stream is None:
            # Never resurrect a file truncated by mode 'w' after close().
            if self.mode != 'w' or not self._closed:
                try:
                    self.stream = self._open()
                except Exception:
                    self.handleError(record)
                    return
        if self.stream:
            StreamHandler.emit(self, record)

    def __repr__(self):
        return '<%s %s (%s)>' % (self.__class__.__name__, self.baseFilename,
                                 getLevelName(self.level))


class _StderrHandler(StreamHandler):
    """Writes to whatever sys.stderr is at the time of the call."""

    def __init__(self, level=NOTSET):
        Handler.__init__(self, level)

    @property
    def stream(self):
        return sys.stderr


_defaultLastResort = _StderrHandler(WARNING)
# Used when a record finds no handler at all; may be set to None.
lastResort = _defaultLastResort


class NullHandler(Handler):
    """Discards everything; lets libraries avoid "no handler" output."""

    def handle(self, record):
        pass

    def emit(self, record):
        pass

    def createLock(self):
        self.lock = None

    def _at_fork_reinit(self):
        pass


# --------------------------------------------------------------------------
# Logger hierarchy

class PlaceHolder(object):
    """Stands in the hierarchy for a dotted name that has descendants but
    no logger of its own yet."""

    def __init__(self, alogger):
        self.loggerMap = {alogger: None}

    def append(self, alogger):
        if alogger not in self.loggerMap:
            self.loggerMap[alogger] = None


_loggerClass = None

def setLoggerClass(klass):
    """Class used by getLogger() for new loggers (a Logger subclass)."""
    global _loggerClass
    if klass != Logger and not issubclass(klass, Logger):
        raise TypeError("logger not derived from logging.Logger: "
                        + klass.__name__)
    _loggerClass = klass

def getLoggerClass():
    return _loggerClass


class Manager(object):
    """Owns the logger hierarchy (normally there is exactly one)."""

    def __init__(self, rootnode):
        self.root = rootnode
        self.disable = 0
        self.emittedNoHandlerWarning = False
        self.loggerDict = {}
        self.loggerClass = None
        self.logRecordFactory = None

    @property
    def disable(self):
        return self._disable

    @disable.setter
    def disable(self, value):
        self._disable = _checkLevel(value)

    def getLogger(self, name):
        """Return the logger called *name*, creating it (and wiring it into
        the hierarchy) if necessary."""
        if not isinstance(name, str):
            raise TypeError('A logger name must be a string')
        with _lock:
            existing = self.loggerDict.get(name)
            if isinstance(existing, Logger):
                return existing
            logger = (self.loggerClass or _loggerClass)(name)
            logger.manager = self
            self.loggerDict[name] = logger
            if isinstance(existing, PlaceHolder):
                self._adoptChildren(existing, logger)
            self._attachToParent(logger)
            return logger

    def setLoggerClass(self, klass):
        if klass != Logger and not issubclass(klass, Logger):
            raise TypeError("logger not derived from logging.Logger: "
                            + klass.__name__)
        self.loggerClass = klass

    def setLogRecordFactory(self, factory):
        self.logRecordFactory = factory

    def _attachToParent(self, logger):
        """Set logger.parent to the nearest existing ancestor, leaving
        placeholders for the missing intermediate names."""
        name = logger.name
        parent = None
        cut = name.rfind(".")
        while cut > 0 and parent is None:
            prefix = name[:cut]
            node = self.loggerDict.get(prefix)
            if node is None:
                self.loggerDict[prefix] = PlaceHolder(logger)
            elif isinstance(node, Logger):
                parent = node
            else:
                node.append(logger)
            cut = name.rfind(".", 0, cut - 1)
        logger.parent = parent if parent is not None else self.root

    def _adoptChildren(self, placeholder, logger):
        """A logger replaced *placeholder*: re-parent the loggers waiting on
        it unless they already hang below a deeper existing logger."""
        prefix = logger.name
        for child in placeholder.loggerMap:
            if not child.parent.name.startswith(prefix):
                logger.parent = child.parent
                child.parent = logger

    def _clear_cache(self):
        """Forget all cached isEnabledFor() answers."""
        with _lock:
            for node in self.loggerDict.values():
                if isinstance(node, Logger):
                    node._cache.clear()
            self.root._cache.clear()


class Logger(Filterer):
    """A named channel for log events.

    Loggers are obtained with getLogger() rather than instantiated
    directly; names form a hierarchy ("a.b" is a child of "a").
    """

    def __init__(self, name, level=NOTSET):
        Filterer.__init__(self)
        self.name = name
        self.level = _checkLevel(level)
        self.parent = None
        self.propagate = True
        self.handlers = []
        self.disabled = False
        self._cache = {}

    def setLevel(self, level):
        self.level = _checkLevel(level)
        self.manager._clear_cache()

    # -- convenience methods (one per level)

    def debug(self, msg, *args, **kwargs):
        if self.isEnabledFor(DEBUG):
            self._log(DEBUG, msg, args, **kwargs)

    def info(self, msg, *args, **kwargs):
        if self.isEnabledFor(INFO):
            self._log(INFO, msg, args, **kwargs)

    def warning(self, msg, *args, **kwargs):
        if self.isEnabledFor(WARNING):
            self._log(WARNING, msg, args, **kwargs)

    def warn(self, msg, *args, **kwargs):
        warnings.warn("The 'warn' method is deprecated, "
                      "use 'warning' instead", DeprecationWarning, 2)
        self.warning(msg, *args, **kwargs)

    def error(self, msg, *args, **kwargs):
        if self.isEnabledFor(ERROR):
            self._log(ERROR, msg, args, **kwargs)

    def exception(self, msg, *args, exc_info=True, **kwargs):
        """Log at ERROR level including the current exception."""
        self.error(msg, *args, exc_info=exc_info, **kwargs)

    def critical(self, msg, *args, **kwargs):
        if self.isEnabledFor(CRITICAL):
            self._log(CRITICAL, msg, args, **kwargs)

    def fatal(self, msg, *args, **kwargs):
        self.critical(msg, *args, **kwargs)

    def log(self, level, msg, *args, **kwargs):
        """Log at an explicit numeric *level*."""
        if not isinstance(level, int):
            if raiseExceptions:
                raise TypeError("level must be an integer")
            return
        if self.isEnabledFor(level):
            self._log(level, msg, args, **kwargs)

    # -- record creation

    def findCaller(self, stack_info=False, stacklevel=1):
        """Return (filename, lineno, function name, stack text) of the
        code that called into logging, *stacklevel* frames up."""
        frame = sys._getframe(0)
        remaining = max(stacklevel, 1)
        chosen = None
        while frame is not None:
            if not _is_internal_frame(frame):
                chosen = frame
                remaining -= 1
                if remaining == 0:
                    break
            frame = frame.f_back
        if chosen is None:
            return "(unknown file)", 0, "(unknown function)", None
        code = chosen.f_code
        sinfo = None
        if stack_info:
            with io.StringIO() as out:
                out.write("Stack (most recent call last):\n")
                traceback.print_stack(chosen, file=out)
                sinfo = out.getvalue()
            if sinfo.endswith("\n"):
                sinfo = sinfo[:-1]
        return code.co_filename, chosen.f_lineno, code.co_name, sinfo

    def makeRecord(self, name, level, fn, lno, msg, args, exc_info,
                   func=None, extra=None, sinfo=None):
        """Create a LogRecord (via the record factory) and apply *extra*."""
        record = _logRecordFactory(name, level, fn, lno, msg, args, exc_info,
                                   func, sinfo)
        if extra is not None:
            for key in extra:
                if key in ("message", "asctime") or key in record.__dict__:
                    raise KeyError("Attempt to overwrite %r in LogRecord" % key)
                record.__dict__[key] = extra[key]
        return record

    def _log(self, level, msg, args, exc_info=None, extra=None,
             stack_info=False, stacklevel=1):
        if _srcfile:
            try:
                fn, lno, func, sinfo = self.findCaller(stack_info, stacklevel)
            except ValueError:
                fn, lno, func, sinfo = "(unknown file)", 0, "(unknown function)", None
        else:
            fn, lno, func, sinfo = "(unknown file)", 0, "(unknown function)", None
        if exc_info:
            if isinstance(exc_info, BaseException):
                exc_info = (type(exc_info), exc_info, exc_info.__traceback__)
            elif not isinstance(exc_info, tuple):
                exc_info = sys.exc_info()
        record = self.makeRecord(self.name, level, fn, lno, msg, args,
                                 exc_info, func, extra, sinfo)
        self.handle(record)

    def handle(self, record):
        """Filter the record and pass it to the handlers."""
        if self.disabled:
            return
        verdict = self.filter(record)
        if not verdict:
            return
        if isinstance(verdict, LogRecord):
            record = verdict
        self.callHandlers(record)

    # -- handlers

    def addHandler(self, hdlr):
        with _lock:
            if hdlr not in self.handlers:
                self.handlers.append(hdlr)

    def removeHandler(self, hdlr):
        with _lock:
            if hdlr in self.handlers:
                self.handlers.remove(hdlr)

    def hasHandlers(self):
        """True if this logger or an ancestor reachable through
        propagation has any handler."""
        node = self
        while node:
            if node.handlers:
                return True
            if not node.propagate:
                return False
            node = node.parent
        return False

    def callHandlers(self, record):
        """Give the record to every eligible handler up the hierarchy."""
        seen = 0
        node = self
        while node:
            for hdlr in list(node.handlers):
                seen += 1
                if record.levelno >= hdlr.level:
                    hdlr.handle(record)
            node = node.parent if node.propagate else None
        if seen:
            return
        if lastResort:
            if record.levelno >= lastResort.level:
                lastResort.handle(record)
        elif raiseExceptions and not self.manager.emittedNoHandlerWarning:
            sys.stderr.write("No handlers could be found for logger"
                             " \"%s\"\n" % self.name)
            self.manager.emittedNoHandlerWarning = True

    # -- levels

    def getEffectiveLevel(self):
        """The first non-NOTSET level found walking up the hierarchy."""
        node = self
        while node:
            if node.level:
                return node.level
            node = node.parent
        return NOTSET

    def isEnabledFor(self, level):
        if self.disabled:
            return False
        try:
            return self._cache[level]
        except KeyError:
            pass
        with _lock:
            if self.manager.disable >= level:
                enabled = False
            else:
                enabled = level >= self.getEffectiveLevel()
            self._cache[level] = enabled
        return enabled

    # -- hierarchy

    def getChild(self, suffix):
        """getChild('b') on logger 'a' is getLogger('a.b')."""
        if self.root is not self:
            suffix = '.'.join((self.name, suffix))
        return self.manager.getLogger(suffix)

    def getChildren(self):
        """Set of the existing loggers exactly one level below this one."""
        with _lock:
            nodes = list(self.manager.loggerDict.items())
        if self.root is self:
            def is_child(name):
                return '.' not in name
        else:
            prefix = self.name + '.'

            def is_child(name):
                return name.startswith(prefix) and '.' not in name[len(prefix):]
        return {node for name, node in nodes
                if isinstance(node, Logger) and is_child(name)}

    def __repr__(self):
        level = getLevelName(self.getEffectiveLevel())
        return '<%s %s (%s)>' % (self.__class__.__name__, self.name, level)

    def __reduce__(self):
        if getLogger(self.name) is not self:
            import pickle
            raise pickle.PicklingError('logger cannot be pickled')
        return getLogger, (self.name,)


class RootLogger(Logger):
    """The top of the hierarchy; its name is "root"."""

    def __init__(self, level):
        Logger.__init__(self, "root", level)

    def __reduce__(self):
        return getLogger, ()


_loggerClass = Logger


class LoggerAdapter(object):
    """Wraps a logger to add contextual information to every call.

    Subclasses override process() to change the message or keyword
    arguments; by default 'extra' is set to the adapter's own dict (or
    merged with the call's 'extra' when merge_extra is true).
    """

    def __init__(self, logger, extra=None, merge_extra=False):
        self.logger = logger
        self.extra = extra
        self.merge_extra = merge_extra

    def process(self, msg, kwargs):
        if self.merge_extra and kwargs.get("extra") is not None:
            combined = dict(self.extra) if self.extra else {}
            combined.update(kwargs["extra"])
            kwargs["extra"] = combined
        else:
            kwargs["extra"] = self.extra
        return msg, kwargs

    def debug(self, msg, *args, **kwargs):
        self.log(DEBUG, msg, *args, **kwargs)

    def info(self, msg, *args, **kwargs):
        self.log(INFO, msg, *args, **kwargs)

    def warning(self, msg, *args, **kwargs):
        self.log(WARNING, msg, *args, **kwargs)

    def warn(self, msg, *args, **kwargs):
        warnings.warn("The 'warn' method is deprecated, "
                      "use 'warning' instead", DeprecationWarning, 2)
        self.warning(msg, *args, **kwargs)

    def error(self, msg, *args, **kwargs):
        self.log(ERROR, msg, *args, **kwargs)

    def exception(self, msg, *args, exc_info=True, **kwargs):
        self.log(ERROR, msg, *args, exc_info=exc_info, **kwargs)

    def critical(self, msg, *args, **kwargs):
        self.log(CRITICAL, msg, *args, **kwargs)

    def log(self, level, msg, *args, **kwargs):
        if self.isEnabledFor(level):
            msg, kwargs = self.process(msg, kwargs)
            self.logger.log(level, msg, *args, **kwargs)

    def isEnabledFor(self, level):
        return self.logger.isEnabledFor(level)

    def setLevel(self, level):
        self.logger.setLevel(level)

    def getEffectiveLevel(self):
        return self.logger.getEffectiveLevel()

    def hasHandlers(self):
        return self.logger.hasHandlers()

    def _log(self, level, msg, args, **kwargs):
        return self.logger._log(level, msg, args, **kwargs)

    @property
    def manager(self):
        return self.logger.manager

    @manager.setter
    def manager(self, value):
        self.logger.manager = value

    @property
    def name(self):
        return self.logger.name

    def __repr__(self):
        level = getLevelName(self.logger.getEffectiveLevel())
        return '<%s %s (%s)>' % (self.__class__.__name__, self.logger.name, level)

    __class_getitem__ = classmethod(GenericAlias)


root = RootLogger(WARNING)
Logger.root = root
Logger.manager = Manager(Logger.root)


# --------------------------------------------------------------------------
# Module-level convenience API

def basicConfig(**kwargs):
    """One-shot configuration of the root logger.

    Does nothing if the root logger already has handlers, unless force=True
    is given.  Keyword arguments: filename, filemode, format, datefmt,
    style, level, stream, handlers, force, encoding, errors, formatter.
    """
    with _lock:
        force = kwargs.pop('force', False)
        encoding = kwargs.pop('encoding', None)
        errors = kwargs.pop('errors', 'backslashreplace')
        if force:
            for hdlr in root.handlers[:]:
                root.removeHandler(hdlr)
                hdlr.close()
        if root.handlers:
            return

        handlers = kwargs.pop("handlers", None)
        if handlers is None:
            if "stream" in kwargs and "filename" in kwargs:
                raise ValueError("'stream' and 'filename' should not be "
                                 "specified together")
        elif "stream" in kwargs or "filename" in kwargs:
            raise ValueError("'stream' or 'filename' should not be "
                             "specified together with 'handlers'")
        if handlers is None:
            filename = kwargs.pop("filename", None)
            mode = kwargs.pop("filemode", 'a')
            if filename:
                if 'b' in mode:
                    errors = None
                else:
                    encoding = io.text_encoding(encoding)
                hdlr = FileHandler(filename, mode, encoding=encoding,
                                   errors=errors)
            else:
                hdlr = StreamHandler(kwargs.pop("stream", None))
            handlers = [hdlr]

        formatter = kwargs.pop("formatter", None)
        if formatter is None:
            datefmt = kwargs.pop("datefmt", None)
            style = kwargs.pop("style", '%')
            if style not in _STYLES:
                raise ValueError('Style must be one of: %s'
                                 % ','.join(_STYLES.keys()))
            fmt_text = kwargs.pop("format", _STYLES[style][1])
            formatter = Formatter(fmt_text, datefmt, style)
        else:
            for clash in ("format", "datefmt", "style"):
                if clash in kwargs:
                    raise ValueError("'%s' should not be specified together "
                                     "with 'formatter'" % clash)
        for hdlr in handlers:
            if hdlr.formatter is None:
                hdlr.setFormatter(formatter)
            root.addHandler(hdlr)

        level = kwargs.pop("level", None)
        if level is not None:
            root.setLevel(level)
        if kwargs:
            raise ValueError('Unrecognised argument(s): %s'
                             % ', '.join(kwargs.keys()))


def getLogger(name=None):
    """The logger called *name*; the root logger for None, '' or 'root'."""
    if not name or (isinstance(name, str) and name == root.name):
        return root
    return Logger.manager.getLogger(name)


def _ensure_root_configured():
    if not root.handlers:
        basicConfig()


def critical(msg, *args, **kwargs):
    """Log at CRITICAL on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.critical(msg, *args, **kwargs)

def fatal(msg, *args, **kwargs):
    critical(msg, *args, **kwargs)

def error(msg, *args, **kwargs):
    """Log at ERROR on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.error(msg, *args, **kwargs)

def exception(msg, *args, exc_info=True, **kwargs):
    """Log at ERROR on the root logger, including the current exception."""
    error(msg, *args, exc_info=exc_info, **kwargs)

def warning(msg, *args, **kwargs):
    """Log at WARNING on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.warning(msg, *args, **kwargs)

def warn(msg, *args, **kwargs):
    warnings.warn("The 'warn' function is deprecated, "
                  "use 'warning' instead", DeprecationWarning, 2)
    warning(msg, *args, **kwargs)

def info(msg, *args, **kwargs):
    """Log at INFO on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.info(msg, *args, **kwargs)

def debug(msg, *args, **kwargs):
    """Log at DEBUG on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.debug(msg, *args, **kwargs)

def log(level, msg, *args, **kwargs):
    """Log at *level* on the root logger (configuring it if needed)."""
    _ensure_root_configured()
    root.log(level, msg, *args, **kwargs)


def disable(level=CRITICAL):
    """Suppress all records at *level* and below, for every logger."""
    root.manager.disable = level
    root.manager._clear_cache()


def shutdown(handlerList=_handlerList):
    """Flush and close all handlers (registered to run at exit)."""
    for ref in reversed(handlerList[:]):
        try:
            hdlr = ref()
            if hdlr:
                try:
                    hdlr.acquire()
                    if getattr(hdlr, 'flushOnClose', True):
                        hdlr.flush()
                    hdlr.close()
                except (OSError, ValueError):
                    # Streams may already be gone at interpreter exit.
                    pass
                finally:
                    hdlr.release()
        except:
            if raiseExceptions:
                raise

import atexit
atexit.register(shutdown)
del atexit


# --------------------------------------------------------------------------
# Routing warnings through logging

_warnings_showwarning = None

def _showwarning(message, category, filename, lineno, file=None, line=None):
    """warnings.showwarning replacement used by captureWarnings(True)."""
    if file is not None:
        if _warnings_showwarning is not None:
            _warnings_showwarning(message, category, filename, lineno, file, line)
        return
    text = warnings.formatwarning(message, category, filename, lineno, line)
    logger = getLogger("py.warnings")
    if not logger.handlers:
        logger.addHandler(NullHandler())
    logger.warning("%s", text)


def captureWarnings(capture):
    """Send warnings to the 'py.warnings' logger (True) or restore the
    previous behaviour (False)."""
    global _warnings_showwarning
    if capture:
        if _warnings_showwarning is None:
            _warnings_showwarning = warnings.showwarning
            warnings.showwarning = _showwarning
    elif _warnings_showwarning is not None:
        warnings.showwarning = _warnings_showwarning
        _warnings_showwarning = None
