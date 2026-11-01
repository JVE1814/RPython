"""Open URLs in a web browser.

open(url, new=0, autoraise=True) shows `url` in the first usable browser
(new=1 asks for a new window, new=2 for a new tab).  Browsers are found on
first use: the BROWSER environment variable (a os.pathsep separated list of
commands, '%s' standing for the URL) is preferred, then graphical browsers
when a display is available, then text-mode browsers.  register() adds
browsers of your own.
"""

import os
import shlex
import shutil
import subprocess
import sys
import threading

__all__ = ["Error", "open", "open_new", "open_new_tab", "get", "register"]


class Error(Exception):
    pass


_lock = threading.RLock()
_browsers = {}              # lower-case name -> [class, instance]
_tryorder = None            # names in order of preference (None: not set up)
_os_preferred_browser = None


def register(name, klass, instance=None, *, preferred=False):
    """Make browser `name` available to get(): either a class to
    instantiate without arguments, or a ready `instance`."""
    with _lock:
        if _tryorder is None:
            register_standard_browsers()
        _browsers[name.lower()] = [klass, instance]
        if preferred or (_os_preferred_browser
                         and f'{name}.desktop' == _os_preferred_browser):
            _tryorder.insert(0, name)
        else:
            _tryorder.append(name)


def get(using=None):
    """A controller for browser `using` (a registered name or a command
    line), or for the first usable browser."""
    if _tryorder is None:
        with _lock:
            if _tryorder is None:
                register_standard_browsers()
    candidates = [using] if using is not None else list(_tryorder)
    for browser in candidates:
        if '%s' in browser:
            words = shlex.split(browser)
            if words[-1] == '&':
                return BackgroundBrowser(words[:-1])
            return GenericBrowser(words)
        entry = _browsers.get(browser.lower())
        if entry is None:
            entry = _synthesize(browser)
        if entry[1] is not None:
            return entry[1]
        if entry[0] is not None:
            return entry[0]()
    raise Error("could not locate runnable browser")


def open(url, new=0, autoraise=True):
    """Show `url` in the first browser that accepts it; True on success."""
    if _tryorder is None:
        with _lock:
            if _tryorder is None:
                register_standard_browsers()
    for name in _tryorder:
        browser = get(name)
        if browser.open(url, new, autoraise):
            return True
    return False


def open_new(url):
    """Open `url` in a new browser window if possible."""
    return open(url, 1)


def open_new_tab(url):
    """Open `url` in a new browser tab if possible."""
    return open(url, 2)


def _synthesize(browser, *, preferred=False):
    """Derive a controller for the command line `browser` from a registered
    controller of the same program; returns [None, controller-or-None]."""
    command = browser.split()[0]
    if not shutil.which(command):
        return [None, None]
    program = os.path.basename(command)
    entry = _browsers.get(program.lower())
    if entry is None:
        return [None, None]
    controller = entry[1]
    if controller is not None and program.lower() == controller.basename:
        import copy
        controller = copy.copy(controller)
        controller.name = browser
        controller.basename = os.path.basename(browser)
        register(browser, None, instance=controller, preferred=preferred)
        return [None, controller]
    return [None, None]


def _reject_dash(value):
    if value.startswith('-'):
        raise ValueError(f"Invalid URL (leading dash disallowed): {value!r}")


# ---------------------------------------------------------------- controllers

class BaseBrowser:
    """Common interface of the browser controllers."""

    args = ['%s']

    def __init__(self, name=""):
        self.name = name
        self.basename = name

    def open(self, url, new=0, autoraise=True):
        raise NotImplementedError

    def open_new(self, url):
        return self.open(url, 1)

    def open_new_tab(self, url):
        return self.open(url, 2)

    @staticmethod
    def _check_url(url):
        if url and url.lstrip().startswith('-'):
            raise ValueError(f"Invalid URL (leading dash disallowed): {url!r}")


class GenericBrowser(BaseBrowser):
    """Runs a command and waits for it.  `name` is the program, or a list
    of the program and its arguments ('%s' is replaced by the URL)."""

    def __init__(self, name):
        if isinstance(name, str):
            self.name = name
            self.args = ['%s']
        else:
            self.name = name[0]
            self.args = name[1:]
        self.basename = os.path.basename(self.name)

    def _command(self, url):
        return [self.name] + [arg.replace('%s', url) for arg in self.args]

    def open(self, url, new=0, autoraise=True):
        sys.audit("webbrowser.open", url)
        self._check_url(url)
        try:
            process = subprocess.Popen(self._command(url), close_fds=True)
            return not process.wait()
        except OSError:
            return False


