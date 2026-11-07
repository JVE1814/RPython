"""A small DOM implementation (DOM Level 2 core, parts of Level 3).

parse() and parseString() build a Document from XML (with expat unless
a SAX parser is given); getDOMImplementation() creates empty documents.
"""

import io
import xml.dom

from xml.dom import (EMPTY_NAMESPACE, EMPTY_PREFIX, XMLNS_NAMESPACE, domreg)
from xml.dom.minicompat import *
from xml.dom.xmlbuilder import DOMImplementationLS, DocumentLS

_ELEMENT = xml.dom.Node.ELEMENT_NODE
_ATTRIBUTE = xml.dom.Node.ATTRIBUTE_NODE
_TEXT = xml.dom.Node.TEXT_NODE
_CDATA = xml.dom.Node.CDATA_SECTION_NODE
_ENTITY_REF = xml.dom.Node.ENTITY_REFERENCE_NODE
_PI = xml.dom.Node.PROCESSING_INSTRUCTION_NODE
_COMMENT = xml.dom.Node.COMMENT_NODE
_DOCUMENT = xml.dom.Node.DOCUMENT_NODE
_DOCTYPE = xml.dom.Node.DOCUMENT_TYPE_NODE
_FRAGMENT = xml.dom.Node.DOCUMENT_FRAGMENT_NODE
_NOTATION = xml.dom.Node.NOTATION_NODE

_TEXTUAL = (_TEXT, _CDATA)

# Moving nodes of these types may change which IDs a document has.
_nodeTypes_with_children = (_ELEMENT, _ENTITY_REF)


class Node(xml.dom.Node):
    namespaceURI = None
    parentNode = None
    ownerDocument = None
    nextSibling = None
    previousSibling = None
    prefix = EMPTY_PREFIX

    def __bool__(self):
        # nodes are true even when they have no children
        return True

    # -- serialization

    def toxml(self, encoding=None, standalone=None):
        return self.toprettyxml("", "", encoding, standalone)

    def toprettyxml(self, indent="\t", newl="\n", encoding=None,
                    standalone=None):
        if encoding is None:
            out = io.StringIO()
        else:
            out = io.TextIOWrapper(io.BytesIO(), encoding=encoding,
                                   errors="xmlcharrefreplace", newline='\n')
        if self.nodeType == _DOCUMENT:
            self.writexml(out, "", indent, newl, encoding, standalone)
        else:
            self.writexml(out, "", indent, newl)
        if encoding is None:
            return out.getvalue()
        return out.detach().getvalue()

    # -- the tree

    def hasChildNodes(self):
        return bool(self.childNodes)

    def _get_childNodes(self):
        return self.childNodes

    def _get_firstChild(self):
        children = self.childNodes
        return children[0] if children else None

    def _get_lastChild(self):
        children = self.childNodes
        return children[-1] if children else None

    def _check_child_type(self, node):
        if node.nodeType not in self._child_node_types:
            raise xml.dom.HierarchyRequestErr(
                "%s cannot be child of %s" % (repr(node), repr(self)))

    def insertBefore(self, newChild, refChild):
        if newChild.nodeType == _FRAGMENT:
            for child in list(newChild.childNodes):
                self.insertBefore(child, refChild)
            return newChild
        self._check_child_type(newChild)
        if newChild.parentNode is not None:
            newChild.parentNode.removeChild(newChild)
        if refChild is None:
            return self.appendChild(newChild)
        children = self.childNodes
        position = _position_of(children, refChild)
        if newChild.nodeType in _nodeTypes_with_children:
            _clear_id_cache(self)
        children.insert(position, newChild)
        before = children[position - 1] if position else None
        _join(before, newChild)
        _join(newChild, refChild)
        newChild.parentNode = self
        return newChild

    def appendChild(self, node):
        if node.nodeType == _FRAGMENT:
            for child in list(node.childNodes):
                self.appendChild(child)
            return node
        self._check_child_type(node)
        if node.nodeType in _nodeTypes_with_children:
            _clear_id_cache(self)
        if node.parentNode is not None:
            node.parentNode.removeChild(node)
        _append_child(self, node)
        node.nextSibling = None
        return node

    def replaceChild(self, newChild, oldChild):
        if newChild.nodeType == _FRAGMENT:
            following = oldChild.nextSibling
            self.removeChild(oldChild)
            return self.insertBefore(newChild, following)
        self._check_child_type(newChild)
        if newChild is oldChild:
            return
        if newChild.parentNode is not None:
            newChild.parentNode.removeChild(newChild)
        children = self.childNodes
        position = _position_of(children, oldChild)
        children[position] = newChild
        newChild.parentNode = self
        oldChild.parentNode = None
        if (newChild.nodeType in _nodeTypes_with_children
                or oldChild.nodeType in _nodeTypes_with_children):
            _clear_id_cache(self)
        before, after = oldChild.previousSibling, oldChild.nextSibling
        oldChild.previousSibling = oldChild.nextSibling = None
        newChild.previousSibling = newChild.nextSibling = None
        _join(before, newChild)
        _join(newChild, after)
        return oldChild

    def removeChild(self, oldChild):
        children = self.childNodes
        children.pop(_position_of(children, oldChild))
        _join(oldChild.previousSibling, oldChild.nextSibling)
        oldChild.previousSibling = oldChild.nextSibling = None
        if oldChild.nodeType in _nodeTypes_with_children:
            _clear_id_cache(self)
        oldChild.parentNode = None
        return oldChild

    def normalize(self):
        """Merge adjacent text nodes and drop empty ones, in the whole
        subtree."""
        kept = []
        for child in self.childNodes:
            if child.nodeType == _TEXT:
                if not child.data:
                    _join(kept[-1] if kept else None, child.nextSibling)
                    child.unlink()
                    continue
                if kept and kept[-1].nodeType == _TEXT:
                    survivor = kept[-1]
                    survivor.data = survivor.data + child.data
                    _join(survivor, child.nextSibling)
                    child.unlink()
                    continue
            elif child.nodeType == _ELEMENT:
                child.normalize()
            kept.append(child)
        if kept:
            kept[0].previousSibling = None
            kept[-1].nextSibling = None
        self.childNodes[:] = kept

    def cloneNode(self, deep):
        return _clone_node(self, deep, self.ownerDocument or self)

    # -- features and identity

    def isSupported(self, feature, version):
        return self.ownerDocument.implementation.hasFeature(feature, version)

    def _get_localName(self):
        # only elements and attributes have one
        return None

    def isSameNode(self, other):
        return self is other

    def getInterface(self, feature):
        return self if self.isSupported(feature, None) else None

    # -- user data (DOM Level 3)

    def getUserData(self, key):
        entry = getattr(self, '_user_data', {}).get(key)
        return None if entry is None else entry[0]

    def setUserData(self, key, data, handler):
        store = self.__dict__.setdefault('_user_data', {})
        previous = store.get(key)
        if data is None:
            store.pop(key, None)
        else:
            store[key] = (data, handler)
        return None if previous is None else previous[0]

    def _call_user_data_handler(self, operation, src, dst):
        store = getattr(self, '_user_data', None)
        if not store:
            return
        for key, (data, handler) in list(store.items()):
            if handler is not None:
                handler.handle(operation, key, data, src, dst)

    # -- tear-down

    def unlink(self):
        """Break the references between this node and the rest of the
        tree (and below), so that it can be freed without the cycle
        collector."""
        self.parentNode = self.ownerDocument = None
        if self.childNodes:
            for child in self.childNodes:
                child.unlink()
            self.childNodes = NodeList()
        self.previousSibling = None
        self.nextSibling = None

    def __enter__(self):
        return self

    def __exit__(self, et, ev, tb):
        self.unlink()


