"""Locations of the online documentation of standard library modules,
relative to the library reference (used by pydoc's MODULE REFERENCE
links)."""

import sys as _sys

# Module name -> "<page>#<anchor>" in the library reference.  Every public
# standard library module is documented on the page named after it.
module_docs = {
    name: f'{name.lower()}#module-{name}'
    for name in sorted(_sys.stdlib_module_names)
    if not name.startswith('_')
}

# Submodules documented on their own pages.
for _name in (
    'collections.abc', 'concurrent.futures', 'email.message', 'email.parser',
    'email.utils', 'encodings.idna', 'html.parser', 'http.client',
    'http.server', 'importlib.metadata', 'importlib.resources', 'json.tool',
    'logging.config', 'logging.handlers', 'multiprocessing.pool', 'os.path',
    'unittest.mock', 'urllib.parse', 'urllib.request', 'xml.dom',
    'xml.dom.minidom', 'xml.etree.ElementTree', 'xml.sax',
):
    module_docs[_name] = f'{_name.lower()}#module-{_name}'

module_docs['encodings'] = 'codecs#module-encodings'
module_docs['encodings.idna'] = 'codecs#module-encodings.idna'
del _name
