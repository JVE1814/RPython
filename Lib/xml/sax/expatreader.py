"""SAX2 driver for the expat parser (an xmlreader.IncrementalParser)."""

_VERSION = "0.20"

from xml.sax._exceptions import *
from xml.sax.handler import feature_validation, feature_namespaces
from xml.sax.handler import feature_namespace_prefixes
from xml.sax.handler import feature_external_ges, feature_external_pes
from xml.sax.handler import feature_string_interning
from xml.sax.handler import property_xml_string, property_interning_dict

try:
    from xml.parsers import expat
except ImportError:
    raise SAXReaderNotAvailable("expat not supported", None)
if not hasattr(expat, "ParserCreate"):
    raise SAXReaderNotAvailable("expat not supported", None)

from xml.sax import xmlreader, saxutils, handler

AttributesImpl = xmlreader.AttributesImpl
AttributesNSImpl = xmlreader.AttributesNSImpl

import weakref


class _ClosedParser:
    """Takes the expat parser's place after close(), keeping the position
    of the end (or of the error) for the locator."""
    ErrorColumnNumber = None
    ErrorLineNumber = None


class ExpatLocator(xmlreader.Locator):
    """The locator handed to the content handler.  It refers to the reader
    weakly: the handler often keeps it, and the reader keeps the handler."""

    def __init__(self, parser):
        self._ref = weakref.ref(parser)

    def _reader(self):
        return self._ref()

    def getColumnNumber(self):
        reader = self._reader()
        if reader is None or reader._parser is None:
            return None
        return reader._parser.ErrorColumnNumber

    def getLineNumber(self):
        reader = self._reader()
        if reader is None or reader._parser is None:
            return 1
        return reader._parser.ErrorLineNumber

    def getPublicId(self):
        reader = self._reader()
        return None if reader is None else reader._source.getPublicId()

    def getSystemId(self):
        reader = self._reader()
        return None if reader is None else reader._source.getSystemId()


def _ns_pair(name):
    """expat's "uri local [prefix]" name -> (uri, local); a name without a
    namespace -> (None, name)."""
    parts = name.split()
    if len(parts) == 1:
        return (None, name)
    return (parts[0], parts[1])


def _ns_qname(name):
    """The qualified name of an expat namespace name."""
    parts = name.split()
    if len(parts) == 3:
        return "%s:%s" % (parts[2], parts[1])
    return parts[-1]


