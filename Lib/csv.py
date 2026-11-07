"""Reading and writing CSV (comma separated values) files.

    import csv
    with open("data.csv", newline="") as f:
        for row in csv.reader(f):          # each row is a list of str
            ...
    with open("out.csv", "w", newline="") as f:
        csv.writer(f).writerow(["a", 1, None])

Files should be opened with ``newline=""`` so that line endings inside
quoted fields survive.  The format details (delimiter, quoting, escaping,
line terminator, ...) are described by a dialect: a registered name such
as 'excel' (the default), 'excel-tab' or 'unix', a Dialect subclass, or
keyword arguments overriding individual settings.

DictReader and DictWriter map rows to and from dictionaries; Sniffer
guesses the dialect of a sample and whether it starts with a header row.
"""

import types
from io import StringIO
from _csv import (Error, writer, reader, register_dialect,
                  unregister_dialect, get_dialect, list_dialects,
                  field_size_limit,
                  QUOTE_MINIMAL, QUOTE_ALL, QUOTE_NONNUMERIC, QUOTE_NONE,
                  QUOTE_STRINGS, QUOTE_NOTNULL)
from _csv import Dialect as _Dialect

__all__ = ["QUOTE_MINIMAL", "QUOTE_ALL", "QUOTE_NONNUMERIC", "QUOTE_NONE",
           "QUOTE_STRINGS", "QUOTE_NOTNULL",
           "Error", "Dialect", "excel", "excel_tab",
           "field_size_limit", "reader", "writer",
           "register_dialect", "get_dialect", "list_dialects", "Sniffer",
           "unregister_dialect", "DictReader", "DictWriter",
           "unix_dialect"]


def __getattr__(name):
    if name == "__version__":
        import warnings
        warnings.warn(
            "'__version__' is deprecated and slated for removal in "
            "Python 3.20",
            DeprecationWarning, stacklevel=2)
        return "1.0"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


class Dialect:
    """Base class for dialects described as classes.

    Subclasses set the format attributes as class attributes; creating an
    instance checks that they form a valid dialect (raising Error if not).
    """
    _name = ""
    _valid = False
    # format settings, filled in by subclasses
    delimiter = None
    quotechar = None
    escapechar = None
    doublequote = None
    skipinitialspace = None
    lineterminator = None
    quoting = None

    def __init__(self):
        if type(self) is not Dialect:
            self._valid = True
        self._validate()

    def _validate(self):
        try:
            _Dialect(self)
        except TypeError as exc:
            # report bad settings as csv.Error
            raise Error(str(exc)) from None


class excel(Dialect):
    """Format of CSV files written by Excel."""
    delimiter = ','
    quotechar = '"'
    doublequote = True
    skipinitialspace = False
    lineterminator = '\r\n'
    quoting = QUOTE_MINIMAL


register_dialect("excel", excel)


class excel_tab(excel):
    """Format of tab-separated files written by Excel."""
    delimiter = '\t'


register_dialect("excel-tab", excel_tab)


class unix_dialect(Dialect):
    """Format of CSV files usually produced on UNIX systems (every field
    quoted, '\\n' line terminator)."""
    delimiter = ','
    quotechar = '"'
    doublequote = True
    skipinitialspace = False
    lineterminator = '\n'
    quoting = QUOTE_ALL


register_dialect("unix", unix_dialect)


def _materialize(fieldnames):
    # one-shot iterators are turned into lists so they can be reused
    if fieldnames is not None and iter(fieldnames) is fieldnames:
        return list(fieldnames)
    return fieldnames


class DictReader:
    """Iterate over the rows of a CSV file as dictionaries.

    Keys come from *fieldnames*, or from the first row when fieldnames is
    None.  Surplus values of a long row are collected in a list under
    *restkey*; missing values of a short row are filled with *restval*.
    Blank rows are skipped.
    """

    def __init__(self, f, fieldnames=None, restkey=None, restval=None,
                 dialect="excel", *args, **kwds):
        self._fieldnames = _materialize(fieldnames)
        self.restkey = restkey
        self.restval = restval
        self.reader = reader(f, dialect, *args, **kwds)
        self.dialect = dialect
        self.line_num = 0

    def __iter__(self):
        return self

    @property
    def fieldnames(self):
        if self._fieldnames is None:
            try:
                self._fieldnames = next(self.reader)
            except StopIteration:
                pass
        self.line_num = self.reader.line_num
        return self._fieldnames

    @fieldnames.setter
    def fieldnames(self, value):
        self._fieldnames = value

    def __next__(self):
        if self.line_num == 0:
            # reading the header (if any) first
            self.fieldnames
        row = next(self.reader)
        while row == []:
            row = next(self.reader)
        self.line_num = self.reader.line_num
        names = self.fieldnames
        result = dict(zip(names, row))
        if len(row) > len(names):
            result[self.restkey] = row[len(names):]
        else:
            for name in names[len(row):]:
                result[name] = self.restval
        return result

    __class_getitem__ = classmethod(types.GenericAlias)


