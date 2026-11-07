#!/bin/sh
# python3.15t-config: build flags for C code that embeds or extends this
# interpreter.  Installed next to the interpreter (<prefix>/bin); every path is
# derived from the directory the script itself lives in, so a relocated
# installation reports its real location.

usage() {
    printf '%s\n' "Usage: $0 --prefix|--exec-prefix|--includes|--libs|--cflags|--ldflags|--extension-suffix|--help|--abiflags|--configdir|--embed" >&2
    exit "$1"
}

[ $# -eq 0 ] && usage 1

# values configured at build time (matched literally, never interpreted)
configured_prefix='/usr/local'
configured_includedir='/usr/local/include'
version='3.15'
abiflags='t'
ext_suffix='.cpython-315t-x86_64-linux-gnu.so'
multiarch='x86_64-linux-gnu'
libs='-lm'
cflags_extra='-O2 -Wall'

script_dir=$(cd -P -- "$(dirname -- "$0")" && pwd -P) || exit 1
prefix=$(cd -P -- "$script_dir/.." && pwd -P) || exit 1
exec_prefix=$prefix
case $configured_includedir in
    "$configured_prefix"*) includedir=$prefix${configured_includedir#"$configured_prefix"} ;;
    *) includedir=$configured_includedir ;;
esac
ldversion=$version$abiflags
incdir=$includedir/python$ldversion
libdir=$exec_prefix/lib
configdir=$libdir/python$ldversion/config-$ldversion-$multiarch
embed=0

for arg in "$@"; do
    case $arg in
        --embed) embed=1 ;;
        --prefix|--exec-prefix|--includes|--libs|--cflags|--ldflags|--extension-suffix|--abiflags|--configdir) ;;
        --help) usage 0 ;;
        *) usage 1 ;;
    esac
done

for arg in "$@"; do
    case $arg in
        --prefix) printf '%s\n' "$prefix" ;;
        --exec-prefix) printf '%s\n' "$exec_prefix" ;;
        --includes) printf '%s\n' "-I$incdir -I$incdir" ;;
        --cflags) printf '%s\n' "-I$incdir -I$incdir $cflags_extra" ;;
        --libs)
            if [ $embed = 1 ]; then printf '%s\n' "-lpython$ldversion $libs"; else printf '%s\n' "$libs"; fi ;;
        --ldflags)
            if [ $embed = 1 ]; then printf '%s\n' "-L$libdir -lpython$ldversion $libs"; else printf '%s\n' "-L$libdir $libs"; fi ;;
        --extension-suffix) printf '%s\n' "$ext_suffix" ;;
        --abiflags) printf '%s\n' "$abiflags" ;;
        --configdir) printf '%s\n' "$configdir" ;;
    esac
done
