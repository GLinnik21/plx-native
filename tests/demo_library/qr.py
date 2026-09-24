"""Pure-Python, dependency-free QR-code encoder (byte mode subset of ISO/IEC 18004).

Written for a mock server in a test suite that needs to serve a plausible
sign-in QR code without pulling in a third-party dependency (qrcode, segno,
Pillow, ...). It implements just enough of the standard to produce a
spec-correct symbol for arbitrary bytes: byte-mode data encoding, ECC levels
L/M/Q/H, automatic version selection over versions 1..10, Reed-Solomon error
correction, codeword interleaving, all function patterns, format/version
information, and standard mask selection by penalty score. It intentionally
does not implement numeric/alphanumeric/kanji modes, ECI, or versions above
10 -- those are unnecessary for a short URL/token and would roughly double
the size of this file.

Two public functions:

    encode(data: bytes, ecc: str = "M") -> list[list[bool]]
        Returns the module matrix, True meaning "dark module".

    png(matrix, scale: int = 8, border: int = 4) -> bytes
        Renders the matrix as a deterministic 8-bit grayscale PNG.
"""

from __future__ import annotations

import struct
import zlib

# ---------------------------------------------------------------------------
# Per-(version, ECC level) block structure: (ec_codewords_per_block, groups)
# where groups is a list of (block_count, data_codewords_per_block).
# This is Table 9 of ISO/IEC 18004, versions 1..10 only. Every entry here
# satisfies sum(count*data_len) + ec_per_block*sum(count) == total codewords
# for that version (26, 44, 70, 100, 134, 172, 196, 242, 292, 346).
# ---------------------------------------------------------------------------
_BLOCK_TABLE = {
    1: {"L": (7, [(1, 19)]), "M": (10, [(1, 16)]), "Q": (13, [(1, 13)]), "H": (17, [(1, 9)])},
    2: {"L": (10, [(1, 34)]), "M": (16, [(1, 28)]), "Q": (22, [(1, 22)]), "H": (28, [(1, 16)])},
    3: {"L": (15, [(1, 55)]), "M": (26, [(1, 44)]), "Q": (18, [(2, 17)]), "H": (22, [(2, 13)])},
    4: {"L": (20, [(1, 80)]), "M": (18, [(2, 32)]), "Q": (26, [(2, 24)]), "H": (16, [(4, 9)])},
    5: {"L": (26, [(1, 108)]), "M": (24, [(2, 43)]), "Q": (18, [(2, 15), (2, 16)]), "H": (22, [(2, 11), (2, 12)])},
    6: {"L": (18, [(2, 68)]), "M": (16, [(4, 27)]), "Q": (24, [(4, 19)]), "H": (28, [(4, 15)])},
    7: {"L": (20, [(2, 78)]), "M": (18, [(4, 31)]), "Q": (18, [(2, 14), (4, 15)]), "H": (26, [(4, 13), (1, 14)])},
    8: {"L": (24, [(2, 97)]), "M": (22, [(2, 38), (2, 39)]), "Q": (22, [(4, 18), (2, 19)]), "H": (26, [(4, 14), (2, 15)])},
    9: {"L": (30, [(2, 116)]), "M": (22, [(3, 36), (2, 37)]), "Q": (20, [(4, 16), (4, 17)]), "H": (24, [(4, 12), (4, 13)])},
    10: {"L": (18, [(2, 68), (2, 69)]), "M": (26, [(4, 43), (1, 44)]), "Q": (24, [(6, 19), (2, 20)]), "H": (28, [(6, 15), (2, 16)])},
}

# Format-info ECC-level indicator bits, per spec Table 25.
_ECC_BITS = {"L": 1, "M": 0, "Q": 3, "H": 2}


def _total_data_codewords(version: int, ecc: str) -> int:
    ec_per_block, groups = _BLOCK_TABLE[version][ecc]
    return sum(count * dlen for count, dlen in groups)


def _select_version(data_len: int, ecc: str) -> int:
    """Smallest version 1..10 whose byte-mode capacity fits `data_len` bytes."""
    for version in range(1, 11):
        # Character-count indicator is 8 bits for versions 1-9, 16 for 10-26.
        cci_bits = 8 if version <= 9 else 16
        capacity_bits = _total_data_codewords(version, ecc) * 8
        required_bits = 4 + cci_bits + 8 * data_len  # mode + count + payload
        if required_bits <= capacity_bits:
            return version
    raise ValueError(
        f"{data_len} bytes at ECC {ecc} do not fit in any byte-mode version 1..10"
    )


