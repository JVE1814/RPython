"""Easter egg: opens the "import antigravity" comic in a web browser."""

import hashlib
import webbrowser

webbrowser.open("https://xkcd.com/353/")


def geohash(latitude, longitude, datedow):
    """Print the xkcd geohashing coordinates (comic 426).

    `datedow` is the date and the Dow Jones opening as bytes, e.g.
    b'2005-05-26-10458.68'.  The MD5 digest is split into two halves that
    are read as hexadecimal fractions and appended to the integer parts of
    the given latitude and longitude.
    """
    digest = hashlib.md5(datedow, usedforsecurity=False).hexdigest()
    fractions = []
    for half in (digest[:16], digest[16:]):
        value = float.fromhex('0.' + half)
        fractions.append(('%f' % value)[1:])   # drop the leading '0'
    print('%d%s %d%s' % (latitude, fractions[0], longitude, fractions[1]))
