r"""UUID objects (universally unique identifiers) as specified in RFC 9562
(which supersedes RFC 4122).

UUID() builds an identifier from its string, bytes, fields or integer form;
uuid1() .. uuid8() generate new ones (versions 1 and 6 from time and the
host's node id, 3 and 5 from name hashes, 4 and 8 from random data, 7 from
Unix time plus a monotonic counter).
"""

import os
import sys
import time
from enum import Enum


if sys.platform in {'win32', 'darwin', 'emscripten', 'wasi'}:
    _AIX = _LINUX = False
elif sys.platform == 'linux':
    _LINUX = True
    _AIX = False
else:
    import platform as _platform
    _platform_system = _platform.system()
    _AIX = _platform_system == 'AIX'
    _LINUX = _platform_system in ('Linux', 'Android')

# Separator and digit style of MAC addresses printed by system tools.
_MAC_DELIM = b'.' if _AIX else b':'
_MAC_OMITS_LEADING_ZEROES = _AIX

RESERVED_NCS, RFC_4122, RESERVED_MICROSOFT, RESERVED_FUTURE = [
    'reserved for NCS compatibility', 'specified in RFC 4122',
    'reserved for Microsoft compatibility', 'reserved for future definition']

int_ = int        # the UUID constructor has an argument called "int"
bytes_ = bytes    # ... and one called "bytes"

# Offset between the UUID epoch (1582-10-15) and the Unix epoch, in 100 ns.
_GREGORIAN_OFFSET = 0x01b21dd213814000

_UINT128_MAX = (1 << 128) - 1
# Masks clearing the variant (2 bits) and version (4 bits) fields.
_VARIANT_MASK = ~(0xc000 << 48)
_VERSION_MASK = ~(0xf000 << 64)
_RFC_4122_VARIANT = 0x8000 << 48


class SafeUUID(Enum):
    """Whether a version 1 UUID was generated in a multiprocessing-safe way."""
    safe = 0
    unsafe = -1
    unknown = None


