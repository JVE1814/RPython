"""A package for parsing, handling and generating email messages."""

__all__ = [
    'base64mime',
    'charset',
    'encoders',
    'errors',
    'feedparser',
    'generator',
    'header',
    'iterators',
    'message',
    'message_from_file',
    'message_from_binary_file',
    'message_from_string',
    'message_from_bytes',
    'mime',
    'parser',
    'quoprimime',
    'utils',
]


def message_from_string(s, *args, **kws):
    """Parse a str into a message object (arguments as for Parser)."""
    from email.parser import Parser
    return Parser(*args, **kws).parsestr(s)


def message_from_bytes(s, *args, **kws):
    """Parse bytes into a message object (arguments as for BytesParser)."""
    from email.parser import BytesParser
    return BytesParser(*args, **kws).parsebytes(s)


def message_from_file(fp, *args, **kws):
    """Parse an open text file into a message object."""
    from email.parser import Parser
    return Parser(*args, **kws).parse(fp)


def message_from_binary_file(fp, *args, **kws):
    """Parse an open binary file into a message object."""
    from email.parser import BytesParser
    return BytesParser(*args, **kws).parse(fp)
