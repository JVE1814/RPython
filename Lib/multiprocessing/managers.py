#
# multiprocessing.managers -- objects that live in a server process and are
# used from other processes through proxies.
#
# Wire protocol (over connection.Connection / ConnectionWrapper):
#
#   request  (ident, method name, args, kwds)
#   reply    (kind, payload) where kind is one of
#              '#RETURN'          payload is the result
#              '#ERROR'           payload is the exception the call raised
#              '#TRACEBACK'       payload is a formatted traceback (str)
#              '#UNSERIALIZABLE'  the result could not be sent (str)
#              '#PROXY'           (exposed, token) of a new shared object
#
# A short-lived connection (ident None) asks the server itself to do
# something: create, incref, decref, ...  A proxy opens one long-lived
# connection per thread with 'accept_connection' and then sends method calls
# for shared objects over it.
#

__all__ = ['BaseManager', 'SyncManager', 'BaseProxy', 'Token']

import collections.abc
import os
import signal
import sys
import threading
import time
import types
import queue
from traceback import format_exc

from . import connection
from .context import reduction, get_spawning_popen, ProcessError
from . import pool
from . import process
from . import util
from . import get_context
try:
    from . import shared_memory
except ImportError:
    HAS_SHMEM = False
else:
    HAS_SHMEM = True
    __all__.append('SharedMemoryManager')


#
# How some values cross the process boundary
#

try:
    import array
except ImportError:
    array = None
else:
    def reduce_array(a):
        return array.array, (a.typecode, a.tobytes())
    reduction.register(array.array, reduce_array)

# dict views are sent as lists
view_types = [type(getattr({}, name)()) for name in ('items', 'keys', 'values')]


def rebuild_as_list(obj):
    return list, (list(obj),)


for _view_type in view_types:
    reduction.register(_view_type, rebuild_as_list)
del _view_type


#
# Token, replies, errors
#

class Token(object):
    """Names a shared object: its registered type, its server and its id
    there."""
    __slots__ = ('typeid', 'address', 'id')

    def __init__(self, typeid, address, id):
        self.typeid, self.address, self.id = typeid, address, id

    def __getstate__(self):
        return (self.typeid, self.address, self.id)

    def __setstate__(self, state):
        self.typeid, self.address, self.id = state

    def __repr__(self):
        return '%s(typeid=%r, address=%r, id=%r)' % (
            type(self).__name__, self.typeid, self.address, self.id)


class RemoteError(Exception):
    """An error reported by the server as text."""

    def __str__(self):
        rule = '-' * 75
        return '\n%s\n%s%s' % (rule, self.args[0], rule)


def convert_to_error(kind, result):
    """The exception a non-'#RETURN' reply stands for."""
    if kind == '#ERROR':
        return result
    if kind not in ('#TRACEBACK', '#UNSERIALIZABLE'):
        return ValueError('Unrecognized message type {!r}'.format(kind))
    if not isinstance(result, str):
        return TypeError(
            "Result {0!r} (kind '{1}') type is {2}, not str".format(
                result, kind, type(result)))
    if kind == '#UNSERIALIZABLE':
        return RemoteError('Unserializable message: %s\n' % result)
    return RemoteError(result)


def _raise_reply(kind, result):
    """Raise the error a reply carries.  The caller must not keep `result`
    in a local afterwards: the exception's traceback holds its frame."""
    error = convert_to_error(kind, result)
    try:
        raise error
    finally:
        error = result = None


def dispatch(c, id, methodname, args=(), kwds={}):
    """Send one request over c and return the result of the reply."""
    c.send((id, methodname, args, kwds))
    kind, result = c.recv()
    if kind == '#RETURN':
        return result
    try:
        _raise_reply(kind, result)
    finally:
        kind = result = None


def all_methods(obj):
    """Names of the callable attributes of obj."""
    return [name for name in dir(obj) if callable(getattr(obj, name, None))]


def public_methods(obj):
    """all_methods() without the names that start with an underscore."""
    return [name for name in all_methods(obj) if not name.startswith('_')]


#
# The server
#

