"""Keyed-hashing for message authentication (HMAC, RFC 2104).

new(key, msg=None, digestmod) returns an HMAC object; digest(key, msg, digest)
computes an HMAC in one call.  compare_digest() compares digests in constant
time.
"""

try:
    import _hashlib as _hashopenssl
except ImportError:
    _hashopenssl = None
    _functype = None
    from _operator import _compare_digest as compare_digest
else:
    compare_digest = _hashopenssl.compare_digest
    _functype = type(_hashopenssl.openssl_sha256)

try:
    import _hmac
except ImportError:
    _hmac = None

trans_5C = bytes(x ^ 0x5C for x in range(256))
trans_36 = bytes(x ^ 0x36 for x in range(256))

# HMAC objects expose their own digest_size; this module-level value is kept
# for backwards compatibility only.
digest_size = None

__all__ = ['HMAC', 'new', 'digest', 'compare_digest']


def _get_digest_constructor(digest_like):
    """Turn a digestmod (callable, name or module) into a constructor."""
    if callable(digest_like):
        return digest_like
    if isinstance(digest_like, str):
        def digest_wrapper(d=b''):
            import hashlib
            return hashlib.new(digest_like, d)
    else:
        def digest_wrapper(d=b''):
            return digest_like.new(d)
    return digest_wrapper


def _is_shake_constructor(digest_like):
    if isinstance(digest_like, str):
        name = digest_like
    else:
        h = digest_like() if callable(digest_like) else digest_like.new()
        if not isinstance(name := getattr(h, "name", None), str):
            return False
    return name.startswith(("shake", "SHAKE"))


class HMAC:
    """RFC 2104 HMAC class.  Also complies with RFC 4231.

    This supports the API for Cryptographic Hash Functions (PEP 247).
    """

    blocksize = 64  # fallback block size used by _init_old()

    __slots__ = ("_hmac", "_inner", "_outer", "block_size", "digest_size")

    def __init__(self, key, msg=None, digestmod=''):
        """Create a new HMAC object.

        key: bytes or buffer, key for the keyed hash object.
        msg: bytes or buffer, initial input for the hash or None.
        digestmod: a hash name, a constructor returning a hash object, or a
                   module supporting PEP 247.  Required.
        """
        if not isinstance(key, (bytes, bytearray)):
            raise TypeError("key: expected bytes or bytearray, "
                            f"but got {type(key).__name__!r}")
        if not digestmod:
            raise TypeError("Missing required parameter 'digestmod'.")

        if _hashopenssl and isinstance(digestmod, (str, _functype)):
            try:
                self._init_openssl_hmac(key, msg, digestmod)
                return
            except _hashopenssl.UnsupportedDigestmodError:
                pass
        if _hmac and isinstance(digestmod, str):
            try:
                self._init_builtin_hmac(key, msg, digestmod)
                return
            except _hmac.UnknownHashError:
                pass
        self._init_old(key, msg, digestmod)

    def _init_openssl_hmac(self, key, msg, digestmod):
        self._hmac = _hashopenssl.hmac_new(key, msg, digestmod=digestmod)
        self._inner = self._outer = None
        self.digest_size = self._hmac.digest_size
        self.block_size = self._hmac.block_size

    _init_hmac = _init_openssl_hmac  # older private name

    def _init_builtin_hmac(self, key, msg, digestmod):
        self._hmac = _hmac.new(key, msg, digestmod=digestmod)
        self._inner = self._outer = None
        self.digest_size = self._hmac.digest_size
        self.block_size = self._hmac.block_size

    def _init_old(self, key, msg, digestmod):
        import warnings

        digest_cons = _get_digest_constructor(digestmod)
        if _is_shake_constructor(digest_cons):
            raise ValueError(f"unsupported hash type {digestmod}")

        self._hmac = None
        self._outer = digest_cons()
        self._inner = digest_cons()
        self.digest_size = self._inner.digest_size

        if hasattr(self._inner, 'block_size'):
            blocksize = self._inner.block_size
            if blocksize < 16:
                warnings.warn(f'block_size of {blocksize} seems too small; '
                              f'using our default of {self.blocksize}.',
                              RuntimeWarning, 2)
                blocksize = self.blocksize
        else:
            warnings.warn('No block_size attribute on given digest object; '
                          f'Assuming {self.blocksize}.',
                          RuntimeWarning, 2)
            blocksize = self.blocksize

        if len(key) > blocksize:
            key = digest_cons(key).digest()

        self.block_size = blocksize
        key = key.ljust(blocksize, b'\0')
        self._outer.update(key.translate(trans_5C))
        self._inner.update(key.translate(trans_36))
        if msg is not None:
            self.update(msg)

    @property
    def name(self):
        if self._hmac:
            return self._hmac.name
        return f"hmac-{self._inner.name}"

    def update(self, msg):
        """Feed data from msg into this hashing object."""
        (self._hmac or self._inner).update(msg)

    def copy(self):
        """Return a separate copy of this hashing object.

        An update to this copy won't affect the original object.
        """
        other = self.__class__.__new__(self.__class__)
        other.digest_size = self.digest_size
        other.block_size = self.block_size
        if self._hmac:
            other._hmac = self._hmac.copy()
            other._inner = other._outer = None
        else:
            other._hmac = None
            other._inner = self._inner.copy()
            other._outer = self._outer.copy()
        return other

    def _current(self):
        """Return a hash object for the current state."""
        if self._hmac:
            return self._hmac
        h = self._outer.copy()
        h.update(self._inner.digest())
        return h

    def digest(self):
        """Return the hash value of this hashing object.

        This returns the hmac value as bytes.  The object is
        not altered in any way by this function; you can continue
        updating the object after calling this function.
        """
        return self._current().digest()

    def hexdigest(self):
        """Like digest(), but returns a string of hexadecimal digits instead.
        """
        return self._current().hexdigest()


def new(key, msg=None, digestmod=''):
    """Create a new hashing object and return it.

    key: bytes or buffer, the starting key for the hash.
    msg: bytes or buffer, initial input for the hash, or None.
    digestmod: a hash name, constructor or PEP 247 module.  Required.
    """
    return HMAC(key, msg, digestmod)


def digest(key, msg, digest):
    """Fast inline implementation of HMAC.

    key: bytes or buffer, the key for the keyed hash object.
    msg: bytes or buffer, input message.
    digest: a hash name, constructor or PEP 247 module.
    """
    if _hashopenssl and isinstance(digest, (str, _functype)):
        try:
            return _hashopenssl.hmac_digest(key, msg, digest)
        except OverflowError:
            return _compute_digest_fallback(key, msg, digest)
        except _hashopenssl.UnsupportedDigestmodError:
            pass

    if _hmac and isinstance(digest, str):
        try:
            return _hmac.compute_digest(key, msg, digest)
        except (OverflowError, _hmac.UnknownHashError):
            pass

    return _compute_digest_fallback(key, msg, digest)


def _compute_digest_fallback(key, msg, digest):
    digest_cons = _get_digest_constructor(digest)
    if _is_shake_constructor(digest_cons):
        raise ValueError(f"unsupported hash type {digest}")
    inner = digest_cons()
    outer = digest_cons()
    blocksize = getattr(inner, 'block_size', 64)
    if len(key) > blocksize:
        key = digest_cons(key).digest()
    key = key.ljust(blocksize, b'\0')
    inner.update(key.translate(trans_36))
    outer.update(key.translate(trans_5C))
    inner.update(msg)
    outer.update(inner.digest())
    return outer.digest()