# ---------------------------------------------------------------------------
# Bit buffer
# ---------------------------------------------------------------------------
class _BitBuffer:
    __slots__ = ("bits",)

    def __init__(self):
        self.bits: list[int] = []

    def append(self, value: int, length: int) -> None:
        for i in range(length - 1, -1, -1):
            self.bits.append((value >> i) & 1)

    def __len__(self) -> int:
        return len(self.bits)

    def to_bytes(self) -> bytes:
        assert len(self.bits) % 8 == 0
        out = bytearray()
        for i in range(0, len(self.bits), 8):
            byte = 0
            for b in self.bits[i : i + 8]:
                byte = (byte << 1) | b
            out.append(byte)
        return bytes(out)


def _make_data_segment(data: bytes, version: int, ecc: str) -> bytes:
    cci_bits = 8 if version <= 9 else 16
    capacity_codewords = _total_data_codewords(version, ecc)

    bb = _BitBuffer()
    bb.append(0b0100, 4)  # byte-mode indicator
    bb.append(len(data), cci_bits)
    for byte in data:
        bb.append(byte, 8)

    # Terminator: up to four zero bits, but never past capacity.
    bb.append(0, min(4, capacity_codewords * 8 - len(bb)))
    while len(bb) % 8 != 0:
        bb.append(0, 1)

    pad_bytes = (0xEC, 0x11)
    i = 0
    while len(bb) // 8 < capacity_codewords:
        bb.append(pad_bytes[i % 2], 8)
        i += 1

    return bb.to_bytes()


# ---------------------------------------------------------------------------
# GF(256) arithmetic and Reed-Solomon, using the spec's primitive polynomial
# x^8 + x^4 + x^3 + x^2 + 1 (0x11D) and generator element 2.
# ---------------------------------------------------------------------------
_EXP = [0] * 256
_LOG = [0] * 256
_x = 1
for _i in range(255):
    _EXP[_i] = _x
    _LOG[_x] = _i
    _x <<= 1
    if _x & 0x100:
        _x ^= 0x11D
_EXP[255] = _EXP[0]


def _gf_mul(a: int, b: int) -> int:
    if a == 0 or b == 0:
        return 0
    return _EXP[(_LOG[a] + _LOG[b]) % 255]


def _rs_generator_poly(degree: int) -> list[int]:
    """Monic generator poly (x - a^0)(x - a^1)...(x - a^(degree-1)), highest
    degree coefficient first. Subtraction == addition == XOR in GF(2^8)."""
    poly = [1]
    for i in range(degree):
        # multiply poly by (x + a^i)
        new = [0] * (len(poly) + 1)
        for j, coeff in enumerate(poly):
            new[j] ^= coeff
            new[j + 1] ^= _gf_mul(coeff, _EXP[i])
        poly = new
    return poly


def _rs_encode(data: bytes, generator: list[int]) -> list[int]:
    """Standard polynomial long-division ("shift register") RS encoder."""
    ec_len = len(generator) - 1
    remainder = list(data) + [0] * ec_len
    for i in range(len(data)):
        factor = remainder[i]
        if factor == 0:
            continue
        for j, gcoef in enumerate(generator):
            remainder[i + j] ^= _gf_mul(gcoef, factor)
    return remainder[len(data):]


def _make_codewords(data: bytes, version: int, ecc: str) -> bytes:
    data_codewords = _make_data_segment(data, version, ecc)
    ec_per_block, groups = _BLOCK_TABLE[version][ecc]

    blocks = []
    idx = 0
    for count, dlen in groups:
        for _ in range(count):
            blocks.append(data_codewords[idx : idx + dlen])
            idx += dlen

    generator = _rs_generator_poly(ec_per_block)
    ec_blocks = [_rs_encode(b, generator) for b in blocks]

    result = bytearray()
    max_len = max(len(b) for b in blocks)
    for i in range(max_len):
        for b in blocks:
            if i < len(b):
                result.append(b[i])
    for i in range(ec_per_block):
        for eb in ec_blocks:
            result.append(eb[i])
    return bytes(result)


# ---------------------------------------------------------------------------
# Matrix construction
# ---------------------------------------------------------------------------
def _bit(x: int, i: int) -> bool:
    return ((x >> i) & 1) != 0


def _alignment_positions(version: int, size: int) -> list[int]:
    """Center coordinates for alignment patterns (spec Annex E formula)."""
    if version == 1:
        return []
    num_align = version // 7 + 2
    if version == 32:  # not reachable for versions <= 10, kept for fidelity
        step = 26
    else:
        step = (version * 4 + num_align * 2 + 1) // (num_align * 2 - 2) * 2
    positions = [6]
    pos = size - 7
    for _ in range(num_align - 1):
        positions.insert(1, pos)
        pos -= step
    return positions


