"""Generate cryptographically strong pseudo-random numbers suitable for
managing secrets such as account authentication, tokens, and similar.

All randomness comes from the operating system (random.SystemRandom).
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


def randbelow(exclusive_upper_bound):
    """Return a random int in the range [0, n)."""
    if exclusive_upper_bound <= 0:
        raise ValueError("Upper bound must be positive.")
    return _sysrand._randbelow(exclusive_upper_bound)


# Default number of random bytes used by the token_* functions.
DEFAULT_ENTROPY = 32


def token_bytes(nbytes=None):
    """Return a random byte string containing *nbytes* bytes.

    If *nbytes* is ``None`` or not supplied, a reasonable
    default is used.
    """
    if nbytes is None:
        nbytes = DEFAULT_ENTROPY
    return _sysrand.randbytes(nbytes)


def token_hex(nbytes=None):
    """Return a random text string, in hexadecimal.

    The string has *nbytes* random bytes, each byte converted to two
    hex digits.  If *nbytes* is ``None`` or not supplied, a reasonable
    default is used.
    """
    return token_bytes(nbytes).hex()


def token_urlsafe(nbytes=None):
    """Return a random URL-safe text string, in Base64 encoding.

    The string has *nbytes* random bytes.  If *nbytes* is ``None``
    or not supplied, a reasonable default is used.
    """
    return urlsafe_b64encode(token_bytes(nbytes)).rstrip(b'=').decode('ascii')
