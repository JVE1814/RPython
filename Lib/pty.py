"""Pseudo-terminal utilities: openpty(), fork() and spawn()."""

import os
import sys
import tty
from os import close, waitpid
from select import select
from tty import setraw, tcgetattr, tcsetattr

__all__ = ["openpty", "fork", "spawn"]

STDIN_FILENO = 0
STDOUT_FILENO = 1
STDERR_FILENO = 2

CHILD = 0

# Stop reading a source once this much of its data is waiting to be written.
_HIGH_WATER = 4096


def openpty():
    """Open a pseudo-terminal pair; returns (master_fd, slave_fd)."""
    return os.openpty()


def fork():
    """Fork with the child connected to a new pseudo-terminal as its
    controlling terminal and standard streams.

    Returns (pid, master_fd) in the parent and (CHILD, fd) in the child,
    like os.forkpty()."""
    try:
        pid, fd = os.forkpty()
    except (AttributeError, OSError):
        pass
    else:
        if pid == CHILD:
            try:
                os.setsid()
            except OSError:
                pass   # forkpty() already made us a session leader
        return pid, fd

    master_fd, slave_fd = openpty()
    pid = os.fork()
    if pid == CHILD:
        os.close(master_fd)
        os.login_tty(slave_fd)
    else:
        os.close(slave_fd)
    return pid, master_fd


def _read(fd):
    """Default read function: up to 1024 bytes."""
    return os.read(fd, 1024)


def _copy(master_fd, master_read=_read, stdin_read=_read):
    """Shuttle data between the pseudo-terminal master and our stdin/stdout
    until the master reports end of file."""
    stdin_open = master_fd != STDIN_FILENO
    stdout_open = master_fd != STDOUT_FILENO
    to_child = b""     # read from stdin, waiting for the master
    to_parent = b""    # read from the master, waiting for stdout
    while True:
        readers = []
        writers = []
        if stdin_open and len(to_child) < _HIGH_WATER:
            readers.append(STDIN_FILENO)
        if stdout_open and len(to_parent) < _HIGH_WATER:
            readers.append(master_fd)
        if stdout_open and to_parent:
            writers.append(STDOUT_FILENO)
        if to_child:
            writers.append(master_fd)
        readable, writable, _ = select(readers, writers, [])

        if STDOUT_FILENO in writable:
            try:
                written = os.write(STDOUT_FILENO, to_parent)
            except OSError:
                stdout_open = False
            else:
                to_parent = to_parent[written:]

        if master_fd in readable:
            try:
                data = master_read(master_fd)
            except OSError:
                data = b""
            if not data:
                return   # the child side was closed
            to_parent += data

        if master_fd in writable:
            written = os.write(master_fd, to_child)
            to_child = to_child[written:]

        if stdin_open and STDIN_FILENO in readable:
            data = stdin_read(STDIN_FILENO)
            if not data:
                stdin_open = False
            else:
                to_child += data


def spawn(argv, master_read=_read, stdin_read=_read):
    """Run argv in a child process connected to a pseudo-terminal, relaying
    our terminal to it; returns the child's wait status."""
    if isinstance(argv, str):
        argv = (argv,)
    sys.audit('pty.spawn', argv)

    pid, master_fd = fork()
    if pid == CHILD:
        os.execlp(argv[0], *argv)

    try:
        saved_mode = tcgetattr(STDIN_FILENO)
        setraw(STDIN_FILENO)
        restore = True
    except tty.error:
        restore = False

    try:
        _copy(master_fd, master_read, stdin_read)
    finally:
        if restore:
            tcsetattr(STDIN_FILENO, tty.TCSAFLUSH, saved_mode)

    close(master_fd)
    return waitpid(pid, 0)[1]
