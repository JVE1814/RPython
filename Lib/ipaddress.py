"""Manipulate IPv4 and IPv6 addresses, networks and interfaces.

The factory functions ip_address(), ip_network() and ip_interface() pick the
right class for a value; the classes IPv4Address, IPv4Network, IPv4Interface,
IPv6Address, IPv6Network and IPv6Interface can also be used directly.
"""

import functools

IPV4LENGTH = 32
IPV6LENGTH = 128

__all__ = (
    'IPV4LENGTH', 'IPV6LENGTH', 'AddressValueError', 'NetmaskValueError',
    'ip_address', 'ip_network', 'ip_interface',
    'v4_int_to_packed', 'v6_int_to_packed',
    'summarize_address_range', 'collapse_addresses', 'get_mixed_type_key',
    'IPv4Address', 'IPv4Network', 'IPv4Interface',
    'IPv6Address', 'IPv6Network', 'IPv6Interface',
)


def __getattr__(name):
    if name == '__version__':
        import warnings
        warnings._deprecated('__version__', remove=(3, 20))
        return '1.0'
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


class AddressValueError(ValueError):
    """An address could not be parsed or is out of range."""


class NetmaskValueError(ValueError):
    """A netmask or prefix length is not valid."""


# --------------------------------------------------------------------------
# factory functions

def _first_matching(value, v4_cls, v6_cls, *args):
    for cls in (v4_cls, v6_cls):
        try:
            return cls(value, *args)
        except (AddressValueError, NetmaskValueError):
            pass
    return None


def ip_address(address):
    """Return an IPv4Address or IPv6Address for *address*.

    *address* may be a string, an integer or packed bytes.  Integers below
    2**32 are taken to be IPv4 addresses.
    """
    result = _first_matching(address, IPv4Address, IPv6Address)
    if result is None:
        raise ValueError(
            f'{address!r} does not appear to be an IPv4 or IPv6 address')
    return result


def ip_network(address, strict=True):
    """Return an IPv4Network or IPv6Network for *address*."""
    result = _first_matching(address, IPv4Network, IPv6Network, strict)
    if result is None:
        raise ValueError(
            f'{address!r} does not appear to be an IPv4 or IPv6 network')
    return result


def ip_interface(address):
    """Return an IPv4Interface or IPv6Interface for *address*."""
    result = _first_matching(address, IPv4Interface, IPv6Interface)
    if result is None:
        raise ValueError(
            f'{address!r} does not appear to be an IPv4 or IPv6 interface')
    return result


def v4_int_to_packed(address):
    """Return the 4 byte big-endian representation of an integer."""
    try:
        return address.to_bytes(4, 'big')
    except OverflowError:
        raise ValueError('Address negative or too large for IPv4') from None


def v6_int_to_packed(address):
    """Return the 16 byte big-endian representation of an integer."""
    try:
        return address.to_bytes(16, 'big')
    except OverflowError:
        raise ValueError('Address negative or too large for IPv6') from None


def _split_optional_netmask(address):
    text = str(address)
    pieces = text.split('/')
    if len(pieces) > 2:
        raise AddressValueError(f"Only one '/' permitted in {address!r}")
    return pieces


def _find_address_range(addresses):
    """Yield (first, last) pairs for each run of consecutive addresses.

    *addresses* must be sorted.
    """
    it = iter(addresses)
    try:
        start = end = next(it)
    except StopIteration:
        return
    for addr in it:
        if addr._ip == end._ip + 1:
            end = addr
        else:
            yield start, end
            start = end = addr
    yield start, end


def _count_righthand_zero_bits(number, bits):
    """Number of trailing zero bits of *number*, at most *bits*."""
    if number == 0:
        return bits
    return min(bits, (number & -number).bit_length() - 1)


def _blocks_for_range(first, last, max_prefixlen):
    """Yield (start, prefixlen) CIDR blocks exactly covering [first, last]."""
    while first <= last:
        align = _count_righthand_zero_bits(first, max_prefixlen)
        span = (last - first + 1).bit_length() - 1
        size = min(align, span)
        yield first, max_prefixlen - size
        first += 1 << size


def summarize_address_range(first, last):
    """Yield the networks that exactly cover the range first..last.

    Both arguments must be addresses of the same version.
    """
    if not (isinstance(first, _BaseAddress) and
            isinstance(last, _BaseAddress)):
        raise TypeError('first and last must be IP addresses, not networks')
    if first.version != last.version:
        raise TypeError(f'{first} and {last} are not of the same version')
    if first.version == 4:
        net_cls = IPv4Network
    elif first.version == 6:
        net_cls = IPv6Network
    else:
        raise ValueError('unknown IP version')
    if first > last:
        raise ValueError('last IP address must be greater than first')
    for start, plen in _blocks_for_range(first._ip, last._ip,
                                         first.max_prefixlen):
        yield net_cls((start, plen))


def _check_same_version(a, b):
    if a.version != b.version:
        raise TypeError(f'{a} and {b} are not of the same version')


