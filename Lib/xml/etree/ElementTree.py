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
        other than us-ascii, utf-8 and unicode; method: 'xml', 'html' or
        'text'.
        """
        if self._root is None:
            raise TypeError('ElementTree not initialized')
        method = method or "xml"
        if method not in _OUTPUT_METHODS:
            raise ValueError("unknown method %r" % method)
        if not encoding:
            encoding = "utf-8" if method == "c14n" else "us-ascii"
        with _text_sink(file_or_filename, encoding) as (write, declared):
            if method == "xml" and _declaration_wanted(xml_declaration,
                                                       encoding, declared):
                write("<?xml version='1.0' encoding='%s'?>\n" % (declared,))
            if isinstance(file_or_filename, _ChunkCollector):
                # tostringlist() keeps the individual pieces
                _emit(write, self._root, method, default_namespace,
                      short_empty_elements)
                return
            # the emitters produce many small strings: write them at once
            chunks = []
            try:
                _emit(chunks.append, self._root, method, default_namespace,
                      short_empty_elements)
            finally:
                if chunks:
                    write("".join(chunks))

    def write_c14n(self, file):
        return self.write(file, method="c14n")


def _declaration_wanted(requested, encoding, declared):
    if requested is not None:
        return bool(requested)
    return (encoding.lower() != "unicode"
            and declared.lower() not in ("utf-8", "us-ascii"))


class _BinaryWriteAdapter(io.BufferedIOBase):
    # Presents an object that only has write(bytes) as a binary stream that
    # TextIOWrapper accepts.

    def __init__(self, target):
        self._target = target

    def writable(self):
        return True

    def write(self, data):
        return self._target.write(data)

    def seekable(self):
        probe = getattr(self._target, 'seekable', None)
        return bool(probe()) if probe is not None else False

    def tell(self):
        return self._target.tell()


@contextlib.contextmanager
def _text_sink(target, encoding):
    """Yield (write(str) function, encoding name for the declaration) for a
    file name or a text/binary file object."""
    write = getattr(target, "write", None)
    if write is None:
        if encoding.lower() == "unicode":
            encoding = "utf-8"
        with open(target, "w", encoding=encoding,
                  errors="xmlcharrefreplace") as file:
            yield file.write, encoding
        return
    if encoding.lower() == "unicode":
        # a text stream: str goes straight through
        yield write, getattr(target, "encoding", None) or "utf-8"
        return
    if isinstance(target, io.BufferedIOBase):
        binary, detach_binary = target, False
    elif isinstance(target, io.RawIOBase):
        binary, detach_binary = io.BufferedWriter(target), True
    else:
        binary, detach_binary = _BinaryWriteAdapter(target), False
    text = io.TextIOWrapper(binary, encoding=encoding,
                            errors="xmlcharrefreplace", newline="\n")
    try:
        yield text.write, encoding
    finally:
        # flush and hand the stream back without closing it
        text.detach()
        if detach_binary:
            binary.detach()


def _unserializable(value):
    raise TypeError("cannot serialize %r (type %s)"
                    % (value, type(value).__name__))


def _qualified_names(root, default_namespace=None):
    """Return (qnames, namespaces) for serializing *root*.

    qnames maps every tag and attribute name (and QName value) found in the
    tree to its text in the output, 'prefix:local' or 'local'; namespaces
    maps each namespace URI used to its prefix ('' for the default one).
    """
    qnames = {None: None}
    namespaces = {}
    if default_namespace:
        namespaces[default_namespace] = ""

    def register(name):
        if name in qnames:
            return
        if not isinstance(name, str):
            _unserializable(name)
        if not name.startswith("{"):
            if default_namespace:
                raise ValueError("cannot use non-qualified names with "
                                 "default_namespace option")
            qnames[name] = name
            return
        uri, sep, local = name[1:].rpartition("}")
        if not sep:
            raise ValueError(f"malformed qualified name {name!r}")
        prefix = namespaces.get(uri)
        if prefix is None:
            # a URI registered with the empty prefix is written as the default
            # namespace (xmlns="uri")
            prefix = _namespace_map.get(uri)
            if prefix is None:
                prefix = "ns%d" % len(namespaces)
            if prefix != "xml":
                namespaces[uri] = prefix
        qnames[name] = "%s:%s" % (prefix, local) if prefix else local

    for elem in root.iter():
        tag = elem.tag
        if isinstance(tag, QName):
            register(tag.text)
        elif isinstance(tag, str):
            register(tag)
        elif tag is not None and tag is not Comment and tag is not PI:
            _unserializable(tag)
        for key, value in elem.items():
            register(key.text if isinstance(key, QName) else key)
            if isinstance(value, QName):
                register(value.text)
        if isinstance(elem.text, QName):
            register(elem.text.text)
    return qnames, namespaces


def _escaped(text, replacements):
    try:
        for char, entity in replacements:
            if char in text:
                text = text.replace(char, entity)
        return text
    except (TypeError, AttributeError):
        _unserializable(text)


_TEXT_ESCAPES = (("&", "&amp;"), ("<", "&lt;"), (">", "&gt;"))
_ATTRIBUTE_ESCAPES = _TEXT_ESCAPES + (('"', "&quot;"), ("\r", "&#13;"),
                                      ("\n", "&#10;"), ("\t", "&#09;"))
_HTML_ATTRIBUTE_ESCAPES = (("&", "&amp;"), (">", "&gt;"), ('"', "&quot;"))

# HTML elements written without an end tag
HTML_EMPTY = {"area", "base", "basefont", "br", "col", "embed", "frame", "hr",
              "img", "input", "isindex", "link", "meta", "param", "source",
              "track", "wbr"}

# HTML elements whose text is written without escaping
_HTML_RAW_TEXT = {"script", "style", "xmp", "iframe", "noembed", "noframes",
                  "plaintext"}

_OUTPUT_METHODS = ("xml", "html", "text")


def _emit(write, root, method, default_namespace, short_empty_elements):
    """Write *root* and its subtree (and its tail) with *write*."""
    if method == "text":
        for piece in root.itertext():
            write(piece)
        if root.tail:
            write(root.tail)
        return
    qnames, namespaces = _qualified_names(root, default_namespace)
    html = method == "html"
    # Work stack of elements to open and of ('close', end text, tail) items,
    # so that deep trees need no recursion.
    stack = [root]
    declare = sorted(namespaces.items(), key=lambda item: item[1])
    while stack:
        item = stack.pop()
        if type(item) is tuple:
            end, tail = item
            if end:
                write(end)
            if tail:
                write(_escaped(tail, _TEXT_ESCAPES))
            continue
        elem = item
        tag, text = elem.tag, elem.text
        if tag is Comment:
            write("<!--%s-->" % text)
            end = None
        elif tag is ProcessingInstruction:
            write("<?%s?>" % text)
            end = None
        else:
            name = qnames[tag]
            end = None
            if name is not None:
                write("<" + name)
                if elem is root:
                    # namespace declarations go on the root element only
                    for uri, prefix in declare:
                        write(' xmlns%s="%s"' % (":" + prefix if prefix else "",
                                                 _escaped(uri, _ATTRIBUTE_ESCAPES)))
                _write_attributes(write, elem, qnames, html)
                if html:
                    write(">")
                    lowered = name.lower()
                    if lowered not in HTML_EMPTY and lowered != "plaintext":
                        end = "</" + name + ">"
                    if text and lowered in _HTML_RAW_TEXT:
                        write(text)
                        text = None
                elif text or len(elem) or not short_empty_elements:
                    write(">")
                    end = "</" + name + ">"
                else:
                    write(" />")
            if text:
                write(_escaped(text, _TEXT_ESCAPES))
            stack.append((end, elem.tail))
            stack.extend(reversed(elem))
            continue
        if elem.tail:
            write(_escaped(elem.tail, _TEXT_ESCAPES))


def _write_attributes(write, elem, qnames, html):
    for key, value in elem.items():
        if isinstance(key, QName):
            key = key.text
        if html and value is None:
            # HTML boolean attribute
            write(" " + qnames[key])
            continue
        if isinstance(value, QName):
            value = qnames[value.text]
        else:
            value = _escaped(value, _HTML_ATTRIBUTE_ESCAPES if html
                             else _ATTRIBUTE_ESCAPES)
        write(' %s="%s"' % (qnames[key], value))


def register_namespace(prefix, uri):
    """Register a prefix to use for *uri* when serializing.

    Prefixes of the form ns<digits> are reserved for generated names.
    Any earlier mapping of the prefix or of the URI is replaced.
    """
    if re.match(r"ns\d+$", prefix):
        raise ValueError("Prefix format reserved for internal use")
    for known_uri, known_prefix in list(_namespace_map.items()):
        if known_uri == uri or known_prefix == prefix:
            del _namespace_map[known_uri]
    _namespace_map[uri] = prefix


# namespace URI -> preferred prefix
_namespace_map = {
    "http://www.w3.org/XML/1998/namespace": "xml",
    "http://www.w3.org/1999/xhtml": "html",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#": "rdf",
    "http://schemas.xmlsoap.org/wsdl/": "wsdl",
    "http://www.w3.org/2001/XMLSchema": "xs",
    "http://www.w3.org/2001/XMLSchema-instance": "xsi",
    "http://purl.org/dc/elements/1.1/": "dc",
}
# the tests reach the table through the function
register_namespace._namespace_map = _namespace_map


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


class _ChunkCollector(io.BufferedIOBase):
    """Write-only stream that keeps every chunk written, in a list."""

    def __init__(self, chunks):
        self.chunks = chunks

    def writable(self):
        return True

    def seekable(self):
        return True

    def write(self, data):
        self.chunks.append(data)

    def tell(self):
        return len(self.chunks)


def tostringlist(element, encoding=None, method=None, *,
                 xml_declaration=None, default_namespace=None,
                 short_empty_elements=True):
    """Like tostring(), but return the output as a list of chunks."""
    chunks = []
    ElementTree(element).write(_ChunkCollector(chunks), encoding,
                               xml_declaration=xml_declaration,
                               default_namespace=default_namespace,
                               method=method,
                               short_empty_elements=short_empty_elements)
    return chunks


def dump(elem):
    """Write an element tree to sys.stdout (for debugging)."""
    tree = elem if isinstance(elem, ElementTree) else ElementTree(elem)
    tree.write(sys.stdout, encoding="unicode")
    tail = tree.getroot().tail
    if not tail or not tail.endswith("\n"):
        sys.stdout.write("\n")


def _blank(text):
    return not text or not text.strip(" \t\r\n")


def indent(tree, space="  ", level=0):
    """Pretty-print in place: make every element that has children start
    them on new lines indented by *space* per level, replacing text and
    tails that are only whitespace."""
    if isinstance(tree, ElementTree):
        tree = tree.getroot()
    if level < 0:
        raise ValueError(f"Initial indentation level must be >= 0, got {level}")
    if not len(tree):
        return
    # one string object per depth, shared by all elements at that depth
    prefixes = ["\n" + space * level]

    def prefix(depth):
        while len(prefixes) <= depth:
            prefixes.append(prefixes[-1] + space)
        return prefixes[depth]

    pending = [(tree, 0)]
    while pending:
        elem, depth = pending.pop()
        inner = prefix(depth + 1)
        if _blank(elem.text):
            elem.text = inner
        child = None
        for child in elem:
            if len(child):
                pending.append((child, depth + 1))
            if _blank(child.tail):
                child.tail = inner
        # the last child's tail leads back to this element's level
        if _blank(child.tail):
            child.tail = prefix(depth)


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
    return _IterParser(source, events, parser)


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


class _IterParser(collections.abc.Iterator):

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
    """Build an element tree from start/end/data/comment/pi calls.

    element_factory(tag, attrs) creates elements (default Element);
    comment_factory and pi_factory create comments and processing
    instructions, which are only put into the tree when insert_comments /
    insert_pis is true.
    """

    def __init__(self, element_factory=None, *,
                 comment_factory=None, pi_factory=None,
                 insert_comments=False, insert_pis=False):
        self._make_element = Element if element_factory is None else element_factory
        self._make_comment = Comment if comment_factory is None else comment_factory
        self._make_pi = ProcessingInstruction if pi_factory is None else pi_factory
        self.insert_comments = insert_comments
        self.insert_pis = insert_pis
        self._open = []         # elements started but not yet ended
        self._root = None
        self._text = []         # character data not yet attached
        self._anchor = None     # the node that pending text belongs to ...
        self._as_tail = False   # ... as its tail (after it) or its text

    def close(self):
        """Return the root element."""
        if self._open:
            raise AssertionError("missing end tags")
        if self._root is None:
            raise AssertionError("missing toplevel element")
        return self._root

    def _attach_text(self):
        if not self._text:
            return
        node = self._anchor
        if node is not None:
            text = "".join(self._text)
            if self._as_tail:
                if node.tail is not None:
                    raise AssertionError("internal error (tail)")
                node.tail = text
            else:
                if node.text is not None:
                    raise AssertionError("internal error (text)")
                node.text = text
        self._text = []

    def data(self, data):
        """Add text to the current element."""
        self._text.append(data)

    def start(self, tag, attrs):
        """Open an element."""
        self._attach_text()
        elem = self._make_element(tag, attrs)
        if self._open:
            self._open[-1].append(elem)
        elif self._root is None:
            self._root = elem
        self._open.append(elem)
        self._anchor, self._as_tail = elem, False
        return elem

    def end(self, tag):
        """Close the current element."""
        self._attach_text()
        elem = self._open.pop()
        if elem.tag != tag:
            raise AssertionError("end tag mismatch (expected %s, got %s)"
                                 % (elem.tag, tag))
        self._anchor, self._as_tail = elem, True
        return elem

    def _leaf(self, node, insert):
        # a comment or PI: in the tree, it ends the preceding text
        if insert:
            self._attach_text()
            if self._open:
                self._open[-1].append(node)
            self._anchor, self._as_tail = node, True
        return node

    def comment(self, text):
        """Create a comment (inserted into the tree if insert_comments)."""
        return self._leaf(self._make_comment(text), self.insert_comments)

    def pi(self, target, text=None):
        """Create a processing instruction (inserted if insert_pis)."""
        return self._leaf(self._make_pi(target, text), self.insert_pis)


def _expat_module():
    try:
        from xml.parsers import expat
    except ImportError:
        try:
            import pyexpat as expat
        except ImportError:
            raise ImportError("No module named expat; ElementTree needs it "
                              "for parsing") from None
    return expat


class XMLParser:
    """An expat based parser feeding a target (by default a TreeBuilder).

    The target receives start(tag, attrs), end(tag), data(text),
    comment(text), pi(target, data), start_ns(prefix, uri), end_ns(prefix)
    and doctype(name, pubid, system) calls for whichever of these methods
    it has; close() returns the result of target.close().
    """

    def __init__(self, *, target=None, encoding=None):
        expat = _expat_module()
        parser = expat.ParserCreate(encoding, "}")
        if target is None:
            target = TreeBuilder()
        self.parser = self._parser = parser
        self.target = self._target = target
        self._error = expat.error
        self._clark_names = {}      # expat's "uri}local" -> "{uri}local"
        self.entity = {}            # replacement text of undeclared entities
        parser.DefaultHandlerExpand = self._default
        for method, slot, handler in (
                ("start", "StartElementHandler", self._start),
                ("end", "EndElementHandler", self._end),
                ("start_ns", "StartNamespaceDeclHandler", self._start_ns),
                ("end_ns", "EndNamespaceDeclHandler", self._end_ns),
                ("data", "CharacterDataHandler", None),
                ("comment", "CommentHandler", None),
                ("pi", "ProcessingInstructionHandler", None)):
            if hasattr(target, method):
                setattr(parser, slot, handler or getattr(target, method))
        parser.StartDoctypeDeclHandler = self._start_doctype
        parser.buffer_text = 1
        parser.ordered_attributes = 1
        try:
            self.version = "Expat %d.%d.%d" % expat.version_info
        except AttributeError:
            pass

    def _setevents(self, events_queue, events_to_report):
        # Used by XMLPullParser: every reported event appends
        # (event name, result of the target call) to events_queue.
        parser = self._parser
        append = events_queue.append

        def reporting(event, call):
            def handler(*args):
                append((event, call(*args)))
            return handler

        target = self.target
        for event in events_to_report:
            if event == "start":
                parser.ordered_attributes = 1
                parser.StartElementHandler = reporting(event, self._start)
            elif event == "end":
                parser.EndElementHandler = reporting(event, self._end)
            elif event == "start-ns":
                call = (self._start_ns if hasattr(target, "start_ns")
                        else lambda prefix, uri: (prefix or '', uri or ''))
                parser.StartNamespaceDeclHandler = reporting(event, call)
            elif event == "end-ns":
                call = (self._end_ns if hasattr(target, "end_ns")
                        else lambda prefix: None)
                parser.EndNamespaceDeclHandler = reporting(event, call)
            elif event == "comment":
                parser.CommentHandler = reporting(
                    event, lambda text: self.target.comment(text))
            elif event == "pi":
                parser.ProcessingInstructionHandler = reporting(
                    event, lambda name, data: self.target.pi(name, data))
            else:
                raise ValueError("unknown event %r" % event)

    def _parse_error(self, exc):
        err = ParseError(exc)
        err.code = exc.code
        err.position = exc.lineno, exc.offset
        return err

    def _clark(self, name):
        try:
            return self._clark_names[name]
        except KeyError:
            clark = "{" + name if "}" in name else name
            self._clark_names[name] = clark
            return clark

    def _start_ns(self, prefix, uri):
        return self.target.start_ns(prefix or '', uri or '')

    def _end_ns(self, prefix):
        return self.target.end_ns(prefix or '')

    def _start(self, tag, attr_list):
        clark = self._clark
        # ordered_attributes: [name0, value0, name1, value1, ...]
        attrib = {clark(attr_list[i]): attr_list[i + 1]
                  for i in range(0, len(attr_list), 2)} if attr_list else {}
        return self.target.start(clark(tag), attrib)

    def _end(self, tag):
        return self.target.end(self._clark(tag))

    def _start_doctype(self, name, sysid, pubid, has_internal_subset):
        if hasattr(self.target, "doctype"):
            self.target.doctype(name, pubid, sysid)
        elif hasattr(self, "doctype"):
            warnings.warn(
                "The doctype() method of XMLParser is ignored.  "
                "Define doctype() method on the TreeBuilder target.",
                RuntimeWarning)

    def _default(self, text):
        # expat passes undeclared entity references here
        if text[:1] != "&":
            return
        handler = getattr(self.target, "data", None)
        if handler is None:
            return
        name = text[1:-1]
        if name in self.entity:
            handler(self.entity[name])
            return
        line = self.parser.ErrorLineNumber
        column = self.parser.ErrorColumnNumber
        err = _expat_module().error(
            "undefined entity %s: line %d, column %d" % (text, line, column))
        err.code = 11
        err.lineno = line
        err.offset = column
        raise err

    def feed(self, data):
        """Feed encoded data to the parser."""
        try:
            self.parser.Parse(data, False)
        except self._error as exc:
            raise self._parse_error(exc)

    def close(self):
        """Finish feeding data; return the target's close() result."""
        try:
            self.parser.Parse(b"", True)
        except self._error as exc:
            raise self._parse_error(exc)
        try:
            finish = getattr(self.target, "close", None)
            if finish is not None:
                return finish()
        finally:
            # the parser cannot be used again
            del self.parser, self._parser
            del self.target, self._target

    def flush(self):
        """Parse buffered data now (expat may defer it otherwise)."""
        was_enabled = self.parser.GetReparseDeferralEnabled()
        try:
            self.parser.SetReparseDeferralEnabled(False)
            self.parser.Parse(b"", False)
        except self._error as exc:
            raise self._parse_error(exc)
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
    buffer = io.StringIO() if out is None else None
    target = C14NWriterTarget((buffer or out).write, **options)
    parser = XMLParser(target=target)
    if xml_data is not None:
        parser.feed(xml_data)
        parser.close()
    else:
        parse(from_file, parser=parser)
    return buffer.getvalue() if buffer is not None else None


