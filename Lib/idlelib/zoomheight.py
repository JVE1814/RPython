"""Options > Zoom Height: toggle a window between its own height and the
full usable height of the screen."""
import re
import sys
import tkinter


class WmInfoGatheringError(Exception):
    pass


# (usable height, y) of a full-height window, by screen size.
_full_height = {}


def get_window_geometry(top):
    """Return (width, height, x, y) of a top-level window."""
    m = re.match(r"(\d+)x(\d+)\+(-?\d+)\+(-?\d+)", top.wm_geometry())
    return tuple(map(int, m.groups()))


def set_window_geometry(top, geometry):
    top.wm_geometry("{:d}x{:d}+{:d}+{:d}".format(*geometry))


def _full_height_of(top):
    screen = (top.winfo_screenwidth(), top.winfo_screenheight())
    if screen not in _full_height:
        state = top.wm_state()
        try:
            top.wm_state('zoomed')
        except tkinter.TclError:
            # No 'zoomed' state (X11): leave room for panels and title.
            _full_height[screen] = (top.winfo_screenheight() - 72, 0)
        else:
            top.update()
            width, height, x, y = get_window_geometry(top)
            if sys.platform == 'win32':
                y = 0
            top.wm_state('normal')
            top.update()
            decoration = top.winfo_rooty() - top.winfo_y()
            top.wm_state(state)
            _full_height[screen] = (height - decoration, y)
    return _full_height[screen]


def zoom_height(top):
    """Toggle the height of *top*.  Return True if it is now full height,
    False if restored, None if it cannot be changed (e.g. maximized)."""
    if top.wm_state() != 'normal':
        return None
    width, height, x, y = get_window_geometry(top)
    try:
        full, full_y = _full_height_of(top)
    except WmInfoGatheringError:
        return None
    if height != full:
        set_window_geometry(top, (width, full, x, full_y))
        return True
    top.wm_geometry('')
    return False


class ZoomHeight:

    def __init__(self, editwin):
        self.editwin = editwin
        self.top = self.editwin.top

    def zoom_height_event(self, event=None):
        zoomed = self.zoom_height()
        label = 'Restore Height' if zoomed else 'Zoom Height'
        self.editwin.update_menu_label(menu='options', index='* Height',
                                       label=label)
        return "break"

    def zoom_height(self):
        return zoom_height(self.top)


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_zoomheight', verbosity=2, exit=False)
