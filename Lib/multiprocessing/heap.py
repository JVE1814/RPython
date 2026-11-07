#
# multiprocessing.heap -- carving small blocks out of shared memory.
#
# Shared memory comes in "arenas": mmaps of unlinked temporary files, which
# children inherit or receive as descriptors.  A Heap hands out blocks
# (arena, start, stop) from them with a best-fit search over the free
# blocks.  Freed blocks merge with free neighbours; an arena that becomes
# entirely free is given back when it is big enough to matter.
#

import bisect
import mmap
import os
import sys
import tempfile
import threading

from .context import reduction
from . import util

__all__ = ['BufferWrapper']


class Arena(object):
    """One mmap'ed region of shared memory backed by an unlinked file."""

    # Tried first for the backing file: RAM-backed and normally roomy.
    _dir_candidates = ['/dev/shm']

    def __init__(self, size, fd=-1):
        self.size = size
        self.fd = fd
        if fd == -1:
            # A new arena: make an anonymous file of the right size.
            self.fd, path = tempfile.mkstemp(prefix='pym-%d-' % os.getpid(),
                                             dir=self._choose_dir(size))
            os.unlink(path)
            util.Finalize(self, os.close, (self.fd,))
            os.ftruncate(self.fd, size)
        self.buffer = mmap.mmap(self.fd, self.size)

    def _choose_dir(self, size):
        for directory in self._dir_candidates:
            try:
                info = os.statvfs(directory)
            except (OSError, AttributeError):
                continue
            if info.f_bavail * info.f_frsize >= size:
                return directory
        return util.get_temp_dir()


def reduce_arena(a):
    if a.fd == -1:
        raise ValueError('Arena is unpicklable because '
                         'forking was enabled when it was created')
    return rebuild_arena, (a.size, reduction.DupFd(a.fd))


def rebuild_arena(size, dupfd):
    return Arena(size, dupfd.detach())


reduction.register(Arena, reduce_arena)


