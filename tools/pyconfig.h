/* pyconfig.h for rpython (CPython 3.15 free-threaded ABI), x86_64 Linux / glibc.
   Written for this project; values describe the target platform and build. */
#ifndef Py_PYCONFIG_H
#define Py_PYCONFIG_H

#define Py_GIL_DISABLED 1
#define Py_ENABLE_SHARED 0
#define WITH_DOC_STRINGS 1
#define WITH_MIMALLOC 1
#define WITH_THREAD 1
#define PY_SUPPORT_TIER 1

#define SIZEOF_DOUBLE 8
#define SIZEOF_FLOAT 4
#define SIZEOF_FPOS_T 16
#define SIZEOF_INT 4
#define SIZEOF_LONG 8
#define SIZEOF_LONG_DOUBLE 16
#define SIZEOF_LONG_LONG 8
#define SIZEOF_OFF_T 8
#define SIZEOF_PID_T 4
#define SIZEOF_PTHREAD_KEY_T 4
#define SIZEOF_PTHREAD_T 8
#define SIZEOF_SHORT 2
#define SIZEOF_SIZE_T 8
#define SIZEOF_TIME_T 8
#define SIZEOF_UINTPTR_T 8
#define SIZEOF_VOID_P 8
#define SIZEOF_WCHAR_T 4
#define SIZEOF__BOOL 1
#define ALIGNOF_LONG 8
#define ALIGNOF_SIZE_T 8
#define ALIGNOF_MAX_ALIGN_T 16

#define DOUBLE_IS_LITTLE_ENDIAN_IEEE754 1
#define HAVE_GCC_ASM_FOR_X64 1
#define HAVE_GCC_ASM_FOR_X87 1
#define HAVE_BUILTIN_ATOMIC 1
#define HAVE_STD_ATOMIC 1

#define HAVE_ALLOCA_H 1
#define HAVE_GETENTROPY 1
#define HAVE_GETRANDOM 1
#define HAVE_GETRANDOM_SYSCALL 1
#define HAVE_DIRENT_H 1
#define HAVE_DLFCN_H 1
#define HAVE_ERRNO_H 1
#define HAVE_FCNTL_H 1
#define HAVE_FORK 1
#define HAVE_INTTYPES_H 1
#define HAVE_LANGINFO_H 1
#define HAVE_LIMITS_H 1
#define HAVE_LOCALE_H 1
#define HAVE_LONG_DOUBLE 1
#define HAVE_PTHREAD_H 1
#define HAVE_SIGNAL_H 1
#define HAVE_STDDEF_H 1
#define HAVE_STDINT_H 1
#define HAVE_STDIO_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRINGS_H 1
#define HAVE_STRING_H 1
#define HAVE_SYS_SELECT_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_SYS_WAIT_H 1
#define HAVE_UNISTD_H 1
#define HAVE_WCHAR_H 1
#define HAVE_USABLE_WCHAR_T 0
#define HAVE_CLOCK_GETTIME 1
#define HAVE_STRFTIME 1
#define HAVE_THREAD_LOCAL 1
#define STDC_HEADERS 1

#define _GNU_SOURCE 1
#define _POSIX_C_SOURCE 200809L
#define _XOPEN_SOURCE 700
#define _XOPEN_SOURCE_EXTENDED 1
#define _DEFAULT_SOURCE 1
#define _FILE_OFFSET_BITS 64
#define _LARGEFILE_SOURCE 1
#define _NETBSD_SOURCE 1
#define _DARWIN_C_SOURCE 1
#define __BSD_VISIBLE 1

#define PY_SSIZE_T_CLEAN 1
#define SOABI "cpython-315t-x86_64-linux-gnu"
#define PLATLIBDIR "lib"

#endif /* Py_PYCONFIG_H */
