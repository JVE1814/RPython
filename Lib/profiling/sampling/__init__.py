"""Statistical (sampling) profiler.

The profiler looks at the stacks of a running Python process at regular
intervals instead of recording every call; collectors turn the samples
into reports (pstats-like tables, collapsed stacks, flame graphs, line
heat maps...).

Reading the stacks of another process needs the _remote_debugging
extension module; the collectors themselves are plain Python and work on
any sequence of samples.
"""

__all__ = ()