class ExpatParser(xmlreader.IncrementalParser, xmlreader.Locator):
    """SAX reader on top of pyexpat."""

    def __init__(self, namespaceHandling=0, bufsize=2**16 - 20):
        xmlreader.IncrementalParser.__init__(self, bufsize)
        self._source = xmlreader.InputSource()
        self._parser = None
        self._namespaces = namespaceHandling
        self._lex_handler_prop = None
        self._parsing = False
        # (parser, source) of the documents an external entity interrupted
        self._entity_stack = []
        self._external_ges = 0
        self._interning = None

    # -- XMLReader

    def parse(self, source):
        """Parse a whole document (system id, file or InputSource)."""
        source = saxutils.prepare_input_source(source)
        self._source = source
        try:
            self.reset()
            self._cont_handler.setDocumentLocator(ExpatLocator(self))
            xmlreader.IncrementalParser.parse(self, source)
        except:
            # do not leave the file open when parsing fails
            self._close_source()
            raise

    def prepareParser(self, source):
        if source.getSystemId() is not None:
            self._parser.SetBase(source.getSystemId())

    def setContentHandler(self, handler):
        xmlreader.IncrementalParser.setContentHandler(self, handler)
        if self._parsing:
            self._reset_cont_handler()

    def getFeature(self, name):
        if name == feature_namespaces:
            return self._namespaces
        if name == feature_string_interning:
            return self._interning is not None
        if name in (feature_validation, feature_external_pes,
                    feature_namespace_prefixes):
            return 0
        if name == feature_external_ges:
            return self._external_ges
        raise SAXNotRecognizedException("Feature '%s' not recognized" % name)

    # features expat cannot provide: name -> what to say when asked to
    _UNSUPPORTED = {
        feature_validation: "expat does not support validation",
        feature_external_pes:
            "expat does not read external parameter entities",
        feature_namespace_prefixes:
            "expat does not report namespace prefixes",
    }

    def setFeature(self, name, state):
        if self._parsing:
            raise SAXNotSupportedException("Cannot set features while parsing")
        if name == feature_namespaces:
            self._namespaces = state
        elif name == feature_external_ges:
            self._external_ges = state
        elif name == feature_string_interning:
            if not state:
                self._interning = None
            elif self._interning is None:
                self._interning = {}
        elif name in self._UNSUPPORTED:
            if state:
                raise SAXNotSupportedException(self._UNSUPPORTED[name])
        else:
            raise SAXNotRecognizedException(
                "Feature '%s' not recognized" % name)

    def getProperty(self, name):
        if name == handler.property_lexical_handler:
            return self._lex_handler_prop
        if name == property_interning_dict:
            return self._interning
        if name == property_xml_string:
            if not self._parser:
                raise SAXNotSupportedException(
                    "XML string cannot be returned when not parsing")
            if not hasattr(self._parser, "GetInputContext"):
                raise SAXNotRecognizedException(
                    "This version of expat does not support getting"
                    " the XML string")
            return self._parser.GetInputContext()
        raise SAXNotRecognizedException("Property '%s' not recognized" % name)

    def setProperty(self, name, value):
        if name == handler.property_lexical_handler:
            self._lex_handler_prop = value
            if self._parsing:
                self._reset_lex_handler_prop()
        elif name == property_interning_dict:
            self._interning = value
        elif name == property_xml_string:
            raise SAXNotSupportedException("Property '%s' cannot be set" %
                                           name)
        else:
            raise SAXNotRecognizedException("Property '%s' not recognized" %
                                            name)

    # -- IncrementalParser

    def _report(self, error):
        self._err_handler.fatalError(
            SAXParseException(expat.ErrorString(error.code), error, self))

    def feed(self, data, isFinal=False):
        if not self._parsing:
            self.reset()
            self._parsing = True
            self._cont_handler.startDocument()
        # isFinal is only true when close() completes the document
        try:
            self._parser.Parse(data, isFinal)
        except expat.error as e:
            self._report(e)

    def flush(self):
        """Have expat process everything fed so far (expat may hold back
        a partial token otherwise)."""
        parser = self._parser
        if parser is None:
            return
        deferring = parser.GetReparseDeferralEnabled()
        try:
            parser.SetReparseDeferralEnabled(False)
            parser.Parse(b"", False)
        except expat.error as e:
            self._report(e)
        finally:
            parser.SetReparseDeferralEnabled(deferring)

    def _close_source(self):
        source = self._source
        try:
            stream = source.getCharacterStream()
            if stream is not None:
                stream.close()
        finally:
            stream = source.getByteStream()
            if stream is not None:
                stream.close()

    def close(self):
        if (self._entity_stack or self._parser is None
                or isinstance(self._parser, _ClosedParser)):
            # nothing is being parsed, or an external entity ended: the
            # entity's parser is finished by external_entity_ref()
            return
        try:
            self.feed(b"", isFinal=True)
            self._cont_handler.endDocument()
            self._parsing = False
            # the parser's handlers are our bound methods: drop the cycle
            self._parser = None
        finally:
            self._parsing = False
            if self._parser is not None:
                # an error: keep where it happened
                closed = _ClosedParser()
                closed.ErrorColumnNumber = self._parser.ErrorColumnNumber
                closed.ErrorLineNumber = self._parser.ErrorLineNumber
                self._parser = closed
            self._close_source()

    def _reset_cont_handler(self):
        parser, content = self._parser, self._cont_handler
        parser.ProcessingInstructionHandler = content.processingInstruction
        parser.CharacterDataHandler = content.characters

    def _reset_lex_handler_prop(self):
        lexical = self._lex_handler_prop
        parser = self._parser
        if lexical is None:
            parser.CommentHandler = None
            parser.StartCdataSectionHandler = None
            parser.EndCdataSectionHandler = None
            parser.StartDoctypeDeclHandler = None
            parser.EndDoctypeDeclHandler = None
        else:
            parser.CommentHandler = lexical.comment
            parser.StartCdataSectionHandler = lexical.startCDATA
            parser.EndCdataSectionHandler = lexical.endCDATA
            parser.StartDoctypeDeclHandler = self.start_doctype_decl
            parser.EndDoctypeDeclHandler = lexical.endDTD

    def reset(self):
        encoding = self._source.getEncoding()
        if self._namespaces:
            parser = expat.ParserCreate(encoding, " ", intern=self._interning)
            parser.namespace_prefixes = 1
            parser.StartElementHandler = self.start_element_ns
            parser.EndElementHandler = self.end_element_ns
        else:
            parser = expat.ParserCreate(encoding, intern=self._interning)
            parser.StartElementHandler = self.start_element
            parser.EndElementHandler = self.end_element
        self._parser = parser
        self._reset_cont_handler()
        parser.UnparsedEntityDeclHandler = self.unparsed_entity_decl
        parser.NotationDeclHandler = self.notation_decl
        parser.StartNamespaceDeclHandler = self.start_namespace_decl
        parser.EndNamespaceDeclHandler = self.end_namespace_decl
        self._decl_handler_prop = None
        if self._lex_handler_prop:
            self._reset_lex_handler_prop()
        parser.ExternalEntityRefHandler = self.external_entity_ref
        try:
            parser.SkippedEntityHandler = self.skipped_entity_handler
        except AttributeError:
            # an old pyexpat
            pass
        parser.SetParamEntityParsing(
            expat.XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE)
        self._parsing = False
        self._entity_stack = []

    # -- Locator

    def getColumnNumber(self):
        if self._parser is None:
            return None
        return self._parser.ErrorColumnNumber

    def getLineNumber(self):
        if self._parser is None:
            return 1
        return self._parser.ErrorLineNumber

    def getPublicId(self):
        return self._source.getPublicId()

    def getSystemId(self):
        return self._source.getSystemId()

    # -- expat handlers

    def start_element(self, name, attrs):
        self._cont_handler.startElement(name, AttributesImpl(attrs))

    def end_element(self, name):
        self._cont_handler.endElement(name)

    def start_element_ns(self, name, attrs):
        values = {}
        qnames = {}
        for aname, value in attrs.items():
            pair = _ns_pair(aname)
            values[pair] = value
            qnames[pair] = _ns_qname(aname)
        self._cont_handler.startElementNS(_ns_pair(name), None,
                                          AttributesNSImpl(values, qnames))

    def end_element_ns(self, name):
        self._cont_handler.endElementNS(_ns_pair(name), None)

    def processing_instruction(self, target, data):
        self._cont_handler.processingInstruction(target, data)

    def character_data(self, data):
        self._cont_handler.characters(data)

    def start_namespace_decl(self, prefix, uri):
        self._cont_handler.startPrefixMapping(prefix, uri)

    def end_namespace_decl(self, prefix):
        self._cont_handler.endPrefixMapping(prefix)

    def start_doctype_decl(self, name, sysid, pubid, has_internal_subset):
        self._lex_handler_prop.startDTD(name, pubid, sysid)

    def unparsed_entity_decl(self, name, base, sysid, pubid, notation_name):
        self._dtd_handler.unparsedEntityDecl(name, pubid, sysid, notation_name)

    def notation_decl(self, name, base, sysid, pubid):
        self._dtd_handler.notationDecl(name, pubid, sysid)

    def external_entity_ref(self, context, base, sysid, pubid):
        if not self._external_ges:
            return 1
        source = self._ent_handler.resolveEntity(pubid, sysid)
        source = saxutils.prepare_input_source(source,
                                               self._source.getSystemId() or "")
        # parse the entity with a sub-parser, then carry on with ours
        self._entity_stack.append((self._parser, self._source))
        self._parser = self._parser.ExternalEntityParserCreate(context)
        self._source = source
        try:
            xmlreader.IncrementalParser.parse(self, source)
        except:
            return 0
        self._parser, self._source = self._entity_stack.pop()
        return 1

    def skipped_entity_handler(self, name, is_pe):
        if is_pe:
            # SAX names skipped parameter entities with a leading '%'
            name = '%' + name
        self._cont_handler.skippedEntity(name)


def create_parser(*args, **kwargs):
    return ExpatParser(*args, **kwargs)


def __getattr__(name):
    if name == "version":
        import warnings
        warnings.warn("'version' is deprecated and slated for removal in "
                      "Python 3.20", DeprecationWarning, stacklevel=2)
        return _VERSION
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
