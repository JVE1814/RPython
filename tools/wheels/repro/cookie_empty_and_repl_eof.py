# Two root causes found with tornado's suite:
# 1. http.cookies wrote an empty cookie value bare (`baz=;`); it is a quoted empty
#    string (`baz="";`): tornado's clear_all_cookies test.
# 2. An interactive session (-i) on a pipe that reaches end of input ends the prompt line
#    with a newline on stderr also when stdin is not a terminal (test_close_stdin).
import http.cookies
import subprocess
import sys

c = http.cookies.SimpleCookie()
c["baz"] = ""
c["baz"]["path"] = "/"
assert c["baz"].OutputString() == 'baz=""; Path=/', c["baz"].OutputString()

r = subprocess.run([sys.executable, "-u", "-i", "-I"], input=b"", capture_output=True)
assert r.stderr.endswith(b">>> \n"), r.stderr[-20:]
print("OK")
