"""Pull-style parsing to minidom nodes.

parse() and parseString() return a DOMEventStream: iterating it yields
(event, node) pairs; expandNode() builds the whole subtree of the node a
START_ELEMENT event returned.
"""

import collections
import xml.sax
import xml.sax.handler

START_ELEMENT = "START_ELEMENT"
END_ELEMENT = "END_ELEMENT"
COMMENT = "COMMENT"
START_DOCUMENT = "START_DOCUMENT"
END_DOCUMENT = "END_DOCUMENT"
PROCESSING_INSTRUCTION = "PROCESSING_INSTRUCTION"
IGNORABLE_WHITESPACE = "IGNORABLE_WHITESPACE"
CHARACTERS = "CHARACTERS"

_XMLNS_NAMESPACE = "http://www.w3.org/2000/xmlns/"


class PullDOM(xml.sax.ContentHandler):
    """A SAX content handler queueing (event, node) pairs.  The Document is
    created when the root element starts (its name is needed); comments
    and processing instructions seen before are kept until then."""

    _locator = None
    document = None

    def __init__(self, documentFactory=None):
        from xml.dom import XML_NAMESPACE
        self.documentFactory = documentFactory
        self._events = collections.deque()
        self.elementStack = []
        self.push = self.elementStack.append
        self.pop = self.elementStack.pop
        # namespace URI -> prefix, one dict per mapping scope
        self._ns_contexts = [{XML_NAMESPACE: 'xml'}]
        self._current_context = self._ns_contexts[-1]
        self._xmlns_attrs = []
        # (kind, data...) seen before the document exists
        self.pending_events = []

    def setDocumentLocator(self, locator):
        self._locator = locator

    def _queue(self, event, node):
        self._events.append((event, node))

    def _take(self):
        return self._events.popleft() if self._events else None

    # -- namespaces

    def startPrefixMapping(self, prefix, uri):
        self._xmlns_attrs.append((prefix or 'xmlns', uri))
        self._ns_contexts.append(self._current_context.copy())
        self._current_context[uri] = prefix or None

    def endPrefixMapping(self, prefix):
        self._current_context = self._ns_contexts.pop()

    def _qualified(self, uri, local):
        """A qualified name for (uri, local) from the prefixes in scope."""
        prefix = self._current_context.get(uri)
        return prefix + ":" + local if prefix else local

    # -- elements

    def _new_element(self, uri, qname):
        if self.document:
            if uri:
                return self.document.createElementNS(uri, qname)
            return self.document.createElement(qname)
        return self.buildDocument(uri, qname)

    def startElementNS(self, name, tagName, attrs):
        declarations, self._xmlns_attrs = self._xmlns_attrs, []

        uri, local = name
        if uri:
            # the reader may not give the original qualified name
            if tagName is None:
                tagName = self._qualified(uri, local)
            node = self._new_element(uri, tagName)
        else:
            node = self._new_element(None, local)

        doc = self.document
        # the prefix mappings that started here become xmlns attributes,
        # ahead of the others
        for aname, value in declarations:
            qname = aname if aname == 'xmlns' else 'xmlns:' + aname
            attr = doc.createAttributeNS(_XMLNS_NAMESPACE, qname)
            attr.value = value
            node.setAttributeNodeNS(attr)
        for (a_uri, a_local), value in attrs.items():
            if a_uri == _XMLNS_NAMESPACE:
                qname = a_local if a_local == 'xmlns' else 'xmlns:' + a_local
                attr = doc.createAttributeNS(a_uri, qname)
                node.setAttributeNodeNS(attr)
            elif a_uri:
                attr = doc.createAttributeNS(a_uri,
                                             self._qualified(a_uri, a_local))
                node.setAttributeNodeNS(attr)
            else:
                attr = doc.createAttribute(a_local)
                node.setAttributeNode(attr)
            attr.value = value

        self._queue(START_ELEMENT, node)
        self.push(node)

    def endElementNS(self, name, tagName):
        self._queue(END_ELEMENT, self.pop())

    def startElement(self, name, attrs):
        node = self._new_element(None, name)
        for aname, value in attrs.items():
            attr = self.document.createAttribute(aname)
            attr.value = value
            node.setAttributeNode(attr)
        self._queue(START_ELEMENT, node)
        self.push(node)

    def endElement(self, name):
        self._queue(END_ELEMENT, self.pop())

    # -- other content

    def comment(self, s):
        if self.document:
            self._queue(COMMENT, self.document.createComment(s))
        else:
            self.pending_events.append((COMMENT, s))

    def processingInstruction(self, target, data):
        if self.document:
            node = self.document.createProcessingInstruction(target, data)
            self._queue(PROCESSING_INSTRUCTION, node)
        else:
            self.pending_events.append((PROCESSING_INSTRUCTION, target, data))

    def ignorableWhitespace(self, chars):
        self._queue(IGNORABLE_WHITESPACE, self.document.createTextNode(chars))

    def characters(self, chars):
        self._queue(CHARACTERS, self.document.createTextNode(chars))

    # -- the document

    def startDocument(self):
        if self.documentFactory is None:
            import xml.dom.minidom
            self.documentFactory = xml.dom.minidom.Document.implementation

    def buildDocument(self, uri, tagname):
        """Create the document (with its root element, which is returned)
        when the root element starts."""
        doc = self.documentFactory.createDocument(uri, tagname, None)
        self.document = doc
        self._queue(START_DOCUMENT, doc)
        self.push(doc)
        for pending in self.pending_events:
            if pending[0] == PROCESSING_INSTRUCTION:
                node = doc.createProcessingInstruction(pending[1], pending[2])
            elif pending[0] == COMMENT:
                node = doc.createComment(pending[1])
            else:
                raise AssertionError("Unknown pending event ", pending[0])
            self._queue(pending[0], node)
        self.pending_events = None
        return doc.firstChild

    def endDocument(self):
        self._queue(END_DOCUMENT, self.document)
        self.pop()

    def clear(self):
        """Drop the document."""
        self.document = None


