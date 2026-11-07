"""Priority queues kept in plain lists (binary min-heaps).

A list is a heap when a[k] <= a[2*k+1] and a[k] <= a[2*k+2] for every k
that has children, so a[0] is its smallest item.  heapify() makes a heap of
a list in linear time; heappush()/heappop() add and remove items in
logarithmic time; heappushpop() and heapreplace() combine a push and a pop.
The *_max variants work on max-heaps (a[0] is the largest item).  merge(),
nsmallest() and nlargest() are built on the same machinery.
"""

__all__ = ['heappush', 'heappop', 'heapify', 'heapreplace', 'merge',
           'nlargest', 'nsmallest', 'heappushpop',
           'heappush_max', 'heappop_max', 'heapify_max', 'heapreplace_max',
           'heappushpop_max']


# ---------------------------------------------------------------------------
# Core helpers.  `before(a, b)` decides whether a belongs above b.

def _sift_toward_root(heap, start, pos, before):
    item = heap[pos]
    while pos > start:
        parent = (pos - 1) >> 1
        up = heap[parent]
        if before(item, up):
            heap[pos] = up
            pos = parent
        else:
            break
    heap[pos] = item

def _sift_toward_leaves(heap, pos, before):
    end = len(heap)
    start = pos
    item = heap[pos]
    child = 2 * pos + 1
    # Move the better child up until a leaf is reached ...
    while child < end:
        right = child + 1
        if right < end and not before(heap[child], heap[right]):
            child = right
        heap[pos] = heap[child]
        pos = child
        child = 2 * pos + 1
    # ... then put the item there and let it bubble back up.
    heap[pos] = item
    _sift_toward_root(heap, start, pos, before)

def _lt(a, b):
    return a < b

def _gt(a, b):
    return b < a


def heappush(heap, item, /):
    """Push item onto heap, maintaining the heap invariant."""
    heap.append(item)
    _sift_toward_root(heap, 0, len(heap) - 1, _lt)

def heappop(heap, /):
    """Pop the smallest item off the heap, maintaining the heap invariant."""
    last = heap.pop()
    if heap:
        top = heap[0]
        heap[0] = last
        _sift_toward_leaves(heap, 0, _lt)
        return top
    return last

def heapreplace(heap, item, /):
    """Pop and return the current smallest value, and add the new item.

    This is more efficient than heappop() followed by heappush(), and can be
    more appropriate when using a fixed-size heap.  Note that the value
    returned may be larger than item!  That constrains reasonable uses of
    this routine unless written as part of a conditional replacement:

        if item > heap[0]:
            item = heapreplace(heap, item)
    """
    top = heap[0]
    heap[0] = item
    _sift_toward_leaves(heap, 0, _lt)
    return top

def heappushpop(heap, item, /):
    """Fast version of a heappush followed by a heappop."""
    if heap and heap[0] < item:
        item, heap[0] = heap[0], item
        _sift_toward_leaves(heap, 0, _lt)
    return item

