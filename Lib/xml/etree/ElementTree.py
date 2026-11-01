"""Lightweight XML support: the Element type and ElementTree wrapper,
parsing (XMLParser, TreeBuilder, iterparse, XMLPullParser) and
serialization (tostring, ElementTree.write, canonicalize).
"""

__all__ = [
    "C14NWriterTarget",
    "Comment",
    "dump",
    "Element", "ElementTree",
    "canonicalize",
    "fromstring", "fromstringlist",
    "indent", "iselement", "iterparse",
    "parse", "ParseError",
    "PI", "ProcessingInstruction",
    "QName",
    "SubElement",
    "tostring", "tostringlist", "TreeBuilder",
    "XML", "XMLID",
    "XMLParser", "XMLPullParser",
    "register_namespace",
]

_VERSION = "1.3.0"

import sys
import re
import warnings
import io
import collections
import collections.abc
import contextlib
import weakref

from . import ElementPath


class ParseError(SyntaxError):
    """An error while parsing XML (also has 'code' and 'position')."""


def iselement(element):
    """Return True if *element* looks like an Element."""
    return hasattr(element, 'tag')


# mappings accepted as the attrib argument
try:
    _ATTRIB_TYPES = (dict, frozendict)
except NameError:
    _ATTRIB_TYPES = (dict,)


class Element:
    """An XML element: a tag, a dict of attributes, text, tail and a list
    of child elements."""

    tag = None
    attrib = None
    text = None
    tail = None

    def __init__(self, tag, /, attrib={}, **extra):
        if not isinstance(attrib, _ATTRIB_TYPES):
            raise TypeError("attrib must be dict or frozendict, not %s"
                            % (attrib.__class__.__name__,))
        self.tag = tag
        self.attrib = {**attrib, **extra}
        self._children = []

    def __repr__(self):
        return "<%s %r at %#x>" % (self.__class__.__name__, self.tag, id(self))

    def makeelement(self, tag, attrib, /):
        """Create a new element of the same type."""
        return self.__class__(tag, attrib)

    def __copy__(self):
        elem = self.makeelement(self.tag, self.attrib)
        elem.text = self.text
        elem.tail = self.tail
        elem[:] = self
        return elem

    def __len__(self):
        return len(self._children)

    def __bool__(self):
        warnings.warn(
            "Testing an element's truth value will always return True in "
            "future versions.  "
            "Use specific 'len(elem)' or 'elem is not None' test instead.",
            DeprecationWarning, stacklevel=2
        )
        return len(self._children) != 0

    def __getitem__(self, index):
        return self._children[index]

    def __setitem__(self, index, element):
        if isinstance(index, slice):
            for elt in element:
                self._assert_is_element(elt)
        else:
            self._assert_is_element(element)
        self._children[index] = element

    def __delitem__(self, index):
        del self._children[index]

    def append(self, subelement, /):
        """Add *subelement* at the end."""
        self._assert_is_element(subelement)
        self._children.append(subelement)

    def extend(self, elements, /):
        """Append all elements of an iterable."""
        for element in elements:
            self._assert_is_element(element)
            self._children.append(element)

    def insert(self, index, subelement, /):
        """Insert *subelement* at *index*."""
        self._assert_is_element(subelement)
        self._children.insert(index, subelement)

    def _assert_is_element(self, e):
        if not isinstance(e, _Element_Py):
            raise TypeError('expected an Element, not %s' % type(e).__name__)

    def remove(self, subelement, /):
        """Remove the first occurrence of *subelement*; ValueError if it is
        not a child."""
        if not isinstance(subelement, Element):
            raise TypeError("remove() argument must be xml.etree.ElementTree.Element, not %s"
                            % type(subelement).__name__)
        children = self._children
        for i, child in enumerate(children):
            if child is subelement or child == subelement:
                # comparisons may have mutated the list; never fail here
                del children[i:i + 1]
                return
        raise ValueError(f"{subelement!r} not in {self!r}")

    def find(self, path, namespaces=None):
        """The first matching subelement, or None."""
        return ElementPath.find(self, path, namespaces)

    def findtext(self, path, default=None, namespaces=None):
        """The text of the first matching subelement."""
        return ElementPath.findtext(self, path, default, namespaces)

    def findall(self, path, namespaces=None):
        """All matching subelements, in document order."""
        return ElementPath.findall(self, path, namespaces)

    def iterfind(self, path, namespaces=None):
        """Iterate over the matching subelements."""
        return ElementPath.iterfind(self, path, namespaces)

    def clear(self):
        """Remove all subelements, attributes, text and tail."""
        self.attrib.clear()
        self._children = []
        self.text = self.tail = None

    def get(self, key, default=None):
        """An attribute value, or *default*."""
        return self.attrib.get(key, default)

    def set(self, key, value, /):
        """Set an attribute."""
        self.attrib[key] = value

    def keys(self):
        return self.attrib.keys()

    def items(self):
        return self.attrib.items()

    def iter(self, tag=None):
        """Iterate over this element and all elements below it (with the
        given tag; None or '*' means all)."""
        if tag == "*":
            tag = None
        if tag is None or self.tag == tag:
            yield self
        for child in self._children:
            yield from child.iter(tag)

    def itertext(self):
        """Iterate over the text content (text and tails) of the tree."""
        tag = self.tag
        if not isinstance(tag, str) and tag is not None:
            return
        text = self.text
        if text:
            yield text
        for child in self:
            yield from child.itertext()
            tail = child.tail
            if tail:
                yield tail


