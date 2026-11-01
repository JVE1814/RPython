"""Report the statistics collected by profile or cProfile.

Stats(source, ...) loads data from profiler objects or files written by
dump_stats(); sort_stats(), strip_dirs(), print_stats(), print_callers()
and print_callees() produce reports.
"""

import marshal
import os
import re
import sys
import time
from dataclasses import dataclass
from enum import StrEnum, _simple_enum
from functools import cmp_to_key

__all__ = ["Stats", "SortKey", "FunctionProfile", "StatsProfile"]


@_simple_enum(StrEnum)
class SortKey:
    CALLS = 'calls', 'ncalls'
    CUMULATIVE = 'cumulative', 'cumtime'
    FILENAME = 'filename', 'module'
    LINE = 'line'
    NAME = 'name'
    NFL = 'nfl'
    PCALLS = 'pcalls'
    STDNAME = 'stdname'
    TIME = 'time', 'tottime'

    def __new__(cls, *values):
        value = values[0]
        member = str.__new__(cls, value)
        member._value_ = value
        for alias in values[1:]:
            cls._value2member_map_[alias] = member
        member._all_values = values
        return member


@dataclass(unsafe_hash=True)
class FunctionProfile:
    ncalls: str
    tottime: float
    percall_tottime: float
    cumtime: float
    percall_cumtime: float
    file_name: str
    line_number: int


@dataclass(unsafe_hash=True)
class StatsProfile:
    '''Class for keeping track of an item in inventory.'''
    total_tt: float
    func_profiles: dict[str, FunctionProfile]


# Row layout used for sorting: (primitive calls, total calls, internal time,
# cumulative time, file, line, name, standard name).
_PCALLS, _NCALLS, _TT, _CT, _FILE, _LINE, _NAME, _STDNAME = range(8)


