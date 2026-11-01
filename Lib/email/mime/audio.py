"""MIME audio/* messages."""

__all__ = ['MIMEAudio']

from email import encoders
from email.mime.nonmultipart import MIMENonMultipart


class MIMEAudio(MIMENonMultipart):
    """An audio/* message (base64 encoded by default)."""

    def __init__(self, _audiodata, _subtype=None,
                 _encoder=encoders.encode_base64, *, policy=None, **_params):
        """Without *_subtype*, the format is detected from the data
        (AIFF, AU, WAV); TypeError if that fails."""
        if _subtype is None:
            _subtype = _what(_audiodata)
        if _subtype is None:
            raise TypeError('Could not find audio MIME subtype')
        MIMENonMultipart.__init__(self, 'audio', _subtype, policy=policy,
                                  **_params)
        self.set_payload(_audiodata)
        _encoder(self)


def _what(data):
    head = data[:512]
    if head.startswith(b'FORM') and head[8:12] in (b'AIFC', b'AIFF'):
        return 'x-aiff'
    if head.startswith(b'.snd'):
        return 'basic'
    if (head.startswith(b'RIFF') and head[8:12] == b'WAVE'
            and head[12:16] == b'fmt '):
        return 'x-wav'
    return None