class DictWriter:
    """Write dictionaries as CSV rows, in the order given by *fieldnames*.

    Keys missing from a row are written as *restval*; keys not listed in
    fieldnames raise ValueError unless *extrasaction* is 'ignore'.
    """

    def __init__(self, f, fieldnames, restval="", extrasaction="raise",
                 dialect="excel", *args, **kwds):
        self.fieldnames = _materialize(fieldnames)
        self.restval = restval
        if extrasaction.lower() not in ("raise", "ignore"):
            raise ValueError("extrasaction (%s) must be 'raise' or 'ignore'"
                             % extrasaction)
        self.extrasaction = extrasaction
        self.writer = writer(f, dialect, *args, **kwds)

    def writeheader(self):
        """Write a row of the field names; returns what write() returned."""
        return self.writerow(dict(zip(self.fieldnames, self.fieldnames)))

    def _values(self, rowdict):
        if self.extrasaction.lower() == "raise":
            unknown = [key for key in rowdict if key not in self.fieldnames]
            if unknown:
                raise ValueError("dict contains fields not in fieldnames: "
                                 + ", ".join(repr(key) for key in unknown))
        return (rowdict.get(key, self.restval) for key in self.fieldnames)

    def writerow(self, rowdict):
        return self.writer.writerow(self._values(rowdict))

    def writerows(self, rowdicts):
        return self.writer.writerows(map(self._values, rowdicts))

    __class_getitem__ = classmethod(types.GenericAlias)