def SubElement(parent, tag, /, attrib={}, **extra):
    """Create an element and append it to *parent*."""
    attrib = {**attrib, **extra}
    element = parent.makeelement(tag, attrib)
    parent.append(element)
    return element


def Comment(text=None):
    """A comment element."""
    element = Element(Comment)
    element.text = text
    return element


def ProcessingInstruction(target, text=None):
    """A processing instruction element."""
    element = Element(ProcessingInstruction)
    element.text = target
    if text:
        element.text = element.text + " " + text
    return element


PI = ProcessingInstruction


class QName:
    """A qualified name: QName(uri, local) or QName('{uri}local')."""

    def __init__(self, text_or_uri, tag=None):
        if tag:
            text_or_uri = "{%s}%s" % (text_or_uri, tag)
        self.text = text_or_uri

    def __str__(self):
        return self.text

    def __repr__(self):
        return '<%s %r>' % (self.__class__.__name__, self.text)

    def __hash__(self):
        return hash(self.text)

    def _other(self, other):
        return other.text if isinstance(other, QName) else other

    def __le__(self, other):
        return self.text <= self._other(other)

    def __lt__(self, other):
        return self.text < self._other(other)

    def __ge__(self, other):
        return self.text >= self._other(other)

    def __gt__(self, other):
        return self.text > self._other(other)

    def __eq__(self, other):
        return self.text == self._other(other)


# --------------------------------------------------------------------


class ElementTree:
    """A document: wraps a root element and adds parsing and writing."""

    def __init__(self, element=None, file=None):
        if element is not None and not iselement(element):
            raise TypeError('expected an Element, not %s'
                            % type(element).__name__)
        self._root = element
        if file:
            self.parse(file)

    def getroot(self):
        return self._root

    def _setroot(self, element):
        if not iselement(element):
            raise TypeError('expected an Element, not %s'
                            % type(element).__name__)
        self._root = element

    def parse(self, source, parser=None):
        """Load the tree from a file name or file object."""
        close_source = False
        if not hasattr(source, "read"):
            source = open(source, "rb")
            close_source = True
        try:
            if parser is None:
                parser = XMLParser()
                whole = getattr(parser, '_parse_whole', None)
                if whole is not None:
                    # the accelerated parser reads the file itself
                    self._root = whole(source)
                    return self._root
            while True:
                data = source.read(65536)
                if not data:
                    break
                parser.feed(data)
            self._root = parser.close()
            return self._root
        finally:
            if close_source:
                source.close()

    def iter(self, tag=None):
        return self._root.iter(tag)

    def _check_path(self, path):
        if path[:1] == "/":
            path = "." + path
            warnings.warn(
                "This search is broken in 1.3 and earlier, and will be "
                "fixed in a future version.  If you rely on the current "
                "behaviour, change it to %r" % path,
                FutureWarning, stacklevel=3
            )
        return path

    def find(self, path, namespaces=None):
        return self._root.find(self._check_path(path), namespaces)

    def findtext(self, path, default=None, namespaces=None):
        return self._root.findtext(self._check_path(path), default, namespaces)

    def findall(self, path, namespaces=None):
        return self._root.findall(self._check_path(path), namespaces)

    def iterfind(self, path, namespaces=None):
        return self._root.iterfind(self._check_path(path), namespaces)

    def write(self, file_or_filename,
              encoding=None,
              xml_declaration=None,
              default_namespace=None,
              method=None, *,
              short_empty_elements=True):
        """Write the tree to a file (name or file object).

        encoding: 'unicode' for text output, otherwise a codec name
        (default us-ascii); xml_declaration: None means only for encodings
        other than us-ascii, utf-8 and unicode; method: 'xml', 'html',
        'text' or 'c14n'.
        """
        if self._root is None:
            raise TypeError('ElementTree not initialized')
        if not method:
            method = "xml"
        elif method not in _serialize:
            raise ValueError("unknown method %r" % method)
        if not encoding:
            encoding = "utf-8" if method == "c14n" else "us-ascii"
        with _get_writer(file_or_filename, encoding) as (write, declared_encoding):
            if method == "xml" and (xml_declaration or
                    (xml_declaration is None and
                     encoding.lower() != "unicode" and
                     declared_encoding.lower() not in ("utf-8", "us-ascii"))):
                write("<?xml version='1.0' encoding='%s'?>\n" % (
                    declared_encoding,))
            if method == "text":
                _serialize_text(write, self._root)
            else:
                qnames, namespaces = _namespaces(self._root, default_namespace)
                serialize = _serialize[method]
                serialize(write, self._root, qnames, namespaces,
                          short_empty_elements=short_empty_elements)

    def write_c14n(self, file):
        return self.write(file, method="c14n")


@contextlib.contextmanager
def _get_writer(file_or_filename, encoding):
    # Yields (write function, encoding to declare).
    try:
        write = file_or_filename.write
    except AttributeError:
        if encoding.lower() == "unicode":
            encoding = "utf-8"
        with open(file_or_filename, "w", encoding=encoding,
                  errors="xmlcharrefreplace") as file:
            yield file.write, encoding
        return
    if encoding.lower() == "unicode":
        yield write, getattr(file_or_filename, "encoding", None) or "utf-8"
        return
    with contextlib.ExitStack() as stack:
        if isinstance(file_or_filename, io.BufferedIOBase):
            file = file_or_filename
        elif isinstance(file_or_filename, io.RawIOBase):
            file = io.BufferedWriter(file_or_filename)
            stack.callback(file.detach)
        else:
            # A file-like object with a write method that takes bytes.
            file = io.BufferedIOBase()
            file.writable = lambda: True
            file.write = write
            try:
                file.seekable = file_or_filename.seekable
                file.tell = file_or_filename.tell
            except AttributeError:
                pass
        file = io.TextIOWrapper(file, encoding=encoding,
                                errors="xmlcharrefreplace", newline="\n")
        stack.callback(file.detach)
        yield file.write, encoding


