"""Helpers for locating shared libraries (find_library) and listing the
libraries loaded into the process (dllist)."""

import os
import re
import shutil
import subprocess
import sys


# --------------------------------------------------------------- Windows

if os.name == "nt":

    def find_library(name):
        if name in ("c", "m"):
            return None  # the C runtime is not located by name on Windows
        for directory in os.environ["PATH"].split(os.pathsep):
            for candidate in (os.path.join(directory, name),
                              os.path.join(directory, name + ".dll")):
                if candidate.lower().endswith(".dll") and os.path.isfile(candidate):
                    return candidate
        return None

    def dllist():
        """Return a list of loaded shared libraries in the current process."""
        import ctypes
        from ctypes import wintypes
        psapi = ctypes.WinDLL("psapi")
        kernel32 = ctypes.WinDLL("kernel32")
        proc = kernel32.GetCurrentProcess()
        count = wintypes.DWORD()
        mods = (wintypes.HMODULE * 1024)()
        if not psapi.EnumProcessModules(proc, mods, ctypes.sizeof(mods), ctypes.byref(count)):
            return None
        out = []
        buf = ctypes.create_unicode_buffer(32768)
        for h in mods[:count.value // ctypes.sizeof(wintypes.HMODULE)]:
            if kernel32.GetModuleFileNameW(h, buf, len(buf)):
                out.append(buf.value)
        return out

# --------------------------------------------------------------- Apple

elif sys.platform in ("darwin", "ios", "tvos", "watchos"):
    from ctypes.macholib.dyld import dyld_find as _dyld_find

    def find_library(name):
        candidates = ["lib%s.dylib" % name, "%s.dylib" % name,
                      "%s.framework/%s" % (name, name)]
        for c in candidates:
            try:
                return _dyld_find(c)
            except ValueError:
                continue
        return None

    def dllist():
        import ctypes
        libc = ctypes.CDLL(None)
        count = libc._dyld_image_count
        count.restype = ctypes.c_uint32
        get_name = libc._dyld_get_image_name
        get_name.restype = ctypes.c_char_p
        get_name.argtypes = [ctypes.c_uint32]
        return [os.fsdecode(get_name(i)) for i in range(count()) if get_name(i)]

elif sys.platform == "android":

    def find_library(name):
        directory = "/system/lib64" if "64" in os.uname().machine else "/system/lib"
        path = os.path.join(directory, "lib%s.so" % name)
        return path if os.path.isfile(path) else None

elif sys.platform == "emscripten":

    def _is_wasm(filename):
        with open(filename, "br") as f:
            return f.read(4) == b"\0asm"

    def find_library(name):
        paths = os.environ.get("LD_LIBRARY_PATH", "").split(":")
        for libdir in paths:
            for candidate in ("lib%s.so" % name, "lib%s.wasm" % name):
                path = os.path.join(libdir, candidate)
                if os.path.isfile(path) and _is_wasm(path):
                    return path
        return None

# --------------------------------------------------------------- other POSIX

elif os.name == "posix":

    def _run(cmd, env=None):
        """Run a command, returning its stdout as bytes (None on failure)."""
        try:
            proc = subprocess.run(cmd, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, env=env)
        except OSError:
            return None
        return proc.stdout

    def _is_elf(filename):
        try:
            with open(filename, "br") as f:
                return f.read(4) == b"\x7fELF"
        except OSError:
            return False

    def _get_soname(path):
        """The DT_SONAME of an ELF shared object (or None)."""
        if not path or not _is_elf(path):
            return None
        objdump = shutil.which("objdump")
        if not objdump:
            return None
        out = _run([objdump, "-p", "-j", ".dynamic", path])
        if not out:
            return None
        m = re.search(rb"\sSONAME\s+([^\s]+)", out)
        return os.fsdecode(m.group(1)) if m else None

    def _findLib_gcc(name):
        """Ask the C compiler driver which file ``-l<name>`` resolves to."""
        compiler = shutil.which("gcc") or shutil.which("cc")
        if not compiler:
            return None
        import tempfile
        expr = os.fsencode(r"[^\(\)\s]*lib%s\.[^\(\)\s]*" % re.escape(name))
        with tempfile.TemporaryDirectory() as tmp:
            out_file = os.path.join(tmp, "a.out")
            env = dict(os.environ, LC_ALL="C", LANG="C")
            out = _run([compiler, "-Wl,-t", "-o", out_file, "-l" + name], env=env)
        if not out:
            return None
        for match in re.findall(expr, out):
            path = os.fsdecode(match)
            if _is_elf(path):
                return path
        return None

    def _findSoname_ldconfig(name):
        """Look ``lib<name>.so`` up in the ldconfig cache."""
        import struct
        bits = struct.calcsize("l") * 8
        machine = os.uname().machine + "-" + str(bits)
        abi_tags = {
            "x86_64-64": "libc6,x86-64",
            "ppc64-64": "libc6,64bit",
            "sparc64-64": "libc6,64bit",
            "s390x-64": "libc6,64bit",
            "ia64-64": "libc6,IA-64",
            "aarch64-64": "libc6,AArch64",
        }
        abi = abi_tags.get(machine, "libc6")
        ldconfig = "/sbin/ldconfig" if os.path.exists("/sbin/ldconfig") else shutil.which("ldconfig")
        if not ldconfig:
            return None
        out = _run([ldconfig, "-p"], env=dict(os.environ, LC_ALL="C", LANG="C"))
        if not out:
            return None
        pattern = re.compile(
            rb"\s+(lib%s\.[^\s]+)\s+\(%s" % (re.escape(os.fsencode(name)), re.escape(os.fsencode(abi))))
        m = pattern.search(out)
        return os.fsdecode(m.group(1)) if m else None

    def _findLib_ld(name):
        """Search LD_LIBRARY_PATH with the linker (``ld -t``)."""
        ld = shutil.which("ld")
        if not ld:
            return None
        cmd = [ld, "-t"]
        for d in os.environ.get("LD_LIBRARY_PATH", "").split(":"):
            if d:
                cmd.extend(["-L", d])
        cmd.extend(["-o", os.devnull, "-l%s" % name])
        out = _run(cmd)
        if not out:
            return None
        expr = os.fsencode(r"[^\(\)\s]*lib%s\.[^\(\)\s]*" % re.escape(name))
        for match in re.findall(expr, out):
            path = os.fsdecode(match)
            if _is_elf(path):
                return _get_soname(path) or os.path.basename(path)
        return None

    def find_library(name):
        """Return the file name of the shared library ``lib<name>``, or None."""
        return (_findSoname_ldconfig(name)
                or _get_soname(_findLib_gcc(name))
                or _findLib_ld(name))

    if sys.platform.startswith(("linux", "freebsd", "openbsd", "netbsd")):
        import ctypes

        class _dl_phdr_info(ctypes.Structure):
            _fields_ = [
                ("dlpi_addr", ctypes.c_void_p),
                ("dlpi_name", ctypes.c_char_p),
                ("dlpi_phdr", ctypes.c_void_p),
                ("dlpi_phnum", ctypes.c_ushort),
            ]

        _dl_phdr_callback = ctypes.CFUNCTYPE(
            ctypes.c_int, ctypes.POINTER(_dl_phdr_info), ctypes.c_size_t,
            ctypes.POINTER(ctypes.py_object))

        @_dl_phdr_callback
        def _collect_library(info, size, data):
            name = info.contents.dlpi_name
            if name:
                data.contents.value.append(os.fsdecode(name))
            return 0

        def dllist():
            """Return a list of loaded shared libraries in the current process."""
            libraries = []
            libc = ctypes.CDLL(None)
            libc.dl_iterate_phdr(_collect_library,
                                 ctypes.byref(ctypes.py_object(libraries)))
            return libraries

