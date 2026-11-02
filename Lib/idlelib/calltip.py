"""Calltips: show the signature and the start of the docstring of a
callable while its arguments are typed.

In the shell, the expression before '(' is evaluated in the user's process
(via RPC); in an editor only names already defined in the shell's
namespace (or sys.modules) are found.
"""
import __main__
import inspect
import re
import sys
import textwrap
import types

from idlelib import calltip_w
from idlelib.hyperparser import HyperParser


class Calltip:

    def __init__(self, editwin=None):
        if editwin is None:  # in the subprocess and in tests
            self.editwin = None
        else:
            self.editwin = editwin
            self.text = editwin.text
            self.active_calltip = None
            self._calltip_window = self._make_tk_calltip_window

    def close(self):
        self._calltip_window = None

    def _make_tk_calltip_window(self):
        # See __init__ for usage
        return calltip_w.CalltipWindow(self.text)

    def remove_calltip_window(self, event=None):
        if self.active_calltip:
            self.active_calltip.hidetip()
            self.active_calltip = None

    def force_open_calltip_event(self, event):
        """Show the calltip for the enclosing call (Edit > Show Call Tip),
        evaluating calls in the expression if needed."""
        self.open_calltip(True)
        return "break"

    def try_open_calltip_event(self, event):
        """Show the calltip after '(' is typed (no function calls are made
        to find the callable)."""
        self.open_calltip(False)

    def refresh_calltip_event(self, event):
        """Update the calltip after ')' is typed."""
        if self.active_calltip and self.active_calltip.tipwindow:
            self.open_calltip(False)

    def open_calltip(self, evalfuncs):
        """Show (or keep, or remove) the calltip for the innermost open
        call around the cursor."""
        hp = HyperParser(self.editwin, "insert")
        sur_paren = hp.get_surrounding_brackets('(')
        if not sur_paren:
            self.remove_calltip_window()
            return
        if self.active_calltip:
            # The tip of this very call is already shown.
            paren = tuple(map(int, sur_paren[0].split('.')))
            if paren == (self.active_calltip.parenline,
                         self.active_calltip.parencol):
                return
        hp.set_index(sur_paren[0])
        try:
            expression = hp.get_expression()
        except ValueError:
            expression = None
        if not expression:
            # Not after an expression (e.g. after an operator).
            return
        self.remove_calltip_window()
        # Only evaluate calls inside the expression on explicit request.
        if not evalfuncs and expression.find('(') != -1:
            return
        argspec = self.fetch_tip(expression)
        if not argspec:
            return
        self.active_calltip = self._calltip_window()
        self.active_calltip.showtip(argspec, sur_paren[0], sur_paren[1])

    def fetch_tip(self, expression):
        """Return the calltip text for *expression*.

        In the shell the expression is evaluated by the user's process, so
        that its objects are found; otherwise it is evaluated here.
        """
        try:
            rpcclt = self.editwin.flist.pyshell.interp.rpcclt
        except AttributeError:
            rpcclt = None
        if rpcclt:
            return rpcclt.remotecall("exec", "get_the_calltip",
                                     (expression,), {})
        return get_argspec(get_entity(expression))


def get_entity(expression):
    """Return the object *expression* refers to (None if it cannot be
    evaluated).  Names are looked up in __main__ and among the modules."""
    if expression:
        namespace = {**sys.modules, **__main__.__dict__}
        try:
            return eval(expression, namespace)  # Only names found by the user.
        except BaseException:
            # An uncaught exception closes idle, and eval can raise any
            # exception, especially if user classes are involved.
            return None


# The tip shows at most _MAX_LINES lines of the docstring, each at most
# _MAX_COLS characters; a long signature wraps with _INDENT.
_MAX_COLS = 85
_MAX_LINES = 5
_INDENT = ' ' * 4
# The first parameter of a signature (removed for bound methods).
_first_param = re.compile(r'(?<=\()\w*\,?\s*')
_default_callable_argspec = "See source or doc"
_invalid_method = "invalid method signature"


def _signature_text(callable_):
    """Return the signature of *callable_* as text, '' if there is none,
    or None if it is a method that cannot accept its instance."""
    try:
        return str(inspect.signature(callable_))
    except Exception as err:
        if str(err).startswith(_invalid_method):
            return None
        return ''


def get_argspec(ob):
    """Return the calltip for *ob*: its signature (wrapped when long)
    followed by the first lines of its docstring; '' for non-callables.
    """
    try:
        call = ob.__call__
    except BaseException:  # buggy user objects can raise anything
        return ''
    # For an instance with __call__, describe the bound method; classes
    # and functions describe themselves.
    target = call if isinstance(call, types.MethodType) else ob

    signature = _signature_text(target)
    if signature is None:
        return _invalid_method
    if isinstance(target, type) and signature == '()':
        # The class does not tell how to call it.
        signature = _default_callable_argspec

    if len(signature) > _MAX_COLS:
        lines = textwrap.wrap(signature, _MAX_COLS, subsequent_indent=_INDENT)
    elif signature:
        lines = [signature]
    else:
        lines = []

    doc = inspect.getdoc(ob)
    if doc:
        for line in doc.split('\n', _MAX_LINES)[:_MAX_LINES]:
            line = line.strip()
            if not line:
                break
            if len(line) > _MAX_COLS:
                line = line[:_MAX_COLS - 3] + '...'
            lines.append(line)
    return '\n'.join(lines) or _default_callable_argspec


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_calltip', verbosity=2)
