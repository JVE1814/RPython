"""Calendar computations and plain-text / HTML calendar rendering.

Weeks start on Monday unless setfirstweekday() (or a Calendar's
firstweekday) says otherwise; weekdays are numbered 0 (Monday) to 6
(Sunday).  Dates follow the proleptic Gregorian calendar in both
directions, so year 0 and negative years work as well.

``python -m calendar [year [month]]`` prints a calendar.
"""

import sys
import datetime
from enum import IntEnum, global_enum
import locale as _locale
from itertools import repeat

__all__ = ["IllegalMonthError", "IllegalWeekdayError", "setfirstweekday",
           "firstweekday", "isleap", "leapdays", "weekday", "monthrange",
           "monthcalendar", "prmonth", "month", "prcal", "calendar",
           "timegm", "month_name", "month_abbr", "day_name", "day_abbr",
           "standalone_month_name", "standalone_month_abbr",
           "Calendar", "TextCalendar", "HTMLCalendar", "LocaleTextCalendar",
           "LocaleHTMLCalendar", "weekheader",
           "Day", "Month", "JANUARY", "FEBRUARY", "MARCH",
           "APRIL", "MAY", "JUNE", "JULY",
           "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER",
           "MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY",
           "SATURDAY", "SUNDAY"]

class IllegalMonthError(ValueError, IndexError):
    def __init__(self, month):
        self.month = month

    def __str__(self):
        return f"bad month number {self.month!r}; must be 1-12"


class IllegalWeekdayError(ValueError):
    def __init__(self, weekday):
        self.weekday = weekday

    def __str__(self):
        return (f"bad weekday number {self.weekday!r}; "
                f"must be 0 (Monday) to 6 (Sunday)")


def __getattr__(name):
    # the old mixed-case month constants
    if name in ('January', 'February'):
        import warnings
        warnings.warn(f"The '{name}' attribute is deprecated, use "
                      f"'{name.upper()}' instead",
                      DeprecationWarning, stacklevel=2)
        return 1 if name == 'January' else 2
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


@global_enum
class Month(IntEnum):
    JANUARY = 1
    FEBRUARY = 2
    MARCH = 3
    APRIL = 4
    MAY = 5
    JUNE = 6
    JULY = 7
    AUGUST = 8
    SEPTEMBER = 9
    OCTOBER = 10
    NOVEMBER = 11
    DECEMBER = 12


@global_enum
class Day(IntEnum):
    MONDAY = 0
    TUESDAY = 1
    WEDNESDAY = 2
    THURSDAY = 3
    FRIDAY = 4
    SATURDAY = 5
    SUNDAY = 6


# days per month in a common year (index 0 unused)
mdays = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]

# days before the first of each month in a common year
_DAYS_BEFORE = (0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334)


# --------------------------------------------------------------------------
# localised names

class _NameTable:
    """Read-only sequence of names produced by strftime() on access, so the
    values follow the current LC_TIME locale."""

    def __init__(self, length, make):
        self._length = length
        self._make = make

    def __len__(self):
        return self._length

    def __getitem__(self, index):
        if isinstance(index, slice):
            return [self._make(i) for i in range(self._length)[index]]
        if not -self._length <= index < self._length:
            raise IndexError(f"{index} is out of range")
        return self._make(index % self._length)

    def __iter__(self):
        return (self._make(i) for i in range(self._length))


def _weekday_namer(fmt):
    # 2001-01-01 was a Monday
    return lambda i: datetime.date(2001, 1, i + 1).strftime(fmt)


def _month_namer(fmt, fallback=None):
    def name(i):
        if i == 0:
            return ''
        when = datetime.date(2001, i, 1)
        text = when.strftime(fmt)
        if fallback is not None and (not text or '%' in text):
            # the platform has no standalone (nominative) month names
            text = when.strftime(fallback)
        return text
    return name


day_name = _NameTable(7, _weekday_namer('%A'))
day_abbr = _NameTable(7, _weekday_namer('%a'))
month_name = _NameTable(13, _month_namer('%B'))
month_abbr = _NameTable(13, _month_namer('%b'))
standalone_month_name = _NameTable(13, _month_namer('%OB', '%B'))
standalone_month_abbr = _NameTable(13, _month_namer('%Ob', '%b'))


