"""Remote procedure calls between the IDLE GUI and the user's process.

The shell runs user code in a separate Python process; the two talk over
a TCP socket on localhost.  Each message is a pickled (seq, payload) pair
preceded by its 4-byte length.  A payload is either a request

    ("CALL" or "QUEUE", (object id, method name, args, kwargs))

or a response ("OK", value), ("QUEUED", None), ("CALLEXC", exception),
("ERROR", message), ("EXCEPTION", None) or ("EOF", None).

Objects registered in an object table under an id can be called remotely
through an RPCProxy.  Requests are served by the thread owning the socket;
other threads wait on a condition variable for their response.  Code
objects are pickled with marshal (CodePickler).
"""
import builtins
import copyreg
import io
import marshal
import os
import pickle
import queue
import select
import socket
import socketserver
import struct
import sys
import threading
import traceback
import types


def unpickle_code(ms):
    """Rebuild a code object pickled by pickle_code()."""
    co = marshal.loads(ms)
    assert isinstance(co, types.CodeType)
    return co


def pickle_code(co):
    """Pickle a code object as its marshal data."""
    assert isinstance(co, types.CodeType)
    ms = marshal.dumps(co)
    return unpickle_code, (ms,)


def dumps(obj, protocol=None):
    """Pickle *obj*, allowing code objects in it."""
    f = io.BytesIO()
    p = CodePickler(f, protocol)
    p.dump(obj)
    return f.getvalue()


class CodePickler(pickle.Pickler):
    dispatch_table = {types.CodeType: pickle_code, **copyreg.dispatch_table}


BUFSIZE = 8 * 1024
LOCALHOST = '127.0.0.1'


class RPCServer(socketserver.TCPServer):
    """The user process side: it connects to the GUI, which listens."""

    def __init__(self, addr, handlerclass=None):
        if handlerclass is None:
            handlerclass = RPCHandler
        socketserver.TCPServer.__init__(self, addr, handlerclass)

    def server_bind(self):
        # Do not bind: this server connects (see server_activate).
        pass

    def server_activate(self):
        # Connect to the GUI's listening socket instead of listening.
        self.socket.connect(self.server_address)

    def get_request(self):
        # The connected socket is the only "request".
        return self.socket, self.server_address

    def handle_error(self, request, client_address):
        """Report an unexpected error of the server and exit."""
        try:
            raise
        except SystemExit:
            raise
        except:
            erf = sys.__stderr__
            print('\n' + '-' * 40, file=erf)
            print('Unhandled server exception!', file=erf)
            print('Thread: %s' % threading.current_thread().name, file=erf)
            print('Client Address: ', client_address, file=erf)
            print('Request: ', repr(request), file=erf)
            traceback.print_exc(file=erf)
            print('\n*** Unrecoverable, server exiting!', file=erf)
            print('-' * 40, file=erf)
            os._exit(0)


# Objects that may be called remotely, by id.
objecttable = {}
# Requests to be executed by the main thread of the user process, and
# their responses.
request_queue = queue.Queue(0)
response_queue = queue.Queue(0)


