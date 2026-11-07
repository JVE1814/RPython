"""Helpers for SAX applications: escaping, XMLGenerator, XMLFilterBase
and prepare_input_source()."""

import os
import io
import sys
import codecs

from . import handler
from . import xmlreader


def _replace_all(text, mapping):
    for key, value in mapping.items():
        text = text.replace(key, value)
    return text


def escape(data, entities={}):
    """Escape &, < and > (and the keys of *entities*) in *data*."""
    data = data.replace("&", "&amp;")
    data = data.replace(">", "&gt;")
    data = data.replace("<", "&lt;")
    if entities:
        data = _replace_all(data, entities)
    return data


def unescape(data, entities={}):
    """Undo escape(): replace &lt;, &gt;, &amp; (and *entities*)."""
    data = data.replace("&lt;", "<")
    data = data.replace("&gt;", ">")
    if entities:
        data = _replace_all(data, entities)
    return data.replace("&amp;", "&")


def quoteattr(data, entities={}):
    """Escape *data* for use as an attribute value and add quotes."""
    entities = {**entities, '\n': '&#10;', '\r': '&#13;', '\t': '&#9;'}
    data = escape(data, entities)
    if '"' in data:
        if "'" in data:
            data = '"%s"' % data.replace('"', "&quot;")
        else:
            data = "'%s'" % data
    else:
        data = '"%s"' % data
    return data


def _gettextwriter(out, encoding):
    if out is None:
        return sys.stdout
    if isinstance(out, io.TextIOBase):
        return out
    if isinstance(out, (codecs.StreamWriter, codecs.StreamReaderWriter)):
        return out
    if isinstance(out, io.RawIOBase):
        class _wrapper:
            __class__ = out.__class__

            def __getattr__(self, name):
                return getattr(out, name)
        buffer = _wrapper()
        buffer.close = lambda: None
    else:
        # a binary stream of some other kind: wrap its write method
        buffer = io.BufferedIOBase()
        buffer.writable = lambda: True
        buffer.write = out.write
        try:
            buffer.seekable = out.seekable
            buffer.tell = out.tell
        except AttributeError:
            pass
    return io.TextIOWrapper(buffer, encoding=encoding,
                            errors='xmlcharrefreplace',
                            newline='\n',
                            write_through=True)


class XMLGenerator(handler.ContentHandler):
    """A ContentHandler writing the events back out as XML."""

    def __init__(self, out=None, encoding="iso-8859-1",
                 short_empty_elements=False):
        handler.ContentHandler.__init__(self)
        out = _gettextwriter(out, encoding)
        self._write = out.write
        self._flush = out.flush
        self._ns_contexts = [{}]
        self._current_context = self._ns_contexts[-1]
        self._undeclared_ns_maps = []
        self._encoding = encoding
        self._short_empty_elements = short_empty_elements
        self._pending_start_element = False

    def _qname(self, name):
        uri, local = name
        if uri:
            if uri == 'http://www.w3.org/XML/1998/namespace':
                return 'xml:' + local
            prefix = self._current_context[uri]
            if prefix:
                return prefix + ":" + local
        return local

    def _finish_pending_start_element(self, endElement=False):
        if self._pending_start_element:
            self._write('>')
            self._pending_start_element = False

    def startDocument(self):
        self._write('<?xml version="1.0" encoding="%s"?>\n' % self._encoding)

    def endDocument(self):
        self._flush()

    def startPrefixMapping(self, prefix, uri):
        self._ns_contexts.append(self._current_context.copy())
        self._current_context[uri] = prefix
        self._undeclared_ns_maps.append((prefix, uri))

    def endPrefixMapping(self, prefix):
        self._current_context = self._ns_contexts[-1]
        del self._ns_contexts[-1]

    def _open_tag(self, name, attrs, qualify):
        self._finish_pending_start_element()
        self._write('<' + name)
        if qualify:
            for prefix, uri in self._undeclared_ns_maps:
                if prefix:
                    self._write(' xmlns:%s=%s' % (prefix, quoteattr(uri)))
                else:
                    self._write(' xmlns=%s' % quoteattr(uri))
            self._undeclared_ns_maps = []
        for key, value in attrs.items():
            if qualify:
                key = self._qname(key)
            self._write(' %s=%s' % (key, quoteattr(value)))
        if self._short_empty_elements:
            self._pending_start_element = True
        else:
            self._write(">")

    def _close_tag(self, name):
        if self._pending_start_element:
            self._write('/>')
            self._pending_start_element = False
        else:
            self._write('</%s>' % name)

    def startElement(self, name, attrs):
        self._open_tag(name, attrs, False)

    def endElement(self, name):
        self._close_tag(name)

    def startElementNS(self, name, qname, attrs):
        self._open_tag(self._qname(name), attrs, True)

    def endElementNS(self, name, qname):
        self._close_tag(self._qname(name))

    def characters(self, content):
        if content:
            self._finish_pending_start_element()
            if not isinstance(content, str):
                content = str(content, self._encoding)
            self._write(escape(content))

    def ignorableWhitespace(self, content):
        if content:
            self._finish_pending_start_element()
            if not isinstance(content, str):
                content = str(content, self._encoding)
            self._write(content)

    def processingInstruction(self, target, data):
        self._finish_pending_start_element()
        self._write('<?%s %s?>' % (target, data))


