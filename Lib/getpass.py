"""Prompt for a password without echoing it, and find the login name.

getpass(prompt='Password: ', stream=None, *, echo_char=None)
    read a password from the controlling terminal; with echo_char, each
    typed character is shown as that character
getuser()
    the login name, from the environment or the password database
GetPassWarning
    issued when input may be echoed
"""

import contextlib
import io
import os
import sys

__all__ = ["getpass", "getuser", "GetPassWarning"]


class GetPassWarning(UserWarning):
    pass


# Editing keys understood when echo_char is used (standard terminal defaults).
_CTRL_A = '\x01'   # move to start of line
_CTRL_C = '\x03'   # interrupt
_CTRL_D = '\x04'   # end of input
_CTRL_E = '\x05'   # move to end of line
_CTRL_K = '\x0b'   # erase to end of line
_CTRL_U = '\x15'   # erase line
_CTRL_V = '\x16'   # insert next character literally
_CTRL_W = '\x17'   # erase previous word
_BACKSPACES = ('\x7f', '\b')

_DEFAULT_KEYS = {
    _CTRL_A: 'home', _CTRL_C: 'intr', _CTRL_D: 'eof', _CTRL_E: 'end',
    _CTRL_K: 'kill_to_end', _CTRL_U: 'kill', _CTRL_V: 'lnext',
    _CTRL_W: 'werase', '\x7f': 'erase', '\b': 'erase',
}


def _terminal_ctrl_chars(attrs):
    """The editing characters configured in termios attributes, as
    {action: char}; empty when they cannot be read."""
    roles = {'erase': 'VERASE', 'kill': 'VKILL', 'werase': 'VWERASE',
             'lnext': 'VLNEXT', 'intr': 'VINTR', 'eof': 'VEOF'}
    found = {}
    try:
        cc = attrs[6]
        for action, name in roles.items():
            index = getattr(termios, name, None)
            if index is None:
                continue
            value = cc[index]
            if isinstance(value, int):
                value = bytes([value])
            if isinstance(value, bytes) and len(value) == 1 and value != b'\0':
                found[action] = value.decode('latin-1')
    except Exception:
        return {}
    return found


def _check_echo_char(echo_char):
    if echo_char is None:
        return
    if not isinstance(echo_char, str):
        raise TypeError("'echo_char' must be a str or None, not "
                        f"{type(echo_char).__name__}")
    if len(echo_char) != 1 or not echo_char.isprintable() or not echo_char.isascii():
        raise ValueError("'echo_char' must be a single printable ASCII character")


class _EchoingLineEditor:
    """Reads a line character by character, showing echo_char for every
    character and supporting a few editing keys."""

    def __init__(self, prompt, stream, source, echo_char, term_ctrl_chars=None):
        self.prompt = prompt
        # control character -> editing action; the terminal's own settings
        # (erase, kill, ...) are honoured in addition to the defaults
        self.keys = dict(_DEFAULT_KEYS)
        for action, char in (term_ctrl_chars or {}).items():
            if isinstance(char, str) and len(char) == 1 and char not in '\n\r':
                self.keys[char] = action
        self.stream = stream
        self.source = source
        self.echo = echo_char
        self.text = []
        self.cursor = 0
        self.shown = 0      # number of echo characters currently displayed

    def _redraw(self):
        width = len(self.prompt) + self.shown
        self.stream.write('\r' + ' ' * width + '\r' + self.prompt +
                          self.echo * len(self.text) +
                          '\b' * (len(self.text) - self.cursor))
        self.shown = len(self.text)
        self.stream.flush()

    def _insert(self, char):
        at_end = self.cursor == len(self.text)
        self.text.insert(self.cursor, char)
        self.cursor += 1
        if at_end:
            self.stream.write(self.echo)
            self.shown += 1
            self.stream.flush()
        else:
            self._redraw()

    def read(self):
        literal = False
        while True:
            char = self.source.read(1)
            if not char:
                if self.text:
                    break
                raise EOFError
            if literal:
                literal = False
                self._insert(char)
                continue
            if char in ('\n', '\r'):
                break
            if char == '\x00':
                continue
            action = self.keys.get(char)
            if action == 'lnext':
                literal = True
            elif action == 'intr':
                raise KeyboardInterrupt
            elif action == 'eof':
                if not self.text:
                    raise EOFError
                break
            elif action == 'erase':
                if self.cursor:
                    self.cursor -= 1
                    del self.text[self.cursor]
                    self._redraw()
            elif action == 'kill':
                if self.text:
                    del self.text[:]
                    self.cursor = 0
                    self._redraw()
            elif action == 'werase':
                start = self.cursor
                while start and self.text[start - 1] == ' ':
                    start -= 1
                while start and self.text[start - 1] != ' ':
                    start -= 1
                if start != self.cursor:
                    del self.text[start:self.cursor]
                    self.cursor = start
                    self._redraw()
            elif action == 'home':
                if self.cursor:
                    self.cursor = 0
                    self._redraw()
            elif action == 'end':
                if self.cursor != len(self.text):
                    self.cursor = len(self.text)
                    self._redraw()
            elif action == 'kill_to_end':
                if self.cursor != len(self.text):
                    del self.text[self.cursor:]
                    self._redraw()
            else:
                self._insert(char)
        return ''.join(self.text)


