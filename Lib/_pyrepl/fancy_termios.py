"""termios attributes as an object with named fields."""

from __future__ import annotations

import termios


class TermState:
    """The seven termios attributes (iflag, oflag, cflag, lflag, ispeed,
    ospeed, cc) of a terminal."""

    def __init__(self, attrs) -> None:
        (self.iflag, self.oflag, self.cflag, self.lflag,
         self.ispeed, self.ospeed, cc) = attrs
        self.cc = list(cc)

    def as_list(self) -> list:
        return [self.iflag, self.oflag, self.cflag, self.lflag,
                self.ispeed, self.ospeed, list(self.cc)]

    def copy(self) -> "TermState":
        return self.__class__(self.as_list())


def tcgetattr(fd: int) -> TermState:
    return TermState(termios.tcgetattr(fd))


def tcsetattr(fd: int, when: int, attrs: TermState) -> None:
    termios.tcsetattr(fd, when, attrs.as_list())


class Term(TermState):
    """A terminal whose attributes can be changed and put back."""

    TS__init__ = TermState.__init__

    def __init__(self, fd: int = 0) -> None:
        self.TS__init__(termios.tcgetattr(fd))
        self.fd = fd
        self.stack: list[list] = []

    def save(self) -> None:
        self.stack.append(self.as_list())

    def set(self, when: int = termios.TCSANOW) -> None:
        termios.tcsetattr(self.fd, when, self.as_list())

    def restore(self) -> None:
        self.TS__init__(self.stack.pop())
        self.set()
