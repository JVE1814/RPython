"""Tokenizer for shell-like command lines.

``shlex.shlex`` is a small lexer for simple shell-style languages
(configuration files, command lines); ``split`` turns a command line into an
argument list, ``quote``/``join`` do the reverse.
"""

import sys
from io import StringIO

__all__ = ["shlex", "split", "quote", "join"]

_ASCII_WORD = ("abcdefghijklmnopqrstuvwxyz"
               "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
               "0123456789_")
# Latin-1 letters (without the multiplication and division signs) count as
# word characters in POSIX mode.
_LATIN1_WORD = "".join(chr(c) for c in range(0xC0, 0x100)
                       if c not in (0xD7, 0xF7))
_DEFAULT_PUNCTUATION = "();<>|&"
_PUNCTUATION_WORD_EXTRA = "~-./*?="

# States of the lexer, besides "inside a quote" (the state is the quote
# character) and "after an escape" (the state is the escape character).
_IDLE = " "
_WORD = "a"
_PUNCT = "c"


class shlex:
    """A lexical analyzer for simple shell-like syntaxes."""

    def __init__(self, instream=None, infile=None, posix=False,
                 punctuation_chars=False):
        if isinstance(instream, str):
            instream = StringIO(instream)
        if instream is None:
            self.instream = sys.stdin
            self.infile = None
        else:
            self.instream = instream
            self.infile = infile
        self.posix = posix
        self.eof = None if posix else ''
        self.commenters = '#'
        self.wordchars = _ASCII_WORD + (_LATIN1_WORD if posix else '')
        self.whitespace = ' \t\r\n'
        self.whitespace_split = False
        self.quotes = '\'"'
        self.escape = '\\'
        self.escapedquotes = '"'
        self.state = _IDLE
        self.pushback = []          # pushed tokens, last one is read first
        self.lineno = 1
        self.debug = 0
        self.token = ''
        self.filestack = []         # saved (infile, instream, lineno)
        self.source = None
        if punctuation_chars is True:
            punctuation_chars = _DEFAULT_PUNCTUATION
        elif not punctuation_chars:
            punctuation_chars = ''
        self._punctuation_chars = punctuation_chars
        self._pending_chars = []    # characters to re-read, last one first
        if punctuation_chars:
            self.wordchars = ''.join(
                ch for ch in self.wordchars + _PUNCTUATION_WORD_EXTRA
                if ch not in punctuation_chars)

    @property
    def punctuation_chars(self):
        return self._punctuation_chars

    # -- token and input stacks ---------------------------------------------

    def push_token(self, tok):
        """Push a token back; it is returned by the next get_token()."""
        if self.debug >= 1:
            print(f"shlex: pushing token {tok!r}")
        self.pushback.append(tok)

    def push_source(self, newstream, newfile=None):
        """Read from newstream (a stream or a string) until it is exhausted."""
        if isinstance(newstream, str):
            newstream = StringIO(newstream)
        self.filestack.append((self.infile, self.instream, self.lineno))
        self.infile = newfile
        self.instream = newstream
        self.lineno = 1
        if self.debug:
            if newfile is not None:
                print(f"shlex: pushing to file {self.infile}")
            else:
                print(f"shlex: pushing to stream {self.instream}")

    def pop_source(self):
        """Close the current input and go back to the previous one."""
        self.instream.close()
        self.infile, self.instream, self.lineno = self.filestack.pop()
        if self.debug:
            print(f"shlex: popping to {self.instream}, line {self.lineno}")
        self.state = _IDLE

    def get_token(self):
        """Return the next token, handling pushback and source inclusion."""
        if self.pushback:
            tok = self.pushback.pop()
            if self.debug >= 1:
                print(f"shlex: popping token {tok!r}")
            return tok
        tok = self.read_token()
        if self.source is not None:
            while tok == self.source:
                included = self.sourcehook(self.read_token())
                if included:
                    name, stream = included
                    self.push_source(stream, name)
                tok = self.get_token()
        while tok == self.eof:
            if not self.filestack:
                return self.eof
            self.pop_source()
            tok = self.get_token()
        if self.debug >= 1:
            if tok != self.eof:
                print(f"shlex: token={tok!r}")
            else:
                print("shlex: token=EOF")
        return tok

    # -- the lexer proper -----------------------------------------------------

    def _fail(self, message, what):
        if self.debug >= 2:
            print(f"shlex: I see EOF in {what} state")
        raise ValueError(message)

    def read_token(self):
        """Read one raw token from the input, ignoring pushback and sources."""
        posix = self.posix
        pending = self._pending_chars
        stream = self.instream
        tok = ''
        quoted = False      # posix: a quote was seen, so '' is a real token
        resume = _IDLE      # the state to return to after an escape
        while True:
            if pending:
                ch = pending.pop()
            else:
                ch = stream.read(1)
                if ch == '\n':
                    self.lineno += 1
            state = self.state
            if self.debug >= 3:
                print(f"shlex: in state {state!r} I see character: {ch!r}")

            if state is None:
                tok = ''
                break

            if state == _IDLE:
                if not ch:
                    self.state = None
                    break
                if ch in self.whitespace:
                    if self.debug >= 2:
                        print("shlex: I see whitespace in whitespace state")
                    if tok or (posix and quoted):
                        break
                elif ch in self.commenters:
                    stream.readline()
                    self.lineno += 1
                elif posix and ch in self.escape:
                    resume = _WORD
                    self.state = ch
                elif ch in self.wordchars:
                    tok = ch
                    self.state = _WORD
                elif ch in self._punctuation_chars:
                    tok = ch
                    self.state = _PUNCT
                elif ch in self.quotes:
                    if not posix:
                        tok = ch
                    self.state = ch
                elif self.whitespace_split:
                    tok = ch
                    self.state = _WORD
                else:
                    # any other character is a token of its own
                    tok = ch
                    break

            elif state == _WORD:
                if not ch:
                    self.state = None
                    break
                if ch in self.whitespace:
                    if self.debug >= 2:
                        print("shlex: I see whitespace in word state")
                    self.state = _IDLE
                    if tok or (posix and quoted):
                        break
                elif ch in self.commenters:
                    stream.readline()
                    self.lineno += 1
                    if posix:
                        self.state = _IDLE
                        if tok or quoted:
                            break
                    # outside POSIX mode the word goes on after the comment
                elif posix and ch in self.quotes:
                    self.state = ch
                elif posix and ch in self.escape:
                    resume = _WORD
                    self.state = ch
                elif (ch in self.wordchars or ch in self.quotes
                      or (self.whitespace_split
                          and ch not in self._punctuation_chars)):
                    tok += ch
                else:
                    # the character ends the word and is read again
                    if self.debug >= 2:
                        print("shlex: I see punctuation in word state")
                    pending.append(ch)
                    self.state = _IDLE
                    if tok or (posix and quoted):
                        break

            elif state == _PUNCT:
                if ch and ch in self._punctuation_chars:
                    tok += ch
                else:
                    if ch and ch not in self.whitespace:
                        pending.append(ch)
                    self.state = _IDLE
                    break

            elif state in self.quotes:
                quoted = True
                if not ch:
                    self.token = tok
                    self._fail("No closing quotation", "quotes")
                if ch == state:
                    if not posix:
                        tok += ch
                        self.state = _IDLE
                        break
                    self.state = _WORD
                elif posix and ch in self.escape and state in self.escapedquotes:
                    resume = state
                    self.state = ch
                else:
                    tok += ch

            elif state in self.escape:
                if not ch:
                    self.token = tok
                    self._fail("No escaped character", "escape")
                # inside quotes the escape only applies to the quote and to
                # itself; otherwise it stays in the token
                if resume in self.quotes and ch != state and ch != resume:
                    tok += state
                tok += ch
                self.state = resume

        self.token = ''
        if posix and not quoted and tok == '':
            tok = None
        if self.debug >= 2:
            if tok:
                print(f"shlex: raw token={tok!r}")
            else:
                print("shlex: raw token=EOF")
        return tok

    # -- helpers --------------------------------------------------------------

    def sourcehook(self, newfile):
        """Open a file named by a source request; return (name, stream)."""
        import os.path
        if newfile[:1] == '"':
            newfile = newfile[1:-1]
        # relative names are taken relative to the including file
        if isinstance(self.infile, str) and not os.path.isabs(newfile):
            newfile = os.path.join(os.path.dirname(self.infile), newfile)
        return newfile, open(newfile, "r")

    def error_leader(self, infile=None, lineno=None):
        """Return a '"file", line N: ' prefix for error messages."""
        if infile is None:
            infile = self.infile
        if lineno is None:
            lineno = self.lineno
        return '"%s", line %d: ' % (infile, lineno)

    def __iter__(self):
        return self

    def __next__(self):
        tok = self.get_token()
        if tok == self.eof:
            raise StopIteration
        return tok


def split(s, comments=False, posix=True):
    """Split the string s into a list of words, like a POSIX shell would."""
    if s is None:
        raise ValueError("s argument must not be None")
    lexer = shlex(s, posix=posix)
    lexer.whitespace_split = True
    if not comments:
        lexer.commenters = ''
    return list(lexer)


def join(split_command):
    """Join a list of words into a shell-escaped command line."""
    return ' '.join(map(quote, split_command))


_SHELL_SAFE = frozenset("abcdefghijklmnopqrstuvwxyz"
                        "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                        "0123456789_@%+=:,./-")


def quote(s):
    """Return a shell-escaped version of the string s."""
    if not s:
        return "''"
    if not isinstance(s, str):
        raise TypeError(f"expected string or bytes-like object, "
                        f"got {type(s).__name__!r}")
    if _SHELL_SAFE.issuperset(s):
        return s
    # single quotes protect everything; an embedded ' becomes '"'"'
    return "'" + s.replace("'", "'\"'\"'") + "'"