class XMLFilterBase(xmlreader.XMLReader):
    """An XMLReader between an application and a parent reader, passing
    all events through (override methods to filter)."""

    def __init__(self, parent=None):
        xmlreader.XMLReader.__init__(self)
        self._parent = parent

    # ErrorHandler
    def error(self, exception):
        self._err_handler.error(exception)

    def fatalError(self, exception):
        self._err_handler.fatalError(exception)

    def warning(self, exception):
        self._err_handler.warning(exception)

    # ContentHandler
    def setDocumentLocator(self, locator):
        self._cont_handler.setDocumentLocator(locator)

    def startDocument(self):
        self._cont_handler.startDocument()

    def endDocument(self):
        self._cont_handler.endDocument()

    def startPrefixMapping(self, prefix, uri):
        self._cont_handler.startPrefixMapping(prefix, uri)

    def endPrefixMapping(self, prefix):
        self._cont_handler.endPrefixMapping(prefix)

    def startElement(self, name, attrs):
        self._cont_handler.startElement(name, attrs)

    def endElement(self, name):
        self._cont_handler.endElement(name)

    def startElementNS(self, name, qname, attrs):
        self._cont_handler.startElementNS(name, qname, attrs)

    def endElementNS(self, name, qname):
        self._cont_handler.endElementNS(name, qname)

    def characters(self, content):
        self._cont_handler.characters(content)

    def ignorableWhitespace(self, chars):
        self._cont_handler.ignorableWhitespace(chars)

    def processingInstruction(self, target, data):
        self._cont_handler.processingInstruction(target, data)

    def skippedEntity(self, name):
        self._cont_handler.skippedEntity(name)

    # DTDHandler
    def notationDecl(self, name, publicId, systemId):
        self._dtd_handler.notationDecl(name, publicId, systemId)

    def unparsedEntityDecl(self, name, publicId, systemId, ndata):
        self._dtd_handler.unparsedEntityDecl(name, publicId, systemId, ndata)

    # EntityResolver
    def resolveEntity(self, publicId, systemId):
        return self._ent_handler.resolveEntity(publicId, systemId)

    # XMLReader
    def parse(self, source):
        self._parent.setContentHandler(self)
        self._parent.setErrorHandler(self)
        self._parent.setEntityResolver(self)
        self._parent.setDTDHandler(self)
        self._parent.parse(source)

    def setLocale(self, locale):
        self._parent.setLocale(locale)

    def getFeature(self, name):
        return self._parent.getFeature(name)

    def setFeature(self, name, state):
        self._parent.setFeature(name, state)

    def getProperty(self, name):
        return self._parent.getProperty(name)

    def setProperty(self, name, value):
        self._parent.setProperty(name, value)

    def getParent(self):
        return self._parent

    def setParent(self, parent):
        self._parent = parent


def prepare_input_source(source, base=""):
    """An InputSource with an open stream for source: a system id (URL or
    file name, resolved against base), a path-like object, an open file
    (text or binary) or an InputSource."""
    if isinstance(source, os.PathLike):
        source = os.fspath(source)
    if isinstance(source, str):
        source = xmlreader.InputSource(source)
    elif hasattr(source, "read"):
        source = _input_source_for_file(source)
    if source.getCharacterStream() is None and source.getByteStream() is None:
        source.setByteStream(_open_system_id(source, base))
    return source


def _input_source_for_file(f):
    wrapped = xmlreader.InputSource()
    # read(0) tells text files from binary ones without consuming anything
    if isinstance(f.read(0), str):
        wrapped.setCharacterStream(f)
    else:
        wrapped.setByteStream(f)
    name = getattr(f, "name", None)
    if isinstance(name, str):
        wrapped.setSystemId(name)
    return wrapped


def _open_system_id(source, base):
    """Open source's system id: a file next to base if there is one, else
    the URL it makes relative to base."""
    sysid = source.getSystemId()
    candidate = os.path.join(os.path.dirname(os.path.normpath(base)), sysid)
    if os.path.isfile(candidate):
        source.setSystemId(candidate)
        return open(candidate, "rb")
    import urllib.parse
    import urllib.request
    source.setSystemId(urllib.parse.urljoin(base, sysid))
    return urllib.request.urlopen(source.getSystemId())