class Server(object):
    """Holds the shared objects and answers requests about them."""

    # requests a short-lived connection may make
    public = ['shutdown', 'create', 'accept_connection', 'get_methods',
              'debug_info', 'number_of_objects', 'dummy', 'incref', 'decref']

    def __init__(self, registry, address, authkey, serializer):
        if not isinstance(authkey, bytes):
            raise TypeError(
                "Authkey {0!r} is type {1!s}, not bytes".format(
                    authkey, type(authkey)))
        self.registry = registry
        self.authkey = process.AuthenticationString(authkey)
        listener_type, _ = listener_client[serializer]
        self.listener = listener_type(address=address, backlog=128)
        self.address = self.listener.address

        # ident -> (obj, exposed names, method_to_typeid or None)
        self.id_to_obj = {'0': (None, ())}
        # ident -> number of proxies holding the object
        self.id_to_refcount = {}
        # objects that proxies living in this process refer to; they stay
        # reachable after every outside proxy is gone
        self.id_to_local_proxy_obj = {}
        self.mutex = threading.Lock()

    # -- running

    def serve_forever(self):
        """Serve until shutdown() is requested, then exit the process."""
        self.stop_event = threading.Event()
        process.current_process()._manager_server = self
        try:
            threading.Thread(target=self.accepter, daemon=True).start()
            try:
                while not self.stop_event.wait(1):
                    pass
            except (KeyboardInterrupt, SystemExit):
                pass
        finally:
            if sys.stdout != sys.__stdout__:
                util.debug('resetting stdout, stderr')
                sys.stdout, sys.stderr = sys.__stdout__, sys.__stderr__
            sys.exit(0)

    def accepter(self):
        """Accept connections, each served by its own thread."""
        while True:
            try:
                conn = self.listener.accept()
            except OSError:
                continue
            threading.Thread(target=self.handle_request, args=(conn,),
                             daemon=True).start()

    def handle_request(self, conn):
        """Authenticate a new connection and answer its first request."""
        try:
            self._answer_first_request(conn)
        except SystemExit:
            pass
        finally:
            conn.close()

    def _answer_first_request(self, conn):
        connection.deliver_challenge(conn, self.authkey)
        connection.answer_challenge(conn, self.authkey)
        try:
            _, name, args, kwds = conn.recv()
            if name not in self.public:
                raise AssertionError('%r unrecognized' % name)
            handler = getattr(self, name)
        except Exception:
            reply = ('#TRACEBACK', format_exc())
        else:
            try:
                reply = ('#RETURN', handler(conn, *args, **kwds))
            except Exception:
                reply = ('#TRACEBACK', format_exc())
        self._send_reply(conn, reply)

    def _send_reply(self, conn, reply):
        """Send reply; if it cannot be pickled, say so instead.  Returns
        False when not even that worked."""
        try:
            conn.send(reply)
            return True
        except Exception:
            failure = format_exc()
        try:
            conn.send(('#UNSERIALIZABLE', failure))
        except Exception:
            pass
        util.info('Failure to send message: %r', reply)
        util.info(' ... exception was %r', failure)
        return False

    def serve_client(self, conn):
        """Answer method calls on shared objects until the client goes."""
        util.debug('starting server thread to service %r',
                   threading.current_thread().name)
        while not self.stop_event.is_set():
            try:
                request = conn.recv()
            except (OSError, EOFError):
                util.debug('got EOF -- exiting thread serving %r',
                           threading.current_thread().name)
                sys.exit(0)
            except Exception:
                # e.g. an argument that cannot be unpickled here
                reply = ('#TRACEBACK', format_exc())
            else:
                reply = self._call_shared(conn, request)
                request = None
            if not self._send_reply(conn, reply):
                conn.close()
                sys.exit(1)

    def _lookup(self, ident):
        try:
            return self.id_to_obj[ident]
        except KeyError:
            return self.id_to_local_proxy_obj[ident]

    def _call_shared(self, conn, request):
        """The reply to one method call request."""
        name = None
        try:
            ident, name, args, kwds = request
            obj, exposed, method_to_typeid = self._lookup(ident)
            if name not in exposed:
                return self._fallback(conn, ident, obj, name, args, kwds)
            method = getattr(obj, name)
            try:
                result = method(*args, **kwds)
            except Exception as exc:
                return ('#ERROR', exc)
            typeid = method_to_typeid and method_to_typeid.get(name)
            if not typeid:
                return ('#RETURN', result)
            new_ident, new_exposed = self.create(conn, typeid, result)
            token = Token(typeid, self.address, new_ident)
            return ('#PROXY', (new_exposed, token))
        except Exception:
            return ('#TRACEBACK', format_exc())

    def _fallback(self, conn, ident, obj, name, args, kwds):
        """Answer a call of a name that is not exposed: a few names get a
        default answer, the others an error."""
        handler = self.fallback_mapping.get(name)
        if handler is None:
            message = 'method %r of %r object is not in exposed=%r' % (
                name, type(obj), self._lookup(ident)[1])
            try:
                raise AttributeError(message)
            except AttributeError:
                return ('#TRACEBACK', format_exc())
        try:
            return ('#RETURN', handler(self, conn, ident, obj, *args, **kwds))
        except Exception:
            return ('#TRACEBACK', format_exc())

    def fallback_getvalue(self, conn, ident, obj):
        return obj

    def fallback_str(self, conn, ident, obj):
        return str(obj)

    def fallback_repr(self, conn, ident, obj):
        return repr(obj)

    fallback_mapping = {
        '__str__': fallback_str,
        '__repr__': fallback_repr,
        '#GETVALUE': fallback_getvalue,
        }

    # -- requests of short-lived connections

    def dummy(self, c):
        pass

    def debug_info(self, c):
        """A description of the objects the server holds."""
        with self.mutex:
            lines = []
            for ident in sorted(self.id_to_refcount):
                if ident == '0':
                    continue
                lines.append('  %s:       refcount=%s\n    %s' % (
                    ident, self.id_to_refcount[ident],
                    str(self.id_to_obj[ident][0])[:75]))
            return '\n'.join(lines)

    def number_of_objects(self, c):
        """How many shared objects are referenced."""
        # '0' is a placeholder, not counted in id_to_refcount
        return len(self.id_to_refcount)

    def shutdown(self, c):
        """Answer, then stop the server."""
        try:
            util.debug('manager received shutdown message')
            c.send(('#RETURN', None))
        except Exception:
            import traceback
            traceback.print_exc()
        finally:
            self.stop_event.set()

    def create(self, c, typeid, /, *args, **kwds):
        """Make a new shared object of a registered type; returns its ident
        and exposed names (its reference count starts at one)."""
        with self.mutex:
            factory, exposed, method_to_typeid, _proxytype = \
                self.registry[typeid]
            if factory is not None:
                obj = factory(*args, **kwds)
            elif kwds or len(args) != 1:
                raise ValueError(
                    "Without callable, must have one non-keyword argument")
            else:
                obj = args[0]
            if exposed is None:
                exposed = public_methods(obj)
            if method_to_typeid is not None:
                if not isinstance(method_to_typeid, dict):
                    raise TypeError(
                        "Method_to_typeid {0!r}: type {1!s}, not dict".format(
                            method_to_typeid, type(method_to_typeid)))
                exposed = list(exposed) + list(method_to_typeid)
            ident = '%x' % id(obj)
            util.debug('%r callable returned object with id %r',
                       typeid, ident)
            self.id_to_obj[ident] = (obj, set(exposed), method_to_typeid)
            self.id_to_refcount.setdefault(ident, 0)
        self.incref(c, ident)
        return ident, tuple(exposed)

    def get_methods(self, c, token):
        """The exposed names of the object token refers to."""
        return tuple(self.id_to_obj[token.id][1])

    def accept_connection(self, c, name):
        """Turn c into a long-lived connection for method calls."""
        threading.current_thread().name = name
        c.send(('#RETURN', None))
        self.serve_client(c)

    def incref(self, c, ident):
        with self.mutex:
            if ident in self.id_to_refcount:
                self.id_to_refcount[ident] += 1
            elif ident in self.id_to_local_proxy_obj:
                # an object only proxies in this process referred to is
                # referenced from outside again
                self.id_to_refcount[ident] = 1
                self.id_to_obj[ident] = self.id_to_local_proxy_obj[ident]
                util.debug('Server re-enabled tracking & INCREF %r', ident)
            else:
                raise KeyError(ident)

    def decref(self, c, ident):
        if (ident not in self.id_to_refcount and
                ident in self.id_to_local_proxy_obj):
            util.debug('Server DECREF skipping %r', ident)
            return
        with self.mutex:
            count = self.id_to_refcount[ident]
            if count <= 0:
                raise AssertionError(
                    "Id {0!s} ({1!r}) has refcount {2:n}, not 1+".format(
                        ident, self.id_to_obj[ident], count))
            if count > 1:
                self.id_to_refcount[ident] = count - 1
                return
            del self.id_to_refcount[ident]
            # keep the object until the lock is released: its finalizer may
            # run code that needs the server
            entry = self.id_to_obj.pop(ident, None)
        util.debug('disposing of obj with id %r', ident)
        entry = None


