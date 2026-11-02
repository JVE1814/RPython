"""The shell's command history (Alt-p / Alt-n)."""
from idlelib.config import idleConf


class History:
    """Previously entered statements, recalled into the input area.

    The input typed so far acts as a prefix: only entries starting with it
    are recalled.  With the 'cyclic' option the history wraps around.
    """

    def __init__(self, text):
        """*text* is the shell's Text widget; the input starts at the
        'iomark' mark."""
        self.text = text
        self.history = []
        self.prefix = None   # the input when recall started
        self.pointer = None  # index of the entry shown, while recalling
        self.cyclic = idleConf.GetOption("main", "History", "cyclic", 1, "bool")
        text.bind("<<history-previous>>", self.history_prev)
        text.bind("<<history-next>>", self.history_next)

    def history_next(self, event):
        """Show the next matching entry."""
        self.fetch(reverse=False)
        return "break"

    def history_prev(self, event):
        """Show the previous matching entry."""
        self.fetch(reverse=True)
        return "break"

    def _set_input(self, source):
        text = self.text
        if text.get("iomark", "end-1c") != source:
            text.delete("iomark", "end-1c")
            text.insert("iomark", source, "stdin")

    def fetch(self, reverse):
        """Replace the input by the next (or previous) entry starting with
        the prefix; at the end of the history restore the prefix."""
        text = self.text
        count = len(self.history)
        pointer, prefix = self.pointer, self.prefix
        if pointer is not None and prefix is not None:
            # Recall continues only if the input was not edited.
            if (text.compare("insert", "!=", "end-1c") or
                    text.get("iomark", "end-1c") != self.history[pointer]):
                pointer = prefix = None
                text.mark_set("insert", "end-1c")
        if pointer is None or prefix is None:
            prefix = text.get("iomark", "end-1c")
            if reverse:
                pointer = count
            elif self.cyclic:
                pointer = -1
            else:
                text.bell()
                return
        step = -1 if reverse else 1
        while True:
            pointer += step
            if not 0 <= pointer < count:
                text.bell()
                if not self.cyclic and pointer < 0:
                    return
                self._set_input(prefix)
                pointer = prefix = None
                break
            entry = self.history[pointer]
            if entry.startswith(prefix) and len(entry) > len(prefix):
                text.delete("iomark", "end-1c")
                text.insert("iomark", entry, "stdin")
                break
        text.see("insert")
        text.tag_remove("sel", "1.0", "end")
        self.pointer = pointer
        self.prefix = prefix

    def store(self, source):
        """Add *source* (a statement entered) to the end of the history."""
        source = source.strip()
        if len(source) > 2:
            # an entry appears only once, at its latest position
            try:
                self.history.remove(source)
            except ValueError:
                pass
            self.history.append(source)
        self.pointer = None
        self.prefix = None


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_history', verbosity=2, exit=False)
