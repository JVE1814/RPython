r"""Universally unique identifiers (RFC 9562, formerly RFC 4122).

The UUID class represents a 128-bit identifier and accepts any of its usual
spellings (a hex string, 16 bytes in either byte order, the six fields or an
integer).  New identifiers come from:

    uuid1()  time + node (hardware address)     uuid5()  SHA-1 of a name
    uuid3()  MD5 of a namespace and a name      uuid6()  uuid1 reordered to sort by time
    uuid4()  random                             uuid7()  Unix time in ms + counter + random
                                                uuid8()  caller-chosen bits
"""

import builtins
import os
import sys
import time
from enum import Enum


# ------------------------------------------------------------ platform

if sys.platform == 'linux':
    _LINUX, _AIX = True, False
elif sys.platform in {'win32', 'darwin', 'emscripten', 'wasi'}:
    _LINUX = _AIX = False
else:
    import platform as _platform
    _system_name = _platform.system()
    _LINUX = _system_name in ('Linux', 'Android')
    _AIX = _system_name == 'AIX'

# How system tools print hardware addresses: AIX separates the octets with
# dots and drops leading zeroes ("fe.ad.c.1.23.4"), others use "52:54:00:...".
_MAC_DELIM = b'.' if _AIX else b':'
_MAC_OMITS_LEADING_ZEROES = _AIX


# ------------------------------------------------------------ constants

RESERVED_NCS = 'reserved for NCS compatibility'
RFC_4122 = 'specified in RFC 4122'
RESERVED_MICROSOFT = 'reserved for Microsoft compatibility'
RESERVED_FUTURE = 'reserved for future definition'

# 100-nanosecond intervals from 1582-10-15 (start of the Gregorian
# calendar, the epoch of version 1 and 6 timestamps) to 1970-01-01.
_GREGORIAN_OFFSET = 0x01b21dd213814000

_ALL_BITS = (1 << 128) - 1
_VARIANT_BITS = 0xc000 << 48        # top two bits of clock_seq_hi_variant
_VERSION_BITS = 0xf000 << 64        # top four bits of time_hi_version
_RFC_VARIANT = 0x8000 << 48         # binary 10 in the variant bits

_FIELD_WIDTHS = (32, 16, 16, 8, 8, 48)


def _with_version(value, version):
    """value with the RFC variant and the given version stamped into it."""
    value &= ~(_VARIANT_BITS | _VERSION_BITS)
    return value | _RFC_VARIANT | (version << 76)


def _int_from_hex(text):
    # drop the optional "urn:uuid:" prefix, braces and hyphens
    digits = text.replace('urn:', '').replace('uuid:', '').strip('{}').replace('-', '')
    if len(digits) == 32:
        try:
            return builtins.int(digits, 16)
        except ValueError:
            pass
    raise ValueError('badly formed hexadecimal UUID string')


def _swap_leading_fields(data):
    # Reverse the byte order of the first three fields (4, 2 and 2 bytes);
    # the conversion is its own inverse.
    return data[3::-1] + data[5:3:-1] + data[7:5:-1] + data[8:]


def _int_from_bytes(data, name):
    if len(data) != 16:
        raise ValueError('%s is not a 16-char string' % name)
    assert isinstance(data, builtins.bytes), repr(data)
    if name == 'bytes_le':
        data = _swap_leading_fields(data)
    return builtins.int.from_bytes(data)


def _int_from_fields(fields):
    if len(fields) != 6:
        raise ValueError('fields is not a 6-tuple')
    value = 0
    for position, (field, width) in builtins.enumerate(zip(fields, _FIELD_WIDTHS), 1):
        if not 0 <= field < 1 << width:
            raise ValueError('field %d out of range (need a %d-bit value)'
                             % (position, width))
        value = value << width | field
    return value


class SafeUUID(Enum):
    """How a version 1 UUID was made: in a way that is safe with several
    processes (safe), not (unsafe), or nobody can tell (unknown)."""
    safe = 0
    unsafe = -1
    unknown = None


