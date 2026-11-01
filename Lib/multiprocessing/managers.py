#
# multiprocessing.managers: a server process owning shared objects, and
# proxies that call their methods remotely.
#
# Every request is a pickled (ident, methodname, args, kwds) tuple sent over
# an authenticated connection; replies are (kind, value) where kind is
# '#RETURN', '#ERROR' (an exception to re-raise), '#TRACEBACK' (formatted
# server-side failure), '#PROXY' (the result is itself a managed object) or
# '#UNSERIALIZABLE'.
#

__all__ = ['BaseManager', 'SyncManager', 'BaseProxy', 'Token',
           'SharedMemoryManager']

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

# ---------------------------------------------------------------- pickling helpers

try:
    import array
except ImportError:
    array = None
else:
    def reduce_array(a):
        return array.array, (a.typecode, a.tobytes())
    reduction.register(array.array, reduce_array)

# dict views travel as lists
view_types = [type(getattr({}, name)()) for name in ('items', 'keys', 'values')]


def rebuild_as_list(obj):
    return list, (list(obj),)


for view_type in view_types:
    reduction.register(view_type, rebuild_as_list)
del view_type


class Token(object):
    """Names a shared object: its type id, its server's address and id."""
    __slots__ = ('typeid', 'address', 'id')

    def __init__(self, typeid, address, id):
        (self.typeid, self.address, self.id) = (typeid, address, id)

    def __getstate__(self):
        return (self.typeid, self.address, self.id)

    def __setstate__(self, state):
        (self.typeid, self.address, self.id) = state

    def __repr__(self):
        return '%s(typeid=%r, address=%r, id=%s)' % \
               (self.__class__.__name__, self.typeid, self.address, self.id)


def dispatch(c, id, methodname, args=(), kwds={}):
    """Call a server function over the connection *c*."""
    c.send((id, methodname, args, kwds))
    kind, result = c.recv()
    if kind == '#RETURN':
        return result
    raise convert_to_error(kind, result)


def convert_to_error(kind, result):
    if kind == '#ERROR':
        return result
    elif kind in ('#TRACEBACK', '#UNSERIALIZABLE'):
        if not isinstance(result, str):
            raise TypeError(
                "Result {0!r} (kind '{1}') type is {2}, not str".format(
                    result, kind, type(result)))
        if kind == '#UNSERIALIZABLE':
            return RemoteError('Unserializable message: %s\n' % result)
        else:
            return RemoteError(result)
    else:
        return ValueError('Unrecognized message type {!r}'.format(kind))


class RemoteError(Exception):
    """A failure in the manager process (carries its formatted traceback)."""

    def __str__(self):
        line = '-' * 75
        return '\n' + line + '\n' + str(self.args[0]) + line


def all_methods(obj):
    """Names of the callable attributes of *obj*."""
    return [name for name in dir(obj) if callable(getattr(obj, name))]


def public_methods(obj):
    """Names of the public callable attributes of *obj*."""
    return [name for name in all_methods(obj) if name[0] != '_']


# ---------------------------------------------------------------- server