# --------------------------------------------------------------------------
# arithmetic

def isleap(year):
    """True for leap years of the Gregorian calendar."""
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def leapdays(y1, y2):
    """Number of leap years in range(y1, y2)."""
    def through(y):           # leap years in [1, y] (negative for y < 0)
        return y // 4 - y // 100 + y // 400
    return through(y2 - 1) - through(y1 - 1)


def _ordinal(year, month, day):
    """Days since 0001-01-00 (proleptic Gregorian; any integer year)."""
    before = year - 1
    days = before * 365 + before // 4 - before // 100 + before // 400
    days += _DAYS_BEFORE[month] + (month > 2 and isleap(year))
    return days + day


def _check_month(month):
    if not 1 <= month <= 12:
        raise IllegalMonthError(month)


def weekday(year, month, day):
    """Day of the week (0 is Monday) of the given date."""
    _check_month(month)
    return (_ordinal(year, month, day) + 6) % 7


def monthlen(year, month):
    return mdays[month] + (month == FEBRUARY and isleap(year))


def prevmonth(year, month):
    return (year - 1, 12) if month == 1 else (year, month - 1)


def nextmonth(year, month):
    return (year + 1, 1) if month == 12 else (year, month + 1)


def monthrange(year, month):
    """(weekday of the first day, number of days) for the month."""
    _check_month(month)
    return weekday(year, month, 1), monthlen(year, month)


EPOCH = 1970
_EPOCH_ORDINAL = _ordinal(EPOCH, 1, 1)


def timegm(tuple):
    """Inverse of time.gmtime(): seconds since the epoch for a UTC time
    tuple (year, month, day, hour, minute, second, ...)."""
    year, month, day, hour, minute, second = tuple[:6]
    days = _ordinal(year, month, 1) - _EPOCH_ORDINAL + day - 1
    return ((days * 24 + hour) * 60 + minute) * 60 + second


# --------------------------------------------------------------------------
# calendar data

class Calendar(object):
    """Produces the data of month and year calendars (weeks of day numbers,
    dates, ...) without formatting them."""

    def __init__(self, firstweekday=0):
        self.firstweekday = firstweekday

    def getfirstweekday(self):
        return self._firstweekday % 7

    def setfirstweekday(self, firstweekday):
        self._firstweekday = firstweekday

    firstweekday = property(getfirstweekday, setfirstweekday)

    def iterweekdays(self):
        """The seven weekday numbers in display order."""
        start = self.firstweekday
        for offset in range(7):
            yield (start + offset) % 7

    def _padding(self, year, month):
        """(blank days before day 1, days in month, blank days after)."""
        first, length = monthrange(year, month)
        lead = (first - self.firstweekday) % 7
        trail = -(lead + length) % 7
        return lead, length, trail

    def itermonthdays(self, year, month):
        """Day numbers of the full weeks covering the month; days of the
        neighbouring months are 0."""
        lead, length, trail = self._padding(year, month)
        yield from repeat(0, lead)
        yield from range(1, length + 1)
        yield from repeat(0, trail)

    def itermonthdays2(self, year, month):
        """Like itermonthdays() but yields (day number, weekday) pairs."""
        start = self.firstweekday
        for position, day in enumerate(self.itermonthdays(year, month)):
            yield day, (start + position) % 7

    def itermonthdays3(self, year, month):
        """(year, month, day) for every day of the full weeks covering the
        month, including the neighbouring months' days."""
        lead, length, trail = self._padding(year, month)
        py, pm = prevmonth(year, month)
        plen = monthlen(py, pm)
        for day in range(plen - lead + 1, plen + 1):
            yield py, pm, day
        for day in range(1, length + 1):
            yield year, month, day
        ny, nm = nextmonth(year, month)
        for day in range(1, trail + 1):
            yield ny, nm, day

    def itermonthdays4(self, year, month):
        """Like itermonthdays3() with the weekday appended."""
        start = self.firstweekday
        for position, (y, m, d) in enumerate(
                self.itermonthdays3(year, month)):
            yield y, m, d, (start + position) % 7

    def itermonthdates(self, year, month):
        """datetime.date objects for the full weeks covering the month."""
        for y, m, d in self.itermonthdays3(year, month):
            yield datetime.date(y, m, d)

    @staticmethod
    def _weeks(items):
        items = list(items)
        return [items[i:i + 7] for i in range(0, len(items), 7)]

    def monthdatescalendar(self, year, month):
        """The month as a list of weeks of seven datetime.date objects."""
        return self._weeks(self.itermonthdates(year, month))

    def monthdays2calendar(self, year, month):
        """The month as a list of weeks of (day number, weekday) pairs."""
        return self._weeks(self.itermonthdays2(year, month))

    def monthdayscalendar(self, year, month):
        """The month as a list of weeks of day numbers (0 for padding)."""
        return self._weeks(self.itermonthdays(year, month))

    def _year_rows(self, monthly, year, width):
        months = [monthly(year, m) for m in range(1, 13)]
        return [months[i:i + width] for i in range(0, 12, width)]

    def yeardatescalendar(self, year, width=3):
        """The year as rows of *width* months as monthdatescalendar()
        returns them."""
        return self._year_rows(self.monthdatescalendar, year, width)

    def yeardays2calendar(self, year, width=3):
        """Like yeardatescalendar() with monthdays2calendar() months."""
        return self._year_rows(self.monthdays2calendar, year, width)

    def yeardayscalendar(self, year, width=3):
        """Like yeardatescalendar() with monthdayscalendar() months."""
        return self._year_rows(self.monthdayscalendar, year, width)


