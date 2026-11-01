"""Map file names and URLs to MIME types (and back).

Module-level functions use a shared database that is built lazily from the
built-in tables plus the system files listed in ``knownfiles``:

guess_type(url, strict=True)          -- (type, encoding) for a URL
guess_file_type(path, strict=True)    -- (type, encoding) for a file path
guess_extension(type, strict=True)    -- preferred extension for a type
guess_all_extensions(type, strict=True)
add_type(type, ext, strict=True)      -- register a mapping
init(files=None)                      -- (re)build the shared database
read_mime_types(file)                 -- read a mime.types style file

With strict=False, a few widely used but unregistered types are also known.
"""

import sys

try:
    from _winapi import _mimetypes_read_windows_registry
except ImportError:
    _mimetypes_read_windows_registry = None

try:
    import winreg as _winreg
except ImportError:
    _winreg = None

__all__ = [
    "knownfiles", "inited", "MimeTypes",
    "guess_type", "guess_file_type", "guess_all_extensions",
    "guess_extension", "add_type", "init", "read_mime_types",
    "suffix_map", "encodings_map", "types_map", "common_types",
]

knownfiles = [
    "/etc/mime.types",
    "/etc/httpd/mime.types",                    # Mac OS X
    "/etc/httpd/conf/mime.types",               # Apache
    "/etc/apache/mime.types",                   # Apache 1
    "/etc/apache2/mime.types",                  # Apache 2
    "/usr/local/etc/httpd/conf/mime.types",
    "/usr/local/lib/netscape/mime.types",
    "/usr/local/etc/httpd/conf/mime.types",     # Apache 1.2
    "/usr/local/etc/mime.types",                # Apache 1.3
]

inited = False
_db = None


def _split_scheme(url):
    """Return (scheme, rest) if *url* starts with a URL scheme."""
    colon = url.find(':')
    if colon <= 0:
        return None, url
    scheme = url[:colon]
    if not (scheme[0].isascii() and scheme[0].isalpha()):
        return None, url
    for ch in scheme[1:]:
        if not (ch.isascii() and (ch.isalnum() or ch in '+-.')):
            return None, url
    return scheme.lower(), url[colon + 1:]


def _url_path(rest):
    """Extract the path component of the part of a URL after the scheme."""
    if rest.startswith('//'):
        slash = len(rest)
        for delim in '/?#':
            pos = rest.find(delim, 2)
            if pos >= 0 and pos < slash:
                slash = pos
        rest = rest[slash:]
    for delim in '#?':
        pos = rest.find(delim)
        if pos >= 0:
            rest = rest[:pos]
    return rest


