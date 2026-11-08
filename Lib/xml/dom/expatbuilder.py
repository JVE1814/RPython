"""Build minidom documents with the expat parser.

parse() and parseString() return a Document; parseFragment() and
parseFragmentString() return a DocumentFragment parsed in the context of
an existing node.  makeBuilder() is what xml.dom.xmlbuilder uses.
"""

from xml.dom import xmlbuilder, minidom, Node
from xml.dom import EMPTY_NAMESPACE, EMPTY_PREFIX, XMLNS_NAMESPACE
from xml.parsers import expat
from xml.dom.minidom import _append_child, _set_attribute_node
from xml.dom.NodeFilter import NodeFilter

TEXT_NODE = Node.TEXT_NODE
CDATA_SECTION_NODE = Node.CDATA_SECTION_NODE
DOCUMENT_NODE = Node.DOCUMENT_NODE

FILTER_ACCEPT = xmlbuilder.DOMBuilderFilter.FILTER_ACCEPT
FILTER_REJECT = xmlbuilder.DOMBuilderFilter.FILTER_REJECT
FILTER_SKIP = xmlbuilder.DOMBuilderFilter.FILTER_SKIP
FILTER_INTERRUPT = xmlbuilder.DOMBuilderFilter.FILTER_INTERRUPT

theDOMImplementation = minidom.getDOMImplementation()

# DTD attribute type -> TypeInfo
_typeinfo_map = {
    name: minidom.TypeInfo(None, info) for name, info in (
        ("CDATA", "cdata"),
        ("ENUM", "enumeration"),
        ("ENTITY", "entity"),
        ("ENTITIES", "entities"),
        ("ID", "id"),
        ("IDREF", "idref"),
        ("IDREFS", "idrefs"),
        ("NMTOKEN", "nmtoken"),
        ("NMTOKENS", "nmtokens"),
    )}

# XML whitespace (str.strip() would also remove other spaces)
_XML_SPACE = " \t\r\n"


class ElementInfo:
    """An element type's declarations from the DTD: its content model and
    its attribute list."""

    __slots__ = '_attr_info', '_model', 'tagName'

    def __init__(self, tagName, model=None):
        self.tagName = tagName
        # one entry per ATTLIST attribute: [.., name, .., default, .., type,
        # required]; the name is at 1 and the type at -2
        self._attr_info = []
        self._model = model

    def __getstate__(self):
        return self._attr_info, self._model, self.tagName

    def __setstate__(self, state):
        self._attr_info, self._model, self.tagName = state

    def _declared_type(self, aname):
        for entry in self._attr_info:
            if entry[1] == aname:
                return entry[-2]
        return None

    def getAttributeType(self, aname):
        declared = self._declared_type(aname)
        if declared is None:
            return minidom._no_type
        if declared.startswith("("):
            return _typeinfo_map["ENUM"]
        return _typeinfo_map[declared]

    def getAttributeTypeNS(self, namespaceURI, localName):
        return minidom._no_type

    def isElementContent(self):
        if not self._model:
            return False
        return self._model[0] not in (expat.model.XML_CTYPE_ANY,
                                      expat.model.XML_CTYPE_MIXED)

    def isEmpty(self):
        return bool(self._model) and \
            self._model[0] == expat.model.XML_CTYPE_EMPTY

    def isId(self, aname):
        return self._declared_type(aname) == "ID"

    def isIdNS(self, euri, ename, auri=None, aname=None):
        # namespaced DTD attributes are not tracked
        return False


def _intern(builder, s):
    return builder._intern_setdefault(s, s)


def _parse_ns_name(builder, name):
    """Split an expat "uri local [prefix]" name into (uri, local name,
    prefix, qualified name)."""
    assert ' ' in name
    intern = builder._intern_setdefault
    parts = name.split(' ')
    if len(parts) == 3:
        uri, local, prefix = parts
        prefix = intern(prefix, prefix)
        qname = prefix + ":" + local
        qname = intern(qname, qname)
        local = intern(local, local)
    elif len(parts) == 2:
        uri, local = parts
        prefix = EMPTY_PREFIX
        qname = local = intern(local, local)
    else:
        raise ValueError("Unsupported syntax: spaces in URIs not supported: %r"
                         % name)
    return intern(uri, uri), local, prefix, qname