#
# Managers
#

class State(object):
    """A manager's life cycle state (shared with its finalizer)."""
    __slots__ = ['value']
    INITIAL = 0
    STARTED = 1
    SHUTDOWN = 2


# serializer name -> (Listener, Client)
listener_client = {
    'pickle': (connection.Listener, connection.Client),
    'xmlrpclib': (connection.XmlListener, connection.XmlClient),
    }


def _short_request(client, address, authkey, name, args=(), kwds={}):
    """Make one request over a connection opened for it."""
    conn = client(address, authkey=authkey)
    try:
        return dispatch(conn, None, name, args, kwds)
    finally:
        conn.close()


class BaseManager(object):
    """Starts (or connects to) a server process holding shared objects; the
    registered types become methods returning proxies."""

    _registry = {}
    _Server = Server

    def __init__(self, address=None, authkey=None, serializer='pickle',
                 ctx=None, *, shutdown_timeout=1.0):
        if authkey is None:
            authkey = process.current_process().authkey
        self._address = address
        self._authkey = process.AuthenticationString(authkey)
        self._state = State()
        self._state.value = State.INITIAL
        self._serializer = serializer
        self._Listener, self._Client = listener_client[serializer]
        self._ctx = ctx or get_context()
        self._shutdown_timeout = shutdown_timeout

    def _require_initial(self):
        state = self._state.value
        if state == State.INITIAL:
            return
        if state == State.STARTED:
            raise ProcessError("Already started server")
        if state == State.SHUTDOWN:
            raise ProcessError("Manager has shut down")
        raise ProcessError("Unknown state {!r}".format(state))

    def get_server(self):
        """A Server for this manager, to run in the current process."""
        self._require_initial()
        return self._Server(self._registry, self._address, self._authkey,
                            self._serializer)

    def connect(self):
        """Use a server that is already running at self.address."""
        _short_request(self._Client, self._address, self._authkey, 'dummy')
        self._state.value = State.STARTED

    def start(self, initializer=None, initargs=()):
        """Start the server in a child process."""
        self._require_initial()
        if initializer is not None and not callable(initializer):
            raise TypeError('initializer must be a callable')

        # the child reports the address it listens on through this pipe
        reader, writer = connection.Pipe(duplex=False)
        self._process = self._ctx.Process(
            target=type(self)._run_server,
            args=(self._registry, self._address, self._authkey,
                  self._serializer, writer, initializer, initargs))
        ident = ':'.join(str(part) for part in self._process._identity)
        self._process.name = type(self).__name__ + '-' + ident
        self._process.start()
        writer.close()
        self._address = reader.recv()
        reader.close()

        self._state.value = State.STARTED
        self.shutdown = util.Finalize(
            self, type(self)._finalize_manager,
            args=(self._process, self._address, self._authkey, self._state,
                  self._Client, self._shutdown_timeout),
            exitpriority=0)

    @classmethod
    def _run_server(cls, registry, address, authkey, serializer, writer,
                    initializer=None, initargs=()):
        """Body of the server process."""
        # Ctrl-C in the terminal reaches the whole process group: the parent
        # decides when the server stops
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        if initializer is not None:
            initializer(*initargs)
        server = cls._Server(registry, address, authkey, serializer)
        writer.send(server.address)
        writer.close()
        util.info('manager serving at %r', server.address)
        server.serve_forever()

    def _create(self, typeid, /, *args, **kwds):
        """Create a shared object; returns its token and exposed names."""
        assert self._state.value == State.STARTED, 'server not yet started'
        ident, exposed = _short_request(self._Client, self._address,
                                        self._authkey, 'create',
                                        (typeid,) + args, kwds)
        return Token(typeid, self._address, ident), exposed

    def join(self, timeout=None):
        """Wait for the server process to end."""
        if getattr(self, '_process', None) is not None:
            self._process.join(timeout)
            if not self._process.is_alive():
                self._process = None

    def _debug_info(self):
        return _short_request(self._Client, self._address, self._authkey,
                              'debug_info')

    def _number_of_objects(self):
        return _short_request(self._Client, self._address, self._authkey,
                              'number_of_objects')

    def __enter__(self):
        if self._state.value == State.INITIAL:
            self.start()
        if self._state.value != State.STARTED:
            raise ProcessError("Unable to start server")
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.shutdown()

    @staticmethod
    def _finalize_manager(process, address, authkey, state, _Client,
                          shutdown_timeout):
        """Stop the server process: ask, then terminate, then kill."""
        if process.is_alive():
            util.info('sending shutdown message to manager')
            try:
                _short_request(_Client, address, authkey, 'shutdown')
            except Exception:
                pass
            process.join(timeout=shutdown_timeout)
            if process.is_alive():
                util.info('manager still alive')
                if hasattr(process, 'terminate'):
                    util.info('trying to `terminate()` manager process')
                    process.terminate()
                    process.join(timeout=shutdown_timeout)
                    if process.is_alive():
                        util.info('manager still alive after terminate')
                        process.kill()
                        process.join()
        state.value = State.SHUTDOWN
        BaseProxy._address_to_local.pop(address, None)

    @property
    def address(self):
        return self._address

    @classmethod
    def register(cls, typeid, callable=None, proxytype=None, exposed=None,
                 method_to_typeid=None, create_method=True):
        """Register a type (or factory) under typeid; unless create_method is
        false, the manager gets a method of that name returning a proxy."""
        if '_registry' not in cls.__dict__:
            # each class gets its own copy of the inherited registry
            cls._registry = cls._registry.copy()
        if proxytype is None:
            proxytype = AutoProxy
        exposed = exposed or getattr(proxytype, '_exposed_', None)
        method_to_typeid = (method_to_typeid or
                            getattr(proxytype, '_method_to_typeid_', None))
        if method_to_typeid:
            for key, value in method_to_typeid.items():
                assert type(key) is str, '%r is not a string' % key
                assert type(value) is str, '%r is not a string' % value
        cls._registry[typeid] = (callable, exposed, method_to_typeid, proxytype)
        if create_method:
            setattr(cls, typeid, _creation_method(typeid, proxytype))


