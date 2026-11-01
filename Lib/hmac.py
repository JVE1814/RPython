"""Message authentication codes built from hash functions (RFC 2104).

Create objects with new(key, msg, digestmod) / HMAC(...), or compute a MAC in
a single call with digest(key, msg, digest).  compare_digest() performs a
comparison whose duration does not depend on where the inputs differ.
"""

try:
    import _hashlib as _hashopenssl
except ImportError:
    _hashopenssl = None
    _functype = None
    from _operator import _compare_digest as compare_digest
else:
    compare_digest = _hashopenssl.compare_digest
    # Type of the accelerated named constructors such as openssl_sha256.
    _functype = type(_hashopenssl.openssl_sha256)

try:
    import _hmac
except ImportError:
    _hmac = None

__all__ = ['HMAC', 'new', 'digest', 'compare_digest']

# Translation tables applying the outer (0x5C) and inner (0x36) pads.
trans_5C = bytes(i ^ 0x5C for i in range(256))
trans_36 = bytes(i ^ 0x36 for i in range(256))

# Kept for compatibility; the real size is an attribute of each HMAC object.
digest_size = None


def _constructor_for(digestmod):
    """Return a callable creating a fresh hash object for *digestmod*."""
    if callable(digestmod):
        return digestmod
    if isinstance(digestmod, str):
        name = digestmod

        def make(data=b''):
            import hashlib
            return hashlib.new(name, data)
        return make

    module = digestmod

    def make(data=b''):
        return module.new(data)
    return make


def _is_xof(constructor):
    """True if *constructor* builds an extendable-output (SHAKE) hash."""
    probe = constructor()
    name = getattr(probe, "name", None)
    return isinstance(name, str) and name.lower().startswith("shake")


def _padded_key(key, blocksize, constructor):
    if len(key) > blocksize:
        key = constructor(key).digest()
    return key.ljust(blocksize, b'\0')


class HMAC:
    """Keyed hash object implementing RFC 2104 (and the RFC 4231 vectors).

    The interface mirrors hash objects: update(), digest(), hexdigest(),
    copy(), plus the name, digest_size and block_size attributes.
    """

    # Block size assumed when the hash object does not report a usable one.
    blocksize = 64

    __slots__ = ("_hmac", "_inner", "_outer", "block_size", "digest_size")

    def __init__(self, key, msg=None, digestmod=''):
        """Prepare a MAC computation.

        key must be bytes or bytearray; msg (optional) is fed through
        update(); digestmod selects the hash: a name, a constructor or a
        module with a new() function.  It has no default.
        """
        if not isinstance(key, (bytes, bytearray)):
            raise TypeError("key: expected bytes or bytearray, "
                            f"but got {type(key).__name__!r}")
        if not digestmod:
            raise TypeError("Missing required parameter 'digestmod'.")

        accelerated = (str,) if _functype is None else (str, _functype)
        if _hashopenssl is not None and isinstance(digestmod, accelerated):
            try:
                return self._init_openssl_hmac(key, msg, digestmod)
            except _hashopenssl.UnsupportedDigestmodError:
                pass
        if _hmac is not None and isinstance(digestmod, str):
            try:
                return self._init_builtin_hmac(key, msg, digestmod)
            except _hmac.UnknownHashError:
                pass
        self._init_old(key, msg, digestmod)

    def _adopt(self, native):
        self._hmac = native
        self._inner = None
        self._outer = None
        self.digest_size = native.digest_size
        self.block_size = native.block_size

    def _init_openssl_hmac(self, key, msg, digestmod):
        self._adopt(_hashopenssl.hmac_new(key, msg, digestmod=digestmod))

    _init_hmac = _init_openssl_hmac

    def _init_builtin_hmac(self, key, msg, digestmod):
        self._adopt(_hmac.new(key, msg, digestmod=digestmod))

    def _init_old(self, key, msg, digestmod):
        make = _constructor_for(digestmod)
        if _is_xof(make):
            raise ValueError(f"unsupported hash type {digestmod}")
        self._hmac = None
        inner = make()
        outer = make()
        self.digest_size = inner.digest_size

        bs = getattr(inner, 'block_size', None)
        if bs is None:
            import warnings
            warnings.warn('No block_size attribute on given digest object; '
                          f'Assuming {self.blocksize}.', RuntimeWarning, 2)
            bs = self.blocksize
        elif bs < 16:
            import warnings
            warnings.warn(f'block_size of {bs} seems too small; '
                          f'using our default of {self.blocksize}.',
                          RuntimeWarning, 2)
            bs = self.blocksize
        self.block_size = bs

        padded = _padded_key(key, bs, make)
        inner.update(padded.translate(trans_36))
        outer.update(padded.translate(trans_5C))
        self._inner = inner
        self._outer = outer
        if msg is not None:
            self.update(msg)

    @property
    def name(self):
        if self._hmac is not None:
            return self._hmac.name
        return "hmac-" + self._inner.name

    def update(self, msg):
        """Append msg to the authenticated data."""
        target = self._hmac if self._hmac is not None else self._inner
        target.update(msg)

    def copy(self):
        """Return an independent clone of the current state."""
        cls = type(self)
        clone = cls.__new__(cls)
        clone.digest_size = self.digest_size
        clone.block_size = self.block_size
        if self._hmac is not None:
            clone._hmac = self._hmac.copy()
            clone._inner = clone._outer = None
        else:
            clone._hmac = None
            clone._inner = self._inner.copy()
            clone._outer = self._outer.copy()
        return clone

    def _final(self):
        if self._hmac is not None:
            return self._hmac
        result = self._outer.copy()
        result.update(self._inner.digest())
        return result

    def digest(self):
        """Return the MAC of the data seen so far as bytes (non-destructive)."""
        return self._final().digest()

    def hexdigest(self):
        """Return the MAC of the data seen so far as a hex string."""
        return self._final().hexdigest()


def new(key, msg=None, digestmod=''):
    """Return a new HMAC object; see HMAC for the arguments."""
    return HMAC(key, msg, digestmod)


def digest(key, msg, digest):
    """Compute the HMAC of msg under key in one step.

    digest names the hash function (a name, constructor or module).
    """
    if _hashopenssl is not None and isinstance(
            digest, (str,) if _functype is None else (str, _functype)):
        try:
            return _hashopenssl.hmac_digest(key, msg, digest)
        except OverflowError:
            # Key too large for the accelerated path.
            return _compute_digest_fallback(key, msg, digest)
        except _hashopenssl.UnsupportedDigestmodError:
            pass

    if _hmac is not None and isinstance(digest, str):
        try:
            return _hmac.compute_digest(key, msg, digest)
        except (OverflowError, _hmac.UnknownHashError):
            pass

    return _compute_digest_fallback(key, msg, digest)


def _compute_digest_fallback(key, msg, digest):
    make = _constructor_for(digest)
    if _is_xof(make):
        raise ValueError(f"unsupported hash type {digest}")
    inner = make()
    outer = make()
    padded = _padded_key(key, getattr(inner, 'block_size', 64), make)
    inner.update(padded.translate(trans_36))
    inner.update(msg)
    outer.update(padded.translate(trans_5C))
    outer.update(inner.digest())
    return outer.digest()