class ParseEscape(Exception):
    """Stops a parse early (a filter interrupted, or enough was seen)."""


#
# Filters
#

_ALLOWED_FILTERS = (FILTER_ACCEPT, FILTER_REJECT, FILTER_SKIP,
                    FILTER_INTERRUPT)

_SHOW_FLAG = {
    Node.ELEMENT_NODE: NodeFilter.SHOW_ELEMENT,
    Node.ATTRIBUTE_NODE: NodeFilter.SHOW_ATTRIBUTE,
    Node.TEXT_NODE: NodeFilter.SHOW_TEXT,
    Node.CDATA_SECTION_NODE: NodeFilter.SHOW_CDATA_SECTION,
    Node.ENTITY_REFERENCE_NODE: NodeFilter.SHOW_ENTITY_REFERENCE,
    Node.ENTITY_NODE: NodeFilter.SHOW_ENTITY,
    Node.PROCESSING_INSTRUCTION_NODE: NodeFilter.SHOW_PROCESSING_INSTRUCTION,
    Node.COMMENT_NODE: NodeFilter.SHOW_COMMENT,
    Node.DOCUMENT_NODE: NodeFilter.SHOW_DOCUMENT,
    Node.DOCUMENT_TYPE_NODE: NodeFilter.SHOW_DOCUMENT_TYPE,
    Node.DOCUMENT_FRAGMENT_NODE: NodeFilter.SHOW_DOCUMENT_FRAGMENT,
    Node.NOTATION_NODE: NodeFilter.SHOW_NOTATION,
}


class FilterVisibilityController(object):
    """Consults a DOMBuilderFilter for the node types its whatToShow
    selects (the others are accepted)."""

    __slots__ = 'filter',

    def __init__(self, filter):
        self.filter = filter

    def _ask(self, method, node):
        if not self.filter.whatToShow & _SHOW_FLAG.get(node.nodeType, 0):
            return FILTER_ACCEPT
        verdict = getattr(self.filter, method)(node)
        if verdict == FILTER_INTERRUPT:
            raise ParseEscape
        if verdict not in _ALLOWED_FILTERS:
            raise ValueError("%s() returned illegal value: %r"
                             % (method, verdict))
        return verdict

    def startContainer(self, node):
        return self._ask("startContainer", node)

    def acceptNode(self, node):
        verdict = self._ask("acceptNode", node)
        if verdict == FILTER_SKIP:
            # keep the children in the node's place; the caller drops it
            parent = node.parentNode
            for child in list(node.childNodes):
                parent.appendChild(child)
            return FILTER_REJECT
        return verdict


#
# The builder
#

