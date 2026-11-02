"""A demonstration extension (disabled by default).

Format > Z in prefixes the selected lines with the configured text;
Z out removes it.  It shows how an extension gets options and key bindings
from config-extensions.def.
"""
from idlelib.config import idleConf

ztext = None  # set by reload()


class ZzDummy:
    """Prefix lines with z-text, or remove it."""

    def __init__(self, editwin):
        self.editwin = editwin
        self.text = editwin.text

    @classmethod
    def reload(cls):
        cls.ztext = idleConf.GetOption('extensions', 'ZzDummy', 'z-text')

    def z_in_event(self, event=None):
        """Insert the z-text at the start of each selected line (set_region
        makes it one undo block)."""
        head, tail, chars, lines = self.editwin.fregion.get_region()
        # the last item is what follows the final newline of the region
        lines = [self.ztext + line for line in lines[:-1]] + lines[-1:]
        self.editwin.fregion.set_region(head, tail, chars, lines)
        return "break"

    def z_out_event(self, event=None):
        """Remove the z-text from the start of each selected line."""
        head, tail, chars, lines = self.editwin.fregion.get_region()
        zlen = len(self.ztext)
        lines = [line[zlen:] if line.startswith(self.ztext) else line
                 for line in lines]
        self.editwin.fregion.set_region(head, tail, chars, lines)
        return "break"


ZzDummy.reload()


if __name__ == "__main__":
    import unittest
    unittest.main('idlelib.idle_test.test_zzdummy', verbosity=2, exit=False)