defproperty(Node, "firstChild", doc="First child node, or None.")
defproperty(Node, "lastChild", doc="Last child node, or None.")
defproperty(Node, "localName", doc="Namespace-local name of this node.")


def _join(first, second):
    """Make first and second (either may be None) siblings."""
    if first is not None:
        first.nextSibling = second
    if second is not None:
        second.previousSibling = first


def _position_of(children, node):
    for position, child in enumerate(children):
        if child is node:
            return position
    raise xml.dom.NotFoundErr()


def _append_child(self, node):
    """Append node to self's children without any checks."""
    children = self.childNodes
    if children:
        _join(children[-1], node)
    children.append(node)
    node.parentNode = self


def _in_document(node):
    """True if node is (below) a Document."""
    while node is not None:
        if node.nodeType == _DOCUMENT:
            return True
        node = node.parentNode
    return False


def _write_data(writer, text, attr):
    """Write text, escaping markup characters (and, in attribute values,
    quotes and whitespace that a parser would normalize)."""
    if not text:
        return
    text = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    if attr:
        for char, ref in (('"', "&quot;"), ("\r", "&#13;"), ("\n", "&#10;"),
                          ("\t", "&#9;")):
            text = text.replace(char, ref)
    writer.write(text)


def _descendant_elements(parent):
    """Elements below parent, in document order."""
    pending = [iter(parent.childNodes)]
    while pending:
        for node in pending[-1]:
            if node.nodeType == _ELEMENT:
                yield node
                pending.append(iter(node.childNodes))
                break
        else:
            pending.pop()


def _get_elements_by_tagName_helper(parent, name, rc):
    rc.extend(node for node in _descendant_elements(parent)
              if name == "*" or node.tagName == name)
    return rc


def _get_elements_by_tagName_ns_helper(parent, nsURI, localName, rc):
    rc.extend(node for node in _descendant_elements(parent)
              if (localName == "*" or node.localName == localName)
              and (nsURI == "*" or node.namespaceURI == nsURI))
    return rc


class DocumentFragment(Node):
    nodeType = Node.DOCUMENT_FRAGMENT_NODE
    nodeName = "#document-fragment"
    nodeValue = None
    attributes = None
    parentNode = None
    _child_node_types = (_ELEMENT, _TEXT, _CDATA, _ENTITY_REF, _PI, _COMMENT,
                         _NOTATION)

    def __init__(self):
        self.childNodes = NodeList()


class Attr(Node):
    __slots__ = ('_name', '_value', 'namespaceURI', '_prefix', 'childNodes',
                 '_localName', 'ownerDocument', 'ownerElement')
    nodeType = Node.ATTRIBUTE_NODE
    attributes = None
    specified = False
    _is_id = False

    _child_node_types = (_TEXT, _ENTITY_REF)

    def __init__(self, qName, namespaceURI=EMPTY_NAMESPACE, localName=None,
                 prefix=None):
        self.ownerElement = None
        self.ownerDocument = None
        self._name = qName
        self.namespaceURI = namespaceURI
        self._prefix = prefix
        if localName is not None:
            self._localName = localName
        # the value is also available as the one Text child
        self.childNodes = NodeList()
        self.childNodes.append(Text())

    def _owner_changed(self):
        if self.ownerElement is not None:
            _clear_id_cache(self.ownerElement)

    def _get_localName(self):
        try:
            return self._localName
        except AttributeError:
            return self.nodeName.split(":", 1)[-1]

    def _get_specified(self):
        return self.specified

    def _get_name(self):
        return self._name

    def _set_name(self, value):
        self._name = value
        self._owner_changed()

    nodeName = name = property(_get_name, _set_name)

    def _get_value(self):
        return self._value

    def _set_value(self, value):
        self._value = value
        self.childNodes[0].data = value
        self._owner_changed()

    nodeValue = value = property(_get_value, _set_value)

    def _get_prefix(self):
        return self._prefix

    def _set_prefix(self, prefix):
        if prefix == "xmlns":
            uri = self.namespaceURI
            if uri and uri != XMLNS_NAMESPACE:
                raise xml.dom.NamespaceErr(
                    "illegal use of 'xmlns' prefix for the wrong namespace")
        self._prefix = prefix
        local = self.localName
        self._owner_changed()
        self.name = local if prefix is None else "%s:%s" % (prefix, local)

    prefix = property(_get_prefix, _set_prefix)

    def unlink(self):
        owner = self.ownerElement
        if owner is not None:
            del owner._attrs[self.nodeName]
            del owner._attrsNS[(self.namespaceURI, self.localName)]
            if self._is_id:
                self._is_id = False
                owner._magic_id_nodes -= 1
                self.ownerDocument._magic_id_count -= 1
        for child in self.childNodes:
            child.unlink()
        del self.childNodes[:]

    def _element_info(self):
        doc, owner = self.ownerDocument, self.ownerElement
        if doc is None or owner is None:
            return None
        return doc._get_elem_info(owner)

    def _get_isId(self):
        if self._is_id:
            return True
        info = self._element_info()
        if info is None:
            return False
        if self.namespaceURI:
            return info.isIdNS(self.namespaceURI, self.localName)
        return info.isId(self.nodeName)

    def _get_schemaType(self):
        info = self._element_info()
        if info is None:
            return _no_type
        if self.namespaceURI:
            return info.getAttributeTypeNS(self.namespaceURI, self.localName)
        return info.getAttributeType(self.nodeName)