class ExpatBuilder:
    """Builds a Document from expat events (without namespace support;
    see ExpatBuilderNS)."""

    _namespaces = False

    def __init__(self, options=None):
        if options is None:
            options = xmlbuilder.Options()
        self._options = options
        if options.filter is not None:
            self._filter = FilterVisibilityController(options.filter)
        else:
            self._filter = None
        self._parser = None
        self.reset()

    def createParser(self):
        """A new expat parser."""
        return expat.ParserCreate()

    def getParser(self):
        """The parser, configured and with our handlers installed."""
        if not self._parser:
            self._parser = self.createParser()
            self._intern_setdefault = self._parser.intern.setdefault
            self._parser.buffer_text = True
            self._parser.ordered_attributes = True
            self._parser.specified_attributes = True
            self.install(self._parser)
        return self._parser

    def reset(self):
        """Start over with an empty document."""
        self.document = theDOMImplementation.createDocument(
            EMPTY_NAMESPACE, None, None)
        self.curNode = self.document
        self._elem_info = self.document._elem_info
        self._cdata = False          # inside a CDATA section
        self._cdata_open = False     # its CDATASection node exists
        self._in_subset = False      # inside the DTD's internal subset
        self._ignoring = 0           # depth inside a rejected element
        self._created = []           # per open element: was a node made?
        self._ns_pending = []        # namespace declarations to attach

    def install(self, parser):
        """Connect the handlers the options ask for."""
        options = self._options
        parser.StartDoctypeDeclHandler = self.start_doctype_decl_handler
        parser.EndDoctypeDeclHandler = self.end_doctype_decl_handler
        parser.StartElementHandler = self.start_element_handler
        parser.EndElementHandler = self.end_element_handler
        parser.ProcessingInstructionHandler = self.pi_handler
        if options.entities:
            parser.EntityDeclHandler = self.entity_decl_handler
        parser.NotationDeclHandler = self.notation_decl_handler
        if options.comments:
            parser.CommentHandler = self.comment_handler
        if options.cdata_sections:
            parser.StartCdataSectionHandler = self.start_cdata_section_handler
            parser.EndCdataSectionHandler = self.end_cdata_section_handler
            parser.CharacterDataHandler = self.character_data_handler_cdata
        else:
            parser.CharacterDataHandler = self.character_data_handler
        parser.ExternalEntityRefHandler = self.external_entity_ref_handler
        parser.XmlDeclHandler = self.xml_decl_handler
        parser.ElementDeclHandler = self.element_decl_handler
        parser.AttlistDeclHandler = self.attlist_decl_handler

    def _finish(self):
        doc = self.document
        self.reset()
        self._parser = None
        return doc

    def parseFile(self, file):
        """Parse an open (binary) file into a Document."""
        parser = self.getParser()
        first = True
        try:
            while True:
                chunk = file.read(16 * 1024)
                if not chunk:
                    break
                parser.Parse(chunk, False)
                if first and self.document.documentElement:
                    self._setup_subset(chunk)
                first = False
            parser.Parse(b"", True)
        except ParseEscape:
            pass
        return self._finish()

    def parseString(self, string):
        """Parse a str or bytes into a Document."""
        parser = self.getParser()
        try:
            parser.Parse(string, True)
            self._setup_subset(string)
        except ParseEscape:
            pass
        return self._finish()

    def _setup_subset(self, buffer):
        """Record the doctype's internal subset (as text) from the start of
        the document."""
        if self.document.doctype:
            extractor = InternalSubsetExtractor()
            extractor.parseString(buffer)
            self.document.doctype.internalSubset = extractor.getSubset()

    # -- adding nodes

    def _skipping(self):
        return self._ignoring > 0

    def _add(self, node, sequence=None):
        """Append node (to curNode, or to the list sequence); drop it again
        if the filter rejects it.  Returns whether it was kept."""
        if sequence is None:
            _append_child(self.curNode, node)
        else:
            sequence.append(node)
        if self._filter and self._filter.acceptNode(node) == FILTER_REJECT:
            if sequence is None:
                self.curNode.removeChild(node)
            else:
                sequence.remove(node)
            return False
        return True

    # -- the DTD

    def start_doctype_decl_handler(self, doctypeName, systemId, publicId,
                                   has_internal_subset):
        doc = self.document
        doctype = doc.implementation.createDocumentType(
            doctypeName, publicId, systemId)
        doctype.ownerDocument = doc
        _append_child(doc, doctype)
        doc.doctype = doctype
        if self._filter and self._filter.acceptNode(doctype) == FILTER_REJECT:
            doc.doctype = None
            doc.removeChild(doctype)
            doctype = None
        if has_internal_subset:
            if doctype is not None:
                doctype.entities._seq = []
                doctype.notations._seq = []
            self._in_subset = True

    def end_doctype_decl_handler(self):
        self._in_subset = False

    def entity_decl_handler(self, entityName, is_parameter_entity, value,
                            base, systemId, publicId, notationName):
        doctype = self.document.doctype
        if is_parameter_entity or doctype is None:
            return
        entity = self.document._create_entity(entityName, publicId,
                                              systemId, notationName)
        if value is not None:
            # an internal entity: its replacement text is its child
            text = self.document.createTextNode(value)
            entity.childNodes.append(text)
        self._add(entity, doctype.entities._seq)

    def notation_decl_handler(self, notationName, base, systemId, publicId):
        doctype = self.document.doctype
        if doctype is None:
            return
        notation = self.document._create_notation(notationName, publicId,
                                                  systemId)
        self._add(notation, doctype.notations._seq)

    def element_decl_handler(self, name, model):
        info = self._elem_info.get(name)
        if info is None:
            self._elem_info[name] = ElementInfo(name, model)
        else:
            assert info._model is None
            info._model = model

    def attlist_decl_handler(self, elem, name, type, default, required):
        info = self._elem_info.get(elem)
        if info is None:
            info = self._elem_info[elem] = ElementInfo(elem)
        info._attr_info.append(
            [None, name, None, None, default, 0, type, required])

    def xml_decl_handler(self, version, encoding, standalone):
        doc = self.document
        doc.version = version
        doc.encoding = encoding
        # expat reports -1 when there is no standalone pseudo-attribute
        if standalone >= 0:
            doc.standalone = bool(standalone)

    def external_entity_ref_handler(self, context, base, systemId, publicId):
        # external entities are not loaded
        return 1

    # -- content

    def pi_handler(self, target, data):
        if self._in_subset or self._skipping():
            return
        self._add(self.document.createProcessingInstruction(target, data))

    def comment_handler(self, data):
        if self._in_subset or self._skipping():
            return
        self._add(self.document.createComment(data))

    def start_cdata_section_handler(self):
        self._cdata = True
        self._cdata_open = False

    def end_cdata_section_handler(self):
        self._cdata = False
        self._cdata_open = False

    def character_data_handler_cdata(self, data):
        if self._skipping():
            return
        if not self._cdata:
            return self.character_data_handler(data)
        children = self.curNode.childNodes
        if self._cdata_open and children and \
                children[-1].nodeType == CDATA_SECTION_NODE:
            children[-1].appendData(data)
            return
        self._cdata_open = True
        self._add(self.document.createCDATASection(data))

    def character_data_handler(self, data):
        if self._skipping():
            return
        children = self.curNode.childNodes
        if children and children[-1].nodeType == TEXT_NODE:
            last = children[-1]
            last.data = last.data + data
            return
        text = minidom.Text()
        text.data = data
        text.ownerDocument = self.document
        _append_child(self.curNode, text)

    def _make_element(self, name, attributes):
        element = self.document.createElement(name)
        for i in range(0, len(attributes), 2):
            attr = minidom.Attr(attributes[i], EMPTY_NAMESPACE, None,
                                EMPTY_PREFIX)
            attr.value = attributes[i + 1]
            attr.ownerDocument = self.document
            _set_attribute_node(element, attr)
        return element

    def start_element_handler(self, name, attributes):
        if self._skipping():
            self._ignoring += 1
            return
        element = self._make_element(name, attributes)
        _append_child(self.curNode, element)
        is_root = element is self.document.documentElement
        verdict = FILTER_ACCEPT
        if self._filter and not is_root:
            verdict = self._filter.startContainer(element)
        if verdict == FILTER_ACCEPT:
            self.curNode = element
            self._created.append(True)
            return
        # rejected (with everything inside) or skipped (its content goes
        # into the parent)
        element.parentNode.removeChild(element)
        element.unlink()
        if verdict == FILTER_REJECT:
            self._ignoring = 1
        else:
            self._created.append(False)

    def end_element_handler(self, name):
        if self._skipping():
            self._ignoring -= 1
            return
        if not self._created.pop():
            return
        element = self.curNode
        self.curNode = element.parentNode
        info = self._elem_info.get(element.tagName)
        if info:
            self._drop_ignorable_whitespace(element, info)
        if self._filter and element is not self.document.documentElement:
            if self._filter.acceptNode(element) == FILTER_REJECT:
                self.curNode.removeChild(element)
                element.unlink()

    def _drop_ignorable_whitespace(self, element, info):
        """Remove whitespace-only text from element content, unless the
        options keep it."""
        if (self._options.whitespace_in_element_content
                or not info.isElementContent()):
            return
        for child in list(element.childNodes):
            if child.nodeType == TEXT_NODE and not child.data.strip(_XML_SPACE):
                element.removeChild(child)


