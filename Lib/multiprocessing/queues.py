#
# multiprocessing.queues: process-shared queues built on a pipe.
#
# Queue.put() appends to a local buffer; a daemon "feeder" thread pickles
# buffered objects and writes them to the pipe, so put() never blocks on
# the pipe.  get() reads and unpickles one message under a lock.
#

__all__ = ['Queue', 'SimpleQueue', 'JoinableQueue']

import sys
import os
import threading
import collections
import time
import types
import weakref
import errno

from queue import Empty, Full

from . import connection
from . import context
_ForkingPickler = context.reduction.ForkingPickler

from .util import debug, info, Finalize, register_after_fork, is_exiting

# put into the buffer to make the feeder thread close the pipe and exit
_sentinel = object()


class Queue(object):

    def __init__(self, maxsize=0, *, ctx):
        if maxsize <= 0:
            from .synchronize import SEM_VALUE_MAX as maxsize
        self._maxsize = maxsize
        self._reader, self._writer = connection.Pipe(duplex=False)
        self._rlock = ctx.Lock()
        self._opid = os.getpid()
        if sys.platform == 'win32':
            self._wlock = None
        else:
            self._wlock = ctx.Lock()
        self._sem = ctx.BoundedSemaphore(maxsize)
        # Set by subclasses that tolerate a closed pipe (e.g. the pool).
        self._ignore_epipe = False
        self._reset()

        if sys.platform != 'win32':
            register_after_fork(self, Queue._after_fork)

    def __getstate__(self):
        context.assert_spawning(self)
        return (self._ignore_epipe, self._maxsize, self._reader, self._writer,
                self._rlock, self._wlock, self._sem, self._opid)

    def __setstate__(self, state):
        (self._ignore_epipe, self._maxsize, self._reader, self._writer,
         self._rlock, self._wlock, self._sem, self._opid) = state
        self._reset()

    def _after_fork(self):
        debug('Queue._after_fork()')
        self._reset(after_fork=True)

    def _reset(self, after_fork=False):
        if after_fork:
            self._notempty._at_fork_reinit()
        else:
            self._notempty = threading.Condition(threading.Lock())
        self._buffer = collections.deque()
        self._thread = None
        self._jointhread = None
        self._joincancelled = False
        self._closed = False
        self._close = None
        self._send_bytes = self._writer.send_bytes
        self._recv_bytes = self._reader.recv_bytes
        self._poll = self._reader.poll

    def put(self, obj, block=True, timeout=None):
        if self._closed:
            raise ValueError(f"Queue {self!r} is closed")
        if not self._sem.acquire(block, timeout):
            raise Full

        with self._notempty:
            if self._thread is None:
                self._start_thread()
            self._buffer.append(obj)
            self._notempty.notify()

    def get(self, block=True, timeout=None):
        if self._closed:
            raise ValueError(f"Queue {self!r} is closed")
        if block and timeout is None:
            with self._rlock:
                res = self._recv_bytes()
            self._sem.release()
        else:
            if block:
                deadline = time.monotonic() + timeout
            if not self._rlock.acquire(block, timeout):
                raise Empty
            try:
                if block:
                    timeout = deadline - time.monotonic()
                    if not self._poll(timeout):
                        raise Empty
                elif not self._poll():
                    raise Empty
                res = self._recv_bytes()
                self._sem.release()
            finally:
                self._rlock.release()
        # unpickle outside of the lock
        return _ForkingPickler.loads(res)

    def qsize(self):
        return self._maxsize - self._sem._semlock._get_value()

    def empty(self):
        return not self._poll()

    def full(self):
        return self._sem._semlock._is_zero()

    def get_nowait(self):
        return self.get(False)

    def put_nowait(self, obj):
        return self.put(obj, False)

    def close(self):
        """No more puts from this process; the feeder flushes the buffer
        and closes the pipe."""
        self._closed = True
        close = self._close
        if close:
            self._close = None
            close()

    def join_thread(self):
        """Wait for the feeder thread (only after close())."""
        debug('Queue.join_thread()')
        assert self._closed, "Queue {0!r} not closed".format(self)
        if self._jointhread:
            self._jointhread()

    def cancel_join_thread(self):
        """Do not wait for the feeder thread at process exit."""
        debug('Queue.cancel_join_thread()')
        self._joincancelled = True
        try:
            self._jointhread.cancel()
        except AttributeError:
            pass

    def _terminate_broken(self):
        # The queue cannot be used any more (e.g. the reading process
        # died): drop pending data and stop the feeder.
        self._reader.close()
        if self._wlock is not None:
            # a dead process might hold the write lock forever
            self._wlock = None
        self.close()
        self.join_thread()

    def _start_thread(self):
        debug('Queue._start_thread()')

        self._thread = threading.Thread(
            target=Queue._feed,
            args=(self._buffer, self._notempty, self._send_bytes,
                  self._wlock, self._reader.close, self._writer.close,
                  self._ignore_epipe, self._on_queue_feeder_error,
                  self._sem),
            name='QueueFeederThread',
            daemon=True,
        )

        try:
            debug('doing self._thread.start()')
            self._thread.start()
            debug('... done self._thread.start()')
        except:
            self._thread = None
            raise

        if not self._joincancelled:
            # Join the feeder at exit so buffered data reaches the pipe;
            # only a weak reference: the Finalize must not keep it alive.
            self._jointhread = Finalize(
                self._thread, Queue._finalize_join,
                [weakref.ref(self._thread)],
                exitpriority=-5
                )

        # Flush the buffer when the queue is collected or at exit (before
        # the join above, which needs the sentinel).
        self._close = Finalize(
            self, Queue._finalize_close,
            [self._buffer, self._notempty],
            exitpriority=10
            )

    @staticmethod
    def _finalize_join(twr):
        debug('joining queue thread')
        thread = twr()
        if thread is not None:
            thread.join()
            debug('... queue thread joined')
        else:
            debug('... queue thread already dead')

    @staticmethod
    def _finalize_close(buffer, notempty):
        debug('telling queue thread to quit')
        with notempty:
            buffer.append(_sentinel)
            notempty.notify()

    @staticmethod
    def _feed(buffer, notempty, send_bytes, writelock, reader_close,
              writer_close, ignore_epipe, onerror, queue_sem):
        debug('starting thread to feed data to pipe')
        nacquire = notempty.acquire
        nrelease = notempty.release
        nwait = notempty.wait
        bpopleft = buffer.popleft
        sentinel = _sentinel
        if sys.platform != 'win32' and writelock is not None:
            wacquire = writelock.acquire
            wrelease = writelock.release
        else:
            wacquire = None

        while True:
            try:
                nacquire()
                try:
                    if not buffer:
                        nwait()
                finally:
                    nrelease()
                try:
                    while True:
                        obj = bpopleft()
                        if obj is sentinel:
                            debug('feeder thread got sentinel -- exiting')
                            reader_close()
                            writer_close()
                            return

                        data = _ForkingPickler.dumps(obj)
                        if wacquire is None:
                            send_bytes(data)
                        else:
                            wacquire()
                            try:
                                send_bytes(data)
                            finally:
                                wrelease()
                        del obj, data
                except IndexError:
                    pass
            except Exception as e:
                if ignore_epipe and getattr(e, 'errno', 0) == errno.EPIPE:
                    return
                # At exit the pipe may already be gone: only log.
                if is_exiting():
                    info('error in queue thread: %s', e)
                    return
                else:
                    # the object was not delivered: give its slot back
                    queue_sem.release()
                    onerror(e, obj)

    @staticmethod
    def _on_queue_feeder_error(e, obj):
        """Called in the feeder thread when *obj* cannot be sent (e.g. it
        is not picklable).  Prints the traceback by default."""
        import traceback
        traceback.print_exc()

    __class_getitem__ = classmethod(types.GenericAlias)


