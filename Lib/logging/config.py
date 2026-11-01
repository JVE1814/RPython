"""Configure logging from dictionaries (dictConfig), ini-style files
(fileConfig), or configuration sent over a socket (listen)."""

import errno
import functools
import io
import logging
import logging.handlers
import os
import queue
import re
import struct
import threading
import traceback
import warnings

DEFAULT_LOGGING_CONFIG_PORT = 9030

RESET_ERROR = errno.ECONNRESET

# The thread started by listen(), so stopListening() can stop it.
_listener = None


# --------------------------------------------------------------------------
# Shared helpers

def _resolve(name):
    """Import and return the object at dotted path *name*."""
    parts = name.split('.')
    path = parts.pop(0)
    obj = __import__(path)
    for part in parts:
        path += '.' + part
        try:
            obj = getattr(obj, part)
        except AttributeError:
            __import__(path)
            obj = getattr(obj, part)
    return obj


def _clearExistingHandlers():
    """Close and forget every existing handler."""
    logging._handlers.clear()
    logging.shutdown(logging._handlerList[:])
    del logging._handlerList[:]


def _apply_existing_logger_policy(existing, keep_enabled, disable_existing):
    """Loggers not named by a new configuration are either disabled or
    reset; descendants of configured loggers are reset so that their
    records reach the configured ancestors."""
    loggers = logging.root.manager.loggerDict
    for name in existing:
        node = loggers[name]
        if name in keep_enabled:
            if not isinstance(node, logging.PlaceHolder):
                node.setLevel(logging.NOTSET)
                node.handlers = []
                node.propagate = True
        elif not isinstance(node, logging.PlaceHolder):
            node.disabled = disable_existing


def _descendants(names, candidates):
    """Names in *candidates* below any of *names* in the hierarchy."""
    prefixes = tuple(name + '.' for name in names)
    return {cand for cand in candidates if cand.startswith(prefixes)}


# --------------------------------------------------------------------------
# fileConfig

def fileConfig(fname, defaults=None, disable_existing_loggers=True,
               encoding=None):
    """Read logging configuration from a configparser-format file.

    *fname* may be a file name, a file-like object or a ready
    ConfigParser instance.  Expected sections: [loggers], [handlers],
    [formatters], and one [logger_X] / [handler_X] / [formatter_X] per key.
    """
    import configparser

    if isinstance(fname, str):
        if not os.path.exists(fname):
            raise FileNotFoundError(f"{fname} doesn't exist")
        if not os.path.getsize(fname):
            raise RuntimeError(f'{fname} is an empty file')

    if isinstance(fname, configparser.RawConfigParser):
        parser = fname
    else:
        try:
            parser = configparser.ConfigParser(defaults)
            if hasattr(fname, 'readline'):
                parser.read_file(fname)
            else:
                encoding = io.text_encoding(encoding)
                parser.read(fname, encoding=encoding)
        except configparser.ParsingError as exc:
            raise RuntimeError(f'{fname} is invalid: {exc}')

    formatters = _ini_formatters(parser)
    with logging._lock:
        _clearExistingHandlers()
        handlers = _ini_handlers(parser, formatters)
        _ini_loggers(parser, handlers, disable_existing_loggers)


def _key_list(text):
    return [item.strip() for item in text.split(",") if item.strip()]


def _ini_formatters(parser):
    names = _key_list(parser["formatters"]["keys"]) if parser.has_section("formatters") else []
    result = {}
    for name in names:
        section = parser["formatter_%s" % name]
        fmt = section.get("format", None, raw=True)
        datefmt = section.get("datefmt", None, raw=True)
        style = section.get("style", '%', raw=True)
        class_name = section.get("class", None, raw=True)
        defaults = section.get("defaults", None, raw=True)
        factory = _resolve(class_name) if class_name else logging.Formatter
        kwargs = {}
        if defaults is not None:
            import ast
            kwargs['defaults'] = ast.literal_eval(defaults)
        result[name] = factory(fmt or None, datefmt or None, style, **kwargs)
    return result