#
# Fragments
#

class FragmentBuilder(ExpatBuilder):
    """Parses well-formed content (not a whole document) in the context of
    an existing node: entities declared in the context's document and, for
    the namespace version, the namespace prefixes in scope at the context
    node can be used.  The result is a DocumentFragment of the context's
    document."""

    def __init__(self, context, options=None):
        if context.nodeType == DOCUMENT_NODE:
            self.originalDocument = context
            self.context = context
        else:
            self.originalDocument = context.ownerDocument
            self.context = context
        ExpatBuilder.__init__(self, options)

    def reset(self):
        ExpatBuilder.reset(self)
        self.fragment = None

    def parseFile(self, file):
        """Parse the content of an open file into a DocumentFragment."""
        return self.parseString(file.read())

    def parseString(self, string):
        """Parse a str or bytes into a DocumentFragment."""
        if isinstance(string, bytes):
            string = string.decode("utf-8")
        wrapper = '<!DOCTYPE wrapper [\n%s\n]>\n<wrapper %s\n>%s</wrapper>' % (
            self._getDeclarations(), self._getNSattrs(), string)
        parser = self.getParser()
        try:
            parser.Parse(wrapper, True)
        except ParseEscape:
            pass
        temporary = self.document
        self.reset()
        self._parser = None
        fragment = self.originalDocument.createDocumentFragment()
        wrapper_element = temporary.documentElement
        if wrapper_element is not None:
            for child in list(wrapper_element.childNodes):
                fragment.appendChild(
                    self.originalDocument.importNode(child, True))
        temporary.unlink()
        return fragment

    def _getDeclarations(self):
        """The context document's DTD as an internal subset."""
        doctype = self.originalDocument.doctype
        if doctype is None:
            return ""
        lines = []
        for notation in doctype.notations._seq:
            lines.append("<!NOTATION %s%s>" % (
                notation.nodeName,
                _external_id(notation.publicId, notation.systemId)))
        for entity in doctype.entities._seq:
            if entity.publicId or entity.systemId:
                text = "<!ENTITY %s%s" % (
                    entity.nodeName,
                    _external_id(entity.publicId, entity.systemId))
                if entity.notationName:
                    text += " NDATA " + entity.notationName
                lines.append(text + ">")
            else:
                value = "".join(child.data for child in entity.childNodes)
                lines.append('<!ENTITY %s "%s">' % (
                    entity.nodeName, value.replace('"', "&#34;")))
        return "\n".join(lines)

    def _getNSattrs(self):
        return ""