class Server(object):
    """Runs in the manager process: owns the objects, serves requests."""

    public = ['shutdown', 'create', 'accept_connection', 'get_methods',
              'debug_info', 'number_of_objects', 'dummy', 'incref', 'decref']

    def __init__(self, registry, address, authkey, serializer):
        if not isinstance(authkey, bytes):
            raise TypeError(
                "Authkey {0!r} is type {1!s}, not bytes".format(
                    authkey, type(authkey)))
        self.registry = registry
        self.authkey = process.AuthenticationString(authkey)
        Listener, Client = listener_client[serializer]

        self.listener = Listener(address=address, backlog=128)
        self.address = self.listener.address

        self.id_to_obj = {'0': (None, ())}
        self.id_to_refcount = {}
        # objects referenced by proxies living inside the manager itself
        self.id_to_local_proxy_obj = {}
        self.mutex = threading.Lock()

    def serve_forever(self):
        """Serve until shutdown() is requested."""
        self.stop_event = threading.Event()
        process.current_process()._manager_server = self
        try:
            accepter = threading.Thread(target=self.accepter)
            accepter.daemon = True
            accepter.start()
            try:
                while not self.stop_event.is_set():
                    self.stop_event.wait(1)
            except (KeyboardInterrupt, SystemExit):
                pass
        finally:
            if sys.stdout != sys.__stdout__:
                util.debug('resetting stdout, stderr')
                sys.stdout = sys.__stdout__
                sys.stderr = sys.__stderr__
            sys.exit(0)

    def accepter(self):
        while True:
            try:
                c = self.listener.accept()
            except OSError:
                continue
            t = threading.Thread(target=self.handle_request, args=(c,))
            t.daemon = True
            t.start()

    def _handle_request(self, c):
        request = None
        try:
            connection.deliver_challenge(c, self.authkey)
            connection.answer_challenge(c, self.authkey)
            request = c.recv()
            ignore, funcname, args, kwds = request
            assert funcname in self.public, '%r unrecognized' % funcname
            func = getattr(self, funcname)
        except Exception:
            msg = ('#TRACEBACK', format_exc())
        else:
            try:
                result = func(c, *args, **kwds)
            except Exception:
                msg = ('#TRACEBACK', format_exc())
            else:
                msg = ('#RETURN', result)

        try:
            c.send(msg)
        except Exception as e:
            try:
                c.send(('#TRACEBACK', format_exc()))
            except Exception:
                pass
            util.info('Failure to send message: %r', msg)
            util.info(' ... request was %r', request)
            util.info(' ... exception was %r', e)

    def handle_request(self, conn):
        """Handle one new connection."""
        try:
            self._handle_request(conn)
        except SystemExit:
            # accept_connection() ends the thread with sys.exit()
            pass
        finally:
            conn.close()

    def serve_client(self, conn):
        """Answer method calls from one proxy connection until it closes."""
        util.debug('starting server thread to service %r',
                   threading.current_thread().name)

        recv = conn.recv
        send = conn.send
        id_to_obj = self.id_to_obj

        while not self.stop_event.is_set():
            try:
                methodname = obj = None
                request = recv()
                ident, methodname, args, kwds = request
                try:
                    obj, exposed, gettypeid = id_to_obj[ident]
                except KeyError as ke:
                    try:
                        obj, exposed, gettypeid = \
                            self.id_to_local_proxy_obj[ident]
                    except KeyError:
                        raise ke

                if methodname not in exposed:
                    raise AttributeError(
                        'method %r of %r object is not in exposed=%r' %
                        (methodname, type(obj), exposed)
                        )

                function = getattr(obj, methodname)

                try:
                    res = function(*args, **kwds)
                except Exception as e:
                    msg = ('#ERROR', e)
                else:
                    typeid = gettypeid and gettypeid.get(methodname, None)
                    if typeid:
                        rident, rexposed = self.create(conn, typeid, res)
                        token = Token(typeid, self.address, rident)
                        msg = ('#PROXY', (rexposed, token))
                    else:
                        msg = ('#RETURN', res)

            except AttributeError:
                if methodname is None:
                    msg = ('#TRACEBACK', format_exc())
                else:
                    try:
                        fallback_func = self.fallback_mapping[methodname]
                        result = fallback_func(
                            self, conn, ident, obj, *args, **kwds
                            )
                        msg = ('#RETURN', result)
                    except Exception:
                        msg = ('#TRACEBACK', format_exc())

            except EOFError:
                util.debug('got EOF -- exiting thread serving %r',
                           threading.current_thread().name)
                sys.exit(0)

            except Exception:
                msg = ('#TRACEBACK', format_exc())

            try:
                try:
                    send(msg)
                except Exception:
                    send(('#UNSERIALIZABLE', format_exc()))
            except Exception as e:
                util.info('exception in thread serving %r',
                          threading.current_thread().name)
                util.info(' ... message was %r', msg)
                util.info(' ... exception was %r', e)
                conn.close()
                sys.exit(1)

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

    def dummy(self, c):
        pass

    def debug_info(self, c):
        """Description of the shared objects (for debugging)."""
        with self.mutex:
            result = []
            keys = list(self.id_to_refcount.keys())
            keys.sort()
            for ident in keys:
                if ident != '0':
                    result.append('  %s:       refcount=%s\n    %s' %
                                  (ident, self.id_to_refcount[ident],
                                   str(self.id_to_obj[ident][0])[:75]))
            return '\n'.join(result)

    def number_of_objects(self, c):
        """Number of shared objects."""
        return len(self.id_to_refcount)

    def shutdown(self, c):
        """Stop the server."""
        try:
            util.debug('manager received shutdown message')
            c.send(('#RETURN', None))
        except:
            import traceback
            traceback.print_exc()
        finally:
            self.stop_event.set()

    def create(self, c, typeid, /, *args, **kwds):
        """Create a shared object; returns its id and exposed methods."""
        with self.mutex:
            callable, exposed, method_to_typeid, proxytype = \
                      self.registry[typeid]

            if callable is None:
                if kwds or (len(args) != 1):
                    raise ValueError(
                        "Without callable, must have one non-keyword argument")
                obj = args[0]
            else:
                obj = callable(*args, **kwds)

            if exposed is None:
                exposed = public_methods(obj)
            if method_to_typeid is not None:
                if not isinstance(method_to_typeid, dict):
                    raise TypeError(
                        "Method_to_typeid {0!r}: type {1!s}, not dict".format(
                            method_to_typeid, type(method_to_typeid)))
                exposed = list(exposed) + list(method_to_typeid)

            ident = '%x' % id(obj)
            util.debug('%r callable returned object with id %r', typeid, ident)

            self.id_to_obj[ident] = (obj, set(exposed), method_to_typeid)
            if ident not in self.id_to_refcount:
                self.id_to_refcount[ident] = 0

        self.incref(c, ident)
        return ident, tuple(exposed)

    def get_methods(self, c, token):
        """Methods exposed by the object named by *token*."""
        return tuple(self.id_to_obj[token.id][1])

    def accept_connection(self, c, name):
        """Turn the connection into a dedicated proxy connection."""
        threading.current_thread().name = name
        c.send(('#RETURN', None))
        self.serve_client(c)

    def incref(self, c, ident):
        with self.mutex:
            try:
                self.id_to_refcount[ident] += 1
            except KeyError as ke:
                # a proxy held by a managed object starts tracking again
                if ident in self.id_to_local_proxy_obj:
                    self.id_to_refcount[ident] = 1
                    self.id_to_obj[ident] = \
                        self.id_to_local_proxy_obj[ident]
                    util.debug('Server re-enabled tracking & INCREF %r', ident)
                else:
                    raise ke

    def decref(self, c, ident):
        if ident not in self.id_to_refcount and \
            ident in self.id_to_local_proxy_obj:
            util.debug('Server DECREF skipping %r', ident)
            return

        with self.mutex:
            if self.id_to_refcount[ident] <= 0:
                raise AssertionError(
                    "Id {0!s} ({1!r}) has refcount {2:n}, not 1+".format(
                        ident, self.id_to_obj[ident],
                        self.id_to_refcount[ident]))
            self.id_to_refcount[ident] -= 1
            if self.id_to_refcount[ident] == 0:
                del self.id_to_refcount[ident]

        if ident not in self.id_to_refcount:
            # Release the object outside the mutex: its destructor may
            # call back into the server (proxies held by the object).
            self.id_to_obj[ident] = (None, (), None)
            util.debug('disposing of obj with id %r', ident)
            with self.mutex:
                del self.id_to_obj[ident]