# --------------------------------------------------------------------------
# text output

class TextCalendar(Calendar):
    """Calendars as monospaced text."""

    def prweek(self, theweek, width):
        """Print one week (see formatweek()) without a newline."""
        print(self.formatweek(theweek, width), end='')

    def formatday(self, day, weekday, width):
        """One day cell; blank for day 0."""
        text = '' if day == 0 else '%2i' % day
        return text.center(width)

    def formatweek(self, theweek, width):
        """A week of (day, weekday) pairs as one line."""
        return ' '.join(self.formatday(day, wd, width) for day, wd in theweek)

    def formatweekday(self, day, width):
        """Weekday name fitted to *width* (full names when they all fit)."""
        if width >= max(len(name) for name in day_name):
            names = day_name
        else:
            names = day_abbr
        return names[day][:width].center(width)

    def formatweekheader(self, width):
        """The weekday names of one week, in display order."""
        return ' '.join(self.formatweekday(day, width)
                        for day in self.iterweekdays())

    def formatmonthname(self, theyear, themonth, width, withyear=True):
        """Month title centred in *width*."""
        _check_month(themonth)
        title = month_name[themonth]
        if withyear:
            title = f"{title} {theyear!r}"
        return title.center(width)

    def prmonth(self, theyear, themonth, w=0, l=0):
        """Print formatmonth()."""
        print(self.formatmonth(theyear, themonth, w, l), end='')

    def _month_lines(self, theyear, themonth, w, withyear):
        """Lines of a month block, all unstripped, title first."""
        width = 7 * (w + 1) - 1
        lines = [self.formatmonthname(theyear, themonth, width, withyear),
                 self.formatweekheader(w)]
        lines.extend(self.formatweek(week, w)
                     for week in self.monthdays2calendar(theyear, themonth))
        return lines

    def formatmonth(self, theyear, themonth, w=0, l=0):
        """The month as text; *w* is the day column width, *l* the number
        of lines per week."""
        _check_month(themonth)
        w = max(2, w)
        newline = '\n' * max(1, l)
        lines = self._month_lines(theyear, themonth, w, True)
        return ''.join(line.rstrip() + newline for line in lines)

    def formatyear(self, theyear, w=2, l=1, c=6, m=3):
        """The year as text, *m* months per row separated by *c* spaces."""
        w = max(2, w)
        newline = '\n' * max(1, l)
        gap = ' ' * max(2, c)
        colwidth = 7 * (w + 1) - 1
        lines = [repr(theyear).center(colwidth * m + len(gap) * (m - 1)), '']
        for first in range(1, 13, m):
            months = range(first, min(first + m, 13))
            blocks = [self._month_lines(theyear, k, w, False)
                      for k in months]
            depth = max(len(block) for block in blocks)
            for row in range(depth):
                cells = [block[row] if row < len(block) else ''
                         for block in blocks]
                lines.append(gap.join(cell.center(colwidth)
                                      for cell in cells))
            lines.append('')
        lines.pop()           # no blank line after the last row
        return ''.join(line.rstrip() + newline for line in lines)

    def pryear(self, theyear, w=0, l=0, c=6, m=3):
        """Print formatyear()."""
        print(self.formatyear(theyear, w, l, c, m), end='')