def _external_id(publicId, systemId):
    if publicId:
        return ' PUBLIC "%s"\n "%s"' % (publicId, systemId or "")
    if systemId:
        return ' SYSTEM "%s"' % systemId
    return ""


#
# Namespace support
#

class Namespaces:
    """Mixin turning a builder into its namespace-aware version."""

    _namespaces = True

    def createParser(self):
        parser = expat.ParserCreate(namespace_separator=" ")
        parser.namespace_prefixes = True
        return parser

    def install(self, parser):
        ExpatBuilder.install(self, parser)
        if self._options.namespace_declarations:
            parser.StartNamespaceDeclHandler = \
                self.start_namespace_decl_handler

    def start_namespace_decl_handler(self, prefix, uri):
        self._ns_pending.append((prefix, uri))

    def _make_element(self, name, attributes):
        doc = self.document
        if ' ' in name:
            uri, local, prefix, qname = _parse_ns_name(self, name)
        else:
            uri, qname, local, prefix = EMPTY_NAMESPACE, name, None, EMPTY_PREFIX
        element = minidom.Element(qname, uri, prefix, local)
        element.ownerDocument = doc
        # the declarations become xmlns attributes, before the others
        for decl_prefix, decl_uri in self._ns_pending:
            if decl_prefix:
                attr = minidom.Attr(_intern(self, 'xmlns:' + decl_prefix),
                                    XMLNS_NAMESPACE, decl_prefix, "xmlns")
            else:
                attr = minidom.Attr("xmlns", XMLNS_NAMESPACE, "xmlns",
                                    EMPTY_PREFIX)
            attr.value = decl_uri
            attr.ownerDocument = doc
            _set_attribute_node(element, attr)
        del self._ns_pending[:]
        if attributes:
            element._ensure_attributes()
            by_name, by_ns = element._attrs, element._attrsNS
            for i in range(0, len(attributes), 2):
                aname = attributes[i]
                if ' ' in aname:
                    uri, local, prefix, qname = _parse_ns_name(self, aname)
                    attr = minidom.Attr(qname, uri, local, prefix)
                else:
                    qname = local = aname
                    uri = EMPTY_NAMESPACE
                    attr = minidom.Attr(aname, EMPTY_NAMESPACE, aname,
                                        EMPTY_PREFIX)
                by_name[qname] = attr
                by_ns[(uri, local)] = attr
                attr.ownerDocument = doc
                attr.value = attributes[i + 1]
                attr.ownerElement = element
        return element