class State(object):
    __slots__ = ['value']
    INITIAL = 0
    STARTED = 1
    SHUTDOWN = 2


listener_client = {
    'pickle': (connection.Listener, connection.Client),
    'xmlrpclib': (connection.XmlListener, connection.XmlClient),
    }


# ---------------------------------------------------------------- manager

class BaseManager(object):
    """Starts (or connects to) a server process holding shared objects."""

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

    def _check_initial(self):
        if self._state.value != State.INITIAL:
            if self._state.value == State.STARTED:
                raise ProcessError("Already started server")
            elif self._state.value == State.SHUTDOWN:
                raise ProcessError("Manager has shut down")
            else:
                raise ProcessError(
                    "Unknown state {!r}".format(self._state.value))

    def get_server(self):
        """A Server to run in the current process (serve_forever())."""
        self._check_initial()
        return self._Server(self._registry, self._address,
                            self._authkey, self._serializer)

    def connect(self):
        """Connect to an already running manager."""
        Listener, Client = listener_client[self._serializer]
        conn = Client(self._address, authkey=self._authkey)
        dispatch(conn, None, 'dummy')
        self._state.value = State.STARTED

    def start(self, initializer=None, initargs=()):
        """Spawn the server process."""
        self._check_initial()

        if initializer is not None and not callable(initializer):
            raise TypeError('initializer must be a callable')

        # the child reports the address it listens on through this pipe
        reader, writer = connection.Pipe(duplex=False)

        self._process = self._ctx.Process(
            target=type(self)._run_server,
            args=(self._registry, self._address, self._authkey,
                  self._serializer, writer, initializer, initargs),
            )
        ident = ':'.join(str(i) for i in self._process._identity)
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
            exitpriority=0
            )

    @classmethod
    def _run_server(cls, registry, address, authkey, serializer, writer,
                    initializer=None, initargs=()):
        # Ctrl-C is for the parent
        signal.signal(signal.SIGINT, signal.SIG_IGN)

        if initializer is not None:
            initializer(*initargs)

        server = cls._Server(registry, address, authkey, serializer)

        writer.send(server.address)
        writer.close()

        util.info('manager serving at %r', server.address)
        server.serve_forever()

    def _create(self, typeid, /, *args, **kwds):
        """Create a shared object; returns (token, exposed methods)."""
        assert self._state.value == State.STARTED, 'server not yet started'
        conn = self._Client(self._address, authkey=self._authkey)
        try:
            id, exposed = dispatch(conn, None, 'create', (typeid,) + args, kwds)
        finally:
            conn.close()
        return Token(typeid, self._address, id), exposed

    def join(self, timeout=None):
        """Wait for the manager process."""
        if self._process is not None:
            self._process.join(timeout)
            if not self._process.is_alive():
                self._process = None

    def _debug_info(self):
        conn = self._Client(self._address, authkey=self._authkey)
        try:
            return dispatch(conn, None, 'debug_info')
        finally:
            conn.close()

    def _number_of_objects(self):
        conn = self._Client(self._address, authkey=self._authkey)
        try:
            return dispatch(conn, None, 'number_of_objects')
        finally:
            conn.close()

    def __enter__(self):
        if self._state.value == State.INITIAL:
            self.start()
        if self._state.value != State.STARTED:
            if self._state.value == State.SHUTDOWN:
                raise ProcessError("Manager has shut down")
            raise ProcessError(
                "Unknown state {!r}".format(self._state.value))
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.shutdown()

    @staticmethod
    def _finalize_manager(process, address, authkey, state, _Client,
                          shutdown_timeout):
        """Ask the server to stop, then terminate/kill it if needed."""
        if process.is_alive():
            util.info('sending shutdown message to manager')
            try:
                conn = _Client(address, authkey=authkey)
                try:
                    dispatch(conn, None, 'shutdown')
                finally:
                    conn.close()
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
        try:
            del BaseProxy._address_to_local[address]
        except KeyError:
            pass

    @property
    def address(self):
        return self._address

    @classmethod
    def register(cls, typeid, callable=None, proxytype=None, exposed=None,
                 method_to_typeid=None, create_method=True):
        """Register a type (or factory) so that managers can create it."""
        if '_registry' not in cls.__dict__:
            cls._registry = cls._registry.copy()

        if proxytype is None:
            proxytype = AutoProxy

        exposed = exposed or getattr(proxytype, '_exposed_', None)

        method_to_typeid = method_to_typeid or \
                           getattr(proxytype, '_method_to_typeid_', None)

        if method_to_typeid:
            for key, value in list(method_to_typeid.items()):
                assert type(key) is str, '%r is not a string' % key
                assert type(value) is str, '%r is not a string' % value

        cls._registry[typeid] = (
            callable, exposed, method_to_typeid, proxytype
            )

        if create_method:
            def temp(self, /, *args, **kwds):
                util.debug('requesting creation of a shared %r object', typeid)
                token, exp = self._create(typeid, *args, **kwds)
                proxy = proxytype(
                    token, self._serializer, manager=self,
                    authkey=self._authkey, exposed=exp
                    )
                conn = self._Client(token.address, authkey=self._authkey)
                dispatch(conn, None, 'decref', (token.id,))
                return proxy
            temp.__name__ = typeid
            setattr(cls, typeid, temp)