def heapify(x, /):
    """Transform list into a heap, in-place, in O(len(x)) time."""
    for i in reversed(range(len(x) // 2)):
        _sift_toward_leaves(x, i, _lt)


def heappush_max(heap, item, /):
    """Maxheap version of a heappush."""
    heap.append(item)
    _sift_toward_root(heap, 0, len(heap) - 1, _gt)

def heappop_max(heap, /):
    """Maxheap version of a heappop."""
    last = heap.pop()
    if heap:
        top = heap[0]
        heap[0] = last
        _sift_toward_leaves(heap, 0, _gt)
        return top
    return last

def heapreplace_max(heap, item, /):
    """Maxheap version of a heappop followed by a heappush."""
    top = heap[0]
    heap[0] = item
    _sift_toward_leaves(heap, 0, _gt)
    return top

def heappushpop_max(heap, item, /):
    """Maxheap fast version of a heappush followed by a heappop."""
    if heap and item < heap[0]:
        item, heap[0] = heap[0], item
        _sift_toward_leaves(heap, 0, _gt)
    return item

def heapify_max(x, /):
    """Transform list into a maxheap, in-place, in O(len(x)) time."""
    for i in reversed(range(len(x) // 2)):
        _sift_toward_leaves(x, i, _gt)


def merge(*iterables, key=None, reverse=False):
    '''Merge multiple sorted inputs into a single sorted output.

    Similar to sorted(itertools.chain(*iterables)) but returns a generator,
    does not pull the data into memory all at once, and assumes that each of
    the input streams is already sorted (smallest to largest).

    >>> list(merge([2, 4, 8], [1, 5], [3, 6, 7]))
    [1, 2, 3, 4, 5, 6, 7, 8]

    If *key* is not None, applies a key function to each element to determine
    its sort order.

    >>> list(merge(['bb', 'aaa'], ['c', 'dddd'], key=len))
    ['c', 'bb', 'aaa', 'dddd']

    '''
    # Each heap entry is [sort_key, stream_index, value, next_function].  The
    # stream index breaks ties (keeping the merge stable) and guarantees that
    # values themselves are never compared.  With reverse=True the stream
    # index is negated so that earlier streams still win ties.
    sign = -1 if reverse else 1
    push, pop, replace = ((heappush_max, heappop_max, heapreplace_max)
                          if reverse else (heappush, heappop, heapreplace))
    heap = []
    for i, it in enumerate(map(iter, iterables)):
        nxt = it.__next__
        try:
            value = nxt()
        except StopIteration:
            continue
        k = value if key is None else key(value)
        heap.append([k, sign * i, value, nxt])
    (heapify_max if reverse else heapify)(heap)
    while len(heap) > 1:
        entry = heap[0]
        k, order, value, nxt = entry
        yield value
        try:
            value = nxt()
        except StopIteration:
            pop(heap)
            continue
        entry[0] = value if key is None else key(value)
        entry[2] = value
        replace(heap, entry)
    if heap:
        k, order, value, nxt = heap[0]
        yield value
        yield from nxt.__self__


def nsmallest(n, iterable, key=None):
    """Find the n smallest elements in a dataset.

    Equivalent to:  sorted(iterable, key=key)[:n]
    """
    if n == 1:
        sentinel = object()
        it = iter(iterable)
        result = min(it, default=sentinel, key=key)
        return [] if result is sentinel else [result]
    try:
        size = len(iterable)
    except (TypeError, AttributeError):
        pass
    else:
        if n >= size:
            return sorted(iterable, key=key)[:n]
    if n <= 0:
        return []
    # Keep the n best entries in a max-heap of (key, order, value); the
    # order field makes the selection stable and avoids comparing values.
    heap = []
    for order, value in enumerate(iterable):
        k = value if key is None else key(value)
        entry = (k, order, value)
        if len(heap) < n:
            heappush_max(heap, entry)
        elif (k, order) < heap[0][:2]:
            heapreplace_max(heap, entry)
    heap.sort()
    return [value for k, order, value in heap]


def nlargest(n, iterable, key=None):
    """Find the n largest elements in a dataset.

    Equivalent to:  sorted(iterable, key=key, reverse=True)[:n]
    """
    if n == 1:
        sentinel = object()
        it = iter(iterable)
        result = max(it, default=sentinel, key=key)
        return [] if result is sentinel else [result]
    try:
        size = len(iterable)
    except (TypeError, AttributeError):
        pass
    else:
        if n >= size:
            return sorted(iterable, key=key, reverse=True)[:n]
    if n <= 0:
        return []
    # Min-heap of (key, -order, value): among equal keys, earlier items rank
    # higher, matching the stability of sorted(..., reverse=True).
    heap = []
    for order, value in enumerate(iterable):
        k = value if key is None else key(value)
        entry = (k, -order, value)
        if len(heap) < n:
            heappush(heap, entry)
        elif heap[0][:2] < (k, -order):
            heapreplace(heap, entry)
    heap.sort(reverse=True)
    return [value for k, order, value in heap]


try:
    from _heapq import *
except ImportError:
    pass


if __name__ == "__main__":
    import doctest
    print(doctest.testmod())
