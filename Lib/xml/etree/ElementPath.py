"""Limited XPath support for ElementTree's find/findall/findtext/iterfind.

Supported syntax: tag names (with {uri} or prefix: namespaces and the
{*} / {}* wildcards), '*', '.', '..', '//' and the predicates [@attr],
[@attr='value'], [@attr!='value'], [tag], [tag='text'], [tag!='text'],
[.='text'], [.!='text'], [N], [last()] and [last()-N].
"""

import re

xpath_tokenizer_re = re.compile(
    r"("
    r"'[^']*'|\"[^\"]*\"|"         # quoted strings
    r"::|//?|\.\.|\(\)|!=|"         # multi character operators
    r"[/.*:\[\]\(\)@=]"             # single character operators
    r")|"
    r"((?:\{[^}]*\})?[^/\[\]\(\)@!=\s]+)|"   # names, possibly {uri}name
    r"\s+"
)


def xpath_tokenizer(pattern, namespaces=None):
    """Yield (operator, name) pairs for an ElementPath expression.

    Prefixed names are resolved with *namespaces*; the '' entry gives the
    default namespace for (unprefixed) element names.
    """
    default_ns = namespaces.get('') if namespaces else None
    after_at = False
    for op, name in xpath_tokenizer_re.findall(pattern):
        if name and name[0] != '{':
            if ':' in name:
                prefix, local = name.split(':', 1)
                try:
                    if not namespaces:
                        raise KeyError
                    yield op, '{%s}%s' % (namespaces[prefix], local)
                except KeyError:
                    raise SyntaxError("prefix %r not found in prefix map"
                                      % prefix) from None
            elif default_ns and not after_at:
                yield op, '{%s}%s' % (default_ns, name)
            else:
                yield op, name
        else:
            yield op, name
        after_at = op == '@'


def get_parent_map(context):
    parent_map = context.parent_map
    if parent_map is None:
        parent_map = context.parent_map = {}
        for parent in context.root.iter():
            for child in parent:
                parent_map[child] = parent
    return parent_map


def _is_wildcard_tag(tag):
    return tag[:3] == '{*}' or tag[-2:] == '}*'


def _tag_matcher(tag):
    """A predicate on element tags for a (possibly wildcard) name."""
    if tag == '{*}*':
        return lambda t: isinstance(t, str)
    if tag == '{}*':
        return lambda t: isinstance(t, str) and t[:1] != '{'
    if tag[:3] == '{*}':
        local = tag[3:]
        suffix = '}' + local
        return lambda t: t == local or (isinstance(t, str) and t.endswith(suffix)
                                        and t[:1] == '{')
    if tag[-2:] == '}*':
        ns = tag[:-1]
        return lambda t: isinstance(t, str) and t.startswith(ns)
    raise RuntimeError("internal parser error, got %s" % tag)


def _select_children(tag):
    if tag == '*':
        def select(context, result):
            for elem in result:
                for child in elem:
                    if isinstance(child.tag, str):
                        yield child
        return select
    if _is_wildcard_tag(tag):
        match = _tag_matcher(tag)

        def select(context, result):
            for elem in result:
                for child in elem:
                    if match(child.tag):
                        yield child
        return select

    def select(context, result):
        for elem in result:
            for child in elem:
                if child.tag == tag:
                    yield child
    return select


def prepare_child(next, token):
    return _select_children(token[1])


def prepare_star(next, token):
    return _select_children('*')


def prepare_self(next, token):
    def select(context, result):
        yield from result
    return select


def prepare_descendant(next, token):
    try:
        token = next()
    except StopIteration:
        return
    if token[0] == '*':
        tag = '*'
    elif not token[0]:
        tag = token[1]
    else:
        raise SyntaxError("invalid descendant")
    if _is_wildcard_tag(tag):
        match = _tag_matcher(tag)

        def select(context, result):
            for elem in result:
                for e in elem.iter():
                    if e is not elem and match(e.tag):
                        yield e
        return select
    if tag == '*':
        tag = None

    def select(context, result):
        for elem in result:
            for e in elem.iter(tag):
                if e is not elem:
                    yield e
    return select


def prepare_parent(next, token):
    def select(context, result):
        parent_map = get_parent_map(context)
        seen = set()
        for elem in result:
            parent = parent_map.get(elem)
            if parent is not None and id(parent) not in seen:
                seen.add(id(parent))
                yield parent
    return select


def _children_with_tag(context, parent, tag):
    # cached per evaluation, so that index predicates stay linear
    cache = context.sibling_cache
    key = (id(parent), tag)
    found = cache.get(key)
    if found is None:
        found = cache[key] = (parent, list(parent.findall(tag)))
    return found[1]