# ---------------------------------------------------------------- proxies

class ProcessLocalSet(set):
    """A set emptied in forked children."""

    def __init__(self):
        util.register_after_fork(self, lambda obj: obj.clear())

    def __reduce__(self):
        return type(self), ()


class BaseProxy(object):
    """Base class of the proxies to managed objects."""

    # server address -> (per-thread connections, ids referenced here)
    _address_to_local = {}
    _mutex = util.ForkAwareThreadLock()

    def __init__(self, token, serializer, manager=None,
                 authkey=None, exposed=None, incref=True, manager_owned=False):
        with BaseProxy._mutex:
            tls_idset = BaseProxy._address_to_local.get(token.address, None)
            if tls_idset is None:
                tls_idset = util.ForkAwareLocal(), ProcessLocalSet()
                BaseProxy._address_to_local[token.address] = tls_idset

        self._tls = tls_idset[0]
        self._idset = tls_idset[1]

        self._token = token
        self._id = self._token.id
        self._manager = manager
        self._serializer = serializer
        self._Client = listener_client[serializer][1]

        # A proxy living in the manager process does not hold a reference.
        self._owned_by_manager = manager_owned

        if authkey is not None:
            self._authkey = process.AuthenticationString(authkey)
        elif self._manager is not None:
            self._authkey = self._manager._authkey
        else:
            self._authkey = process.current_process().authkey

        if incref:
            self._incref()

        util.register_after_fork(self, BaseProxy._after_fork)

    def _connect(self):
        util.debug('making connection to manager')
        name = process.current_process().name
        if threading.current_thread().name != 'MainThread':
            name += '|' + threading.current_thread().name
        conn = self._Client(self._token.address, authkey=self._authkey)
        dispatch(conn, None, 'accept_connection', (name,))
        self._tls.connection = conn

    def _callmethod(self, methodname, args=(), kwds={}):
        """Call *methodname* on the referent; returns its result."""
        try:
            conn = self._tls.connection
        except AttributeError:
            util.debug('thread %r does not own a connection',
                       threading.current_thread().name)
            self._connect()
            conn = self._tls.connection

        conn.send((self._id, methodname, args, kwds))
        kind, result = conn.recv()

        if kind == '#RETURN':
            return result
        elif kind == '#PROXY':
            exposed, token = result
            proxytype = self._manager._registry[token.typeid][-1]
            token.address = self._token.address
            proxy = proxytype(
                token, self._serializer, manager=self._manager,
                authkey=self._authkey, exposed=exposed
                )
            conn = self._Client(token.address, authkey=self._authkey)
            dispatch(conn, None, 'decref', (token.id,))
            return proxy
        raise convert_to_error(kind, result)

    def _getvalue(self):
        """A copy of the referent."""
        return self._callmethod('#GETVALUE')

    def _incref(self):
        if self._owned_by_manager:
            util.debug('owned_by_manager skipped INCREF of %r', self._token.id)
            return

        conn = self._Client(self._token.address, authkey=self._authkey)
        dispatch(conn, None, 'incref', (self._id,))
        util.debug('INCREF %r', self._token.id)

        self._idset.add(self._id)

        state = self._manager and self._manager._state

        self._close = util.Finalize(
            self, BaseProxy._decref,
            args=(self._token, self._authkey, state,
                  self._tls, self._idset, self._Client),
            exitpriority=10
            )

    @staticmethod
    def _decref(token, authkey, state, tls, idset, _Client):
        idset.discard(token.id)

        # the server is gone if the manager was shut down
        if state is None or state.value == State.STARTED:
            try:
                util.debug('DECREF %r', token.id)
                conn = _Client(token.address, authkey=authkey)
                dispatch(conn, None, 'decref', (token.id,))
            except Exception as e:
                util.debug('... decref failed %s', e)
        else:
            util.debug('DECREF %r -- manager already shutdown', token.id)

        # close the thread's connection once it has no proxies left
        if not idset and hasattr(tls, 'connection'):
            util.debug('thread %r has no more proxies so closing conn',
                       threading.current_thread().name)
            tls.connection.close()
            del tls.connection

    def _after_fork(self):
        self._manager = None
        try:
            self._incref()
        except Exception as e:
            # the proxy may be useless in the child
            util.info('incref failed: %s' % e)

    def __reduce__(self):
        kwds = {}
        if get_spawning_popen() is not None:
            kwds['authkey'] = self._authkey

        if getattr(self, '_isauto', False):
            kwds['exposed'] = self._exposed_
            return (RebuildProxy,
                    (AutoProxy, self._token, self._serializer, kwds))
        else:
            return (RebuildProxy,
                    (type(self), self._token, self._serializer, kwds))

    def __deepcopy__(self, memo):
        return self._getvalue()

    def __repr__(self):
        return '<%s object, typeid %r at %#x>' % \
               (type(self).__name__, self._token.typeid, id(self))

    def __str__(self):
        """str() of the referent."""
        try:
            return self._callmethod('__repr__')
        except Exception:
            return repr(self)[:-1] + "; '__str__()' failed>"