class UUID:
    """An immutable UUID.

    Exactly one of *hex* (32 hex digits, optionally with braces, hyphens
    and a 'urn:uuid:' prefix), *bytes* (16 bytes, big-endian), *bytes_le*
    (16 bytes with the first three fields little-endian), *fields* (a
    6-tuple: time_low, time_mid, time_hi_version, clock_seq_hi_variant,
    clock_seq_low, node) or *int* (128-bit integer) must be given.  If
    *version* is given, the variant and version bits are overwritten.

    Read-only views: bytes, bytes_le, fields and its parts, time,
    clock_seq, node, hex, int, urn, variant, version, is_safe.
    """

    __slots__ = ('int', 'is_safe', '__weakref__')

    def __init__(self, hex=None, bytes=None, bytes_le=None, fields=None,
                 int=None, version=None,
                 *, is_safe=SafeUUID.unknown):
        given = [hex, bytes, bytes_le, fields, int].count(None)
        if given != 4:
            raise TypeError('one of the hex, bytes, bytes_le, fields, '
                            'or int arguments must be given')
        if int is not None:
            value = int
        elif hex is not None:
            text = hex.replace('urn:', '').replace('uuid:', '')
            text = text.strip('{}').replace('-', '')
            if len(text) != 32:
                raise ValueError('badly formed hexadecimal UUID string')
            try:
                value = int_(text, 16)
            except ValueError:
                raise ValueError('badly formed hexadecimal UUID string') from None
        elif bytes_le is not None:
            if len(bytes_le) != 16:
                raise ValueError('bytes_le is not a 16-char string')
            assert isinstance(bytes_le, bytes_), repr(bytes_le)
            be = (bytes_le[3::-1] + bytes_le[5:3:-1] + bytes_le[7:5:-1]
                  + bytes_le[8:])
            value = int_.from_bytes(be)
        elif bytes is not None:
            if len(bytes) != 16:
                raise ValueError('bytes is not a 16-char string')
            assert isinstance(bytes, bytes_), repr(bytes)
            value = int_.from_bytes(bytes)
        else:
            if len(fields) != 6:
                raise ValueError('fields is not a 6-tuple')
            widths = (32, 16, 16, 8, 8, 48)
            value = 0
            for i, (field, width) in enumerate(zip(fields, widths), 1):
                if not 0 <= field < (1 << width):
                    raise ValueError(f'field {i} out of range '
                                     f'(need a {width}-bit value)')
                value = (value << width) | field
        if not 0 <= value <= _UINT128_MAX:
            raise ValueError('int is out of range (need a 128-bit value)')
        if version is not None:
            if not 1 <= version <= 8:
                raise ValueError('illegal version number')
            value = (value & _VARIANT_MASK) | _RFC_4122_VARIANT
            value = (value & _VERSION_MASK) | (version << 76)
        object.__setattr__(self, 'int', value)
        object.__setattr__(self, 'is_safe', is_safe)

    @classmethod
    def _from_int(cls, value):
        """Build a UUID from a valid 128-bit integer without checks."""
        self = object.__new__(cls)
        object.__setattr__(self, 'int', value)
        object.__setattr__(self, 'is_safe', SafeUUID.unknown)
        return self

    def __getstate__(self):
        state = {'int': self.int}
        if self.is_safe != SafeUUID.unknown:
            # Store the raw value so older Pythons can unpickle it.
            state['is_safe'] = self.is_safe.value
        return state

    def __setstate__(self, state):
        object.__setattr__(self, 'int', state['int'])
        object.__setattr__(self, 'is_safe',
                           SafeUUID(state['is_safe'])
                           if 'is_safe' in state else SafeUUID.unknown)

    def __eq__(self, other):
        if isinstance(other, UUID):
            return self.int == other.int
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, UUID):
            return self.int < other.int
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, UUID):
            return self.int > other.int
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, UUID):
            return self.int <= other.int
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, UUID):
            return self.int >= other.int
        return NotImplemented

    def __hash__(self):
        return hash(self.int)

    def __int__(self):
        return self.int

    def __repr__(self):
        return '%s(%r)' % (self.__class__.__name__, str(self))

    def __setattr__(self, name, value):
        raise TypeError('UUID objects are immutable')

    def __str__(self):
        h = self.hex
        return f'{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}'

    @property
    def bytes(self):
        return self.int.to_bytes(16)

    @property
    def bytes_le(self):
        b = self.bytes
        return b[3::-1] + b[5:3:-1] + b[7:5:-1] + b[8:]

    @property
    def fields(self):
        return (self.time_low, self.time_mid, self.time_hi_version,
                self.clock_seq_hi_variant, self.clock_seq_low, self.node)

    @property
    def time_low(self):
        return self.int >> 96

    @property
    def time_mid(self):
        return (self.int >> 80) & 0xffff

    @property
    def time_hi_version(self):
        return (self.int >> 64) & 0xffff

    @property
    def clock_seq_hi_variant(self):
        return (self.int >> 56) & 0xff

    @property
    def clock_seq_low(self):
        return (self.int >> 48) & 0xff

    @property
    def time(self):
        if self.version == 6:
            # 32 high bits | 16 middle bits | 12 low bits
            hi_mid = self.int >> 80
            low = (self.int >> 64) & 0x0fff
            return (hi_mid << 12) | low
        if self.version == 7:
            # Unix time in milliseconds
            return self.int >> 80
        hi = self.time_hi_version & 0x0fff
        return (hi << 48) | (self.time_mid << 32) | self.time_low

    @property
    def clock_seq(self):
        return ((self.clock_seq_hi_variant & 0x3f) << 8) | self.clock_seq_low

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
        if not self.int & (0x8000 << 48):
            return RESERVED_NCS
        if not self.int & (0x4000 << 48):
            return RFC_4122
        if not self.int & (0x2000 << 48):
            return RESERVED_MICROSOFT
        return RESERVED_FUTURE

    @property
    def version(self):
        # The version field only means something for the RFC variant.
        if self.variant == RFC_4122:
            return int((self.int >> 76) & 0xf)
        return None


