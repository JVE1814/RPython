"""The debugger across the GUI / user process boundary.

With a user process, the Idb (bdb) runs there while the Debugger window
lives in the GUI.  Objects that cannot be pickled -- frames, code objects,
dicts and tracebacks -- stay in the user process, registered in tables by
id; the GUI works with proxies that fetch what they need over rpc.

User process: start_debugger() registers an IdbAdapter (commands from the
GUI) and gives the Idb a GUIProxy (stops reported to the GUI).
GUI: start_remote_debugger() creates the Debugger with an IdbProxy and
registers a GUIAdapter.
"""
import reprlib
import types

from idlelib import debugger

debugging = 0

idb_adap_oid = "idb_adapter"
gui_adap_oid = "gui_adapter"

# Objects of the user process referred to by id from the GUI.
frametable = {}
dicttable = {}
codetable = {}
tracebacktable = {}


def wrap_frame(frame):
    fid = id(frame)
    frametable[fid] = frame
    return fid


def wrap_info(info):
    """Replace the traceback of exc_info *info* by its id."""
    if info is None:
        return None
    tb = info[2]
    assert isinstance(tb, types.TracebackType)
    tbid = id(tb)
    tracebacktable[tbid] = tb
    return (info[0], info[1], tbid)


class GUIProxy:
    """User process: the Idb's view of the debugger window."""

    def __init__(self, conn, gui_adap_oid):
        self.conn = conn
        self.oid = gui_adap_oid

    def interaction(self, message, frame, info=None):
        # The remote call returns when the user has chosen what to do next.
        self.conn.remotecall(self.oid, "interaction",
                             (message, wrap_frame(frame), wrap_info(info)), {})


class IdbAdapter:
    """User process: the commands the GUI sends to the Idb, plus access to
    the frames, code objects and dicts of the program."""

    def __init__(self, idb):
        self.idb = idb

    # -- control

    def set_step(self):
        self.idb.set_step()

    def set_quit(self):
        self.idb.set_quit()

    def set_continue(self):
        self.idb.set_continue()

    def set_next(self, fid):
        self.idb.set_next(frametable[fid])

    def set_return(self, fid):
        self.idb.set_return(frametable[fid])

    def get_stack(self, fid, tbid):
        frame = frametable[fid]
        tb = None if tbid is None else tracebacktable[tbid]
        stack, i = self.idb.get_stack(frame, tb)
        return [(wrap_frame(f), k) for f, k in stack], i

    def run(self, cmd):
        import __main__
        self.idb.run(cmd, __main__.__dict__)

    def set_break(self, filename, lineno):
        return self.idb.set_break(filename, lineno)

    def clear_break(self, filename, lineno):
        return self.idb.clear_break(filename, lineno)

    def clear_all_file_breaks(self, filename):
        return self.idb.clear_all_file_breaks(filename)

    # -- frames

    def frame_attr(self, fid, name):
        return getattr(frametable[fid], name)

    def frame_globals(self, fid):
        d = frametable[fid].f_globals
        did = id(d)
        dicttable[did] = d
        return did

    def frame_locals(self, fid):
        d = frametable[fid].f_locals
        did = id(d)
        dicttable[did] = d
        return did

    def frame_code(self, fid):
        code = frametable[fid].f_code
        cid = id(code)
        codetable[cid] = code
        return cid

    # -- code objects

    def code_name(self, cid):
        return codetable[cid].co_name

    def code_filename(self, cid):
        return codetable[cid].co_filename

    # -- dicts

    def dict_keys(self, did):
        raise NotImplementedError("dict_keys not public or pickleable")

    def dict_keys_list(self, did):
        return list(dicttable[did].keys())

    def dict_item(self, did, key):
        value = dicttable[did][key]
        return reprlib.repr(value)  # the GUI shows the text only


def start_debugger(rpchandler, gui_adap_oid):
    """User process: create the Idb for the GUI's debugger window;
    return the id of its adapter."""
    gui_proxy = GUIProxy(rpchandler, gui_adap_oid)
    idb = debugger.Idb(gui_proxy)
    rpchandler.register(idb_adap_oid, IdbAdapter(idb))
    return idb_adap_oid


# ---------------------------------------------------------------------------
# GUI side

