"""A "pull" DOM: iterate over (event, node) pairs produced by a SAX parser,
expanding selected subtrees into full DOM nodes on request."""

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
    """SAX content handler queueing (event, node) pairs.

    The Document node is only created when the root element starts; events
    seen before that are queued after the START_DOCUMENT event."""

    _locator = None
    document = None

    def __init__(self, documentFactory=None):
        self.documentFactory = documentFactory
        # queued (event, node) pairs not yet handed out
        self.events = collections.deque()
        # events seen before the document existed, as (event, factory, args)
        self.pending_events = []
        self.elementStack = []
        self._last_node = None
        # namespace prefix bookkeeping
        self._ns_contexts = [{_XMLNS_NAMESPACE: 'xmlns'}]
        self._current_context = self._ns_contexts[-1]
        self._xmlns_attrs = []

    def pop(self):
        result = self.elementStack[-1]
        del self.elementStack[-1]
        return result

    def setDocumentLocator(self, locator):
        self._locator = locator

    def _push(self, event, node):
        self._last_node = node
        self.events.append((event, node))

    # namespaces

    def startPrefixMapping(self, prefix, uri):
        if not hasattr(self, '_xmlns_attrs'):
            self._xmlns_attrs = []
        self._xmlns_attrs.append((prefix or 'xmlns', uri))
        self._ns_contexts.append(self._current_context.copy())
        self._current_context[uri] = prefix or None

    def endPrefixMapping(self, prefix):
        self._current_context = self._ns_contexts.pop()

    def _qualify(self, uri, localname):
        prefix = self._current_context.get(uri)
        if prefix:
            return "%s:%s" % (prefix, localname)
        return localname

    # elements

    def startElementNS(self, name, tagName, attrs):
        uri, localname = name
        if uri:
            if tagName is None:
                tagName = self._qualify(uri, localname)
            if self.document is not None:
                node = self.document.createElementNS(uri, tagName)
            else:
                node = self.buildDocument(uri, tagName)
        else:
            if self.document is not None:
                node = self.document.createElement(localname)
            else:
                node = self.buildDocument(None, localname)

        # namespace declarations become xmlns attributes
        for aname, value in self._xmlns_attrs:
            if aname == 'xmlns':
                qname = aname
            else:
                qname = 'xmlns:' + aname
            attr = self.document.createAttributeNS(_XMLNS_NAMESPACE, qname)
            attr.value = value
            node.setAttributeNodeNS(attr)
        self._xmlns_attrs = []

        for aname, value in attrs.items():
            a_uri, a_localname = aname
            if a_uri == _XMLNS_NAMESPACE:
                if a_localname == 'xmlns':
                    qname = a_localname
                else:
                    qname = 'xmlns:' + a_localname
                attr = self.document.createAttributeNS(a_uri, qname)
                node.setAttributeNodeNS(attr)
            elif a_uri:
                qname = self._qualify(a_uri, a_localname)
                attr = self.document.createAttributeNS(a_uri, qname)
                node.setAttributeNodeNS(attr)
            else:
                attr = self.document.createAttribute(a_localname)
                node.setAttributeNode(attr)
            attr.value = value

        self._push(START_ELEMENT, node)
        self.elementStack.append(node)

    def endElementNS(self, name, tagName):
        self._push(END_ELEMENT, self.pop())

    def startElement(self, name, attrs):
        if self.document is not None:
            node = self.document.createElement(name)
        else:
            node = self.buildDocument(None, name)
        for aname, value in attrs.items():
            attr = self.document.createAttribute(aname)
            attr.value = value
            node.setAttributeNode(attr)
        self._push(START_ELEMENT, node)
        self.elementStack.append(node)

    def endElement(self, name):
        self._push(END_ELEMENT, self.pop())

    # other content

    def comment(self, s):
        if self.document is not None:
            self._push(COMMENT, self.document.createComment(s))
        else:
            self.pending_events.append((COMMENT, 'createComment', (s,)))

    def processingInstruction(self, target, data):
        if self.document is not None:
            node = self.document.createProcessingInstruction(target, data)
            self._push(PROCESSING_INSTRUCTION, node)
        else:
            self.pending_events.append(
                (PROCESSING_INSTRUCTION, 'createProcessingInstruction',
                 (target, data)))

    def ignorableWhitespace(self, chars):
        self._push(IGNORABLE_WHITESPACE, self.document.createTextNode(chars))

    def characters(self, chars):
        self._push(CHARACTERS, self.document.createTextNode(chars))

    # the document

    def startDocument(self):
        if self.documentFactory is None:
            import xml.dom.minidom
            self.documentFactory = xml.dom.minidom.Document.implementation

    def buildDocument(self, uri, tagname):
        """Create the Document (with *tagname* as its root element) and
        return the root element."""
        doc = self.documentFactory.createDocument(uri, tagname, None)
        self.document = doc
        self._push(START_DOCUMENT, doc)
        self.elementStack.append(doc)
        for event, factory, args in self.pending_events:
            self._push(event, getattr(doc, factory)(*args))
        self.pending_events = None
        return doc.documentElement

    def endDocument(self):
        self._push(END_DOCUMENT, self.document)
        self.pop()

    def clear(self):
        """Drop all references so the DOM can be collected."""
        self.document = None


