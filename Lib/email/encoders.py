"""Functions that apply a Content-Transfer-Encoding to a message payload."""

__all__ = [
    'encode_7or8bit',
    'encode_base64',
    'encode_noop',
    'encode_quopri',
]

from base64 import encodebytes as _encodebytes
from quopri import encodestring as _encodestring


def _qencode(data):
    # quoted-printable with spaces always escaped
    return _encodestring(data, quotetabs=True).replace(b' ', b'=20')


def encode_base64(msg):
    """Base64-encode the payload of *msg* and set the CTE header."""
    original = msg.get_payload(decode=True)
    msg.set_payload(str(_encodebytes(original), 'ascii'))
    msg['Content-Transfer-Encoding'] = 'base64'


def encode_quopri(msg):
    """Quoted-printable encode the payload of *msg* and set the CTE header."""
    original = msg.get_payload(decode=True)
    msg.set_payload(_qencode(original))
    msg['Content-Transfer-Encoding'] = 'quoted-printable'


def encode_7or8bit(msg):
    """Set the CTE header to 7bit or 8bit depending on the payload."""
    original = msg.get_payload(decode=True)
    if original is None:
        msg['Content-Transfer-Encoding'] = '7bit'
        return
    try:
        original.decode('ascii')
    except UnicodeError:
        msg['Content-Transfer-Encoding'] = '8bit'
    else:
        msg['Content-Transfer-Encoding'] = '7bit'


def encode_noop(msg):
    """Leave the payload and headers alone."""