defproperty(Attr, "isId", doc="True if this attribute is an ID.")
defproperty(Attr, "localName", doc="Namespace-local name of this attribute.")
defproperty(Attr, "schemaType", doc="Schema type for this attribute.")


class NamedNodeMap(object):
    """The attributes of an element, by name and by (namespace, local
    name).  It shares the element's dictionaries."""

    __slots__ = ('_attrs', '_attrsNS', '_ownerElement')

    def __init__(self, attrs, attrsNS, ownerElement):
        self._attrs = attrs
        self._attrsNS = attrsNS
        self._ownerElement = ownerElement

    def _get_length(self):
        return len(self._attrs)

    __len__ = _get_length

    def item(self, index):
        names = list(self._attrs)
        if 0 <= index < len(names):
            return self._attrs[names[index]]
        if index < 0:
            try:
                return self._attrs[names[index]]
            except IndexError:
                return None
        return None

    def items(self):
        return [(node.nodeName, node.value) for node in self._attrs.values()]

    def itemsNS(self):
        return [((node.namespaceURI, node.localName), node.value)
                for node in self._attrs.values()]

    def __contains__(self, key):
        if isinstance(key, str):
            return key in self._attrs
        return key in self._attrsNS

    def keys(self):
        return self._attrs.keys()

    def keysNS(self):
        return self._attrsNS.keys()

    def values(self):
        return self._attrs.values()

    def get(self, name, value=None):
        return self._attrs.get(name, value)

    # maps of the same element compare equal; others by identity
    def _cmp(self, other):
        if self._attrs is getattr(other, "_attrs", None):
            return 0
        return (id(self) > id(other)) - (id(self) < id(other))

    def __eq__(self, other):
        return self._cmp(other) == 0

    def __ge__(self, other):
        return self._cmp(other) >= 0

    def __gt__(self, other):
        return self._cmp(other) > 0

    def __le__(self, other):
        return self._cmp(other) <= 0

    def __lt__(self, other):
        return self._cmp(other) < 0

    __hash__ = None

    def __getitem__(self, attname_or_tuple):
        if isinstance(attname_or_tuple, tuple):
            return self._attrsNS[attname_or_tuple]
        return self._attrs[attname_or_tuple]

    def __setitem__(self, attname, value):
        if isinstance(value, str):
            node = self._attrs.get(attname)
            if node is None:
                node = Attr(attname)
                node.ownerDocument = self._ownerElement.ownerDocument
                self.setNamedItem(node)
            node.value = value
        elif isinstance(value, Attr):
            self.setNamedItem(value)
        else:
            raise TypeError("value must be a string or Attr object")

    def getNamedItem(self, name):
        return self._attrs.get(name)

    def getNamedItemNS(self, namespaceURI, localName):
        return self._attrsNS.get((namespaceURI, localName))

    def _remove(self, node):
        if node is None:
            raise xml.dom.NotFoundErr()
        _clear_id_cache(self._ownerElement)
        del self._attrs[node.nodeName]
        del self._attrsNS[(node.namespaceURI, node.localName)]
        node.ownerElement = None
        return node

    def removeNamedItem(self, name):
        return self._remove(self.getNamedItem(name))

    def removeNamedItemNS(self, namespaceURI, localName):
        return self._remove(self.getNamedItemNS(namespaceURI, localName))

    def setNamedItem(self, node):
        if not isinstance(node, Attr):
            raise xml.dom.HierarchyRequestErr(
                "%s cannot be child of %s" % (repr(node), repr(self)))
        previous = self._attrs.get(node.name)
        if previous:
            previous.unlink()
        self._attrs[node.name] = node
        self._attrsNS[(node.namespaceURI, node.localName)] = node
        node.ownerElement = self._ownerElement
        _clear_id_cache(node.ownerElement)
        return previous

    setNamedItemNS = setNamedItem

    def __delitem__(self, attname_or_tuple):
        node = self[attname_or_tuple]
        _clear_id_cache(node.ownerElement)
        node.unlink()

    def __getstate__(self):
        return self._attrs, self._attrsNS, self._ownerElement

    def __setstate__(self, state):
        self._attrs, self._attrsNS, self._ownerElement = state


defproperty(NamedNodeMap, "length",
            doc="Number of nodes in the NamedNodeMap.")

AttributeList = NamedNodeMap


class TypeInfo(object):
    __slots__ = 'namespace', 'name'

    def __init__(self, namespace, name):
        self.namespace = namespace
        self.name = name

    def __repr__(self):
        if self.namespace:
            return "<%s %r (from %r)>" % (type(self).__name__, self.name,
                                          self.namespace)
        return "<%s %r>" % (type(self).__name__, self.name)

    def _get_name(self):
        return self.name

    def _get_namespace(self):
        return self.namespace


_no_type = TypeInfo(None, None)


