"""dyld-style search for dynamic libraries and frameworks."""

import os
from ctypes.macholib.framework import framework_info
from ctypes.macholib.dylib import dylib_info
from itertools import chain

__all__ = ["dyld_find", "framework_find", "framework_info", "dylib_info"]

DEFAULT_FRAMEWORK_FALLBACK = [
    os.path.expanduser("~/Library/Frameworks"),
    "/Library/Frameworks",
    "/Network/Library/Frameworks",
    "/System/Library/Frameworks",
]

DEFAULT_LIBRARY_FALLBACK = [
    os.path.expanduser("~/lib"),
    "/usr/local/lib",
    "/lib",
    "/usr/lib",
]


def _env_list(env, name):
    if env is None:
        env = os.environ
    value = env.get(name)
    return value.split(":") if value else []


def dyld_image_suffix(env=None):
    if env is None:
        env = os.environ
    return env.get("DYLD_IMAGE_SUFFIX")


def dyld_framework_path(env=None):
    return _env_list(env, "DYLD_FRAMEWORK_PATH")


def dyld_library_path(env=None):
    return _env_list(env, "DYLD_LIBRARY_PATH")


def dyld_fallback_framework_path(env=None):
    return _env_list(env, "DYLD_FALLBACK_FRAMEWORK_PATH")


def dyld_fallback_library_path(env=None):
    return _env_list(env, "DYLD_FALLBACK_LIBRARY_PATH")


def dyld_image_suffix_search(iterator, env=None):
    suffix = dyld_image_suffix(env)
    if suffix is None:
        return iterator

    def _with_suffix():
        for path in iterator:
            if path.endswith(".dylib"):
                yield path[:-len(".dylib")] + suffix + ".dylib"
            else:
                yield path + suffix
            yield path
    return _with_suffix()


def dyld_override_search(name, env=None):
    info = framework_info(name)
    if info is not None:
        for d in dyld_framework_path(env):
            yield os.path.join(d, info["name"])
    for d in dyld_library_path(env):
        yield os.path.join(d, os.path.basename(name))


def dyld_executable_path_search(name, executable_path=None):
    if name.startswith("@executable_path/") and executable_path is not None:
        yield os.path.join(executable_path, name[len("@executable_path/"):])


def dyld_default_search(name, env=None):
    yield name
    info = framework_info(name)
    if info is not None:
        for d in dyld_fallback_framework_path(env):
            yield os.path.join(d, info["name"])
    fallback = dyld_fallback_library_path(env)
    for d in fallback:
        yield os.path.join(d, os.path.basename(name))
    if info is not None and not dyld_fallback_framework_path(env):
        for d in DEFAULT_FRAMEWORK_FALLBACK:
            yield os.path.join(d, info["name"])
    if not fallback:
        for d in DEFAULT_LIBRARY_FALLBACK:
            yield os.path.join(d, os.path.basename(name))


def dyld_find(name, executable_path=None, env=None):
    """Find a library or framework using dyld semantics."""
    for path in dyld_image_suffix_search(chain(
            dyld_override_search(name, env),
            dyld_executable_path_search(name, executable_path),
            dyld_default_search(name, env)), env):
        if os.path.isfile(path):
            return path
    raise ValueError("dylib %s could not be found" % (name,))


def framework_find(fn, executable_path=None, env=None):
    """Find a framework by name (e.g. 'Python' or 'Python.framework')."""
    try:
        return dyld_find(fn, executable_path=executable_path, env=env)
    except ValueError as e:
        error = e
    idx = fn.rfind(".framework")
    if idx == -1:
        idx = len(fn)
        fn += ".framework"
    fn = os.path.join(fn, os.path.basename(fn[:idx]))
    try:
        return dyld_find(fn, executable_path=executable_path, env=env)
    except ValueError:
        raise error