def _merge_networks(nets):
    """Collapse sorted, same-version networks into a minimal list."""
    kept = []
    last_end = -1
    for net in nets:
        start = net.network_address._ip
        end = start | net._host_bits()
        if kept and end <= last_end:
            continue            # wholly inside something already kept
        kept.append(net)
        last_end = end
    stack = []
    for net in kept:
        stack.append(net)
        while len(stack) >= 2:
            a, b = stack[-2], stack[-1]
            plen = a._prefixlen
            if plen == 0 or b._prefixlen != plen:
                break
            a_start = a.network_address._ip
            b_start = b.network_address._ip
            size = 1 << (a.max_prefixlen - plen)
            if a_start & size or b_start != a_start + size:
                break
            stack[-2:] = [a.supernet()]
    return stack


def collapse_addresses(addresses):
    """Return an iterator over the collapsed networks of *addresses*.

    *addresses* may contain addresses and networks of one IP version.
    """
    nets = []
    first = None
    for item in addresses:
        if first is None:
            first = item
        else:
            _check_same_version(first, item)
        if isinstance(item, _BaseAddress):
            if isinstance(item, (IPv4Interface, IPv6Interface)):
                nets.append(item.network)
            else:
                nets.append(item._network_class((item._ip,
                                                 item.max_prefixlen)))
        elif isinstance(item, _BaseNetwork):
            nets.append(item)
        else:
            raise TypeError(f'{item!r} is not an IP address or network')
    nets.sort(key=lambda n: (n.network_address._ip, n._prefixlen))
    return iter(_merge_networks(nets))


def get_mixed_type_key(obj):
    """Sort key that lets addresses and networks be sorted together."""
    if isinstance(obj, _BaseNetwork):
        return obj._get_networks_key()
    if isinstance(obj, _BaseAddress):
        return obj._get_address_key()
    return NotImplemented


# --------------------------------------------------------------------------
# text <-> integer conversion

_DECIMAL = frozenset('0123456789')
_HEX = frozenset('0123456789abcdefABCDEF')


def _v4_octet(text):
    if not text or not (_DECIMAL.issuperset(text)):
        raise ValueError(f'Only decimal digits permitted in {text!r}')
    if len(text) > 3:
        raise ValueError(f'At most 3 characters permitted in {text!r}')
    if len(text) > 1 and text[0] == '0':
        raise ValueError(f'Leading zeros are not permitted in {text!r}')
    value = int(text)
    if value > 255:
        raise ValueError(f'Octet {value} (> 255) not permitted')
    return value


def _v4_parse(text):
    if not text:
        raise AddressValueError('Address cannot be empty')
    octets = text.split('.')
    if len(octets) != 4:
        raise AddressValueError(f'Expected 4 octets in {text!r}')
    if '' in octets:
        raise AddressValueError(f'Empty octet not permitted in {text!r}')
    value = 0
    try:
        for octet in octets:
            value = (value << 8) | _v4_octet(octet)
    except ValueError as exc:
        raise AddressValueError(f'{exc} in {text!r}') from None
    return value


def _v4_format(value):
    return '.'.join(str((value >> shift) & 0xFF) for shift in (24, 16, 8, 0))


_V6_MAX_TEXT = 45


def _v6_hextet(text):
    if not _HEX.issuperset(text):
        raise ValueError(f'Only hex digits permitted in {text!r}')
    if len(text) > 4:
        raise ValueError(f'At most 4 characters permitted in {text!r}')
    return int(text, 16)


def _v6_parse(text):
    if not text:
        raise AddressValueError('Address cannot be empty')
    if len(text) > _V6_MAX_TEXT:
        raise AddressValueError(
            f'At most {_V6_MAX_TEXT} characters expected in {text!r}')
    parts = text.split(':')
    if len(parts) < 3:
        raise AddressValueError(f'At least 3 parts expected in {text!r}')
    if '.' in parts[-1]:
        try:
            tail = _v4_parse(parts[-1])
        except AddressValueError as exc:
            raise AddressValueError(f'{exc} in {text!r}') from None
        parts[-1:] = ['%x' % (tail >> 16), '%x' % (tail & 0xFFFF)]
    if len(parts) > 9:
        raise AddressValueError(f'At most 8 colons permitted in {text!r}')

    gap = None
    for i in range(1, len(parts) - 1):
        if not parts[i]:
            if gap is not None:
                raise AddressValueError(
                    f"At most one '::' permitted in {text!r}")
            gap = i

    if gap is not None:
        head = parts[:gap]
        tail_parts = parts[gap + 1:]
        if head == ['']:
            head = []
        elif '' in head[:1]:
            raise AddressValueError(
                f"Leading ':' only permitted as part of '::' in {text!r}")
        if tail_parts == ['']:
            tail_parts = []
        elif tail_parts and not tail_parts[-1]:
            raise AddressValueError(
                f"Trailing ':' only permitted as part of '::' in {text!r}")
        missing = 8 - len(head) - len(tail_parts)
        if missing < 1:
            raise AddressValueError(
                f"Expected at most 7 other parts with '::' in {text!r}")
        groups = head + ['0'] * missing + tail_parts
    else:
        if len(parts) != 8:
            raise AddressValueError(
                f"Exactly 8 parts expected without '::' in {text!r}")
        if not parts[0]:
            raise AddressValueError(
                f"Leading ':' only permitted as part of '::' in {text!r}")
        if not parts[-1]:
            raise AddressValueError(
                f"Trailing ':' only permitted as part of '::' in {text!r}")
        groups = parts

    value = 0
    try:
        for group in groups:
            value = (value << 16) | _v6_hextet(group)
    except ValueError as exc:
        raise AddressValueError(f'{exc} in {text!r}') from None
    return value


