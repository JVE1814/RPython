# Root cause (test_threading test_reinit_tls_after_fork hung ~1 in 20 runs): a thread
# handle's state was a std Mutex. A thread finishing its run marks its handle done after
# it has left the stop-the-world protocol, so os.fork() in another thread could happen
# while it held that mutex; in the child, threading's after-fork hook marks the other
# threads' handles done and blocked forever on the orphaned lock. Handle states are now
# a futex word without a lock, and _thread's non-daemon list is locked across fork()
# (emptied in the child).
# Many threads fork at once while others finish; a child that does not exit in time hung.
import os, threading, time, warnings

warnings.simplefilter("ignore", DeprecationWarning)  # forking with threads
hung = []


def fork_and_wait():
    pid = os.fork()
    if pid == 0:
        os._exit(50)  # the after-fork hooks have run by now
    deadline = time.monotonic() + 20
    while True:
        p, status = os.waitpid(pid, os.WNOHANG)
        if p:
            assert os.waitstatus_to_exitcode(status) == 50
            return
        if time.monotonic() > deadline:
            hung.append(pid)
            os.kill(pid, 9)
            os.waitpid(pid, 0)
            return
        time.sleep(0.001)


for i in range(60):
    ts = [threading.Thread(target=fork_and_wait) for _ in range(16)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    assert not hung, f"{len(hung)} children hung after fork (round {i})"
print("OK")