# "prefix:local" text, as found in QName-valued content
_PREFIXED_NAME = re.compile(r'^\w+:\w+$', re.UNICODE).match

_XML_NAMESPACE = "http://www.w3.org/XML/1998/namespace"


class C14NWriterTarget:
    """A parser target writing canonical XML (C14N 2.0) via *write*.

    with_comments: keep comments; strip_text: strip leading and trailing
    whitespace of text (unless xml:space="preserve"); rewrite_prefixes:
    replace namespace prefixes by n0, n1, ...; qname_aware_tags and
    qname_aware_attrs: tags whose text and attributes whose values are
    QNames ("prefix:name") to be rewritten with the output prefixes;
    exclude_attrs and exclude_tags: attributes and subtrees to leave out.
    """

    def __init__(self, write, *,
                 with_comments=False, strip_text=False, rewrite_prefixes=False,
                 qname_aware_tags=None, qname_aware_attrs=None,
                 exclude_attrs=None, exclude_tags=None):
        self._write = write
        self._with_comments = with_comments
        self._strip_text = strip_text
        self._rewrite_prefixes = rewrite_prefixes
        self._qname_tags = frozenset(qname_aware_tags or ())
        self._qname_attrs = frozenset(qname_aware_attrs or ())
        self._skip_attrs = frozenset(exclude_attrs or ())
        self._skip_tags = frozenset(exclude_tags or ())
        self._text = []
        # (uri, prefix) bindings seen in the input: a base list, then one
        # list per open element; _announced collects start_ns() events for
        # the element about to start
        self._known = [[] if rewrite_prefixes else list(_namespace_map.items())]
        self._announced = []
        # bindings declared in the output, one list per open element
        self._written = [[(_XML_NAMESPACE, "xml")]]
        self._generated = {}        # uri -> n<N> prefix (rewrite_prefixes)
        self._preserve = [False]    # xml:space="preserve" in effect
        self._deferred = None       # qname-aware element awaiting its text
        self._skipping = 0          # depth inside an excluded subtree
        self._started = False       # the root element has been opened
        self._finished = False      # the root element has been closed

    # -- namespace bookkeeping ---------------------------------------------

    @staticmethod
    def _innermost_first(frames):
        for frame in reversed(frames):
            yield from frame

    def _expand_prefixed(self, text):
        prefix, local = text.split(':', 1)
        for uri, known_prefix in self._innermost_first(self._known):
            if known_prefix == prefix:
                return f'{{{uri}}}{local}'
        raise ValueError(f'Prefix {prefix} of QName "{text}" is not declared in scope')

    def _render_name(self, name):
        """(output name, local part, namespace URI) for a Clark name,
        declaring a prefix on the current element when needed."""
        if name[:1] == '{':
            uri, _, local = name[1:].rpartition('}')
        else:
            uri, local = '', name
        shadowed = set()
        for bound_uri, prefix in self._innermost_first(self._written):
            if bound_uri == uri and prefix not in shadowed:
                return (f'{prefix}:{local}' if prefix else local), local, uri
            shadowed.add(prefix)
        if self._rewrite_prefixes:
            prefix = self._generated.get(uri)
            if prefix is None:
                prefix = self._generated[uri] = f'n{len(self._generated)}'
            self._written[-1].append((uri, prefix))
            return f'{prefix}:{local}', local, uri
        if not uri and '' not in shadowed:
            return local, local, uri
        for bound_uri, prefix in self._innermost_first(self._known):
            if bound_uri == uri:
                self._written[-1].append((uri, prefix))
                return (f'{prefix}:{local}' if prefix else local), local, uri
        if not uri:
            return local, local, uri
        raise ValueError(f'Namespace "{uri}" is not declared in scope')

    # -- text ---------------------------------------------------------------

    def data(self, data):
        if not self._skipping:
            self._text.append(data)

    def _settle(self):
        # write pending text, opening a deferred element first
        if not self._text and self._deferred is None:
            return
        text = ''.join(self._text)
        self._text.clear()
        if self._strip_text and not self._preserve[-1]:
            text = text.strip(" \t\r\n")
        if self._deferred is not None:
            tag, attrs = self._deferred
            self._deferred = None
            qname_text = text if text and _PREFIXED_NAME(text) else None
            self._open(tag, attrs, qname_text)
            if qname_text is not None:
                return
        if text and self._started:
            self._write(_c14n_text(text))

    # -- parser target interface --------------------------------------------

    def start_ns(self, prefix, uri):
        if self._skipping:
            return
        self._settle()
        self._announced.append((uri, prefix))

    def start(self, tag, attrs):
        if self._skip_tags and (self._skipping or tag in self._skip_tags):
            self._skipping += 1
            return
        self._settle()
        self._written.append([])
        self._known.append(self._announced)
        self._announced = []
        if tag in self._qname_tags:
            # the text decides whether it is a QName to rewrite
            self._deferred = (tag, attrs)
            return
        self._open(tag, attrs, None)

    def _open(self, tag, attrs, qname_text):
        if self._skip_attrs and attrs:
            attrs = {k: v for k, v in attrs.items() if k not in self._skip_attrs}
        names = {tag, *attrs}
        expanded = {}           # "prefix:local" content -> Clark name
        if qname_text is not None:
            expanded[qname_text] = self._expand_prefixed(qname_text)
            names.add(expanded[qname_text])
        qname_attrs = self._qname_attrs.intersection(attrs) if attrs else ()
        for attr in qname_attrs:
            value = attrs[attr]
            if _PREFIXED_NAME(value):
                expanded[value] = self._expand_prefixed(value)
                names.add(expanded[value])
        # the order of resolution fixes the order of generated prefixes
        rendered = {name: self._render_name(name)
                    for name in sorted(names, key=lambda n: n.split('}', 1))}
        output_attrs = sorted(('xmlns:' + prefix if prefix else 'xmlns', uri)
                              for uri, prefix in self._written[-1])
        for key, value in sorted(attrs.items()):
            if key in qname_attrs and value in expanded:
                value = rendered[expanded[value]][0]
            text, local, uri = rendered[key]
            output_attrs.append((text if uri else local, value))
        space = attrs.get(f'{{{_XML_NAMESPACE}}}space')
        self._preserve.append(space == 'preserve' if space else self._preserve[-1])
        parts = ['<', rendered[tag][0]]
        for name, value in output_attrs:
            parts.append(f' {name}="{_c14n_attr(value)}"')
        parts.append('>')
        if qname_text is not None:
            parts.append(_c14n_text(rendered[expanded[qname_text]][0]))
        self._write(''.join(parts))
        self._started = True

    def end(self, tag):
        if self._skipping:
            self._skipping -= 1
            return
        self._settle()
        self._write(f'</{self._render_name(tag)[0]}>')
        self._preserve.pop()
        self._finished = len(self._preserve) == 1
        self._written.pop()
        self._known.pop()

    def _before_markup(self):
        # comments and PIs outside the root go on lines of their own
        if self._finished:
            self._write('\n')
        elif self._started:
            self._settle()

    def comment(self, text):
        if not self._with_comments or self._skipping:
            return
        self._before_markup()
        self._write(f'<!--{_c14n_text(text)}-->')
        if not self._started:
            self._write('\n')

    def pi(self, target, data):
        if self._skipping:
            return
        self._before_markup()
        self._write(f'<?{target} {_c14n_text(data)}?>' if data else f'<?{target}?>')
        if not self._started:
            self._write('\n')


_C14N_TEXT_ESCAPES = (('&', '&amp;'), ('<', '&lt;'), ('>', '&gt;'),
                      ('\r', '&#xD;'))
_C14N_ATTR_ESCAPES = (('&', '&amp;'), ('<', '&lt;'), ('"', '&quot;'),
                      ('\t', '&#x9;'), ('\n', '&#xA;'), ('\r', '&#xD;'))


def _c14n_text(text):
    return _escaped(text, _C14N_TEXT_ESCAPES)


def _c14n_attr(text):
    return _escaped(text, _C14N_ATTR_ESCAPES)


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