class Element(Node):
    __slots__ = ('ownerDocument', 'parentNode', 'tagName', 'nodeName',
                 'prefix', 'namespaceURI', '_localName', 'childNodes',
                 '_attrs', '_attrsNS', 'nextSibling', 'previousSibling')
    nodeType = Node.ELEMENT_NODE
    nodeValue = None
    schemaType = _no_type

    # attributes of this element made IDs with setIdAttribute*()
    _magic_id_nodes = 0

    _child_node_types = (_ELEMENT, _PI, _COMMENT, _TEXT, _CDATA, _ENTITY_REF)

    def __init__(self, tagName, namespaceURI=EMPTY_NAMESPACE, prefix=None,
                 localName=None):
        self.ownerDocument = None
        self.parentNode = None
        self.nextSibling = None
        self.previousSibling = None
        self.tagName = tagName
        self.nodeName = tagName
        self.prefix = prefix
        self.namespaceURI = namespaceURI
        self.childNodes = NodeList()
        # the attribute dictionaries are created on first use
        self._attrs = None
        self._attrsNS = None
        if localName is not None:
            self._localName = localName

    def _ensure_attributes(self):
        if self._attrs is None:
            self._attrs = {}
            self._attrsNS = {}

    def _get_localName(self):
        try:
            return self._localName
        except AttributeError:
            return self.tagName.split(":", 1)[-1]

    def _get_tagName(self):
        return self.tagName

    def unlink(self):
        if self._attrs is not None:
            for attr in list(self._attrs.values()):
                attr.unlink()
        self._attrs = None
        self._attrsNS = None
        Node.unlink(self)

    # -- attributes by name

    def getAttribute(self, attname):
        node = self.getAttributeNode(attname)
        return "" if node is None else node.value

    def getAttributeNS(self, namespaceURI, localName):
        node = self.getAttributeNodeNS(namespaceURI, localName)
        return "" if node is None else node.value

    def setAttribute(self, attname, value):
        node = self.getAttributeNode(attname)
        if node is None:
            node = Attr(attname)
            node.value = value
            node.ownerDocument = self.ownerDocument
            self.setAttributeNode(node)
        elif value != node.value:
            node.value = value
            if node.isId:
                _clear_id_cache(self)

    def setAttributeNS(self, namespaceURI, qualifiedName, value):
        prefix, local = _nssplit(qualifiedName)
        node = self.getAttributeNodeNS(namespaceURI, local)
        if node is None:
            node = Attr(qualifiedName, namespaceURI, local, prefix)
            node.value = value
            node.ownerDocument = self.ownerDocument
            self.setAttributeNode(node)
            return
        if value != node.value:
            node.value = value
            if node.isId:
                _clear_id_cache(self)
        if node.prefix != prefix:
            node.prefix = prefix
            node.nodeName = qualifiedName

    def getAttributeNode(self, attrname):
        if self._attrs is None:
            return None
        return self._attrs.get(attrname)

    def getAttributeNodeNS(self, namespaceURI, localName):
        if self._attrsNS is None:
            return None
        return self._attrsNS.get((namespaceURI, localName))

    def setAttributeNode(self, attr):
        if attr.ownerElement not in (None, self):
            raise xml.dom.InuseAttributeErr("attribute node already owned")
        self._ensure_attributes()
        by_name = self._attrs.get(attr.name)
        if by_name is not None:
            self.removeAttributeNode(by_name)
        by_ns = self._attrsNS.get((attr.namespaceURI, attr.localName))
        if by_ns is not None and by_ns is not by_name:
            self.removeAttributeNode(by_ns)
        _set_attribute_node(self, attr)
        if by_name is not attr:
            return by_name
        if by_ns is not attr:
            return by_ns

    setAttributeNodeNS = setAttributeNode

    def removeAttribute(self, name):
        node = None if self._attrs is None else self._attrs.get(name)
        if node is None:
            raise xml.dom.NotFoundErr()
        self.removeAttributeNode(node)

    def removeAttributeNS(self, namespaceURI, localName):
        node = self.getAttributeNodeNS(namespaceURI, localName)
        if node is None:
            raise xml.dom.NotFoundErr()
        self.removeAttributeNode(node)

    def removeAttributeNode(self, node):
        if node is None or self._attrs is None or node.name not in self._attrs:
            raise xml.dom.NotFoundErr()
        _clear_id_cache(self)
        node.unlink()
        # unlink() forgot the document; the node is still usable on its own
        node.ownerDocument = self.ownerDocument
        return node

    removeAttributeNodeNS = removeAttributeNode

    def hasAttribute(self, name):
        return self._attrs is not None and name in self._attrs

    def hasAttributeNS(self, namespaceURI, localName):
        return (self._attrsNS is not None
                and (namespaceURI, localName) in self._attrsNS)

    def _get_attributes(self):
        self._ensure_attributes()
        return NamedNodeMap(self._attrs, self._attrsNS, self)

    def hasAttributes(self):
        return bool(self._attrs)

    # -- searching

    def getElementsByTagName(self, name):
        return _get_elements_by_tagName_helper(self, name, NodeList())

    def getElementsByTagNameNS(self, namespaceURI, localName):
        return _get_elements_by_tagName_ns_helper(
            self, namespaceURI, localName, NodeList())

    def __repr__(self):
        return "<DOM Element: %s at %#x>" % (self.tagName, id(self))

    def writexml(self, writer, indent="", addindent="", newl=""):
        """Write the element; indent is this element's indentation,
        addindent the extra indentation of each level below it."""
        writer.write(indent + "<" + self.tagName)
        attrs = self._attrs or {}
        for name, node in attrs.items():
            writer.write(' %s="' % name)
            _write_data(writer, node.value, True)
            writer.write('"')
        children = self.childNodes
        if not children:
            writer.write("/>" + newl)
            return
        writer.write(">")
        if len(children) == 1 and children[0].nodeType in _TEXTUAL:
            # a lone text keeps its exact value
            children[0].writexml(writer, '', '', '')
        else:
            writer.write(newl)
            for node in children:
                node.writexml(writer, indent + addindent, addindent, newl)
            writer.write(indent)
        writer.write("</%s>%s" % (self.tagName, newl))

    # -- IDs (DOM Level 3)

    def setIdAttribute(self, name):
        self.setIdAttributeNode(self.getAttributeNode(name))

    def setIdAttributeNS(self, namespaceURI, localName):
        self.setIdAttributeNode(self.getAttributeNodeNS(namespaceURI, localName))

    def setIdAttributeNode(self, idAttr):
        if idAttr is None or not self.isSameNode(idAttr.ownerElement):
            raise xml.dom.NotFoundErr()
        if _get_containing_entref(self) is not None:
            raise xml.dom.NoModificationAllowedErr()
        if not idAttr._is_id:
            idAttr._is_id = True
            self._magic_id_nodes += 1
            self.ownerDocument._magic_id_count += 1
            _clear_id_cache(self)


defproperty(Element, "attributes",
            doc="NamedNodeMap of attributes on the element.")
defproperty(Element, "localName",
            doc="Namespace-local name of this element.")


def _set_attribute_node(element, attr):
    _clear_id_cache(element)
    element._ensure_attributes()
    element._attrs[attr.name] = attr
    element._attrsNS[(attr.namespaceURI, attr.localName)] = attr
    # a reference cycle, broken by Element.unlink()
    attr.ownerElement = element


class Childless:
    """Mixin for the node types that never have children."""

    attributes = None
    childNodes = EmptyNodeList()

    def _get_firstChild(self):
        return None

    def _get_lastChild(self):
        return None

    def appendChild(self, node):
        raise xml.dom.HierarchyRequestErr(
            self.nodeName + " nodes cannot have children")

    def hasChildNodes(self):
        return False

    def insertBefore(self, newChild, refChild):
        raise xml.dom.HierarchyRequestErr(
            self.nodeName + " nodes do not have children")

    def removeChild(self, oldChild):
        raise xml.dom.NotFoundErr(
            self.nodeName + " nodes do not have children")

    def normalize(self):
        pass

    def replaceChild(self, newChild, oldChild):
        raise xml.dom.HierarchyRequestErr(
            self.nodeName + " nodes do not have children")


