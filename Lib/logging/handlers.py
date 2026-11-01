"""Additional handlers for the logging package: rotating and watched
files, sockets, syslog, SMTP, HTTP, the Windows event log, buffering and
queue based handlers.
"""

import copy
import io
import logging
import os
import pickle
import queue
import re
import struct
import threading
import time

try:
    import socket
except ImportError:          # the interpreter may be built without sockets
    socket = None

DEFAULT_TCP_LOGGING_PORT = 9020
DEFAULT_UDP_LOGGING_PORT = 9021
DEFAULT_HTTP_LOGGING_PORT = 9022
DEFAULT_SOAP_LOGGING_PORT = 9023
SYSLOG_UDP_PORT = 514
SYSLOG_TCP_PORT = 514

_MIDNIGHT = 24 * 60 * 60  # seconds in a day


# --------------------------------------------------------------------------
# Rotating files

class BaseRotatingHandler(logging.FileHandler):
    """A FileHandler that may switch to a fresh file before writing.

    Subclasses provide shouldRollover() and doRollover().  The optional
    ``namer`` and ``rotator`` callables customise backup names and how a
    file is turned into a backup.
    """

    namer = None
    rotator = None

    def __init__(self, filename, mode, encoding=None, delay=False, errors=None):
        logging.FileHandler.__init__(self, filename, mode=mode,
                                     encoding=encoding, delay=delay,
                                     errors=errors)
        self.mode = mode
        self.encoding = encoding
        self.errors = errors

    def emit(self, record):
        try:
            if self.shouldRollover(record):
                self.doRollover()
            logging.FileHandler.emit(self, record)
        except Exception:
            self.handleError(record)

    def rotation_filename(self, default_name):
        """Name to use for a backup (namer(default_name) if a namer is set)."""
        if not callable(self.namer):
            return default_name
        return self.namer(default_name)

    def rotate(self, source, dest):
        """Turn *source* into the backup *dest* (rename unless a rotator
        is set)."""
        if callable(self.rotator):
            self.rotator(source, dest)
        elif os.path.exists(source):
            os.rename(source, dest)

    def _is_special_file(self):
        """True for existing non-regular files (devices, pipes...), which
        are never rotated."""
        name = self.baseFilename
        return os.path.exists(name) and not os.path.isfile(name)


class RotatingFileHandler(BaseRotatingHandler):
    """Rolls over when the file would exceed *maxBytes*; keeps up to
    *backupCount* old files as name.1, name.2, ..."""

    def __init__(self, filename, mode='a', maxBytes=0, backupCount=0,
                 encoding=None, delay=False, errors=None):
        if maxBytes > 0:
            mode = 'a'          # truncating would defeat size based rotation
        if "b" not in mode:
            encoding = io.text_encoding(encoding)
        BaseRotatingHandler.__init__(self, filename, mode, encoding=encoding,
                                     delay=delay, errors=errors)
        self.maxBytes = maxBytes
        self.backupCount = backupCount

    def doRollover(self):
        if self.stream:
            self.stream.close()
            self.stream = None
        if self.backupCount > 0:
            base = self.baseFilename
            for index in range(self.backupCount - 1, 0, -1):
                older = self.rotation_filename("%s.%d" % (base, index))
                newer = self.rotation_filename("%s.%d" % (base, index + 1))
                if os.path.exists(older):
                    if os.path.exists(newer):
                        os.remove(newer)
                    os.rename(older, newer)
            first = self.rotation_filename(base + ".1")
            if os.path.exists(first):
                os.remove(first)
            self.rotate(base, first)
        if not self.delay:
            self.stream = self._open()

    def shouldRollover(self, record):
        if self.maxBytes <= 0 or self._is_special_file():
            return False
        if self.stream is None:
            try:
                size = os.path.getsize(self.baseFilename)
            except OSError:
                size = 0
        else:
            size = self.stream.seek(0, 2)
        if size <= 0:
            # A single record never causes an empty file to rotate.
            return False
        text = "%s\n" % self.format(record)
        return size + len(text) >= self.maxBytes