# ---------------------------------------------------------------- node id

def _get_command_stdout(command, *args):
    """Run a system tool (looked up also in /sbin and /usr/sbin) in the C
    locale; return its stdout as a binary file object, or None."""
    import io
    import shutil
    import subprocess
    try:
        path_dirs = os.environ.get('PATH', os.defpath).split(os.pathsep)
        path_dirs.extend(['/sbin', '/usr/sbin'])
        executable = shutil.which(command, path=os.pathsep.join(path_dirs))
        if executable is None:
            return None
        env = dict(os.environ)
        env['LC_ALL'] = 'C'
        if args != ('',):
            cmd = (executable, *args)
        else:
            cmd = (executable,)
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, env=env)
        if not proc:
            return None
        stdout, _ = proc.communicate()
        return io.BytesIO(stdout)
    except (OSError, subprocess.SubprocessError):
        return None


def _is_universal(mac):
    """True unless the locally-administered bit of the address is set."""
    return not mac & (1 << 41)


def _find_mac_near_keyword(command, args, keywords, get_word_index):
    """Search *command*'s output for a word in *keywords*; the MAC address
    is the word at get_word_index(i) on the same line.  Universally
    administered addresses are preferred."""
    stdout = _get_command_stdout(command, args)
    if stdout is None:
        return None
    first_local = None
    for line in stdout:
        words = line.lower().rstrip().split()
        for i, word in enumerate(words):
            if word not in keywords:
                continue
            try:
                candidate = words[get_word_index(i)]
                mac = int(candidate.replace(_MAC_DELIM, b''), 16)
            except (ValueError, IndexError):
                continue
            if _is_universal(mac):
                return mac
            if first_local is None:
                first_local = mac
    return first_local


def _parse_mac(word):
    """Parse a MAC address written with _MAC_DELIM separators; None if the
    word is not one (for instance an IPv6 address)."""
    parts = word.split(_MAC_DELIM)
    if len(parts) != 6:
        return None
    if _MAC_OMITS_LEADING_ZEROES:
        if not all(1 <= len(p) <= 2 for p in parts):
            return None
        digits = b''.join(p.rjust(2, b'0') for p in parts)
    else:
        if not all(len(p) == 2 for p in parts):
            return None
        digits = b''.join(parts)
    try:
        return int(digits, 16)
    except ValueError:
        return None


def _find_mac_under_heading(command, args, heading):
    """Find a MAC address in the column titled *heading* of a table
    printed by *command*."""
    stdout = _get_command_stdout(command, args)
    if stdout is None:
        return None
    titles = stdout.readline().rstrip().split()
    try:
        column = titles.index(heading)
    except ValueError:
        return None
    first_local = None
    for line in stdout:
        words = line.rstrip().split()
        if column >= len(words):
            continue
        mac = _parse_mac(words[column])
        if mac is None:
            continue
        if _is_universal(mac):
            return mac
        if first_local is None:
            first_local = mac
    return first_local


def _ifconfig_getnode():
    """MAC address from ifconfig output."""
    keywords = (b'hwaddr', b'ether', b'address:', b'lladdr')
    for args in ('', '-a', '-av'):
        mac = _find_mac_near_keyword('ifconfig', args, keywords,
                                     lambda i: i + 1)
        if mac:
            return mac
    return None


def _ip_getnode():
    """MAC address from `ip link` output."""
    mac = _find_mac_near_keyword('ip', 'link', [b'link/ether'],
                                 lambda i: i + 1)
    if mac:
        return mac
    return None