class UUID:
    """An immutable UUID.

    Give exactly one of:
      hex       a string of 32 hex digits; braces, hyphens and a "urn:uuid:"
                prefix are allowed
      bytes     16 bytes, most significant first
      bytes_le  16 bytes with time_low, time_mid and time_hi_version in
                little-endian order (as Windows stores GUIDs)
      fields    (time_low, time_mid, time_hi_version, clock_seq_hi_variant,
                clock_seq_low, node)
      int       a 128-bit integer
    With version, the variant and version bits are set accordingly.
    is_safe records how a version 1 UUID was made (a SafeUUID).
    """

    __slots__ = ('int', 'is_safe', '__weakref__')

    def __init__(self, hex=None, bytes=None, bytes_le=None, fields=None,
                 int=None, version=None,
                 *, is_safe=SafeUUID.unknown):
        # exactly one of the five spellings may be given
        if (hex, bytes, bytes_le, fields, int).count(None) != 4:
            raise TypeError('one of the hex, bytes, bytes_le, fields, '
                            'or int arguments must be given')
        if int is not None:
            value = int
        elif hex is not None:
            value = _int_from_hex(hex)
        elif bytes is not None:
            value = _int_from_bytes(bytes, 'bytes')
        elif bytes_le is not None:
            value = _int_from_bytes(bytes_le, 'bytes_le')
        else:
            value = _int_from_fields(fields)
        if not 0 <= value <= _ALL_BITS:
            raise ValueError('int is out of range (need a 128-bit value)')
        if version is not None:
            if not 1 <= version <= 8:
                raise ValueError('illegal version number')
            value = _with_version(value, version)
        object.__setattr__(self, 'int', value)
        object.__setattr__(self, 'is_safe', is_safe)

    @classmethod
    def _from_int(cls, value):
        # For the generators: value is known to be a valid 128-bit int.
        made = object.__new__(cls)
        object.__setattr__(made, 'int', value)
        object.__setattr__(made, 'is_safe', SafeUUID.unknown)
        return made

    # Pickles hold a dict {'int': ..., ['is_safe': raw value]}, readable by
    # every Python version since 2.5.
    def __getstate__(self):
        if self.is_safe == SafeUUID.unknown:
            return {'int': self.int}
        return {'int': self.int, 'is_safe': self.is_safe.value}

    def __setstate__(self, state):
        object.__setattr__(self, 'int', state['int'])
        safety = state.get('is_safe', None)
        object.__setattr__(self, 'is_safe', SafeUUID(safety))

    # Ordered and compared by their integer value, against other UUIDs only.
    def __eq__(self, other):
        return self.int == other.int if isinstance(other, UUID) else NotImplemented

    def __lt__(self, other):
        return self.int < other.int if isinstance(other, UUID) else NotImplemented

    def __le__(self, other):
        return self.int <= other.int if isinstance(other, UUID) else NotImplemented

    def __gt__(self, other):
        return self.int > other.int if isinstance(other, UUID) else NotImplemented

    def __ge__(self, other):
        return self.int >= other.int if isinstance(other, UUID) else NotImplemented

    def __hash__(self):
        return hash(self.int)

    def __int__(self):
        return self.int

    def __repr__(self):
        return '%s(%r)' % (type(self).__name__, str(self))

    def __setattr__(self, name, value):
        raise TypeError('UUID objects are immutable')

    def __str__(self):
        digits = '%032x' % self.int
        return '-'.join((digits[:8], digits[8:12], digits[12:16],
                         digits[16:20], digits[20:]))

    @property
    def bytes(self):
        return self.int.to_bytes(16)

    @property
    def bytes_le(self):
        return _swap_leading_fields(self.bytes)

    @property
    def fields(self):
        return (self.time_low, self.time_mid, self.time_hi_version,
                self.clock_seq_hi_variant, self.clock_seq_low, self.node)

    @property
    def time_low(self):
        return self.int >> 96

    @property
    def time_mid(self):
        return self.int >> 80 & 0xffff

    @property
    def time_hi_version(self):
        return self.int >> 64 & 0xffff

    @property
    def clock_seq_hi_variant(self):
        return self.int >> 56 & 0xff

    @property
    def clock_seq_low(self):
        return self.int >> 48 & 0xff

    @property
    def time(self):
        """The timestamp: 100 ns ticks since 1582 for versions 1 and 6,
        milliseconds since 1970 for version 7."""
        version = self.version
        if version == 7:
            return self.int >> 80
        if version == 6:
            # stored most significant part first: 48 bits, then 12
            return (self.int >> 80) << 12 | (self.int >> 64 & 0x0fff)
        return ((self.time_hi_version & 0x0fff) << 48
                | self.time_mid << 32 | self.time_low)

    @property
    def clock_seq(self):
        return (self.clock_seq_hi_variant & 0x3f) << 8 | self.clock_seq_low

    @property
    def node(self):
        return self.int & 0xffffffffffff

    @property
    def hex(self):
        return '%032x' % self.int

    @property
    def urn(self):
        return 'urn:uuid:' + str(self)

    @property
    def variant(self):
        # The variant is given by the number of leading one bits of
        # clock_seq_hi_variant.
        top = self.int >> 61 & 0b111
        if top < 0b100:
            return RESERVED_NCS
        if top < 0b110:
            return RFC_4122
        if top < 0b111:
            return RESERVED_MICROSOFT
        return RESERVED_FUTURE

    @property
    def version(self):
        """The version number, for UUIDs of the RFC variant (else None)."""
        if self.variant != RFC_4122:
            return None
        return self.int >> 76 & 0xf