def _ini_handlers(parser, formatters):
    names = _key_list(parser["handlers"]["keys"]) if parser.has_section("handlers") else []
    namespace = vars(logging)
    handlers = {}
    pending_targets = []
    for name in names:
        section = parser["handler_%s" % name]
        class_name = section.get("class", None, raw=True)
        if not class_name:
            raise KeyError(f"handler {name!r} has no class")
        # The file format is documented to evaluate 'class', 'args' and
        # 'kwargs' as Python expressions in the logging namespace; config
        # files are trusted input (like code).
        try:
            klass = eval(class_name, namespace)
        except (AttributeError, NameError):
            klass = _resolve(class_name)
        args = eval(section.get("args", '()', raw=True), namespace)
        kwargs = eval(section.get("kwargs", '{}', raw=True), namespace)
        handler = klass(*args, **kwargs)
        handler.name = name
        level = section.get("level", None, raw=True)
        if level:
            handler.setLevel(level)
        fmt_name = section.get("formatter", None, raw=True)
        if fmt_name:
            handler.setFormatter(formatters[fmt_name])
        if issubclass(klass, logging.handlers.MemoryHandler):
            target = section.get("target", None, raw=True)
            if target:
                pending_targets.append((handler, target))
        handlers[name] = handler
    for handler, target in pending_targets:
        handler.setTarget(handlers[target])
    return handlers


def _ini_loggers(parser, handlers, disable_existing):
    names = _key_list(parser["loggers"]["keys"])
    if "root" in names:
        names.remove("root")
    section = parser["logger_root"]
    root = logging.root
    level = section.get("level", None, raw=True)
    if level:
        root.setLevel(level)
    for hdlr in root.handlers[:]:
        root.removeHandler(hdlr)
    for hname in _key_list(section.get("handlers", "", raw=True)):
        root.addHandler(handlers[hname])

    existing = list(root.manager.loggerDict.keys())
    configured = []
    for name in names:
        section = parser["logger_%s" % name]
        qualname = section.get("qualname", None, raw=True)
        propagate = section.getint("propagate", fallback=1, raw=True)
        logger = logging.getLogger(qualname)
        configured.append(qualname)
        level = section.get("level", None, raw=True)
        if level:
            logger.setLevel(level)
        for hdlr in logger.handlers[:]:
            logger.removeHandler(hdlr)
        logger.propagate = propagate
        logger.disabled = False
        for hname in _key_list(section.get("handlers", "", raw=True)):
            logger.addHandler(handlers[hname])

    keep = _descendants(configured, existing)
    leftover = [name for name in existing if name not in configured]
    _apply_existing_logger_policy(leftover, keep, disable_existing)


# --------------------------------------------------------------------------
# Converting containers: values are converted lazily on access, so that
# "ext://" and "cfg://" references are resolved by the configurator.

_IDENTIFIER = re.compile(r'^[a-z_][a-z0-9_]*$', re.I)

def valid_ident(s):
    """Raise ValueError unless *s* is a valid Python identifier."""
    if not _IDENTIFIER.match(s):
        raise ValueError('Not a valid Python identifier: %r' % s)
    return True


class _ConvertingMixin(object):
    """Shared logic of the converting containers."""

    def convert_with_key(self, key, value, replace=True):
        result = self.configurator.convert(value)
        if value is not result:
            if replace:
                self[key] = result
            if type(result) in (ConvertingDict, ConvertingList, ConvertingTuple):
                result.parent = self
                result.key = key
        return result

    def convert(self, value):
        result = self.configurator.convert(value)
        if value is not result:
            if type(result) in (ConvertingDict, ConvertingList, ConvertingTuple):
                result.parent = self
        return result


class ConvertingDict(dict, _ConvertingMixin):
    """dict whose values are converted when read."""

    def __getitem__(self, key):
        return self.convert_with_key(key, dict.__getitem__(self, key))

    def get(self, key, default=None):
        return self.convert_with_key(key, dict.get(self, key, default))

    def pop(self, key, default=None):
        return self.convert_with_key(key, dict.pop(self, key, default),
                                     replace=False)


class ConvertingList(list, _ConvertingMixin):
    """list whose items are converted when read."""

    def __getitem__(self, key):
        return self.convert_with_key(key, list.__getitem__(self, key))

    def pop(self, idx=-1):
        return self.convert(list.pop(self, idx))


class ConvertingTuple(tuple, _ConvertingMixin):
    """tuple whose items are converted when read."""

    def __getitem__(self, key):
        return self.convert_with_key(key, tuple.__getitem__(self, key),
                                     replace=False)