def _creation_method(typeid, proxytype):
    """The manager method creating a shared typeid object."""
    def create(self, /, *args, **kwds):
        util.debug('requesting creation of a shared %r object', typeid)
        token, exposed = self._create(typeid, *args, **kwds)
        proxy = proxytype(token, self._serializer, manager=self,
                          authkey=self._authkey, exposed=exposed)
        # the proxy holds its own reference now: drop the one create() made
        _short_request(self._Client, token.address, self._authkey,
                       'decref', (token.id,))
        return proxy
    create.__name__ = typeid
    return create


#
# Proxies
#

class ProcessLocalSet(set):
    """A set that is emptied in a forked child and pickles as an empty set."""

    def __init__(self):
        util.register_after_fork(self, lambda obj: obj.clear())

    def __reduce__(self):
        return type(self), ()


class BaseProxy(object):
    """Stands for a shared object held by a manager's server."""

    # server address -> (thread-local connection holder, set of the idents
    # this process holds references to)
    _address_to_local = {}
    _mutex = util.ForkAwareThreadLock()

    def __init__(self, token, serializer, manager=None,
                 authkey=None, exposed=None, incref=True, manager_owned=False):
        with BaseProxy._mutex:
            local = BaseProxy._address_to_local.get(token.address)
            if local is None:
                local = (util.ForkAwareLocal(), ProcessLocalSet())
                BaseProxy._address_to_local[token.address] = local
        self._tls, self._idset = local
        self._token = token
        self._id = token.id
        self._manager = manager
        self._serializer = serializer
        self._Client = listener_client[serializer][1]
        # a proxy rebuilt inside the server process itself takes no reference
        self._owned_by_manager = manager_owned

        if authkey is not None:
            self._authkey = process.AuthenticationString(authkey)
        elif manager is not None:
            self._authkey = manager._authkey
        else:
            self._authkey = process.current_process().authkey

        if incref:
            self._incref()
        util.register_after_fork(self, BaseProxy._after_fork)

    def _connect(self):
        """Open this thread's long-lived connection to the server."""
        util.debug('making connection to manager')
        name = process.current_process().name
        thread_name = threading.current_thread().name
        if thread_name != 'MainThread':
            name += '|' + thread_name
        conn = self._Client(self._token.address, authkey=self._authkey)
        dispatch(conn, None, 'accept_connection', (name,))
        self._tls.connection = conn

    def _callmethod(self, methodname, args=(), kwds={}):
        """Call methodname on the shared object and return the result."""
        conn = getattr(self._tls, 'connection', None)
        if conn is None:
            util.debug('thread %r does not own a connection',
                       threading.current_thread().name)
            self._connect()
            conn = self._tls.connection
        conn.send((self._id, methodname, args, kwds))
        kind, result = conn.recv()
        if kind == '#RETURN':
            return result
        if kind == '#PROXY':
            return self._adopt(*result)
        try:
            _raise_reply(kind, result)
        finally:
            kind = result = None

    def _adopt(self, exposed, token):
        """A proxy for an object the server created for a method result."""
        proxytype = self._manager._registry[token.typeid][-1]
        token.address = self._token.address
        proxy = proxytype(token, self._serializer, manager=self._manager,
                          authkey=self._authkey, exposed=exposed)
        _short_request(self._Client, token.address, self._authkey,
                       'decref', (token.id,))
        return proxy

    def _getvalue(self):
        """A copy of the shared object."""
        return self._callmethod('#GETVALUE')

    def _incref(self):
        if self._owned_by_manager:
            util.debug('owned_by_manager skipped INCREF of %r', self._token.id)
            return
        _short_request(self._Client, self._token.address, self._authkey,
                       'incref', (self._id,))
        util.debug('INCREF %r', self._token.id)
        self._idset.add(self._id)
        state = self._manager and self._manager._state
        self._close = util.Finalize(
            self, BaseProxy._decref,
            args=(self._token, self._authkey, state,
                  self._tls, self._idset, self._Client),
            exitpriority=10)

    @staticmethod
    def _decref(token, authkey, state, tls, idset, _Client):
        idset.discard(token.id)
        # nothing to tell a server that has been shut down
        if state is None or state.value == State.STARTED:
            try:
                util.debug('DECREF %r', token.id)
                _short_request(_Client, token.address, authkey,
                               'decref', (token.id,))
            except Exception as e:
                util.debug('... decref failed %s', e)
        else:
            util.debug('DECREF %r -- manager already shutdown', token.id)
        # the last proxy of this thread is gone: close its connection
        if not idset and hasattr(tls, 'connection'):
            util.debug('thread %r has no more proxies so closing conn',
                       threading.current_thread().name)
            tls.connection.close()
            del tls.connection

    def _after_fork(self):
        # the child holds references of its own
        self._manager = None
        try:
            self._incref()
        except Exception as e:
            util.info('incref failed: %s' % e)

    def __reduce__(self):
        kwds = {}
        if get_spawning_popen() is not None:
            kwds['authkey'] = self._authkey
        if getattr(self, '_isauto', False):
            kwds['exposed'] = self._exposed_
            return (RebuildProxy, (AutoProxy, self._token, self._serializer,
                                   kwds))
        return (RebuildProxy, (type(self), self._token, self._serializer, kwds))

    def __deepcopy__(self, memo):
        return self._getvalue()

    def __repr__(self):
        return '<%s object, typeid %r at %#x>' % (
            type(self).__name__, self._token.typeid, id(self))

    def __str__(self):
        """The repr() of the shared object."""
        try:
            return self._callmethod('__repr__')
        except Exception:
            return repr(self)[:-1] + "; '__str__()' failed>"


