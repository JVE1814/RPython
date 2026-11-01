"""Compatibility location of the simple resource reader helpers; they live
in importlib.resources.simple."""

from .resources.simple import (
    ResourceContainer,
    ResourceHandle,
    SimpleReader,
    TraversableReader,
)

__all__ = [
    'SimpleReader',
    'ResourceHandle',
    'ResourceContainer',
    'TraversableReader',
]