class SocketIO:
    """One end of an RPC connection."""

    nextseq = 0
    debugging = False
    location = "#?"

    def __init__(self, sock, objtable=None, debugging=None):
        self.sockthread = threading.current_thread()
        if debugging is not None:
            self.debugging = debugging
        self.sock = sock
        if objtable is None:
            objtable = objecttable
        self.objtable = objtable
        self.responses = {}
        self.cvars = {}
        # Reading state: buffered bytes, bytes needed next, and whether the
        # length (0) or the body (1) of a packet is being read.
        self.buff = b''
        self.bufneed = 4
        self.bufstate = 0

    def close(self):
        sock = self.sock
        self.sock = None
        if sock is not None:
            sock.close()

    def exithook(self):
        # Called to end the process; subclasses may do otherwise.
        os._exit(0)

    def debug(self, *args):
        if not self.debugging:
            return
        s = self.location + " " + str(threading.current_thread().name)
        for a in args:
            s = s + " " + str(a)
        print(s, file=sys.__stderr__)

    def register(self, oid, object):
        self.objtable[oid] = object

    def unregister(self, oid):
        try:
            del self.objtable[oid]
        except KeyError:
            pass

    def localcall(self, seq, request):
        """Execute a request from the other side; return the response."""
        self.debug("localcall:", request)
        try:
            how, (oid, methodname, args, kwargs) = request
        except TypeError:
            return ("ERROR", "Bad request format")
        if oid not in self.objtable:
            return ("ERROR", f"Unknown object id: {oid!r}")
        obj = self.objtable[oid]
        if methodname == "__methods__":
            methods = {}
            _getmethods(obj, methods)
            return ("OK", methods)
        if methodname == "__attributes__":
            attributes = {}
            _getattributes(obj, attributes)
            return ("OK", attributes)
        if not hasattr(obj, methodname):
            return ("ERROR", f"Unsupported method name: {methodname!r}")
        method = getattr(obj, methodname)
        try:
            if how == 'CALL':
                ret = method(*args, **kwargs)
                if isinstance(ret, RemoteObject):
                    ret = remoteref(ret)
                return ("OK", ret)
            elif how == 'QUEUE':
                request_queue.put((seq, (method, args, kwargs)))
                return ("QUEUED", None)
            else:
                return ("ERROR", f"Unsupported message type: {how}")
        except SystemExit:
            raise
        except KeyboardInterrupt:
            raise
        except OSError:
            raise
        except Exception as ex:
            return ("CALLEXC", ex)
        except:
            msg = ("*** Internal Error: rpc.py:SocketIO.localcall()\n\n"
                   " Object: %s \n Method: %s \n Args: %s\n")
            print(msg % (oid, method, args), file=sys.__stderr__)
            traceback.print_exc(file=sys.__stderr__)
            return ("EXCEPTION", None)

    def remotecall(self, oid, methodname, args, kwargs):
        """Call a method of a remote object and return its result."""
        self.debug("remotecall:asynccall: ", oid, methodname)
        seq = self.asynccall(oid, methodname, args, kwargs)
        return self.asyncreturn(seq)

    def remotequeue(self, oid, methodname, args, kwargs):
        """Like remotecall, but run by the remote main thread's queue."""
        self.debug("remotequeue:asyncqueue: ", oid, methodname)
        seq = self.asyncqueue(oid, methodname, args, kwargs)
        return self.asyncreturn(seq)

    def asynccall(self, oid, methodname, args, kwargs):
        request = ("CALL", (oid, methodname, args, kwargs))
        seq = self.newseq()
        if threading.current_thread() != self.sockthread:
            self.cvars[seq] = threading.Condition()
        self.debug(("asynccall:%d:" % seq), oid, methodname, args, kwargs)
        self.putmessage((seq, request))
        return seq

    def asyncqueue(self, oid, methodname, args, kwargs):
        request = ("QUEUE", (oid, methodname, args, kwargs))
        seq = self.newseq()
        if threading.current_thread() != self.sockthread:
            self.cvars[seq] = threading.Condition()
        self.debug(("asyncqueue:%d:" % seq), oid, methodname, args, kwargs)
        self.putmessage((seq, request))
        return seq

    def asyncreturn(self, seq):
        self.debug("asyncreturn:%d:call getresponse(): " % seq)
        response = self.getresponse(seq, wait=0.05)
        self.debug(("asyncreturn:%d:response: " % seq), response)
        return self.decoderesponse(response)

    def decoderesponse(self, response):
        how, what = response
        if how == "OK":
            return what
        if how == "QUEUED":
            return None
        if how == "EXCEPTION":
            self.debug("decoderesponse: EXCEPTION")
            return None
        if how == "EOF":
            self.debug("decoderesponse: EOF")
            self.decode_interrupthook()
            return None
        if how == "ERROR":
            self.debug("decoderesponse: Internal ERROR:", what)
            raise RuntimeError(what)
        if how == "CALLEXC":
            self.debug("decoderesponse: Call Exception:", what)
            raise what
        raise SystemError(how, what)

    def decode_interrupthook(self):
        raise EOFError

    def mainloop(self):
        """Serve requests until the connection is closed."""
        try:
            self.getresponse(myseq=None, wait=0.05)
        except EOFError:
            self.debug("mainloop:return")
            return

    def getresponse(self, myseq, wait):
        response = self._getresponse(myseq, wait)
        if response is not None:
            how, what = response
            if how == "OK":
                response = how, self._proxify(what)
        return response

    def _proxify(self, obj):
        if isinstance(obj, RemoteProxy):
            return RPCProxy(self, obj.oid)
        if isinstance(obj, list):
            return list(map(self._proxify, obj))
        return obj

    def _getresponse(self, myseq, wait):
        self.debug("_getresponse:myseq:", myseq)
        if threading.current_thread() is self.sockthread:
            # This thread reads the socket: serve requests until our
            # response arrives.
            while True:
                response = self.pollresponse(myseq, wait)
                if response is not None:
                    return response
        # Another thread reads the socket and notifies us.
        cvar = self.cvars[myseq]
        try:
            with cvar:
                while myseq not in self.responses:
                    cvar.wait()
                return self.responses.pop(myseq)
        finally:
            # Also when the wait is interrupted: a late response for this
            # sequence number is then discarded.
            self.cvars.pop(myseq, None)
            self.responses.pop(myseq, None)

    def newseq(self):
        self.nextseq = seq = self.nextseq + 2
        return seq

    def putmessage(self, message):
        self.debug("putmessage:%d:" % message[0])
        try:
            s = dumps(message)
        except pickle.PicklingError:
            print("Cannot pickle:", repr(message), file=sys.__stderr__)
            raise
        s = struct.pack("<i", len(s)) + s
        while len(s) > 0:
            try:
                r, w, x = select.select([], [self.sock], [])
                n = self.sock.send(s[:BUFSIZE])
            except (AttributeError, TypeError):
                raise OSError("socket no longer exists")
            s = s[n:]

    def _stage0(self):
        if self.bufstate == 0 and len(self.buff) >= 4:
            s = self.buff[:4]
            self.buff = self.buff[4:]
            self.bufneed = struct.unpack("<i", s)[0]
            self.bufstate = 1

    def _stage1(self):
        if self.bufstate == 1 and len(self.buff) >= self.bufneed:
            packet = self.buff[:self.bufneed]
            self.buff = self.buff[self.bufneed:]
            self.bufneed = 4
            self.bufstate = 0
            return packet
        return None

    def pollpacket(self, wait):
        """Return the next complete packet, or None after *wait* seconds."""
        self._stage0()
        if len(self.buff) < self.bufneed:
            r, w, x = select.select([self.sock.fileno()], [], [], wait)
            if len(r) == 0:
                return None
            try:
                s = self.sock.recv(BUFSIZE)
            except OSError:
                raise EOFError
            if len(s) == 0:
                raise EOFError
            self.buff += s
            self._stage0()
        return self._stage1()

    def pollmessage(self, wait):
        packet = self.pollpacket(wait)
        if packet is None:
            return None
        try:
            message = pickle.loads(packet)
        except pickle.UnpicklingError:
            print("-----------------------", file=sys.__stderr__)
            print("cannot unpickle packet:", repr(packet), file=sys.__stderr__)
            traceback.print_stack(file=sys.__stderr__)
            print("-----------------------", file=sys.__stderr__)
            raise
        return message

    def pollresponse(self, myseq, wait):
        """Handle incoming messages until the response for *myseq* arrives
        (return it) or nothing arrives in *wait* seconds (return None).

        Requests are executed (or queued), responses for other threads
        are handed to them, and queued results are sent back.
        """
        while True:
            try:
                qmsg = response_queue.get(0)
            except queue.Empty:
                pass
            else:
                seq, response = qmsg
                self.putmessage((seq, ('OK', response)))
            try:
                message = self.pollmessage(wait)
                if message is None:
                    return None
            except EOFError:
                self.handle_EOF()
                return None
            except AttributeError:
                return None
            seq, resq = message
            how = resq[0]
            self.debug("pollresponse:%d:myseq:%s" % (seq, myseq))
            if how in ("CALL", "QUEUE"):
                self.debug("pollresponse:%d:localcall:call:" % seq)
                response = self.localcall(seq, resq)
                self.debug("pollresponse:%d:localcall:response:%s" % (seq, response))
                if how == "CALL":
                    self.putmessage((seq, response))
                continue
            if seq == myseq:
                return resq
            cv = self.cvars.get(seq, None)
            # A response for an unknown sequence number (e.g. for a request
            # of a previous server process, or abandoned) is discarded.
            if cv is not None:
                with cv:
                    self.responses[seq] = resq
                    cv.notify()

    def handle_EOF(self):
        """Wake up the waiting threads when the connection closes."""
        self.EOFhook()
        self.debug("handle_EOF")
        for key in self.cvars:
            cv = self.cvars[key]
            with cv:
                self.responses[key] = ('EOF', None)
                cv.notify()
        self.EOFhook()

    def EOFhook(self):
        # Called when the connection is lost; subclasses add actions.
        pass