def RebuildProxy(func, token, serializer, kwds):
    """Unpickle a proxy (func is its type or AutoProxy)."""
    server = getattr(process.current_process(), '_manager_server', None)
    if server is not None and server.address == token.address:
        # unpickled inside the server that holds the object: the server
        # keeps the object for this proxy, which takes no reference
        util.debug('Rebuild a proxy owned by manager, token=%r', token)
        kwds['manager_owned'] = True
        if token.id not in server.id_to_local_proxy_obj:
            server.id_to_local_proxy_obj[token.id] = server.id_to_obj[token.id]
    # a child being spawned unpickles the proxies its parent passed; the
    # parent's references cover them
    incref = (kwds.pop('incref', True) and
              not getattr(process.current_process(), '_inheriting', False))
    return func(token, serializer, incref=incref, **kwds)


def _forwarder(name):
    def forward(self, /, *args, **kwds):
        return self._callmethod(name, args, kwds)
    forward.__name__ = name
    return forward


def MakeProxyType(name, exposed, _cache={}):
    """A BaseProxy subclass with one forwarding method per exposed name."""
    exposed = tuple(exposed)
    key = (name, exposed)
    proxytype = _cache.get(key)
    if proxytype is None:
        namespace = {meth: _forwarder(meth) for meth in exposed}
        namespace['_exposed_'] = exposed
        proxytype = _cache[key] = type(name, (BaseProxy,), namespace)
    return proxytype