class FrameProxy:
    """A frame of the user process."""

    def __init__(self, conn, fid):
        self._conn = conn
        self._fid = fid
        self._oid = "idb_adapter"
        self._dictcache = {}

    def __getattr__(self, name):
        if name[:1] == "_":
            raise AttributeError(name)
        if name == "f_code":
            return self._get_f_code()
        if name == "f_globals":
            return self._get_f_globals()
        if name == "f_locals":
            return self._get_f_locals()
        return self._conn.remotecall(self._oid, "frame_attr",
                                     (self._fid, name), {})

    def _get_f_code(self):
        cid = self._conn.remotecall(self._oid, "frame_code", (self._fid,), {})
        return CodeProxy(self._conn, self._oid, cid)

    def _get_f_globals(self):
        did = self._conn.remotecall(self._oid, "frame_globals", (self._fid,), {})
        return self._get_dict_proxy(did)

    def _get_f_locals(self):
        did = self._conn.remotecall(self._oid, "frame_locals", (self._fid,), {})
        return self._get_dict_proxy(did)

    def _get_dict_proxy(self, did):
        if did not in self._dictcache:
            self._dictcache[did] = DictProxy(self._conn, self._oid, did)
        return self._dictcache[did]


class CodeProxy:
    """A code object of the user process (only name and file name)."""

    def __init__(self, conn, oid, cid):
        self._conn = conn
        self._oid = oid
        self._cid = cid

    def __getattr__(self, name):
        if name == "co_name":
            return self._conn.remotecall(self._oid, "code_name", (self._cid,), {})
        if name == "co_filename":
            return self._conn.remotecall(self._oid, "code_filename",
                                         (self._cid,), {})
        raise AttributeError(name)


class DictProxy:
    """A namespace of the user process; values are reprs."""

    def __init__(self, conn, oid, did):
        self._conn = conn
        self._oid = oid
        self._did = did

    def keys(self):
        return self._conn.remotecall(self._oid, "dict_keys_list",
                                     (self._did,), {})

    def __getitem__(self, key):
        return self._conn.remotecall(self._oid, "dict_item",
                                     (self._did, key), {})

    def __getattr__(self, name):
        raise AttributeError(name)


class GUIAdapter:
    """GUI: receives the stops reported by the user process."""

    def __init__(self, conn, gui):
        self.conn = conn
        self.gui = gui

    def interaction(self, message, fid, modified_info):
        frame = FrameProxy(self.conn, fid)
        self.gui.interaction(message, frame, modified_info)


class IdbProxy:
    """GUI: the Idb of the user process."""

    def __init__(self, conn, shell, oid):
        self.oid = oid
        self.conn = conn
        self.shell = shell

    def call(self, methodname, /, *args, **kwargs):
        return self.conn.remotecall(self.oid, methodname, args, kwargs)

    def run(self, cmd, locals):
        # Run through the user process's main thread queue; the shell
        # waits for the result like for any other code.
        seq = self.conn.asyncqueue(self.oid, "run", (cmd,), {})
        self.shell.interp.active_seq = seq

    def get_stack(self, frame, tbid):
        stack, i = self.call("get_stack", frame._fid, tbid)
        return [(FrameProxy(self.conn, fid), k) for fid, k in stack], i

    def set_continue(self):
        self.call("set_continue")

    def set_step(self):
        self.call("set_step")

    def set_next(self, frame):
        self.call("set_next", frame._fid)

    def set_return(self, frame):
        self.call("set_return", frame._fid)

    def set_quit(self):
        self.call("set_quit")

    def set_break(self, filename, lineno):
        return self.call("set_break", filename, lineno)

    def clear_break(self, filename, lineno):
        return self.call("clear_break", filename, lineno)

    def clear_all_file_breaks(self, filename):
        return self.call("clear_all_file_breaks", filename)


def start_remote_debugger(rpcclt, pyshell):
    """GUI: start the Idb in the user process and open the window."""
    global idb_adap_oid
    idb_adap_oid = rpcclt.remotecall("exec", "start_the_debugger",
                                     (gui_adap_oid,), {})
    idb_proxy = IdbProxy(rpcclt, pyshell, idb_adap_oid)
    gui = debugger.Debugger(pyshell, idb_proxy)
    rpcclt.register(gui_adap_oid, GUIAdapter(rpcclt, gui))
    return gui


def close_remote_debugger(rpcclt):
    """GUI: stop the debugger of the user process and forget it."""
    close_subprocess_debugger(rpcclt)
    rpcclt.unregister(gui_adap_oid)


def close_subprocess_debugger(rpcclt):
    rpcclt.remotecall("exec", "stop_the_debugger", (idb_adap_oid,), {})


def restart_subprocess_debugger(rpcclt):
    """GUI: start the Idb again in a restarted user process."""
    oid = rpcclt.remotecall("exec", "start_the_debugger", (gui_adap_oid,), {})
    assert oid == idb_adap_oid, 'Idb restarted with different oid'


if __name__ == "__main__":
    from unittest import main
    main('idlelib.idle_test.test_debugger_r', verbosity=2, exit=False)