def RebuildProxy(func, token, serializer, kwds):
    """Unpickle a proxy (in the manager itself it refers to the object
    directly)."""
    server = getattr(process.current_process(), '_manager_server', None)
    if server and server.address == token.address:
        util.debug('Rebuild a proxy owned by manager, token=%r', token)
        kwds['manager_owned'] = True
        if token.id not in server.id_to_local_proxy_obj:
            server.id_to_local_proxy_obj[token.id] = \
                server.id_to_obj[token.id]
    incref = (
        kwds.pop('incref', True) and
        not getattr(process.current_process(), '_inheriting', False)
        )
    return func(token, serializer, incref=incref, **kwds)


def _make_forwarder(name):
    def forward(self, /, *args, **kwds):
        return self._callmethod(name, args, kwds)
    forward.__name__ = name
    forward.__qualname__ = name
    return forward


def MakeProxyType(name, exposed, _cache={}):
    """A BaseProxy subclass forwarding the methods named in *exposed*."""
    exposed = tuple(exposed)
    try:
        return _cache[(name, exposed)]
    except KeyError:
        pass

    namespace = {meth: _make_forwarder(meth) for meth in exposed}
    ProxyType = type(name, (BaseProxy,), namespace)
    ProxyType._exposed_ = exposed
    _cache[(name, exposed)] = ProxyType
    return ProxyType


