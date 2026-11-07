"""Conversion between Windows path names and the path part of file: URLs.

Deprecated: urllib.request.pathname2url() and url2pathname() handle Windows
paths themselves.
"""

import warnings

warnings._deprecated(
    __name__,
    message=f"{warnings._DEPRECATED_MSG}; use 'urllib.request' instead",
    remove=(3, 19))


def _fs_codec():
    import sys
    return sys.getfilesystemencoding(), sys.getfilesystemencodeerrors()


def _split_drive_letter(url):
    """Return (drive, rest) when *url* starts with 'X:' or 'X|' followed by
    a slash or nothing; otherwise ('', url)."""
    if len(url) >= 2 and url[1] in ':|' and url[0] != '/' and url[2:3] in ('', '/'):
        return url[0] + ':', url[2:]
    return '', url


def url2pathname(url):
    """Turn the path part of a file: URL into a Windows path name.

    'C|/a/b' and '///C:/a/b' become 'C:\\a\\b'; '//server/share/x' becomes
    the UNC path '\\\\server\\share\\x'.
    """
    from urllib.parse import unquote

    if url[:2] == '//':
        slash = url.find('/', 2)
        if slash < 0:
            host, tail = url[2:], ''
        else:
            host, tail = url[2:slash], url[slash:]
        if not host:
            # '///path' or '////server/share': an empty authority.
            while tail[:3] == '///':
                tail = tail[1:]
            url = tail
        elif host.lower() == 'localhost':
            url = tail or '/'
        # any other host stays part of a UNC path

    # '/C:/x' -> 'C:/x'
    if url[:1] == '/' and url[2:3] in (':', '|') and url[1:2] != '/' and url[3:4] in ('', '/'):
        url = url[1:]
    drive, url = _split_drive_letter(url)

    encoding, errors = _fs_codec()
    # Separators are swapped before unquoting so that an encoded '%2F'
    # stays a forward slash.
    return drive + unquote(url.replace('/', '\\'), encoding=encoding, errors=errors)


def pathname2url(p):
    """Turn a Windows path name into the path part of a file: URL.

    'C:\\a\\b' becomes '///C:/a/b'; a UNC path '\\\\server\\share\\x' becomes
    '//server/share/x'.
    """
    from urllib.parse import quote

    p = p.replace('\\', '/')
    if p[:4] == '//?/':
        # Extended-length prefix: '\\?\C:\x' or '\\?\UNC\server\share'.
        p = p[4:]
        if p[:4].upper() == 'UNC/':
            p = '//' + p[4:]

    encoding, errors = _fs_codec()
    if p[1:2] == ':' and p[:1] != '/':
        drive, p = p[:2], p[2:]
        return '///' + drive + quote(p, encoding=encoding, errors=errors)
    url = quote(p, encoding=encoding, errors=errors)
    if url[:1] == '/' and url[:2] != '//':
        # Rooted but driveless: give it an empty authority so that the
        # first path segment cannot be mistaken for a host.
        url = '//' + url
    return url