def AutoProxy(token, serializer, manager=None, authkey=None,
              exposed=None, incref=True, manager_owned=False):
    """A proxy whose type is made from the shared object's exposed names."""
    _Client = listener_client[serializer][1]
    if exposed is None:
        exposed = _short_request(_Client, token.address, authkey,
                                 'get_methods', (token,))
    if authkey is None and manager is not None:
        authkey = manager._authkey
    if authkey is None:
        authkey = process.current_process().authkey
    proxytype = MakeProxyType('AutoProxy[%s]' % token.typeid, exposed)
    proxy = proxytype(token, serializer, manager=manager, authkey=authkey,
                      incref=incref, manager_owned=manager_owned)
    proxy._isauto = True
    return proxy


#
# Types for SyncManager
#

class Namespace(object):
    """A bag of attributes."""

    def __init__(self, /, **kwds):
        self.__dict__.update(kwds)

    def __repr__(self):
        shown = sorted('%s=%r' % (name, value)
                       for name, value in self.__dict__.items()
                       if not name.startswith('_'))
        return '%s(%s)' % (type(self).__name__, ', '.join(shown))


class Value(object):
    """A box for one value (the typecode is informative only)."""

    def __init__(self, typecode, value, lock=True):
        self._typecode = typecode
        self._value = value

    def get(self):
        return self._value

    def set(self, value):
        self._value = value

    def __repr__(self):
        return '%s(%r, %r)' % (type(self).__name__, self._typecode, self._value)

    value = property(get, set)


def Array(typecode, sequence, lock=True):
    return array.array(typecode, sequence)


#
# Proxy types
#

class IteratorProxy(BaseProxy):
    _exposed_ = ('__next__', 'send', 'throw', 'close')

    def __iter__(self):
        return self

    def __next__(self, *args):
        return self._callmethod('__next__', args)

    def send(self, *args):
        return self._callmethod('send', args)

    def throw(self, *args):
        return self._callmethod('throw', args)

    def close(self, *args):
        return self._callmethod('close', args)


class AcquirerProxy(BaseProxy):
    _exposed_ = ('acquire', 'release', 'locked')

    def acquire(self, blocking=True, timeout=None):
        args = (blocking,) if timeout is None else (blocking, timeout)
        return self._callmethod('acquire', args)

    def release(self):
        return self._callmethod('release')

    def locked(self):
        return self._callmethod('locked')

    def __enter__(self):
        return self._callmethod('acquire')

    def __exit__(self, exc_type, exc_val, exc_tb):
        return self._callmethod('release')


class ConditionProxy(AcquirerProxy):
    _exposed_ = ('acquire', 'release', 'locked', 'wait', 'notify',
                 'notify_all')

    def wait(self, timeout=None):
        return self._callmethod('wait', (timeout,))

    def notify(self, n=1):
        return self._callmethod('notify', (n,))

    def notify_all(self):
        return self._callmethod('notify_all')

    def wait_for(self, predicate, timeout=None):
        # evaluated here: the predicate cannot be sent to the server
        result = predicate()
        if result:
            return result
        deadline = None if timeout is None else time.monotonic() + timeout
        while not result:
            remaining = None
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
            self.wait(remaining)
            result = predicate()
        return result


class EventProxy(BaseProxy):
    _exposed_ = ('is_set', 'set', 'clear', 'wait')

    def is_set(self):
        return self._callmethod('is_set')

    def set(self):
        return self._callmethod('set')

    def clear(self):
        return self._callmethod('clear')

    def wait(self, timeout=None):
        return self._callmethod('wait', (timeout,))


