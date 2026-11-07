#
# multiprocessing.queues -- queues between processes.
#
# Queue: put() hands the object to a per-process feeder thread that pickles
# it and writes it to a pipe; a bounded semaphore counts the items in
# flight.  JoinableQueue adds task_done()/join().  SimpleQueue writes to
# the pipe directly, without a thread.
#

__all__ = ['Queue', 'SimpleQueue', 'JoinableQueue']

import collections
import errno
import os
import sys
import threading
import time
import types
import weakref

from queue import Empty, Full

from . import connection
from . import context
_ForkingPickler = context.reduction.ForkingPickler

from .util import debug, info, Finalize, register_after_fork, is_exiting

# tells the feeder thread to stop
_sentinel = object()

# pipe writes are atomic per message on Windows: no write lock there
_NEEDS_WRITE_LOCK = sys.platform != 'win32'


class Queue(object):
    """A FIFO queue shared between processes (see queue.Queue)."""

    def __init__(self, maxsize=0, *, ctx):
        if maxsize <= 0:
            from .synchronize import SEM_VALUE_MAX as maxsize
        self._maxsize = maxsize
        self._reader, self._writer = connection.Pipe(duplex=False)
        self._rlock = ctx.Lock()
        self._opid = os.getpid()
        self._wlock = ctx.Lock() if _NEEDS_WRITE_LOCK else None
        self._sem = ctx.BoundedSemaphore(maxsize)
        # concurrent.futures sets this: a feeder writing to a pipe nobody
        # reads any more just stops
        self._ignore_epipe = False
        self._reset()
        if _NEEDS_WRITE_LOCK:
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
        """Per-process state: the buffer and the (not yet started) feeder."""
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

    def _check_open(self):
        if self._closed:
            raise ValueError(f"Queue {self!r} is closed")

    def _enqueue(self, obj):
        # with self._notempty held
        if self._thread is None:
            self._start_thread()
        self._buffer.append(obj)
        self._notempty.notify()

    def put(self, obj, block=True, timeout=None):
        self._check_open()
        if not self._sem.acquire(block, timeout):
            raise Full
        with self._notempty:
            self._enqueue(obj)

    def get(self, block=True, timeout=None):
        self._check_open()
        if block and timeout is None:
            with self._rlock:
                data = self._recv_bytes()
            self._sem.release()
        else:
            deadline = time.monotonic() + timeout if block else None
            if not self._rlock.acquire(block, timeout):
                raise Empty
            try:
                remaining = 0.0
                if block:
                    remaining = deadline - time.monotonic()
                if not self._poll(remaining):
                    raise Empty
                data = self._recv_bytes()
                self._sem.release()
            finally:
                self._rlock.release()
        # unpickle outside the lock
        return _ForkingPickler.loads(data)

    def qsize(self):
        # sem_getvalue() is missing on macOS: NotImplementedError there
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
        """No more puts from this process; the feeder writes what is
        buffered and stops."""
        self._closed = True
        close, self._close = self._close, None
        if close:
            close()

    def join_thread(self):
        """Wait for the feeder thread (after close())."""
        debug('Queue.join_thread()')
        assert self._closed, "Queue {0!r} not closed".format(self)
        if self._jointhread:
            self._jointhread()

    def cancel_join_thread(self):
        """Do not wait for the feeder when the process exits (buffered
        items may be lost)."""
        debug('Queue.cancel_join_thread()')
        self._joincancelled = True
        if self._jointhread is not None:
            self._jointhread.cancel()

    def _terminate_broken(self):
        # The reading side is gone (a broken process pool): stop writing.
        # Closing the reader makes a feeder blocked in a write fail.
        self._reader.close()
        if not _NEEDS_WRITE_LOCK:
            self._writer.close()
        self.close()
        self.join_thread()

    def _start_thread(self):
        debug('Queue._start_thread()')
        self._buffer.clear()
        self._thread = threading.Thread(
            target=Queue._feed,
            args=(self._buffer, self._notempty, self._send_bytes,
                  self._wlock, self._reader.close, self._writer.close,
                  self._ignore_epipe, self._on_queue_feeder_error,
                  self._sem),
            name='QueueFeederThread', daemon=True)
        try:
            self._thread.start()
        except:
            # starting a thread can fail at interpreter shutdown
            self._thread = None
            raise
        debug('feeder thread started')
        if not self._joincancelled:
            # wait for the feeder at exit, after the queue's own finalizer
            self._jointhread = Finalize(
                self._thread, Queue._finalize_join,
                [weakref.ref(self._thread)], exitpriority=-5)
        # stop the feeder when the queue is collected (or at exit)
        self._close = Finalize(self, Queue._finalize_close,
                               [self._buffer, self._notempty],
                               exitpriority=10)

    @staticmethod
    def _finalize_join(twr):
        thread = twr()
        if thread is None:
            debug('feeder thread already gone')
            return
        debug('joining feeder thread')
        thread.join()
        debug('feeder thread joined')

    @staticmethod
    def _finalize_close(buffer, notempty):
        debug('asking the feeder thread to stop')
        with notempty:
            buffer.append(_sentinel)
            notempty.notify()

    @staticmethod
    def _feed(buffer, notempty, send_bytes, writelock, reader_close,
              writer_close, ignore_epipe, onerror, queue_sem):
        """Feeder thread: pickle buffered objects and write them to the
        pipe.  It refers to the queue only through these arguments, so the
        queue can be collected while the thread runs."""
        debug('feeder thread running')
        while True:
            with notempty:
                while not buffer:
                    notempty.wait()
                batch = list(buffer)
                buffer.clear()
            for position, obj in enumerate(batch):
                if obj is _sentinel:
                    debug('feeder thread got the stop marker')
                    reader_close()
                    writer_close()
                    return
                try:
                    data = _ForkingPickler.dumps(obj)
                    if writelock is None:
                        send_bytes(data)
                    else:
                        with writelock:
                            send_bytes(data)
                except Exception as exc:
                    if ignore_epipe and getattr(exc, 'errno', 0) == errno.EPIPE:
                        return
                    if is_exiting():
                        # the process is being torn down: whatever this
                        # thread uses may be gone already
                        info('error in queue thread: %s', exc)
                        return
                    # the object never got into the pipe: the queue holds
                    # one item less
                    queue_sem.release()
                    onerror(exc, obj)
                finally:
                    obj = data = None
            batch = None

    @staticmethod
    def _on_queue_feeder_error(e, obj):
        """Called in the feeder thread when sending obj failed with e.
        concurrent.futures overrides it."""
        import traceback
        traceback.print_exc()

    __class_getitem__ = classmethod(types.GenericAlias)


class JoinableQueue(Queue):
    """A Queue whose consumers report task_done(), so that join() can wait
    until every item has been processed."""

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
        self._check_open()
        if not self._sem.acquire(block, timeout):
            raise Full
        with self._notempty, self._cond:
            self._enqueue(obj)
            self._unfinished_tasks.release()

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
    """A queue without a feeder thread or a size limit."""

    def __init__(self, *, ctx):
        self._reader, self._writer = connection.Pipe(duplex=False)
        self._rlock = ctx.Lock()
        self._poll = self._reader.poll
        self._wlock = ctx.Lock() if _NEEDS_WRITE_LOCK else None

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
            data = self._reader.recv_bytes()
        return _ForkingPickler.loads(data)

    def put(self, obj):
        # pickle before taking the lock
        data = _ForkingPickler.dumps(obj)
        if self._wlock is None:
            self._writer.send_bytes(data)
        else:
            with self._wlock:
                self._writer.send_bytes(data)

    __class_getitem__ = classmethod(types.GenericAlias)