def _v6_groups(value):
    return [(value >> shift) & 0xFFFF for shift in range(112, -1, -16)]


def _v6_format(value):
    if value >> 32 == 0xFFFF:
        return '::ffff:' + _v4_format(value & 0xFFFFFFFF)
    groups = _v6_groups(value)
    # find the longest run (length >= 2) of zero groups; first one wins ties
    best_at, best_len = -1, 1
    run_at, run_len = -1, 0
    for i, g in enumerate(groups):
        if g == 0:
            if run_len == 0:
                run_at = i
            run_len += 1
            if run_len > best_len:
                best_at, best_len = run_at, run_len
        else:
            run_len = 0
    texts = ['%x' % g for g in groups]
    if best_at < 0:
        return ':'.join(texts)
    left = ':'.join(texts[:best_at])
    right = ':'.join(texts[best_at + best_len:])
    return left + '::' + right


def _v6_exploded(value):
    if value >> 32 == 0xFFFF:
        return '0000:0000:0000:0000:0000:ffff:' + _v4_format(value & 0xFFFFFFFF)
    return ':'.join('%04x' % g for g in _v6_groups(value))


# --------------------------------------------------------------------------
# common machinery

def _format_integer(value, bits, spec):
    """Implement the b/n/x/X format codes shared by both families."""
    alt = spec.startswith('#')
    if alt:
        spec = spec[1:]
    grouped = spec.startswith('_')
    if grouped:
        spec = spec[1:]
    if len(spec) != 1 or spec not in 'bnxX':
        return None
    kind = spec
    if kind == 'n':
        kind = 'b' if bits == 32 else 'x'
    width = bits if kind == 'b' else bits // 4
    if grouped:
        width += width // 4 - 1
    body = format(value, '0%d%s%s' % (width, '_' if grouped else '', kind))
    if alt:
        body = ('0b' if kind == 'b' else '0X' if kind == 'X' else '0x') + body
    return body


class _IPAddressBase:
    """Behaviour shared by addresses, networks and interfaces."""

    __slots__ = ()

    @classmethod
    def _ip_int_from_string(cls, text):
        return cls._parse(text)

    @classmethod
    def _string_from_ip_int(cls, value):
        if not 0 <= value <= cls._ALL_ONES:
            raise ValueError(f'IPv{cls.version} address is out of range')
        return cls._format(value)

    @classmethod
    def _prefix_from_prefix_string(cls, text):
        if not (text.isascii() and text.isdigit()):
            raise NetmaskValueError(f'{text!r} is not a valid netmask')
        plen = int(text)
        if plen > cls.max_prefixlen:
            raise NetmaskValueError(f'{text!r} is not a valid netmask')
        return plen

    @classmethod
    def _prefix_from_ip_int(cls, value):
        """Prefix length of a contiguous netmask; ValueError otherwise."""
        bits = cls.max_prefixlen
        zeros = _count_righthand_zero_bits(value, bits)
        plen = bits - zeros
        if value != (cls._ALL_ONES ^ ((1 << zeros) - 1)):
            raise ValueError('not a netmask')
        return plen

    @classmethod
    def _make_netmask(cls, mask):
        """Return (netmask address, prefix length) for a mask argument."""
        cache = cls._netmask_cache
        hit = cache.get(mask) if isinstance(mask, (int, str)) else None
        if hit is not None:
            return hit
        if isinstance(mask, int):
            plen = mask
            if not 0 <= plen <= cls.max_prefixlen:
                raise NetmaskValueError(f'{mask!r} is not a valid netmask')
        else:
            plen = cls._prefix_from_mask_text(mask)
        netmask = cls._address_class(
            cls._ALL_ONES ^ ((1 << (cls.max_prefixlen - plen)) - 1))
        result = (netmask, plen)
        cache[mask] = result
        return result

    @classmethod
    def _prefix_from_mask_text(cls, text):
        return cls._prefix_from_prefix_string(text)

    @classmethod
    def _split_addr_prefix(cls, address):
        if isinstance(address, (bytes, int)):
            return address, cls.max_prefixlen
        if isinstance(address, tuple):
            if len(address) == 1:
                return address[0], cls.max_prefixlen
            return address
        pieces = _split_optional_netmask(address)
        if len(pieces) == 1:
            return pieces[0], cls.max_prefixlen
        return pieces[0], pieces[1]

    @property
    def exploded(self):
        """The longhand text form."""
        return self._explode_shorthand_ip_string()

    @property
    def compressed(self):
        """The shorthand text form."""
        return str(self)

    @property
    def reverse_pointer(self):
        """The name used for the reverse DNS PTR record."""
        return self._reverse_pointer()

    def __reduce__(self):
        return self.__class__, (str(self),)