def _namespaces(elem, default_namespace=None):
    # Map tags/attribute names to their serialized forms and collect the
    # namespace declarations needed on the root.
    qnames = {None: None}
    namespaces = {}
    if default_namespace:
        namespaces[default_namespace] = ""

    def add_qname(qname):
        try:
            if qname[:1] == "{":
                uri, tag = qname[1:].rsplit("}", 1)
                prefix = namespaces.get(uri)
                if prefix is None:
                    prefix = _namespace_map.get(uri)
                    if prefix is None:
                        prefix = "ns%d" % len(namespaces)
                    if prefix != "xml":
                        namespaces[uri] = prefix
                if prefix:
                    qnames[qname] = "%s:%s" % (prefix, tag)
                else:
                    qnames[qname] = tag
            else:
                if default_namespace:
                    raise ValueError(
                        "cannot use non-qualified names with "
                        "default_namespace option"
                    )
                qnames[qname] = qname
        except TypeError:
            _raise_serialization_error(qname)

    for elem in elem.iter():
        tag = elem.tag
        if isinstance(tag, QName):
            if tag.text not in qnames:
                add_qname(tag.text)
        elif isinstance(tag, str):
            if tag not in qnames:
                add_qname(tag)
        elif tag is not None and tag is not Comment and tag is not PI:
            _raise_serialization_error(tag)
        for key, value in elem.items():
            if isinstance(key, QName):
                key = key.text
            if key not in qnames:
                add_qname(key)
            if isinstance(value, QName) and value.text not in qnames:
                add_qname(value.text)
        text = elem.text
        if isinstance(text, QName) and text.text not in qnames:
            add_qname(text.text)
    return qnames, namespaces


def _serialize_xml(write, elem, qnames, namespaces,
                   short_empty_elements, **kwargs):
    tag = elem.tag
    text = elem.text
    if tag is Comment:
        write("<!--%s-->" % text)
    elif tag is ProcessingInstruction:
        write("<?%s?>" % text)
    else:
        tag = qnames[tag]
        if tag is None:
            if text:
                write(_escape_cdata(text))
            for e in elem:
                _serialize_xml(write, e, qnames, None,
                               short_empty_elements=short_empty_elements)
        else:
            write("<" + tag)
            if namespaces:
                for uri, prefix in sorted(namespaces.items(),
                                          key=lambda x: x[1]):
                    if prefix:
                        prefix = ":" + prefix
                    write(" xmlns%s=\"%s\"" % (prefix, _escape_attrib(uri)))
            for key, value in elem.items():
                if isinstance(key, QName):
                    key = key.text
                if isinstance(value, QName):
                    value = qnames[value.text]
                else:
                    value = _escape_attrib(value)
                write(" %s=\"%s\"" % (qnames[key], value))
            if text or len(elem) or not short_empty_elements:
                write(">")
                if text:
                    write(_escape_cdata(text))
                for e in elem:
                    _serialize_xml(write, e, qnames, None,
                                   short_empty_elements=short_empty_elements)
                write("</" + tag + ">")
            else:
                write(" />")
    if elem.tail:
        write(_escape_cdata(elem.tail))


HTML_EMPTY = {"area", "base", "basefont", "br", "col", "embed", "frame", "hr",
              "img", "input", "isindex", "link", "meta", "param", "source",
              "track", "wbr"}


_HTML_RAW_TEXT = {"script", "style", "xmp", "iframe", "noembed", "noframes",
                  "plaintext"}


def _serialize_html(write, elem, qnames, namespaces, **kwargs):
    tag = elem.tag
    text = elem.text
    if tag is Comment:
        write("<!--%s-->" % text)
    elif tag is ProcessingInstruction:
        write("<?%s?>" % text)
    else:
        tag = qnames[tag]
        if tag is None:
            if text:
                write(_escape_cdata(text))
            for e in elem:
                _serialize_html(write, e, qnames, None)
        else:
            write("<" + tag)
            if namespaces:
                for uri, prefix in sorted(namespaces.items(),
                                          key=lambda x: x[1]):
                    if prefix:
                        prefix = ":" + prefix
                    write(" xmlns%s=\"%s\"" % (prefix, _escape_attrib(uri)))
            for key, value in elem.items():
                if isinstance(key, QName):
                    key = key.text
                if value is None:
                    # an attribute without a value (HTML only)
                    write(" %s" % qnames[key])
                    continue
                if isinstance(value, QName):
                    value = qnames[value.text]
                else:
                    value = _escape_attrib_html(value)
                write(" %s=\"%s\"" % (qnames[key], value))
            write(">")
            ltag = tag.lower()
            if text:
                if ltag in _HTML_RAW_TEXT:
                    write(text)
                else:
                    write(_escape_cdata(text))
            for e in elem:
                _serialize_html(write, e, qnames, None)
            if ltag not in HTML_EMPTY and ltag != "plaintext":
                write("</" + tag + ">")
    if elem.tail:
        write(_escape_cdata(elem.tail))