def _arp_getnode():
    """MAC address of this host's IP address from the ARP cache."""
    import socket
    if not hasattr(socket, "gethostbyname"):
        return None
    try:
        ip_addr = socket.gethostbyname(socket.gethostname())
    except OSError:
        return None
    for word_index in (lambda i: i + 2, lambda i: i + 1):
        mac = _find_mac_near_keyword('arp', '-an',
                                     [os.fsencode('(%s)' % ip_addr)],
                                     word_index)
        if mac:
            return mac
    mac = _find_mac_near_keyword('arp', '-an', [os.fsencode(ip_addr)],
                                 lambda i: i + 2)
    if mac:
        return mac
    return None


def _lanscan_getnode():
    """MAC address from HP-UX lanscan output."""
    return _find_mac_near_keyword('lanscan', '-ai', [b'lan0'], lambda i: 0)


def _netstat_getnode():
    """MAC address from `netstat -ian` output."""
    return _find_mac_under_heading('netstat', '-ian', b'Address')


# Optional accelerator.
_generate_time_safe = None
_UuidCreate = None
_has_stable_extractable_node = False
try:
    import _uuid
    _generate_time_safe = getattr(_uuid, "generate_time_safe", None)
    _has_stable_extractable_node = getattr(_uuid,
                                           "has_stable_extractable_node",
                                           False)
    _UuidCreate = getattr(_uuid, "UuidCreate", None)
except ImportError:
    _uuid = None


def _unix_getnode():
    """Node id from a time-based UUID made by the system library."""
    if _generate_time_safe and _has_stable_extractable_node:
        uuid_time, _ = _generate_time_safe()
        return UUID(bytes=uuid_time).node


def _windll_getnode():
    """Node id from a UUID made by the Windows API."""
    if _UuidCreate and _has_stable_extractable_node:
        uuid_bytes = _UuidCreate()
        return UUID(bytes_le=uuid_bytes).node


def _random_getnode():
    """A random 48-bit node id with the multicast bit set (RFC 4122 4.5)."""
    import random
    return random.getrandbits(48) | (1 << 40)


if _LINUX:
    _OS_GETTERS = [_ip_getnode, _ifconfig_getnode]
elif sys.platform == 'darwin':
    _OS_GETTERS = [_ifconfig_getnode, _arp_getnode, _netstat_getnode]
elif sys.platform == 'win32':
    _OS_GETTERS = []
elif _AIX:
    _OS_GETTERS = [_netstat_getnode]
else:
    _OS_GETTERS = [_ifconfig_getnode, _ip_getnode, _arp_getnode,
                   _netstat_getnode, _lanscan_getnode]
if os.name == 'posix':
    _GETTERS = [_unix_getnode] + _OS_GETTERS
elif os.name == 'nt':
    _GETTERS = [_windll_getnode] + _OS_GETTERS
else:
    _GETTERS = _OS_GETTERS

_node = None


def getnode():
    """Return the 48-bit hardware address of this host as an integer.

    Falls back to a random number with the multicast bit set when no
    address can be found; the result is cached.
    """
    global _node
    if _node is not None:
        return _node
    for getter in _GETTERS + [_random_getnode]:
        try:
            candidate = getter()
        except:
            continue
        if candidate is not None and 0 <= candidate < (1 << 48):
            _node = candidate
            return _node
    assert False, '_random_getnode() returned an invalid value: {}'.format(_node)


# ---------------------------------------------------------------- generators

_last_timestamp = None


