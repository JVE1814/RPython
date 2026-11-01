"""SAX driver for the expat parser."""

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
else:
    if not hasattr(expat, "ParserCreate"):
        raise SAXReaderNotAvailable("expat not supported", None)
from xml.sax import xmlreader, saxutils, handler

AttributesImpl = xmlreader.AttributesImpl
AttributesNSImpl = xmlreader.AttributesNSImpl

import weakref


class _ClosedParser:
    # Stands in for the expat parser after close(); keeps the position.
    ErrorColumnNumber = None
    ErrorLineNumber = None


class ExpatLocator(xmlreader.Locator):
    """Locator for an ExpatParser (holds only a weak reference)."""

    def __init__(self, parser):
        self._ref = weakref.ref(parser)

    def getColumnNumber(self):
        parser = self._ref()
        if parser is None or parser._parser is None:
            return None
        return parser._parser.ErrorColumnNumber

    def getLineNumber(self):
        parser = self._ref()
        if parser is None or parser._parser is None:
            return 1
        return parser._parser.ErrorLineNumber

    def getPublicId(self):
        parser = self._ref()
        if parser is None:
            return None
        return parser._source.getPublicId()

    def getSystemId(self):
        parser = self._ref()
        if parser is None:
            return None
        return parser._source.getSystemId()


