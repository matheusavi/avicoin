"""The avicoin wire format, implemented independently of the node.

This is a second implementation on purpose. A conformance test that reuses the
node's own encoder cannot catch a bug that is symmetric across encode and
decode, and the wire format is the one contract this project promises. If this
file and `src/messages/` ever disagree, that is the suite working.
"""

import ipaddress
import struct
from dataclasses import dataclass
from hashlib import sha256
from typing import Optional, Tuple

MAGIC = b"AVI1"
OTHER_NETWORK_MAGIC = b"AVIT"
HEADER_LENGTH = 24
COMMAND_LENGTH = 12
MAX_PAYLOAD_SIZE = 32 * 1024 * 1024
PROTOCOL_VERSION = 1

NET_ADDRESS_LENGTH = 18
MAX_ADDRESSES = 256

# What each fixed-width command's payload must weigh. A node that sends a
# different number of bytes under one of these names has broken the format, so
# this is an assertion about the node rather than a lookup table. `addr` is
# variable-length and is checked where it is parsed instead.
PAYLOAD_SIZES = {"ping": 8, "pong": 8, "version": 30, "verack": 0, "getaddr": 0}


def hash256(payload: bytes) -> bytes:
    return sha256(sha256(payload).digest()).digest()


def frame(command: str, payload: bytes) -> bytes:
    name = command.encode("ascii")
    if len(name) > COMMAND_LENGTH:
        raise ValueError(f"command {command!r} exceeds {COMMAND_LENGTH} bytes")

    return b"".join(
        [
            MAGIC,
            name.ljust(COMMAND_LENGTH, b"\0"),
            struct.pack("<I", len(payload)),
            hash256(payload)[:4],
            payload,
        ]
    )


def ping(nonce: int) -> bytes:
    return frame("ping", struct.pack("<Q", nonce))


def pong(nonce: int) -> bytes:
    return frame("pong", struct.pack("<Q", nonce))


def version(
    nonce: int, listen_address: str, protocol_version: int = PROTOCOL_VERSION
) -> bytes:
    return frame(
        "version",
        struct.pack("<IQ", protocol_version, nonce) + pack_address(listen_address),
    )


def verack() -> bytes:
    return frame("verack", b"")


def getaddr() -> bytes:
    return frame("getaddr", b"")


def addr(addresses) -> bytes:
    return frame(
        "addr", compact_size(len(addresses)) + b"".join(pack_address(a) for a in addresses)
    )


def compact_size(number: int) -> bytes:
    if number <= 252:
        return struct.pack("<B", number)
    if number <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", number)
    if number <= 0xFFFFFFFF:
        return b"\xfe" + struct.pack("<I", number)
    return b"\xff" + struct.pack("<Q", number)


def read_compact_size(payload: bytes):
    """Returns (value, bytes consumed)."""
    first = payload[0]
    if first == 0xFD:
        return struct.unpack("<H", payload[1:3])[0], 3
    if first == 0xFE:
        return struct.unpack("<I", payload[1:5])[0], 5
    if first == 0xFF:
        return struct.unpack("<Q", payload[1:9])[0], 9
    return first, 1


def pack_address(address: str) -> bytes:
    """16 bytes of IPv6 and a port, with IPv4 mapped in, so one field fits both."""
    host, port = address.rsplit(":", 1)
    parsed = ipaddress.ip_address(host.strip("[]"))
    mapped = (
        ipaddress.IPv6Address(f"::ffff:{parsed}") if parsed.version == 4 else parsed
    )

    return mapped.packed + struct.pack("<H", int(port))


def unpack_address(packed: bytes) -> str:
    mapped = ipaddress.IPv6Address(packed[:16])
    (port,) = struct.unpack("<H", packed[16:18])
    host = mapped.ipv4_mapped

    return f"{host}:{port}" if host is not None else f"[{mapped}]:{port}"


@dataclass(frozen=True)
class Version:
    protocol_version: int
    nonce: int
    listen_address: str


@dataclass(frozen=True)
class Frame:
    command: str
    payload: bytes

    @property
    def nonce(self) -> int:
        assert len(self.payload) == 8, f"a {self.command} carries no bare nonce"
        return struct.unpack("<Q", self.payload)[0]

    def as_addresses(self) -> list:
        assert self.command == "addr", f"a {self.command} is not an addr"
        count, read = read_compact_size(self.payload)
        assert count <= MAX_ADDRESSES, f"node sent {count} addresses"

        body = self.payload[read:]
        assert len(body) == count * NET_ADDRESS_LENGTH, (
            f"{count} addresses claimed, {len(body)} bytes supplied"
        )

        return [
            unpack_address(body[at : at + NET_ADDRESS_LENGTH])
            for at in range(0, len(body), NET_ADDRESS_LENGTH)
        ]

    def as_version(self) -> Version:
        assert self.command == "version", f"a {self.command} is not a version"
        protocol_version, nonce = struct.unpack("<IQ", self.payload[:12])

        return Version(
            protocol_version=protocol_version,
            nonce=nonce,
            listen_address=unpack_address(self.payload[12:]),
        )


def parse(buffer: bytes) -> Tuple[Optional[Frame], int]:
    """Returns (frame, bytes consumed), or (None, 0) if more bytes are needed.

    Every check here is an assertion about the node, not defensive coding: a
    node that emits foreign magic bytes or a checksum that does not cover its
    payload has failed, and should fail loudly at the point of parsing.
    """
    if len(buffer) < HEADER_LENGTH:
        return None, 0

    assert buffer[:4] == MAGIC, f"frame is not on our network: {buffer[:4].hex()}"

    size = struct.unpack("<I", buffer[16:20])[0]
    assert size <= MAX_PAYLOAD_SIZE, f"node emitted a {size}-byte payload"

    if len(buffer) < HEADER_LENGTH + size:
        return None, 0

    payload = buffer[HEADER_LENGTH : HEADER_LENGTH + size]
    assert (
        hash256(payload)[:4] == buffer[20:24]
    ), "checksum does not cover the payload it was sent with"

    command = buffer[4 : 4 + COMMAND_LENGTH].rstrip(b"\0").decode("ascii")
    if command != "addr":
        assert command in PAYLOAD_SIZES, f"node emitted an unknown command {command!r}"
        assert len(payload) == PAYLOAD_SIZES[command], (
            f"a {command} should carry {PAYLOAD_SIZES[command]} bytes, "
            f"got {len(payload)}"
        )

    return Frame(command=command, payload=payload), HEADER_LENGTH + size


def parse_all(buffer: bytes) -> list:
    frames = []
    rest = buffer

    while True:
        parsed, consumed = parse(rest)
        if parsed is None:
            break
        frames.append(parsed)
        rest = rest[consumed:]

    return frames
