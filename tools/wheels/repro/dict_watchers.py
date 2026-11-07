# Root cause: PyDict_AddWatcher/PyDict_Watch were accepted but the callbacks never
# fired (test_capi.test_watchers dict tests, test_free_threading.test_dict_watcher).
# Watched dicts are now kept in a side table and the dict primitives notify.
import _testcapi

wid = _testcapi.add_dict_watcher(0)
try:
    d = {}
    _testcapi.watch_dict(wid, d)
    d["a"] = 1
    d["a"] = 2
    d.update({"b": 3})
    del d["a"]
    d.clear()
    d2 = {}
    _testcapi.watch_dict(wid, d2)
    d2.update({"x": 1})
    del d2
    ev = _testcapi.get_dict_watcher_events()
    assert ev == ["new:a:1", "mod:a:2", "new:b:3", "del:a", "clear", "clone", "dealloc"], ev
    _testcapi.unwatch_dict(wid, d)
    d["z"] = 0
    assert len(_testcapi.get_dict_watcher_events()) == 7
finally:
    _testcapi.clear_dict_watcher(wid)
print("OK")
