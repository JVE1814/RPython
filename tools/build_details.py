"""Write the PEP 739 build-details.json of an installed layout (tail4).

Run with the installed interpreter itself (ours, never another Python):
    <prefix>/bin/python3 tools/build_details.py
It writes <stdlib>/build-details.json and <prefix>/lib/pkgconfig/python-<VERSION>.pc
(the pkg-config file the c_api section points to).
"""

import json
import os
import sys
import sysconfig


def main():
    prefix = sys.base_prefix
    stdlib = sysconfig.get_path("stdlib")
    version = sysconfig.get_config_var("VERSION") or f"{sys.version_info[0]}.{sys.version_info[1]}"
    abiflags = getattr(sys, "abiflags", "")
    include = sysconfig.get_path("include")
    pkgconfig = os.path.join(prefix, "lib", "pkgconfig")
    os.makedirs(pkgconfig, exist_ok=True)
    pc_text = (
        f"prefix={prefix}\n"
        "exec_prefix=${prefix}\n"
        "libdir=${exec_prefix}/lib\n"
        f"includedir={include}\n\n"
        "Name: Python\n"
        "Description: Build a C extension for Python\n"
        "Requires:\n"
        f"Version: {version}\n"
        "Libs.private:\n"
        "Libs:\n"
        "Cflags: -I${includedir}\n"
    )
    names = {f"python-{version}.pc", f"python-{version}{abiflags}.pc"}
    for name in names:
        with open(os.path.join(pkgconfig, name), "w", encoding="utf-8") as f:
            f.write(pc_text)

    vi = sys.version_info
    impl = sys.implementation
    iv = impl.version
    implementation = {
        "name": impl.name,
        "cache_tag": impl.cache_tag,
        "version": {
            "major": iv.major, "minor": iv.minor, "micro": iv.micro,
            "releaselevel": iv.releaselevel, "serial": iv.serial,
        },
        "hexversion": impl.hexversion,
    }
    if hasattr(impl, "_multiarch"):
        implementation["_multiarch"] = impl._multiarch
    ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".so"
    from importlib.machinery import EXTENSION_SUFFIXES, SOURCE_SUFFIXES, BYTECODE_SUFFIXES
    data = {
        "schema_version": "1.0",
        "base_prefix": prefix,
        "base_interpreter": os.path.realpath(sys.executable),
        "platform": sysconfig.get_platform(),
        "language": {
            "version": version,
            "version_info": {
                "major": vi.major, "minor": vi.minor, "micro": vi.micro,
                "releaselevel": vi.releaselevel, "serial": vi.serial,
            },
        },
        "implementation": implementation,
        "abi": {
            "flags": list(abiflags),
            "extension_suffix": ext_suffix,
        },
        "suffixes": {
            "source": list(SOURCE_SUFFIXES),
            "bytecode": list(BYTECODE_SUFFIXES),
            "extensions": list(EXTENSION_SUFFIXES),
        },
        # one static executable (D13): no libpython to link against
        "libpython": {},
        "c_api": {
            "headers": include,
            "pkgconfig_path": pkgconfig,
        },
    }
    with open(os.path.join(stdlib, "build-details.json"), "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()