def _draw_finder(matrix, isfunc, size, cx, cy) -> None:
    for dy in range(-4, 5):
        for dx in range(-4, 5):
            x, y = cx + dx, cy + dy
            if 0 <= x < size and 0 <= y < size:
                d = max(abs(dx), abs(dy))
                # d in {0,1}: center 3x3 dark; d==2: light ring; d==3: dark
                # border ring; d==4: light one-module separator.
                dark = d != 2 and d <= 3
                matrix[y][x] = dark
                isfunc[y][x] = True


def _draw_alignment(matrix, isfunc, size, cx, cy) -> None:
    for dy in range(-2, 3):
        for dx in range(-2, 3):
            x, y = cx + dx, cy + dy
            d = max(abs(dx), abs(dy))
            matrix[y][x] = d != 1
            isfunc[y][x] = True


def _draw_function_patterns(matrix, isfunc, size, version) -> None:
    for cx, cy in ((3, 3), (size - 4, 3), (3, size - 4)):
        _draw_finder(matrix, isfunc, size, cx, cy)

    for i in range(8, size - 8):
        matrix[6][i] = (i % 2 == 0)
        isfunc[6][i] = True
        matrix[i][6] = (i % 2 == 0)
        isfunc[i][6] = True

    positions = _alignment_positions(version, size)
    if positions:
        skip = {
            (positions[0], positions[0]),
            (positions[0], positions[-1]),
            (positions[-1], positions[0]),
        }
        for row in positions:
            for col in positions:
                if (row, col) in skip:
                    continue
                _draw_alignment(matrix, isfunc, size, col, row)

    # The dark module, always present, position depends on version.
    matrix[size - 8][8] = True
    isfunc[size - 8][8] = True


def _format_bits(ecc: str, mask: int) -> int:
    data = (_ECC_BITS[ecc] << 3) | mask
    rem = data
    for _ in range(10):
        rem = (rem << 1) ^ ((rem >> 9) * 0x537)
    return ((data << 10) | rem) ^ 0x5412


def _version_bits(version: int) -> int:
    rem = version
    for _ in range(12):
        rem = (rem << 1) ^ ((rem >> 11) * 0x1F25)
    return (version << 12) | rem


def _draw_format_bits(matrix, isfunc, size, ecc, mask) -> None:
    bits = _format_bits(ecc, mask)

    def put(x, y, v):
        matrix[y][x] = v
        isfunc[y][x] = True

    for i in range(6):
        put(8, i, _bit(bits, i))
    put(8, 7, _bit(bits, 6))
    put(8, 8, _bit(bits, 7))
    put(7, 8, _bit(bits, 8))
    for i in range(9, 15):
        put(14 - i, 8, _bit(bits, i))

    for i in range(8):
        put(size - 1 - i, 8, _bit(bits, i))
    for i in range(8, 15):
        put(8, size - 15 + i, _bit(bits, i))

    put(8, size - 8, True)  # the fixed dark module, redundant but harmless


def _draw_version_bits(matrix, isfunc, size, version) -> None:
    if version < 7:
        return
    bits = _version_bits(version)
    for i in range(18):
        v = _bit(bits, i)
        a = size - 11 + i % 3
        b = i // 3
        matrix[b][a] = v
        isfunc[b][a] = True
        matrix[a][b] = v
        isfunc[a][b] = True


def _draw_codewords(matrix, isfunc, size, data: bytes) -> None:
    """Zig-zag placement: two-column strips scanned bottom-up then top-down,
    right to left, skipping the vertical timing column."""
    total_bits = len(data) * 8
    bit_index = 0
    right = size - 1
    while right >= 1:
        if right == 6:
            right = 5
        for vert in range(size):
            for j in range(2):
                x = right - j
                upward = ((right + 1) & 2) == 0
                y = (size - 1 - vert) if upward else vert
                if not isfunc[y][x] and bit_index < total_bits:
                    byte = data[bit_index >> 3]
                    matrix[y][x] = _bit(byte, 7 - (bit_index & 7))
                    bit_index += 1
                # else: remainder bit, stays at its initialized False value.
        right -= 2


