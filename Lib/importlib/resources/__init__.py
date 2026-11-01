"""Read resources contained within a package.

Use ``files(anchor)`` to obtain a Traversable for a package (or module)
and navigate/open its resources; ``as_file()`` gives a real file system
path when one is needed.  The function-style helpers (``read_text``,
``open_binary``, ``path``, ...) are thin conveniences over ``files()``.
"""

from ._common import (
    Anchor,
    Package,
    as_file,
    files,
)
from ._functional import (
    contents,
    is_resource,
    open_binary,
    open_text,
    path,
    read_binary,
    read_text,
)
from .abc import ResourceReader

__all__ = [
    'Package',
    'as_file',
    'contents',
    'files',
    'is_resource',
    'open_binary',
    'open_text',
    'path',
    'read_binary',
    'read_text',
    'Anchor',
    'ResourceReader',
]
