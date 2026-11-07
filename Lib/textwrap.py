"""Text wrapping and filling (wrap, fill, shorten, dedent, indent)."""

import re

__all__ = ['TextWrapper', 'wrap', 'fill', 'dedent', 'indent', 'shorten']

# Only ASCII whitespace separates words; e.g. NO-BREAK SPACE does not.
_whitespace = '\t\n\x0b\x0c\r '


class TextWrapper:
    """
    Object for wrapping/filling text.  The public interface consists of
    the wrap() and fill() methods; the other methods are just there for
    subclasses to override in order to tweak the default behaviour.
    If you want to completely replace the main wrapping algorithm,
    you'll probably have to override _wrap_chunks().

    Attributes (all settable as constructor keywords):
      width (default 70), initial_indent, subsequent_indent,
      expand_tabs, tabsize, replace_whitespace, fix_sentence_endings,
      break_long_words, drop_whitespace, break_on_hyphens,
      max_lines, placeholder (' [...]').
    """

    unicode_whitespace_trans = dict.fromkeys(map(ord, _whitespace), ord(' '))

    # Chunking.  Each chunk is a whitespace run or a word piece; words are
    # split after hyphens that sit between letters, and em-dashes ('--')
    # between words become separate chunks.
    word_punct = r'[\w!"\'&.,?]'
    letter = r'[^\d\W]'
    whitespace = r'[%s]' % re.escape(_whitespace)
    nowhitespace = '[^' + whitespace[1:]
    wordsep_re = re.compile(r'''
        ( {ws}+                                   # whitespace run
        | (?<={wp}) -{{2,}} (?=\w)                # em-dash between words
        | {nws}+? (?:                             # word piece, ending at:
              -(?: (?<={lt}{{2}}-) | (?<={lt}-{lt}-))
               (?= {lt} -? {lt})                  #   a hyphen inside a word
            | (?={ws}|\Z)                         #   the end of the word
            | (?<={wp}) (?=-{{2,}}\w)             #   an em-dash
            )
        )'''.format(wp=word_punct, lt=letter, ws=whitespace, nws=nowhitespace),
        re.VERBOSE)
    del word_punct, letter, nowhitespace

    # Chunking when break_on_hyphens is false: whitespace only.
    wordsep_simple_re = re.compile(r'(%s+)' % whitespace)
    del whitespace

    # A lowercase letter, sentence-ending punctuation and an optional
    # closing quote at the end of a chunk.
    sentence_end_re = re.compile(r'[a-z]'
                                 r'[\.\!\?]'
                                 r'[\"\']?'
                                 r'\z')

    def __init__(self,
                 width=70,
                 initial_indent="",
                 subsequent_indent="",
                 expand_tabs=True,
                 replace_whitespace=True,
                 fix_sentence_endings=False,
                 break_long_words=True,
                 drop_whitespace=True,
                 break_on_hyphens=True,
                 tabsize=8,
                 *,
                 max_lines=None,
                 placeholder=' [...]'):
        self.width = width
        self.initial_indent = initial_indent
        self.subsequent_indent = subsequent_indent
        self.expand_tabs = expand_tabs
        self.replace_whitespace = replace_whitespace
        self.fix_sentence_endings = fix_sentence_endings
        self.break_long_words = break_long_words
        self.drop_whitespace = drop_whitespace
        self.break_on_hyphens = break_on_hyphens
        self.tabsize = tabsize
        self.max_lines = max_lines
        self.placeholder = placeholder

    # -- private methods -------------------------------------------------

    def _munge_whitespace(self, text):
        """Expand tabs and turn every whitespace character into a space
        (as configured)."""
        if self.expand_tabs:
            text = text.expandtabs(self.tabsize)
        if self.replace_whitespace:
            text = text.translate(self.unicode_whitespace_trans)
        return text

    def _split(self, text):
        """Split text into chunks (words, word pieces, whitespace runs)."""
        if self.break_on_hyphens is True:
            chunks = self.wordsep_re.split(text)
        else:
            chunks = self.wordsep_simple_re.split(text)
        return [c for c in chunks if c]

    def _fix_sentence_endings(self, chunks):
        """Make sure there are two spaces after each sentence end."""
        i = 0
        patsearch = self.sentence_end_re.search
        while i < len(chunks) - 1:
            if chunks[i + 1] == " " and patsearch(chunks[i]):
                chunks[i + 1] = "  "
                i += 2
            else:
                i += 1

    def _handle_long_word(self, reversed_chunks, cur_line, cur_len, width):
        """Deal with a chunk too long for any line."""
        space_left = 1 if width < 1 else width - cur_len

        if self.break_long_words and space_left > 0:
            end = space_left
            chunk = reversed_chunks[-1]
            if self.break_on_hyphens and len(chunk) > space_left:
                # Prefer breaking after the last hyphen that fits, if it is
                # preceded by something other than hyphens.
                hyphen = chunk.rfind('-', 0, space_left)
                if hyphen > 0 and any(c != '-' for c in chunk[:hyphen]):
                    end = hyphen + 1
            cur_line.append(chunk[:end])
            reversed_chunks[-1] = chunk[end:]
        elif not cur_line and not self.break_long_words:
            # Put the whole word on its own (overlong) line.
            cur_line.append(reversed_chunks.pop())
        # Otherwise the word goes to the next line (a full line has no room
        # for even a piece of it).

    def _wrap_chunks(self, chunks):
        """Assemble chunks into lines of at most self.width characters."""
        lines = []
        if self.width <= 0:
            raise ValueError("invalid width %r (must be > 0)" % self.width)
        if self.max_lines is not None:
            if self.max_lines > 1:
                indent = self.subsequent_indent
            else:
                indent = self.initial_indent
            if len(indent) + len(self.placeholder.lstrip()) > self.width:
                raise ValueError("placeholder too large for max width")

        # Pop chunks from the end of the reversed list.
        chunks.reverse()

        while chunks:
            cur_line = []
            cur_len = 0

            indent = self.subsequent_indent if lines else self.initial_indent
            width = self.width - len(indent)

            # Whitespace at the start of a line (except the first) is dropped.
            if self.drop_whitespace and chunks[-1].strip() == '' and lines:
                del chunks[-1]

            while chunks:
                n = len(chunks[-1])
                if cur_len + n <= width:
                    cur_line.append(chunks.pop())
                    cur_len += n
                else:
                    break

            if chunks and len(chunks[-1]) > width:
                self._handle_long_word(chunks, cur_line, cur_len, width)
                cur_len = sum(map(len, cur_line))

            # Trailing whitespace of a line is dropped.
            if self.drop_whitespace and cur_line and cur_line[-1].strip() == '':
                cur_len -= len(cur_line[-1])
                del cur_line[-1]

            if cur_line:
                if (self.max_lines is None or
                        len(lines) + 1 < self.max_lines or
                        (not chunks or
                         self.drop_whitespace and
                         len(chunks) == 1 and
                         not chunks[0].strip()) and cur_len <= width):
                    lines.append(indent + ''.join(cur_line))
                else:
                    # Last allowed line: append the placeholder, removing
                    # chunks until it fits.
                    while cur_line:
                        if (cur_line[-1].strip() and
                                cur_len + len(self.placeholder) <= width):
                            cur_line.append(self.placeholder)
                            lines.append(indent + ''.join(cur_line))
                            break
                        cur_len -= len(cur_line[-1])
                        del cur_line[-1]
                    else:
                        if lines:
                            prev_line = lines[-1].rstrip()
                            if (len(prev_line) + len(self.placeholder) <=
                                    self.width):
                                lines[-1] = prev_line + self.placeholder
                                break
                        lines.append(indent + self.placeholder.lstrip())
                    break

        return lines

    def _split_chunks(self, text):
        text = self._munge_whitespace(text)
        return self._split(text)

    # -- public interface ------------------------------------------------

    def wrap(self, text):
        """Wrap text into a list of lines (without final newlines)."""
        chunks = self._split_chunks(text)
        if self.fix_sentence_endings:
            self._fix_sentence_endings(chunks)
        return self._wrap_chunks(chunks)

    def fill(self, text):
        """Wrap text and return the lines joined with newlines."""
        return "\n".join(self.wrap(text))