class BaseConfigurator(object):
    """Holds a configuration dict and resolves references inside it.

    String values of the form ``prefix://suffix`` are converted with the
    method named in value_converters: ``ext://pkg.mod.name`` imports an
    object, ``cfg://a.b[c]`` refers to another part of the configuration.
    """

    CONVERT_PATTERN = re.compile(r'^(?P<prefix>[a-z]+)://(?P<suffix>.*)$')

    _CFG_HEAD = re.compile(r'^\s*(\w+)\s*')
    _CFG_DOT = re.compile(r'^\.\s*(\w+)\s*')
    _CFG_INDEX = re.compile(r'^\[([^\[\]]*)\]\s*')
    _DIGITS = re.compile(r'^\d+$')

    value_converters = {
        'ext': 'ext_convert',
        'cfg': 'cfg_convert',
    }

    importer = staticmethod(__import__)

    def __init__(self, config):
        self.config = ConvertingDict(config)
        self.config.configurator = self

    def resolve(self, s):
        """Import the object named by the dotted path *s*."""
        parts = s.split('.')
        path = parts.pop(0)
        try:
            found = self.importer(path)
            for part in parts:
                path += '.' + part
                try:
                    found = getattr(found, part)
                except AttributeError:
                    self.importer(path)
                    found = getattr(found, part)
            return found
        except ImportError as exc:
            raise ValueError('Cannot resolve %r: %s' % (s, exc)) from exc

    def ext_convert(self, value):
        return self.resolve(value)

    def cfg_convert(self, value):
        """Follow a path like ``handlers.h1`` or ``a[0][key].b`` through
        the configuration."""
        rest = value
        match = self._CFG_HEAD.match(rest)
        if match is None:
            raise ValueError("Unable to convert %r" % value)
        current = self.config[match.group(1)]
        rest = rest[match.end():]
        while rest:
            match = self._CFG_DOT.match(rest)
            if match:
                current = current[match.group(1)]
            else:
                match = self._CFG_INDEX.match(rest)
                if match is None:
                    raise ValueError('Unable to convert '
                                     '%r at %r' % (value, rest))
                index = match.group(1)
                if self._DIGITS.match(index):
                    try:
                        current = current[int(index)]
                    except TypeError:
                        current = current[index]
                else:
                    current = current[index]
            rest = rest[match.end():]
        return current

    def convert(self, value):
        """Wrap containers so their contents get converted, and resolve
        prefixed string references."""
        if isinstance(value, dict) and not isinstance(value, ConvertingDict):
            value = ConvertingDict(value)
            value.configurator = self
        elif isinstance(value, list) and not isinstance(value, ConvertingList):
            value = ConvertingList(value)
            value.configurator = self
        elif type(value) is tuple:
            value = ConvertingTuple(value)
            value.configurator = self
        elif isinstance(value, str):
            match = self.CONVERT_PATTERN.match(value)
            if match:
                converter = self.value_converters.get(match.group('prefix'))
                if converter:
                    value = getattr(self, converter)(match.group('suffix'))
        return value

    def configure_custom(self, config):
        """Build an object from a dict with a '()' factory entry; other
        keys are keyword arguments and '.' maps attributes to set."""
        factory = config['()']
        if not callable(factory):
            factory = self.resolve(factory)
        kwargs = {}
        for key in list(config.keys()):
            if key in ('()', '.'):
                continue
            valid_ident(key)
            kwargs[key] = config[key]
        props = config.get('.', None)
        result = factory(**kwargs)
        if props:
            for name, value in props.items():
                setattr(result, name, value)
        return result

    def as_tuple(self, value):
        """Lists (as JSON produces them) become tuples."""
        if isinstance(value, list):
            value = tuple(value)
        return value


class _NotConfiguredYet(Exception):
    """A handler refers to another handler that comes later."""


def _is_queue_like(obj):
    return (callable(getattr(obj, 'put_nowait', None))
            and callable(getattr(obj, 'get', None)))