# ------------------------------------------------------------ hardware address

def _get_command_stdout(command, args):
    """Run a system tool with the arguments in the string args (in the C
    locale; /sbin and /usr/sbin are searched too) and return its output as
    a binary file, or None if it cannot be run."""
    import io
    import shutil
    import subprocess
    search = os.environ.get('PATH', os.defpath).split(os.pathsep)
    search += ['/sbin', '/usr/sbin']
    program = shutil.which(command, path=os.pathsep.join(search))
    if program is None:
        return None
    environment = dict(os.environ, LC_ALL='C')
    try:
        output = subprocess.run([program, *args.split()],
                                stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL,
                                env=environment).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    return io.BytesIO(output)


def _is_universal(mac):
    # Bit 1 of the first octet marks a locally administered address.
    return not mac & (1 << 41)


def _prefer_universal(macs):
    """The first universally administered address among macs, else the
    first local one, else None."""
    fallback = None
    for mac in macs:
        if _is_universal(mac):
            return mac
        if fallback is None:
            fallback = mac
    return fallback


def _find_mac_near_keyword(command, args, keywords, get_word_index):
    """Look through command's output for one of keywords (lowercase bytes);
    the address is the word at get_word_index(position of the keyword) on
    that line."""
    output = _get_command_stdout(command, args)
    if output is None:
        return None

    def candidates():
        for line in output:
            words = line.lower().split()
            for position, word in builtins.enumerate(words):
                if word not in keywords:
                    continue
                try:
                    text = words[get_word_index(position)]
                    yield builtins.int(text.replace(_MAC_DELIM, b''), 16)
                except (IndexError, ValueError):
                    pass

    return _prefer_universal(candidates())


def _parse_mac(word):
    """The address written in word (octets separated by _MAC_DELIM), or
    None if word is something else, such as an IPv6 address."""
    octets = word.split(_MAC_DELIM)
    if len(octets) != 6:
        return None
    if _MAC_OMITS_LEADING_ZEROES:
        if any(not 1 <= len(octet) <= 2 for octet in octets):
            return None
        octets = [octet.rjust(2, b'0') for octet in octets]
    elif any(len(octet) != 2 for octet in octets):
        return None
    try:
        return builtins.int(b''.join(octets), 16)
    except ValueError:
        return None