_MASK_FUNCS = (
    lambda x, y: (x + y) % 2 == 0,
    lambda x, y: y % 2 == 0,
    lambda x, y: x % 3 == 0,
    lambda x, y: (x + y) % 3 == 0,
    lambda x, y: (y // 2 + x // 3) % 2 == 0,
    lambda x, y: (x * y) % 2 + (x * y) % 3 == 0,
    lambda x, y: ((x * y) % 2 + (x * y) % 3) % 2 == 0,
    lambda x, y: ((x + y) % 2 + (x * y) % 3) % 2 == 0,
)


def _penalty(m, size) -> int:
    total = 0

    # Rule 1: runs of >=5 same-colored modules, per row then per column.
    for y in range(size):
        run, prev = 1, m[y][0]
        for x in range(1, size):
            if m[y][x] == prev:
                run += 1
            else:
                if run >= 5:
                    total += 3 + (run - 5)
                run, prev = 1, m[y][x]
        if run >= 5:
            total += 3 + (run - 5)
    for x in range(size):
        run, prev = 1, m[0][x]
        for y in range(1, size):
            if m[y][x] == prev:
                run += 1
            else:
                if run >= 5:
                    total += 3 + (run - 5)
                run, prev = 1, m[y][x]
        if run >= 5:
            total += 3 + (run - 5)

    # Rule 2: each 2x2 block of one color.
    for y in range(size - 1):
        for x in range(size - 1):
            v = m[y][x]
            if v == m[y][x + 1] == m[y + 1][x] == m[y + 1][x + 1]:
                total += 3

    # Rule 3: the 1:1:3:1:1 finder-like ratio, with a 4-module quiet run on
    # either side (the common interpretation used by reference encoders).
    pattern = (True, False, True, True, True, False, True)

    def scan(line):
        n = len(line)
        count = 0
        for i in range(n - 6):
            if tuple(line[i : i + 7]) == pattern:
                before = i >= 4 and all(not v for v in line[i - 4 : i])
                after = i + 11 <= n and all(not v for v in line[i + 7 : i + 11])
                if before or after:
                    count += 1
        return count

    for y in range(size):
        total += 40 * scan(m[y])
    for x in range(size):
        total += 40 * scan([m[y][x] for y in range(size)])

    # Rule 4: overall dark/light balance.
    dark = sum(1 for row in m for v in row if v)
    percent = dark * 100 // (size * size)
    prev5 = percent - percent % 5
    next5 = prev5 + 5
    total += min(abs(prev5 - 50), abs(next5 - 50)) // 5 * 10

    return total


def encode(data: bytes, ecc: str = "M") -> list[list[bool]]:
    if not isinstance(data, (bytes, bytearray)):
        raise TypeError("data must be bytes")
    ecc = ecc.upper()
    if ecc not in _ECC_BITS:
        raise ValueError("ecc must be one of 'L', 'M', 'Q', 'H'")

    version = _select_version(len(data), ecc)
    codewords = _make_codewords(bytes(data), version, ecc)
    size = version * 4 + 17

    matrix = [[False] * size for _ in range(size)]
    isfunc = [[False] * size for _ in range(size)]

    _draw_function_patterns(matrix, isfunc, size, version)
    _draw_format_bits(matrix, isfunc, size, ecc, 0)  # reserves the cells
    _draw_version_bits(matrix, isfunc, size, version)
    _draw_codewords(matrix, isfunc, size, codewords)

    best, best_penalty = None, None
    for mask_id in range(8):
        trial = [row[:] for row in matrix]
        fn = _MASK_FUNCS[mask_id]
        for y in range(size):
            for x in range(size):
                if not isfunc[y][x] and fn(x, y):
                    trial[y][x] = not trial[y][x]
        _draw_format_bits(trial, isfunc, size, ecc, mask_id)
        p = _penalty(trial, size)
        if best_penalty is None or p < best_penalty:
            best, best_penalty = trial, p

    return best


# ---------------------------------------------------------------------------
# Minimal PNG writer: 8-bit grayscale (or grayscale + alpha), filter type 0, zlib level 9.
# ---------------------------------------------------------------------------
def _chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def png(matrix: list[list[bool]], scale: int = 8, border: int = 4, plex_style: bool = False) -> bytes:
    """Black modules on white by default. `plex_style` draws what plex.tv's
    `/api/v2/pins/qr/<code>` serves instead: WHITE modules on a TRANSPARENT ground (grayscale +
    alpha), which the client tints dark over its own white card."""
    n = len(matrix)
    dim = n + 2 * border
    img = dim * scale

    raw = bytearray()
    for y in range(img):
        raw.append(0)  # scanline filter type: None
        my = y // scale - border
        row_in_bounds = 0 <= my < n
        row = matrix[my] if row_in_bounds else None
        for x in range(img):
            mx = x // scale - border
            dark = row_in_bounds and 0 <= mx < n and row[mx]
            if plex_style:
                raw += b"\xff\xff" if dark else b"\xff\x00"
            else:
                raw.append(0 if dark else 255)

    colour = 4 if plex_style else 0  # grayscale + alpha, or grayscale
    ihdr = struct.pack(">IIBBBBB", img, img, 8, colour, 0, 0, 0)
    compressed = zlib.compress(bytes(raw), 9)

    return (
        b"\x89PNG\r\n\x1a\n"
        + _chunk(b"IHDR", ihdr)
        + _chunk(b"IDAT", compressed)
        + _chunk(b"IEND", b"")
    )