def _serialize_text(write, elem):
    for part in elem.itertext():
        write(part)
    if elem.tail:
        write(elem.tail)


def _serialize_c14n(write, elem, qnames, namespaces, **kwargs):
    raise ValueError("canonicalization via ElementTree.write is not supported; "
                     "use canonicalize()")


_serialize = {
    "xml": _serialize_xml,
    "html": _serialize_html,
    "text": _serialize_text,
}


def register_namespace(prefix, uri):
    """Register a prefix to use for *uri* when serializing."""
    if re.match(r"ns\d+$", prefix):
        raise ValueError("Prefix format reserved for internal use")
    for k, v in list(_namespace_map.items()):
        if k == uri or v == prefix:
            del _namespace_map[k]
    _namespace_map[uri] = prefix


_namespace_map = {
    "http://www.w3.org/XML/1998/namespace": "xml",
    "http://www.w3.org/1999/xhtml": "html",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#": "rdf",
    "http://schemas.xmlsoap.org/wsdl/": "wsdl",
    "http://www.w3.org/2001/XMLSchema": "xs",
    "http://www.w3.org/2001/XMLSchema-instance": "xsi",
    "http://purl.org/dc/elements/1.1/": "dc",
}
register_namespace._namespace_map = _namespace_map


def _raise_serialization_error(text):
    raise TypeError(
        "cannot serialize %r (type %s)" % (text, type(text).__name__)
    )


def _escape_cdata(text):
    try:
        if "&" in text:
            text = text.replace("&", "&amp;")
        if "<" in text:
            text = text.replace("<", "&lt;")
        if ">" in text:
            text = text.replace(">", "&gt;")
        return text
    except (TypeError, AttributeError):
        _raise_serialization_error(text)


def _escape_attrib(text):
    try:
        if "&" in text:
            text = text.replace("&", "&amp;")
        if "<" in text:
            text = text.replace("<", "&lt;")
        if ">" in text:
            text = text.replace(">", "&gt;")
        if "\"" in text:
            text = text.replace("\"", "&quot;")
        if "\r" in text:
            text = text.replace("\r", "&#13;")
        if "\n" in text:
            text = text.replace("\n", "&#10;")
        if "\t" in text:
            text = text.replace("\t", "&#09;")
        return text
    except (TypeError, AttributeError):
        _raise_serialization_error(text)


def _escape_attrib_html(text):
    try:
        if "&" in text:
            text = text.replace("&", "&amp;")
        if ">" in text:
            text = text.replace(">", "&gt;")
        if "\"" in text:
            text = text.replace("\"", "&quot;")
        return text
    except (TypeError, AttributeError):
        _raise_serialization_error(text)


def tostring(element, encoding=None, method=None, *,
             xml_declaration=None, default_namespace=None,
             short_empty_elements=True):
    """Serialize *element* (with its subtree); a str if encoding is
    'unicode', bytes otherwise."""
    stream = io.StringIO() if encoding == 'unicode' else io.BytesIO()
    ElementTree(element).write(stream, encoding,
                               xml_declaration=xml_declaration,
                               default_namespace=default_namespace,
                               method=method,
                               short_empty_elements=short_empty_elements)
    return stream.getvalue()


class _ListDataStream(io.BufferedIOBase):
    """A write-only stream collecting written chunks in a list."""

    def __init__(self, lst):
        self.lst = lst

    def writable(self):
        return True

    def seekable(self):
        return True

    def write(self, b):
        self.lst.append(b)

    def tell(self):
        return len(self.lst)


def tostringlist(element, encoding=None, method=None, *,
                 xml_declaration=None, default_namespace=None,
                 short_empty_elements=True):
    """Like tostring(), but return a list of string chunks."""
    lst = []
    stream = _ListDataStream(lst)
    ElementTree(element).write(stream, encoding,
                               xml_declaration=xml_declaration,
                               default_namespace=default_namespace,
                               method=method,
                               short_empty_elements=short_empty_elements)
    return lst


def dump(elem):
    """Write an element tree to sys.stdout (for debugging)."""
    if not isinstance(elem, ElementTree):
        elem = ElementTree(elem)
    elem.write(sys.stdout, encoding="unicode")
    tail = elem.getroot().tail
    if not tail or tail[-1] != "\n":
        sys.stdout.write("\n")


def indent(tree, space="  ", level=0):
    """Add whitespace (text and tails) so that the tree is pretty-printed
    with *space* per nesting level."""
    if isinstance(tree, ElementTree):
        tree = tree.getroot()
    if level < 0:
        raise ValueError(f"Initial indentation level must be >= 0, got {level}")
    if not len(tree):
        return
    indentations = ["\n" + level * space]

    def walk(elem, level):
        child_level = level + 1
        try:
            child_indent = indentations[child_level]
        except IndexError:
            child_indent = indentations[level] + space
            indentations.append(child_indent)
        if not elem.text or not elem.text.strip(" \t\r\n"):
            elem.text = child_indent
        for child in elem:
            if len(child):
                walk(child, child_level)
            if not child.tail or not child.tail.strip(" \t\r\n"):
                child.tail = child_indent
        if not child.tail.strip(" \t\r\n"):
            child.tail = indentations[level]

    walk(tree, 0)


# --------------------------------------------------------------------
# parsing

def parse(source, parser=None):
    """Parse a file (name or object) into an ElementTree."""
    tree = ElementTree()
    tree.parse(source, parser)
    return tree