class MimeTypes:
    """A MIME type database.

    Instances start from the built-in tables (they do not read the system
    files); read() and readfp() add more mappings from mime.types files.
    """

    def __init__(self, filenames=(), strict=True):
        if not inited:
            init()
        self.encodings_map = _encodings_map_default.copy()
        self.suffix_map = _suffix_map_default.copy()
        # index True: registered types, False: common unregistered ones
        self.types_map = ({}, {})
        self.types_map_inv = ({}, {})
        for ext, mime in _types_map_default.items():
            self.add_type(mime, ext, True)
        for ext, mime in _common_types_default.items():
            self.add_type(mime, ext, False)
        for name in filenames:
            self.read(name, strict)

    def add_type(self, type, ext, strict=True):
        """Map the extension *ext* (including the leading dot) to *type*.

        A type that already has extensions keeps them; the new extension is
        appended to the list used by guess_all_extensions().
        """
        if ext and not ext.startswith('.'):
            from warnings import _deprecated
            _deprecated(
                "Undotted extensions",
                "Using undotted extensions is deprecated and "
                "will raise a ValueError in Python {remove}",
                remove=(3, 16),
            )
        if not type:
            return
        self.types_map[strict][ext] = type
        extensions = self.types_map_inv[strict].setdefault(type, [])
        if ext not in extensions:
            extensions.append(ext)

    def guess_type(self, url, strict=True):
        """Guess the type of a file given by URL or path.

        Returns a (type, encoding) pair; either may be None.  ``data:`` URLs
        are understood (their media type is returned).
        """
        import os
        url = os.fspath(url)
        if isinstance(url, bytes):
            url = os.fsdecode(url)
        scheme, rest = _split_scheme(url)
        if scheme is None or len(scheme) == 1:
            # no scheme, or a Windows drive letter: a plain path
            return self.guess_file_type(url, strict=strict)
        if scheme == 'data':
            comma = rest.find(',')
            if comma < 0:
                return None, None
            semi = rest.find(';', 0, comma)
            media = rest[:semi] if semi >= 0 else rest[:comma]
            if '=' in media or '/' not in media:
                media = 'text/plain'
            return media, None
        import posixpath
        return self._guess_from_path(_url_path(rest), strict, posixpath.splitext)

    def guess_file_type(self, path, *, strict=True):
        """Guess the type of a file given by its path (a str, bytes or
        path-like object).  Returns a (type, encoding) pair."""
        import os
        path = os.fsdecode(path)
        path = os.path.splitdrive(path)[1]
        return self._guess_from_path(path, strict, os.path.splitext)

    def _guess_from_path(self, path, strict, splitext):
        base, ext = splitext(path)
        while True:
            replacement = self.suffix_map.get(ext.lower())
            if replacement is None:
                break
            base, ext = splitext(base + replacement)
        encoding = self.encodings_map.get(ext)
        if encoding is not None:
            base, ext = splitext(base)
        mime = self._lookup(ext, strict)
        return mime, encoding

    def _lookup(self, ext, strict):
        tables = (self.types_map[True],) if strict else self.types_map
        for table in tables:
            mime = table.get(ext)
            if mime is None:
                mime = table.get(ext.lower())
            if mime is not None:
                return mime
        return None

    def guess_all_extensions(self, type, strict=True):
        """Return the list of known extensions (with leading dot) for *type*,
        preferred extension first."""
        type = type.lower()
        result = list(self.types_map_inv[True].get(type, ()))
        if not strict:
            for ext in self.types_map_inv[False].get(type, ()):
                if ext not in result:
                    result.append(ext)
        return result

    def guess_extension(self, type, strict=True):
        """Return the preferred extension for *type*, or None."""
        extensions = self.guess_all_extensions(type, strict)
        return extensions[0] if extensions else None

    def read(self, filename, strict=True):
        """Add the mappings of a mime.types style file."""
        with open(filename, encoding='utf-8', errors='surrogateescape') as fp:
            self.readfp(fp, strict)

    def readfp(self, fp, strict=True):
        """Add the mappings of an open mime.types style file.

        Each line holds a type followed by extensions (without dots);
        '#' starts a comment."""
        for line in fp:
            hash_pos = line.find('#')
            if hash_pos >= 0:
                line = line[:hash_pos]
            words = line.split()
            if not words:
                continue
            mime = words[0]
            for suffix in words[1:]:
                self.add_type(mime, '.' + suffix, strict)

    def read_windows_registry(self, strict=True):
        """Load the MIME types registered in the Windows registry."""
        if not _mimetypes_read_windows_registry and not _winreg:
            return

        def add(mime, ext):
            self.add_type(mime, ext, strict)

        if _mimetypes_read_windows_registry:
            try:
                _mimetypes_read_windows_registry(add)
                return
            except OSError:
                pass
        if _winreg:
            self._read_windows_registry(add)

    @classmethod
    def _read_windows_registry(cls, add_type):
        def subkeys(key):
            index = 0
            while True:
                try:
                    name = _winreg.EnumKey(key, index)
                except OSError:
                    return
                if '\0' not in name:
                    yield name
                index += 1

        root = _winreg.OpenKey(_winreg.HKEY_CLASSES_ROOT, '')
        with root:
            for name in subkeys(root):
                if not name.startswith('.'):
                    continue
                try:
                    with _winreg.OpenKey(root, name) as key:
                        mime, kind = _winreg.QueryValueEx(key, 'Content Type')
                except OSError:
                    continue
                if kind == _winreg.REG_SZ:
                    add_type(mime, name)


