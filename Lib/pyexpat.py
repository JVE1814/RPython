"""Python wrapper for the Expat-compatible XML parser.

The parsing itself is done by the Rust engine in _expat_engine; this module
implements the xmlparser API on top of its event stream.
"""

import sys as _sys
import types as _types

from _expat_engine import (new_engine as _new_engine,
                           ERROR_NAMES as _ERROR_NAMES,
                           ERROR_MESSAGES as _ERROR_MESSAGES)

EXPAT_VERSION = 'expat_2.7.3'
version_info = (2, 7, 3)
native_encoding = 'UTF-8'
features = [
    ('sizeof(XML_Char)', 1),
    ('sizeof(XML_LChar)', 1),
    ('XML_DTD', 0),
    ('XML_CONTEXT_BYTES', 1024),
    ('XML_NS', 0),
    ('XML_GE', 0),
]

XML_PARAM_ENTITY_PARSING_NEVER = 0
XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE = 1
XML_PARAM_ENTITY_PARSING_ALWAYS = 2


class ExpatError(Exception):
    pass


error = ExpatError

# -- the errors and model submodules -----------------------------------------

errors = _types.ModuleType('pyexpat.errors')
errors.__doc__ = "Constants used to describe error conditions."
errors.codes = {}
errors.messages = {}
for _code in range(1, len(_ERROR_NAMES)):
    setattr(errors, _ERROR_NAMES[_code], _ERROR_MESSAGES[_code])
    errors.codes[_ERROR_MESSAGES[_code]] = _code
    errors.messages[_code] = _ERROR_MESSAGES[_code]
del _code

model = _types.ModuleType('pyexpat.model')
model.__doc__ = "Constants used to interpret content model information."
model.XML_CTYPE_EMPTY = 1
model.XML_CTYPE_ANY = 2
model.XML_CTYPE_MIXED = 3
model.XML_CTYPE_NAME = 4
model.XML_CTYPE_CHOICE = 5
model.XML_CTYPE_SEQ = 6
model.XML_CQUANT_NONE = 0
model.XML_CQUANT_OPT = 1
model.XML_CQUANT_REP = 2
model.XML_CQUANT_PLUS = 3

_sys.modules['pyexpat.errors'] = errors
_sys.modules['pyexpat.model'] = model


def ErrorString(code):
    """Returns string error for given number."""
    return errors.messages.get(code)


# -- the parser ---------------------------------------------------------------

_HANDLER_NAMES = (
    'StartElementHandler', 'EndElementHandler', 'ProcessingInstructionHandler',
    'CharacterDataHandler', 'UnparsedEntityDeclHandler',
    'NotationDeclHandler', 'StartNamespaceDeclHandler',
    'EndNamespaceDeclHandler', 'CommentHandler', 'StartCdataSectionHandler',
    'EndCdataSectionHandler', 'DefaultHandler', 'DefaultHandlerExpand',
    'NotStandaloneHandler', 'ExternalEntityRefHandler',
    'StartDoctypeDeclHandler', 'EndDoctypeDeclHandler', 'EntityDeclHandler',
    'XmlDeclHandler', 'ElementDeclHandler', 'AttlistDeclHandler',
    'SkippedEntityHandler',
)

_FLAG_NAMES = ('buffer_text', 'namespace_prefixes', 'ordered_attributes',
               'specified_attributes')

_INTERNAL = (
    '_engine', '_ns_sep', '_encoding', '_buffer', '_buffer_used',
    '_buffer_size', '_in_callback', '_parent', '_is_root', '_base',
    '_line', '_col', '_byte', '_err', '_param_entity_parsing',
    '_foreign_dtd', '_reparse_deferral', '_ampl', '_flags',
    'intern', '__weakref__',
)

_CREATE_TOKEN = object()

# Single byte encodings are only supported if they map these bytes to the
# same ASCII characters.
_DATA_CHUNK = 1024


def _invalid_return(kind, value):
    exc = TypeError("invalid return value for handler: {!r}".format(value))
    exc.add_note("invalid '{}' event handler return value".format(kind))
    return exc