def AutoProxy(token, serializer, manager=None, authkey=None,
              exposed=None, incref=True, manager_owned=False):
    """A proxy whose methods are those the server exposes."""
    _Client = listener_client[serializer][1]

    if exposed is None:
        conn = _Client(token.address, authkey=authkey)
        try:
            exposed = dispatch(conn, None, 'get_methods', (token,))
        finally:
            conn.close()

    if authkey is None and manager is not None:
        authkey = manager._authkey
    if authkey is None:
        authkey = process.current_process().authkey

    ProxyType = MakeProxyType('AutoProxy[%s]' % token.typeid, exposed)
    proxy = ProxyType(token, serializer, manager=manager, authkey=authkey,
                      incref=incref, manager_owned=manager_owned)
    proxy._isauto = True
    return proxy


# ---------------------------------------------------------------- shared types

class Namespace(object):
    def __init__(self, /, **kwds):
        self.__dict__.update(kwds)

    def __repr__(self):
        items = sorted('%s=%r' % (name, value)
                       for name, value in self.__dict__.items()
                       if not name.startswith('_'))
        return '%s(%s)' % (self.__class__.__name__, ', '.join(items))


class Value(object):
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


# ---------------------------------------------------------------- proxy types

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
    _exposed_ = ('acquire', 'release', 'locked', 'wait', 'notify', 'notify_all')

    def wait(self, timeout=None):
        return self._callmethod('wait', (timeout,))

    def notify(self, n=1):
        return self._callmethod('notify', (n,))

    def notify_all(self):
        return self._callmethod('notify_all')

    def wait_for(self, predicate, timeout=None):
        result = predicate()
        if result:
            return result
        if timeout is not None:
            endtime = time.monotonic() + timeout
        else:
            endtime = None
            waittime = None
        while not result:
            if endtime is not None:
                waittime = endtime - time.monotonic()
                if waittime <= 0:
                    break
            self.wait(waittime)
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

    def __getattr__(self, key):
        if key[0] == '_':
            return object.__getattribute__(self, key)
        callmethod = object.__getattribute__(self, '_callmethod')
        return callmethod('__getattribute__', (key,))

    def __setattr__(self, key, value):
        if key[0] == '_':
            return object.__setattr__(self, key, value)
        callmethod = object.__getattribute__(self, '_callmethod')
        return callmethod('__setattr__', (key, value))

    def __delattr__(self, key):
        if key[0] == '_':
            return object.__delattr__(self, key)
        callmethod = object.__getattribute__(self, '_callmethod')
        return callmethod('__delattr__', (key,))