class Stats:
    """Profile statistics, aggregated over one or more sources.

    Each source is a file written by a profiler's dump_stats(), a profiler
    object, or another Stats object.  `stream` receives the reports
    (default sys.stdout)."""

    sort_arg_dict_default = {
        "calls":      (((_NCALLS, -1),), "call count"),
        "ncalls":     (((_NCALLS, -1),), "call count"),
        "cumtime":    (((_CT, -1),), "cumulative time"),
        "cumulative": (((_CT, -1),), "cumulative time"),
        "filename":   (((_FILE, 1),), "file name"),
        "line":       (((_LINE, 1),), "line number"),
        "module":     (((_FILE, 1),), "file name"),
        "name":       (((_NAME, 1),), "function name"),
        "nfl":        (((_NAME, 1), (_FILE, 1), (_LINE, 1)), "name/file/line"),
        "pcalls":     (((_PCALLS, -1),), "primitive call count"),
        "stdname":    (((_STDNAME, 1),), "standard name"),
        "time":       (((_TT, -1),), "internal time"),
        "tottime":    (((_TT, -1),), "internal time"),
    }

    def __init__(self, *args, stream=None):
        self.stream = stream or sys.stdout
        if not args:
            arg = None
        else:
            arg = args[0]
            args = args[1:]
        self.init(arg)
        self.add(*args)

    def init(self, arg):
        self.all_callees = None
        self.files = []
        self.fcn_list = None
        self.total_tt = 0
        self.total_calls = 0
        self.prim_calls = 0
        self.max_name_len = 0
        self.top_level = set()
        self.stats = {}
        self.sort_arg_dict = {}
        self.load_stats(arg)
        try:
            self.get_top_level_stats()
        except Exception:
            print("Invalid timing data %s" %
                  (self.files[-1] if self.files else ''), file=self.stream)
            raise

    def load_stats(self, arg):
        if arg is None:
            self.stats = {}
            return
        if isinstance(arg, str):
            with open(arg, 'rb') as stream:
                self.stats = marshal.load(stream)
            try:
                when = time.ctime(os.stat(arg).st_mtime) + "    "
            except OSError:
                when = ""
            self.files = [when + arg]
        elif hasattr(arg, 'create_stats'):
            arg.create_stats()
            self.stats = arg.stats
            arg.stats = {}
        if not self.stats:
            raise TypeError("Cannot create or construct a %r object from %r"
                            % (self.__class__, arg))

    def get_top_level_stats(self):
        for func, (cc, nc, tt, ct, callers) in self.stats.items():
            self.total_calls += nc
            self.prim_calls += cc
            self.total_tt += tt
            if not callers:
                # nothing profiled called it: an entry point
                self.top_level.add(func)
            self.max_name_len = max(self.max_name_len, len(func_std_string(func)))

    def add(self, *arg_list):
        """Merge more sources into these statistics."""
        if not arg_list:
            return self
        for item in reversed(arg_list):
            if type(self) is not type(item):
                item = Stats(item)
            self.files += item.files
            self.total_calls += item.total_calls
            self.prim_calls += item.prim_calls
            self.total_tt += item.total_tt
            self.top_level.update(item.top_level)
            self.max_name_len = max(self.max_name_len, item.max_name_len)
            self.fcn_list = None
            for func, stat in item.stats.items():
                if func in self.stats:
                    old = self.stats[func]
                else:
                    old = (0, 0, 0, 0, {})
                self.stats[func] = add_func_stats(old, stat)
        return self

    def dump_stats(self, filename):
        """Write the statistics to a file readable by Stats()."""
        with open(filename, 'wb') as stream:
            marshal.dump(self.stats, stream)

    def get_sort_arg_defs(self):
        """Sort names including all unique abbreviations."""
        if not self.sort_arg_dict:
            table = {}
            ambiguous = set()
            for word, value in self.sort_arg_dict_default.items():
                fragment = word
                while fragment:
                    if fragment in ambiguous:
                        fragment = fragment[:-1]
                        continue
                    if fragment in table:
                        ambiguous.add(fragment)
                        del table[fragment]
                    else:
                        table[fragment] = value
                    fragment = fragment[:-1]
            # full names always win over abbreviations
            for word, value in self.sort_arg_dict_default.items():
                table[word] = value
            self.sort_arg_dict = table
        return self.sort_arg_dict

    def sort_stats(self, *field):
        """Sort by the given keys (names, unique prefixes or SortKey
        members); a single int selects the legacy orders -1..2."""
        if not field:
            self.fcn_list = 0
            return self
        if len(field) == 1 and isinstance(field[0], int):
            field = [{-1: "stdname", 0: "calls", 1: "time",
                      2: "cumulative"}[field[0]]]
        elif len(field) >= 2:
            kinds = {type(f) for f in field}
            if len(kinds) > 1:
                raise TypeError("Can't have mixed argument type")
        definitions = self.get_sort_arg_defs()
        keys = []
        description = ""
        for word in field:
            if isinstance(word, SortKey):
                word = word.value
            spec, label = definitions[word]
            keys.extend(spec)
            description += (", " if description else "") + label
        rows = []
        for func, (cc, nc, tt, ct, callers) in self.stats.items():
            rows.append((cc, nc, tt, ct) + func + (func_std_string(func), func))

        def compare(left, right):
            for index, direction in keys:
                a, b = left[index], right[index]
                if a < b:
                    return -direction
                if a > b:
                    return direction
            return 0

        rows.sort(key=cmp_to_key(compare))
        self.fcn_list = [row[-1] for row in rows]
        self.sort_type = description
        return self

    def reverse_order(self):
        if self.fcn_list:
            self.fcn_list.reverse()
        return self

    def strip_dirs(self):
        """Remove directory names from the file names (merging entries
        that become identical)."""
        stripped = {}
        longest = 0
        for func, (cc, nc, tt, ct, callers) in self.stats.items():
            new_func = func_strip_path(func)
            longest = max(longest, len(func_std_string(new_func)))
            new_callers = {func_strip_path(f): v for f, v in callers.items()}
            if new_func in stripped:
                stripped[new_func] = add_func_stats(stripped[new_func],
                                                    (cc, nc, tt, ct, new_callers))
            else:
                stripped[new_func] = (cc, nc, tt, ct, new_callers)
        self.stats = stripped
        self.top_level = {func_strip_path(f) for f in self.top_level}
        self.max_name_len = longest
        self.fcn_list = 0
        self.all_callees = None
        return self

    def calc_callees(self):
        if self.all_callees:
            return
        self.all_callees = callees = {}
        for func, (cc, nc, tt, ct, callers) in self.stats.items():
            callees.setdefault(func, {})
            for caller, stat in callers.items():
                callees.setdefault(caller, {})[func] = stat

    # ---- selecting entries

    def eval_print_amount(self, sel, rows, msg):
        if isinstance(sel, str):
            try:
                pattern = re.compile(sel)
            except re.error:
                msg += "   <Invalid regular expression %r>\n" % sel
                return rows, msg
            selected = [f for f in rows if pattern.search(func_std_string(f))]
        else:
            count = len(rows)
            if isinstance(sel, float) and 0.0 <= sel < 1.0:
                count = int(count * sel + .5)
                selected = rows[:count]
            elif isinstance(sel, int) and 0 <= sel < count:
                selected = rows[:sel]
            else:
                selected = rows
        if len(rows) != len(selected):
            msg += "   List reduced from %r to %r due to restriction <%r>\n" % (
                len(rows), len(selected), sel)
        return selected, msg

    def get_stats_profile(self):
        """A StatsProfile with a FunctionProfile per function."""
        functions = list(self.fcn_list) if self.fcn_list else list(self.stats)
        profiles = {}
        for func in functions:
            cc, nc, tt, ct, callers = self.stats[func]
            name = func[2]
            ncalls = str(nc) if nc == cc else f'{nc}/{cc}'
            profiles[name] = FunctionProfile(
                ncalls,
                round(tt, 3),
                round(tt / nc, 3) if nc else 0,
                round(ct, 3),
                round(ct / cc, 3) if cc else 0,
                func[0],
                int(func[1]))
        return StatsProfile(round(self.total_tt, 3), profiles)

    def get_print_list(self, sel_list):
        width = self.max_name_len
        if self.fcn_list:
            rows = self.fcn_list[:]
            msg = "   Ordered by: " + self.sort_type + '\n'
        else:
            rows = list(self.stats.keys())
            msg = "   Random listing order was used\n"
        for selection in sel_list:
            rows, msg = self.eval_print_amount(selection, rows, msg)
        count = len(rows)
        if not rows:
            return 0, rows
        print(msg, file=self.stream)
        if count < len(self.stats):
            width = max(len(func_std_string(f)) for f in rows)
        return width + 2, rows

    # ---- printing

    def print_stats(self, *amount):
        for filename in self.files:
            print(filename, file=self.stream)
        if self.files:
            print(file=self.stream)
        indent = ' ' * 8
        print(indent, self.total_calls, "function calls", end=' ', file=self.stream)
        if self.total_calls != self.prim_calls:
            print("(%d primitive calls)" % self.prim_calls, end=' ', file=self.stream)
        print("in %.3f seconds" % self.total_tt, file=self.stream)
        print(file=self.stream)
        width, rows = self.get_print_list(amount)
        if rows:
            self.print_title()
            for func in rows:
                self.print_line(func)
            print(file=self.stream)
            print(file=self.stream)
        return self

    def print_callees(self, *amount):
        width, rows = self.get_print_list(amount)
        if rows:
            self.calc_callees()
            self.print_call_heading(width, "called...")
            for func in rows:
                self.print_call_line(width, func, self.all_callees.get(func, {}))
            print(file=self.stream)
            print(file=self.stream)
        return self

    def print_callers(self, *amount):
        width, rows = self.get_print_list(amount)
        if rows:
            self.print_call_heading(width, "was called by...")
            for func in rows:
                cc, nc, tt, ct, callers = self.stats[func]
                self.print_call_line(width, func, callers, "<-")
            print(file=self.stream)
            print(file=self.stream)
        return self

    def print_call_heading(self, name_size, column_title):
        print("Function ".ljust(name_size) + column_title, file=self.stream)
        # caller tables of profile hold 4-tuples, those of cProfile ints
        detailed = False
        for cc, nc, tt, ct, callers in self.stats.values():
            if callers:
                detailed = isinstance(next(iter(callers.values())), tuple)
                break
        if detailed:
            print(" " * name_size + "    ncalls  tottime  cumtime", file=self.stream)

    def print_call_line(self, name_size, source, call_dict, arrow="->"):
        print(func_std_string(source).ljust(name_size) + arrow, end=' ', file=self.stream)
        if not call_dict:
            print(file=self.stream)
            return
        indent = ""
        for func in sorted(call_dict.keys()):
            name = func_std_string(func)
            value = call_dict[func]
            if isinstance(value, tuple):
                nc, cc, tt, ct = value
                if nc != cc:
                    substats = '%d/%d' % (nc, cc)
                else:
                    substats = '%d' % (nc,)
                substats = '%s %s %s  %s' % (substats.rjust(7 + 2 * len(indent)),
                                             f8(tt), f8(ct), name)
                left_width = name_size + 1
            else:
                substats = '%s(%r) %s' % (name, value, f8(self.stats[func][3]))
                left_width = name_size + 3
            print(indent * left_width + substats, file=self.stream)
            indent = " "

    def print_title(self):
        print('   ncalls  tottime  percall  cumtime  percall', end=' ', file=self.stream)
        print('filename:lineno(function)', file=self.stream)

    def print_line(self, func):
        cc, nc, tt, ct, callers = self.stats[func]
        calls = str(nc)
        if nc != cc:
            calls = calls + '/' + str(cc)
        print(calls.rjust(9), end=' ', file=self.stream)
        print(f8(tt), end=' ', file=self.stream)
        print(' ' * 8 if nc == 0 else f8(tt / nc), end=' ', file=self.stream)
        print(f8(ct), end=' ', file=self.stream)
        print(' ' * 8 if cc == 0 else f8(ct / cc), end=' ', file=self.stream)
        print(func_std_string(func), file=self.stream)