class ProcessingInstruction(Childless, Node):
    nodeType = Node.PROCESSING_INSTRUCTION_NODE
    __slots__ = ('target', 'data')

    def __init__(self, target, data):
        self.target = target
        self.data = data

    def _get_nodeValue(self):
        return self.data

    def _set_nodeValue(self, value):
        self.data = value

    nodeValue = property(_get_nodeValue, _set_nodeValue)

    def _get_nodeName(self):
        return self.target

    def _set_nodeName(self, value):
        self.target = value

    nodeName = property(_get_nodeName, _set_nodeName)

    def writexml(self, writer, indent="", addindent="", newl=""):
        writer.write("%s<?%s %s?>%s" % (indent, self.target, self.data, newl))


class CharacterData(Childless, Node):
    __slots__ = ('_data', 'ownerDocument', 'parentNode', 'previousSibling',
                 'nextSibling')

    def __init__(self):
        self.ownerDocument = self.parentNode = None
        self.previousSibling = self.nextSibling = None
        self._data = ''
        Node.__init__(self)

    def _get_length(self):
        return len(self.data)

    __len__ = _get_length

    def _get_data(self):
        return self._data

    def _set_data(self, data):
        self._data = data

    data = nodeValue = property(_get_data, _set_data)

    def __repr__(self):
        data = self.data
        more = "..." if len(data) > 10 else ""
        return '<DOM %s node "%r%s">' % (type(self).__name__, data[:10], more)

    def _check_range(self, offset, count=None):
        if offset < 0:
            raise xml.dom.IndexSizeErr("offset cannot be negative")
        if offset >= len(self.data):
            raise xml.dom.IndexSizeErr("offset cannot be beyond end of data")
        if count is not None and count < 0:
            raise xml.dom.IndexSizeErr("count cannot be negative")

    def substringData(self, offset, count):
        self._check_range(offset, count)
        return self.data[offset:offset + count]

    def appendData(self, arg):
        self.data = self.data + arg

    def insertData(self, offset, arg):
        self._check_range(offset)
        if arg:
            self.data = self.data[:offset] + arg + self.data[offset:]

    def deleteData(self, offset, count):
        self._check_range(offset, count)
        if count:
            self.data = self.data[:offset] + self.data[offset + count:]

    def replaceData(self, offset, count, arg):
        self._check_range(offset, count)
        if count:
            self.data = self.data[:offset] + arg + self.data[offset + count:]


defproperty(CharacterData, "length", doc="Length of the string data.")


class Text(CharacterData):
    __slots__ = ()

    nodeType = Node.TEXT_NODE
    nodeName = "#text"
    attributes = None

    def splitText(self, offset):
        if offset < 0 or offset > len(self.data):
            raise xml.dom.IndexSizeErr("illegal offset value")
        tail = type(self)()
        tail.data = self.data[offset:]
        tail.ownerDocument = self.ownerDocument
        parent = self.parentNode
        if parent is not None and any(c is self for c in parent.childNodes):
            following = self.nextSibling
            if following is None:
                parent.appendChild(tail)
            else:
                parent.insertBefore(tail, following)
        self.data = self.data[:offset]
        return tail

    def writexml(self, writer, indent="", addindent="", newl=""):
        _write_data(writer, "%s%s%s" % (indent, self.data, newl), False)

    def _textual_neighbours(self, attribute):
        node = getattr(self, attribute)
        while node is not None and node.nodeType in _TEXTUAL:
            yield node
            node = getattr(node, attribute)

    def _get_wholeText(self):
        before = [n.data for n in self._textual_neighbours('previousSibling')]
        after = [n.data for n in self._textual_neighbours('nextSibling')]
        return ''.join(before[::-1]) + self.data + ''.join(after)

    def replaceWholeText(self, content):
        parent = self.parentNode
        for node in list(self._textual_neighbours('previousSibling')):
            parent.removeChild(node)
        following = list(self._textual_neighbours('nextSibling'))
        if not content:
            parent.removeChild(self)
        for node in following:
            parent.removeChild(node)
        if content:
            self.data = content
            return self
        return None

    def _get_isWhitespaceInElementContent(self):
        # XML whitespace is only these four characters
        if self.data.strip(" \t\r\n"):
            return False
        element = _get_containing_element(self)
        if element is None:
            return False
        info = self.ownerDocument._get_elem_info(element)
        return info is not None and info.isElementContent()


defproperty(Text, "isWhitespaceInElementContent",
            doc="True iff this text node contains only whitespace"
                " and is in element content.")
defproperty(Text, "wholeText",
            doc="The text of all logically-adjacent text nodes.")


def _nearest_ancestor(node, node_type):
    parent = node.parentNode
    while parent is not None:
        if parent.nodeType == node_type:
            return parent
        parent = parent.parentNode
    return None


def _get_containing_element(node):
    return _nearest_ancestor(node, _ELEMENT)


def _get_containing_entref(node):
    return _nearest_ancestor(node, _ENTITY_REF)


class Comment(CharacterData):
    nodeType = Node.COMMENT_NODE
    nodeName = "#comment"

    def __init__(self, data):
        CharacterData.__init__(self)
        self._data = data

    def writexml(self, writer, indent="", addindent="", newl=""):
        if "--" in self.data:
            raise ValueError("'--' is not allowed in a comment node")
        writer.write("%s<!--%s-->%s" % (indent, self.data, newl))


class CDATASection(Text):
    __slots__ = ()

    nodeType = Node.CDATA_SECTION_NODE
    nodeName = "#cdata-section"

    def writexml(self, writer, indent="", addindent="", newl=""):
        if "]]>" in self.data:
            raise ValueError("']]>' not allowed in a CDATA section")
        writer.write("<![CDATA[%s]]>" % self.data)


class ReadOnlySequentialNamedNodeMap(object):
    """A read-only NamedNodeMap over a list (a doctype's entities and
    notations)."""

    __slots__ = '_seq',

    def __init__(self, seq=()):
        # seq should be a list or tuple
        self._seq = seq

    def __len__(self):
        return len(self._seq)

    def _get_length(self):
        return len(self._seq)

    def getNamedItem(self, name):
        for node in self._seq:
            if node.nodeName == name:
                return node

    def getNamedItemNS(self, namespaceURI, localName):
        for node in self._seq:
            if node.namespaceURI == namespaceURI and node.localName == localName:
                return node

    def __getitem__(self, name_or_tuple):
        if isinstance(name_or_tuple, tuple):
            node = self.getNamedItemNS(*name_or_tuple)
        else:
            node = self.getNamedItem(name_or_tuple)
        if node is None:
            raise KeyError(name_or_tuple)
        return node

    def item(self, index):
        if 0 <= index < len(self._seq):
            return self._seq[index]
        return None

    def _read_only(self, *args):
        raise xml.dom.NoModificationAllowedErr(
            "NamedNodeMap instance is read-only")

    removeNamedItem = removeNamedItemNS = _read_only
    setNamedItem = setNamedItemNS = _read_only

    def __getstate__(self):
        return [self._seq]

    def __setstate__(self, state):
        self._seq = state[0]