class RemoteObject:
    """Base class: instances are passed to the other side by reference."""


def remoteref(obj):
    oid = id(obj)
    objecttable[oid] = obj
    return RemoteProxy(oid)


class RemoteProxy:

    def __init__(self, oid):
        self.oid = oid


class RPCHandler(socketserver.BaseRequestHandler, SocketIO):

    debugging = False
    location = "#S"  # Server

    def __init__(self, sock, addr, svr):
        svr.current_handler = self
        SocketIO.__init__(self, sock)
        socketserver.BaseRequestHandler.__init__(self, sock, addr, svr)

    def handle(self):
        # Serve requests until the GUI closes the connection.
        self.mainloop()

    def get_remote_proxy(self, oid):
        return RPCProxy(self, oid)


class RPCClient(SocketIO):
    """The GUI side: listens for the user process to connect."""

    debugging = False
    location = "#C"  # Client

    nextseq = 1  # odd sequence numbers here, even ones in the server

    def __init__(self, address, family=socket.AF_INET, type=socket.SOCK_STREAM):
        self.listening_sock = socket.socket(family, type)
        self.listening_sock.bind(address)
        self.listening_sock.listen(1)

    def accept(self):
        working_sock, address = self.listening_sock.accept()
        if self.debugging:
            print("****** Connection request from ", address, file=sys.__stderr__)
        if address[0] == LOCALHOST:
            SocketIO.__init__(self, working_sock)
        else:
            print("** Invalid host: ", address, file=sys.__stderr__)
            raise OSError

    def get_remote_proxy(self, oid):
        return RPCProxy(self, oid)