def iterparse(source, events=None, parser=None):
    """Parse incrementally, yielding (event, element) pairs.

    *events* is a sequence of 'start', 'end', 'comment', 'pi', 'start-ns'
    and 'end-ns' (default: 'end' only).  The iterator has a 'root'
    attribute once parsing is complete and a close() method.
    """
    return _IterParseIterator(source, events, parser)


class _IterParseState:
    __slots__ = ('root',)

    def __init__(self):
        self.root = None


def _iterparse_events(pull, source, owns_source, state):
    # A free function, so the generator holds no reference to the iterator
    # object: dropping the iterator finalizes it immediately.
    try:
        while True:
            yield from pull.read_events()
            data = source.read(16 * 1024)
            if not data:
                break
            pull.feed(data)
        root = pull._close_and_return_root()
        yield from pull.read_events()
        state.root = root
    finally:
        if owns_source:
            source.close()


class _IterParseIterator(collections.abc.Iterator):

    def __init__(self, source, events, parser):
        self._owns_source = False
        self._closed = False
        # validate the events before opening anything
        pull = XMLPullParser(events=events, _parser=parser)
        owns_source = not hasattr(source, "read")
        if owns_source:
            source = open(source, "rb")
        self._owns_source = owns_source
        self._source = source
        self._pull = pull
        self._state = _IterParseState()
        self._gen = _iterparse_events(pull, source, owns_source, self._state)

    @property
    def root(self):
        return self._state.root

    def __next__(self):
        return next(self._gen)

    def close(self):
        self._closed = True
        self._gen.close()
        if self._owns_source:
            self._source.close()

    def __del__(self):
        if self._owns_source and not self._closed:
            name = getattr(self._source, 'name', self._source)
            warnings.warn(f"unclosed iterparse iterator {name!r}",
                          ResourceWarning, stacklevel=2)
            self.close()


class XMLPullParser:
    """A non-blocking parser: feed() data, then read_events()."""

    def __init__(self, events=None, *, _parser=None):
        self._events_queue = collections.deque()
        self._parser = _parser or XMLParser(target=TreeBuilder())
        if events is None:
            events = ("end",)
        self._parser._setevents(self._events_queue, events)

    def feed(self, data):
        """Feed encoded data to the parser."""
        if self._parser is None:
            raise ValueError("feed() called after end of stream")
        if data:
            try:
                self._parser.feed(data)
            except SyntaxError as exc:
                self._events_queue.append(exc)

    def _close_and_return_root(self):
        root = self._parser.close()
        self._parser = None
        return root

    def close(self):
        """Finish feeding data."""
        self._close_and_return_root()

    def read_events(self):
        """Iterate over the (event, element) pairs available so far."""
        events = self._events_queue
        while events:
            event = events.popleft()
            if isinstance(event, Exception):
                raise event
            yield event

    def flush(self):
        """Parse any buffered data now (disabling reparse deferral)."""
        if self._parser is None:
            raise ValueError("flush() called after end of stream")
        self._parser.flush()


def XML(text, parser=None):
    """Parse an XML document from a string; return the root element."""
    if not parser:
        parser = XMLParser(target=TreeBuilder())
    parser.feed(text)
    return parser.close()


def XMLID(text, parser=None):
    """Parse a document; return (root, {id attribute: element})."""
    if not parser:
        parser = XMLParser(target=TreeBuilder())
    parser.feed(text)
    tree = parser.close()
    ids = {}
    for elem in tree.iter():
        id = elem.get("id")
        if id:
            ids[id] = elem
    return tree, ids


fromstring = XML


def fromstringlist(sequence, parser=None):
    """Parse a document given as a sequence of string fragments."""
    if not parser:
        parser = XMLParser(target=TreeBuilder())
    for text in sequence:
        parser.feed(text)
    return parser.close()


class TreeBuilder:
    """Build an element tree from start/end/data/comment/pi calls."""

    def __init__(self, element_factory=None, *,
                 comment_factory=None, pi_factory=None,
                 insert_comments=False, insert_pis=False):
        self._data = []
        self._elem = []
        self._last = None
        self._root = None
        self._tail = None
        if comment_factory is None:
            comment_factory = Comment
        self._comment_factory = comment_factory
        self.insert_comments = insert_comments
        if pi_factory is None:
            pi_factory = ProcessingInstruction
        self._pi_factory = pi_factory
        self.insert_pis = insert_pis
        if element_factory is None:
            element_factory = Element
        self._factory = element_factory

    def close(self):
        """Return the root element."""
        assert len(self._elem) == 0, "missing end tags"
        assert self._root is not None, "missing toplevel element"
        return self._root

    def _flush(self):
        if self._data:
            if self._last is not None:
                text = "".join(self._data)
                if self._tail:
                    assert self._last.tail is None, "internal error (tail)"
                    self._last.tail = text
                else:
                    assert self._last.text is None, "internal error (text)"
                    self._last.text = text
            self._data = []

    def data(self, data):
        """Add text to the current element."""
        self._data.append(data)

    def start(self, tag, attrs):
        """Open an element."""
        self._flush()
        self._last = elem = self._factory(tag, attrs)
        if self._elem:
            self._elem[-1].append(elem)
        elif self._root is None:
            self._root = elem
        self._elem.append(elem)
        self._tail = 0
        return elem

    def end(self, tag):
        """Close the current element."""
        self._flush()
        self._last = self._elem.pop()
        assert self._last.tag == tag,\
               "end tag mismatch (expected %s, got %s)" % (
                   self._last.tag, tag)
        self._tail = 1
        return self._last

    def comment(self, text):
        """Create a comment (inserted into the tree if insert_comments)."""
        return self._handle_single(
            self._comment_factory, self.insert_comments, text)

    def pi(self, target, text=None):
        """Create a processing instruction (inserted if insert_pis)."""
        return self._handle_single(
            self._pi_factory, self.insert_pis, target, text)

    def _handle_single(self, factory, insert, *args):
        elem = factory(*args)
        if insert:
            self._flush()
            self._last = elem
            if self._elem:
                self._elem[-1].append(elem)
            self._tail = 1
        return elem