def uuid1(node=None, clock_seq=None):
    """Version 1 UUID from the current time, *clock_seq* (random if None)
    and *node* (getnode() if None)."""
    global _last_timestamp
    if _generate_time_safe is not None and node is clock_seq is None:
        uuid_time, safely_generated = _generate_time_safe()
        try:
            is_safe = SafeUUID(safely_generated)
        except ValueError:
            is_safe = SafeUUID.unknown
        return UUID(bytes=uuid_time, is_safe=is_safe)

    timestamp = time.time_ns() // 100 + _GREGORIAN_OFFSET
    if _last_timestamp is not None and timestamp <= _last_timestamp:
        timestamp = _last_timestamp + 1
    _last_timestamp = timestamp
    if clock_seq is None:
        import random
        clock_seq = random.getrandbits(14)
    if node is None:
        node = getnode()
    fields = (timestamp & 0xffffffff,
              (timestamp >> 32) & 0xffff,
              (timestamp >> 48) & 0x0fff,
              (clock_seq >> 8) & 0x3f,
              clock_seq & 0xff,
              node)
    return UUID(fields=fields, version=1)


def uuid3(namespace, name):
    """Version 3 UUID from the MD5 hash of *namespace* and *name*."""
    if isinstance(name, str):
        name = bytes(name, "utf-8")
    import hashlib
    digest = hashlib.md5(namespace.bytes + name, usedforsecurity=False).digest()
    return UUID(bytes=digest[:16], version=3)


def uuid4():
    """Random (version 4) UUID."""
    value = int.from_bytes(os.urandom(16))
    value = (value & _VARIANT_MASK) | _RFC_4122_VARIANT
    value = (value & _VERSION_MASK) | (4 << 76)
    return UUID._from_int(value)


def uuid5(namespace, name):
    """Version 5 UUID from the SHA-1 hash of *namespace* and *name*."""
    if isinstance(name, str):
        name = bytes(name, "utf-8")
    import hashlib
    digest = hashlib.sha1(namespace.bytes + name, usedforsecurity=False).digest()
    return UUID(bytes=digest[:16], version=5)


_last_timestamp_v6 = None


def uuid6(node=None, clock_seq=None):
    """Version 6 UUID: like version 1 but with the timestamp stored most
    significant bits first, so that values sort by creation time."""
    global _last_timestamp_v6
    import random
    timestamp = time.time_ns() // 100 + _GREGORIAN_OFFSET
    if _last_timestamp_v6 is not None and timestamp <= _last_timestamp_v6:
        timestamp = _last_timestamp_v6 + 1
    _last_timestamp_v6 = timestamp
    if clock_seq is None:
        clock_seq = random.getrandbits(14)
    if node is None:
        node = getnode()
    value = ((timestamp >> 12) & 0xffff_ffff_ffff) << 80
    value |= (timestamp & 0x0fff) << 64
    value |= (clock_seq & 0x3fff) << 48
    value |= node & 0xffff_ffff_ffff
    value = (value & _VARIANT_MASK) | _RFC_4122_VARIANT
    value = (value & _VERSION_MASK) | (6 << 76)
    return UUID._from_int(value)


_last_timestamp_v7 = None
_last_counter_v7 = 0  # 42-bit counter
_COUNTER_V7_MAX = 0x3ff_ffff_ffff


def _uuid7_new_counter_and_tail():
    rand = int.from_bytes(os.urandom(10))
    # The counter's top bit starts at zero to leave room for increments.
    counter = (rand >> 32) & 0x1ff_ffff_ffff
    tail = rand & 0xffff_ffff
    return counter, tail