class xmlparser:
    """XML parser"""

    __slots__ = _INTERNAL + _HANDLER_NAMES

    def __new__(cls, *args, **kw):
        if not args or args[0] is not _CREATE_TOKEN:
            raise TypeError("cannot create 'pyexpat.xmlparser' instances")
        return object.__new__(cls)

    def __init__(self, token, encoding, namespace_separator, intern,
                 engine=None, parent=None):
        set_ = object.__setattr__
        set_(self, '_engine', engine if engine is not None
             else _new_engine(namespace_separator, encoding))
        set_(self, '_ns_sep', namespace_separator)
        set_(self, '_encoding', encoding)
        set_(self, '_buffer', None)
        set_(self, '_buffer_used', 0)
        set_(self, '_buffer_size', 8192)
        set_(self, '_in_callback', False)
        set_(self, '_parent', parent)
        set_(self, '_is_root', parent is None)
        set_(self, '_base', None)
        set_(self, '_line', 1)
        set_(self, '_col', 0)
        set_(self, '_byte', -1)
        set_(self, '_err', (0, 1, 0, -1))
        set_(self, '_param_entity_parsing', 0)
        set_(self, '_foreign_dtd', False)
        set_(self, '_reparse_deferral', True)
        set_(self, '_ampl', None)
        set_(self, '_flags', {'buffer_text': False, 'namespace_prefixes': False,
                              'ordered_attributes': False,
                              'specified_attributes': False})
        set_(self, 'intern', intern)
        for name in _HANDLER_NAMES:
            set_(self, name, None)

    # -- attributes -----------------------------------------------------------

    def __getattr__(self, name):
        flags = object.__getattribute__(self, '_flags')
        if name in flags:
            return flags[name]
        if name == 'buffer_size':
            return self._buffer_size
        if name == 'buffer_used':
            return self._buffer_used
        if name == 'ErrorCode':
            return self._err[0]
        if name == 'ErrorLineNumber':
            return self._err[1]
        if name == 'ErrorColumnNumber':
            return self._err[2]
        if name == 'ErrorByteIndex':
            return self._err[3]
        if name == 'CurrentLineNumber':
            return self._line
        if name == 'CurrentColumnNumber':
            return self._col
        if name == 'CurrentByteIndex':
            return self._byte
        raise AttributeError("'pyexpat.xmlparser' object has no attribute "
                             "{!r}".format(name))

    def __setattr__(self, name, value):
        if not isinstance(name, str):
            raise TypeError("attribute name must be string, not '{}'".format(
                type(name).__name__))
        if name in _HANDLER_NAMES:
            if name == 'CharacterDataHandler':
                self._flush()
            if value is not None and not callable(value):
                pass
            object.__setattr__(self, name, value)
            return
        if name in _FLAG_NAMES:
            value = bool(value)
            if name == 'buffer_text':
                if value and self._buffer is None:
                    object.__setattr__(self, '_buffer', [])
                    object.__setattr__(self, '_buffer_used', 0)
                elif not value and self._buffer is not None:
                    self._flush()
                    object.__setattr__(self, '_buffer', None)
            self._flags[name] = value
            return
        if name == 'buffer_size':
            if not isinstance(value, int):
                raise TypeError("buffer_size must be an integer")
            if value <= 0:
                raise ValueError("buffer_size must be greater than zero")
            if value > 2**31 - 1:
                raise ValueError("buffer_size must not be greater than %d"
                                 % (2**31 - 1))
            if self._buffer is not None and value != self._buffer_size:
                self._flush()
            object.__setattr__(self, '_buffer_size', value)
            return
        if name == 'intern':
            raise AttributeError("attribute 'intern' of 'pyexpat.xmlparser' "
                                 "objects is not writable")
        raise AttributeError("'pyexpat.xmlparser' object has no attribute "
                             "{!r}".format(name))

    def __delattr__(self, name):
        raise AttributeError("cannot delete attribute {!r}".format(name))

    # -- helpers --------------------------------------------------------------

    def _intern(self, s):
        d = self.intern
        if d is None:
            return s
        return d.setdefault(s, s)

    def _call(self, handler, *args):
        object.__setattr__(self, '_in_callback', True)
        try:
            return handler(*args)
        finally:
            object.__setattr__(self, '_in_callback', False)

    def _flush(self):
        buf = self._buffer
        if not buf:
            return
        text = ''.join(buf)
        buf.clear()
        object.__setattr__(self, '_buffer_used', 0)
        handler = self.CharacterDataHandler
        if handler is not None:
            self._call(handler, text)

    def _chardata(self, data):
        if self._buffer is None:
            handler = self.CharacterDataHandler
            if handler is not None:
                self._call(handler, data)
            elif self.DefaultHandler is not None:
                self._call(self.DefaultHandler, data)
            elif self.DefaultHandlerExpand is not None:
                self._call(self.DefaultHandlerExpand, data)
            return
        size = len(data.encode('utf-8', 'surrogatepass'))
        if self._buffer_used + size > self._buffer_size:
            self._flush()
            if self._buffer is None:
                handler = self.CharacterDataHandler
                if handler is not None:
                    self._call(handler, data)
                return
        if size > self._buffer_size:
            handler = self.CharacterDataHandler
            if handler is not None:
                self._call(handler, data)
            object.__setattr__(self, '_buffer_used', 0)
        else:
            self._buffer.append(data)
            object.__setattr__(self, '_buffer_used', self._buffer_used + size)

    def _handler(self, name):
        handler = getattr(self, name)
        if handler is not None:
            self._flush()
            # the flush may have removed the handler
            handler = getattr(self, name)
        return handler

    def _default(self, text):
        handler = self.DefaultHandler or self.DefaultHandlerExpand
        if handler is not None:
            self._flush()
            self._call(handler, text)

    def _raise_error(self, code, line, col, byte):
        object.__setattr__(self, '_err', (code, line, col, byte))
        message = "%s: line %d, column %d" % (ErrorString(code), line, col)
        exc = ExpatError(message)
        exc.code = code
        exc.lineno = line
        exc.offset = col
        raise exc

    def _configure(self):
        kw = dict(ns_triplets=self._flags['namespace_prefixes'],
                  param_entity_parsing=self._param_entity_parsing,
                  use_foreign_dtd=self._foreign_dtd,
                  reparse_deferral=self._reparse_deferral,
                  base=self._base)
        if self._ampl is not None:
            kw['ampl_threshold'], kw['ampl_max'] = self._ampl
        self._engine.configure(**kw)

    def _encoding_table(self, name):
        # Single byte encodings known to Python's codecs are supported.
        table = str(bytes(range(256)), name, 'replace')
        if len(table) != 256:
            raise ValueError("multi-byte encodings are not supported")
        return table

    def _feed(self, data, final):
        self._configure()
        events = self._engine.feed(data, final)
        while True:
            need = self._dispatch(events)
            if need is None:
                break
            try:
                table = self._encoding_table(need)
            except BaseException:
                self._engine.encoding_failed()
                raise
            events = self._engine.set_encoding_table(table)
        self._flush()

    def _model(self, nodes, index=0):
        ctype, quant, name, children = nodes[index]
        return (ctype, quant, name,
                tuple(self._model(nodes, c) for c in children))

    def _dispatch(self, events):
        for ev in events:
            kind = ev[0]
            set_ = object.__setattr__
            set_(self, '_line', ev[1])
            set_(self, '_col', ev[2])
            set_(self, '_byte', ev[3])
            if kind == 11:
                self._chardata(ev[4])
            elif kind == 7:
                handler = self._handler('StartElementHandler')
                if handler is None:
                    continue
                name, flat, nspec = ev[4], ev[5], ev[6]
                name = self._intern(name)
                if self._flags['specified_attributes']:
                    flat = flat[:2 * nspec]
                if self._flags['ordered_attributes']:
                    attrs = [self._intern(x) if i % 2 == 0 else x
                             for i, x in enumerate(flat)]
                else:
                    attrs = {}
                    for i in range(0, len(flat), 2):
                        attrs[self._intern(flat[i])] = flat[i + 1]
                self._call(handler, name, attrs)
            elif kind == 8:
                handler = self._handler('EndElementHandler')
                if handler is not None:
                    self._call(handler, self._intern(ev[4]))
            elif kind == 21:
                self._flush()
                self._raise_error(ev[4], ev[1], ev[2], ev[3])
            elif kind == 20:
                return ev[4]
            elif kind == 16:
                self._default(ev[4])
            else:
                self._dispatch_other(kind, ev)
        return None

    def _dispatch_other(self, kind, ev):
        if kind == 0:
            handler = self._handler('XmlDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5], ev[6])
        elif kind == 1:
            handler = self._handler('StartDoctypeDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5], ev[6], ev[7])
        elif kind == 2:
            handler = self._handler('EndDoctypeDeclHandler')
            if handler is not None:
                self._call(handler)
        elif kind == 3:
            handler = self._handler('ElementDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], self._model(ev[5]))
        elif kind == 4:
            handler = self._handler('AttlistDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5], ev[6], ev[7], ev[8])
        elif kind == 5:
            name, is_pe, value, base, sysid, pubid, notation = ev[4:11]
            if notation is not None and self.UnparsedEntityDeclHandler is not None:
                handler = self._handler('UnparsedEntityDeclHandler')
                if handler is not None:
                    self._call(handler, name, base, sysid, pubid, notation)
                return
            handler = self._handler('EntityDeclHandler')
            if handler is not None:
                self._call(handler, name, is_pe, value, base, sysid, pubid,
                           notation)
        elif kind == 6:
            handler = self._handler('NotationDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5], ev[6], ev[7])
        elif kind == 9:
            handler = self._handler('StartNamespaceDeclHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5])
        elif kind == 10:
            handler = self._handler('EndNamespaceDeclHandler')
            if handler is not None:
                self._call(handler, ev[4])
        elif kind == 12:
            handler = self._handler('ProcessingInstructionHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5])
            else:
                data = ev[5]
                self._default('<?%s%s?>' % (ev[4], ' ' + data if data else ''))
        elif kind == 13:
            handler = self._handler('CommentHandler')
            if handler is not None:
                self._call(handler, ev[4])
            else:
                self._default('<!--%s-->' % ev[4])
        elif kind == 14:
            handler = self._handler('StartCdataSectionHandler')
            if handler is not None:
                self._call(handler)
        elif kind == 15:
            handler = self._handler('EndCdataSectionHandler')
            if handler is not None:
                self._call(handler)
        elif kind == 17:
            handler = self._handler('SkippedEntityHandler')
            if handler is not None:
                self._call(handler, ev[4], ev[5])
            else:
                self._default(('%%%s;' if ev[5] else '&%s;') % ev[4])
        elif kind == 18:
            handler = self._handler('ExternalEntityRefHandler')
            if handler is not None:
                result = self._call(handler, ev[4], ev[5], ev[6], ev[7])
                if not isinstance(result, int):
                    raise _invalid_return('ExternalEntityRef', result)
                if not result:
                    self._raise_error(21, ev[1], ev[2], ev[3])
            elif ev[4] is not None:
                self._default('&%s;' % ev[4])
        elif kind == 19:
            handler = self._handler('NotStandaloneHandler')
            if handler is not None:
                result = self._call(handler)
                if not isinstance(result, int):
                    raise _invalid_return('NotStandalone', result)
                if not result:
                    self._raise_error(22, ev[1], ev[2], ev[3])

    # -- public methods ---------------------------------------------------------

    def Parse(self, data, isfinal=False, /):
        """Parse XML data.

        'isfinal' should be true at end of input."""
        if self._in_callback:
            raise RuntimeError("cannot call Parse() from within a handler")
        if isinstance(data, str):
            if not self._engine.started() and self._is_root \
                    and self._encoding is None:
                object.__setattr__(self, '_engine',
                                   _new_engine(self._ns_sep, 'utf-8'))
            data = data.encode('utf-8', 'surrogatepass')
        else:
            data = memoryview(data).tobytes()
        self._feed(data, bool(isfinal))
        return 1

    def ParseFile(self, file, /):
        """Parse XML data from file-like object."""
        if self._in_callback:
            raise RuntimeError("cannot call ParseFile() from within a handler")
        read = file.read
        while True:
            chunk = read(2048)
            if not isinstance(chunk, (bytes, bytearray)):
                raise TypeError("read() did not return a bytes object "
                                "(type=%.400s)" % type(chunk).__name__)
            if not chunk:
                self._feed(b'', True)
                return 1
            self._feed(bytes(chunk), False)

    def SetBase(self, base, /):
        """Set the base URL for the parser."""
        object.__setattr__(self, '_base', base)

    def GetBase(self):
        """Return base URL string for the parser."""
        return self._base

    def GetInputContext(self):
        """Return the untranslated text of the input that caused the current
        event.  (Not available: always None.)"""
        return None

    def ExternalEntityParserCreate(self, context, encoding=None, /):
        """Create a parser for parsing an external entity based on the
        information passed to the ExternalEntityRefHandler."""
        if context is not None and not isinstance(context, str):
            raise TypeError("ExternalEntityParserCreate() argument 1 must be "
                            "str or None, not %s" % type(context).__name__)
        if encoding is not None and not isinstance(encoding, str):
            raise TypeError("ExternalEntityParserCreate() argument 2 must be "
                            "str, not %s" % type(encoding).__name__)
        self._configure()
        engine = self._engine.external(context, encoding)
        sub = xmlparser(_CREATE_TOKEN, encoding, self._ns_sep, self.intern,
                        engine=engine, parent=self)
        for name in _HANDLER_NAMES:
            object.__setattr__(sub, name, getattr(self, name))
        for name in _FLAG_NAMES:
            setattr(sub, name, self._flags[name])
        object.__setattr__(sub, '_buffer_size', self._buffer_size)
        object.__setattr__(sub, '_base', self._base)
        object.__setattr__(sub, '_param_entity_parsing',
                           self._param_entity_parsing)
        object.__setattr__(sub, '_reparse_deferral', self._reparse_deferral)
        return sub

    def SetParamEntityParsing(self, flag, /):
        """Controls parsing of parameter entities (including the external
        DTD subset)."""
        flag = int(flag)
        if flag not in (0, 1, 2):
            return 0
        object.__setattr__(self, '_param_entity_parsing', flag)
        return 1

    def UseForeignDTD(self, flag=True, /):
        """Allows the application to provide an artificial external
        subset if one is not specified as part of the document instance."""
        if self._engine.started():
            self._raise_error(26, self._line, self._col, self._byte)
        object.__setattr__(self, '_foreign_dtd', bool(flag))

    def GetReparseDeferralEnabled(self):
        """Retrieve reparse deferral enabled status; always returns false
        with Expat <2.6.0."""
        return self._reparse_deferral

    def SetReparseDeferralEnabled(self, enabled, /):
        """Enable/Disable reparse deferral; enabled by default with Expat
        >=2.6.0."""
        object.__setattr__(self, '_reparse_deferral', bool(enabled))

    def _check_root(self):
        if not self._is_root:
            raise ExpatError("parser must be a root parser")

    def SetBillionLaughsAttackProtectionActivationThreshold(self, threshold, /):
        """Sets the number of output bytes needed to activate protection
        against billion laughs attacks."""
        if not isinstance(threshold, int):
            raise TypeError("threshold must be an integer")
        if threshold < 0:
            raise ValueError("threshold must be non-negative")
        if threshold > 2**64 - 1:
            raise OverflowError("threshold is too large")
        self._check_root()
        current = self._ampl or (8 * 1024 * 1024, 100.0)
        object.__setattr__(self, '_ampl', (min(threshold, 2**63 - 1), current[1]))

    def SetBillionLaughsAttackProtectionMaximumAmplification(self, max_factor, /):
        """Sets the maximum tolerated amplification factor for protection
        against billion laughs attacks."""
        if isinstance(max_factor, bool) or not isinstance(max_factor, (int, float)):
            raise TypeError("max_factor must be a float")
        max_factor = float(max_factor)
        self._check_root()
        if not max_factor >= 1.0:
            raise ExpatError("'max_factor' must be at least 1.0")
        current = self._ampl or (8 * 1024 * 1024, 100.0)
        object.__setattr__(self, '_ampl', (current[0], max_factor))


xmlparser.__module__ = 'pyexpat'
XMLParserType = xmlparser


_UNSET = object()


def ParserCreate(encoding=None, namespace_separator=None, intern=_UNSET):
    """Return a new XML parser object."""
    if encoding is not None and not isinstance(encoding, str):
        raise TypeError("ParserCreate() argument 'encoding' must be str or "
                        "None, not %s" % type(encoding).__name__)
    if namespace_separator is not None:
        if not isinstance(namespace_separator, str):
            raise TypeError("ParserCreate() argument 'namespace_separator' "
                            "must be str or None, not %s"
                            % type(namespace_separator).__name__)
        if len(namespace_separator) > 1:
            raise ValueError("namespace_separator must be at most one "
                             "character, omitted, or None")
    if intern is _UNSET:
        intern = {}
    elif intern is not None and not isinstance(intern, dict):
        raise TypeError("intern must be a dictionary")
    return xmlparser(_CREATE_TOKEN, encoding, namespace_separator, intern)
