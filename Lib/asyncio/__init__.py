"""Asynchronous I/O: event loops, coroutines and tasks, streams,
subprocesses, synchronization primitives and queues."""

import sys

from .base_events import *
from .coroutines import *
from .events import *
from .exceptions import *
from .futures import *
from .graph import *
from .locks import *
from .protocols import *
from .queues import *
from .runners import *
from .streams import *
from .subprocess import *
from .tasks import *
from .taskgroups import *
from .timeouts import *
from .threads import *
from .transports import *

__all__ = (base_events.__all__ +
           coroutines.__all__ +
           events.__all__ +
           exceptions.__all__ +
           futures.__all__ +
           graph.__all__ +
           locks.__all__ +
           protocols.__all__ +
           queues.__all__ +
           runners.__all__ +
           streams.__all__ +
           subprocess.__all__ +
           tasks.__all__ +
           taskgroups.__all__ +
           threads.__all__ +
           timeouts.__all__ +
           transports.__all__)

if sys.platform == 'win32':  # pragma: no cover
    from .windows_events import *
    __all__ += windows_events.__all__
else:
    from .unix_events import *
    from .unix_events import _DefaultEventLoopPolicy
    __all__ += unix_events.__all__


# Names kept for compatibility; using them emits a DeprecationWarning.
_DEPRECATED_NAMES = {
    "AbstractEventLoopPolicy": ("events", "_AbstractEventLoopPolicy"),
    "DefaultEventLoopPolicy": (None, "_DefaultEventLoopPolicy"),
    "WindowsSelectorEventLoopPolicy": ("windows_events",
                                       "_WindowsSelectorEventLoopPolicy"),
    "WindowsProactorEventLoopPolicy": ("windows_events",
                                       "_WindowsProactorEventLoopPolicy"),
}


def __getattr__(name):
    if name not in _DEPRECATED_NAMES:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    module_name, attr = _DEPRECATED_NAMES[name]
    if module_name == "windows_events" and sys.platform != 'win32':
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    import warnings
    warnings._deprecated(f"asyncio.{name}", remove=(3, 16))
    source = globals() if module_name is None else \
        vars(globals()[module_name])
    return source[attr]
