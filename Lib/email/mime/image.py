"""MIME image/* messages."""

__all__ = ['MIMEImage']

from email import encoders
from email.mime.nonmultipart import MIMENonMultipart


class MIMEImage(MIMENonMultipart):
    """An image/* message (base64 encoded by default)."""

    def __init__(self, _imagedata, _subtype=None,
                 _encoder=encoders.encode_base64, *, policy=None, **_params):
        """Without *_subtype*, the image format is detected from the data;
        TypeError if that fails."""
        if _subtype is None:
            _subtype = _what(_imagedata)
        if _subtype is None:
            raise TypeError('Could not guess image MIME subtype')
        MIMENonMultipart.__init__(self, 'image', _subtype, policy=policy,
                                  **_params)
        self.set_payload(_imagedata)
        _encoder(self)


def _is_pnm(h, kinds):
    return (len(h) >= 3 and h[0] == ord(b'P') and h[1] in kinds
            and h[2] in b' \t\n\r')


# (subtype, test) pairs, tried in order
_rules = [
    ('jpeg', lambda h: h[6:10] in (b'JFIF', b'Exif') or h[:2] == b'\xff\xd8'),
    ('png', lambda h: h.startswith(b'\x89PNG\r\n\x1a\n')),
    ('gif', lambda h: h[:6] in (b'GIF87a', b'GIF89a')),
    ('tiff', lambda h: h[:2] in (b'MM', b'II')),
    ('rgb', lambda h: h.startswith(b'\x01\xda')),
    ('pbm', lambda h: _is_pnm(h, b'14')),
    ('pgm', lambda h: _is_pnm(h, b'25')),
    ('ppm', lambda h: _is_pnm(h, b'36')),
    ('rast', lambda h: h.startswith(b'\x59\xa6\x6a\x95')),
    ('xbm', lambda h: h.startswith(b'#define ')),
    ('bmp', lambda h: h.startswith(b'BM')),
    ('webp', lambda h: h.startswith(b'RIFF') and h[8:12] == b'WEBP'),
    ('exr', lambda h: h.startswith(b'\x76\x2f\x31\x01')),
]


def _what(data):
    for subtype, test in _rules:
        result = test(data)
        if result:
            return subtype if subtype is not None else result
    return None


def rule(rulefunc):
    """Register a detection function returning a subtype or None."""
    _rules.append((None, rulefunc))
    return rulefunc