def guess_type(url, strict=True):
    """Guess the (type, encoding) of a URL; see MimeTypes.guess_type()."""
    if _db is None:
        init()
    return _db.guess_type(url, strict)


def guess_file_type(path, *, strict=True):
    """Guess the (type, encoding) of a file path."""
    if _db is None:
        init()
    return _db.guess_file_type(path, strict=strict)


def guess_all_extensions(type, strict=True):
    """Return all known extensions for a MIME type."""
    if _db is None:
        init()
    return _db.guess_all_extensions(type, strict)


def guess_extension(type, strict=True):
    """Return the preferred extension for a MIME type, or None."""
    if _db is None:
        init()
    return _db.guess_extension(type, strict)


def add_type(type, ext, strict=True):
    """Add a mapping to the shared database."""
    if _db is None:
        init()
    return _db.add_type(type, ext, strict)


def init(files=None):
    """(Re)build the shared database.

    Without arguments the built-in tables, the Windows registry and the
    files in ``knownfiles`` are used.  Given *files*, those are read too.
    """
    global inited, _db, suffix_map, encodings_map, types_map, common_types
    import os
    inited = True
    if files is None or _db is None:
        db = MimeTypes()
        db.read_windows_registry()
        sources = list(knownfiles)
        if files is not None:
            sources.extend(files)
    else:
        db = _db
        sources = list(files)
    for name in sources:
        if os.path.isfile(name):
            db.read(name)
    suffix_map = db.suffix_map
    encodings_map = db.encodings_map
    types_map = db.types_map[True]
    common_types = db.types_map[False]
    _db = db


def read_mime_types(file):
    """Read a mime.types file; return its mapping of extensions to types,
    or None if the file cannot be opened."""
    try:
        fp = open(file, encoding='utf-8', errors='surrogateescape')
    except OSError:
        return None
    with fp:
        db = MimeTypes()
        db.readfp(fp, True)
        return db.types_map[True]