class ExpatParser(xmlreader.IncrementalParser, xmlreader.Locator):
    """SAX driver for the pyexpat C module."""

    def __init__(self, namespaceHandling=0, bufsize=2**16 - 20):
        xmlreader.IncrementalParser.__init__(self, bufsize)
        self._source = xmlreader.InputSource()
        self._parser = None
        self._namespaces = namespaceHandling
        self._lex_handler_prop = None
        self._parsing = False
        self._entity_stack = []
        self._external_ges = 0
        self._interning = None

    # -- XMLReader --------------------------------------------------------

    def parse(self, source):
        """Parse an XML document from a URL or an InputSource."""
        source = saxutils.prepare_input_source(source)
        self._source = source
        try:
            self.reset()
            self._cont_handler.setDocumentLocator(ExpatLocator(self))
            xmlreader.IncrementalParser.parse(self, source)
        except:
            # the content handler is notified in close() on success only
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

    def setFeature(self, name, state):
        if self._parsing:
            raise SAXNotSupportedException("Cannot set features while parsing")
        if name == feature_namespaces:
            self._namespaces = state
        elif name == feature_external_ges:
            self._external_ges = state
        elif name == feature_string_interning:
            if state:
                if self._interning is None:
                    self._interning = {}
            else:
                self._interning = None
        elif name == feature_validation:
            if state:
                raise SAXNotSupportedException(
                    "expat does not support validation")
        elif name == feature_external_pes:
            if state:
                raise SAXNotSupportedException(
                    "expat does not read external parameter entities")
        elif name == feature_namespace_prefixes:
            if state:
                raise SAXNotSupportedException(
                    "expat does not report namespace prefixes")
        else:
            raise SAXNotRecognizedException(
                "Feature '%s' not recognized" % name)

    def getProperty(self, name):
        if name == handler.property_lexical_handler:
            return self._lex_handler_prop
        if name == property_interning_dict:
            return self._interning
        if name == property_xml_string:
            if self._parser:
                if hasattr(self._parser, "GetInputContext"):
                    return self._parser.GetInputContext()
                raise SAXNotRecognizedException(
                    "This version of expat does not support getting"
                    " the XML string")
            raise SAXNotSupportedException(
                "XML string cannot be returned when not parsing")
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

    # -- IncrementalParser ------------------------------------------------

    def feed(self, data, isFinal=False):
        if not self._parsing:
            self.reset()
            self._parsing = True
            self._cont_handler.startDocument()
        try:
            self._parser.Parse(data, isFinal)
        except expat.error as e:
            exc = SAXParseException(expat.ErrorString(e.code), e, self)
            self._err_handler.fatalError(exc)

    def flush(self):
        if self._parser is None:
            return
        was_enabled = self._parser.GetReparseDeferralEnabled()
        try:
            self._parser.SetReparseDeferralEnabled(False)
            self._parser.Parse(b"", False)
        except expat.error as e:
            exc = SAXParseException(expat.ErrorString(e.code), e, self)
            self._err_handler.fatalError(exc)
        finally:
            self._parser.SetReparseDeferralEnabled(was_enabled)

    def _close_source(self):
        source = self._source
        try:
            file = source.getCharacterStream()
            if file is not None:
                file.close()
        finally:
            file = source.getByteStream()
            if file is not None:
                file.close()

    def close(self):
        if (self._entity_stack or self._parser is None or
                isinstance(self._parser, _ClosedParser)):
            # a nested entity parser, or already closed
            return
        try:
            self.feed(b"", isFinal=True)
            self._cont_handler.endDocument()
            self._parsing = False
            self._parser = None
        finally:
            self._parsing = False
            if self._parser is not None:
                closed = _ClosedParser()
                closed.ErrorColumnNumber = self._parser.ErrorColumnNumber
                closed.ErrorLineNumber = self._parser.ErrorLineNumber
                self._parser = closed
            self._close_source()

    def _reset_cont_handler(self):
        self._parser.ProcessingInstructionHandler = \
            self._cont_handler.processingInstruction
        self._parser.CharacterDataHandler = self._cont_handler.characters

    def _reset_lex_handler_prop(self):
        lex = self._lex_handler_prop
        parser = self._parser
        if lex is None:
            parser.CommentHandler = None
            parser.StartCdataSectionHandler = None
            parser.EndCdataSectionHandler = None
            parser.StartDoctypeDeclHandler = None
            parser.EndDoctypeDeclHandler = None
        else:
            parser.CommentHandler = lex.comment
            parser.StartCdataSectionHandler = lex.startCDATA
            parser.EndCdataSectionHandler = lex.endCDATA
            parser.StartDoctypeDeclHandler = self.start_doctype_decl
            parser.EndDoctypeDeclHandler = lex.endDTD

    def reset(self):
        if self._namespaces:
            self._parser = expat.ParserCreate(self._source.getEncoding(), " ",
                                              intern=self._interning)
            self._parser.namespace_prefixes = 1
            self._parser.StartElementHandler = self.start_element_ns
            self._parser.EndElementHandler = self.end_element_ns
        else:
            self._parser = expat.ParserCreate(self._source.getEncoding(),
                                              intern=self._interning)
            self._parser.StartElementHandler = self.start_element
            self._parser.EndElementHandler = self.end_element

        self._reset_cont_handler()
        self._parser.UnparsedEntityDeclHandler = self.unparsed_entity_decl
        self._parser.NotationDeclHandler = self.notation_decl
        self._parser.StartNamespaceDeclHandler = self.start_namespace_decl
        self._parser.EndNamespaceDeclHandler = self.end_namespace_decl

        self._decl_handler_prop = None
        if self._lex_handler_prop:
            self._reset_lex_handler_prop()
        self._parser.ExternalEntityRefHandler = self.external_entity_ref
        self._parser.SkippedEntityHandler = self.skipped_entity_handler
        self._parser.SetParamEntityParsing(
            expat.XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE)

        self._parsing = False
        self._entity_stack = []

    # -- Locator ------------------------------------------------------------

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

    # -- event handlers ------------------------------------------------------

    def start_element(self, name, attrs):
        self._cont_handler.startElement(name, AttributesImpl(attrs))

    def end_element(self, name):
        self._cont_handler.endElement(name)

    @staticmethod
    def _split_name(name):
        # 'uri local prefix' / 'uri local' / 'local' -> (uri, local)
        parts = name.split()
        if len(parts) == 1:
            return (None, name)
        if len(parts) == 3:
            return parts[0], parts[1]
        return tuple(parts)

    def start_element_ns(self, name, attrs):
        pair = self._split_name(name)
        newattrs = {}
        qnames = {}
        for aname, value in attrs.items():
            parts = aname.split()
            if len(parts) == 1:
                qname = aname
                apair = (None, aname)
            elif len(parts) == 3:
                qname = "%s:%s" % (parts[2], parts[1])
                apair = parts[0], parts[1]
            else:
                apair = tuple(parts)
                qname = parts[1]
            newattrs[apair] = value
            qnames[apair] = qname
        self._cont_handler.startElementNS(pair, None,
                                          AttributesNSImpl(newattrs, qnames))

    def end_element_ns(self, name):
        self._cont_handler.endElementNS(self._split_name(name), None)

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
        self._entity_stack.append((self._parser, self._source))
        self._parser = self._parser.ExternalEntityParserCreate(context)
        self._source = source
        try:
            xmlreader.IncrementalParser.parse(self, source)
        except:
            return 0
        (self._parser, self._source) = self._entity_stack[-1]
        del self._entity_stack[-1]
        return 1

    def skipped_entity_handler(self, name, is_pe):
        if is_pe:
            name = '%' + name
        self._cont_handler.skippedEntity(name)


def create_parser(*args, **kwargs):
    return ExpatParser(*args, **kwargs)

def __getattr__(name):
    # the module version attribute is kept only for compatibility
    if name == "version":
        import warnings
        warnings.warn("'version' is deprecated and slated for removal in "
                      "Python 3.20", DeprecationWarning, stacklevel=2)
        return _VERSION
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