class TimedRotatingFileHandler(BaseRotatingHandler):
    """Rolls over at regular intervals.

    when: 'S', 'M', 'H', 'D' (seconds .. days), 'MIDNIGHT', or 'W0'-'W6'
    (weekday, 0 = Monday).  For MIDNIGHT and weekly rotation *atTime*
    (a datetime.time) chooses the time of day.  Times are local unless
    utc is true.
    """

    _UNITS = {
        'S': (1, "%Y-%m-%d_%H-%M-%S", r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}"),
        'M': (60, "%Y-%m-%d_%H-%M", r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}"),
        'H': (60 * 60, "%Y-%m-%d_%H", r"\d{4}-\d{2}-\d{2}_\d{2}"),
        'D': (_MIDNIGHT, "%Y-%m-%d", r"\d{4}-\d{2}-\d{2}"),
        'MIDNIGHT': (_MIDNIGHT, "%Y-%m-%d", r"\d{4}-\d{2}-\d{2}"),
        'W': (_MIDNIGHT * 7, "%Y-%m-%d", r"\d{4}-\d{2}-\d{2}"),
    }

    def __init__(self, filename, when='h', interval=1, backupCount=0,
                 encoding=None, delay=False, utc=False, atTime=None,
                 errors=None):
        encoding = io.text_encoding(encoding)
        BaseRotatingHandler.__init__(self, filename, 'a', encoding=encoding,
                                     delay=delay, errors=errors)
        self.when = when.upper()
        self.backupCount = backupCount
        self.utc = utc
        self.atTime = atTime

        unit = self.when
        if unit.startswith('W'):
            if len(unit) != 2:
                raise ValueError("You must specify a day for weekly rollover "
                                 "from 0 to 6 (0 is Monday): %s" % self.when)
            if unit[1] not in "0123456":
                raise ValueError("Invalid day specified for weekly rollover: %s"
                                 % self.when)
            self.dayOfWeek = int(unit[1])
            unit = 'W'
        if unit not in self._UNITS:
            raise ValueError("Invalid rollover interval specified: %s" % self.when)
        seconds, self.suffix, stamp_pattern = self._UNITS[unit]
        self.extMatch = re.compile(r"(?<!\d)%s(?!\d)" % stamp_pattern, re.ASCII)
        self.interval = seconds * interval

        if os.path.exists(self.baseFilename):
            start = int(os.stat(self.baseFilename).st_mtime)
        else:
            start = int(time.time())
        self.rolloverAt = self.computeRollover(start)

    # -- schedule

    def _rollover_time_of_day(self):
        """Seconds after midnight at which MIDNIGHT/weekly rotation happens
        (a full day, i.e. the end of the day, when no atTime is given)."""
        if self.atTime is None:
            return _MIDNIGHT
        at = self.atTime
        return (at.hour * 60 + at.minute) * 60 + at.second

    def _days_until_rollover(self, weekday, seconds_into_day):
        target = self._rollover_time_of_day()
        if self.when == 'MIDNIGHT':
            days = 0 if seconds_into_day < target else 1
            return days + self.interval // _MIDNIGHT - 1
        days = (self.dayOfWeek - weekday) % 7
        if days == 0 and seconds_into_day >= target:
            days = 7
        return days + 7 * (self.interval // (_MIDNIGHT * 7) - 1)

    def computeRollover(self, currentTime):
        """The time of the first rollover after *currentTime*."""
        if self.when != 'MIDNIGHT' and not self.when.startswith('W'):
            return currentTime + self.interval
        target = self._rollover_time_of_day()
        if self.utc:
            into_day = currentTime % _MIDNIGHT
            weekday = time.gmtime(currentTime).tm_wday
            days = self._days_until_rollover(weekday, into_day)
            return currentTime - into_day + days * _MIDNIGHT + target
        now = time.localtime(currentTime)
        into_day = (now.tm_hour * 60 + now.tm_min) * 60 + now.tm_sec
        days = self._days_until_rollover(now.tm_wday, into_day)
        return self._local_timestamp(now, days, target)

    @staticmethod
    def _local_timestamp(now, days, seconds):
        """Timestamp of local wall-clock time *seconds* after midnight,
        *days* days after the date of *now* (a struct_time).

        Ambiguous or missing wall-clock times around DST changes are
        resolved by preferring the DST flag in effect at *now*; the other
        flag is used only if it yields exactly the requested wall time.
        """
        hour, rest = divmod(seconds, 3600)
        minute, second = divmod(rest, 60)
        fields = (now.tm_year, now.tm_mon, now.tm_mday + days,
                  hour, minute, second, 0, 0)
        wanted = (hour % 24, minute, second)

        def wall(ts):
            t = time.localtime(ts)
            return (t.tm_hour, t.tm_min, t.tm_sec)

        first = time.mktime(fields + (now.tm_isdst,))
        if wall(first) != wanted and now.tm_isdst in (0, 1):
            other = time.mktime(fields + (1 - now.tm_isdst,))
            if wall(other) == wanted:
                return int(other)
        return int(first)

    def shouldRollover(self, record):
        now = int(time.time())
        if now < self.rolloverAt:
            return False
        if self._is_special_file():
            # Devices and pipes are not rotated; just move the schedule.
            self.rolloverAt = self.computeRollover(now)
            return False
        return True

    # -- backups

    def getFilesToDelete(self):
        """Backups beyond backupCount (oldest first) that should go."""
        directory, base = os.path.split(self.baseFilename)
        found = []
        for name in os.listdir(directory):
            for match in self.extMatch.finditer(name):
                stamp = match.group(0)
                expected = self.rotation_filename(self.baseFilename + "." + stamp)
                if os.path.basename(expected) == name:
                    found.append((stamp, name))
                    break
        if len(found) <= self.backupCount:
            return []
        found.sort()
        stale = found[:len(found) - self.backupCount]
        return [os.path.join(directory, name) for _stamp, name in stale]

    def _period_start_tuple(self):
        start = self.rolloverAt - self.interval
        if self.utc:
            return time.gmtime(start)
        stamp = time.localtime(start)
        current_dst = time.localtime(time.time()).tm_isdst
        if stamp.tm_isdst != current_dst and self.interval >= _MIDNIGHT:
            # Keep the date of the period when a DST switch happened.
            shift = 3600 if current_dst else -3600
            stamp = time.localtime(start + shift)
        return stamp

    def doRollover(self):
        """Close the current file and rename it with the date suffix of
        the period that just ended."""
        now = int(time.time())
        backup = self.rotation_filename(
            self.baseFilename + "." + time.strftime(self.suffix,
                                                    self._period_start_tuple()))
        # Schedule the next rollover before doing anything that can fail,
        # so that a failure is not retried for every record.
        upcoming = self.computeRollover(now)
        while upcoming <= now:
            upcoming += self.interval
        self.rolloverAt = upcoming

        if os.path.exists(backup):
            # This period was already rotated (e.g. by another handler or
            # process): keep appending to the current file.
            return
        if self.stream:
            self.stream.close()
            self.stream = None
        self.rotate(self.baseFilename, backup)
        if self.backupCount > 0:
            for stale in self.getFilesToDelete():
                os.remove(stale)
        if not self.delay:
            self.stream = self._open()


class WatchedFileHandler(logging.FileHandler):
    """Reopens the file when it was moved or deleted (e.g. by logrotate)
    since it was opened.  Not meaningful on Windows."""

    def __init__(self, filename, mode='a', encoding=None, delay=False,
                 errors=None):
        if "b" not in mode:
            encoding = io.text_encoding(encoding)
        logging.FileHandler.__init__(self, filename, mode=mode,
                                     encoding=encoding, delay=delay,
                                     errors=errors)
        self.dev, self.ino = -1, -1
        self._statstream()

    def _statstream(self):
        if self.stream is None:
            return
        info = os.fstat(self.stream.fileno())
        self.dev, self.ino = info.st_dev, info.st_ino

    def reopenIfNeeded(self):
        """Reopen the file if the path no longer refers to our stream."""
        if self.stream is None:
            return
        try:
            info = os.stat(self.baseFilename)
            moved = (info.st_dev, info.st_ino) != (self.dev, self.ino)
        except FileNotFoundError:
            moved = True
        if not moved:
            return
        self.stream.flush()
        self.stream.close()
        self.stream = None
        self.stream = self._open()
        self._statstream()

    def emit(self, record):
        try:
            self.reopenIfNeeded()
        except Exception:
            self.handleError(record)
            return
        opened_now = self.stream is None
        logging.FileHandler.emit(self, record)
        if opened_now and self.stream is not None:
            self._statstream()


# --------------------------------------------------------------------------
# Network handlers

class SocketHandler(logging.Handler):
    """Sends pickled record dictionaries over a TCP (or Unix stream)
    socket, each prefixed with its length as a 4-byte big-endian int.

    Connection failures are retried with exponential back-off.
    """

    def __init__(self, host, port):
        logging.Handler.__init__(self)
        self.host = host
        self.port = port
        self.address = host if port is None else (host, port)
        self.sock = None
        self.closeOnError = False
        self.retryTime = None
        self.retryStart = 1.0
        self.retryMax = 30.0
        self.retryFactor = 2.0

    def makeSocket(self, timeout=1):
        if self.port is not None:
            return socket.create_connection(self.address, timeout=timeout)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        try:
            sock.connect(self.address)
        except OSError:
            sock.close()
            raise
        return sock

    def createSocket(self):
        """Connect unless we are still inside a back-off period."""
        now = time.time()
        if self.retryTime is not None and now < self.retryTime:
            return
        try:
            self.sock = self.makeSocket()
            self.retryTime = None
        except OSError:
            if self.retryTime is None:
                self.retryPeriod = self.retryStart
            else:
                self.retryPeriod = min(self.retryPeriod * self.retryFactor,
                                       self.retryMax)
            self.retryTime = now + self.retryPeriod

    def send(self, s):
        if self.sock is None:
            self.createSocket()
        if self.sock:
            try:
                self.sock.sendall(s)
            except OSError:
                self.sock.close()
                self.sock = None

    def makePickle(self, record):
        """Serialise the record (message pre-merged, no live exc_info)."""
        if record.exc_info:
            self.format(record)       # fills record.exc_text
        data = dict(record.__dict__)
        data['msg'] = record.getMessage()
        data['args'] = None
        data['exc_info'] = None
        data.pop('message', None)
        payload = pickle.dumps(data, 1)
        return struct.pack(">L", len(payload)) + payload

    def handleError(self, record):
        if self.closeOnError and self.sock:
            self.sock.close()
            self.sock = None
        else:
            logging.Handler.handleError(self, record)

    def emit(self, record):
        try:
            self.send(self.makePickle(record))
        except Exception:
            self.handleError(record)

    def close(self):
        with self.lock:
            sock, self.sock = self.sock, None
            if sock:
                sock.close()
            logging.Handler.close(self)


class DatagramHandler(SocketHandler):
    """Like SocketHandler but sends each record as one UDP datagram (or
    Unix datagram when port is None)."""

    def __init__(self, host, port):
        SocketHandler.__init__(self, host, port)
        self.closeOnError = False

    def makeSocket(self):
        family = socket.AF_UNIX if self.port is None else socket.AF_INET
        if self.port is not None and ":" in str(self.host):
            family = socket.AF_INET6
        return socket.socket(family, socket.SOCK_DGRAM)

    def send(self, s):
        if self.sock is None:
            self.createSocket()
        self.sock.sendto(s, self.address)


class SysLogHandler(logging.Handler):
    """Sends records to a syslog daemon over UDP, TCP or a Unix socket."""

    # priorities
    LOG_EMERG = 0
    LOG_ALERT = 1
    LOG_CRIT = 2
    LOG_ERR = 3
    LOG_WARNING = 4
    LOG_NOTICE = 5
    LOG_INFO = 6
    LOG_DEBUG = 7

    # facilities
    LOG_KERN = 0
    LOG_USER = 1
    LOG_MAIL = 2
    LOG_DAEMON = 3
    LOG_AUTH = 4
    LOG_SYSLOG = 5
    LOG_LPR = 6
    LOG_NEWS = 7
    LOG_UUCP = 8
    LOG_CRON = 9
    LOG_AUTHPRIV = 10
    LOG_FTP = 11
    LOG_NTP = 12
    LOG_SECURITY = 13
    LOG_CONSOLE = 14
    LOG_SOLCRON = 15
    LOG_LOCAL0 = 16
    LOG_LOCAL1 = 17
    LOG_LOCAL2 = 18
    LOG_LOCAL3 = 19
    LOG_LOCAL4 = 20
    LOG_LOCAL5 = 21
    LOG_LOCAL6 = 22
    LOG_LOCAL7 = 23

    priority_names = {
        "alert": LOG_ALERT, "crit": LOG_CRIT, "critical": LOG_CRIT,
        "debug": LOG_DEBUG, "emerg": LOG_EMERG, "err": LOG_ERR,
        "error": LOG_ERR, "info": LOG_INFO, "notice": LOG_NOTICE,
        "panic": LOG_EMERG, "warn": LOG_WARNING, "warning": LOG_WARNING,
    }

    facility_names = {
        "auth": LOG_AUTH, "authpriv": LOG_AUTHPRIV, "console": LOG_CONSOLE,
        "cron": LOG_CRON, "daemon": LOG_DAEMON, "ftp": LOG_FTP,
        "kern": LOG_KERN, "lpr": LOG_LPR, "mail": LOG_MAIL,
        "news": LOG_NEWS, "ntp": LOG_NTP, "security": LOG_SECURITY,
        "solaris-cron": LOG_SOLCRON, "syslog": LOG_SYSLOG, "user": LOG_USER,
        "uucp": LOG_UUCP,
        "local0": LOG_LOCAL0, "local1": LOG_LOCAL1, "local2": LOG_LOCAL2,
        "local3": LOG_LOCAL3, "local4": LOG_LOCAL4, "local5": LOG_LOCAL5,
        "local6": LOG_LOCAL6, "local7": LOG_LOCAL7,
    }

    # logging level name -> syslog priority name
    priority_map = {
        "DEBUG": "debug",
        "INFO": "info",
        "WARNING": "warning",
        "ERROR": "error",
        "CRITICAL": "critical",
    }

    # Text put in front of every message (e.g. "myapp: ").
    ident = ''
    # Terminate messages with a NUL byte (expected by most daemons).
    append_nul = True

    def __init__(self, address=('localhost', SYSLOG_UDP_PORT),
                 facility=LOG_USER, socktype=None, timeout=None):
        logging.Handler.__init__(self)
        self.address = address
        self.facility = facility
        self.socktype = socktype
        self.timeout = timeout
        self.socket = None
        self.unixsocket = isinstance(address, str)
        try:
            self.createSocket()
        except socket.gaierror:
            # The host may become resolvable later; retried on emit().
            self.socket = None

    def _connect_unixsocket(self, address):
        kinds = [self.socktype] if self.socktype is not None else [
            socket.SOCK_DGRAM, socket.SOCK_STREAM]
        last_error = None
        for kind in kinds:
            sock = socket.socket(socket.AF_UNIX, kind)
            try:
                if self.timeout is not None:
                    sock.settimeout(self.timeout)
                sock.connect(address)
            except OSError as exc:
                sock.close()
                last_error = exc
                continue
            self.socket = sock
            self.socktype = kind
            return
        raise last_error

    def createSocket(self):
        """(Re)create the socket for the configured address."""
        address = self.address
        if isinstance(address, str):
            self.unixsocket = True
            self._connect_unixsocket(address)
            return
        self.unixsocket = False
        socktype = self.socktype
        if socktype is None:
            socktype = socket.SOCK_DGRAM
        host, port = address
        candidates = socket.getaddrinfo(host, port, 0, socktype)
        if not candidates:
            raise OSError("getaddrinfo returns an empty list")
        last_error = None
        for family, kind, proto, _canon, sockaddr in candidates:
            sock = None
            try:
                sock = socket.socket(family, kind, proto)
                if kind == socket.SOCK_STREAM:
                    if self.timeout is not None:
                        sock.settimeout(self.timeout)
                    sock.connect(sockaddr)
            except OSError as exc:
                last_error = exc
                if sock is not None:
                    sock.close()
                continue
            self.socket = sock
            self.socktype = kind
            return
        raise last_error

    def encodePriority(self, facility, priority):
        """Combine facility and priority (names or numbers) into the
        syslog PRI value."""
        if isinstance(facility, str):
            facility = self.facility_names[facility]
        if isinstance(priority, str):
            priority = self.priority_names[priority]
        return (facility << 3) | priority

    def mapPriority(self, levelName):
        """Syslog priority name for a logging level name."""
        return self.priority_map.get(levelName, "warning")

    def close(self):
        with self.lock:
            sock, self.socket = self.socket, None
            if sock:
                sock.close()
            logging.Handler.close(self)

    def emit(self, record):
        try:
            text = self.format(record)
            if self.ident:
                text = self.ident + text
            if self.append_nul:
                text += '\000'
            prio = '<%d>' % self.encodePriority(self.facility,
                                                self.mapPriority(record.levelname))
            payload = prio.encode('utf-8') + text.encode('utf-8')
            if not self.socket:
                self.createSocket()
            if self.unixsocket:
                try:
                    self.socket.send(payload)
                except OSError:
                    self.socket.close()
                    self._connect_unixsocket(self.address)
                    self.socket.send(payload)
            elif self.socktype == socket.SOCK_DGRAM:
                self.socket.sendto(payload, self.address)
            else:
                self.socket.sendall(payload)
        except Exception:
            self.handleError(record)


class SMTPHandler(logging.Handler):
    """Sends each record as an e-mail."""

    def __init__(self, mailhost, fromaddr, toaddrs, subject,
                 credentials=None, secure=None, timeout=5.0):
        logging.Handler.__init__(self)
        if isinstance(mailhost, (list, tuple)):
            self.mailhost, self.mailport = mailhost
        else:
            self.mailhost, self.mailport = mailhost, None
        if isinstance(credentials, (list, tuple)):
            self.username, self.password = credentials
        else:
            self.username = None
        self.fromaddr = fromaddr
        if isinstance(toaddrs, str):
            toaddrs = [toaddrs]
        self.toaddrs = toaddrs
        self.subject = subject
        self.secure = secure
        self.timeout = timeout

    def getSubject(self, record):
        """Subject line for *record* (override for dynamic subjects)."""
        return self.subject

    def emit(self, record):
        try:
            import smtplib
            import email.utils
            from email.message import EmailMessage

            port = self.mailport or smtplib.SMTP_PORT
            smtp = smtplib.SMTP(self.mailhost, port, timeout=self.timeout)
            message = EmailMessage()
            message['From'] = self.fromaddr
            message['To'] = ','.join(self.toaddrs)
            message['Subject'] = self.getSubject(record)
            message['Date'] = email.utils.localtime()
            message.set_content(self.format(record))
            if self.username:
                if self.secure is not None:
                    smtp.ehlo()
                    smtp.starttls(*self.secure)
                    smtp.ehlo()
                smtp.login(self.username, self.password)
            smtp.send_message(message)
            smtp.quit()
        except Exception:
            self.handleError(record)


class NTEventLogHandler(logging.Handler):
    """Writes records to the Windows NT event log (needs pywin32)."""

    def __init__(self, appname, dllname=None, logtype="Application"):
        logging.Handler.__init__(self)
        self.appname = appname
        self.logtype = logtype
        self._welu = None
        self._winapi = None
        try:
            import win32evtlogutil
            import win32evtlog
            self._welu = win32evtlogutil
            if not dllname:
                dllname = os.path.split(self._welu.__file__)
                dllname = os.path.split(dllname[0])
                dllname = os.path.join(dllname[0], r'win32service.pyd')
            self.dllname = dllname
            self._welu.AddSourceToRegistry(appname, dllname, logtype)
            self.deftype = win32evtlog.EVENTLOG_ERROR_TYPE
            self.typemap = {
                logging.DEBUG: win32evtlog.EVENTLOG_INFORMATION_TYPE,
                logging.INFO: win32evtlog.EVENTLOG_INFORMATION_TYPE,
                logging.WARNING: win32evtlog.EVENTLOG_WARNING_TYPE,
                logging.ERROR: win32evtlog.EVENTLOG_ERROR_TYPE,
                logging.CRITICAL: win32evtlog.EVENTLOG_ERROR_TYPE,
            }
        except ImportError:
            try:
                import _winapi
            except ImportError:
                print("The Python Win32 extensions for NT (service, event "
                      "logging) appear not to be available.")
                self._welu = None
            else:
                self._winapi = _winapi
                self.dllname = dllname
                self.deftype = 1
                self.typemap = {logging.DEBUG: 4, logging.INFO: 4,
                                logging.WARNING: 2, logging.ERROR: 1,
                                logging.CRITICAL: 1}

    def getMessageID(self, record):
        return 1

    def getEventCategory(self, record):
        return 0

    def getEventType(self, record):
        return self.typemap.get(record.levelno, self.deftype)

    def emit(self, record):
        if self._welu is None and self._winapi is None:
            return
        try:
            msg_id = self.getMessageID(record)
            category = self.getEventCategory(record)
            kind = self.getEventType(record)
            text = self.format(record)
            if self._welu is not None:
                self._welu.ReportEvent(self.appname, msg_id, category, kind,
                                       [text])
            else:
                handle = self._winapi.RegisterEventSource(None, self.appname)
                try:
                    self._winapi.ReportEvent(handle, kind, category, msg_id,
                                             [text])
                finally:
                    self._winapi.DeregisterEventSource(handle)
        except Exception:
            self.handleError(record)

    def close(self):
        logging.Handler.close(self)


class HTTPHandler(logging.Handler):
    """Sends records to a web server as GET query or POST form data."""

    def __init__(self, host, url, method="GET", secure=False, credentials=None,
                 context=None):
        logging.Handler.__init__(self)
        method = method.upper()
        if method not in ("GET", "POST"):
            raise ValueError("method must be GET or POST")
        if not secure and context is not None:
            raise ValueError("context parameter only makes sense "
                             "with secure=True")
        self.host = host
        self.url = url
        self.method = method
        self.secure = secure
        self.credentials = credentials
        self.context = context

    def mapLogRecord(self, record):
        """The dict to send (the record's attributes by default)."""
        return record.__dict__

    def getConnection(self, host, secure):
        import http.client
        if secure:
            return http.client.HTTPSConnection(host, context=self.context)
        return http.client.HTTPConnection(host)

    def emit(self, record):
        try:
            import urllib.parse
            host = self.host
            conn = self.getConnection(host, self.secure)
            url = self.url
            data = urllib.parse.urlencode(self.mapLogRecord(record))
            if self.method == "GET":
                url += ('&' if '?' in url else '?') + data
            conn.putrequest(self.method, url)
            if self.method == "POST":
                conn.putheader("Content-type", "application/x-www-form-urlencoded")
                conn.putheader("Content-length", str(len(data)))
            if self.credentials:
                import base64
                token = ('%s:%s' % self.credentials).encode('utf-8')
                token = base64.b64encode(token).strip().decode('ascii')
                conn.putheader('Authorization', 'Basic ' + token)
            conn.endheaders()
            if self.method == "POST":
                conn.send(data.encode('utf-8'))
            conn.getresponse()
        except Exception:
            self.handleError(record)


# --------------------------------------------------------------------------
# Buffering and queues

class BufferingHandler(logging.Handler):
    """Collects records in a list and flushes when shouldFlush() says so.
    The base flush() just discards the buffer."""

    def __init__(self, capacity):
        logging.Handler.__init__(self)
        self.capacity = capacity
        self.buffer = []

    def shouldFlush(self, record):
        return len(self.buffer) >= self.capacity

    def emit(self, record):
        self.buffer.append(record)
        if self.shouldFlush(record):
            self.flush()

    def flush(self):
        with self.lock:
            self.buffer.clear()

    def close(self):
        try:
            self.flush()
        finally:
            logging.Handler.close(self)


class MemoryHandler(BufferingHandler):
    """Buffers records and hands them to *target* when the buffer is full
    or a record of at least *flushLevel* arrives."""

    def __init__(self, capacity, flushLevel=logging.ERROR, target=None,
                 flushOnClose=True):
        BufferingHandler.__init__(self, capacity)
        self.flushLevel = logging._checkLevel(flushLevel)
        self.target = target
        self.flushOnClose = flushOnClose

    def shouldFlush(self, record):
        return (len(self.buffer) >= self.capacity
                or record.levelno >= self.flushLevel)

    def setTarget(self, target):
        with self.lock:
            self.target = target

    def flush(self):
        with self.lock:
            if self.target:
                for record in self.buffer:
                    self.target.handle(record)
                self.buffer.clear()

    def close(self):
        try:
            if self.flushOnClose:
                self.flush()
        finally:
            with self.lock:
                self.target = None
                BufferingHandler.close(self)


class QueueHandler(logging.Handler):
    """Puts records on a queue (e.g. queue.Queue or a multiprocessing
    queue) for a QueueListener to process elsewhere."""

    def __init__(self, queue):
        logging.Handler.__init__(self)
        self.queue = queue
        self.listener = None

    def enqueue(self, record):
        self.queue.put_nowait(record)

    def prepare(self, record):
        """Return a picklable copy of *record* whose message is already
        fully formatted (traceback and stack included)."""
        text = self.format(record)
        record = copy.copy(record)
        record.message = text
        record.msg = text
        record.args = None
        record.exc_info = None
        record.exc_text = None
        record.stack_info = None
        return record

    def emit(self, record):
        try:
            self.enqueue(self.prepare(record))
        except Exception:
            self.handleError(record)


class QueueListener(object):
    """Takes records off a queue in a background thread and passes them
    to handlers."""

    _sentinel = None

    def __init__(self, queue, *handlers, respect_handler_level=False):
        self.queue = queue
        self.handlers = handlers
        self._thread = None
        self.respect_handler_level = respect_handler_level

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc_info):
        self.stop()

    def dequeue(self, block):
        return self.queue.get(block)

    def start(self):
        if self._thread is not None:
            raise RuntimeError("Listener already started")
        self._thread = thread = threading.Thread(target=self._monitor)
        thread.daemon = True
        thread.start()

    def prepare(self, record):
        """Hook to transform records before handling (identity)."""
        return record

    def handle(self, record):
        record = self.prepare(record)
        for handler in self.handlers:
            if self.respect_handler_level and record.levelno < handler.level:
                continue
            handler.handle(record)

    def _monitor(self):
        q = self.queue
        task_done = getattr(q, 'task_done', None)
        while True:
            try:
                record = self.dequeue(True)
            except queue.Empty:
                break
            if record is self._sentinel:
                if task_done is not None:
                    task_done()
                break
            self.handle(record)
            if task_done is not None:
                task_done()

    def enqueue_sentinel(self):
        self.queue.put_nowait(self._sentinel)

    def stop(self):
        """Ask the thread to finish once the queue is drained, and wait."""
        if self._thread is not None:
            self.enqueue_sentinel()
            self._thread.join()
            self._thread = None