def _round_up(n, multiple):
    return -(-n // multiple) * multiple


class Heap(object):

    _alignment = 8
    # A completely free arena at least this large is unmapped.
    _DISCARD_FREE_SPACE_LARGER_THAN = 4 * 1024 ** 2
    # Each new arena is twice the previous one, up to this size.
    _DOUBLE_ARENA_SIZE_UNTIL = 4 * 1024 ** 2

    def __init__(self, size=mmap.PAGESIZE):
        self._lastpid = os.getpid()
        self._lock = threading.Lock()
        self._size = size           # minimum size of the next arena
        self._arenas = []
        # Free space.  _len_to_seq maps a length to the free blocks of that
        # length; _sizes lists those lengths in order (for the best-fit
        # search); the two edge maps find a free neighbour of a block.
        self._len_to_seq = {}
        self._sizes = []
        self._free_at_start = {}    # (arena, start) -> free block
        self._free_at_end = {}      # (arena, stop) -> free block
        # Used space: arena -> {(start, stop), ...}
        self._allocated_blocks = {}
        # Blocks whose free() found the lock taken -- possibly by the very
        # thread the garbage collector interrupted -- handled later.
        self._pending_free_blocks = []
        self._n_mallocs = 0
        self._n_frees = 0

    # -- free-space index ----------------------------------------------------

    def _index_free(self, block):
        arena, start, stop = block
        length = stop - start
        same_length = self._len_to_seq.get(length)
        if same_length is None:
            self._len_to_seq[length] = [block]
            bisect.insort(self._sizes, length)
        else:
            same_length.append(block)
        self._free_at_start[(arena, start)] = block
        self._free_at_end[(arena, stop)] = block

    def _unindex_free(self, block):
        arena, start, stop = block
        length = stop - start
        same_length = self._len_to_seq[length]
        same_length.remove(block)
        if not same_length:
            del self._len_to_seq[length]
            del self._sizes[bisect.bisect_left(self._sizes, length)]
        del self._free_at_start[(arena, start)]
        del self._free_at_end[(arena, stop)]

    def _take_best_fit(self, size):
        # The smallest free block of at least *size* bytes, or None.
        where = bisect.bisect_left(self._sizes, size)
        if where == len(self._sizes):
            return None
        block = self._len_to_seq[self._sizes[where]][-1]
        self._unindex_free(block)
        return block

    def _release_space(self, block):
        # Return a block to the free space, merged with free neighbours.
        arena, start, stop = block
        before = self._free_at_end.get((arena, start))
        if before is not None:
            self._unindex_free(before)
            start = before[1]
        after = self._free_at_start.get((arena, stop))
        if after is not None:
            self._unindex_free(after)
            stop = after[2]
        self._index_free((arena, start, stop))

    # -- arenas --------------------------------------------------------------

    def _new_arena(self, size):
        length = _round_up(max(self._size, size), mmap.PAGESIZE)
        if self._size < self._DOUBLE_ARENA_SIZE_UNTIL:
            self._size *= 2
        util.info('allocating a new mmap of length %d', length)
        arena = Arena(length)
        self._arenas.append(arena)
        return (arena, 0, length)

    def _arena_emptied(self, arena):
        # *arena* has no used block left: drop it if it is large enough.
        if arena.size < self._DISCARD_FREE_SPACE_LARGER_THAN:
            return
        self._unindex_free((arena, 0, arena.size))
        self._arenas.remove(arena)

    # -- used blocks ---------------------------------------------------------

    def _mark_used(self, arena, start, stop):
        self._allocated_blocks.setdefault(arena, set()).add((start, stop))

    def _mark_unused(self, block):
        arena, start, stop = block
        used = self._allocated_blocks[arena]
        used.remove((start, stop))
        if not used:
            del self._allocated_blocks[arena]
            self._arena_emptied(arena)

    def _really_free(self, block):
        self._n_frees += 1
        self._release_space(block)
        self._mark_unused(block)

    def _free_pending_blocks(self):
        pending = self._pending_free_blocks
        while pending:
            self._really_free(pending.pop())

    # -- public operations ---------------------------------------------------

    def free(self, block):
        """Give *block* back; called by BufferWrapper's finalizer."""
        if os.getpid() != self._lastpid:
            raise ValueError(
                "My pid ({0:n}) is not last pid {1:n}".format(
                    os.getpid(), self._lastpid))
        if not self._lock.acquire(False):
            self._pending_free_blocks.append(block)
            return
        try:
            self._free_pending_blocks()
            self._really_free(block)
        finally:
            self._lock.release()

    def malloc(self, size):
        """Return a block (arena, start, stop) of at least *size* bytes."""
        if size < 0:
            raise ValueError("Size {0:n} out of range".format(size))
        if sys.maxsize <= size:
            raise OverflowError("Size {0:n} too large".format(size))
        if os.getpid() != self._lastpid:
            # A forked child must not touch the parent's bookkeeping.
            self.__init__()
        with self._lock:
            self._n_mallocs += 1
            self._free_pending_blocks()
            size = _round_up(max(size, 1), self._alignment)
            block = self._take_best_fit(size)
            if block is None:
                block = self._new_arena(size)
            arena, start, stop = block
            end = start + size
            if end < stop:
                self._release_space((arena, end, stop))
            self._mark_used(arena, start, end)
            return (arena, start, end)


class BufferWrapper(object):
    """A block of shared memory that is freed when the wrapper is."""

    _heap = Heap()

    def __init__(self, size):
        if size < 0:
            raise ValueError("Size {0:n} out of range".format(size))
        if sys.maxsize <= size:
            raise OverflowError("Size {0:n} too large".format(size))
        heap = BufferWrapper._heap
        block = heap.malloc(size)
        self._state = (block, size)
        util.Finalize(self, heap.free, args=(block,))

    def create_memoryview(self):
        (arena, start, _stop), size = self._state
        return memoryview(arena.buffer)[start:start + size]
