"""Undo and redo for a Text widget (a Percolator filter).

UndoDelegator records every insert and delete as a command in a list.
Typing a word character by character is merged into one command.
Commands between undo_block_start() and undo_block_stop() are undone as
one.  The position of the last save is tracked so that the window can
show whether the text is modified.
"""
import string

from idlelib.delegator import Delegator


class UndoDelegator(Delegator):

    max_undo = 1000

    def __init__(self):
        Delegator.__init__(self)
        self.reset_undo()

    def setdelegate(self, delegate):
        if self.delegate is not None:
            self.unbind("<<undo>>")
            self.unbind("<<redo>>")
            self.unbind("<<dump-undo-state>>")
        Delegator.setdelegate(self, delegate)
        if delegate is not None:
            self.bind("<<undo>>", self.undo_event)
            self.bind("<<redo>>", self.redo_event)
            self.bind("<<dump-undo-state>>", self.dump_event)

    def dump_event(self, event):
        """Print the undo state (for debugging)."""
        from pprint import pprint
        pprint(self.undolist[:self.pointer])
        print("pointer:", self.pointer, end=' ')
        print("saved:", self.saved, end=' ')
        print("can_merge:", self.can_merge, end=' ')
        print("get_saved():", self.get_saved())
        pprint(self.undolist[self.pointer:])
        return "break"

    def reset_undo(self):
        """Forget all commands; the current text counts as saved."""
        self.was_saved = -1
        self.pointer = 0     # number of commands done (not undone)
        self.undolist = []
        self.undoblock = 0   # the CommandSequence being collected, or 0
        self.set_saved(1)

    def set_saved(self, flag):
        """Mark the current state as saved (or as not corresponding to
        any saved state)."""
        if flag:
            self.saved = self.pointer
        else:
            self.saved = -1
        self.can_merge = False
        self.check_saved()

    def get_saved(self):
        """Is the text as it was when last saved?"""
        return self.saved == self.pointer

    saved_change_hook = None

    def set_saved_change_hook(self, hook):
        self.saved_change_hook = hook

    was_saved = -1

    def check_saved(self):
        """Call the hook when the saved state changes."""
        is_saved = self.get_saved()
        if is_saved != self.was_saved:
            self.was_saved = is_saved
            if self.saved_change_hook:
                self.saved_change_hook()

    def insert(self, index, chars, tags=None):
        self.addcmd(InsertCommand(index, chars, tags))

    def delete(self, index1, index2=None):
        self.addcmd(DeleteCommand(index1, index2))

    def undo_block_start(self):
        """Group the following commands until undo_block_stop() (blocks
        may be nested)."""
        if self.undoblock == 0:
            self.undoblock = CommandSequence()
        self.undoblock.bump_depth()

    def undo_block_stop(self):
        if self.undoblock.bump_depth(-1) == 0:
            block = self.undoblock
            self.undoblock = 0
            if len(block) > 0:
                if len(block) == 1:
                    block = block.getcmd(0)
                # already executed: just record it
                self.addcmd(block, False)

    def addcmd(self, cmd, execute=True):
        """Execute (if *execute*) and record *cmd*."""
        if execute:
            cmd.do(self.delegate)
        if self.undoblock != 0:
            self.undoblock.append(cmd)
            return
        if self.can_merge and self.pointer > 0:
            if self.undolist[self.pointer - 1].merge(cmd):
                return
        # A new command drops the undone ones.
        self.undolist[self.pointer:] = [cmd]
        if self.saved > self.pointer:
            self.saved = -1
        self.pointer += 1
        if len(self.undolist) > self.max_undo:
            del self.undolist[0]
            self.pointer -= 1
            if self.saved >= 0:
                self.saved -= 1
        self.can_merge = True
        self.check_saved()

    def undo_event(self, event):
        """Undo the last command."""
        if self.pointer == 0:
            self.bell()
            return "break"
        self.undolist[self.pointer - 1].undo(self.delegate)
        self.pointer -= 1
        self.can_merge = False
        self.check_saved()
        return "break"

    def redo_event(self, event):
        """Redo the last undone command."""
        if self.pointer >= len(self.undolist):
            self.bell()
            return "break"
        self.undolist[self.pointer].redo(self.delegate)
        self.pointer += 1
        self.can_merge = False
        self.check_saved()
        return "break"


