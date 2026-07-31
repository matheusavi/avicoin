"""The avicoin wire format, implemented independently of the node.

This is a second implementation on purpose. A conformance test that reuses the
node's own encoder cannot catch a bug that is symmetric across encode and
decode, and the wire format is the one contract this project promises. If this
file and `src/messages/` ever disagree, that is the suite working.
"""

import struct
from dataclasses import dataclass
from hashlib import sha256
from typing import Optional, Tuple

MAGIC = bytes([0xF9, 0xBE, 0xB4, 0xD9])
HEADER_LENGTH = 24
COMMAND_LENGTH = 12
MAX_PAYLOAD_SIZE = 32 * 1024 * 1024


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


@dataclass(frozen=True)
class Frame:
    command: str
    nonce: int


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
    assert (
        len(payload) == 8
    ), f"{command} should carry an 8-byte nonce, got {len(payload)}"

    (nonce,) = struct.unpack("<Q", payload)
    return Frame(command=command, nonce=nonce), HEADER_LENGTH + size


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
