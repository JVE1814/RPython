"""Pieces shared by the cross-interpreter queue and channel modules: what to
do with an item whose sending interpreter no longer exists."""


class ItemInterpreterDestroyed(Exception):
    """The interpreter that sent an item has been destroyed since."""


class UnboundItem:
    """Placeholder handed out instead of an item whose interpreter is gone.

    Each module that hands out such placeholders makes its own singleton with
    UnboundItem.singleton(kind, module_name).
    """

    __slots__ = ('_where',)

    @classmethod
    def singleton(cls, kind, module, name='UNBOUND'):
        # "kind" ('queue', 'channel', ...) only documents the caller
        obj = super().__new__(cls)
        obj._where = f'{module}.{name}'
        return obj

    def __new__(cls):
        raise TypeError(f'use {cls.__name__}.singleton() instead')

    def __repr__(self):
        return self._where


UNBOUND = UnboundItem.singleton('', __name__)
UNBOUND_ERROR = object()
UNBOUND_REMOVE = object()

# the integer flags the low-level modules use
_UNBOUND_CONSTANT_TO_FLAG = {
    UNBOUND_REMOVE: 1,
    UNBOUND_ERROR: 2,
    UNBOUND: 3,
}
_UNBOUND_FLAG_TO_CONSTANT = {flag: const
                             for const, flag in _UNBOUND_CONSTANT_TO_FLAG.items()}


def serialize_unbound(unbound):
    """Return the low-level form (a 1-tuple) of an "unbounditems" value."""
    try:
        flag = _UNBOUND_CONSTANT_TO_FLAG[unbound]
    except (KeyError, TypeError):
        raise NotImplementedError(f'unsupported unbound replacement {unbound!r}')
    return (flag,)


def resolve_unbound(flag, exctype_destroyed):
    """Turn the flag of an unbound item into what get() hands out (or raise)."""
    try:
        const = _UNBOUND_FLAG_TO_CONSTANT[flag]
    except KeyError:
        raise NotImplementedError(f'unsupported unbound flag {flag!r}')
    if const is UNBOUND_REMOVE:
        # removed items are never handed out
        raise AssertionError('an UNBOUND_REMOVE item was not removed')
    if const is UNBOUND_ERROR:
        raise exctype_destroyed('item\'s original interpreter destroyed')
    return const