defproperty(ReadOnlySequentialNamedNodeMap, "length",
            doc="Number of entries in the NamedNodeMap.")


class Identified:
    """Mixin for the nodes that have a public and a system identifier."""

    def _identified_mixin_init(self, publicId, systemId):
        self.publicId = publicId
        self.systemId = systemId

    def _get_publicId(self):
        return self.publicId

    def _get_systemId(self):
        return self.systemId


class DocumentType(Identified, Childless, Node):
    nodeType = Node.DOCUMENT_TYPE_NODE
    nodeValue = None
    name = None
    publicId = None
    systemId = None
    internalSubset = None

    def __init__(self, qualifiedName):
        self.entities = ReadOnlySequentialNamedNodeMap()
        self.notations = ReadOnlySequentialNamedNodeMap()
        if qualifiedName:
            self.name = _nssplit(qualifiedName)[1]
        self.nodeName = self.name

    def _get_internalSubset(self):
        return self.internalSubset

    def cloneNode(self, deep):
        # only a doctype that belongs to no document can be cloned this way
        if self.ownerDocument is not None:
            return None
        clone = DocumentType(None)
        clone.name = self.name
        clone.nodeName = self.name
        if deep:
            _copy_declarations(self, clone, None, xml.dom.UserDataHandler.NODE_CLONED)
        self._call_user_data_handler(xml.dom.UserDataHandler.NODE_CLONED,
                                     self, clone)
        return clone

    def writexml(self, writer, indent="", addindent="", newl=""):
        writer.write("<!DOCTYPE ")
        writer.write(self.name)
        if self.publicId:
            writer.write("%s  PUBLIC '%s'%s  '%s'"
                         % (newl, self.publicId, newl, self.systemId))
        elif self.systemId:
            writer.write("%s  SYSTEM '%s'" % (newl, self.systemId))
        if self.internalSubset is not None:
            writer.write(" [")
            writer.write(self.internalSubset)
            writer.write("]")
        writer.write(">" + newl)


def _copy_declarations(source, target, owner, operation):
    """Copy the notations and entities of doctype source into target."""
    target.notations._seq = []
    target.entities._seq = []
    for old in source.notations._seq:
        new = Notation(old.nodeName, old.publicId, old.systemId)
        new.ownerDocument = owner
        target.notations._seq.append(new)
        if hasattr(old, '_call_user_data_handler'):
            old._call_user_data_handler(operation, old, new)
    for old in source.entities._seq:
        new = Entity(old.nodeName, old.publicId, old.systemId,
                     old.notationName)
        new.actualEncoding = old.actualEncoding
        new.encoding = old.encoding
        new.version = old.version
        new.ownerDocument = owner
        target.entities._seq.append(new)
        if hasattr(old, '_call_user_data_handler'):
            old._call_user_data_handler(operation, old, new)


class Entity(Identified, Node):
    attributes = None
    nodeType = Node.ENTITY_NODE
    nodeValue = None

    actualEncoding = None
    encoding = None
    version = None

    def __init__(self, name, publicId, systemId, notation):
        self.nodeName = name
        self.notationName = notation
        self.childNodes = NodeList()
        self._identified_mixin_init(publicId, systemId)

    def _get_actualEncoding(self):
        return self.actualEncoding

    def _get_encoding(self):
        return self.encoding

    def _get_version(self):
        return self.version

    def appendChild(self, newChild):
        raise xml.dom.HierarchyRequestErr(
            "cannot append children to an entity node")

    def insertBefore(self, newChild, refChild):
        raise xml.dom.HierarchyRequestErr(
            "cannot insert children below an entity node")

    def removeChild(self, oldChild):
        raise xml.dom.HierarchyRequestErr(
            "cannot remove children from an entity node")

    def replaceChild(self, newChild, oldChild):
        raise xml.dom.HierarchyRequestErr(
            "cannot replace children of an entity node")


class Notation(Identified, Childless, Node):
    nodeType = Node.NOTATION_NODE
    nodeValue = None

    def __init__(self, name, publicId, systemId):
        self.nodeName = name
        self._identified_mixin_init(publicId, systemId)


class DOMImplementation(DOMImplementationLS):
    _features = [("core", "1.0"),
                 ("core", "2.0"),
                 ("core", None),
                 ("xml", "1.0"),
                 ("xml", "2.0"),
                 ("xml", None),
                 ("ls-load", "3.0"),
                 ("ls-load", None),
                 ]

    def hasFeature(self, feature, version):
        if version == "":
            version = None
        return (feature.lower(), version) in self._features

    def createDocument(self, namespaceURI, qualifiedName, doctype):
        if doctype and doctype.parentNode is not None:
            raise xml.dom.WrongDocumentErr(
                "doctype object owned by another DOM tree")
        doc = self._create_document()

        # createDocument(None, None, None) makes a document without a root
        # element (used for cloning and by the builders)
        with_root = not (namespaceURI is None and qualifiedName is None
                         and doctype is None)
        if with_root and not qualifiedName:
            # the spec does not say what to raise here
            raise xml.dom.InvalidCharacterErr("Element with no name")

        if with_root:
            prefix, _local = _nssplit(qualifiedName)
            if (prefix == "xml"
                    and namespaceURI != "http://www.w3.org/XML/1998/namespace"):
                raise xml.dom.NamespaceErr("illegal use of 'xml' prefix")
            if prefix and not namespaceURI:
                raise xml.dom.NamespaceErr(
                    "illegal use of prefix without namespaces")
            root = doc.createElementNS(namespaceURI, qualifiedName)
            if doctype:
                doc.appendChild(doctype)
            doc.appendChild(root)

        if doctype:
            doctype.parentNode = doctype.ownerDocument = doc

        doc.doctype = doctype
        doc.implementation = self
        return doc

    def createDocumentType(self, qualifiedName, publicId, systemId):
        doctype = DocumentType(qualifiedName)
        doctype.publicId = publicId
        doctype.systemId = systemId
        return doctype

    def getInterface(self, feature):
        return self if self.hasFeature(feature, None) else None

    def _create_document(self):
        return Document()


