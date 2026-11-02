"""Edit > Expand Word: complete the word before the cursor with the other
words of the text that start with it.

Repeating the command cycles through the candidates, closest before the
cursor first, then those after it, then the original word (with a bell).
"""
import re
import string


class AutoExpand:

    wordchars = string.ascii_letters + string.digits + "_"

    def __init__(self, editwin):
        self.text = editwin.text
        self.bell = self.text.bell
        # (candidates, next index, cursor index, line) of the last
        # expansion, to continue cycling.
        self.state = None

    def expand_word_event(self, event):
        """Replace the word before the cursor by the next candidate."""
        text = self.text
        insert = text.index("insert")
        line = text.get("insert linestart", "insert lineend")
        if self.state and self.state[2:] == (insert, line):
            words, index = self.state[:2]
        else:
            words = self.getwords()
            index = 0
        if not words:
            self.bell()
            return "break"
        word = self.getprevword()
        text.delete("insert - %d chars" % len(word), "insert")
        newword = words[index]
        index = (index + 1) % len(words)
        if index == 0:
            self.bell()  # all candidates were shown
        text.insert("insert", newword)
        self.state = (words, index, text.index("insert"),
                      text.get("insert linestart", "insert lineend"))
        return "break"

    def getwords(self):
        """Return the words of the text starting with the word before the
        cursor: nearest first, then the word itself."""
        word = self.getprevword()
        if not word:
            return []
        pattern = re.compile(r"\b" + re.escape(word) + r"\w+\b")
        before = pattern.findall(self.text.get("1.0", "insert wordstart"))
        after = pattern.findall(self.text.get("insert wordend", "end"))
        if not before and not after:
            return []
        words = []
        for w in list(reversed(before)) + after:
            if w not in words:
                words.append(w)
        words.append(word)
        return words

    def getprevword(self):
        """Return the identifier characters just before the cursor."""
        line = self.text.get("insert linestart", "insert")
        i = len(line)
        while i > 0 and line[i - 1] in self.wordchars:
            i -= 1
        return line[i:]


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_autoexpand', verbosity=2)
