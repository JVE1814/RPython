"""Reader for .netrc files (machine credentials used by ftp-like clients)."""

import os
import stat

__all__ = ["netrc", "NetrcParseError"]


class NetrcParseError(Exception):
    """A .netrc file could not be parsed (or is insecure)."""

    def __init__(self, msg, filename=None, lineno=None):
        self.filename = filename
        self.lineno = lineno
        self.msg = msg
        super().__init__(msg)

    def __str__(self):
        return "%s (%s, line %s)" % (self.msg, self.filename, self.lineno)


def _can_security_check():
    return os.name == 'posix' and hasattr(os, 'getuid')


def _getpwuid(uid):
    try:
        import pwd
        return pwd.getpwuid(uid)[0]
    except (ImportError, LookupError):
        return 'uid %s' % uid


_ENTRY_KEYWORDS = ('login', 'user', 'account', 'password')
_STARTERS = ('machine', 'default', 'macdef')


class _Scanner:
    """Splits .netrc text into tokens.  A token is a run of non-blank
    characters or a double-quoted string; a backslash escapes the next
    character in both.  Comments are only skipped on request, because a
    value such as a password may itself start with '#'."""

    def __init__(self, text):
        self.text = text
        self.pos = 0
        self.lineno = 1

    def _skip_blanks(self):
        text = self.text
        while self.pos < len(text) and text[self.pos].isspace():
            if text[self.pos] == '\n':
                self.lineno += 1
            self.pos += 1

    def at_comment(self):
        self._skip_blanks()
        return self.text.startswith('#', self.pos)

    def skip_line(self):
        end = self.text.find('\n', self.pos)
        if end < 0:
            self.pos = len(self.text)
        else:
            self.pos = end + 1
            self.lineno += 1

    def next(self):
        """The next token, or None at end of input ('' is a real token)."""
        self._skip_blanks()
        text = self.text
        if self.pos >= len(text):
            return None
        chars = []
        if text[self.pos] == '"':
            self.pos += 1
            while self.pos < len(text):
                c = text[self.pos]
                self.pos += 1
                if c == '\\' and self.pos < len(text):
                    chars.append(text[self.pos])
                    self.pos += 1
                elif c == '"':
                    break
                else:
                    if c == '\n':
                        self.lineno += 1
                    chars.append(c)
            return ''.join(chars)
        while self.pos < len(text) and not text[self.pos].isspace():
            c = text[self.pos]
            self.pos += 1
            if c == '\\' and self.pos < len(text):
                chars.append(text[self.pos])
                self.pos += 1
            else:
                chars.append(c)
        return ''.join(chars)

    def peek(self):
        saved = self.pos, self.lineno
        token = self.next()
        self.pos, self.lineno = saved
        return token

    def macro_lines(self):
        """Lines of a macro body, up to (excluding) the first blank line."""
        self.skip_line()
        lines = []
        while True:
            if self.pos >= len(self.text):
                return None
            end = self.text.find('\n', self.pos)
            if end < 0:
                return None
            line = self.text[self.pos:end + 1]
            self.pos = end + 1
            self.lineno += 1
            if not line.strip():
                return lines
            lines.append(line)


class netrc:
    """netrc(file=None): parse `file` (default ~/.netrc).

    hosts maps machine names (and 'default') to (login, account, password);
    macros maps macro names to lists of lines."""

    def __init__(self, file=None):
        default_netrc = file is None
        if file is None:
            file = os.path.join(os.path.expanduser("~"), ".netrc")
        self.hosts = {}
        self.macros = {}
        try:
            with open(file, encoding="utf-8") as fp:
                self._parse(file, fp, default_netrc)
        except UnicodeDecodeError:
            with open(file, encoding="locale") as fp:
                self._parse(file, fp, default_netrc)

    def _parse(self, file, fp, default_netrc):
        scanner = _Scanner(fp.read())
        needs_check = False
        while True:
            if scanner.at_comment():
                scanner.skip_line()
                continue
            lineno = scanner.lineno
            token = scanner.next()
            if token is None:
                break
            if token == 'macdef':
                name = scanner.next() or ''
                lines = scanner.macro_lines()
                if lines is None:
                    raise NetrcParseError(
                        "Macro definition missing null line terminator.",
                        file, scanner.lineno)
                self.macros[name] = lines
                continue
            if token == 'machine':
                name = scanner.next()
                if name is None:
                    raise NetrcParseError("missing machine name", file, lineno)
            elif token == 'default':
                name = 'default'
            else:
                raise NetrcParseError("bad toplevel token %r" % token,
                                      file, lineno)
            login = account = password = ''
            while True:
                if scanner.at_comment():
                    scanner.skip_line()
                    continue
                keyword = scanner.peek()
                if keyword is None or keyword in _STARTERS:
                    break
                lineno = scanner.lineno
                scanner.next()
                if keyword not in _ENTRY_KEYWORDS:
                    raise NetrcParseError("bad follower token %r" % keyword,
                                          file, lineno)
                value = scanner.next()
                if value is None:
                    value = ''
                if keyword in ('login', 'user'):
                    login = value
                elif keyword == 'account':
                    account = value
                else:
                    password = value
            self.hosts[name] = (login, account, password)
            if login != 'anonymous':
                needs_check = True
        if default_netrc and needs_check and _can_security_check():
            self._security_check(fp, file, scanner.lineno)

    def _security_check(self, fp, file, lineno):
        info = os.fstat(fp.fileno())
        uid = os.getuid()
        if info.st_uid != uid:
            raise NetrcParseError(
                "~/.netrc file owner (%s) does not match current user (%s)"
                % (_getpwuid(info.st_uid), _getpwuid(uid)), file, lineno)
        if info.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
            raise NetrcParseError(
                "~/.netrc access too permissive: access permissions must "
                "restrict access to only the owner", file, lineno)

    def authenticators(self, host):
        """(login, account, password) for `host`, falling back to the
        default entry; None if neither exists."""
        if host in self.hosts:
            return self.hosts[host]
        return self.hosts.get('default')

    def __repr__(self):
        out = []
        for host, (login, account, password) in self.hosts.items():
            if host == 'default':
                out.append("default\n")
            else:
                out.append("machine %s\n" % host)
            out.append("\tlogin %s\n" % login)
            if account:
                out.append("\taccount %s\n" % account)
            out.append("\tpassword %s\n" % password)
        for name, lines in self.macros.items():
            out.append("macdef %s\n" % name)
            out.extend(lines)
            out.append("\n")
        return ''.join(out)


if __name__ == '__main__':
    print(netrc())