# Built-in data.  Each entry maps a MIME type to its extensions, preferred
# extension first.
_REGISTERED_TYPES = (
    ('application/epub+zip', '.epub'),
    ('application/gzip', '.gz'),
    ('application/json', '.json'),
    ('application/manifest+json', '.webmanifest'),
    ('application/msword', '.doc .dot .wiz'),
    ('application/n-quads', '.nq'),
    ('application/n-triples', '.nt'),
    ('application/octet-stream', '.bin .a .dll .exe .o .obj .so'),
    ('application/oda', '.oda'),
    ('application/ogg', '.ogx'),
    ('application/pdf', '.pdf'),
    ('application/dicom', '.dcm'),
    ('application/pkcs7-mime', '.p7c'),
    ('application/postscript', '.ps .ai .eps'),
    ('application/rtf', '.rtf'),
    ('application/sql', '.sql'),
    ('application/texinfo', '.texi .texinfo'),
    ('application/toml', '.toml'),
    ('application/trig', '.trig'),
    ('application/vnd.apple.mpegurl', '.m3u .m3u8'),
    ('application/vnd.ms-excel', '.xls .xlb'),
    ('application/vnd.ms-fontobject', '.eot'),
    ('application/vnd.ms-powerpoint', '.ppt .pot .ppa .pps .pwz'),
    ('application/vnd.oasis.opendocument.graphics', '.odg'),
    ('application/vnd.oasis.opendocument.presentation', '.odp'),
    ('application/vnd.oasis.opendocument.spreadsheet', '.ods'),
    ('application/vnd.oasis.opendocument.text', '.odt'),
    ('application/vnd.openxmlformats-officedocument.presentationml.presentation',
     '.pptx'),
    ('application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
     '.xlsx'),
    ('application/vnd.openxmlformats-officedocument.wordprocessingml.document',
     '.docx'),
    ('application/vnd.rar', '.rar'),
    ('application/vnd.sqlite3', '.sqlite3 .sqlite'),
    ('application/wasm', '.wasm'),
    ('application/x-7z-compressed', '.7z'),
    ('application/x-bcpio', '.bcpio'),
    ('application/x-cpio', '.cpio'),
    ('application/x-csh', '.csh'),
    ('application/x-debian-package', '.deb .udeb'),
    ('application/x-dvi', '.dvi'),
    ('application/x-gtar', '.gtar'),
    ('application/x-hdf', '.hdf'),
    ('application/x-hdf5', '.h5'),
    ('application/x-httpd-php', '.php'),
    ('application/x-latex', '.latex'),
    ('application/x-mif', '.mif'),
    ('application/x-netcdf', '.cdf .nc'),
    ('application/x-pkcs12', '.p12 .pfx'),
    ('application/x-pn-realaudio', '.ram'),
    ('application/x-python-code', '.pyc .pyo'),
    ('application/x-rpm', '.rpm'),
    ('application/x-sh', '.sh'),
    ('application/x-shar', '.shar'),
    ('application/x-shockwave-flash', '.swf'),
    ('application/x-sv4cpio', '.sv4cpio'),
    ('application/x-sv4crc', '.sv4crc'),
    ('application/x-tar', '.tar'),
    ('application/x-tcl', '.tcl'),
    ('application/x-tex', '.tex'),
    ('application/x-troff', '.roff .t .tr'),
    ('application/x-troff-man', '.man'),
    ('application/x-troff-me', '.me'),
    ('application/x-troff-ms', '.ms'),
    ('application/x-ustar', '.ustar'),
    ('application/x-wais-source', '.src'),
    ('application/xml', '.xsl .rdf .wsdl .xpdl'),
    ('application/yaml', '.yaml .yml'),
    ('application/zip', '.zip'),
    ('audio/3gpp', '.3gp .3gpp'),
    ('audio/3gpp2', '.3g2 .3gpp2'),
    ('audio/aac', '.aac .adts .loas .ass'),
    ('audio/basic', '.au .snd'),
    ('audio/flac', '.flac'),
    ('audio/matroska', '.mka'),
    ('audio/mp4', '.m4a'),
    ('audio/mpeg', '.mp3 .mp2'),
    ('audio/ogg', '.ogg'),
    ('audio/opus', '.opus'),
    ('audio/vnd.wave', '.wav'),
    ('audio/webm', '.weba'),
    ('audio/x-aiff', '.aif .aifc .aiff'),
    ('audio/x-pn-realaudio', '.ra'),
    ('font/otf', '.otf'),
    ('font/ttf', '.ttf'),
    ('font/woff', '.woff'),
    ('font/woff2', '.woff2'),
    ('image/avif', '.avif'),
    ('image/bmp', '.bmp'),
    ('image/emf', '.emf'),
    ('image/fits', '.fits'),
    ('image/g3fax', '.g3'),
    ('image/gif', '.gif'),
    ('image/heic', '.heic'),
    ('image/heif', '.heif'),
    ('image/ief', '.ief'),
    ('image/jp2', '.jp2'),
    ('image/jpeg', '.jpg .jpe .jpeg'),
    ('image/jpm', '.jpm'),
    ('image/jxl', '.jxl'),
    ('image/png', '.png'),
    ('image/svg+xml', '.svg'),
    ('image/t38', '.t38'),
    ('image/tiff', '.tiff .tif'),
    ('image/tiff-fx', '.tfx'),
    ('image/vnd.microsoft.icon', '.ico'),
    ('image/webp', '.webp'),
    ('image/wmf', '.wmf'),
    ('image/x-cmu-raster', '.ras'),
    ('image/x-portable-anymap', '.pnm'),
    ('image/x-portable-bitmap', '.pbm'),
    ('image/x-portable-graymap', '.pgm'),
    ('image/x-portable-pixmap', '.ppm'),
    ('image/x-rgb', '.rgb'),
    ('image/x-xbitmap', '.xbm'),
    ('image/x-xpixmap', '.xpm'),
    ('image/x-xwindowdump', '.xwd'),
    ('message/rfc822', '.eml .mht .mhtml .nws'),
    ('model/gltf+json', '.gltf'),
    ('model/gltf-binary', '.glb'),
    ('model/stl', '.stl'),
    ('text/css', '.css'),
    ('text/csv', '.csv'),
    ('text/html', '.html .htm'),
    ('text/javascript', '.js .mjs'),
    ('text/markdown', '.md .markdown'),
    ('text/n3', '.n3'),
    ('text/plain', '.txt .bat .c .h .ksh .pl .srt'),
    ('text/richtext', '.rtx'),
    ('text/tab-separated-values', '.tsv'),
    ('text/vtt', '.vtt'),
    ('text/x-python', '.py'),
    ('text/x-rst', '.rst'),
    ('text/x-setext', '.etx'),
    ('text/x-sgml', '.sgm .sgml'),
    ('text/x-vcard', '.vcf'),
    ('text/xml', '.xml'),
    ('video/matroska', '.mkv'),
    ('video/matroska-3d', '.mk3d'),
    ('video/mp4', '.mp4'),
    ('video/mpeg', '.mpeg .m1v .mpa .mpe .mpg'),
    ('video/ogg', '.ogv'),
    ('video/quicktime', '.mov .qt'),
    ('video/vnd.avi', '.avi'),
    ('video/webm', '.webm'),
    ('video/x-m4v', '.m4v'),
    ('video/x-ms-wmv', '.wmv'),
    ('video/x-sgi-movie', '.movie'),
)