# --------------------------------------------------------------------------
# HTML output

class HTMLCalendar(Calendar):
    """Calendars as HTML tables."""

    # CSS classes of the day cells, Monday first
    cssclasses = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
    # CSS classes of the weekday header cells
    cssclasses_weekday_head = cssclasses
    # CSS class of padding cells
    cssclass_noday = "noday"
    # CSS class of the month title cell
    cssclass_month_head = "month"
    # CSS class of a month table
    cssclass_month = "month"
    # CSS class of the year title cell
    cssclass_year_head = "year"
    # CSS class of the year table
    cssclass_year = "year"

    def formatday(self, day, weekday):
        """One <td> cell."""
        if day == 0:
            return f'<td class="{self.cssclass_noday}">&nbsp;</td>'
        return f'<td class="{self.cssclasses[weekday]}">{day}</td>'

    def formatweek(self, theweek):
        """One table row of (day, weekday) pairs."""
        cells = ''.join(self.formatday(day, wd) for day, wd in theweek)
        return f'<tr>{cells}</tr>'

    def formatweekday(self, day):
        """One weekday header cell."""
        return (f'<th class="{self.cssclasses_weekday_head[day]}">'
                f'{day_abbr[day]}</th>')

    def formatweekheader(self):
        """The row of weekday header cells."""
        cells = ''.join(self.formatweekday(day)
                        for day in self.iterweekdays())
        return f'<tr>{cells}</tr>'

    def formatmonthname(self, theyear, themonth, withyear=True):
        """The month title row."""
        _check_month(themonth)
        title = month_name[themonth]
        if withyear:
            title = f'{title} {theyear}'
        return (f'<tr><th colspan="7" class="{self.cssclass_month_head}">'
                f'{title}</th></tr>')

    def formatmonth(self, theyear, themonth, withyear=True):
        """The month as a <table>."""
        rows = [f'<table class="{self.cssclass_month}">',
                self.formatmonthname(theyear, themonth, withyear=withyear),
                self.formatweekheader()]
        rows.extend(self.formatweek(week)
                    for week in self.monthdays2calendar(theyear, themonth))
        rows.append('</table>')
        return ''.join(row + '\n' for row in rows)

    def formatyear(self, theyear, width=3):
        """The year as a <table> of month tables, *width* per row."""
        width = max(width, 1)
        parts = [f'<table class="{self.cssclass_year}">\n',
                 f'<tr><th colspan="{width}" class="{self.cssclass_year_head}">'
                 f'{theyear}</th></tr>']
        for first in range(1, 13, width):
            cells = ''.join(
                f'<td>{self.formatmonth(theyear, m, withyear=False)}</td>'
                for m in range(first, min(first + width, 13)))
            parts.append(f'<tr>{cells}</tr>')
        parts.append('</table>')
        return ''.join(parts)

    _STYLE = ('<style>\n'
              ':root { color-scheme: light dark; }\n'
              'table.year { border: solid; }\n'
              'table.year > tbody > tr > td '
              '{ border: solid; vertical-align: top; }\n'
              '</style>\n')

    def _page(self, title, body, css, encoding):
        if encoding is None:
            encoding = 'utf-8'
        head = ['<!DOCTYPE html>\n',
                '<html lang="en">\n',
                '<head>\n',
                f'<meta charset="{encoding}">\n',
                '<meta name="viewport" content="width=device-width, '
                'initial-scale=1">\n',
                f'<title>{title}</title>\n',
                self._STYLE]
        if css is not None:
            head.append(f'<link rel="stylesheet" href="{css}">\n')
        page = ''.join(head) + '</head>\n<body>\n' + body + \
            '</body>\n</html>\n'
        return page.encode(encoding, 'xmlcharrefreplace')

    def formatyearpage(self, theyear, width=3, css='calendar.css',
                       encoding=None):
        """A complete HTML page for the year, as bytes."""
        return self._page(f'Calendar for {theyear}',
                          self.formatyear(theyear, width), css, encoding)

    def formatmonthpage(self, theyear, themonth, css='calendar.css',
                        encoding=None):
        """A complete HTML page for one month, as bytes."""
        return self._page(f'Calendar for {theyear}',
                          self.formatmonth(theyear, themonth), css, encoding)