class _BaseAddress(_IPAddressBase):
    """A single IP address."""

    __slots__ = ()

    def _init_from(self, address):
        if isinstance(address, int):
            if address < 0:
                raise AddressValueError(
                    f'{address} (< 0) is not permitted as an '
                    f'IPv{self.version} address')
            if address > self._ALL_ONES:
                raise AddressValueError(
                    f'{address} (>= 2**{self.max_prefixlen}) is not '
                    f'permitted as an IPv{self.version} address')
            self._ip = address
            return None
        if isinstance(address, bytes):
            size = self.max_prefixlen // 8
            if len(address) != size:
                raise AddressValueError(
                    f'{address!r} (len {len(address)} != {size}) is not '
                    f'permitted as an IPv{self.version} address')
            self._ip = int.from_bytes(address, 'big')
            return None
        text = str(address)
        if '/' in text:
            raise AddressValueError(f"Unexpected '/' in {address!r}")
        return text

    def __int__(self):
        return self._ip

    def __eq__(self, other):
        try:
            return self._ip == other._ip and self.version == other.version
        except AttributeError:
            return NotImplemented

    def __lt__(self, other):
        if not isinstance(other, _BaseAddress):
            return NotImplemented
        if self.version != other.version:
            raise TypeError(f'{self} and {other} are not of the same version')
        return self._ip < other._ip

    def __le__(self, other):
        if not isinstance(other, _BaseAddress):
            return NotImplemented
        less = self.__lt__(other)
        return less or self == other

    def __gt__(self, other):
        if not isinstance(other, _BaseAddress):
            return NotImplemented
        less = self.__lt__(other)
        return not less and self != other

    def __ge__(self, other):
        if not isinstance(other, _BaseAddress):
            return NotImplemented
        return not self.__lt__(other)

    def __hash__(self):
        return hash(hex(self._ip))

    def __add__(self, other):
        if not isinstance(other, int):
            return NotImplemented
        return self.__class__(self._ip + other)

    def __sub__(self, other):
        if not isinstance(other, int):
            return NotImplemented
        return self.__class__(self._ip - other)

    def __repr__(self):
        return f"{self.__class__.__name__}('{self}')"

    def __str__(self):
        return self._format(self._ip)

    def __format__(self, spec):
        if not spec or spec == 's':
            return str(self)
        text = _format_integer(self._ip, self.max_prefixlen, spec)
        if text is None:
            return super().__format__(spec)
        return text

    def _get_address_key(self):
        return (self.version, self)

    def _explode_shorthand_ip_string(self):
        return str(self)


