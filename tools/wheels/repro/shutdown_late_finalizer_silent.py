# Root cause (tornado's TestMultiprocess.test_bind_start expects empty stderr): an asyncio
# loop that was set as the current loop and never closed is finalized only with the main
# thread's state, after every module's globals were cleared; its __del__ then failed
# ("'NoneType' object has no attribute 'EVENT_READ'") and the report was printed. CPython
# clears the sys module (sys.stderr) before that point, so such late reports go nowhere.
# Shutdown now sets the standard streams to None once the modules are cleared, and the
# default unraisable hook reports nothing when sys.stderr is None.
import subprocess
import sys

CHILD = """
import asyncio
loop = asyncio.new_event_loop()
asyncio.set_event_loop(loop)
loop.run_until_complete(asyncio.sleep(0))
print("done")
"""
r = subprocess.run([sys.executable, "-I", "-c", CHILD], capture_output=True, text=True)
assert r.returncode == 0 and r.stdout == "done\n" and r.stderr == "", (r.stdout, r.stderr[-1500:])

# an unraisable exception while sys.stderr is None is dropped, as in CPython
r = subprocess.run([sys.executable, "-I", "-c",
                    "import sys\nclass A:\n    def __del__(self): 1/0\nsys.stderr = None\nA()\n"],
                   capture_output=True, text=True)
assert r.returncode == 0 and r.stderr == "", r.stderr
print("OK")