# --------------------------------------------------------------------------
# locale-aware variants

class different_locale:
    """Context manager switching LC_TIME temporarily."""

    def __init__(self, locale):
        self.locale = locale
        self.oldlocale = None

    def __enter__(self):
        self.oldlocale = _locale.setlocale(_locale.LC_TIME, None)
        _locale.setlocale(_locale.LC_TIME, self.locale)

    def __exit__(self, *exc_info):
        _locale.setlocale(_locale.LC_TIME, self.oldlocale)


def _current_time_locale():
    found = _locale.getlocale(_locale.LC_TIME)
    if found[0] is None:
        return 'C'
    return found


class LocaleTextCalendar(TextCalendar):
    """TextCalendar whose day and month names come from *locale* (the
    current LC_TIME locale when None)."""

    def __init__(self, firstweekday=0, locale=None):
        super().__init__(firstweekday)
        self.locale = _current_time_locale() if locale is None else locale

    def formatweekday(self, day, width):
        with different_locale(self.locale):
            return super().formatweekday(day, width)

    def formatmonthname(self, theyear, themonth, width, withyear=True):
        with different_locale(self.locale):
            return super().formatmonthname(theyear, themonth, width,
                                           withyear)


class LocaleHTMLCalendar(HTMLCalendar):
    """HTMLCalendar whose day and month names come from *locale* (the
    current LC_TIME locale when None)."""

    def __init__(self, firstweekday=0, locale=None):
        super().__init__(firstweekday)
        self.locale = _current_time_locale() if locale is None else locale

    def formatweekday(self, day):
        with different_locale(self.locale):
            return super().formatweekday(day)

    def formatmonthname(self, theyear, themonth, withyear=True):
        with different_locale(self.locale):
            return super().formatmonthname(theyear, themonth, withyear)


