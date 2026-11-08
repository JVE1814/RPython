"""The SAX 2 driver interfaces: XMLReader and IncrementalParser (what a
parser implements), Locator, InputSource, and the attribute containers
handed to startElement()/startElementNS()."""

from . import handler

from ._exceptions import SAXNotSupportedException, SAXNotRecognizedException


def _abstract(what):
    raise NotImplementedError(f"{what} has to be provided by the parser driver")


class XMLReader:
    """Base class of SAX parsers: holds the four handlers (each starts as
    the do-nothing default from xml.sax.handler) and rejects every feature
    and property it does not know."""

    def __init__(self):
        self._cont_handler = handler.ContentHandler()
        self._dtd_handler = handler.DTDHandler()
        self._ent_handler = handler.EntityResolver()
        self._err_handler = handler.ErrorHandler()

    def parse(self, source):
        """Parse a document given as a system id (URL or file name), a
        file object or an InputSource."""
        _abstract("parse()")

    def getContentHandler(self):
        return self._cont_handler

    def setContentHandler(self, handler):
        self._cont_handler = handler

    def getDTDHandler(self):
        return self._dtd_handler

    def setDTDHandler(self, handler):
        self._dtd_handler = handler

    def getEntityResolver(self):
        return self._ent_handler

    def setEntityResolver(self, resolver):
        self._ent_handler = resolver

    def getErrorHandler(self):
        return self._err_handler

    def setErrorHandler(self, handler):
        self._err_handler = handler

    def setLocale(self, locale):
        """Choose the language of error messages; not supported here."""
        raise SAXNotSupportedException("Locale support not implemented")

    def getFeature(self, name):
        raise SAXNotRecognizedException("Feature '%s' not recognized" % name)

    def setFeature(self, name, state):
        raise SAXNotRecognizedException("Feature '%s' not recognized" % name)

    def getProperty(self, name):
        raise SAXNotRecognizedException("Property '%s' not recognized" % name)

    def setProperty(self, name, value):
        raise SAXNotRecognizedException("Property '%s' not recognized" % name)


class IncrementalParser(XMLReader):
    """A parser that accepts the document in pieces: prepareParser(),
    then feed() any number of times, then close(); reset() makes it ready
    for the next document.  parse() is built from these, reading the input
    *bufsize* characters or bytes at a time."""

    def __init__(self, bufsize=2**16):
        self._bufsize = bufsize
        XMLReader.__init__(self)

    def parse(self, source):
        from . import saxutils
        source = saxutils.prepare_input_source(source)
        self.prepareParser(source)
        stream = source.getCharacterStream()
        if stream is None:
            stream = source.getByteStream()
        while True:
            chunk = stream.read(self._bufsize)
            if not chunk:
                break
            self.feed(chunk)
        self.close()

    def feed(self, data):
        _abstract("feed()")

    def prepareParser(self, source):
        _abstract("prepareParser()")

    def close(self):
        _abstract("close()")

    def reset(self):
        _abstract("reset()")


class Locator:
    """Where in the document the current event comes from.  This base
    class knows nothing: line and column are -1, the ids None."""

    def getColumnNumber(self):
        return -1

    def getLineNumber(self):
        return -1

    def getPublicId(self):
        return None

    def getSystemId(self):
        return None


class InputSource:
    """Everything a parser needs to read one document: its public and
    system ids, an encoding hint, and a byte stream or a character stream
    (the character stream wins when both are set)."""

    def __init__(self, system_id=None):
        self._system_id = system_id
        self._public_id = None
        self._encoding = None
        self._byte_stream = None
        self._char_stream = None

    def setPublicId(self, public_id):
        self._public_id = public_id

    def getPublicId(self):
        return self._public_id

    def setSystemId(self, system_id):
        self._system_id = system_id

    def getSystemId(self):
        return self._system_id

    def setEncoding(self, encoding):
        self._encoding = encoding

    def getEncoding(self):
        return self._encoding

    def setByteStream(self, bytefile):
        self._byte_stream = bytefile

    def getByteStream(self):
        return self._byte_stream

    def setCharacterStream(self, charfile):
        self._char_stream = charfile

    def getCharacterStream(self):
        return self._char_stream


class AttributesImpl:
    """The attributes of an element, as a read-only mapping from name to
    value.  Without namespaces a name is also its qualified name, and
    every attribute has type CDATA."""

    def __init__(self, attrs):
        self._attrs = attrs

    def _known(self, name):
        if name not in self._attrs:
            raise KeyError(name)
        return name

    def getLength(self):
        return len(self._attrs)

    def getType(self, name):
        return "CDATA"

    def getValue(self, name):
        return self._attrs[name]

    def getValueByQName(self, name):
        return self._attrs[name]

    def getNameByQName(self, name):
        return self._known(name)

    def getQNameByName(self, name):
        return self._known(name)

    def getNames(self):
        return list(self._attrs)

    def getQNames(self):
        return list(self._attrs)

    def __len__(self):
        return len(self._attrs)

    def __getitem__(self, name):
        return self._attrs[name]

    def __contains__(self, name):
        return name in self._attrs

    def keys(self):
        return list(self._attrs)

    def get(self, name, alternative=None):
        return self._attrs.get(name, alternative)

    def items(self):
        return list(self._attrs.items())

    def values(self):
        return list(self._attrs.values())

    def copy(self):
        return self.__class__(self._attrs)


class AttributesNSImpl(AttributesImpl):
    """Attributes keyed by (namespace URI, local name) pairs; *qnames* maps
    the same keys to the qualified names used in the document."""

    def __init__(self, attrs, qnames):
        self._attrs = attrs
        self._qnames = qnames

    def _name_for_qname(self, qname):
        for name, candidate in self._qnames.items():
            if candidate == qname:
                return name
        raise KeyError(qname)

    def getValueByQName(self, name):
        return self._attrs[self._name_for_qname(name)]

    def getNameByQName(self, name):
        return self._name_for_qname(name)

    def getQNameByName(self, name):
        return self._qnames[name]

    def getQNames(self):
        return list(self._qnames.values())

    def copy(self):
        return self.__class__(self._attrs, self._qnames)