class ElementInfo(object):
    """What a document knows about an element type (from its DTD).  This
    base version knows nothing."""

    __slots__ = 'tagName',

    def __init__(self, name):
        self.tagName = name

    def getAttributeType(self, aname):
        return _no_type

    def getAttributeTypeNS(self, namespaceURI, localName):
        return _no_type

    def isElementContent(self):
        return False

    def isEmpty(self):
        """True if the element type is declared EMPTY."""
        return False

    def isId(self, aname):
        """True if the attribute aname is declared of type ID."""
        return False

    def isIdNS(self, namespaceURI, localName):
        """True if the attribute (namespaceURI, localName) is declared of
        type ID."""
        return False

    def __getstate__(self):
        return self.tagName

    def __setstate__(self, state):
        self.tagName = state


def _clear_id_cache(node):
    """Forget the getElementById() results of node's document (a cache:
    clearing it more often than needed is harmless, so this does not check
    whether node really is in the document)."""
    if node is None:
        return
    doc = node if node.nodeType == _DOCUMENT else node.ownerDocument
    if doc is not None:
        doc._id_cache.clear()
        doc._id_search_stack = None


class Document(Node, DocumentLS):
    __slots__ = ('_elem_info', 'doctype',
                 '_id_search_stack', 'childNodes', '_id_cache')
    _child_node_types = (_ELEMENT, _PI, _COMMENT, _DOCTYPE)

    implementation = DOMImplementation()
    nodeType = Node.DOCUMENT_NODE
    nodeName = "#document"
    nodeValue = None
    attributes = None
    parentNode = None
    previousSibling = nextSibling = None

    # DOM Level 3 document properties
    actualEncoding = None
    encoding = None
    standalone = None
    version = None
    strictErrorChecking = False
    errorHandler = None
    documentURI = None

    # attributes made IDs with setIdAttribute*() in this document
    _magic_id_count = 0

    def __init__(self):
        self.doctype = None
        self.childNodes = NodeList()
        # tagName or (namespaceURI, localName) -> ElementInfo
        self._elem_info = {}
        # ID value -> element, filled as getElementById() searches
        self._id_cache = {}
        # elements getElementById() has yet to look at (None: start over)
        self._id_search_stack = None

    def _get_elem_info(self, element):
        # a DTD does not know namespaces: declarations are found by the
        # qualified name
        return self._elem_info.get(element.tagName)

    def _get_actualEncoding(self):
        return self.actualEncoding

    def _get_doctype(self):
        return self.doctype

    def _get_documentURI(self):
        return self.documentURI

    def _get_encoding(self):
        return self.encoding

    def _get_errorHandler(self):
        return self.errorHandler

    def _get_standalone(self):
        return self.standalone

    def _get_strictErrorChecking(self):
        return self.strictErrorChecking

    def _get_version(self):
        return self.version

    def appendChild(self, node):
        self._check_child_type(node)
        if node.parentNode is not None:
            # the node may be moved from somewhere in this very document
            node.parentNode.removeChild(node)
        if node.nodeType == _ELEMENT and self._get_documentElement():
            raise xml.dom.HierarchyRequestErr(
                "two document elements disallowed")
        return Node.appendChild(self, node)

    def removeChild(self, oldChild):
        children = self.childNodes
        children.pop(_position_of(children, oldChild))
        _join(oldChild.previousSibling, oldChild.nextSibling)
        oldChild.nextSibling = oldChild.previousSibling = None
        oldChild.parentNode = None
        return oldChild

    def _get_documentElement(self):
        for node in self.childNodes:
            if node.nodeType == _ELEMENT:
                return node

    def unlink(self):
        if self.doctype is not None:
            self.doctype.unlink()
            self.doctype = None
        Node.unlink(self)

    def cloneNode(self, deep):
        if not deep:
            return None
        clone = self.implementation.createDocument(None, None, None)
        clone.encoding = self.encoding
        clone.standalone = self.standalone
        clone.version = self.version
        for child in self.childNodes:
            copy = _clone_node(child, deep, clone)
            _append_child(clone, copy)
            if copy.nodeType == _DOCTYPE:
                clone.doctype = copy
        self._call_user_data_handler(xml.dom.UserDataHandler.NODE_CLONED,
                                     self, clone)
        return clone

    # -- factories

    def _owned(self, node):
        node.ownerDocument = self
        return node

    def createDocumentFragment(self):
        return self._owned(DocumentFragment())

    def createElement(self, tagName):
        element = Element(tagName)
        element.ownerDocument = self
        return element

    def _text_node(self, cls, data):
        if not isinstance(data, str):
            raise TypeError("node contents must be a string")
        node = cls()
        node.data = data
        return self._owned(node)

    def createTextNode(self, data):
        if not isinstance(data, str):
            raise TypeError("node contents must be a string")
        text = Text()
        text._data = data
        text.ownerDocument = self
        return text

    def createCDATASection(self, data):
        return self._text_node(CDATASection, data)

    def createComment(self, data):
        return self._owned(Comment(data))

    def createProcessingInstruction(self, target, data):
        return self._owned(ProcessingInstruction(target, data))

    def createAttribute(self, qName):
        attr = self._owned(Attr(qName))
        attr.value = ""
        return attr

    def createElementNS(self, namespaceURI, qualifiedName):
        prefix, _local = _nssplit(qualifiedName)
        return self._owned(Element(qualifiedName, namespaceURI, prefix))

    def createAttributeNS(self, namespaceURI, qualifiedName):
        prefix, local = _nssplit(qualifiedName)
        attr = self._owned(Attr(qualifiedName, namespaceURI, local, prefix))
        attr.value = ""
        return attr

    # entities and notations only come from a DTD: these are for the
    # builders

    def _create_entity(self, name, publicId, systemId, notationName):
        return self._owned(Entity(name, publicId, systemId, notationName))

    def _create_notation(self, name, publicId, systemId):
        return self._owned(Notation(name, publicId, systemId))

    # -- searching

    def _id_values(self, element):
        """The values of element's ID attributes."""
        info = self._get_elem_info(element)
        if info is None and not element._magic_id_nodes:
            return ()
        values = []
        for attr in element.attributes.values():
            if attr._is_id:
                values.append(attr.value)
            elif info is None:
                continue
            elif attr.namespaceURI:
                if info.isIdNS(attr.namespaceURI, attr.localName):
                    values.append(attr.value)
            elif info.isId(attr.name):
                values.append(attr.value)
        return values

    def getElementById(self, id):
        cache = self._id_cache
        if id in cache:
            return cache[id]
        if not (self._elem_info or self._magic_id_count):
            return None
        pending = self._id_search_stack
        if pending is None:
            # nothing searched since the cache was last cleared
            root = self.documentElement
            pending = self._id_search_stack = [] if root is None else [root]
        # resume the search where the last one stopped
        while pending:
            element = pending.pop()
            pending.extend(child for child in reversed(element.childNodes)
                           if child.nodeType in _nodeTypes_with_children)
            if element.nodeType != _ELEMENT:
                continue
            found = False
            for value in self._id_values(element):
                cache.setdefault(value, element)
                found = found or value == id
            if found:
                return element
        return None

    def getElementsByTagName(self, name):
        return _get_elements_by_tagName_helper(self, name, NodeList())

    def getElementsByTagNameNS(self, namespaceURI, localName):
        return _get_elements_by_tagName_ns_helper(
            self, namespaceURI, localName, NodeList())

    def isSupported(self, feature, version):
        return self.implementation.hasFeature(feature, version)

    def importNode(self, node, deep):
        if node.nodeType == _DOCUMENT:
            raise xml.dom.NotSupportedErr("cannot import document nodes")
        if node.nodeType == _DOCTYPE:
            raise xml.dom.NotSupportedErr("cannot import document type nodes")
        return _clone_node(node, deep, self)

    def writexml(self, writer, indent="", addindent="", newl="", encoding=None,
                 standalone=None):
        pseudo = []
        if encoding:
            pseudo.append(f'encoding="{encoding}"')
        if standalone is not None:
            pseudo.append(f'standalone="{"yes" if standalone else "no"}"')
        writer.write(f'<?xml version="1.0" {" ".join(pseudo)}?>{newl}')
        for node in self.childNodes:
            node.writexml(writer, indent, addindent, newl)

    # DOM Level 3 (WD 9 April 2002)

    def renameNode(self, n, namespaceURI, name):
        if n.ownerDocument is not self:
            raise xml.dom.WrongDocumentErr(
                "cannot rename nodes from other documents;\n"
                "expected %s,\nfound %s" % (self, n.ownerDocument))
        if n.nodeType not in (_ELEMENT, _ATTRIBUTE):
            raise xml.dom.NotSupportedErr(
                "renameNode() only applies to element and attribute nodes")
        if namespaceURI == EMPTY_NAMESPACE:
            prefix = local = None
        elif ':' in name:
            prefix, local = name.split(':', 1)
            if prefix == "xmlns" and namespaceURI != xml.dom.XMLNS_NAMESPACE:
                raise xml.dom.NamespaceErr("illegal use of 'xmlns' prefix")
        else:
            if (name == "xmlns" and namespaceURI != xml.dom.XMLNS_NAMESPACE
                    and n.nodeType == _ATTRIBUTE):
                raise xml.dom.NamespaceErr(
                    "illegal use of the 'xmlns' attribute")
            prefix, local = None, name
        owner = None
        if n.nodeType == _ATTRIBUTE:
            owner = n.ownerElement
            if owner is not None:
                was_id = n._is_id
                owner.removeAttributeNode(n)
        n.prefix = prefix
        n._localName = local
        n.namespaceURI = namespaceURI
        n.nodeName = name
        if n.nodeType == _ELEMENT:
            n.tagName = name
        else:
            n.name = name
            if owner is not None:
                owner.setAttributeNode(n)
                if was_id:
                    owner.setIdAttributeNode(n)
        # the node is reused, not replaced: no NODE_RENAMED notification
        return n