class BarrierProxy(BaseProxy):
    _exposed_ = ('__getattribute__', 'wait', 'abort', 'reset')

    def wait(self, timeout=None):
        return self._callmethod('wait', (timeout,))

    def abort(self):
        return self._callmethod('abort')

    def reset(self):
        return self._callmethod('reset')

    @property
    def parties(self):
        return self._callmethod('__getattribute__', ('parties',))

    @property
    def n_waiting(self):
        return self._callmethod('__getattribute__', ('n_waiting',))

    @property
    def broken(self):
        return self._callmethod('__getattribute__', ('broken',))


class NamespaceProxy(BaseProxy):
    _exposed_ = ('__getattribute__', '__setattr__', '__delattr__')

    # names starting with an underscore are the proxy's own attributes

    def __getattr__(self, key):
        if key[0] == '_':
            return object.__getattribute__(self, key)
        call = object.__getattribute__(self, '_callmethod')
        return call('__getattribute__', (key,))

    def __setattr__(self, key, value):
        if key[0] == '_':
            return object.__setattr__(self, key, value)
        call = object.__getattribute__(self, '_callmethod')
        return call('__setattr__', (key, value))

    def __delattr__(self, key):
        if key[0] == '_':
            return object.__delattr__(self, key)
        call = object.__getattribute__(self, '_callmethod')
        return call('__delattr__', (key,))


class ValueProxy(BaseProxy):
    _exposed_ = ('get', 'set')

    def get(self):
        return self._callmethod('get')

    def set(self, value):
        return self._callmethod('set', (value,))

    value = property(get, set)

    __class_getitem__ = classmethod(types.GenericAlias)


def _in_place(name):
    """An augmented assignment forwarded to the server; the proxy stays the
    result."""
    def method(self, value):
        self._callmethod(name, (value,))
        return self
    method.__name__ = name
    return method


BaseListProxy = MakeProxyType('BaseListProxy', (
    '__add__', '__contains__', '__delitem__', '__getitem__', '__imul__',
    '__len__', '__mul__', '__reversed__', '__rmul__', '__setitem__',
    'append', 'clear', 'copy', 'count', 'extend', 'index', 'insert', 'pop',
    'remove', 'reverse', 'sort',
    ))


class ListProxy(BaseListProxy):
    def __iadd__(self, value):
        self._callmethod('extend', (value,))
        return self

    __imul__ = _in_place('__imul__')

    __class_getitem__ = classmethod(types.GenericAlias)


collections.abc.MutableSequence.register(BaseListProxy)

_BaseDictProxy = MakeProxyType('_BaseDictProxy', (
    '__contains__', '__delitem__', '__getitem__', '__ior__', '__iter__',
    '__len__', '__or__', '__ror__', '__setitem__', 'clear', 'copy',
    'fromkeys', 'get', 'items', 'keys', 'pop', 'popitem', 'setdefault',
    'update', 'values',
    ))
_BaseDictProxy._method_to_typeid_ = {'__iter__': 'Iterator'}


class DictProxy(_BaseDictProxy):
    __ior__ = _in_place('__ior__')

    def __reversed__(self):
        return iter(self._callmethod('keys')[::-1])

    __class_getitem__ = classmethod(types.GenericAlias)


collections.abc.MutableMapping.register(_BaseDictProxy)

_BaseSetProxy = MakeProxyType('_BaseSetProxy', (
    '__and__', '__contains__', '__iand__', '__ior__', '__isub__',
    '__ixor__', '__len__', '__or__', '__rand__', '__ror__', '__rsub__',
    '__rxor__', '__sub__', '__xor__', '__ge__', '__gt__', '__le__', '__lt__',
    'add', 'clear', 'copy', 'difference', 'difference_update', 'discard',
    'intersection', 'intersection_update', 'isdisjoint', 'issubset',
    'issuperset', 'pop', 'remove', 'symmetric_difference',
    'symmetric_difference_update', 'union', 'update',
    ))


class SetProxy(_BaseSetProxy):
    def __iter__(self):
        # iterate over a snapshot
        return iter(self._callmethod('copy'))

    __ior__ = _in_place('__ior__')
    __iand__ = _in_place('__iand__')
    __ixor__ = _in_place('__ixor__')
    __isub__ = _in_place('__isub__')

    __class_getitem__ = classmethod(types.GenericAlias)


collections.abc.MutableSet.register(_BaseSetProxy)

ArrayProxy = MakeProxyType('ArrayProxy', (
    '__len__', '__getitem__', '__setitem__'
    ))

BasePoolProxy = MakeProxyType('PoolProxy', (
    'apply', 'apply_async', 'close', 'imap', 'imap_unordered', 'join',
    'map', 'map_async', 'starmap', 'starmap_async', 'terminate',
    ))
BasePoolProxy._method_to_typeid_ = {
    'apply_async': 'AsyncResult',
    'map_async': 'AsyncResult',
    'starmap_async': 'AsyncResult',
    'imap': 'Iterator',
    'imap_unordered': 'Iterator',
    }


class PoolProxy(BasePoolProxy):
    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.terminate()


#
# SyncManager
#

class SyncManager(BaseManager):
    """Manager with the common shared types registered (Queue, Lock,
    Event, list, dict, set, Namespace, Value, ...)."""