class ErrorHandler:
    def warning(self, exception):
        print(exception)

    def error(self, exception):
        raise exception

    def fatalError(self, exception):
        raise exception


class DOMEventStream:
    """Iterator over the (event, node) pairs of a document, parsed a
    buffer at a time as the events are consumed."""

    def __init__(self, stream, parser, bufsize):
        self.stream = stream
        self.parser = parser
        self.bufsize = bufsize
        if not hasattr(self.parser, 'feed'):
            # not incremental: parse everything at the first request
            self.getEvent = self._slurp
        self.reset()

    def reset(self):
        self.pulldom = PullDOM()
        # PullDOM needs the namespace events
        self.parser.setFeature(xml.sax.handler.feature_namespaces, 1)
        self.parser.setContentHandler(self.pulldom)

    def __next__(self):
        event = self.getEvent()
        if event:
            return event
        raise StopIteration

    def __iter__(self):
        return self

    def expandNode(self, node):
        """Attach to node everything up to its END_ELEMENT event."""
        open_nodes = [node]
        event = self.getEvent()
        while event:
            kind, current = event
            if current is node:
                return
            if kind == START_ELEMENT:
                open_nodes[-1].appendChild(current)
                open_nodes.append(current)
            elif kind == END_ELEMENT:
                del open_nodes[-1]
            else:
                open_nodes[-1].appendChild(current)
            event = self.getEvent()

    def getEvent(self):
        """The next (event, node) pair, or None at the end of the input."""
        handler = self.pulldom
        while not handler._events:
            data = self.stream.read(self.bufsize)
            if not data:
                # the events closing the parse are not reported
                self.parser.close()
                return None
            self.parser.feed(data)
        return handler._take()

    def _slurp(self):
        # for parsers without the incremental interface
        self.parser.parse(self.stream)
        self.getEvent = self._emit
        return self._emit()

    def _emit(self):
        return self.pulldom._take()

    def clear(self):
        """Release the handler, the parser and the stream."""
        self.pulldom.clear()
        del self.pulldom
        self.parser = None
        self.stream = None


class SAX2DOM(PullDOM):
    """A PullDOM that also links every node into the document as it is
    created, so that the document is complete at the end."""

    def _attach_last(self):
        # the node of the event just queued goes into the open element
        self.elementStack[-1].appendChild(self._events[-1][1])

    def startElementNS(self, name, tagName, attrs):
        PullDOM.startElementNS(self, name, tagName, attrs)
        self.elementStack[-2].appendChild(self.elementStack[-1])

    def startElement(self, name, attrs):
        PullDOM.startElement(self, name, attrs)
        self.elementStack[-2].appendChild(self.elementStack[-1])

    def processingInstruction(self, target, data):
        PullDOM.processingInstruction(self, target, data)
        self._attach_last()

    def ignorableWhitespace(self, chars):
        PullDOM.ignorableWhitespace(self, chars)
        self._attach_last()

    def characters(self, chars):
        PullDOM.characters(self, chars)
        self._attach_last()


default_bufsize = (2 ** 14) - 20


def parse(stream_or_string, parser=None, bufsize=None):
    """Events of the document in a file (a name or an open file)."""
    if bufsize is None:
        bufsize = default_bufsize
    if isinstance(stream_or_string, str):
        stream = open(stream_or_string, 'rb')
    else:
        stream = stream_or_string
    if not parser:
        parser = xml.sax.make_parser()
    return DOMEventStream(stream, parser, bufsize)


def parseString(string, parser=None):
    """Events of the document in a string."""
    from io import StringIO
    if not parser:
        parser = xml.sax.make_parser()
    return DOMEventStream(StringIO(string), parser, len(string))