class XMLParser:
    """An expat based parser feeding a target (by default a TreeBuilder)."""

    def __init__(self, *, target=None, encoding=None):
        try:
            from xml.parsers import expat
        except ImportError:
            try:
                import pyexpat as expat
            except ImportError:
                raise ImportError(
                    "No module named expat; use SimpleXMLTreeBuilder instead"
                )
        parser = expat.ParserCreate(encoding, "}")
        if target is None:
            target = TreeBuilder()
        self.parser = self._parser = parser
        self.target = self._target = target
        self._error = expat.error
        self._names = {}
        parser.DefaultHandlerExpand = self._default
        if hasattr(target, 'start'):
            parser.StartElementHandler = self._start
        if hasattr(target, 'end'):
            parser.EndElementHandler = self._end
        if hasattr(target, 'start_ns'):
            parser.StartNamespaceDeclHandler = self._start_ns
        if hasattr(target, 'end_ns'):
            parser.EndNamespaceDeclHandler = self._end_ns
        if hasattr(target, 'data'):
            parser.CharacterDataHandler = target.data
        if hasattr(target, 'comment'):
            parser.CommentHandler = target.comment
        if hasattr(target, 'pi'):
            parser.ProcessingInstructionHandler = target.pi
        parser.StartDoctypeDeclHandler = self._start_doctype
        parser.buffer_text = 1
        parser.ordered_attributes = 1
        self._doctype = None
        self.entity = {}
        try:
            self.version = "Expat %d.%d.%d" % expat.version_info
        except AttributeError:
            pass

    def _setevents(self, events_queue, events_to_report):
        parser = self._parser
        append = events_queue.append
        for event_name in events_to_report:
            if event_name == "start":
                parser.ordered_attributes = 1

                def handler(tag, attrib_in, event=event_name, append=append,
                            start=self._start):
                    append((event, start(tag, attrib_in)))
                parser.StartElementHandler = handler
            elif event_name == "end":
                def handler(tag, event=event_name, append=append,
                            end=self._end):
                    append((event, end(tag)))
                parser.EndElementHandler = handler
            elif event_name == "start-ns":
                if hasattr(self.target, "start_ns"):
                    def handler(prefix, uri, event=event_name, append=append,
                                start_ns=self._start_ns):
                        append((event, start_ns(prefix, uri)))
                else:
                    def handler(prefix, uri, event=event_name, append=append):
                        append((event, (prefix or '', uri or '')))
                parser.StartNamespaceDeclHandler = handler
            elif event_name == "end-ns":
                if hasattr(self.target, "end_ns"):
                    def handler(prefix, event=event_name, append=append,
                                end_ns=self._end_ns):
                        append((event, end_ns(prefix)))
                else:
                    def handler(prefix, event=event_name, append=append):
                        append((event, None))
                parser.EndNamespaceDeclHandler = handler
            elif event_name == 'comment':
                def handler(text, event=event_name, append=append,
                            self=self):
                    append((event, self.target.comment(text)))
                parser.CommentHandler = handler
            elif event_name == 'pi':
                def handler(pi_target, data, event=event_name, append=append,
                            self=self):
                    append((event, self.target.pi(pi_target, data)))
                parser.ProcessingInstructionHandler = handler
            else:
                raise ValueError("unknown event %r" % event_name)

    def _raiseerror(self, value):
        err = ParseError(value)
        err.code = value.code
        err.position = value.lineno, value.offset
        raise err

    def _fixname(self, key):
        # expat reports '{uri}local' as 'uri}local'
        try:
            name = self._names[key]
        except KeyError:
            name = key
            if "}" in name:
                name = "{" + name
            self._names[key] = name
        return name

    def _start_ns(self, prefix, uri):
        return self.target.start_ns(prefix or '', uri or '')

    def _end_ns(self, prefix):
        return self.target.end_ns(prefix or '')

    def _start(self, tag, attr_list):
        fixname = self._fixname
        tag = fixname(tag)
        attrib = {}
        if attr_list:
            for i in range(0, len(attr_list), 2):
                attrib[fixname(attr_list[i])] = attr_list[i + 1]
        return self.target.start(tag, attrib)

    def _end(self, tag):
        return self.target.end(self._fixname(tag))

    def _start_doctype(self, name, sysid, pubid, has_internal_subset):
        if hasattr(self.target, "doctype"):
            self.target.doctype(name, pubid, sysid)
        elif hasattr(self, "doctype"):
            warnings.warn(
                "The doctype() method of XMLParser is ignored.  "
                "Define doctype() method on the TreeBuilder target.",
                RuntimeWarning)

    def _default(self, text):
        if text[:1] == "&":
            # an entity the document does not declare
            try:
                data_handler = self.target.data
            except AttributeError:
                return
            try:
                data_handler(self.entity[text[1:-1]])
            except KeyError:
                from xml.parsers import expat
                err = expat.error(
                    "undefined entity %s: line %d, column %d" %
                    (text, self.parser.ErrorLineNumber,
                     self.parser.ErrorColumnNumber)
                )
                err.code = 11
                err.lineno = self.parser.ErrorLineNumber
                err.offset = self.parser.ErrorColumnNumber
                raise err

    def feed(self, data):
        """Feed encoded data to the parser."""
        try:
            self.parser.Parse(data, False)
        except self._error as v:
            self._raiseerror(v)

    def close(self):
        """Finish feeding data; return the target's close() result."""
        try:
            self.parser.Parse(b"", True)
        except self._error as v:
            self._raiseerror(v)
        try:
            close_handler = self.target.close
        except AttributeError:
            pass
        else:
            return close_handler()
        finally:
            del self.parser, self._parser
            del self.target, self._target

    def flush(self):
        was_enabled = self.parser.GetReparseDeferralEnabled()
        try:
            self.parser.SetReparseDeferralEnabled(False)
            self.parser.Parse(b"", False)
        except self._error as v:
            self._raiseerror(v)
        finally:
            self.parser.SetReparseDeferralEnabled(was_enabled)