def _raw_input(prompt="", stream=None, input=None, echo_char=None,
               term_ctrl_chars=None):
    """Write the prompt to `stream` (default stderr) and read a line from
    `input` (default stdin) without its newline."""
    if not stream:
        stream = sys.stderr
    if not input:
        input = sys.stdin
    prompt = str(prompt)
    if prompt:
        try:
            stream.write(prompt)
        except UnicodeEncodeError:
            encoding = stream.encoding
            prompt = prompt.encode(encoding, 'replace').decode(encoding)
            stream.write(prompt)
        stream.flush()
    if echo_char:
        return _EchoingLineEditor(prompt, stream, input, echo_char,
                                  term_ctrl_chars).read()
    line = input.readline()
    if not line:
        raise EOFError
    if line.endswith('\n'):
        line = line[:-1]
    return line


def fallback_getpass(prompt='Password: ', stream=None, *, echo_char=None):
    """Read a password when the echo cannot be switched off (warns)."""
    _check_echo_char(echo_char)
    import warnings
    warnings.warn("Can not control echo on the terminal.", GetPassWarning,
                  stacklevel=2)
    if not stream:
        stream = sys.stderr
    print("Warning: Password input may be echoed.", file=stream)
    return _raw_input(prompt, stream, echo_char=echo_char)


def unix_getpass(prompt='Password: ', stream=None, *, echo_char=None):
    """Read a password from /dev/tty (or stdin) with echo switched off."""
    _check_echo_char(echo_char)
    password = None
    with contextlib.ExitStack() as cleanup:
        try:
            fd = os.open('/dev/tty', os.O_RDWR | os.O_NOCTTY)
            tty = io.FileIO(fd, 'w+')
            cleanup.enter_context(tty)
            source = io.TextIOWrapper(tty)
            cleanup.callback(source.close)
            if not stream:
                stream = source
        except OSError:
            # no controlling terminal: use stdin if it is one
            cleanup.close()
            try:
                fd = sys.stdin.fileno()
            except (AttributeError, ValueError):
                fd = None
                password = fallback_getpass(prompt, stream)
            source = sys.stdin
            if not stream:
                stream = sys.stderr

        if fd is not None:
            try:
                saved = termios.tcgetattr(fd)
                quiet = saved[:]
                quiet[3] &= ~termios.ECHO
                if echo_char:
                    quiet[3] &= ~termios.ICANON
                when = termios.TCSAFLUSH | getattr(termios, 'TCSASOFT', 0)
                try:
                    termios.tcsetattr(fd, when, quiet)
                    password = _raw_input(
                        prompt, stream, input=source, echo_char=echo_char,
                        term_ctrl_chars=_terminal_ctrl_chars(saved))
                finally:
                    termios.tcsetattr(fd, when, saved)
                    stream.flush()
            except termios.error:
                if password is not None:
                    # the password was read; restoring the terminal failed
                    raise
                if stream is not source:
                    cleanup.close()
                password = fallback_getpass(prompt, stream)

        stream.write('\n')
        return password


def win_getpass(prompt='Password: ', stream=None, *, echo_char=None):
    """Read a password with msvcrt (Windows only)."""
    if sys.stdin is not sys.__stdin__:
        return fallback_getpass(prompt, stream, echo_char=echo_char)
    import msvcrt
    for char in prompt:
        msvcrt.putwch(char)
    typed = ""
    while True:
        char = msvcrt.getwch()
        if char in ('\r', '\n'):
            break
        if char == '\x03':
            raise KeyboardInterrupt
        if char == '\b':
            if typed:
                typed = typed[:-1]
                if echo_char:
                    msvcrt.putwch('\b')
                    msvcrt.putwch(' ')
                    msvcrt.putwch('\b')
        else:
            typed += char
            if echo_char:
                msvcrt.putwch(echo_char)
    msvcrt.putwch('\r')
    msvcrt.putwch('\n')
    return typed


def getuser():
    """The login name: $LOGNAME, $USER, $LNAME or $USERNAME, else the name
    of the current user id in the password database."""
    for variable in ('LOGNAME', 'USER', 'LNAME', 'USERNAME'):
        name = os.environ.get(variable)
        if name:
            return name
    try:
        import pwd
        return pwd.getpwuid(os.getuid())[0]
    except (ImportError, KeyError) as exc:
        raise OSError('No username set in the environment') from exc


try:
    import termios
    termios.tcgetattr, termios.tcsetattr
except (ImportError, AttributeError):
    try:
        import msvcrt
    except ImportError:
        getpass = fallback_getpass
    else:
        getpass = win_getpass
else:
    getpass = unix_getpass