class ErrorHandler:
    def warning(self, exception):
        print(exception)

    def error(self, exception):
        raise exception

    def fatalError(self, exception):
        raise exception


class DOMEventStream:
    """Iterator over the (event, node) pairs of a document read from
    *stream* through the SAX *parser*, *bufsize* bytes at a time."""

    def __init__(self, stream, parser, bufsize):
        self.stream = stream
        self.parser = parser
        self.bufsize = bufsize
        if not hasattr(self.parser, 'feed'):
            # a parser that can only parse whole documents
            self.getEvent = self._slurp
        self.reset()

    def reset(self):
        self.pulldom = PullDOM()
        self.parser.setFeature(xml.sax.handler.feature_namespaces, 1)
        self.parser.setContentHandler(self.pulldom)

    def __next__(self):
        rc = self.getEvent()
        if rc:
            return rc
        raise StopIteration

    def __iter__(self):
        return self

    def expandNode(self, node):
        """Consume events up to the end of *node*, building its subtree."""
        parents = [node]
        event = self.getEvent()
        while event:
            token, cur_node = event
            if cur_node is node:
                return
            if token != END_ELEMENT:
                parents[-1].appendChild(cur_node)
            if token == START_ELEMENT:
                parents.append(cur_node)
            elif token == END_ELEMENT:
                del parents[-1]
            event = self.getEvent()

    def getEvent(self):
        events = self.pulldom.events
        while not events:
            buf = self.stream.read(self.bufsize)
            if not buf:
                self.parser.close()
                return None
            self.parser.feed(buf)
        return events.popleft()

    def _slurp(self):
        """Parse everything at once, then hand out the queued events."""
        self.parser.parse(self.stream)
        self.getEvent = self._emit
        return self._emit()

    def _emit(self):
        events = self.pulldom.events
        if events:
            return events.popleft()
        return None

    def clear(self):
        """Release the parser, the stream and the partial document."""
        self.pulldom.clear()
        del self.pulldom
        self.parser = None
        self.stream = None


class SAX2DOM(PullDOM):
    """A PullDOM that also links every node into the document tree."""

    def startElementNS(self, name, tagName, attrs):
        PullDOM.startElementNS(self, name, tagName, attrs)
        self.elementStack[-2].appendChild(self.elementStack[-1])

    def startElement(self, name, attrs):
        PullDOM.startElement(self, name, attrs)
        self.elementStack[-2].appendChild(self.elementStack[-1])

    def processingInstruction(self, target, data):
        PullDOM.processingInstruction(self, target, data)
        self.elementStack[-1].appendChild(self._last_node)

    def comment(self, s):
        PullDOM.comment(self, s)
        self.elementStack[-1].appendChild(self._last_node)

    def ignorableWhitespace(self, chars):
        PullDOM.ignorableWhitespace(self, chars)
        self.elementStack[-1].appendChild(self._last_node)

    def characters(self, chars):
        PullDOM.characters(self, chars)
        self.elementStack[-1].appendChild(self._last_node)


default_bufsize = (2 ** 14) - 20


def parse(stream_or_string, parser=None, bufsize=None):
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
    import io
    bufsize = len(string)
    if isinstance(string, str):
        buf = io.StringIO(string)
    else:
        buf = io.BytesIO(string)
    if not parser:
        parser = xml.sax.make_parser()
    return DOMEventStream(buf, parser, bufsize)