class BackgroundBrowser(GenericBrowser):
    """Starts a command in the background (a new session)."""

    def open(self, url, new=0, autoraise=True):
        sys.audit("webbrowser.open", url)
        self._check_url(url)
        try:
            process = subprocess.Popen(self._command(url), close_fds=True,
                                       start_new_session=True)
            return process.poll() is None
        except OSError:
            return False


class UnixBrowser(BaseBrowser):
    """A browser driven through its command line.

    `remote_args` is tried first; '%action' in it becomes the remote action
    for the requested kind of window and '%s' the URL.  When that fails, the
    browser is started with `args`."""

    raise_opts = None           # [option without raise, option with raise]
    background = False
    redirect_stdout = True
    remote_args = ['%action', '%s']
    remote_action = None
    remote_action_newwin = None
    remote_action_newtab = None

    def _invoke(self, args, remote, autoraise, url=None):
        options = []
        if remote and self.raise_opts:
            choice = self.raise_opts[int(bool(autoraise))]
            if choice:
                options = [choice]
        command = [self.name] + options + args
        quiet = subprocess.DEVNULL if (remote or self.background) else None
        try:
            process = subprocess.Popen(
                command, close_fds=True, stdin=quiet,
                stdout=quiet if self.redirect_stdout else None,
                stderr=quiet, start_new_session=True)
        except OSError:
            return False
        if remote:
            try:
                return not process.wait(5)
            except subprocess.TimeoutExpired:
                return True
        if self.background:
            return process.poll() is None
        return not process.wait()

    def _action_for(self, new):
        if new == 0:
            return self.remote_action
        if new == 1:
            return self.remote_action_newwin
        if new == 2:
            if self.remote_action_newtab is None:
                return self.remote_action_newwin
            return self.remote_action_newtab
        raise Error("Bad 'new' parameter to open(); "
                    f"expected 0, 1, or 2, got {new}")

    def open(self, url, new=0, autoraise=True):
        sys.audit("webbrowser.open", url)
        self._check_url(url)
        action = self._action_for(new)
        args = []
        for template in self.remote_args:
            value = template.replace('%s', url).replace('%action', action or '')
            if not value:
                continue
            if '%s' in template:
                # the URL must not turn into an option of the browser
                _reject_dash(value)
            args.append(value)
        if self._invoke(args, True, autoraise, url):
            return True
        args = [template.replace('%s', url) for template in self.args]
        return self._invoke(args, False, False)


class Mozilla(UnixBrowser):
    """Firefox and other Mozilla browsers."""
    remote_args = ['%action', '%s']
    remote_action = ""
    remote_action_newwin = "-new-window"
    remote_action_newtab = "-new-tab"
    background = True


class Epiphany(UnixBrowser):
    """GNOME Web (Epiphany)."""
    raise_opts = ["-noraise", ""]
    remote_args = ['%action', '%s']
    remote_action = "-n"
    remote_action_newwin = "-w"
    remote_action_newtab = "-w"
    background = True


class Chrome(UnixBrowser):
    """Google Chrome."""
    remote_args = ['%action', '%s']
    remote_action = ""
    remote_action_newwin = "--new-window"
    remote_action_newtab = ""
    background = True


Chromium = Chrome


class Opera(UnixBrowser):
    """Opera."""
    remote_args = ['%action', '%s']
    remote_action = ""
    remote_action_newwin = "--new-window"
    remote_action_newtab = ""
    background = True


class Elinks(UnixBrowser):
    """ELinks, controlled through -remote."""
    remote_args = ['-remote', 'openURL(%s%action)']
    remote_action = ""
    remote_action_newwin = ",new-window"
    remote_action_newtab = ",new-tab"
    background = False
    # ELinks needs the terminal on stdout
    redirect_stdout = False


class Edge(UnixBrowser):
    """Microsoft Edge."""
    remote_args = ['%action', '%s']
    remote_action = ""
    remote_action_newwin = "--new-window"
    remote_action_newtab = ""
    background = True