def prepare_predicate(next, token):
    # Collect the predicate as a signature of token kinds plus values.
    signature = []
    values = []
    while True:
        try:
            token = next()
        except StopIteration:
            return
        if token[0] == ']':
            break
        if token == ('', ''):
            continue
        if token[0] and token[0][:1] in "'\"":
            token = "'", token[0][1:-1]
        signature.append(token[0] or "-")
        values.append(token[1])
    signature = ''.join(signature)

    if signature == '@-':
        key = values[1]

        def select(context, result):
            for elem in result:
                if elem.get(key) is not None:
                    yield elem
        return select
    if signature in ("@-='", "@-!='"):
        key = values[1]
        value = values[-1]
        if signature == "@-='":
            def select(context, result):
                for elem in result:
                    if elem.get(key) == value:
                        yield elem
        else:
            def select(context, result):
                for elem in result:
                    if elem.get(key) != value:
                        yield elem
        return select
    if signature == '-' and not re.match(r'-?\d+$', values[0]):
        tag = values[0]

        def select(context, result):
            for elem in result:
                if elem.find(tag) is not None:
                    yield elem
        return select
    if signature in ("-='", "-!='") and not re.match(r'-?\d+$', values[0]):
        tag = values[0]
        value = values[-1]
        equal = signature == "-='"
        if tag == '.':
            def select(context, result):
                for elem in result:
                    if (''.join(elem.itertext()) == value) == equal:
                        yield elem
        else:
            def select(context, result):
                for elem in result:
                    for child in elem.findall(tag):
                        if (''.join(child.itertext()) == value) == equal:
                            yield elem
                            break
        return select
    if signature in (".='", ".!='"):
        value = values[-1]
        equal = signature == ".='"

        def select(context, result):
            for elem in result:
                if (''.join(elem.itertext()) == value) == equal:
                    yield elem
        return select
    if signature in ('-', '-()', '-()-'):
        if signature == '-':
            index = int(values[0]) - 1
            if index < 0:
                raise SyntaxError("XPath position >= 1 expected")
        else:
            if values[0] != 'last':
                raise SyntaxError("unsupported function")
            if signature == '-()-':
                try:
                    index = int(values[2]) - 1
                except ValueError:
                    raise SyntaxError("unsupported expression")
                if index > -2:
                    raise SyntaxError("XPath offset from last() must be negative")
            else:
                index = -1

        def select(context, result):
            parent_map = get_parent_map(context)
            for elem in result:
                parent = parent_map.get(elem)
                if parent is None:
                    continue
                siblings = _children_with_tag(context, parent, elem.tag)
                try:
                    if siblings[index] is elem:
                        yield elem
                except IndexError:
                    pass
        return select
    raise SyntaxError("invalid predicate")


ops = {
    '': prepare_child,
    '*': prepare_star,
    '.': prepare_self,
    '..': prepare_parent,
    '//': prepare_descendant,
    '[': prepare_predicate,
}

_cache = {}
_MAX_CACHE = 100


class _SelectorContext:
    parent_map = None

    def __init__(self, root):
        self.root = root
        self.sibling_cache = {}


def iterfind(elem, path, namespaces=None):
    """Yield the elements matching *path* below *elem*."""
    if path[-1:] == '/':
        path = path + '*'
    cache_key = (path,)
    if namespaces:
        cache_key += tuple(sorted(namespaces.items()))
    try:
        selector = _cache[cache_key]
    except KeyError:
        if len(_cache) > _MAX_CACHE:
            _cache.clear()
        if path[:1] == '/':
            raise SyntaxError("cannot use absolute path on element")
        tokens = iter(xpath_tokenizer(path, namespaces))
        next_token = tokens.__next__
        try:
            token = next_token()
        except StopIteration:
            return iter(())
        selector = []
        while True:
            try:
                selector.append(ops[token[0]](next_token, token))
            except StopIteration:
                raise SyntaxError("invalid path") from None
            except KeyError:
                raise SyntaxError("unsupported path syntax") from None
            try:
                token = next_token()
                if token[0] == '/':
                    token = next_token()
            except StopIteration:
                break
        _cache[cache_key] = selector
    result = [elem]
    context = _SelectorContext(elem)
    for select in selector:
        if select is None:
            return iter(())
        result = select(context, result)
    return iter(result)


def find(elem, path, namespaces=None):
    """The first matching element, or None."""
    return next(iterfind(elem, path, namespaces), None)


def findall(elem, path, namespaces=None):
    """A list of all matching elements."""
    return list(iterfind(elem, path, namespaces))


def findtext(elem, path, default=None, namespaces=None):
    """The text of the first matching element ('' if it has none), or
    *default* if nothing matches."""
    try:
        elem = next(iterfind(elem, path, namespaces))
    except StopIteration:
        return default
    text = elem.text
    return '' if text is None else text