class _BaseNetwork(_IPAddressBase):
    """An IP network: an address together with a prefix length."""

    def _setup(self, address, strict):
        addr, mask = self._split_addr_prefix(address)
        self.network_address = self._address_class(addr)
        self.netmask, self._prefixlen = self._make_netmask(mask)
        value = self.network_address._ip
        masked = value & self.netmask._ip
        if masked != value:
            if strict:
                raise ValueError(f'{self} has host bits set')
            self.network_address = self._address_class(masked)

    def _host_bits(self):
        return (1 << (self.max_prefixlen - self._prefixlen)) - 1

    @property
    def prefixlen(self):
        return self._prefixlen

    @functools.cached_property
    def broadcast_address(self):
        return self._address_class(self.network_address._ip | self._host_bits())

    @functools.cached_property
    def hostmask(self):
        return self._address_class(self._host_bits())

    @property
    def num_addresses(self):
        """Number of addresses in the network."""
        return self._host_bits() + 1

    @property
    def with_prefixlen(self):
        return f'{self.network_address}/{self._prefixlen}'

    @property
    def with_netmask(self):
        return f'{self.network_address}/{self.netmask}'

    @property
    def with_hostmask(self):
        return f'{self.network_address}/{self.hostmask}'

    def _explode_shorthand_ip_string(self):
        return f'{self.network_address.exploded}/{self._prefixlen}'

    def __repr__(self):
        return f"{self.__class__.__name__}('{self}')"

    def __str__(self):
        return f'{self.network_address}/{self._prefixlen}'

    def __iter__(self):
        start = self.network_address._ip
        cls = self._address_class
        for value in range(start, start + self._host_bits() + 1):
            yield cls(value)

    def __getitem__(self, n):
        start = self.network_address._ip
        last = start | self._host_bits()
        if n >= 0:
            if start + n > last:
                raise IndexError('address out of range')
            return self._address_class(start + n)
        if last + n + 1 < start:
            raise IndexError('address out of range')
        return self._address_class(last + n + 1)

    def __lt__(self, other):
        if not isinstance(other, _BaseNetwork):
            return NotImplemented
        if self.version != other.version:
            raise TypeError(f'{self} and {other} are not of the same version')
        if self.network_address != other.network_address:
            return self.network_address < other.network_address
        if self.netmask != other.netmask:
            return self.netmask < other.netmask
        return False

    def __le__(self, other):
        if not isinstance(other, _BaseNetwork):
            return NotImplemented
        return self.__lt__(other) or self == other

    def __gt__(self, other):
        if not isinstance(other, _BaseNetwork):
            return NotImplemented
        return not self.__lt__(other) and self != other

    def __ge__(self, other):
        if not isinstance(other, _BaseNetwork):
            return NotImplemented
        return not self.__lt__(other)

    def __eq__(self, other):
        try:
            return (self.version == other.version and
                    self.network_address == other.network_address and
                    int(self.netmask) == int(other.netmask))
        except AttributeError:
            return NotImplemented

    def __hash__(self):
        return hash((int(self.network_address), int(self.netmask)))

    def __contains__(self, other):
        if isinstance(other, _BaseNetwork):
            return False
        if self.version != other.version:
            return False
        return other._ip & self.netmask._ip == self.network_address._ip

    def overlaps(self, other):
        """True if either network contains the other's endpoints."""
        return (self.network_address in other or
                self.broadcast_address in other or
                other.network_address in self or
                other.broadcast_address in self)

    def _get_networks_key(self):
        return (self.version, self.network_address, self.netmask)

    def compare_networks(self, other):
        """Return -1, 0 or 1 comparing network address, then netmask."""
        if self.version != other.version:
            raise TypeError(f'{self} and {other} are not of the same version')
        a, b = self.network_address._ip, other.network_address._ip
        if a != b:
            return -1 if a < b else 1
        a, b = self.netmask._ip, other.netmask._ip
        if a != b:
            return -1 if a < b else 1
        return 0

    def address_exclude(self, other):
        """Yield the networks left after removing *other* from self."""
        if self.version != other.version:
            raise TypeError(f'{self} and {other} are not of the same version')
        if not isinstance(other, _BaseNetwork):
            raise TypeError(f'{other} is not a network object')
        if not other.subnet_of(self):
            raise ValueError(f'{other} not contained in {self}')
        if other == self:
            return
        target = other._address_class(other.network_address._ip)
        target_net = self.__class__((target._ip, other._prefixlen))
        current = self
        while current._prefixlen < target_net._prefixlen:
            low, high = current.subnets()
            if target_net.subnet_of(low):
                yield high
                current = low
            else:
                yield low
                current = high

    def subnets(self, prefixlen_diff=1, new_prefix=None):
        """Yield the subnets that partition this network."""
        if self._prefixlen == self.max_prefixlen:
            yield self
            return
        if new_prefix is not None:
            if new_prefix < self._prefixlen:
                raise ValueError('new prefix must be longer')
            if prefixlen_diff != 1:
                raise ValueError('cannot set prefixlen_diff and new_prefix')
            prefixlen_diff = new_prefix - self._prefixlen
        if prefixlen_diff < 0:
            raise ValueError('prefix length diff must be > 0')
        new_len = self._prefixlen + prefixlen_diff
        if new_len > self.max_prefixlen:
            raise ValueError(
                f'prefix length diff {prefixlen_diff} is invalid for '
                f'netblock {self}')
        start = self.network_address._ip
        end = start | self._host_bits()
        step = 1 << (self.max_prefixlen - new_len)
        cls = self.__class__
        for value in range(start, end + 1, step):
            yield cls((value, new_len))

    def supernet(self, prefixlen_diff=1, new_prefix=None):
        """Return the network containing this one with a shorter prefix."""
        if self._prefixlen == 0:
            return self
        if new_prefix is not None:
            if new_prefix > self._prefixlen:
                raise ValueError('new prefix must be shorter')
            if prefixlen_diff != 1:
                raise ValueError('cannot set prefixlen_diff and new_prefix')
            prefixlen_diff = self._prefixlen - new_prefix
        new_len = self._prefixlen - prefixlen_diff
        if new_len < 0:
            raise ValueError(
                f'current prefixlen is {self._prefixlen}, cannot have a '
                f'prefixlen_diff of {prefixlen_diff}')
        return self.__class__((self.network_address._ip, new_len),
                              strict=False)

    def _check_family(self, other):
        try:
            if self.version != other.version:
                raise TypeError(
                    f'{self} and {other} are not of the same version')
            return (other.network_address._ip,
                    other.network_address._ip | other._host_bits())
        except AttributeError:
            raise TypeError(
                'Unable to test subnet containment between '
                f'{self} and {other}') from None

    def subnet_of(self, other):
        """True if this network lies inside *other*."""
        lo, hi = self._check_family(other)
        mine = self.network_address._ip
        return lo <= mine and (mine | self._host_bits()) <= hi

    def supernet_of(self, other):
        """True if *other* lies inside this network."""
        lo, hi = self._check_family(other)
        mine = self.network_address._ip
        return mine <= lo and hi <= (mine | self._host_bits())

    def hosts(self):
        """Iterate over the usable host addresses of the network."""
        start = self.network_address._ip
        end = start | self._host_bits()
        cls = self._address_class
        if self._prefixlen < self.max_prefixlen - 1:
            start += 1
            if self.version == 4:
                end -= 1
        return (cls(value) for value in range(start, end + 1))

    def _both(self, name):
        return (getattr(self.network_address, name) and
                getattr(self.broadcast_address, name))

    @property
    def is_multicast(self):
        return self._both('is_multicast')

    @property
    def is_reserved(self):
        return self._both('is_reserved')

    @property
    def is_link_local(self):
        return self._both('is_link_local')

    def _within_any(self, nets):
        bits = self.max_prefixlen
        start = self.network_address._ip
        for net_start, plen in nets:
            if (plen <= self._prefixlen and
                    start >> (bits - plen) == net_start >> (bits - plen)):
                return True
        return False

    def _touches_any(self, nets):
        bits = self.max_prefixlen
        start = self.network_address._ip
        end = start | self._host_bits()
        for net_start, plen in nets:
            net_end = net_start | ((1 << (bits - plen)) - 1)
            if net_start <= end and start <= net_end:
                return True
        return False

    @property
    def is_private(self):
        return (self._within_any(self._not_global) and
                not self._touches_any(self._global_exceptions))

    @property
    def is_global(self):
        return not self.is_private

    @property
    def is_unspecified(self):
        return self._both('is_unspecified')

    @property
    def is_loopback(self):
        return self._both('is_loopback')