class Konqueror(BaseBrowser):
    """KDE's Konqueror, through kfmclient (or by starting it)."""

    def open(self, url, new=0, autoraise=True):
        sys.audit("webbrowser.open", url)
        self._check_url(url)
        action = "newTab" if new == 2 else "openURL"
        quiet = subprocess.DEVNULL
        for command in (["kfmclient", action, url],
                        ["konqueror", "--silent", url],
                        ["kfm", "-d", url]):
            try:
                process = subprocess.Popen(command, close_fds=True, stdin=quiet,
                                           stdout=quiet, stderr=quiet,
                                           start_new_session=True)
            except OSError:
                continue
            if command[0] == "kfmclient":
                return not process.wait()
            return process.poll() is None
        return False


# ---------------------------------------------------------------- discovery

def _register_if_found(name, klass=None, instance=None, *, preferred=False):
    if shutil.which(name):
        if klass is None and instance is None:
            klass = lambda: BackgroundBrowser(name)
        register(name, klass, instance, preferred=preferred)
        return True
    return False


def register_X_browsers():
    """Register the graphical browsers that are installed."""
    # desktop-neutral openers
    if shutil.which("xdg-open"):
        register("xdg-open", None, BackgroundBrowser("xdg-open"))
    if shutil.which("gio"):
        register("gio", None, BackgroundBrowser(["gio", "open", "--", "%s"]))
    desktop = os.environ.get("XDG_CURRENT_DESKTOP", "")
    if "GNOME" in desktop and shutil.which("gvfs-open"):
        register("gvfs-open", None, BackgroundBrowser("gvfs-open"))
    if "KDE" in desktop and shutil.which("kfmclient"):
        register("kfmclient", Konqueror, Konqueror("kfmclient"))
    if shutil.which("x-www-browser"):
        register("x-www-browser", None, BackgroundBrowser("x-www-browser"))
    for name in ("firefox", "iceweasel", "seamonkey", "mozilla-firefox",
                 "mozilla"):
        if shutil.which(name):
            register(name, None, Mozilla(name))
    for name in ("google-chrome", "chrome", "chromium", "chromium-browser"):
        if shutil.which(name):
            register(name, None, Chrome(name))
    for name in ("microsoft-edge", "microsoft-edge-stable"):
        if shutil.which(name):
            register(name, None, Edge(name))
    if shutil.which("epiphany"):
        register("epiphany", None, Epiphany("epiphany"))
    if shutil.which("opera"):
        register("opera", None, Opera("opera"))
    for name in ("konqueror", "kfm"):
        if shutil.which(name):
            register(name, Konqueror, Konqueror(name))


def register_standard_browsers():
    """Fill the registry with the browsers available on this system."""
    global _tryorder, _os_preferred_browser
    _tryorder = []
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        try:
            output = subprocess.check_output(
                ["xdg-settings", "get", "default-web-browser"],
                stderr=subprocess.DEVNULL)
        except (FileNotFoundError, NotADirectoryError, PermissionError,
                subprocess.CalledProcessError):
            pass
        else:
            _os_preferred_browser = output.decode().strip()
        register_X_browsers()
    if os.environ.get("TERM"):
        if shutil.which("www-browser"):
            register("www-browser", None, GenericBrowser("www-browser"))
        for name in ("links", "lynx", "w3m"):
            if shutil.which(name):
                register(name, None, GenericBrowser(name))
        if shutil.which("elinks"):
            register("elinks", None, Elinks("elinks"))
    if "BROWSER" in os.environ:
        commands = [c for c in os.environ["BROWSER"].split(os.pathsep) if c]
        for command in reversed(commands):
            entry = _synthesize(command, preferred=True)
            if entry[1] is None:
                register(command, None, GenericBrowser(command), preferred=True)


# ---------------------------------------------------------------- command line

def parse_args(arg_list=None):
    import argparse
    parser = argparse.ArgumentParser(
        description="Open URL in a web browser.", color=True)
    parser.add_argument("url", help="URL to open")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("-n", "--new-window", action="store_const",
                       const=1, default=0, dest="new_win",
                       help="open new window")
    group.add_argument("-t", "--new-tab", action="store_const",
                       const=2, default=0, dest="new_win",
                       help="open new tab")
    return parser.parse_args(arg_list)


def main(arg_list=None):
    args = parse_args(arg_list)
    open(args.url, args.new_win)
    print("\a")


if __name__ == "__main__":
    main()
