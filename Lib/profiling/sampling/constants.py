"""Constants shared by the sampling profiler and its collectors."""

# what is sampled
PROFILING_MODE_WALL = 0     # every thread, whatever it is doing
PROFILING_MODE_CPU = 1      # only threads running on a CPU
PROFILING_MODE_GIL = 2      # only threads holding the GIL
PROFILING_MODE_ALL = 3      # wall clock, with thread states recorded
PROFILING_MODE_EXCEPTION = 4

# sort orders of the pstats-style report
SORT_MODE_NSAMPLES = 0
SORT_MODE_TOTTIME = 1
SORT_MODE_CUMTIME = 2
SORT_MODE_SAMPLE_PCT = 3
SORT_MODE_CUMUL_PCT = 4
SORT_MODE_NSAMPLES_CUMUL = 5

# flags of a sampled thread's status
THREAD_STATUS_HAS_GIL = 1 << 0
THREAD_STATUS_ON_CPU = 1 << 1
THREAD_STATUS_UNKNOWN = 1 << 2
THREAD_STATUS_GIL_REQUESTED = 1 << 3
THREAD_STATUS_HAS_EXCEPTION = 1 << 4
THREAD_STATUS_MAIN_THREAD = 1 << 5

# where a synthetic frame (no Python source) is reported
DEFAULT_LOCATION = (0, 0, -1, -1)