class RPCProxy:
    """Stand-in for a remote object: method calls become remote calls."""

    __methods = None
    __attributes = None

    def __init__(self, sockio, oid):
        self.sockio = sockio
        self.oid = oid

    def __getattr__(self, name):
        if self.__methods is None:
            self.__getmethods()
        if self.__methods.get(name):
            return MethodProxy(self.sockio, self.oid, name)
        if self.__attributes is None:
            self.__getattributes()
        if name in self.__attributes:
            value = self.sockio.remotecall(self.oid, '__getattribute__',
                                           (name,), {})
            return value
        raise AttributeError(name)

    def __getattributes(self):
        self.__attributes = self.sockio.remotecall(self.oid, "__attributes__",
                                                   (), {})

    def __getmethods(self):
        self.__methods = self.sockio.remotecall(self.oid, "__methods__",
                                                (), {})


def _getmethods(obj, methods):
    # Collect the names of the callable attributes (also of base classes).
    for name in dir(obj):
        attr = getattr(obj, name)
        if callable(attr):
            methods[name] = 1
    if isinstance(obj, type):
        for base in obj.__bases__:
            _getmethods(base, methods)


def _getattributes(obj, attributes):
    for name in dir(obj):
        attr = getattr(obj, name)
        if not callable(attr):
            attributes[name] = 1


class MethodProxy:

    def __init__(self, sockio, oid, name):
        self.sockio = sockio
        self.oid = oid
        self.name = name

    def __call__(self, /, *args, **kwargs):
        value = self.sockio.remotecall(self.oid, self.name, args, kwargs)
        return value


def displayhook(value):
    """Print *value* like sys.displayhook, but as text the GUI can show
    (unencodable characters are escaped)."""
    if value is None:
        return
    builtins._ = None
    text = repr(value)
    try:
        sys.stdout.write(text)
    except UnicodeEncodeError:
        encoding = 'ascii'
        text = text.encode(encoding, 'backslashreplace').decode(encoding, 'strict')
        sys.stdout.write(text)
    sys.stdout.write("\n")
    builtins._ = value


if __name__ == '__main__':
    from unittest import main
    main('idlelib.idle_test.test_rpc', verbosity=2,)
