"""Adaptations of IDLE to macOS and the Tk variants it may run with."""
from os.path import expanduser
import plistlib
from sys import platform  # Tests replace this to simulate other platforms.

import tkinter


# The kind of Tk in use: 'carbon', 'cocoa', 'xquartz' (all on macOS) or
# 'other'.  None until _init_tk_type() has looked.
_tk_type = None


def _init_tk_type():
    """Determine the kind of Tk (needs a temporary root window on macOS)."""
    global _tk_type
    if platform == 'darwin':
        # A non-Tk-specific test would be nice, but the windowing system
        # is only known after Tk is initialised.
        root = tkinter.Tk()
        try:
            ws = root.tk.call('tk', 'windowingsystem')
            if 'x11' in ws:
                _tk_type = "xquartz"
            elif 'aqua' not in ws:
                _tk_type = "other"
            elif 'AppKit' in root.tk.call('winfo', 'server', '.'):
                _tk_type = "cocoa"
            else:
                _tk_type = "carbon"
        finally:
            root.update_idletasks()
            root.destroy()
    else:
        _tk_type = "other"


def isAquaTk():
    """Return True for a native macOS Tk (Cocoa or Carbon)."""
    if not _tk_type:
        _init_tk_type()
    return _tk_type in ("cocoa", "carbon")


def isCarbonTk():
    """Return True for the old Carbon-based Tk on macOS."""
    if not _tk_type:
        _init_tk_type()
    return _tk_type == "carbon"


def isCocoaTk():
    """Return True for the Cocoa-based Tk on macOS."""
    if not _tk_type:
        _init_tk_type()
    return _tk_type == "cocoa"


def isXQuartz():
    """Return True for an X11 Tk running under XQuartz on macOS."""
    if not _tk_type:
        _init_tk_type()
    return _tk_type == "xquartz"


def tkVersionWarning(root):
    """Return a warning about Tk versions known to misbehave, or False."""
    if isCocoaTk():
        patchlevel = root.tk.call('info', 'patchlevel')
        if patchlevel not in ('8.5.7', '8.5.9'):
            return False
        return ("WARNING: The version of Tcl/Tk ({0}) in use may"
                " be unstable.\n"
                "Visit https://www.python.org/download/mac/tcltk/"
                " for current information.".format(patchlevel))
    return False


def readSystemPreferences():
    """Return the global macOS preferences (a dict), or None."""
    if platform != 'darwin':
        return None
    path = expanduser('~/Library/Preferences/.GlobalPreferences.plist')
    try:
        with open(path, 'rb') as f:
            return plistlib.load(f)
    except OSError:
        return None


def preferTabsPreferenceWarning():
    """Warn when macOS opens documents in tabs (IDLE expects windows)."""
    if platform == 'darwin':
        prefs = readSystemPreferences()
        if prefs and prefs.get('AppleWindowTabbingMode') == 'always':
            return ('WARNING: The system preference "Prefer tabs when'
                    ' opening documents" is set to "Always". This will'
                    ' cause various problems with IDLE. For the best'
                    ' experience, change this setting when running'
                    ' IDLE (via System Preferences -> Dock).')
    return None


def addOpenEventSupport(root, flist):
    """Open files dropped on the IDLE icon (macOS 'open document' event)."""
    def doOpenFile(*args):
        for fn in args:
            flist.open(fn)

    # The command name is defined by Tk's macOS support.
    root.createcommand("::tk::mac::OpenDocument", doOpenFile)


def hideTkConsole(root):
    """Hide the Tk console window some macOS Tk builds open."""
    try:
        root.tk.call('console', 'hide')
    except tkinter.TclError:
        pass  # no console in this Tk


def overrideRootMenu(root, flist):
    """Replace Tk's default macOS application menu by IDLE's menus."""
    from tkinter import Menu
    from idlelib import mainmenu
    from idlelib import window

    def drop(menu_name, event):
        # Remove the item for *event* (and a separator that follows it).
        for name, items in mainmenu.menudefs:
            if name != menu_name:
                continue
            for i, item in enumerate(items):
                if item is not None and item[1] == event:
                    del items[i]
                    if i < len(items) and items[i] is None:
                        del items[i]
                    return item
        return None

    # macOS puts Quit, About and Preferences in the application menu;
    # Close Window moves up next to the file commands.
    drop('file', '<<close-all-windows>>')
    close_item = drop('file', '<<close-window>>')
    file_items = mainmenu.menudefs[0][1]
    while file_items and file_items[-1] is None:
        file_items.pop()
    if close_item:
        file_items.insert(6, close_item)
    drop('help', '<<about-idle>>')
    drop('options', '<<open-config-dialog>>')
    menubar = Menu(root)
    root.configure(menu=menubar)

    menu = Menu(menubar, name='window', tearoff=0)
    menubar.add_cascade(label='Window', menu=menu, underline=0)

    def postwindowsmenu(menu=menu):
        end = menu.index('end')
        if end is None:
            end = -1
        if end > 0:
            menu.delete(0, end)
        window.add_windows_to_menu(menu)
    window.register_callback(postwindowsmenu)

    def about_dialog(event=None):
        from idlelib import help_about
        help_about.AboutDialog(root)

    def config_dialog(event=None):
        from idlelib import configdialog
        # Tk tracks a window as a toplevel for the root
        root.instance_dict = flist.inversedict
        configdialog.ConfigDialog(root, 'Settings')

    def help_dialog(event=None):
        from idlelib import help
        help.show_idlehelp(root)

    root.bind('<<about-idle>>', about_dialog)
    root.bind('<<open-config-dialog>>', config_dialog)
    root.createcommand('::tk::mac::ShowPreferences', config_dialog)
    if flist:
        root.bind('<<close-all-windows>>', flist.close_all_callback)
        # The binding above doesn't reach the Quit item of the app menu.
        root.createcommand('exit', flist.close_all_callback)

    if isCarbonTk():
        # In Carbon Tk the application menu is the first menu.
        menu = Menu(menubar, name='apple', tearoff=0)
        menubar.add_cascade(label='IDLE', menu=menu)
        mainmenu.menudefs.insert(0,
            ('application', [
                ('About IDLE', '<<about-idle>>'),
                None,
            ]))
    if isCocoaTk():
        # Cocoa Tk provides the application menu; hook up its items.
        root.createcommand('tkAboutDialog', about_dialog)
        root.createcommand('::tk::mac::ShowHelp', help_dialog)
        # The Help menu is created by Tk; remove IDLE's own help item.
        drop('help', '<<help>>')


def fixb2context(root):
    """Removed in Tk 8.6: <B2> must not post a context menu on macOS."""
    root.unbind_class('Text', "<B2>")
    root.unbind_class('Text', "<B2-Motion>")
    root.unbind_class('Text', "<<PasteSelection>>")


def setupApp(root, flist):
    """Adapt IDLE to the macOS Tk in use (call once, with the root)."""
    if isAquaTk():
        hideTkConsole(root)
        overrideRootMenu(root, flist)
        addOpenEventSupport(root, flist)
        fixb2context(root)


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_macosx', verbosity=2)