class TupleComp:
    """Comparison by a list of (index, direction) pairs (legacy helper)."""

    def __init__(self, comp_select_list):
        self.comp_select_list = comp_select_list

    def compare(self, left, right):
        for index, direction in self.comp_select_list:
            a, b = left[index], right[index]
            if a < b:
                return -direction
            if a > b:
                return direction
        return 0


# ---------------------------------------------------------------- helpers

def func_strip_path(func_name):
    filename, line, name = func_name
    return os.path.basename(filename), line, name


def func_get_function_name(func):
    return func[2]


def func_std_string(func_name):
    if func_name[:2] == ('~', 0):
        # built-in functions
        name = func_name[2]
        if name.startswith('<') and name.endswith('>'):
            return '{%s}' % name[1:-1]
        return name
    return "%s:%d(%s)" % func_name


def add_func_stats(target, source):
    """Combine two (cc, nc, tt, ct, callers) records."""
    cc, nc, tt, ct, callers = source
    t_cc, t_nc, t_tt, t_ct, t_callers = target
    return (cc + t_cc, nc + t_nc, tt + t_tt, ct + t_ct,
            add_callers(t_callers, callers))


def add_callers(target, source):
    """Combine two caller dictionaries (values are counts or 4-tuples)."""
    combined = dict(target)
    for func, value in source.items():
        if func in combined:
            if isinstance(value, tuple):
                combined[func] = tuple(a + b for a, b in zip(value, combined[func]))
            else:
                combined[func] += value
        else:
            combined[func] = value
    return combined