def _find_mac_under_heading(command, args, heading):
    """Find an address in the column headed heading of the table command
    prints."""
    output = _get_command_stdout(command, args)
    if output is None:
        return None
    try:
        column = output.readline().split().index(heading)
    except ValueError:
        return None

    def candidates():
        for line in output:
            words = line.split()
            if column < len(words):
                mac = _parse_mac(words[column])
                if mac is not None:
                    yield mac

    return _prefer_universal(candidates())


def _next_word(position):
    return position + 1


def _ifconfig_getnode():
    """Hardware address from ifconfig."""
    keywords = (b'hwaddr', b'ether', b'address:', b'lladdr')
    for args in ('', '-a', '-av'):
        mac = _find_mac_near_keyword('ifconfig', args, keywords, _next_word)
        if mac:
            return mac
    return None


def _ip_getnode():
    """Hardware address from "ip link"."""
    return _find_mac_near_keyword('ip', 'link', [b'link/ether'], _next_word) or None


def _arp_getnode():
    """Hardware address of this host's IP address in the ARP table."""
    import socket
    if not hasattr(socket, 'gethostbyname'):
        return None
    try:
        address = socket.gethostbyname(socket.gethostname())
    except OSError:
        return None
    # "host (1.2.3.4) at 52:54:..." on most systems, "(1.2.3.4) 52:54:..."
    # or "1.2.3.4 ... 52:54:..." on some.
    attempts = ((os.fsencode('(%s)' % address), lambda i: i + 2),
                (os.fsencode('(%s)' % address), _next_word),
                (os.fsencode(address), lambda i: i + 2))
    for keyword, locate in attempts:
        mac = _find_mac_near_keyword('arp', '-an', [keyword], locate)
        if mac:
            return mac
    return None


def _lanscan_getnode():
    """Hardware address from lanscan (HP-UX)."""
    return _find_mac_near_keyword('lanscan', '-ai', [b'lan0'], lambda i: 0)


def _netstat_getnode():
    """Hardware address from "netstat -ian"."""
    return _find_mac_under_heading('netstat', '-ian', b'Address')


# The optional _uuid accelerator wraps the system's uuid library.
try:
    import _uuid
except ImportError:
    _uuid = None
_generate_time_safe = getattr(_uuid, 'generate_time_safe', None)
_UuidCreate = getattr(_uuid, 'UuidCreate', None)
_has_stable_extractable_node = getattr(_uuid, 'has_stable_extractable_node', False)


def _unix_getnode():
    """Node of a version 1 UUID made by the system library (if it uses the
    hardware address)."""
    if _generate_time_safe and _has_stable_extractable_node:
        raw, _ = _generate_time_safe()
        return UUID(bytes=raw).node
    return None


def _windll_getnode():
    """Node of a UUID made by the Windows API."""
    if _UuidCreate and _has_stable_extractable_node:
        return UUID(bytes_le=_UuidCreate()).node
    return None


def _random_getnode():
    """A random node with the multicast bit set, so that it cannot clash
    with a real hardware address (RFC 9562, section 6.10)."""
    import random
    return random.getrandbits(48) | 1 << 40


def _platform_getters():
    if _LINUX:
        return [_ip_getnode, _ifconfig_getnode]
    if sys.platform == 'darwin':
        return [_ifconfig_getnode, _arp_getnode, _netstat_getnode]
    if sys.platform == 'win32':
        return []
    if _AIX:
        return [_netstat_getnode]
    return [_ifconfig_getnode, _ip_getnode, _arp_getnode, _netstat_getnode,
            _lanscan_getnode]


_OS_GETTERS = _platform_getters()
_GETTERS = ({'posix': [_unix_getnode], 'nt': [_windll_getnode]}.get(os.name, [])
            + _OS_GETTERS)

