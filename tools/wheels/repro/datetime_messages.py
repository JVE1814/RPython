# Root causes (3.15 behaviour that pandas' tests check, each verified there):
#  * datetime()/date() report a missing year/month/day like an argument-clinic
#    function: "datetime() missing required argument 'month' (pos 2)"; non-integers
#    give "'float' object cannot be interpreted as an integer".
#  * slice-index errors: "slice indices must be integers or have an __index__ method"
#    (numpy passes it through from PySlice_Unpack).
#  * operator errors name the type by tp_name: 'numpy.ndarray', and a PyType_FromSpec
#    type by its full spec name ('pandas._libs.interval.Interval').
#  * _strptime.TimeRE has the private helper pandas' TimeRE subclass calls
#    (self.__seqToRE inside a class named TimeRE).
#  * the native datetime's private field getsets must not collide with subclass
#    attributes: pandas' Timestamp defines its own `_year`.
import datetime
import _strptime


def msg(f):
    try:
        f()
    except TypeError as e:
        return str(e)
    raise AssertionError("no TypeError")


assert msg(lambda: datetime.datetime(2020, 1)) == "datetime() missing required argument 'day' (pos 3)"
assert msg(lambda: datetime.datetime(month=1, day=1)) == "datetime() missing required argument 'year' (pos 1)"
assert msg(lambda: datetime.date(2020)) == "date() missing required argument 'month' (pos 2)"
assert msg(lambda: datetime.datetime(2020.0, 1, 1)) == "'float' object cannot be interpreted as an integer"
assert msg(lambda: datetime.datetime(2020, None, 1)) == "'NoneType' object cannot be interpreted as an integer"
assert msg(lambda: [1, 2][1.5:]) == "slice indices must be integers or have an __index__ method"
# pickled state forms still work
d = datetime.datetime(2020, 1, 2, 3, 4, 5, 6, datetime.timezone.utc, fold=1)
import pickle  # round trip of our own object (trusted data)
assert pickle.loads(pickle.dumps(d)) == d and pickle.loads(pickle.dumps(d)).fold == 1


class TimeRE(_strptime.TimeRE):
    def zones(self):
        return self.__seqToRE(["UTC", "GMT"], "Z")


assert TimeRE().zones() in ("(?P<Z>UTC|GMT)", "(?P<Z>GMT|UTC)")


class Sub(datetime.datetime):
    _year = 12345          # a subclass attribute must not change the base methods
    _hour = None


s = Sub(2020, 1, 2, 3)
assert s.isoformat() == "2020-01-02T03:00:00", s.isoformat()
assert s.year == 2020 and s.hour == 3

try:
    import numpy as np
except ImportError:
    pass
else:
    class T:
        pass
    assert msg(lambda: np.float64(1) @ T()) == "unsupported operand type(s) for @: 'numpy.float64' and 'T'"
print("OK")
