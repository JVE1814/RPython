"""Secure hashes and message digests.

Use new(name, data=b'') or one of the named constructors (md5, sha1, sha224,
sha256, sha384, sha512, blake2b, blake2s, sha3_224, sha3_256, sha3_384,
sha3_512, shake_128, shake_256) to create a hash object.  Hash objects offer
update(data), digest(), hexdigest() and copy(); the SHAKE variable-length
digests take the output length as an argument of digest()/hexdigest().

algorithms_guaranteed lists the names available on every platform and
algorithms_available the names accepted by new() in this interpreter.
Checksums (crc32, adler32) live in the zlib module.
"""

__always_supported = (
    'md5', 'sha1', 'sha224', 'sha256', 'sha384', 'sha512',
    'blake2b', 'blake2s',
    'sha3_224', 'sha3_256', 'sha3_384', 'sha3_512',
    'shake_128', 'shake_256',
)

algorithms_guaranteed = set(__always_supported)
algorithms_available = set(__always_supported)

__all__ = __always_supported + ('new', 'algorithms_guaranteed',
                                'algorithms_available', 'file_digest')

# name -> constructor of the builtin (non-OpenSSL) implementation
__builtin_constructor_cache = {}

# Algorithms for which the builtin implementation is always preferred because
# it supports keys, salts, personalization and tree hashing.
__block_openssl_constructor = {'blake2b', 'blake2s'}

# Which extension module provides which builtin constructors.  Upper-case
# aliases are accepted for the historical names.
__builtin_modules = {
    'md5': ('_md5', ('md5',), True),
    'sha1': ('_sha1', ('sha1',), True),
    'sha224': ('_sha2', ('sha224', 'sha256'), True),
    'sha256': ('_sha2', ('sha224', 'sha256'), True),
    'sha384': ('_sha2', ('sha384', 'sha512'), True),
    'sha512': ('_sha2', ('sha384', 'sha512'), True),
    'blake2b': ('_blake2', ('blake2b', 'blake2s'), False),
    'blake2s': ('_blake2', ('blake2b', 'blake2s'), False),
    'sha3_224': ('_sha3', ('sha3_224', 'sha3_256', 'sha3_384', 'sha3_512'), False),
    'sha3_256': ('_sha3', ('sha3_224', 'sha3_256', 'sha3_384', 'sha3_512'), False),
    'sha3_384': ('_sha3', ('sha3_224', 'sha3_256', 'sha3_384', 'sha3_512'), False),
    'sha3_512': ('_sha3', ('sha3_224', 'sha3_256', 'sha3_384', 'sha3_512'), False),
    'shake_128': ('_sha3', ('shake_128', 'shake_256'), False),
    'shake_256': ('_sha3', ('shake_128', 'shake_256'), False),
}


def __get_builtin_constructor(name):
    if not isinstance(name, str):
        raise TypeError("new() argument 'name' must be str, not "
                        f"{type(name).__name__}")
    cache = __builtin_constructor_cache
    cons = cache.get(name)
    if cons is not None:
        return cons
    key = name.lower() if name.isupper() else name
    entry = __builtin_modules.get(key)
    if entry is not None and (key == name or entry[2]):
        modname, members, upper_alias = entry
        try:
            module = __import__(modname)
        except ImportError:
            module = None
        if module is not None:
            for member in members:
                func = getattr(module, member)
                cache[member] = func
                if upper_alias:
                    cache[member.upper()] = func
    cons = cache.get(name)
    if cons is None:
        raise ValueError(f'unsupported hash algorithm {name}')
    return cons


def __get_openssl_constructor(name):
    if name in __block_openssl_constructor:
        return __get_builtin_constructor(name)
    try:
        func = getattr(_hashlib, 'openssl_' + name)
        # The function may exist while the digest is disabled by policy.
        func(usedforsecurity=False)
    except (AttributeError, ValueError):
        return __get_builtin_constructor(name)
    return func


def __py_new(name, *args, **kwargs):
    """new(name, data=b'', **kwargs) - Return a new hashing object using the
    named algorithm; optionally initialized with data (which must be
    a bytes-like object).
    """
    return __get_builtin_constructor(name)(*args, **kwargs)


def __hash_new(name, *args, **kwargs):
    """new(name, data=b'') - Return a new hashing object using the named algorithm;
    optionally initialized with data (which must be a bytes-like object).
    """
    if name in __block_openssl_constructor:
        return __get_builtin_constructor(name)(*args, **kwargs)
    try:
        return _hashlib.new(name, *args, **kwargs)
    except ValueError:
        # Not provided by the _hashlib backend: try the builtin modules.
        return __get_builtin_constructor(name)(*args, **kwargs)


try:
    import _hashlib
except ImportError:
    _hashlib = None
    new = __py_new
    __get_hash = __get_builtin_constructor
else:
    new = __hash_new
    __get_hash = __get_openssl_constructor
    algorithms_available = algorithms_available.union(
        _hashlib.openssl_md_meth_names)

try:
    from _hashlib import pbkdf2_hmac
except ImportError:
    pass
else:
    __all__ += ('pbkdf2_hmac',)

try:
    from _hashlib import scrypt
except ImportError:
    pass
else:
    __all__ += ('scrypt',)


def file_digest(fileobj, digest, /, *, _bufsize=2**18):
    """Hash the contents of a file-like object. Returns a digest object.

    *fileobj* must be a file-like object opened for reading in binary mode.
    *digest* is a hash algorithm name, a hash constructor or any callable
    returning a hash object.
    """
    hobj = new(digest) if isinstance(digest, str) else digest()

    getbuffer = getattr(fileobj, "getbuffer", None)
    if getbuffer is not None:
        # In-memory binary stream: hash its buffer without copying.
        hobj.update(getbuffer())
        return hobj

    readable = getattr(fileobj, "readable", None)
    if not hasattr(fileobj, "readinto") or readable is None or not readable():
        raise ValueError(
            f"'{fileobj!r}' is not a file-like object in binary reading mode."
        )

    buf = bytearray(_bufsize)
    view = memoryview(buf)
    while True:
        n = fileobj.readinto(buf)
        if n is None:
            raise BlockingIOError("I/O operation would block.")
        if not n:
            break
        hobj.update(view[:n])
    return hobj


for __func_name in __always_supported:
    try:
        globals()[__func_name] = __get_hash(__func_name)
    except ValueError:
        import logging
        logging.exception('code for hash %s was not found.', __func_name)

del __always_supported, __func_name, __get_hash
del __py_new, __hash_new, __get_openssl_constructor