class ExpatBuilderNS(Namespaces, ExpatBuilder):
    """Document builder with namespace support."""


class FragmentBuilderNS(Namespaces, FragmentBuilder):
    """Fragment builder with namespace support."""

    def _getNSattrs(self):
        """xmlns attributes for the prefixes in scope at the context
        node."""
        seen = set()
        attrs = []
        node = self.context
        while node is not None and node.nodeType != DOCUMENT_NODE:
            if node.nodeType == Node.ELEMENT_NODE and node._attrs:
                for attr in node._attrs.values():
                    if attr.namespaceURI != XMLNS_NAMESPACE:
                        continue
                    name = attr.nodeName
                    if name not in seen:
                        seen.add(name)
                        attrs.append('\n    %s="%s"' % (name, attr.value))
            node = node.parentNode
        return "".join(attrs)


#
# The internal subset as text
#

class InternalSubsetExtractor(ExpatBuilder):
    """Reads the start of a document just far enough to return the text
    of its doctype's internal subset (or None)."""

    subset = None

    def getSubset(self):
        return self.subset

    def parseFile(self, file):
        try:
            ExpatBuilder.parseFile(self, file)
        except ParseEscape:
            pass

    def parseString(self, string):
        try:
            ExpatBuilder.parseString(self, string)
        except ParseEscape:
            pass

    def install(self, parser):
        parser.StartDoctypeDeclHandler = self.start_doctype_decl_handler
        parser.StartElementHandler = self.start_element_handler

    def start_doctype_decl_handler(self, name, publicId, systemId,
                                   has_internal_subset):
        if not has_internal_subset:
            raise ParseEscape()
        # with no handler for the declarations, their text goes to the
        # default handler
        pieces = []
        parser = self.getParser()
        parser.DefaultHandler = pieces.append
        parser.EndDoctypeDeclHandler = self.end_doctype_decl_handler
        self.subset = pieces

    def end_doctype_decl_handler(self):
        text = ''.join(self.subset)
        self.subset = text.replace('\r\n', '\n').replace('\r', '\n')
        raise ParseEscape()

    def start_element_handler(self, name, attrs):
        raise ParseEscape()

    def _setup_subset(self, buffer):
        pass


#
# Entry points
#

def _builder(namespaces, fragment_context=None):
    if fragment_context is not None:
        cls = FragmentBuilderNS if namespaces else FragmentBuilder
        return cls(fragment_context)
    return ExpatBuilderNS() if namespaces else ExpatBuilder()


def _parse_file_or_name(builder, file):
    if isinstance(file, str):
        with open(file, 'rb') as fp:
            return builder.parseFile(fp)
    return builder.parseFile(file)


def parse(file, namespaces=True):
    """Parse a file (a path or an open binary file) into a Document."""
    return _parse_file_or_name(_builder(namespaces), file)


def parseString(string, namespaces=True):
    """Parse a document from a str or bytes."""
    return _builder(namespaces).parseString(string)


def parseFragment(file, context, namespaces=True):
    """Parse content from a file (or path) in the context of the node
    context; returns a DocumentFragment."""
    return _parse_file_or_name(_builder(namespaces, context), file)


def parseFragmentString(string, context, namespaces=True):
    """Parse content from a string in the context of the node context;
    returns a DocumentFragment."""
    return _builder(namespaces, context).parseString(string)


def makeBuilder(options):
    """A document builder for the xml.dom.xmlbuilder options."""
    if options.namespaces:
        return ExpatBuilderNS(options)
    return ExpatBuilder(options)
