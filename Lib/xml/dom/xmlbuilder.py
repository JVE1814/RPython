"""DOM Level 3 Load and Save support (the draft "LS" interfaces) for
minidom: DOMBuilder, DOMInputSource, DOMEntityResolver and friends."""

import copy
import xml.dom

from xml.dom.NodeFilter import NodeFilter

__all__ = ["DOMBuilder", "DOMEntityResolver", "DOMInputSource"]


class Options:
    """Settings that control how a document is built.

    Each attribute corresponds to one DOMBuilder feature (with '-' replaced
    by '_')."""

    namespaces = 1
    namespace_declarations = True
    validation = False
    external_parameter_entities = True
    external_general_entities = True
    external_dtd_subset = True
    validate_if_schema = False
    validate = False
    datatype_normalization = False
    create_entity_ref_nodes = True
    entities = True
    whitespace_in_element_content = True
    cdata_sections = True
    comments = True
    charset_overrides_xml_encoding = True
    infoset = False
    supported_mediatypes_only = False

    errorHandler = None
    filter = None


# The attribute settings implied by turning a feature on or off; a feature
# that is absent (or lacks the requested state) cannot be set.
_FEATURE_SETTINGS = {
    ("validation", 0): [("validation", 0)],
    ("external_general_entities", 0): [("external_general_entities", 0)],
    ("external_general_entities", 1): [("external_general_entities", 1)],
    ("external_parameter_entities", 0): [("external_parameter_entities", 0)],
    ("external_parameter_entities", 1): [("external_parameter_entities", 1)],
    ("validate_if_schema", 0): [("validate_if_schema", 0)],
    ("create_entity_ref_nodes", 0): [("create_entity_ref_nodes", 0)],
    ("create_entity_ref_nodes", 1): [("create_entity_ref_nodes", 1)],
    ("entities", 0): [("create_entity_ref_nodes", 0), ("entities", 0)],
    ("entities", 1): [("entities", 1)],
    ("whitespace_in_element_content", 0):
        [("whitespace_in_element_content", 0)],
    ("whitespace_in_element_content", 1):
        [("whitespace_in_element_content", 1)],
    ("cdata_sections", 0): [("cdata_sections", 0)],
    ("cdata_sections", 1): [("cdata_sections", 1)],
    ("comments", 0): [("comments", 0)],
    ("comments", 1): [("comments", 1)],
    ("charset_overrides_xml_encoding", 0):
        [("charset_overrides_xml_encoding", 0)],
    ("charset_overrides_xml_encoding", 1):
        [("charset_overrides_xml_encoding", 1)],
    ("infoset", 0): [],
    ("infoset", 1): [("namespace_declarations", 0),
                     ("validate_if_schema", 0),
                     ("create_entity_ref_nodes", 0),
                     ("entities", 0),
                     ("cdata_sections", 0),
                     ("datatype_normalization", 1),
                     ("whitespace_in_element_content", 1),
                     ("comments", 1),
                     ("charset_overrides_xml_encoding", 1)],
    ("supported_mediatypes_only", 0): [("supported_mediatypes_only", 0)],
    ("namespaces", 0): [("namespaces", 0)],
    ("namespaces", 1): [("namespaces", 1)],
}

# For getFeature(): a feature whose value is derived from other settings.
_DERIVED_FEATURES = {
    "infoset": (("validate_if_schema", False),
                ("create_entity_ref_nodes", False),
                ("entities", False),
                ("cdata_sections", False),
                ("datatype_normalization", True),
                ("whitespace_in_element_content", True),
                ("comments", True),
                ("charset_overrides_xml_encoding", True)),
}


def _feature_key(name):
    return name.lower().replace("-", "_")


class DOMBuilder:
    entityResolver = None
    errorHandler = None
    filter = None

    ACTION_REPLACE = 1
    ACTION_APPEND_AS_CHILDREN = 2
    ACTION_INSERT_AFTER = 3
    ACTION_INSERT_BEFORE = 4

    _legal_actions = (ACTION_REPLACE, ACTION_APPEND_AS_CHILDREN,
                      ACTION_INSERT_AFTER, ACTION_INSERT_BEFORE)

    def __init__(self):
        self._options = Options()

    # attribute accessors in the DOM style

    def _get_entityResolver(self):
        return self.entityResolver

    def _set_entityResolver(self, entityResolver):
        self.entityResolver = entityResolver

    def _get_errorHandler(self):
        return self.errorHandler

    def _set_errorHandler(self, errorHandler):
        self.errorHandler = errorHandler

    def _get_filter(self):
        return self.filter

    def _set_filter(self, filter):
        self.filter = filter

    # features

    def setFeature(self, name, state):
        if self.supportsFeature(name):
            state = 1 if state else 0
            try:
                settings = _FEATURE_SETTINGS[(_feature_key(name), state)]
            except KeyError:
                raise xml.dom.NotSupportedErr(
                    "unsupported feature: %r" % (name,)) from None
            for attr, value in settings:
                setattr(self._options, attr, value)
        else:
            raise xml.dom.NotFoundErr("unknown feature: " + repr(name))

    def supportsFeature(self, name):
        return hasattr(self._options, _feature_key(name))

    def canSetFeature(self, name, state):
        key = (_feature_key(name), 1 if state else 0)
        return key in _FEATURE_SETTINGS

    def getFeature(self, name):
        key = _feature_key(name)
        derived = _DERIVED_FEATURES.get(key)
        if derived is not None:
            options = self._options
            for attr, wanted in derived:
                if bool(getattr(options, attr)) != wanted:
                    return False
            return True
        try:
            return getattr(self._options, key)
        except AttributeError:
            raise xml.dom.NotFoundErr("feature %s not known" % repr(name))

    # parsing

    def parseURI(self, uri):
        if self.entityResolver:
            source = self.entityResolver.resolveEntity(None, uri)
        else:
            source = DOMEntityResolver().resolveEntity(None, uri)
        return self.parse(source)

    def parse(self, input):
        options = copy.copy(self._options)
        options.filter = self.filter
        options.errorHandler = self.errorHandler
        fp = input.byteStream
        if fp is None and input.systemId:
            import urllib.request
            fp = urllib.request.urlopen(input.systemId)
        return self._parse_bytestream(fp, options)

    def parseWithContext(self, input, cnode, action):
        if action not in self._legal_actions:
            raise ValueError("not a legal action")
        raise NotImplementedError("Haven't written this yet...")

    def _parse_bytestream(self, stream, options):
        import xml.dom.expatbuilder
        builder = xml.dom.expatbuilder.makeBuilder(options)
        return builder.parseFile(stream)