# Types that are in common use but not registered (used with strict=False).
_UNREGISTERED_TYPES = (
    ('audio/midi', '.mid .midi'),
    ('image/jpg', '.jpg'),
    ('image/pict', '.pict .pct .pic'),
    ('text/xul', '.xul'),
)


def _default_mime_types():
    global suffix_map, _suffix_map_default
    global encodings_map, _encodings_map_default
    global types_map, _types_map_default
    global common_types, _common_types_default

    suffix_map = _suffix_map_default = {
        '.svgz': '.svg.gz',
        '.tgz': '.tar.gz',
        '.taz': '.tar.gz',
        '.tz': '.tar.gz',
        '.tbz2': '.tar.bz2',
        '.txz': '.tar.xz',
    }
    encodings_map = _encodings_map_default = {
        '.gz': 'gzip',
        '.Z': 'compress',
        '.bz2': 'bzip2',
        '.xz': 'xz',
        '.br': 'br',
    }
    types_map = _types_map_default = {}
    for mime, extensions in _REGISTERED_TYPES:
        for ext in extensions.split():
            types_map[ext] = mime
    common_types = _common_types_default = {}
    for mime, extensions in _UNREGISTERED_TYPES:
        for ext in extensions.split():
            common_types[ext] = mime


_default_mime_types()


def _make_parser():
    from argparse import ArgumentParser
    parser = ArgumentParser(
        description="map filename extensions to MIME types (mimetypes)")
    parser.add_argument(
        '-e', '--extension', action='store_true',
        help='guess extension instead of type')
    parser.add_argument(
        '-l', '--lenient', action='store_true',
        help='additionally search for common but non-standard types')
    parser.add_argument("type", nargs="+", help="a type to search")
    return parser


def _parse_args(args):
    parser = _make_parser()
    help_text = parser.format_help()
    try:
        parsed = parser.parse_args(args)
    except SystemExit:
        parsed = None
    return parsed, help_text


def _main(args=None):
    """Run the command line interface; return the lines of output."""
    parsed = _make_parser().parse_args(args)
    strict = not parsed.lenient
    lines = []
    for item in parsed.type:
        if parsed.extension:
            ext = guess_extension(item, strict)
            lines.append(ext if ext else f"error: unknown type {item}")
        else:
            mime, encoding = guess_type(item, strict)
            if mime:
                lines.append(f"type: {mime} encoding: {encoding}")
            else:
                lines.append(f"error: media type unknown for {item}")
    return lines


if __name__ == '__main__':
    _lines = _main()
    print("\n".join(_lines))
    sys.exit(1 if any(line.startswith("error: ") for line in _lines) else 0)