class _CLIDemoCalendar(TextCalendar):
    """Text calendar for the terminal: weekday headers in colour and the
    month and day of *highlight_day* highlighted."""

    def __init__(self, highlight_day=None, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.highlight_day = highlight_day

    @staticmethod
    def _marked(text, start):
        from _colorize import ANSIColors
        body = text.strip()
        if not body:
            return text
        left = text.index(body)
        return (text[:left] + start + body + ANSIColors.RESET +
                text[left + len(body):])

    def _today_in(self, year, month):
        day = self.highlight_day
        return day is not None and (day.year, day.month) == (year, month)

    def formatweekheader(self, width):
        from _colorize import ANSIColors
        return self._marked(super().formatweekheader(width),
                            ANSIColors.CYAN)

    def formatmonthname(self, theyear, themonth, width, withyear=True):
        from _colorize import ANSIColors
        text = super().formatmonthname(theyear, themonth, width, withyear)
        if self._today_in(theyear, themonth):
            text = self._marked(
                text, ANSIColors.BLACK + ANSIColors.BACKGROUND_YELLOW)
        return text

    def _month_lines(self, theyear, themonth, w, withyear):
        self._showing = (theyear, themonth)
        return super()._month_lines(theyear, themonth, w, withyear)

    def formatday(self, day, weekday, width):
        from _colorize import ANSIColors
        text = super().formatday(day, weekday, width)
        showing = getattr(self, '_showing', None)
        if (day and showing is not None and self._today_in(*showing)
                and day == self.highlight_day.day):
            text = self._marked(
                text, ANSIColors.BLACK + ANSIColors.BACKGROUND_YELLOW)
        return text


# --------------------------------------------------------------------------
# module-level shortcuts using a shared TextCalendar

c = TextCalendar()

firstweekday = c.getfirstweekday


def setfirstweekday(firstweekday):
    if not MONDAY <= firstweekday <= SUNDAY:
        raise IllegalWeekdayError(firstweekday)
    c.firstweekday = firstweekday


monthcalendar = c.monthdayscalendar
prweek = c.prweek
week = c.formatweek
weekheader = c.formatweekheader
prmonth = c.prmonth
month = c.formatmonth
calendar = c.formatyear
prcal = c.pryear

# spacing of month columns
_colwidth = 7 * 3 - 1
_spacing = 6


def format(cols, colwidth=_colwidth, spacing=_spacing):
    """Print the columns side by side, each centred in *colwidth*."""
    print(formatstring(cols, colwidth, spacing))


def formatstring(cols, colwidth=_colwidth, spacing=_spacing):
    """The columns side by side, each centred in *colwidth*."""
    return (' ' * spacing).join(col.center(colwidth) for col in cols)


# --------------------------------------------------------------------------
# command line

def main(args=None):
    import argparse
    parser = argparse.ArgumentParser(
        prog='python -m calendar',
        description='Print a text or HTML calendar.',
        color=True)
    text_group = parser.add_argument_group('text only arguments')
    html_group = parser.add_argument_group('html only arguments')
    text_group.add_argument('-w', '--width', type=int, default=2,
                            help='width of date column (default 2)')
    text_group.add_argument('-l', '--lines', type=int, default=1,
                            help='number of lines for each week (default 1)')
    text_group.add_argument('-s', '--spacing', type=int, default=6,
                            help='spacing between months (default 6)')
    text_group.add_argument('-m', '--months', type=int, default=3,
                            help='months per row (default 3)')
    html_group.add_argument('-c', '--css', default='calendar.css',
                            help='CSS to use for page')
    parser.add_argument('-L', '--locale', default=None,
                        help='locale to use for month and weekday names')
    parser.add_argument('-e', '--encoding', default=None,
                        help='encoding to use for output')
    parser.add_argument('-t', '--type', default='text',
                        choices=('text', 'html'),
                        help='output type (text or html)')
    parser.add_argument('-f', '--first-weekday', type=int, default=0,
                        help='weekday (0 is Monday, 6 is Sunday) to start '
                             'each week (default 0)')
    parser.add_argument('year', nargs='?', type=int,
                        help='year number')
    parser.add_argument('month', nargs='?', type=int,
                        help='month number (1-12, text only)')

    options = parser.parse_args(args)

    if options.locale and not options.encoding:
        parser.error('if --locale is specified --encoding is required')

    locale = (options.locale, options.encoding)
    today = datetime.date.today()
    year = today.year if options.year is None else options.year

    if options.type == 'html':
        if options.locale:
            cal = LocaleHTMLCalendar(locale=locale)
        else:
            cal = HTMLCalendar()
        cal.setfirstweekday(options.first_weekday)
        encoding = options.encoding or 'utf-8'
        if options.month is None:
            page = cal.formatyearpage(year, css=options.css,
                                      encoding=encoding)
        else:
            page = cal.formatmonthpage(year, options.month, css=options.css,
                                       encoding=encoding)
        sys.stdout.buffer.write(page)
        return

    if options.locale:
        cal = LocaleTextCalendar(locale=locale)
    else:
        from _colorize import can_colorize
        if can_colorize(file=sys.stdout):
            cal = _CLIDemoCalendar(highlight_day=today)
        else:
            cal = TextCalendar()
    cal.setfirstweekday(options.first_weekday)
    if options.month is None:
        result = cal.formatyear(year, options.width, options.lines,
                                options.spacing, options.months)
    else:
        try:
            result = cal.formatmonth(year, options.month, options.width,
                                     options.lines)
        except IllegalMonthError as exc:
            parser.error(str(exc))
    if options.encoding:
        sys.stdout.flush()
        sys.stdout.buffer.write(result.encode(options.encoding))
    else:
        sys.stdout.write(result)


if __name__ == '__main__':
    main()