class DOMEntityResolver:
    __slots__ = '_opener',

    def resolveEntity(self, publicId, systemId):
        assert systemId is not None
        source = DOMInputSource()
        source.publicId = publicId
        source.systemId = systemId
        source.byteStream = self._get_opener().open(systemId)
        source.encoding = self._guess_media_encoding(source)

        # the base URI is everything up to and including the last '/'
        import posixpath
        import urllib.parse
        parts = urllib.parse.urlparse(systemId)
        scheme, netloc, path, params, query, fragment = parts
        if path and not path.endswith("/"):
            path = posixpath.dirname(path) + "/"
            parts = scheme, netloc, path, params, query, fragment
            source.baseURI = urllib.parse.urlunparse(parts)
        return source

    def _get_opener(self):
        try:
            return self._opener
        except AttributeError:
            self._opener = self._create_opener()
            return self._opener

    def _create_opener(self):
        import urllib.request
        return urllib.request.build_opener()

    def _guess_media_encoding(self, source):
        # the charset parameter of the response's Content-Type, if any
        info = source.byteStream.info()
        if "Content-Type" in info:
            return info.get_content_charset()
        return None


class DOMInputSource:
    __slots__ = ('byteStream', 'characterStream', 'stringData',
                 'encoding', 'publicId', 'systemId', 'baseURI')

    def __init__(self):
        self.byteStream = None
        self.characterStream = None
        self.stringData = None
        self.encoding = None
        self.publicId = None
        self.systemId = None
        self.baseURI = None

    def _get_byteStream(self):
        return self.byteStream

    def _set_byteStream(self, byteStream):
        self.byteStream = byteStream

    def _get_characterStream(self):
        return self.characterStream

    def _set_characterStream(self, characterStream):
        self.characterStream = characterStream

    def _get_stringData(self):
        return self.stringData

    def _set_stringData(self, data):
        self.stringData = data

    def _get_encoding(self):
        return self.encoding

    def _set_encoding(self, encoding):
        self.encoding = encoding

    def _get_publicId(self):
        return self.publicId

    def _set_publicId(self, publicId):
        self.publicId = publicId

    def _get_systemId(self):
        return self.systemId

    def _set_systemId(self, systemId):
        self.systemId = systemId

    def _get_baseURI(self):
        return self.baseURI

    def _set_baseURI(self, uri):
        self.baseURI = uri


class DOMBuilderFilter:
    """Filter consulted while a document is built.

    acceptNode() is called for completed nodes, startContainer() when an
    element starts; both return one of the FILTER_* constants."""

    FILTER_ACCEPT = 1
    FILTER_REJECT = 2
    FILTER_SKIP = 3
    FILTER_INTERRUPT = 4

    whatToShow = NodeFilter.SHOW_ALL

    def _get_whatToShow(self):
        return self.whatToShow

    def acceptNode(self, element):
        return self.FILTER_ACCEPT

    def startContainer(self, element):
        return self.FILTER_ACCEPT


del NodeFilter


class DocumentLS:
    """Mixin for Document providing the LS load/save methods."""

    async_ = False

    def _get_async(self):
        return False

    def _set_async(self, flag):
        if flag:
            raise xml.dom.NotSupportedErr(
                "asynchronous document loading is not supported")

    def abort(self):
        raise NotImplementedError(
            "haven't figured out what this means yet")

    def load(self, uri):
        raise NotImplementedError("haven't written this yet")

    def loadXML(self, source):
        raise NotImplementedError("haven't written this yet")

    def saveXML(self, snode):
        if snode is None:
            snode = self
        elif snode.ownerDocument is not self:
            raise xml.dom.WrongDocumentErr()
        return snode.toxml()


class DOMImplementationLS:
    MODE_SYNCHRONOUS = 1
    MODE_ASYNCHRONOUS = 2

    def createDOMBuilder(self, mode, schemaType):
        if schemaType is not None:
            raise xml.dom.NotSupportedErr(
                "schemaType not yet supported")
        if mode == self.MODE_SYNCHRONOUS:
            return DOMBuilder()
        if mode == self.MODE_ASYNCHRONOUS:
            raise xml.dom.NotSupportedErr(
                "asynchronous builders are not supported")
        raise ValueError("unknown value for mode")

    def createDOMWriter(self):
        raise NotImplementedError(
            "the writer interface hasn't been written yet!")

    def createDOMInputSource(self):
        return DOMInputSource()
