"""Random values for security-sensitive uses (tokens, passwords, keys).

Everything here draws from the operating system's randomness source via
random.SystemRandom; never use the default `random` generator for secrets.
"""

__all__ = ['choice', 'randbelow', 'randbits', 'SystemRandom',
           'token_bytes', 'token_hex', 'token_urlsafe',
           'compare_digest',
           ]

from base64 import urlsafe_b64encode
from hmac import compare_digest
from random import SystemRandom

_sysrand = SystemRandom()

randbits = _sysrand.getrandbits
choice = _sysrand.choice

# Bytes of randomness used by the token_* helpers when no size is given.
DEFAULT_ENTROPY = 32


def randbelow(exclusive_upper_bound):
    """Return a uniformly chosen int n with 0 <= n < exclusive_upper_bound."""
    if exclusive_upper_bound <= 0:
        raise ValueError("Upper bound must be positive.")
    return _sysrand._randbelow(exclusive_upper_bound)


def token_bytes(nbytes=None):
    """Return *nbytes* random bytes (DEFAULT_ENTROPY when omitted/None)."""
    return _sysrand.randbytes(DEFAULT_ENTROPY if nbytes is None else nbytes)


def token_hex(nbytes=None):
    """Return token_bytes(nbytes) as a string of hex digits."""
    return token_bytes(nbytes).hex()


def token_urlsafe(nbytes=None):
    """Return token_bytes(nbytes) as unpadded URL-safe Base64 text."""
    raw = urlsafe_b64encode(token_bytes(nbytes))
    return raw.rstrip(b'=').decode('ascii')
