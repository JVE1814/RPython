"""A framework for line-oriented command interpreters.

Subclass Cmd and define do_<command>(self, arg) methods; help_<topic>()
methods (or the docstrings of do_ methods) provide help.  cmdloop() reads
lines, dispatches them with onecmd() and stops when a command returns a
true value.  '?' is short for 'help' and '!' for 'shell' (if do_shell
exists); an empty line repeats the last command.  complete_<command>()
methods provide tab completion (through readline) for arguments.
"""

import sys

__all__ = ["Cmd"]

PROMPT = '(Cmd) '
IDENTCHARS = ("abcdefghijklmnopqrstuvwxyz"
              "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
              "0123456789_")


class Cmd:
    """Base class for line-oriented command interpreters."""
    prompt = PROMPT
    identchars = IDENTCHARS
    ruler = '='
    lastcmd = ''
    intro = None
    doc_leader = ""
    doc_header = "Documented commands (type help <topic>):"
    misc_header = "Miscellaneous help topics:"
    undoc_header = "Undocumented commands:"
    nohelp = "*** No help on %s"
    use_rawinput = 1

    def __init__(self, completekey='tab', stdin=None, stdout=None):
        """completekey: readline key for completion (None disables it);
        stdin/stdout: streams to use instead of sys.stdin/sys.stdout."""
        self.stdin = stdin if stdin is not None else sys.stdin
        self.stdout = stdout if stdout is not None else sys.stdout
        self.cmdqueue = []
        self.completekey = completekey

    # ---- main loop

    def cmdloop(self, intro=None):
        """Read and execute commands until one of them returns true.
        `intro` (or self.intro) is printed first."""
        self.preloop()
        saved_completer = self._install_completer()
        try:
            if intro is not None:
                self.intro = intro
            if self.intro:
                self.stdout.write(str(self.intro) + "\n")
            stop = None
            while not stop:
                line = self.cmdqueue.pop(0) if self.cmdqueue else self._read_line()
                line = self.precmd(line)
                stop = self.onecmd(line)
                stop = self.postcmd(stop, line)
            self.postloop()
        finally:
            self._restore_completer(saved_completer)

    def _read_line(self):
        if self.use_rawinput:
            try:
                return input(self.prompt)
            except EOFError:
                return 'EOF'
        self.stdout.write(self.prompt)
        self.stdout.flush()
        line = self.stdin.readline()
        if not line:
            return 'EOF'
        return line.rstrip('\r\n')

    def _install_completer(self):
        if not (self.use_rawinput and self.completekey):
            return None
        try:
            import readline
        except ImportError:
            return None
        previous = readline.get_completer()
        readline.set_completer(self.complete)
        if getattr(readline, "backend", "readline") == "editline":
            if self.completekey == 'tab':
                command = "bind ^I rl_complete"
            else:
                command = f"bind {self.completekey} rl_complete"
        else:
            command = f"{self.completekey}: complete"
        readline.parse_and_bind(command)
        return (readline, previous)

    def _restore_completer(self, saved):
        if saved is not None:
            readline, previous = saved
            readline.set_completer(previous)

    # ---- hooks

    def precmd(self, line):
        """Called with each line before it is executed; returns the line
        to execute."""
        return line

    def postcmd(self, stop, line):
        """Called after each command; returns the new stop flag."""
        return stop

    def preloop(self):
        """Called once before the loop starts."""

    def postloop(self):
        """Called once after the loop ends."""

    # ---- dispatching

    def parseline(self, line):
        """Split a line into (command, argument, line); command is None if
        the line does not start with a command name."""
        line = line.strip()
        if not line:
            return None, None, line
        if line[0] == '?':
            line = 'help ' + line[1:]
        elif line[0] == '!':
            if not hasattr(self, 'do_shell'):
                return None, None, line
            line = 'shell ' + line[1:]
        end = 0
        while end < len(line) and line[end] in self.identchars:
            end += 1
        return line[:end], line[end:].strip(), line

    def onecmd(self, line):
        """Execute one command line; returns the value of the do_ method
        (true means: stop the loop)."""
        command, arg, line = self.parseline(line)
        if not line:
            return self.emptyline()
        if command is None:
            return self.default(line)
        self.lastcmd = '' if line == 'EOF' else line
        if command == '':
            return self.default(line)
        handler = getattr(self, 'do_' + command, None)
        if handler is None:
            return self.default(line)
        return handler(arg)

    def emptyline(self):
        """Called for an empty line: repeats the last command."""
        if self.lastcmd:
            return self.onecmd(self.lastcmd)

    def default(self, line):
        """Called for lines that are not a known command."""
        self.stdout.write('*** Unknown syntax: %s\n' % line)

    # ---- completion

    def completedefault(self, *ignored):
        """Completion for commands without a complete_ method: none."""
        return []

    def completenames(self, text, *ignored):
        prefix = 'do_' + text
        return [name[3:] for name in self.get_names() if name.startswith(prefix)]

    def complete(self, text, state):
        """readline completer: returns the state-th completion of text."""
        if state == 0:
            import readline
            buffer = readline.get_line_buffer()
            stripped = len(buffer) - len(buffer.lstrip())
            start = readline.get_begidx() - stripped
            end = readline.get_endidx() - stripped
            if start > 0:
                command, args, _ = self.parseline(buffer.lstrip())
                if command == '':
                    completer = self.completedefault
                else:
                    completer = getattr(self, 'complete_' + command,
                                        self.completedefault)
            else:
                completer = self.completenames
            self.completion_matches = completer(text, buffer.lstrip(), start, end)
        try:
            return self.completion_matches[state]
        except IndexError:
            return None

    def get_names(self):
        """Attribute names used to find commands and help topics."""
        return dir(self.__class__)

    def complete_help(self, *args):
        commands = set(self.completenames(*args))
        topics = {name[5:] for name in self.get_names()
                  if name.startswith('help_' + args[0])}
        return list(commands | topics)

    # ---- help

    def do_help(self, arg):
        'List available commands with "help" or detailed help with "help cmd".'
        if arg:
            helper = getattr(self, 'help_' + arg, None)
            if helper is not None:
                helper()
                return
            command = getattr(self, 'do_' + arg, None)
            doc = getattr(command, '__doc__', None) if command is not None else None
            if doc:
                import inspect
                self.stdout.write("%s\n" % inspect.cleandoc(doc))
            else:
                self.stdout.write("%s\n" % (self.nohelp % (arg,)))
            return

        names = sorted(self.get_names())
        topics = {name[5:] for name in names if name.startswith('help_')}
        documented, undocumented = [], []
        previous = None
        for name in names:
            if not name.startswith('do_') or name == previous:
                continue
            previous = name
            command = name[3:]
            if command in topics:
                documented.append(command)
                topics.discard(command)
            elif getattr(self, name).__doc__:
                documented.append(command)
            else:
                undocumented.append(command)
        self.stdout.write("%s\n" % str(self.doc_leader))
        self.print_topics(self.doc_header, documented, 15, 80)
        self.print_topics(self.misc_header, sorted(topics), 15, 80)
        self.print_topics(self.undoc_header, undocumented, 15, 80)

    def print_topics(self, header, cmds, cmdlen, maxcol):
        if not cmds:
            return
        self.stdout.write("%s\n" % str(header))
        if self.ruler:
            self.stdout.write("%s\n" % str(self.ruler * len(header)))
        self.columnize(cmds, maxcol - 1)
        self.stdout.write("\n")

    def columnize(self, list, displaywidth=80):
        """Write a list of strings in as few rows as possible, columns
        separated by two spaces, filling column by column."""
        if not list:
            self.stdout.write("<empty>\n")
            return
        bad = [i for i, item in enumerate(list) if not isinstance(item, str)]
        if bad:
            raise TypeError("list[i] not a string for i in %s"
                            % ", ".join(map(str, bad)))
        count = len(list)
        if count == 1:
            self.stdout.write('%s\n' % str(list[0]))
            return
        rows, widths = self._layout(list, displaywidth)
        for row in range(rows):
            cells = [list[index] if index < count else ""
                     for index in range(row, rows * len(widths), rows)]
            while cells and not cells[-1]:
                cells.pop()
            padded = [cell.ljust(widths[col]) for col, cell in enumerate(cells)]
            self.stdout.write("%s\n" % str("  ".join(padded)))

    @staticmethod
    def _layout(items, displaywidth):
        """(rows, column widths) of the narrowest layout that fits."""
        count = len(items)
        for rows in range(1, count):
            columns = -(-count // rows)
            widths = [max(len(item) for item in items[col * rows:(col + 1) * rows])
                      for col in range(columns)]
            if sum(widths) + 2 * (len(widths) - 1) <= displaywidth:
                return rows, widths
        return count, [0]