class DictConfigurator(BaseConfigurator):
    """Applies a dictionary configuration (schema version 1)."""

    def configure(self):
        config = self.config
        if 'version' not in config:
            raise ValueError("dictionary doesn't specify a version")
        if config['version'] != 1:
            raise ValueError("Unsupported version: %s" % config['version'])
        incremental = config.pop('incremental', False)
        with logging._lock:
            if incremental:
                self._configure_incrementally(config)
            else:
                self._configure_from_scratch(config)

    def _configure_incrementally(self, config):
        handlers = config.get('handlers', {})
        for name in handlers:
            if name not in logging._handlers:
                raise ValueError('No handler found with name %r' % name)
            try:
                level = handlers[name].get('level', None)
                if level:
                    logging._handlers[name].setLevel(logging._checkLevel(level))
            except Exception as exc:
                raise ValueError('Unable to configure handler %r' % name) from exc
        loggers = config.get('loggers', {})
        for name in loggers:
            try:
                self.configure_logger(name, loggers[name], True)
            except Exception as exc:
                raise ValueError('Unable to configure logger %r' % name) from exc
        root = config.get('root', None)
        if root:
            try:
                self.configure_root(root, True)
            except Exception as exc:
                raise ValueError('Unable to configure root logger') from exc

    def _configure_from_scratch(self, config):
        disable_existing = config.pop('disable_existing_loggers', True)
        _clearExistingHandlers()

        formatters = config.get('formatters', {})
        for name in formatters:
            try:
                formatters[name] = self.configure_formatter(formatters[name])
            except Exception as exc:
                raise ValueError('Unable to configure formatter %r' % name) from exc

        filters = config.get('filters', {})
        for name in filters:
            try:
                filters[name] = self.configure_filter(filters[name])
            except Exception as exc:
                raise ValueError('Unable to configure filter %r' % name) from exc

        # Handlers may refer to other handlers (MemoryHandler targets,
        # QueueHandler listeners); those are retried after the others.
        handlers = config.get('handlers', {})
        postponed = []
        for name in sorted(handlers):
            try:
                handler = self.configure_handler(handlers[name])
            except _NotConfiguredYet:
                postponed.append(name)
                continue
            except Exception as exc:
                raise ValueError('Unable to configure handler %r' % name) from exc
            handler.name = name
            handlers[name] = handler
        for name in postponed:
            try:
                handler = self.configure_handler(handlers[name])
            except Exception as exc:
                raise ValueError('Unable to configure handler %r' % name) from exc
            handler.name = name
            handlers[name] = handler

        manager = logging.root.manager
        existing = list(manager.loggerDict.keys())
        loggers = config.get('loggers', {})
        configured = list(loggers)
        for name in configured:
            try:
                self.configure_logger(name, loggers[name])
            except Exception as exc:
                raise ValueError('Unable to configure logger %r' % name) from exc
        keep = _descendants(configured, existing)
        leftover = [name for name in existing if name not in configured]
        _apply_existing_logger_policy(leftover, keep, disable_existing)

        root = config.get('root', None)
        if root:
            try:
                self.configure_root(root)
            except Exception as exc:
                raise ValueError('Unable to configure root logger') from exc

    # -- formatters and filters

    def configure_formatter(self, config):
        if '()' in config:
            try:
                return self.configure_custom(config)
            except TypeError as exc:
                if "'format'" not in str(exc):
                    raise
                # Factories conventionally call the template argument
                # 'fmt' (like Formatter itself); retry with that name.
                renamed = ConvertingDict(
                    (('fmt' if key == 'format' else key), value)
                    for key, value in dict.items(config))
                renamed.configurator = self
                return self.configure_custom(renamed)
        fmt = config.get('format', None)
        datefmt = config.get('datefmt', None)
        style = config.get('style', '%')
        class_name = config.get('class', None)
        defaults = config.get('defaults', None)
        if not class_name:
            factory = logging.Formatter
        elif callable(class_name):
            factory = class_name
        else:
            factory = _resolve(class_name)
        kwargs = {}
        if 'validate' in config:
            kwargs['validate'] = config['validate']
        if defaults:
            kwargs['defaults'] = defaults
        return factory(fmt, datefmt, style, **kwargs)

    def configure_filter(self, config):
        if '()' in config:
            return self.configure_custom(config)
        return logging.Filter(config.get('name', ''))

    def add_filters(self, filterer, filters):
        for item in filters:
            try:
                if callable(item) or callable(getattr(item, 'filter', None)):
                    chosen = item
                else:
                    chosen = self.config['filters'][item]
                filterer.addFilter(chosen)
            except Exception as exc:
                raise ValueError('Unable to add filter %r' % item) from exc

    # -- handlers

    def _referenced_handler(self, name):
        handler = self.config['handlers'][name]
        if not isinstance(handler, logging.Handler):
            raise _NotConfiguredYet(name)
        return handler

    def _build_queue(self, spec):
        if spec is None:
            return queue.Queue()
        if isinstance(spec, str):
            made = self.resolve(spec)()
        elif isinstance(spec, dict):
            if '()' not in spec:
                raise TypeError('Invalid queue specifier %r' % spec)
            made = self.configure_custom(spec)
        else:
            made = spec
        if not _is_queue_like(made):
            raise TypeError('Invalid queue specifier %r' % spec)
        return made

    def _listener_factory(self, spec):
        if spec is None:
            return logging.handlers.QueueListener
        if isinstance(spec, str):
            return self.resolve(spec)
        if isinstance(spec, dict):
            if '()' not in spec:
                raise TypeError('Invalid listener specifier %r' % spec)
            return self.configure_custom(spec)
        if callable(spec):
            return spec
        raise TypeError('Invalid listener specifier %r' % spec)

    def configure_handler(self, config):
        """Create a handler from its configuration dict (not modified)."""
        work = ConvertingDict(dict.items(config))
        work.configurator = self

        formatter = work.pop('formatter', None)
        if formatter:
            try:
                formatter = self.config['formatters'][formatter]
            except Exception as exc:
                raise ValueError('Unable to set formatter %r' % formatter) from exc
        level = work.pop('level', None)
        filters = work.pop('filters', None)
        props = work.pop('.', None)

        queue_setup = None
        if '()' in work:
            factory = work.pop('()')
            if not callable(factory):
                factory = self.resolve(factory)
        else:
            class_name = work.pop('class')
            factory = class_name if callable(class_name) else self.resolve(class_name)
            if not isinstance(factory, type):
                pass
            elif issubclass(factory, logging.handlers.MemoryHandler):
                if 'target' in work:
                    target = work['target']
                    try:
                        work['target'] = self._referenced_handler(target)
                    except _NotConfiguredYet:
                        raise
                    except Exception as exc:
                        raise ValueError('Unable to set target handler %r'
                                         % target) from exc
                if 'flushLevel' in work:
                    work['flushLevel'] = logging._checkLevel(work['flushLevel'])
            elif issubclass(factory, logging.handlers.QueueHandler):
                listener_handlers = []
                for hname in work.pop('handlers', None) or ():
                    listener_handlers.append(self._referenced_handler(hname))
                queue_setup = (self._build_queue(work.pop('queue', None)),
                               self._listener_factory(work.pop('listener', None)),
                               listener_handlers,
                               work.pop('respect_handler_level', False))
            elif issubclass(factory, logging.handlers.SMTPHandler) and 'mailhost' in work:
                work['mailhost'] = self.as_tuple(work['mailhost'])
            elif issubclass(factory, logging.handlers.SysLogHandler) and 'address' in work:
                work['address'] = self.as_tuple(work['address'])

        kwargs = {}
        for key in list(work.keys()):
            valid_ident(key)
            kwargs[key] = work[key]

        if queue_setup is not None:
            q, listener_factory, listener_handlers, respect = queue_setup
            result = factory(q, **kwargs)
            result.listener = listener_factory(q, *listener_handlers,
                                               respect_handler_level=respect)
        else:
            try:
                result = factory(**kwargs)
            except TypeError as exc:
                if "'stream'" not in str(exc) or 'stream' not in kwargs:
                    raise
                # Older custom handlers name the argument 'strm'.
                kwargs['strm'] = kwargs.pop('stream')
                result = factory(**kwargs)
                warnings.warn(
                    "Support for custom logging handlers with the 'strm' "
                    "argument is deprecated and scheduled for removal in "
                    "Python 3.16. Define handlers with the 'stream' argument "
                    "instead.", DeprecationWarning, stacklevel=2)

        if formatter:
            result.setFormatter(formatter)
        if level is not None:
            result.setLevel(logging._checkLevel(level))
        if filters:
            self.add_filters(result, filters)
        if props:
            for name, value in props.items():
                setattr(result, name, value)
        return result

    def add_handlers(self, logger, handlers):
        for name in handlers:
            try:
                logger.addHandler(self.config['handlers'][name])
            except Exception as exc:
                raise ValueError('Unable to add handler %r' % name) from exc

    # -- loggers

    def common_logger_config(self, logger, config, incremental=False):
        level = config.get('level', None)
        if level is not None:
            logger.setLevel(logging._checkLevel(level))
        if not incremental:
            for hdlr in logger.handlers[:]:
                logger.removeHandler(hdlr)
            handlers = config.get('handlers', None)
            if handlers:
                self.add_handlers(logger, handlers)
            filters = config.get('filters', None)
            if filters:
                self.add_filters(logger, filters)

    def configure_logger(self, name, config, incremental=False):
        logger = logging.getLogger(name)
        self.common_logger_config(logger, config, incremental)
        logger.disabled = False
        propagate = config.get('propagate', None)
        if propagate is not None:
            logger.propagate = propagate

    def configure_root(self, config, incremental=False):
        self.common_logger_config(logging.getLogger(), config, incremental)


