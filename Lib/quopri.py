"""Conversions to/from quoted-printable transport encoding (RFC 1521)."""

__all__ = ["encode", "decode", "encodestring", "decodestring"]

ESCAPE = b'='
MAXLINESIZE = 76
HEX = b'0123456789ABCDEF'
EMPTYSTRING = b''

try:
    from binascii import a2b_qp, b2a_qp
except ImportError:
    a2b_qp = None
    b2a_qp = None


def needsquoting(c, quotetabs, header):
    """Decide whether the byte value *c* (a length-1 bytes object) has to be
    written as an escape sequence.

    Spaces and tabs are quoted only if *quotetabs* is true; in *header* mode
    the underscore must be quoted because it stands for a space.
    """
    assert isinstance(c, bytes)
    if c in b' \t':
        return quotetabs
    if c == b'_':
        return header
    return c == ESCAPE or not (b' ' <= c <= b'~')


def quote(c):
    """Return the escape sequence for the single byte *c*."""
    assert isinstance(c, bytes) and len(c) == 1
    value = ord(c)
    return ESCAPE + bytes((HEX[value >> 4], HEX[value & 15]))


def _encode_line(body, quotetabs, header):
    # Encode the bytes of one line (without its line ending) into a list of
    # output tokens; every token is either one byte or a 3-byte escape.
    tokens = []
    last = len(body) - 1
    for pos in range(len(body)):
        c = body[pos:pos + 1]
        if header and c == b' ':
            tokens.append(b'_')
        elif needsquoting(c, quotetabs, header):
            tokens.append(quote(c))
        elif pos == last and c in b' \t':
            # whitespace at the end of a line would be lost in transport
            tokens.append(quote(c))
        else:
            tokens.append(c)
    return tokens


def _wrap(tokens, write_line, lineend):
    # Emit the tokens, inserting soft line breaks so that no output line is
    # longer than MAXLINESIZE characters.
    total = sum(map(len, tokens))
    start = 0
    while total > MAXLINESIZE:
        width = 0
        end = start
        while end < len(tokens) and width + len(tokens[end]) <= MAXLINESIZE - 1:
            width += len(tokens[end])
            end += 1
        write_line(EMPTYSTRING.join(tokens[start:end]) + ESCAPE, b'\n')
        total -= width
        start = end
    write_line(EMPTYSTRING.join(tokens[start:]), lineend)


def encode(input, output, quotetabs, header=False):
    """Read binary data from *input* and write it quoted-printable encoded
    to *output* (both binary file objects)."""
    if b2a_qp is not None:
        data = input.read()
        output.write(b2a_qp(data, quotetabs=quotetabs, header=header))
        return

    def write_line(text, lineend):
        output.write(text + lineend)

    while True:
        line = input.readline()
        if not line:
            break
        lineend = b''
        if line.endswith(b'\n'):
            line = line[:-1]
            lineend = b'\n'
        _wrap(_encode_line(line, quotetabs, header), write_line, lineend)


def encodestring(s, quotetabs=False, header=False):
    if b2a_qp is not None:
        return b2a_qp(s, quotetabs=quotetabs, header=header)
    from io import BytesIO
    infp = BytesIO(s)
    outfp = BytesIO()
    encode(infp, outfp, quotetabs, header)
    return outfp.getvalue()


def ishex(c):
    """Return true if the byte *c* is a hexadecimal digit."""
    assert isinstance(c, bytes)
    return c != b'' and c in b'0123456789abcdefABCDEF'


def unhex(s):
    """Return the integer value of a string of hex digits."""
    return int(s, 16)


def decode(input, output, header=False):
    """Read quoted-printable data from *input* and write the decoded bytes
    to *output* (both binary file objects)."""
    if a2b_qp is not None:
        data = input.read()
        output.write(a2b_qp(data, header=header))
        return

    pending = bytearray()
    while True:
        line = input.readline()
        if not line:
            break
        hard_break = line.endswith(b'\n')
        if hard_break:
            line = line[:-1]
        line = line.rstrip(b' \t\r')
        pos = 0
        n = len(line)
        soft_break = False
        while pos < n:
            c = line[pos:pos + 1]
            if c == b'_' and header:
                pending += b' '
                pos += 1
            elif c != ESCAPE:
                pending += c
                pos += 1
            elif pos + 1 == n:
                soft_break = True            # '=' at the end of the line
                pos += 1
            elif line[pos + 1:pos + 2] == ESCAPE:
                pending += ESCAPE
                pos += 2
            elif (pos + 2 < n + 1 and ishex(line[pos + 1:pos + 2])
                  and ishex(line[pos + 2:pos + 3])):
                pending.append(unhex(line[pos + 1:pos + 3]))
                pos += 3
            else:
                pending += c                 # a stray '=' is kept
                pos += 1
        if hard_break and not soft_break:
            pending += b'\n'
        output.write(bytes(pending))
        pending.clear()


def decodestring(s, header=False):
    if a2b_qp is not None:
        return a2b_qp(s, header=header)
    from io import BytesIO
    infp = BytesIO(s)
    outfp = BytesIO()
    decode(infp, outfp, header=header)
    return outfp.getvalue()


def main():
    import sys
    import getopt
    try:
        opts, args = getopt.getopt(sys.argv[1:], 'td')
    except getopt.error as msg:
        sys.stdout = sys.stderr
        print(msg)
        print("usage: quopri [-t | -d] [file] ...")
        print("-t: quote tabs")
        print("-d: decode; default encode")
        sys.exit(2)
    deco = False
    tabs = False
    for o, a in opts:
        if o == '-t':
            tabs = True
        if o == '-d':
            deco = True
    if tabs and deco:
        sys.stdout = sys.stderr
        print("-t and -d are mutually exclusive")
        sys.exit(2)
    if not args:
        args = ['-']
    status = 0
    for name in args:
        if name == '-':
            fp = sys.stdin.buffer
        else:
            try:
                fp = open(name, "rb")
            except OSError as exc:
                sys.stderr.write("%s: can't open (%s)\n" % (name, exc))
                status = 1
                continue
        try:
            if deco:
                decode(fp, sys.stdout.buffer)
            else:
                encode(fp, sys.stdout.buffer, tabs)
        finally:
            if name != '-':
                fp.close()
    if status:
        sys.exit(status)


if __name__ == '__main__':
    main()