_node = None    # cached result of getnode()


def getnode():
    """The hardware address of this host as a 48-bit integer.

    If none can be found, a random value with the multicast bit set is used
    instead.  The result is cached.
    """
    global _node
    if _node is None:
        for getter in (*_GETTERS, _random_getnode):
            try:
                found = getter()
            except:
                continue
            if found is not None and 0 <= found < 1 << 48:
                _node = found
                break
    return _node


# ------------------------------------------------------------ generators

_last_timestamp = None          # uuid1()
_last_timestamp_v6 = None       # uuid6()


def _gregorian_ticks(last):
    # Current time in 100 ns ticks since 1582, strictly after last.
    now = time.time_ns() // 100 + _GREGORIAN_OFFSET
    if last is not None and now <= last:
        now = last + 1
    return now


def _random_clock_seq():
    import random
    return random.getrandbits(14)


def uuid1(node=None, clock_seq=None):
    """A version 1 UUID: the time, a clock sequence (random unless given)
    and the node (getnode() unless given)."""
    global _last_timestamp
    if node is None and clock_seq is None and _generate_time_safe is not None:
        raw, safety = _generate_time_safe()
        try:
            is_safe = SafeUUID(safety)
        except ValueError:
            is_safe = SafeUUID.unknown
        return UUID(bytes=raw, is_safe=is_safe)
    ticks = _last_timestamp = _gregorian_ticks(_last_timestamp)
    if clock_seq is None:
        clock_seq = _random_clock_seq()
    if node is None:
        node = getnode()
    return UUID(fields=(ticks & 0xffffffff, ticks >> 32 & 0xffff,
                        ticks >> 48 & 0x0fff, clock_seq >> 8 & 0x3f,
                        clock_seq & 0xff, node),
                version=1)


def _name_based(namespace, name, digest_name, version):
    import hashlib
    if isinstance(name, str):
        name = name.encode('utf-8')
    digest = getattr(hashlib, digest_name)(namespace.bytes + name,
                                           usedforsecurity=False).digest()
    return UUID(bytes=digest[:16], version=version)


def uuid3(namespace, name):
    """A version 3 UUID: the MD5 hash of the namespace UUID and the name."""
    return _name_based(namespace, name, 'md5', 3)


def uuid4():
    """A version 4 UUID: random."""
    return UUID._from_int(_with_version(builtins.int.from_bytes(os.urandom(16)), 4))


def uuid5(namespace, name):
    """A version 5 UUID: the SHA-1 hash of the namespace UUID and the name."""
    return _name_based(namespace, name, 'sha1', 5)


def uuid6(node=None, clock_seq=None):
    """A version 6 UUID: the fields of version 1 with the timestamp stored
    most significant part first, so that the UUIDs sort by time."""
    global _last_timestamp_v6
    ticks = _last_timestamp_v6 = _gregorian_ticks(_last_timestamp_v6)
    if clock_seq is None:
        clock_seq = _random_clock_seq()
    if node is None:
        node = getnode()
    value = (ticks >> 12 & 0xffff_ffff_ffff) << 80 | (ticks & 0x0fff) << 64
    value |= (clock_seq & 0x3fff) << 48 | node & 0xffff_ffff_ffff
    return UUID._from_int(_with_version(value, 6))


# uuid7(): 42-bit counter that orders the UUIDs made within one millisecond.
_last_timestamp_v7 = None
_last_counter_v7 = 0
_COUNTER_V7_MAX = (1 << 42) - 1


def _fresh_v7_counter():
    # A random counter and tail from 10 random bytes.  The counter's top bit
    # is left clear so that it has room to count up.
    entropy = builtins.int.from_bytes(os.urandom(10))
    return entropy >> 32 & (_COUNTER_V7_MAX >> 1), entropy & 0xffff_ffff