class _InterfaceMixin:
    """Shared behaviour of IPv4Interface and IPv6Interface."""

    def _setup_interface(self, address):
        addr, mask = self._split_addr_prefix(address)
        self._address_class.__init__(self, addr)
        self.network = self._network_class((addr, mask), strict=False)
        self.netmask = self.network.netmask
        self._prefixlen = self.network._prefixlen

    @functools.cached_property
    def hostmask(self):
        return self.network.hostmask

    def __str__(self):
        return f'{self._address_class.__str__(self)}/{self._prefixlen}'

    def __eq__(self, other):
        same = self._address_class.__eq__(self, other)
        if same is NotImplemented or not same:
            return same
        try:
            return self.network == other.network
        except AttributeError:
            return False

    def __lt__(self, other):
        less = self._address_class.__lt__(self, other)
        if less is NotImplemented:
            return NotImplemented
        try:
            return (self.network < other.network or
                    self.network == other.network and less)
        except AttributeError:
            return False

    def __le__(self, other):
        less = self.__lt__(other)
        if less is NotImplemented:
            return NotImplemented
        return less or self == other

    def __gt__(self, other):
        less = self.__lt__(other)
        if less is NotImplemented:
            return NotImplemented
        return not less and self != other

    def __ge__(self, other):
        less = self.__lt__(other)
        if less is NotImplemented:
            return NotImplemented
        return not less

    def __hash__(self):
        return hash((self._ip, self._prefixlen,
                     int(self.network.network_address)))

    @property
    def ip(self):
        return self._address_class(self._ip)

    @property
    def with_prefixlen(self):
        return f'{self._format(self._ip)}/{self._prefixlen}'

    @property
    def with_netmask(self):
        return f'{self._format(self._ip)}/{self.netmask}'

    @property
    def with_hostmask(self):
        return f'{self._format(self._ip)}/{self.hostmask}'

    def _explode_shorthand_ip_string(self):
        base = self._address_class._explode_shorthand_ip_string(self)
        return f'{base}/{self._prefixlen}'

    @property
    def is_unspecified(self):
        return (self._address_class.is_unspecified.fget(self) and
                self.network.is_unspecified)


# --------------------------------------------------------------------------
# IPv4

class _V4:
    __slots__ = ()
    version = 4
    _version = 4
    max_prefixlen = IPV4LENGTH
    _max_prefixlen = IPV4LENGTH
    _ALL_ONES = (1 << IPV4LENGTH) - 1
    _netmask_cache = {}

    _parse = staticmethod(_v4_parse)
    _format = staticmethod(_v4_format)

    @classmethod
    def _prefix_from_mask_text(cls, text):
        if text.isascii() and text.isdigit():
            return cls._prefix_from_prefix_string(text)
        try:
            value = _v4_parse(text)
        except AddressValueError:
            raise NetmaskValueError(f'{text!r} is not a valid netmask') from None
        try:
            return cls._prefix_from_ip_int(value)
        except ValueError:
            pass
        try:
            return cls._prefix_from_ip_int(value ^ cls._ALL_ONES)
        except ValueError:
            raise NetmaskValueError(f'{text!r} is not a valid netmask') from None

    def _reverse_pointer(self):
        octets = _v4_format(self._ip).split('.')
        return '.'.join(reversed(octets)) + '.in-addr.arpa'


