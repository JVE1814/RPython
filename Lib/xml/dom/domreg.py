"""Registry of DOM implementations (getDOMImplementation)."""

import sys

# name -> factory returning a DOMImplementation
registered = {}

well_known_implementations = {
    'minidom': 'xml.dom.minidom',
    '4DOM': 'xml.dom.DOMImplementation',
}


def registerDOMImplementation(name, factory):
    """Register *factory* (a callable returning a DOMImplementation) under
    *name*."""
    registered[name] = factory


def _good_enough(dom, features):
    # does the implementation support all (feature, version) pairs?
    for feature, version in features:
        if not dom.hasFeature(feature, version):
            return False
    return True


def getDOMImplementation(name=None, features=()):
    """Return a DOM implementation, by name or by the features it must
    support (a sequence of (feature, version) pairs or a string like
    'core 2.0 xml')."""
    import os
    creator = None
    mod = well_known_implementations.get(name)
    if mod:
        mod = __import__(mod, {}, {}, ['getDOMImplementation'])
        return mod.getDOMImplementation()
    elif name:
        return registered[name]()
    elif not sys.flags.ignore_environment and "PYTHON_DOM" in os.environ:
        return getDOMImplementation(name=os.environ["PYTHON_DOM"])

    if isinstance(features, str):
        features = _parse_feature_string(features)
    for creator in registered.values():
        dom = creator()
        if _good_enough(dom, features):
            return dom

    for creator in well_known_implementations.keys():
        try:
            dom = getDOMImplementation(name=creator)
        except Exception:
            continue
        if _good_enough(dom, features):
            return dom

    raise ImportError("no suitable DOM implementation found")


def _parse_feature_string(s):
    features = []
    parts = s.split()
    i = 0
    length = len(parts)
    while i < length:
        feature = parts[i]
        if feature[0] in "0123456789":
            raise ValueError("bad feature name: %r" % (feature,))
        i = i + 1
        version = None
        if i < length:
            v = parts[i]
            if v[0] in "0123456789":
                i = i + 1
                version = v
        features.append((feature, version))
    return tuple(features)