def uuid7():
    """A version 7 UUID: Unix time in milliseconds, a counter that keeps
    UUIDs from the same millisecond in order, and 32 random bits."""
    global _last_timestamp_v7, _last_counter_v7
    now_ms = time.time_ns() // 1_000_000
    last = _last_timestamp_v7
    if last is None or now_ms > last:
        counter, tail = _fresh_v7_counter()
    else:
        # Same millisecond, or the clock went back: stay after the last one.
        if now_ms < last:
            now_ms = last + 1
        counter = _last_counter_v7 + 1
        if counter > _COUNTER_V7_MAX:
            now_ms += 1
            counter, tail = _fresh_v7_counter()
        else:
            tail = builtins.int.from_bytes(os.urandom(4))
    _last_timestamp_v7, _last_counter_v7 = now_ms, counter
    value = (now_ms & 0xffff_ffff_ffff) << 80
    value |= (counter >> 30 & 0x0fff) << 64     # 12 bits beside the version
    value |= (counter & 0x3fff_ffff) << 32      # 30 bits beside the variant
    value |= tail & 0xffff_ffff
    return UUID._from_int(_with_version(value, 7))


def uuid8(a=None, b=None, c=None):
    """A version 8 UUID from the 48-bit a, 12-bit b and 62-bit c, each
    pseudo-random when not given."""
    import random
    if a is None:
        a = random.getrandbits(48)
    if b is None:
        b = random.getrandbits(12)
    if c is None:
        c = random.getrandbits(62)
    value = (a & 0xffff_ffff_ffff) << 80 | (b & 0xfff) << 64
    value |= c & 0x3fff_ffff_ffff_ffff
    return UUID._from_int(_with_version(value, 8))


# ------------------------------------------------------------ command line

def main():
    """Print newly generated UUIDs (python -m uuid)."""
    import argparse

    makers = {'uuid1': uuid1, 'uuid3': uuid3, 'uuid4': uuid4, 'uuid5': uuid5,
              'uuid6': uuid6, 'uuid7': uuid7, 'uuid8': uuid8}
    named = {'@dns': NAMESPACE_DNS, '@url': NAMESPACE_URL,
             '@oid': NAMESPACE_OID, '@x500': NAMESPACE_X500}

    parser = argparse.ArgumentParser(
        description="Print newly generated UUIDs.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
        color=True)
    parser.add_argument('-u', '--uuid', choices=makers.keys(), default='uuid4',
                        help="which generator to use")
    parser.add_argument('-n', '--namespace',
                        help="for uuid3/uuid5: a namespace UUID, or one of "
                             "@dns, @url, @oid and @x500")
    parser.add_argument('-N', '--name',
                        help="for uuid3/uuid5: the name to hash")
    parser.add_argument('-C', '--count', metavar='NUM', type=int, default=1,
                        help="how many UUIDs to print")
    options = parser.parse_args()

    make = makers[options.uuid]
    arguments = ()
    if options.uuid in ('uuid3', 'uuid5'):
        if not options.namespace or not options.name:
            parser.error("Incorrect number of arguments: %s needs both "
                         "--namespace and --name." % options.uuid)
        namespace = named.get(options.namespace)
        if namespace is None:
            try:
                namespace = UUID(options.namespace)
            except ValueError as exc:
                parser.error(str(exc))
        arguments = (namespace, options.name)
    for _ in range(options.count):
        print(make(*arguments))


# Predefined namespaces (RFC 9562, section 6.6).
NAMESPACE_DNS = UUID('6ba7b810-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_URL = UUID('6ba7b811-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_OID = UUID('6ba7b812-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_X500 = UUID('6ba7b814-9dad-11d1-80b4-00c04fd430c8')

# All bits clear / all bits set (RFC 9562, sections 5.9 and 5.10).
NIL = UUID('00000000-0000-0000-0000-000000000000')
MAX = UUID('ffffffff-ffff-ffff-ffff-ffffffffffff')

if __name__ == '__main__':
    main()