class IPv4Address(_V4, _BaseAddress):
    """An IPv4 address."""

    __slots__ = ('_ip', '__weakref__')

    def __init__(self, address):
        text = self._init_from(address)
        if text is not None:
            self._ip = _v4_parse(text)

    @property
    def packed(self):
        """The address as 4 big-endian bytes."""
        return self._ip.to_bytes(4, 'big')

    def _in(self, nets):
        value = self._ip
        for start, plen in nets:
            if value >> (32 - plen) == start >> (32 - plen):
                return True
        return False

    @property
    def is_reserved(self):
        return self._in(_V4_RESERVED)

    @property
    def is_private(self):
        return self._in(_V4_NOT_GLOBAL) and not self._in(_V4_GLOBAL_EXCEPTIONS)

    @property
    def is_global(self):
        return not self._in(_V4_SHARED) and not self.is_private

    @property
    def is_multicast(self):
        return self._in(_V4_MULTICAST)

    @property
    def is_unspecified(self):
        return self._ip == 0

    @property
    def is_loopback(self):
        return self._in(_V4_LOOPBACK)

    @property
    def is_link_local(self):
        return self._in(_V4_LINK_LOCAL)

    @property
    def ipv6_mapped(self):
        """The IPv4-mapped IPv6 address (::ffff:a.b.c.d)."""
        return IPv6Address((0xFFFF << 32) | self._ip)


class IPv4Network(_V4, _BaseNetwork):
    """An IPv4 network such as 192.0.2.0/24."""

    _address_class = IPv4Address

    def __init__(self, address, strict=True):
        self._setup(address, strict)
        if self._prefixlen == self.max_prefixlen - 1:
            self.hosts = self.__iter__
        elif self._prefixlen == self.max_prefixlen:
            self.hosts = lambda: iter((IPv4Address(self.network_address._ip),))

    @property
    def is_global(self):
        shared = IPv4Network('100.64.0.0/10')
        return (not (self.network_address in shared and
                     self.broadcast_address in shared) and
                not self.is_private)


class IPv4Interface(_InterfaceMixin, IPv4Address):
    """An IPv4 address together with the network it belongs to."""

    _address_class = IPv4Address
    _network_class = IPv4Network

    def __init__(self, address):
        self._setup_interface(address)

    __reduce__ = _IPAddressBase.__reduce__


IPv4Address._network_class = IPv4Network
IPv4Address._address_class = IPv4Address


def _nets(*specs):
    out = []
    for spec in specs:
        addr, plen = spec.split('/')
        out.append((_v4_parse(addr), int(plen)))
    return tuple(out)


_V4_NOT_GLOBAL = _nets(
    '0.0.0.0/8', '10.0.0.0/8', '127.0.0.0/8', '169.254.0.0/16',
    '172.16.0.0/12', '192.0.0.0/24', '192.0.2.0/24', '192.168.0.0/16',
    '198.18.0.0/15', '198.51.100.0/24', '203.0.113.0/24', '240.0.0.0/4',
    '255.255.255.255/32')
_V4_GLOBAL_EXCEPTIONS = _nets('192.0.0.9/32', '192.0.0.10/32')
_V4_SHARED = _nets('100.64.0.0/10')
_V4_RESERVED = _nets('240.0.0.0/4')
_V4_MULTICAST = _nets('224.0.0.0/4')
_V4_LOOPBACK = _nets('127.0.0.0/8')
_V4_LINK_LOCAL = _nets('169.254.0.0/16')


# --------------------------------------------------------------------------
# IPv6

class _V6:
    __slots__ = ()
    version = 6
    _version = 6
    max_prefixlen = IPV6LENGTH
    _max_prefixlen = IPV6LENGTH
    _ALL_ONES = (1 << IPV6LENGTH) - 1
    _netmask_cache = {}

    _parse = staticmethod(_v6_parse)
    _format = staticmethod(_v6_format)

    def _reverse_pointer(self):
        digits = '%032x' % self._ip
        return '.'.join(reversed(digits)) + '.ip6.arpa'


def _split_scope(text):
    addr, sep, scope = text.partition('%')
    if sep and (not scope or '%' in scope):
        raise AddressValueError(f'Invalid IPv6 address: "{text!r}"')
    return addr, (scope if sep else None)


