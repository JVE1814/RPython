"""Zstandard (RFC 8878) compression.

One-shot compress()/decompress(), the incremental ZstdCompressor and
ZstdDecompressor classes, ZstdDict dictionaries (train_dict() builds one from
samples), frame inspection helpers, and ZstdFile / open() for files.
"""

__all__ = (
    # compression.zstd
    'COMPRESSION_LEVEL_DEFAULT', 'compress', 'CompressionParameter',
    'decompress', 'DecompressionParameter', 'finalize_dict',
    'get_frame_info', 'Strategy', 'train_dict',
    # compression.zstd._zstdfile
    'open', 'ZstdFile',
    # _zstd
    'get_frame_size', 'zstd_version', 'zstd_version_info',
    'ZstdCompressor', 'ZstdDecompressor', 'ZstdDict', 'ZstdError',
)

import _zstd
import enum
from _zstd import (ZstdCompressor, ZstdDecompressor, ZstdDict, ZstdError,
                   get_frame_size, zstd_version)
from compression.zstd._zstdfile import ZstdFile, open, _nbytes

#: Version of the underlying codec as a string and as a tuple of ints.
zstd_version_info = tuple(int(part) for part in zstd_version.split('.'))

#: Level used when none is given.
COMPRESSION_LEVEL_DEFAULT = _zstd.ZSTD_CLEVEL_DEFAULT


class FrameInfo:
    """Header information of a Zstandard frame."""

    __slots__ = ('decompressed_size', 'dictionary_id')

    def __init__(self, decompressed_size, dictionary_id):
        super().__setattr__('decompressed_size', decompressed_size)
        super().__setattr__('dictionary_id', dictionary_id)

    def __repr__(self):
        return (f'FrameInfo(decompressed_size={self.decompressed_size}, '
                f'dictionary_id={self.dictionary_id})')

    def __setattr__(self, name, _):
        raise AttributeError(f"can't set attribute {name!r}")

    def __eq__(self, other):
        if not isinstance(other, FrameInfo):
            return NotImplemented
        return (self.decompressed_size, self.dictionary_id) == \
               (other.decompressed_size, other.dictionary_id)

    __hash__ = None


def get_frame_info(frame_buffer):
    """Read the header of the frame starting *frame_buffer*.

    Returns a FrameInfo with ``decompressed_size`` (None when the header does
    not record it) and ``dictionary_id`` (0 when no dictionary is needed).
    """
    return FrameInfo(*_zstd.get_frame_info(frame_buffer))


def _concat_samples(samples):
    chunks = []
    sizes = []
    for sample in samples:
        chunks.append(sample)
        sizes.append(_nbytes(sample))
    if not chunks:
        raise ValueError("samples contained no data; can't train a dictionary")
    return b''.join(chunks), tuple(sizes)


def train_dict(samples, dict_size):
    """Train a dictionary of at most *dict_size* bytes from an iterable of
    bytes-like *samples*; returns a ZstdDict."""
    if not isinstance(dict_size, int):
        raise TypeError(f'dict_size must be an int, not {type(dict_size).__name__}')
    samples = tuple(samples)
    data, sizes = _concat_samples(samples)
    if dict_size <= 0:
        raise ValueError("dict_size must be positive")
    return ZstdDict(_zstd.train_dict(data, sizes, dict_size))


def finalize_dict(zstd_dict, samples, dict_size, level):
    """Turn the content of *zstd_dict* into a complete dictionary of at most
    *dict_size* bytes whose entropy tables are fitted to *samples* compressed
    at *level*; returns a new ZstdDict."""
    if not isinstance(zstd_dict, ZstdDict):
        raise TypeError('zstd_dict argument should be a ZstdDict object')
    if not isinstance(dict_size, int):
        raise TypeError(f'dict_size must be an int, not {type(dict_size).__name__}')
    if not isinstance(level, int):
        raise TypeError(f'level must be an int, not {type(level).__name__}')
    samples = tuple(samples)
    data, sizes = _concat_samples(samples)
    if dict_size <= 0:
        raise ValueError("dict_size must be positive")
    content = _zstd.finalize_dict(zstd_dict.dict_content, data, sizes,
                                  dict_size, level)
    return ZstdDict(content)