# --------------------------------------------------------------------
# Canonical XML (C14N 2.0)

def canonicalize(xml_data=None, *, out=None, from_file=None, **options):
    """Convert XML to its C14N 2.0 serialized form.

    Without *out*, the result is returned as a str; otherwise it is written
    to the text stream *out*.  Options: with_comments, strip_text,
    rewrite_prefixes, qname_aware_tags, qname_aware_attrs, exclude_attrs,
    exclude_tags.
    """
    if xml_data is None and from_file is None:
        raise ValueError("Either 'xml_data' or 'from_file' must be provided as input")
    sio = None
    if out is None:
        sio = out = io.StringIO()
    parser = XMLParser(target=C14NWriterTarget(out.write, **options))
    if xml_data is not None:
        parser.feed(xml_data)
        parser.close()
    elif from_file is not None:
        parse(from_file, parser=parser)
    return sio.getvalue() if sio is not None else None


_looks_like_prefix_name = re.compile(r'^\w+:\w+$', re.UNICODE).match


class C14NWriterTarget:
    """A parser target writing canonical XML (C14N 2.0) via *write*."""

    def __init__(self, write, *,
                 with_comments=False, strip_text=False, rewrite_prefixes=False,
                 qname_aware_tags=None, qname_aware_attrs=None,
                 exclude_attrs=None, exclude_tags=None):
        self._write = write
        self._data = []
        self._with_comments = with_comments
        self._strip_text = strip_text
        self._exclude_attrs = set(exclude_attrs) if exclude_attrs else None
        self._exclude_tags = set(exclude_tags) if exclude_tags else None
        self._rewrite_prefixes = rewrite_prefixes
        if qname_aware_tags:
            self._qname_aware_tags = set(qname_aware_tags)
        else:
            self._qname_aware_tags = None
        if qname_aware_attrs:
            self._find_qname_aware_attrs = set(qname_aware_attrs).intersection
        else:
            self._find_qname_aware_attrs = None
        self._declared_ns_stack = [[
            ("http://www.w3.org/XML/1998/namespace", "xml"),
        ]]
        self._ns_stack = []
        if not rewrite_prefixes:
            self._ns_stack.append(list(_namespace_map.items()))
        self._ns_stack.append([])
        self._prefix_map = {}
        self._preserve_space = [False]
        self._pending_start = None
        self._root_seen = False
        self._root_done = False
        self._ignored_depth = 0

    def _iter_namespaces(self, ns_stack, _reversed=reversed):
        for namespaces in _reversed(ns_stack):
            if namespaces:
                yield from namespaces

    def _resolve_prefix_name(self, prefixed_name):
        prefix, name = prefixed_name.split(':', 1)
        for uri, p in self._iter_namespaces(self._ns_stack):
            if p == prefix:
                return f'{{{uri}}}{name}'
        raise ValueError(f'Prefix {prefix} of QName "{prefixed_name}" is not declared in scope')

    def _qname(self, qname, uri=None):
        if uri is None:
            uri, tag = qname[1:].rsplit('}', 1) if qname[:1] == '{' else ('', qname)
        else:
            tag = qname
        prefixes_seen = set()
        for u, prefix in self._iter_namespaces(self._declared_ns_stack):
            if u == uri and prefix not in prefixes_seen:
                return f'{prefix}:{tag}' if prefix else tag, tag, uri
            prefixes_seen.add(prefix)
        if self._rewrite_prefixes:
            if uri in self._prefix_map:
                prefix = self._prefix_map[uri]
            else:
                prefix = self._prefix_map[uri] = f'n{len(self._prefix_map)}'
            self._declared_ns_stack[-1].append((uri, prefix))
            return f'{prefix}:{tag}', tag, uri
        if not uri and '' not in prefixes_seen:
            return tag, tag, uri
        for u, prefix in self._iter_namespaces(self._ns_stack):
            if u == uri:
                self._declared_ns_stack[-1].append((uri, prefix))
                return f'{prefix}:{tag}' if prefix else tag, tag, uri
        if not uri:
            return tag, tag, uri
        raise ValueError(f'Namespace "{uri}" is not declared in scope')

    def data(self, data):
        if not self._ignored_depth:
            self._data.append(data)

    def _flush(self, _join_text=''.join):
        data = _join_text(self._data)
        del self._data[:]
        if self._strip_text and not self._preserve_space[-1]:
            data = data.strip(" \t\r\n")
        if self._pending_start is not None:
            args, self._pending_start = self._pending_start, None
            qname_text = data if data and _looks_like_prefix_name(data) else None
            self._start(*args, qname_text)
            if qname_text is not None:
                return
        if data and self._root_seen:
            self._write(_escape_cdata_c14n(data))

    def start_ns(self, prefix, uri):
        if self._ignored_depth:
            return
        if self._data:
            self._flush()
        self._ns_stack[-1].append((uri, prefix))

    def start(self, tag, attrs):
        if self._exclude_tags is not None and (
                self._ignored_depth or tag in self._exclude_tags):
            self._ignored_depth += 1
            return
        if self._data:
            self._flush()
        new_namespaces = []
        self._declared_ns_stack.append(new_namespaces)
        if self._qname_aware_tags is not None and tag in self._qname_aware_tags:
            self._pending_start = (tag, attrs, new_namespaces)
            return
        self._start(tag, attrs, new_namespaces)

    def _start(self, tag, attrs, new_namespaces, qname_text=None):
        if self._exclude_attrs is not None and attrs:
            attrs = {k: v for k, v in attrs.items() if k not in self._exclude_attrs}
        qnames = {tag, *attrs}
        resolved_names = {}
        if qname_text is not None:
            qname = resolved_names[qname_text] = self._resolve_prefix_name(qname_text)
            qnames.add(qname)
        if self._find_qname_aware_attrs is not None and attrs:
            qattrs = self._find_qname_aware_attrs(attrs)
            if qattrs:
                for attr_name in qattrs:
                    value = attrs[attr_name]
                    if _looks_like_prefix_name(value):
                        qname = resolved_names[value] = self._resolve_prefix_name(value)
                        qnames.add(qname)
            else:
                qattrs = None
        else:
            qattrs = None
        parse_qname = self._qname
        parsed_qnames = {n: parse_qname(n) for n in sorted(
            qnames, key=lambda n: n.split('}', 1))}
        if new_namespaces:
            attr_list = [
                ('xmlns:' + prefix if prefix else 'xmlns', uri)
                for uri, prefix in new_namespaces
            ]
            attr_list.sort()
        else:
            attr_list = []
        if attrs:
            for k, v in sorted(attrs.items()):
                if qattrs is not None and k in qattrs and v in resolved_names:
                    v = parsed_qnames[resolved_names[v]][0]
                attr_qname, attr_name, uri = parsed_qnames[k]
                attr_list.append((attr_qname if uri else attr_name, v))
        space_behaviour = attrs.get('{http://www.w3.org/XML/1998/namespace}space')
        self._preserve_space.append(
            space_behaviour == 'preserve' if space_behaviour
            else self._preserve_space[-1])
        write = self._write
        write('<' + parsed_qnames[tag][0])
        if attr_list:
            write(''.join([f' {k}="{_escape_attrib_c14n(v)}"' for k, v in attr_list]))
        write('>')
        if qname_text is not None:
            write(_escape_cdata_c14n(parsed_qnames[resolved_names[qname_text]][0]))
        self._root_seen = True
        self._ns_stack.append([])

    def end(self, tag):
        if self._ignored_depth:
            self._ignored_depth -= 1
            return
        if self._data:
            self._flush()
        self._write(f'</{self._qname(tag)[0]}>')
        self._preserve_space.pop()
        self._root_done = len(self._preserve_space) == 1
        self._declared_ns_stack.pop()
        self._ns_stack.pop()

    def comment(self, text):
        if not self._with_comments:
            return
        if self._ignored_depth:
            return
        if self._root_done:
            self._write('\n')
        elif self._root_seen and self._data:
            self._flush()
        self._write(f'<!--{_escape_cdata_c14n(text)}-->')
        if not self._root_seen:
            self._write('\n')

    def pi(self, target, data):
        if self._ignored_depth:
            return
        if self._root_done:
            self._write('\n')
        elif self._root_seen and self._data:
            self._flush()
        self._write(
            f'<?{target} {_escape_cdata_c14n(data)}?>' if data else f'<?{target}?>')
        if not self._root_seen:
            self._write('\n')