class IPv6Address(_V6, _BaseAddress):
    """An IPv6 address, optionally with a scope id (fe80::1%eth0)."""

    __slots__ = ('_ip', '_scope_id', '__weakref__')

    def __init__(self, address):
        self._scope_id = None
        text = self._init_from(address)
        if text is not None:
            text, self._scope_id = _split_scope(text)
            self._ip = _v6_parse(text)

    def __str__(self):
        text = _v6_format(self._ip)
        if self._scope_id is not None:
            text = f'{text}%{self._scope_id}'
        return text

    def _explode_shorthand_ip_string(self):
        text = _v6_exploded(self._ip)
        if self._scope_id is not None:
            text = f'{text}%{self._scope_id}'
        return text

    def __hash__(self):
        return hash((self._ip, self._scope_id))

    def __eq__(self, other):
        same = super().__eq__(other)
        if same is NotImplemented or not same:
            return same
        return self._scope_id == getattr(other, '_scope_id', None)

    def __reduce__(self):
        return self.__class__, (str(self),)

    @property
    def scope_id(self):
        """The scope id (zone index) or None."""
        return self._scope_id

    @property
    def packed(self):
        """The address as 16 big-endian bytes."""
        return self._ip.to_bytes(16, 'big')

    def _in(self, nets):
        value = self._ip
        for start, plen in nets:
            if value >> (128 - plen) == start >> (128 - plen):
                return True
        return False

    def _mapped_or(self, name, nets):
        mapped = self.ipv4_mapped
        if mapped is not None:
            return getattr(mapped, name)
        return self._in(nets)

    @property
    def is_multicast(self):
        return self._mapped_or('is_multicast', _V6_MULTICAST)

    @property
    def is_reserved(self):
        return self._mapped_or('is_reserved', _V6_RESERVED)

    @property
    def is_link_local(self):
        return self._mapped_or('is_link_local', _V6_LINK_LOCAL)

    @property
    def is_site_local(self):
        """True for the deprecated site-local range fec0::/10."""
        return self._in(_V6_SITE_LOCAL)

    @property
    def is_private(self):
        mapped = self.ipv4_mapped
        if mapped is not None:
            return mapped.is_private
        return self._in(_V6_NOT_GLOBAL) and not self._in(_V6_GLOBAL_EXCEPTIONS)

    @property
    def is_global(self):
        mapped = self.ipv4_mapped
        if mapped is not None:
            return mapped.is_global
        return not self.is_private

    @property
    def is_unspecified(self):
        mapped = self.ipv4_mapped
        if mapped is not None:
            return mapped.is_unspecified
        return self._ip == 0

    @property
    def is_loopback(self):
        mapped = self.ipv4_mapped
        if mapped is not None:
            return mapped.is_loopback
        return self._ip == 1

    @property
    def ipv4_mapped(self):
        """The embedded IPv4 address of ::ffff:a.b.c.d, else None."""
        if self._ip >> 32 != 0xFFFF:
            return None
        return IPv4Address(self._ip & 0xFFFFFFFF)

    @property
    def teredo(self):
        """(server, client) IPv4 addresses for a Teredo address, else None."""
        if self._ip >> 96 != 0x20010000:
            return None
        return (IPv4Address((self._ip >> 64) & 0xFFFFFFFF),
                IPv4Address(~self._ip & 0xFFFFFFFF))

    @property
    def sixtofour(self):
        """The IPv4 address embedded in a 6to4 address, else None."""
        if self._ip >> 112 != 0x2002:
            return None
        return IPv4Address((self._ip >> 80) & 0xFFFFFFFF)


class IPv6Network(_V6, _BaseNetwork):
    """An IPv6 network such as 2001:db8::/32."""

    _address_class = IPv6Address

    def __init__(self, address, strict=True):
        self._setup(address, strict)
        if self._prefixlen == self.max_prefixlen - 1:
            self.hosts = self.__iter__
        elif self._prefixlen == self.max_prefixlen:
            self.hosts = lambda: iter((IPv6Address(self.network_address._ip),))

    @property
    def is_site_local(self):
        return self._both('is_site_local')


class IPv6Interface(_InterfaceMixin, IPv6Address):
    """An IPv6 address together with the network it belongs to."""

    _address_class = IPv6Address
    _network_class = IPv6Network

    def __init__(self, address):
        self._setup_interface(address)

    def __str__(self):
        return f'{IPv6Address.__str__(self)}/{self._prefixlen}'

    __reduce__ = _IPAddressBase.__reduce__

    @property
    def is_loopback(self):
        return IPv6Address.is_loopback.fget(self) and self.network.is_loopback


IPv6Address._network_class = IPv6Network
IPv6Address._address_class = IPv6Address


def _nets6(*specs):
    out = []
    for spec in specs:
        addr, plen = spec.split('/')
        out.append((_v6_parse(addr), int(plen)))
    return tuple(out)


_V6_NOT_GLOBAL = _nets6(
    '::1/128', '::/128', '::ffff:0:0/96', '64:ff9b:1::/48', '100::/64',
    '2001::/23', '2001:db8::/32', '2002::/16', '3fff::/20', '5f00::/16',
    'fc00::/7', 'fe80::/10')
_V6_GLOBAL_EXCEPTIONS = _nets6(
    '2001:1::1/128', '2001:1::2/128', '2001:3::/32', '2001:4:112::/48',
    '2001:20::/28', '2001:30::/28')
_V6_MULTICAST = _nets6('ff00::/8')
_V6_RESERVED = _nets6(
    '::/8', '100::/8', '200::/7', '400::/6', '800::/5', '1000::/4',
    '4000::/3', '6000::/3', '8000::/3', 'a000::/3', 'c000::/3',
    'e000::/4', 'f000::/5', 'f800::/6', 'fe00::/9')
_V6_LINK_LOCAL = _nets6('fe80::/10')
_V6_SITE_LOCAL = _nets6('fec0::/10')

IPv4Network._not_global = _V4_NOT_GLOBAL
IPv4Network._global_exceptions = _V4_GLOBAL_EXCEPTIONS
IPv6Network._not_global = _V6_NOT_GLOBAL
IPv6Network._global_exceptions = _V6_GLOBAL_EXCEPTIONS