def count_calls(callers):
    """Total number of calls in a caller dictionary."""
    return sum(callers.values())


def f8(x):
    return "%8.3f" % x


if __name__ == '__main__':
    import cmd

    class ProfileBrowser(cmd.Cmd):
        def __init__(self, profile=None):
            cmd.Cmd.__init__(self)
            self.prompt = "% "
            self.stats = None
            self.stream = sys.stdout
            if profile is not None:
                self.do_read(profile)

        def do_read(self, line):
            if line:
                try:
                    self.stats = Stats(line)
                except Exception as err:
                    print(err, file=self.stream)
                    return
                self.prompt = line + "% "
            elif len(self.prompt) > 2:
                line = self.prompt[:-2]
                self.do_read(line)
            else:
                print("No statistics object is current -- cannot reload.",
                      file=self.stream)
            return 0

        def do_stats(self, line):
            if self.stats:
                self.stats.print_stats(*_parse_amounts(line))
            return 0

        def do_sort(self, line):
            if self.stats:
                self.stats.sort_stats(*line.split())
            return 0

        def do_strip(self, line):
            if self.stats:
                self.stats.strip_dirs()
            return 0

        def do_quit(self, line):
            return 1

        def do_EOF(self, line):
            print("", file=self.stream)
            return 1

    def _parse_amounts(line):
        result = []
        for word in line.split():
            try:
                result.append(int(word))
                continue
            except ValueError:
                pass
            try:
                result.append(float(word))
            except ValueError:
                result.append(word)
        return result

    browser = ProfileBrowser(sys.argv[1] if len(sys.argv) > 1 else None)
    try:
        browser.cmdloop()
    except KeyboardInterrupt:
        pass