for _typeid, _factory, _proxytype in (
        ('Queue', queue.Queue, None),
        ('JoinableQueue', queue.Queue, None),
        ('Event', threading.Event, EventProxy),
        ('Lock', threading.Lock, AcquirerProxy),
        ('RLock', threading.RLock, AcquirerProxy),
        ('Semaphore', threading.Semaphore, AcquirerProxy),
        ('BoundedSemaphore', threading.BoundedSemaphore, AcquirerProxy),
        ('Condition', threading.Condition, ConditionProxy),
        ('Barrier', threading.Barrier, BarrierProxy),
        ('Pool', pool.Pool, PoolProxy),
        ('list', list, ListProxy),
        ('dict', dict, DictProxy),
        ('set', set, SetProxy),
        ('Value', Value, ValueProxy),
        ('Array', Array, ArrayProxy),
        ('Namespace', Namespace, NamespaceProxy)):
    SyncManager.register(_typeid, _factory, _proxytype)
del _typeid, _factory, _proxytype

# types only created as method results
SyncManager.register('Iterator', proxytype=IteratorProxy, create_method=False)
SyncManager.register('AsyncResult', create_method=False)


#
# SharedMemoryManager
#

if HAS_SHMEM:
    class _SharedMemoryTracker:
        """The shared memory segments a server must unlink at shutdown."""

        def __init__(self, name, segment_names=[]):
            self.shared_memory_context_name = name
            self.segment_names = segment_names

        def register_segment(self, segment_name):
            util.debug(f"Register segment {segment_name!r} in pid {os.getpid()}")
            self.segment_names.append(segment_name)

        def destroy_segment(self, segment_name):
            util.debug(f"Destroy segment {segment_name!r} in pid {os.getpid()}")
            self.segment_names.remove(segment_name)
            segment = shared_memory.SharedMemory(segment_name)
            segment.close()
            segment.unlink()

        def unlink(self):
            for segment_name in list(self.segment_names):
                self.destroy_segment(segment_name)

        def __del__(self):
            util.debug(f"Call {type(self).__name__}.__del__ in {os.getpid()}")
            self.unlink()

        def __getstate__(self):
            return (self.shared_memory_context_name, self.segment_names)

        def __setstate__(self, state):
            self.__init__(*state)

    class SharedMemoryServer(Server):
        """A Server that also tracks shared memory segments."""

        public = Server.public + [
            'track_segment', 'release_segment', 'list_segments']

        def __init__(self, *args, **kwargs):
            Server.__init__(self, *args, **kwargs)
            address = self.address
            if isinstance(address, bytes):
                address = os.fsdecode(address)
            self.shared_memory_context = _SharedMemoryTracker(
                f"shm_{address}_{os.getpid()}")
            util.debug(f"SharedMemoryServer started by pid {os.getpid()}")

        def create(self, c, typeid, /, *args, **kwargs):
            # types whose proxy asks for it get the segment tracker
            if hasattr(self.registry[typeid][-1], "_shared_memory_proxy"):
                kwargs['shared_memory_context'] = self.shared_memory_context
            return Server.create(self, c, typeid, *args, **kwargs)

        def shutdown(self, c):
            """Unlink every tracked segment, then stop."""
            self.shared_memory_context.unlink()
            return Server.shutdown(self, c)

        def track_segment(self, c, segment_name):
            self.shared_memory_context.register_segment(segment_name)

        def release_segment(self, c, segment_name):
            self.shared_memory_context.destroy_segment(segment_name)

        def list_segments(self, c):
            return self.shared_memory_context.segment_names

    class SharedMemoryManager(BaseManager):
        """A manager whose server unlinks the shared memory blocks created
        through it when it shuts down."""

        _Server = SharedMemoryServer

        def __init__(self, *args, **kwargs):
            if os.name == "posix":
                # the server must use this process's resource tracker, or
                # the parent's tracker would report the segments as leaked
                from . import resource_tracker
                resource_tracker.ensure_running()
            BaseManager.__init__(self, *args, **kwargs)
            util.debug(f"{type(self).__name__} created by pid {os.getpid()}")

        def __del__(self):
            util.debug(f"{type(self).__name__}.__del__ by pid {os.getpid()}")

        def get_server(self):
            self._require_initial()
            return self._Server(self._registry, self._address,
                                self._authkey, self._serializer)

        def _tracked(self, make, segment_name, discard):
            with self._Client(self._address, authkey=self._authkey) as conn:
                made = make()
                try:
                    dispatch(conn, None, 'track_segment', (segment_name(made),))
                except BaseException:
                    discard(made)
                    raise
            return made

        def SharedMemory(self, size):
            """A new SharedMemory block of size bytes, tracked by the
            server."""
            return self._tracked(
                lambda: shared_memory.SharedMemory(None, create=True,
                                                   size=size),
                lambda shm: shm.name, lambda shm: shm.unlink())

        def ShareableList(self, sequence):
            """A new ShareableList holding sequence, tracked by the
            server."""
            return self._tracked(
                lambda: shared_memory.ShareableList(sequence),
                lambda sl: sl.shm.name, lambda sl: sl.shm.unlink())
