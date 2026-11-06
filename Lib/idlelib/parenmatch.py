"""Highlight the matching opening bracket.

When a closing bracket is typed (or Edit > Show Surrounding Parens is
used) the opening bracket -- or, depending on the style, the brackets or
the whole bracketed expression -- is highlighted until the cursor moves
(or for a short time).

Styles: 'opener' (the opening bracket), 'parens' (both brackets),
'expression' (everything between, the default).
"""
from idlelib.config import idleConf
from idlelib.hyperparser import HyperParser

_openers = {')': '(', ']': '[', '}': '{'}
CHECK_DELAY = 100  # milliseconds


class ParenMatch:

    RESTORE_VIRTUAL_EVENT_NAME = "<<parenmatch-check-restore>>"
    # Key events after which the highlighting is removed.
    RESTORE_SEQUENCES = ("<KeyPress>", "<ButtonPress>",
                         "<Key-Return>", "<Key-BackSpace>")

    def __init__(self, editwin):
        self.editwin = editwin
        self.text = editwin.text
        editwin.text.bind(self.RESTORE_VIRTUAL_EVENT_NAME, self.restore_event)
        # Each highlighting gets a number so that the timer of an older
        # one does not remove a newer one.
        self.counter = 0
        self.is_restore_active = False

    @classmethod
    def reload(cls):
        cls.STYLE = idleConf.GetOption(
            'extensions', 'ParenMatch', 'style', default='expression')
        cls.FLASH_DELAY = idleConf.GetOption(
            'extensions', 'ParenMatch', 'flash-delay', type='int', default=500)
        cls.BELL = idleConf.GetOption(
            'extensions', 'ParenMatch', 'bell', type='bool', default=1)
        cls.HILITE_CONFIG = idleConf.GetHighlight(idleConf.CurrentTheme(),
                                                  'hilite')

    def activate_restore(self):
        """Remove the highlighting at the next key or button press."""
        if not self.is_restore_active:
            for seq in self.RESTORE_SEQUENCES:
                self.text.event_add(self.RESTORE_VIRTUAL_EVENT_NAME, seq)
            self.is_restore_active = True

    def deactivate_restore(self):
        if self.is_restore_active:
            for seq in self.RESTORE_SEQUENCES:
                self.text.event_delete(self.RESTORE_VIRTUAL_EVENT_NAME, seq)
            self.is_restore_active = False

    def flash_paren_event(self, event):
        """Highlight the brackets surrounding the cursor."""
        hp = HyperParser(self.editwin, "insert")
        indices = hp.get_surrounding_brackets(_openers.values())
        if indices is None:
            self.bell()
            return "break"
        self.activate_restore()
        self.create_tag(indices)
        self.set_timeout()
        return "break"

    def paren_closed_event(self, event):
        """Highlight the bracket matching the one just typed."""
        closer = self.text.get("insert-1c")
        if closer not in _openers:
            return
        hp = HyperParser(self.editwin, "insert-1c")
        if not hp.is_in_code():
            return
        indices = hp.get_surrounding_brackets(_openers[closer], True)
        if indices is None:
            self.bell()
            return
        self.activate_restore()
        self.create_tag(indices)
        self.set_timeout()

    def restore_event(self, event=None):
        """Remove the highlighting."""
        self.text.tag_delete("paren")
        self.deactivate_restore()
        self.counter += 1  # disable the last timer, if there is one

    def handle_restore_timer(self, timer_count):
        if timer_count == self.counter:
            self.restore_event()

    def bell(self):
        if self.BELL:
            self.text.bell()

    # -- highlighting styles

    def create_tag(self, indices):
        """Highlight *indices* (open, close) in the configured style."""
        styles = {'opener': self.create_tag_opener,
                  'parens': self.create_tag_parens,
                  'expression': self.create_tag_expression}
        styles.get(self.STYLE, self.create_tag_opener)(indices)

    def create_tag_opener(self, indices):
        self.text.tag_add("paren", indices[0])
        self.text.tag_config("paren", self.HILITE_CONFIG)

    def create_tag_parens(self, indices):
        if self.text.get(indices[1]) in (')', ']', '}'):
            rightindex = indices[1] + "+1c"
        else:
            rightindex = indices[1]
        self.text.tag_add("paren", indices[0], indices[0] + "+1c",
                          rightindex + "-1c", rightindex)
        self.text.tag_config("paren", self.HILITE_CONFIG)

    def create_tag_expression(self, indices):
        if self.text.get(indices[1]) in (')', ']', '}'):
            rightindex = indices[1] + "+1c"
        else:
            rightindex = indices[1]
        self.text.tag_add("paren", indices[0], rightindex)
        self.text.tag_config("paren", self.HILITE_CONFIG)

    # -- removal of the highlighting

    def set_timeout(self):
        """Remove the highlighting after FLASH_DELAY ms ('expression'
        style: when the cursor leaves the expression)."""
        self.counter += 1
        self.editwin.text_frame.after(
            self.FLASH_DELAY,
            lambda count=self.counter: self.handle_restore_timer(count))


ParenMatch.reload()


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_parenmatch', verbosity=2)