class ValueProxy(BaseProxy):
    _exposed_ = ('get', 'set')

    def get(self):
        return self._callmethod('get')

    def set(self, value):
        return self._callmethod('set', (value,))

    value = property(get, set)

    __class_getitem__ = classmethod(types.GenericAlias)


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

    def __imul__(self, value):
        self._callmethod('__imul__', (value,))
        return self

    __class_getitem__ = classmethod(types.GenericAlias)


collections.abc.MutableSequence.register(BaseListProxy)

_BaseDictProxy = MakeProxyType('_BaseDictProxy', (
    '__contains__', '__delitem__', '__getitem__', '__ior__', '__iter__',
    '__len__', '__or__', '__ror__', '__setitem__', 'clear', 'copy',
    'fromkeys', 'get', 'items', 'keys', 'pop', 'popitem', 'setdefault',
    'update', 'values',
    ))
_BaseDictProxy._method_to_typeid_ = {
    '__iter__': 'Iterator',
    }


class DictProxy(_BaseDictProxy):
    def __ior__(self, value):
        self._callmethod('__ior__', (value,))
        return self

    def __reversed__(self):
        keys = self._callmethod('keys')
        return iter(keys[::-1])

    __class_getitem__ = classmethod(types.GenericAlias)


collections.abc.MutableMapping.register(_BaseDictProxy)

_BaseSetProxy = MakeProxyType('_BaseSetProxy', (
    '__and__', '__contains__', '__iand__', '__ior__', '__isub__', '__iter__',
    '__ixor__', '__len__', '__or__', '__rand__', '__ror__', '__rsub__',
    '__rxor__', '__sub__', '__xor__', '__ge__', '__gt__', '__le__', '__lt__',
    'add', 'clear', 'copy', 'difference', 'difference_update', 'discard',
    'intersection', 'intersection_update', 'isdisjoint', 'issubset',
    'issuperset', 'pop', 'remove', 'symmetric_difference',
    'symmetric_difference_update', 'union', 'update',
    ))
_BaseSetProxy._method_to_typeid_ = {
    '__iter__': 'Iterator',
    }


class SetProxy(_BaseSetProxy):
    def __ior__(self, value):
        self._callmethod('__ior__', (value,))
        return self

    def __iand__(self, value):
        self._callmethod('__iand__', (value,))
        return self

    def __ixor__(self, value):
        self._callmethod('__ixor__', (value,))
        return self

    def __isub__(self, value):
        self._callmethod('__isub__', (value,))
        return self

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
    'imap_unordered': 'Iterator'
    }


class PoolProxy(BasePoolProxy):
    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.terminate()


class SyncManager(BaseManager):
    """Manager with the common shared types registered (Queue, Lock,
    Event, list, dict, set, Namespace, Value, ...)."""


SyncManager.register('Queue', queue.Queue)
SyncManager.register('JoinableQueue', queue.Queue)
SyncManager.register('Event', threading.Event, EventProxy)
SyncManager.register('Lock', threading.Lock, AcquirerProxy)
SyncManager.register('RLock', threading.RLock, AcquirerProxy)
SyncManager.register('Semaphore', threading.Semaphore, AcquirerProxy)
SyncManager.register('BoundedSemaphore', threading.BoundedSemaphore,
                     AcquirerProxy)