def _escape_cdata_c14n(text):
    try:
        if '&' in text:
            text = text.replace('&', '&amp;')
        if '<' in text:
            text = text.replace('<', '&lt;')
        if '>' in text:
            text = text.replace('>', '&gt;')
        if '\r' in text:
            text = text.replace('\r', '&#xD;')
        return text
    except (TypeError, AttributeError):
        _raise_serialization_error(text)


def _escape_attrib_c14n(text):
    try:
        if '&' in text:
            text = text.replace('&', '&amp;')
        if '<' in text:
            text = text.replace('<', '&lt;')
        if '"' in text:
            text = text.replace('"', '&quot;')
        if '\t' in text:
            text = text.replace('\t', '&#x9;')
        if '\n' in text:
            text = text.replace('\n', '&#xA;')
        if '\r' in text:
            text = text.replace('\r', '&#xD;')
        return text
    except (TypeError, AttributeError):
        _raise_serialization_error(text)


# --------------------------------------------------------------------

# Element, SubElement, ParseError, TreeBuilder, XMLParser and _set_factories
# may be replaced by the C accelerator.
_Element_Py = Element

try:
    from _elementtree import *
    from _elementtree import _set_factories
except ImportError:
    pass
else:
    _set_factories(Comment, ProcessingInstruction)


def __getattr__(name):
    if name == "VERSION":
        warnings._deprecated("VERSION", remove=(3, 20))
        return _VERSION
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