class JoinableQueue(Queue):
    """A Queue with task_done() and join()."""

    def __init__(self, maxsize=0, *, ctx):
        Queue.__init__(self, maxsize, ctx=ctx)
        self._unfinished_tasks = ctx.Semaphore(0)
        self._cond = ctx.Condition()

    def __getstate__(self):
        return Queue.__getstate__(self) + (self._cond, self._unfinished_tasks)

    def __setstate__(self, state):
        Queue.__setstate__(self, state[:-2])
        self._cond, self._unfinished_tasks = state[-2:]

    def put(self, obj, block=True, timeout=None):
        if self._closed:
            raise ValueError(f"Queue {self!r} is closed")
        if not self._sem.acquire(block, timeout):
            raise Full

        with self._notempty, self._cond:
            if self._thread is None:
                self._start_thread()
            self._buffer.append(obj)
            self._unfinished_tasks.release()
            self._notempty.notify()

    def task_done(self):
        with self._cond:
            if not self._unfinished_tasks.acquire(False):
                raise ValueError('task_done() called too many times')
            if self._unfinished_tasks._semlock._is_zero():
                self._cond.notify_all()

    def join(self):
        with self._cond:
            if not self._unfinished_tasks._semlock._is_zero():
                self._cond.wait()


class SimpleQueue(object):
    """A pipe with locks: no buffering thread, no size."""

    def __init__(self, *, ctx):
        self._reader, self._writer = connection.Pipe(duplex=False)
        self._rlock = ctx.Lock()
        self._poll = self._reader.poll
        if sys.platform == 'win32':
            self._wlock = None
        else:
            self._wlock = ctx.Lock()

    def close(self):
        self._reader.close()
        self._writer.close()

    def empty(self):
        return not self._poll()

    def __getstate__(self):
        context.assert_spawning(self)
        return (self._reader, self._writer, self._rlock, self._wlock)

    def __setstate__(self, state):
        (self._reader, self._writer, self._rlock, self._wlock) = state
        self._poll = self._reader.poll

    def get(self):
        with self._rlock:
            res = self._reader.recv_bytes()
        return _ForkingPickler.loads(res)

    def put(self, obj):
        data = _ForkingPickler.dumps(obj)
        if self._wlock is None:
            self._writer.send_bytes(data)
        else:
            with self._wlock:
                self._writer.send_bytes(data)

    __class_getitem__ = classmethod(types.GenericAlias)