SyncManager.register('Condition', threading.Condition, ConditionProxy)
SyncManager.register('Barrier', threading.Barrier, BarrierProxy)
SyncManager.register('Pool', pool.Pool, PoolProxy)
SyncManager.register('list', list, ListProxy)
SyncManager.register('dict', dict, DictProxy)
SyncManager.register('set', set, SetProxy)
SyncManager.register('Value', Value, ValueProxy)
SyncManager.register('Array', Array, ArrayProxy)
SyncManager.register('Namespace', Namespace, NamespaceProxy)

# types only returned by methods, never created directly
SyncManager.register('Iterator', proxytype=IteratorProxy, create_method=False)
SyncManager.register('AsyncResult', create_method=False)


# ---------------------------------------------------------------- shared memory

if HAS_SHMEM:
    class _SharedMemoryTracker:
        """Names of the shared memory blocks a manager owns."""

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
            for segment_name in self.segment_names[:]:
                self.destroy_segment(segment_name)

        def __del__(self):
            util.debug(f"Call {self.__class__.__name__}.__del__ in {os.getpid()}")
            self.unlink()

        def __getstate__(self):
            return (self.shared_memory_context_name, self.segment_names)

        def __setstate__(self, state):
            self.__init__(*state)

    class SharedMemoryServer(Server):

        public = Server.public + \
                 ['track_segment', 'release_segment', 'list_segments']

        def __init__(self, *args, **kwargs):
            Server.__init__(self, *args, **kwargs)
            address = self.address
            if isinstance(address, bytes):
                address = os.fsdecode(address)
            self.shared_memory_context = \
                _SharedMemoryTracker(f"shm_{address}_{os.getpid()}", [])
            util.debug(f"SharedMemoryServer started by pid {os.getpid()}")

        def create(self, c, typeid, /, *args, **kwargs):
            """Like Server.create(); types that want it get the shared
            memory tracker."""
            if hasattr(self.registry[typeid][-1], "_shared_memory_proxy"):
                kwargs['shared_memory_context'] = self.shared_memory_context
            return Server.create(self, c, typeid, *args, **kwargs)

        def shutdown(self, c):
            """Unlink every tracked block, then stop."""
            self.shared_memory_context.unlink()
            return Server.shutdown(self, c)

        def track_segment(self, c, segment_name):
            self.shared_memory_context.register_segment(segment_name)

        def release_segment(self, c, segment_name):
            self.shared_memory_context.destroy_segment(segment_name)

        def list_segments(self, c):
            return self.shared_memory_context.segment_names

    class SharedMemoryManager(BaseManager):
        """A manager owning SharedMemory blocks and ShareableLists; they are
        unlinked when the manager shuts down."""

        _Server = SharedMemoryServer

        def __init__(self, *args, **kwargs):
            if os.name == "posix":
                # Start the tracker now so that the server process (and
                # its children) share it with us.
                from . import resource_tracker
                resource_tracker.ensure_running()
            BaseManager.__init__(self, *args, **kwargs)
            util.debug(f"{self.__class__.__name__} created by pid {os.getpid()}")

        def __del__(self):
            util.debug(f"{self.__class__.__name__}.__del__ by pid {os.getpid()}")

        def get_server(self):
            self._check_initial()
            return self._Server(self._registry, self._address,
                                self._authkey, self._serializer)

        def SharedMemory(self, size):
            """A new SharedMemory block owned by the manager."""
            with self._Client(self._address, authkey=self._authkey) as conn:
                sms = shared_memory.SharedMemory(None, create=True, size=size)
                try:
                    dispatch(conn, None, 'track_segment', (sms.name,))
                except BaseException as e:
                    sms.unlink()
                    raise e
            return sms

        def ShareableList(self, sequence):
            """A new ShareableList owned by the manager."""
            with self._Client(self._address, authkey=self._authkey) as conn:
                sl = shared_memory.ShareableList(sequence)
                try:
                    dispatch(conn, None, 'track_segment', (sl.shm.name,))
                except BaseException as e:
                    sl.shm.unlink()
                    raise e
            return sl