defproperty(Document, "documentElement",
            doc="Top-level element of this document.")


def _clone_node(node, deep, newOwnerDocument):
    """Clone node (and its subtree if deep) for newOwnerDocument."""
    if node.ownerDocument.isSameNode(newOwnerDocument):
        operation = xml.dom.UserDataHandler.NODE_CLONED
    else:
        operation = xml.dom.UserDataHandler.NODE_IMPORTED
    kind = node.nodeType
    doc = newOwnerDocument
    if kind == _ELEMENT:
        clone = doc.createElementNS(node.namespaceURI, node.nodeName)
        for attr in node.attributes.values():
            clone.setAttributeNS(attr.namespaceURI, attr.nodeName, attr.value)
            copy = clone.getAttributeNodeNS(attr.namespaceURI, attr.localName)
            copy.specified = attr.specified
    elif kind == _FRAGMENT:
        clone = doc.createDocumentFragment()
    elif kind == _TEXT:
        clone = doc.createTextNode(node.data)
    elif kind == _CDATA:
        clone = doc.createCDATASection(node.data)
    elif kind == _PI:
        clone = doc.createProcessingInstruction(node.target, node.data)
    elif kind == _COMMENT:
        clone = doc.createComment(node.data)
    elif kind == _ATTRIBUTE:
        clone = doc.createAttributeNS(node.namespaceURI, node.nodeName)
        clone.specified = True
        clone.value = node.value
    elif kind == _DOCTYPE:
        operation = xml.dom.UserDataHandler.NODE_IMPORTED
        clone = doc.implementation.createDocumentType(
            node.name, node.publicId, node.systemId)
        clone.ownerDocument = doc
        if deep:
            _copy_declarations(node, clone, doc, operation)
    else:
        # documents and doctypes clone themselves (cloneNode())
        raise xml.dom.NotSupportedErr("Cannot clone node %s" % repr(node))
    if deep and kind in (_ELEMENT, _FRAGMENT):
        for child in node.childNodes:
            clone.appendChild(_clone_node(child, deep, doc))
    if hasattr(node, '_call_user_data_handler'):
        node._call_user_data_handler(operation, node, clone)
    return clone


def _nssplit(qualifiedName):
    prefix, colon, local = qualifiedName.partition(':')
    if colon:
        return [prefix, local]
    return (None, qualifiedName)


def _do_pulldom_parse(func, args, kwargs):
    events = func(*args, **kwargs)
    _token, root = events.getEvent()
    events.expandNode(root)
    events.clear()
    return root


def parse(file, parser=None, bufsize=None):
    """Parse a file (a name or an open file) into a Document."""
    if parser is None and not bufsize:
        from xml.dom import expatbuilder
        return expatbuilder.parse(file)
    from xml.dom import pulldom
    return _do_pulldom_parse(pulldom.parse, (file,),
                             {'parser': parser, 'bufsize': bufsize})


def parseString(string, parser=None):
    """Parse a string into a Document."""
    if parser is None:
        from xml.dom import expatbuilder
        return expatbuilder.parseString(string)
    from xml.dom import pulldom
    return _do_pulldom_parse(pulldom.parseString, (string,),
                             {'parser': parser})


def getDOMImplementation(features=None):
    if features:
        if isinstance(features, str):
            features = domreg._parse_feature_string(features)
        for feature, version in features:
            if not Document.implementation.hasFeature(feature, version):
                return None
    return Document.implementation
