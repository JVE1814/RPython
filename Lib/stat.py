"""Constants and helpers for decoding os.stat() / os.lstat() results."""

# Tuple positions in a stat result

ST_MODE  = 0
ST_INO   = 1
ST_DEV   = 2
ST_NLINK = 3
ST_UID   = 4
ST_GID   = 5
ST_SIZE  = 6
ST_ATIME = 7
ST_MTIME = 8
ST_CTIME = 9

# Mode bit helpers

def S_IMODE(mode):
    """Return the permission bits of mode (what os.chmod() sets)."""
    return mode & 0o7777

def S_IFMT(mode):
    """Return the file type bits of mode."""
    return mode & 0o170000

# File type values of S_IFMT()

S_IFDIR  = 0o040000  # directory
S_IFCHR  = 0o020000  # character device
S_IFBLK  = 0o060000  # block device
S_IFREG  = 0o100000  # regular file
S_IFIFO  = 0o010000  # fifo (named pipe)
S_IFLNK  = 0o120000  # symbolic link
S_IFSOCK = 0o140000
# Types that do not exist on this platform
S_IFDOOR = 0
S_IFPORT = 0
S_IFWHT = 0

# File type predicates

def S_ISDIR(mode):
    """True for a directory mode."""
    return S_IFMT(mode) == S_IFDIR

def S_ISCHR(mode):
    """True for a character device mode."""
    return S_IFMT(mode) == S_IFCHR

def S_ISBLK(mode):
    """True for a block device mode."""
    return S_IFMT(mode) == S_IFBLK

def S_ISREG(mode):
    """True for a regular file mode."""
    return S_IFMT(mode) == S_IFREG

def S_ISFIFO(mode):
    """True for a FIFO mode."""
    return S_IFMT(mode) == S_IFIFO

def S_ISLNK(mode):
    """True for a symbolic link mode."""
    return S_IFMT(mode) == S_IFLNK

def S_ISSOCK(mode):
    """True for a socket mode."""
    return S_IFMT(mode) == S_IFSOCK

def S_ISDOOR(mode):
    """Always False (no doors here)."""
    return False

def S_ISPORT(mode):
    """Always False (no event ports here)."""
    return False

def S_ISWHT(mode):
    """Always False (no whiteouts here)."""
    return False

# Permission bits

S_ISUID = 0o4000
S_ISGID = 0o2000
S_ENFMT = S_ISGID
S_ISVTX = 0o1000
S_IREAD = 0o0400
S_IWRITE = 0o0200
S_IEXEC = 0o0100
S_IRWXU = 0o0700
S_IRUSR = 0o0400
S_IWUSR = 0o0200
S_IXUSR = 0o0100
S_IRWXG = 0o0070
S_IRGRP = 0o0040
S_IWGRP = 0o0020
S_IXGRP = 0o0010
S_IRWXO = 0o0007
S_IROTH = 0o0004
S_IWOTH = 0o0002
S_IXOTH = 0o0001

# st_flags bits (BSD / macOS)

UF_SETTABLE  = 0x0000ffff
UF_NODUMP    = 0x00000001
UF_IMMUTABLE = 0x00000002
UF_APPEND    = 0x00000004
UF_OPAQUE    = 0x00000008
UF_NOUNLINK  = 0x00000010
UF_COMPRESSED = 0x00000020
UF_TRACKED   = 0x00000040
UF_DATAVAULT = 0x00000080
UF_HIDDEN    = 0x00008000
SF_SETTABLE  = 0xffff0000
SF_ARCHIVED  = 0x00010000
SF_IMMUTABLE = 0x00020000
SF_APPEND    = 0x00040000
SF_RESTRICTED = 0x00080000
SF_NOUNLINK  = 0x00100000
SF_SNAPSHOT  = 0x00200000
SF_FIRMLINK  = 0x00800000
SF_DATALESS  = 0x40000000


_TYPE_CHARS = {
    S_IFLNK: "l",
    S_IFSOCK: "s",
    S_IFREG: "-",
    S_IFBLK: "b",
    S_IFDIR: "d",
    S_IFCHR: "c",
    S_IFIFO: "p",
}


def _triad(mode, r, w, x, special, on, off):
    out = ("r" if mode & r else "-") + ("w" if mode & w else "-")
    if mode & special:
        return out + (on if mode & x else off)
    return out + ("x" if mode & x else "-")


def filemode(mode):
    """Return an ls-style string such as '-rwxr-xr-x' for mode."""
    return (_TYPE_CHARS.get(S_IFMT(mode), "?")
            + _triad(mode, S_IRUSR, S_IWUSR, S_IXUSR, S_ISUID, "s", "S")
            + _triad(mode, S_IRGRP, S_IWGRP, S_IXGRP, S_ISGID, "s", "S")
            + _triad(mode, S_IROTH, S_IWOTH, S_IXOTH, S_ISVTX, "t", "T"))


# Windows file attribute bits (st_file_attributes)

FILE_ATTRIBUTE_ARCHIVE = 32
FILE_ATTRIBUTE_COMPRESSED = 2048
FILE_ATTRIBUTE_DEVICE = 64
FILE_ATTRIBUTE_DIRECTORY = 16
FILE_ATTRIBUTE_ENCRYPTED = 16384
FILE_ATTRIBUTE_HIDDEN = 2
FILE_ATTRIBUTE_INTEGRITY_STREAM = 32768
FILE_ATTRIBUTE_NORMAL = 128
FILE_ATTRIBUTE_NOT_CONTENT_INDEXED = 8192
FILE_ATTRIBUTE_NO_SCRUB_DATA = 131072
FILE_ATTRIBUTE_OFFLINE = 4096
FILE_ATTRIBUTE_READONLY = 1
FILE_ATTRIBUTE_REPARSE_POINT = 1024
FILE_ATTRIBUTE_SPARSE_FILE = 512
FILE_ATTRIBUTE_SYSTEM = 4
FILE_ATTRIBUTE_TEMPORARY = 256
FILE_ATTRIBUTE_VIRTUAL = 65536


# Prefer the accelerator module when present
try:
    from _stat import *
except ImportError:
    pass