def compress(data, level=None, options=None, zstd_dict=None):
    """Compress *data* into a single complete Zstandard frame.

    *level* or the *options* dict (CompressionParameter keys) select the
    settings; *zstd_dict* is an optional dictionary.
    """
    comp = ZstdCompressor(level=level, options=options, zstd_dict=zstd_dict)
    return comp.compress(data, mode=ZstdCompressor.FLUSH_FRAME)


def decompress(data, zstd_dict=None, options=None):
    """Decompress *data*, which may contain several frames (skippable frames
    are ignored), and return the concatenated output."""
    output = []
    while True:
        dec = ZstdDecompressor(options=options, zstd_dict=zstd_dict)
        output.append(dec.decompress(data))
        if not dec.eof:
            raise ZstdError("Compressed data ended before the end-of-stream "
                            "marker was reached")
        data = dec.unused_data
        if not data:
            break
    return b''.join(output)


class CompressionParameter(enum.IntEnum):
    """Compression parameters usable as keys of an options dict."""

    compression_level = _zstd.ZSTD_c_compression_level
    window_log = _zstd.ZSTD_c_window_log
    hash_log = _zstd.ZSTD_c_hash_log
    chain_log = _zstd.ZSTD_c_chain_log
    search_log = _zstd.ZSTD_c_search_log
    min_match = _zstd.ZSTD_c_min_match
    target_length = _zstd.ZSTD_c_target_length
    strategy = _zstd.ZSTD_c_strategy

    enable_long_distance_matching = _zstd.ZSTD_c_enable_long_distance_matching
    ldm_hash_log = _zstd.ZSTD_c_ldm_hash_log
    ldm_min_match = _zstd.ZSTD_c_ldm_min_match
    ldm_bucket_size_log = _zstd.ZSTD_c_ldm_bucket_size_log
    ldm_hash_rate_log = _zstd.ZSTD_c_ldm_hash_rate_log

    content_size_flag = _zstd.ZSTD_c_content_size_flag
    checksum_flag = _zstd.ZSTD_c_checksum_flag
    dict_id_flag = _zstd.ZSTD_c_dict_id_flag

    nb_workers = _zstd.ZSTD_c_nb_workers
    job_size = _zstd.ZSTD_c_job_size
    overlap_log = _zstd.ZSTD_c_overlap_log

    def bounds(self):
        """Return the (lower, upper) bounds of the parameter (inclusive)."""
        return _zstd.get_param_bounds(self.value, is_compress=True)


class DecompressionParameter(enum.IntEnum):
    """Decompression parameters usable as keys of an options dict."""

    window_log_max = _zstd.ZSTD_d_window_log_max

    def bounds(self):
        """Return the (lower, upper) bounds of the parameter (inclusive)."""
        return _zstd.get_param_bounds(self.value, is_compress=False)


class Strategy(enum.IntEnum):
    """Match-finding strategies, from fastest to strongest."""

    fast = _zstd.ZSTD_fast
    dfast = _zstd.ZSTD_dfast
    greedy = _zstd.ZSTD_greedy
    lazy = _zstd.ZSTD_lazy
    lazy2 = _zstd.ZSTD_lazy2
    btlazy2 = _zstd.ZSTD_btlazy2
    btopt = _zstd.ZSTD_btopt
    btultra = _zstd.ZSTD_btultra
    btultra2 = _zstd.ZSTD_btultra2


# Let the accelerator reject options dicts using the wrong kind of key.
_zstd.set_parameter_types(CompressionParameter, DecompressionParameter)