dictConfigClass = DictConfigurator

def dictConfig(config):
    """Configure logging from a dictionary (see the logging docs for the
    schema)."""
    dictConfigClass(config).configure()


# --------------------------------------------------------------------------
# Configuration over a socket

def listen(port=DEFAULT_LOGGING_CONFIG_PORT, verify=None):
    """Return a (not yet started) thread that accepts configurations on
    *port*.  Each one is sent as a 4-byte big-endian length followed by
    the payload: JSON for dictConfig, otherwise fileConfig text.  *verify*
    may check or transform the received bytes (return None to reject).
    """
    from socketserver import ThreadingTCPServer, StreamRequestHandler

    class ConfigStreamHandler(StreamRequestHandler):
        """Reads one configuration and applies it."""

        def handle(self):
            try:
                conn = self.connection
                header = conn.recv(4)
                if len(header) != 4:
                    return
                size = struct.unpack(">L", header)[0]
                payload = conn.recv(size)
                while len(payload) < size:
                    more = conn.recv(size - len(payload))
                    if not more:
                        break
                    payload += more
                if self.server.verify is not None:
                    payload = self.server.verify(payload)
                if payload is not None:
                    _apply_received_config(payload.decode("utf-8"))
                if self.server.ready:
                    self.server.ready.set()
            except OSError as exc:
                if exc.errno != RESET_ERROR:
                    raise

    class ConfigSocketReceiver(ThreadingTCPServer):
        """TCP server polling an abort flag between requests."""

        allow_reuse_address = True
        allow_reuse_port = False

        def __init__(self, host='localhost', port=DEFAULT_LOGGING_CONFIG_PORT,
                     handler=None, ready=None, verify=None):
            ThreadingTCPServer.__init__(self, (host, port), handler)
            with logging._lock:
                self.abort = 0
            self.timeout = 1
            self.ready = ready
            self.verify = verify

        def serve_until_stopped(self):
            import select
            while True:
                readable, _w, _x = select.select([self.socket.fileno()],
                                                 [], [], self.timeout)
                if readable:
                    self.handle_request()
                with logging._lock:
                    if self.abort:
                        break
            self.server_close()

    class Server(threading.Thread):
        """The thread returned by listen()."""

        def __init__(self, rcvr, hdlr, port, verify):
            super().__init__()
            self.rcvr = rcvr
            self.hdlr = hdlr
            self.port = port
            self.verify = verify
            self.ready = threading.Event()

        def run(self):
            try:
                server = self.rcvr(port=self.port, handler=self.hdlr,
                                   ready=self.ready, verify=self.verify)
            except BaseException:
                # Unblock whoever waits for the server to come up.
                self.ready.set()
                raise
            if self.port == 0:
                self.port = server.server_address[1]
            self.ready.set()
            global _listener
            with logging._lock:
                _listener = server
            server.serve_until_stopped()

    return Server(ConfigSocketReceiver, ConfigStreamHandler, port, verify)


def _apply_received_config(text):
    import json
    try:
        settings = json.loads(text)
    except Exception:
        settings = None
    if isinstance(settings, dict):
        try:
            dictConfig(settings)
        except Exception:
            traceback.print_exc()
        return
    try:
        fileConfig(io.StringIO(text))
    except Exception:
        traceback.print_exc()


def stopListening():
    """Stop the thread created by the last listen() call."""
    global _listener
    with logging._lock:
        if _listener:
            _listener.abort = 1
            _listener = None