class Command:
    """An edit between index1 and index2, with the marks before and after
    it (to restore the cursor)."""

    tags = None

    def __init__(self, index1, index2, chars, tags=None):
        self.marks_before = {}
        self.marks_after = {}
        self.index1 = index1
        self.index2 = index2
        self.chars = chars
        if tags:
            self.tags = tags

    def __repr__(self):
        s = self.__class__.__name__
        t = (self.index1, self.index2, self.chars, self.tags)
        if self.tags is None:
            t = t[:-1]
        return s + repr(t)

    def do(self, text):
        pass

    def redo(self, text):
        pass

    def undo(self, text):
        pass

    def merge(self, cmd):
        return 0

    def save_marks(self, text):
        marks = {}
        for name in text.mark_names():
            if name != "insert" and name != "current":
                marks[name] = text.index(name)
        return marks

    def set_marks(self, text, marks):
        for name, index in marks.items():
            text.mark_set(name, index)


class InsertCommand(Command):

    def __init__(self, index1, chars, tags=None):
        Command.__init__(self, index1, None, chars, tags)

    def do(self, text):
        self.marks_before = self.save_marks(text)
        self.index1 = text.index(self.index1)
        if text.compare(self.index1, ">", "end-1c"):
            # Insertion at or after the final newline goes before it.
            self.index1 = text.index("end-1c")
        text.insert(self.index1, self.chars, self.tags)
        self.index2 = text.index("%s+%dc" % (self.index1, len(self.chars)))
        self.marks_after = self.save_marks(text)

    def redo(self, text):
        text.mark_set('insert', self.index1)
        text.insert(self.index1, self.chars, self.tags)
        self.set_marks(text, self.marks_after)
        text.see('insert')

    def undo(self, text):
        text.mark_set('insert', self.index1)
        text.delete(self.index1, self.index2)
        self.set_marks(text, self.marks_before)
        text.see('insert')

    def merge(self, cmd):
        """Absorb a following one-character insert of the same kind."""
        if self.__class__ is not cmd.__class__:
            return False
        if self.index2 != cmd.index1 or self.tags != cmd.tags:
            return False
        if len(cmd.chars) != 1:
            return False
        if self.chars and self.classify(self.chars[-1]) != self.classify(cmd.chars):
            return False
        self.index2 = cmd.index2
        self.chars = self.chars + cmd.chars
        return True

    alphanumeric = string.ascii_letters + string.digits + "_"

    def classify(self, c):
        if c in self.alphanumeric:
            return "alphanumeric"
        if c == "\n":
            return "newline"
        return "punctuation"


class DeleteCommand(Command):

    def __init__(self, index1, index2=None):
        Command.__init__(self, index1, index2, None, None)

    def do(self, text):
        self.marks_before = self.save_marks(text)
        self.index1 = text.index(self.index1)
        if self.index2:
            self.index2 = text.index(self.index2)
        else:
            self.index2 = text.index(self.index1 + " +1c")
        if text.compare(self.index2, ">", "end-1c"):
            # The final newline cannot be deleted.
            self.index2 = text.index("end-1c")
        self.chars = text.get(self.index1, self.index2)
        text.delete(self.index1, self.index2)
        self.marks_after = self.save_marks(text)

    def redo(self, text):
        text.mark_set('insert', self.index1)
        text.delete(self.index1, self.index2)
        self.set_marks(text, self.marks_after)
        text.see('insert')

    def undo(self, text):
        text.mark_set('insert', self.index1)
        text.insert(self.index1, self.chars)
        self.set_marks(text, self.marks_before)
        text.see('insert')


class CommandSequence(Command):
    """Commands undone and redone together."""

    def __init__(self):
        self.cmds = []
        self.depth = 0

    def __repr__(self):
        s = self.__class__.__name__
        strs = ["    %r" % (cmd,) for cmd in self.cmds]
        return s + "(\n" + ",\n".join(strs) + "\n)"

    def __len__(self):
        return len(self.cmds)

    def append(self, cmd):
        self.cmds.append(cmd)

    def getcmd(self, i):
        return self.cmds[i]

    def redo(self, text):
        for cmd in self.cmds:
            cmd.redo(text)

    def undo(self, text):
        for cmd in reversed(self.cmds):
            cmd.undo(text)

    def bump_depth(self, incr=1):
        self.depth = self.depth + incr
        return self.depth


def _undo_delegator(parent):  # htest #
    from tkinter import Toplevel, Text, Button
    from idlelib.percolator import Percolator
    top = Toplevel(parent)
    top.title("Test UndoDelegator")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("+%d+%d" % (x, y + 175))

    text = Text(top, height=10)
    text.pack()
    text.focus_set()
    p = Percolator(text)
    d = UndoDelegator()
    p.insertfilter(d)

    undo = Button(top, text="Undo", command=lambda: d.undo_event(None))
    undo.pack(side='left')
    redo = Button(top, text="Redo", command=lambda: d.redo_event(None))
    redo.pack(side='left')
    dump = Button(top, text="Dump", command=lambda: d.dump_event(None))
    dump.pack(side='left')


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_undo', verbosity=2, exit=False)
