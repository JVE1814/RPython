"""Exceptions raised by the sampling profiler's command-line front end."""

__all__ = (
    'SamplingError',
    'SamplingScriptNotFoundError',
    'SamplingModuleNotFoundError',
    'SamplingUnknownProcessError',
)


class SamplingError(Exception):
    """Base class for sampling-profiler front-end errors."""


class SamplingScriptNotFoundError(SamplingError):
    """A script passed to the profiler does not exist."""

    def __init__(self, path):
        self.path = path
        super().__init__(f"Script '{path}' not found.")


class SamplingModuleNotFoundError(SamplingError):
    """A module passed with -m could not be found."""

    def __init__(self, name):
        self.name = name
        super().__init__(f"Module '{name}' not found.")


class SamplingUnknownProcessError(SamplingError):
    """The target process id does not exist."""

    def __init__(self, pid):
        self.pid = pid
        super().__init__(f"Process {pid} not found.")
