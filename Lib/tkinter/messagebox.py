"""Standard message boxes (Tk's tk_messageBox).

Each function shows a modal box with a title, a message and a set of
buttons and returns the user's answer.  Keyword options: default (the
default button), icon, parent, detail.
"""

from tkinter.commondialog import Dialog

__all__ = ["showinfo", "showwarning", "showerror",
           "askquestion", "askokcancel", "askyesno",
           "askyesnocancel", "askretrycancel"]

# icons
ERROR = "error"
INFO = "info"
QUESTION = "question"
WARNING = "warning"

# button sets
ABORTRETRYIGNORE = "abortretryignore"
OK = "ok"
OKCANCEL = "okcancel"
RETRYCANCEL = "retrycancel"
YESNO = "yesno"
YESNOCANCEL = "yesnocancel"

# answers
ABORT = "abort"
RETRY = "retry"
IGNORE = "ignore"
OK = "ok"
CANCEL = "cancel"
YES = "yes"
NO = "no"


class Message(Dialog):
    """A tk_messageBox dialog."""
    command = "tk_messageBox"


def _show(title=None, message=None, _icon=None, _type=None, **options):
    if _icon and "icon" not in options:
        options["icon"] = _icon
    if _type and "type" not in options:
        options["type"] = _type
    if title:
        options["title"] = title
    if message:
        options["message"] = message
    res = Message(**options).show()
    # some Tk versions answer with a boolean
    if isinstance(res, bool):
        return YES if res else NO
    return str(res)


def showinfo(title=None, message=None, **options):
    """Show an information message; return "ok"."""
    return _show(title, message, INFO, OK, **options)


def showwarning(title=None, message=None, **options):
    """Show a warning message; return "ok"."""
    return _show(title, message, WARNING, OK, **options)


def showerror(title=None, message=None, **options):
    """Show an error message; return "ok"."""
    return _show(title, message, ERROR, OK, **options)


def askquestion(title=None, message=None, **options):
    """Ask a yes/no question; return "yes" or "no"."""
    return _show(title, message, QUESTION, YESNO, **options)


def askokcancel(title=None, message=None, **options):
    """Ask whether to proceed; return True for OK."""
    return _show(title, message, QUESTION, OKCANCEL, **options) == OK


def askyesno(title=None, message=None, **options):
    """Ask a question; return True for yes."""
    return _show(title, message, QUESTION, YESNO, **options) == YES


def askyesnocancel(title=None, message=None, **options):
    """Ask a question; return True, False, or None for cancel."""
    s = _show(title, message, QUESTION, YESNOCANCEL, **options)
    if s == CANCEL:
        return None
    return s == YES


def askretrycancel(title=None, message=None, **options):
    """Ask whether to retry; return True for retry."""
    return _show(title, message, WARNING, RETRYCANCEL, **options) == RETRY


if __name__ == "__main__":
    print("info", showinfo("Spam", "Egg Information"))
    print("warning", showwarning("Spam", "Egg Warning"))
    print("error", showerror("Spam", "Egg Alert"))
    print("question", askquestion("Spam", "Question?"))
    print("proceed", askokcancel("Spam", "Proceed?"))
    print("yes/no", askyesno("Spam", "Got it?"))
    print("yes/no/cancel", askyesnocancel("Spam", "Want it?"))
    print("try again", askretrycancel("Spam", "Try again?"))
