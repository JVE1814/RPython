"""Terminal mode helpers: raw and cbreak modes (built on termios)."""

from termios import *

__all__ = ["cfmakeraw", "cfmakecbreak", "setraw", "setcbreak"]

# Indices into the list returned by termios.tcgetattr().
IFLAG = 0
OFLAG = 1
CFLAG = 2
LFLAG = 3
ISPEED = 4
OSPEED = 5
CC = 6


def _set_min_time(mode):
    # return after each byte, no timeout
    mode[CC][VMIN] = 1
    mode[CC][VTIME] = 0


def cfmakeraw(mode):
    """Change the termios attribute list `mode` in place to raw mode (as
    the C library's cfmakeraw() does)."""
    mode[IFLAG] &= ~(IGNBRK | BRKINT | IGNPAR | PARMRK | INPCK | ISTRIP |
                     INLCR | IGNCR | ICRNL | IXON | IXANY | IXOFF)
    mode[OFLAG] &= ~OPOST
    mode[CFLAG] &= ~(PARENB | CSIZE)
    mode[CFLAG] |= CS8
    mode[LFLAG] &= ~(ECHO | ECHOE | ECHOK | ECHONL | ICANON | IEXTEN |
                     ISIG | NOFLSH | TOSTOP)
    _set_min_time(mode)


def cfmakecbreak(mode):
    """Change `mode` in place to cbreak mode: no echo, no line buffering,
    signals and output processing unchanged."""
    mode[LFLAG] &= ~(ECHO | ICANON)
    _set_min_time(mode)


def _change_mode(fd, when, transform):
    old = tcgetattr(fd)
    new = list(old)
    new[CC] = list(old[CC])
    transform(new)
    tcsetattr(fd, when, new)
    return old


def setraw(fd, when=TCSAFLUSH):
    """Put terminal fd into raw mode; returns the previous attributes."""
    return _change_mode(fd, when, cfmakeraw)


def setcbreak(fd, when=TCSAFLUSH):
    """Put terminal fd into cbreak mode; returns the previous attributes."""
    return _change_mode(fd, when, cfmakecbreak)
