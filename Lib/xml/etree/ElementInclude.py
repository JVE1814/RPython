"""XInclude processing for ElementTree trees (a limited implementation)."""

import copy
from . import ElementTree

XINCLUDE = "{http://www.w3.org/2001/XInclude}"

XINCLUDE_INCLUDE = XINCLUDE + "include"
XINCLUDE_FALLBACK = XINCLUDE + "fallback"

# Default for the 'max_depth' argument of include().
DEFAULT_MAX_INCLUSION_DEPTH = 6


class FatalIncludeError(SyntaxError):
    pass


class LimitedRecursiveIncludeError(FatalIncludeError):
    pass


def default_loader(href, parse, encoding=None):
    """Load *href*: as a tree (parse='xml') or as text (parse='text')."""
    if parse == "xml":
        with open(href, 'rb') as file:
            data = ElementTree.parse(file).getroot()
    else:
        if not encoding:
            encoding = 'UTF-8'
        with open(href, 'r', encoding=encoding) as file:
            data = file.read()
    return data


def include(elem, loader=None, base_url=None,
            max_depth=DEFAULT_MAX_INCLUSION_DEPTH):
    """Expand xi:include elements in the tree *elem* (in place).

    *loader*(href, parse[, encoding]) loads resources (default_loader);
    *max_depth* limits nested inclusions (None means no limit).
    """
    if max_depth is None:
        max_depth = -1
    elif max_depth < 0:
        raise ValueError("expected non-negative depth or None for "
                         "'max_depth', got %r" % max_depth)
    if hasattr(elem, 'getroot'):
        elem = elem.getroot()
    if loader is None:
        loader = default_loader
    _include(elem, loader, base_url, max_depth, set())


def _include(elem, loader, base_url, max_depth, active):
    i = 0
    while i < len(elem):
        child = elem[i]
        if child.tag == XINCLUDE_INCLUDE:
            href = child.get("href")
            if base_url:
                from urllib.parse import urljoin
                href = urljoin(base_url, href)
            parse = child.get("parse", "xml")
            if parse == "xml":
                if href in active:
                    raise FatalIncludeError("recursive include of %s" % href)
                if max_depth == 0:
                    raise LimitedRecursiveIncludeError(
                        "maximum xinclude depth reached when including "
                        "file %s" % href)
                active.add(href)
                node = loader(href, parse)
                if node is None:
                    raise FatalIncludeError(
                        "cannot load %r as %r" % (href, parse))
                node = copy.copy(node)
                _include(node, loader, href, max_depth - 1, active)
                active.discard(href)
                if child.tail:
                    node.tail = (node.tail or "") + child.tail
                elem[i] = node
            elif parse == "text":
                text = loader(href, parse, child.get("encoding"))
                if text is None:
                    raise FatalIncludeError(
                        "cannot load %r as %r" % (href, parse))
                if child.tail:
                    text += child.tail
                if i:
                    previous = elem[i - 1]
                    previous.tail = (previous.tail or "") + text
                else:
                    elem.text = (elem.text or "") + text
                del elem[i]
                continue
            else:
                raise FatalIncludeError(
                    "unknown parse type in xi:include tag (%r)" % parse)
        elif child.tag == XINCLUDE_FALLBACK:
            raise FatalIncludeError(
                "xi:fallback tag must be child of xi:include (%r)" % child.tag)
        else:
            _include(child, loader, base_url, max_depth, active)
        i += 1