# -- convenience functions --------------------------------------------------

def wrap(text, width=70, **kwargs):
    """Wrap a paragraph into lines of at most width characters; see
    TextWrapper for the keyword arguments."""
    w = TextWrapper(width=width, **kwargs)
    return w.wrap(text)


def fill(text, width=70, **kwargs):
    """Like wrap(), but return a single newline-joined string."""
    w = TextWrapper(width=width, **kwargs)
    return w.fill(text)


def shorten(text, width, **kwargs):
    """Collapse whitespace and truncate text to fit in width characters,
    replacing the dropped words with the placeholder."""
    w = TextWrapper(width=width, max_lines=1, **kwargs)
    return w.fill(' '.join(text.strip().split()))


# -- dedent / indent -----------------------------------------------------------

def dedent(text):
    """Remove the whitespace prefix common to all non-blank lines.

    Lines consisting only of whitespace become empty; tabs and spaces are
    not considered equivalent.
    """
    try:
        lines = text.split('\n')
    except (AttributeError, TypeError):
        msg = f'expected str object, not {type(text).__qualname__!r}'
        raise TypeError(msg) from None

    non_blank = [l for l in lines if l and not l.isspace()]
    # The common prefix of all lines is the common prefix of the smallest
    # and the largest one.
    lo = min(non_blank, default='')
    hi = max(non_blank, default='')
    margin = 0
    for margin, c in enumerate(lo):
        if c != hi[margin] or c not in ' \t':
            break

    return '\n'.join([l[margin:] if not l.isspace() else '' for l in lines])


def indent(text, prefix, predicate=None):
    """Add prefix to the lines of text selected by predicate (default:
    lines that are not whitespace-only)."""
    prefixed = []
    if predicate is None:
        for line in text.splitlines(True):
            if not line.isspace():
                prefixed.append(prefix)
            prefixed.append(line)
    else:
        for line in text.splitlines(True):
            if predicate(line):
                prefixed.append(prefix)
            prefixed.append(line)
    return ''.join(prefixed)


if __name__ == "__main__":
    print(dedent("Hello there.\n  This is indented."))