def uuid7():
    """Version 7 UUID: 48-bit Unix time in milliseconds, a 42-bit counter
    (monotonic within one millisecond) and 32 random bits."""
    global _last_timestamp_v7, _last_counter_v7

    timestamp_ms = time.time_ns() // 1_000_000
    if _last_timestamp_v7 is None or timestamp_ms > _last_timestamp_v7:
        counter, tail = _uuid7_new_counter_and_tail()
    else:
        if timestamp_ms < _last_timestamp_v7:
            # The clock went backwards: keep counting from the last value.
            timestamp_ms = _last_timestamp_v7 + 1
        counter = _last_counter_v7 + 1
        if counter > _COUNTER_V7_MAX:
            # Counter exhausted: move to the next millisecond.
            timestamp_ms += 1
            counter, tail = _uuid7_new_counter_and_tail()
        else:
            tail = int.from_bytes(os.urandom(4))

    unix_ts_ms = timestamp_ms & 0xffff_ffff_ffff
    counter_hi = (counter >> 30) & 0x0fff
    counter_lo = counter & 0x3fff_ffff
    value = unix_ts_ms << 80
    value |= counter_hi << 64
    value |= counter_lo << 32
    value |= tail & 0xffff_ffff
    value = (value & _VARIANT_MASK) | _RFC_4122_VARIANT
    value = (value & _VERSION_MASK) | (7 << 76)
    _last_timestamp_v7 = timestamp_ms
    _last_counter_v7 = counter
    return UUID._from_int(value)


def uuid8(a=None, b=None, c=None):
    """Version 8 (custom) UUID from a 48-bit *a*, 12-bit *b* and 62-bit *c*
    (pseudo-random when omitted)."""
    import random
    if a is None:
        a = random.getrandbits(48)
    if b is None:
        b = random.getrandbits(12)
    if c is None:
        c = random.getrandbits(62)
    value = (a & 0xffff_ffff_ffff) << 80
    value |= (b & 0xfff) << 64
    value |= c & 0x3fff_ffff_ffff_ffff
    value = (value & _VARIANT_MASK) | _RFC_4122_VARIANT
    value = (value & _VERSION_MASK) | (8 << 76)
    return UUID._from_int(value)


def main():
    """Command-line interface: print generated UUIDs."""
    import argparse

    generators = {
        "uuid1": uuid1, "uuid3": uuid3, "uuid4": uuid4, "uuid5": uuid5,
        "uuid6": uuid6, "uuid7": uuid7, "uuid8": uuid8,
    }
    named = ("uuid3", "uuid5")
    namespaces = {
        "@dns": NAMESPACE_DNS, "@url": NAMESPACE_URL,
        "@oid": NAMESPACE_OID, "@x500": NAMESPACE_X500,
    }

    parser = argparse.ArgumentParser(
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
        description="Print newly generated UUIDs.",
        color=True,
    )
    parser.add_argument("-u", "--uuid", choices=generators.keys(),
                        default="uuid4",
                        help="function to generate the UUID")
    parser.add_argument("-n", "--namespace",
                        help="uuid3/uuid5 only: a UUID, or one of the "
                             "predefined namespaces @dns, @url, @oid, @x500")
    parser.add_argument("-N", "--name",
                        help="uuid3/uuid5 only: the name to hash")
    parser.add_argument("-C", "--count", metavar="NUM", type=int, default=1,
                        help="generate NUM fresh UUIDs")

    args = parser.parse_args()
    func = generators[args.uuid]
    if args.uuid in named:
        if not (args.namespace and args.name):
            parser.error(
                "Incorrect number of arguments: "
                f"{args.uuid} needs both --namespace and --name."
            )
        namespace = namespaces.get(args.namespace)
        if namespace is None:
            try:
                namespace = UUID(args.namespace)
            except ValueError as e:
                parser.error(str(e))
        for _ in range(args.count):
            print(func(namespace, args.name))
    else:
        for _ in range(args.count):
            print(func())


# Namespaces from RFC 9562 appendix C.
NAMESPACE_DNS = UUID('6ba7b810-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_URL = UUID('6ba7b811-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_OID = UUID('6ba7b812-9dad-11d1-80b4-00c04fd430c8')
NAMESPACE_X500 = UUID('6ba7b814-9dad-11d1-80b4-00c04fd430c8')

# Special values from RFC 9562 section 5.9 / 5.10.
NIL = UUID('00000000-0000-0000-0000-000000000000')
MAX = UUID('ffffffff-ffff-ffff-ffff-ffffffffffff')

if __name__ == "__main__":
    main()