class Sniffer:
    """Guess the format of CSV data from a sample of it."""

    # delimiters preferred when several candidates fit equally well
    preferred = [',', '\t', ';', ' ', ':']

    def __init__(self):
        self.preferred = list(self.preferred)

    def sniff(self, sample, delimiters=None):
        """Return a Dialect subclass describing *sample*.

        *delimiters*, if given, is a string of the characters allowed as
        delimiter.
        """
        quotechar, doublequote, delimiter, skipinitialspace = \
            self._guess_quote_and_delimiter(sample, delimiters)
        if not delimiter:
            delimiter, skipinitialspace = \
                self._guess_delimiter(sample, delimiters)
        if not delimiter:
            raise Error("Could not determine delimiter")

        class dialect(Dialect):
            _name = "sniffed"
            lineterminator = '\r\n'
            quoting = QUOTE_MINIMAL

        dialect.doublequote = doublequote
        dialect.delimiter = delimiter
        dialect.quotechar = quotechar or '"'
        dialect.skipinitialspace = skipinitialspace
        return dialect

    @staticmethod
    def _delimiter_candidate(ch, delimiters, alnum=False):
        # only ASCII characters can be guessed as delimiters
        if ch in '\r\n"\'' or ord(ch) >= 127:
            return False
        if (ch.isalnum() or ch == '_') and not alnum:
            return False
        return delimiters is None or ch in delimiters

    def _guess_quote_and_delimiter(self, data, delimiters):
        """Look at quoted fields: the quote character used, the delimiter
        next to them, whether quotes are doubled inside them and whether
        a space follows the delimiter.

        Returns (quotechar, doublequote, delimiter, skipinitialspace);
        quotechar is '' when no quoted field was found, delimiter is ''
        when none could be seen next to a quoted field.
        """
        quotes = {}
        delims = {}
        spaced = {}
        doubled = {}
        size = len(data)
        pos = 0
        prev_delim = None       # delimiter preceding the current position
        at_field_start = True
        while pos < size:
            ch = data[pos]
            if ch in '\r\n':
                at_field_start = True
                prev_delim = None
                pos += 1
                continue
            if self._delimiter_candidate(ch, delimiters):
                # a delimiter candidate, optionally followed by one space
                start = pos + 1
                space = start < size and data[start] == ' ' and ch != ' '
                if space:
                    start += 1
                if start < size and data[start] in '"\'':
                    end, has_double = self._closing_quote(data, start)
                    if end is not None:
                        self._note_quoted(data, start, end, ch, space,
                                          has_double, delimiters, quotes,
                                          delims, spaced, doubled)
                        pos = end + 1
                        at_field_start = False
                        continue
                pos = start
                at_field_start = True
                continue
            if at_field_start and ch in '"\'':
                end, has_double = self._closing_quote(data, pos)
                if end is not None:
                    self._note_quoted(data, pos, end, None, False,
                                      has_double, delimiters, quotes,
                                      delims, spaced, doubled)
                    pos = end + 1
                    at_field_start = False
                    continue
            at_field_start = False
            pos += 1

        if not quotes:
            return '', False, None, False
        quotechar = max(quotes, key=lambda q: (quotes[q], q == '"'))
        delimiter = ''
        skipinitialspace = False
        if delims:
            delimiter = max(delims, key=lambda d: (delims[d],
                                                   d in self.preferred))
            skipinitialspace = spaced.get(delimiter, 0) == delims[delimiter]
        return (quotechar, bool(doubled.get(quotechar)), delimiter,
                skipinitialspace)

    @staticmethod
    def _closing_quote(data, start):
        """Index of the quote closing the field opened at *start* (doubled
        quotes are content), or None when the line ends first."""
        quote = data[start]
        size = len(data)
        pos = start + 1
        has_double = False
        while pos < size:
            ch = data[pos]
            if ch in '\r\n':
                return None, False
            if ch == quote:
                if pos + 1 < size and data[pos + 1] == quote:
                    has_double = True
                    pos += 2
                    continue
                return pos, has_double
            pos += 1
        return None, False

    def _note_quoted(self, data, start, end, before, space, has_double,
                     delimiters, quotes, delims, spaced, doubled):
        quote = data[start]
        quotes[quote] = quotes.get(quote, 0) + 1
        if has_double:
            doubled[quote] = doubled.get(quote, 0) + 1
        after = data[end + 1] if end + 1 < len(data) else None
        if before is not None:
            delims[before] = delims.get(before, 0) + 1
            if space:
                spaced[before] = spaced.get(before, 0) + 1
        elif after is not None and self._delimiter_candidate(after,
                                                             delimiters):
            delims[after] = delims.get(after, 0) + 1
            if end + 2 < len(data) and data[end + 2] == ' ' and after != ' ':
                spaced[after] = spaced.get(after, 0) + 1

    def _guess_delimiter(self, data, delimiters):
        """Pick the character that occurs the same number of times on (at
        least 90% of) the lines of *data*.

        Returns (delimiter, skipinitialspace); delimiter is '' when no
        character qualifies.
        """
        found = self._guess_delimiter_from(data, delimiters, False)
        if not found[0]:
            # no punctuation qualifies: letters and digits may separate too
            # ("aob" splits at "o"); among equals the highest character wins
            found = self._guess_delimiter_from(data, delimiters, True)
        return found

    def _guess_delimiter_from(self, data, delimiters, alnum):
        lines = [line for line in data.split('\n') if line]
        if not lines:
            return '', False
        per_line = []
        seen = {}
        for number, line in enumerate(lines):
            counts = {}
            for ch in line:
                if self._delimiter_candidate(ch, delimiters, alnum):
                    counts[ch] = counts.get(ch, 0) + 1
                    seen.setdefault(ch, number)
            per_line.append(counts)
        total = len(lines)
        scores = {}
        for ch in seen:
            histogram = {}
            for counts in per_line:
                n = counts.get(ch, 0)
                histogram[n] = histogram.get(n, 0) + 1
            best = max(histogram.values())
            modes = [n for n, freq in histogram.items() if freq == best]
            if len(modes) != 1 or modes[0] == 0:
                continue        # no single typical count per line
            scores[ch] = best / total
        candidates = []
        threshold = 1.0
        while not candidates and threshold >= 0.9 - 1e-9:
            candidates = [ch for ch, score in scores.items()
                          if score >= threshold - 1e-9]
            threshold -= 0.01
        if not candidates:
            return '', False
        if len(candidates) == 1:
            delimiter = candidates[0]
        else:
            for ch in self.preferred:
                if ch in candidates:
                    delimiter = ch
                    break
            else:
                if alnum:
                    best = max(scores[ch] for ch in candidates)
                    delimiter = max(ch for ch in candidates
                                    if scores[ch] == best)
                else:
                    delimiter = min(candidates, key=lambda ch: (-scores[ch],
                                                                seen[ch]))
        occurrences = data.count(delimiter)
        followed = data.count(delimiter + ' ')
        return delimiter, occurrences == followed and delimiter != ' '

    def has_header(self, sample):
        """Guess whether the first row of *sample* is a header.

        Every column whose values all have the same type (a number, or text
        of one fixed length) votes: a first-row value that does not fit the
        column's type counts for a header, one that fits counts against.
        """
        rows = reader(StringIO(sample), self.sniff(sample))
        header = next(rows)
        width = len(header)
        kinds = dict.fromkeys(range(width))
        checked = 0
        for row in rows:
            if checked > 20:
                break
            checked += 1
            if len(row) != width:
                continue
            for column in list(kinds):
                kind = self._kind(row[column])
                if kinds[column] is None:
                    kinds[column] = kind
                elif kind != kinds[column]:
                    del kinds[column]
        votes = 0
        for column, kind in kinds.items():
            if kind is None:
                continue
            if kind == 'number':
                fits = self._kind(header[column]) == 'number'
            else:
                fits = len(header[column]) == kind
            votes += -1 if fits else 1
        return votes > 0

    @staticmethod
    def _kind(value):
        """'number' for values complex() accepts, else the text length."""
        try:
            complex(value)
        except (ValueError, OverflowError):
            return len(value)
        return 'number'
