"""An OptionMenu whose items can be replaced after creation."""
from tkinter import OptionMenu, _setit, StringVar, Button


class DynOptionMenu(OptionMenu):
    """OptionMenu with SetMenu(values, value) to change the items."""

    def __init__(self, master, variable, value, *values, **kwargs):
        # OptionMenu accepts only 'command' among the keyword options;
        # the others configure the menubutton.
        highlightthickness = kwargs.pop('highlightthickness', None)
        OptionMenu.__init__(self, master, variable, value, *values, **kwargs)
        self['highlightthickness'] = highlightthickness
        self.variable = variable
        self.command = kwargs.get('command')

    def SetMenu(self, valueList, value=None):
        """Replace the items by *valueList*; select *value* if given."""
        self['menu'].delete(0, 'end')
        for item in valueList:
            self['menu'].add_command(
                label=item, command=_setit(self.variable, item, self.command))
        if value:
            self.variable.set(value)


def _dyn_option_menu(parent):  # htest #
    from tkinter import Toplevel

    top = Toplevel(parent)
    top.title("Test dynamic option menu")
    x, y = map(int, parent.geometry().split('+')[1:])
    top.geometry("200x100+%d+%d" % (x + 250, y + 175))
    top.focus_set()

    var = StringVar(top)
    var.set("Old option set")
    dyn = DynOptionMenu(top, var, "old1", "old2", "old3", "old4",
                        highlightthickness=5)
    dyn.pack()

    def update():
        dyn.SetMenu(["new1", "new2", "new3", "new4"], value="new option set")
    button = Button(top, text="Change option set", command=update)
    button.pack()


if __name__ == '__main__':
    from idlelib.idle_test.htest import run
    run(_dyn_option_menu)
