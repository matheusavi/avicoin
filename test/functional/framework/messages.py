"""The avicoin wire format, implemented independently of the node.

This is a second implementation on purpose. A conformance test that reuses the
node's own encoder cannot catch a bug that is symmetric across encode and
decode, and the wire format is the one contract this project promises. If this
file and `src/messages/` ever disagree, that is the suite working.
"""

import ipaddress
import struct
from dataclasses import dataclass
from hashlib import new as new_hash, sha256
from typing import Optional, Tuple

from ecdsa import SECP256k1, SigningKey
from ecdsa.util import sigencode_string_canonize


def new_ripemd160(data: bytes):
    return new_hash("ripemd160", data)

MAGIC = b"AVI1"
TEST_MAGIC = b"AVIT"
# Kept under its old name for the hostile-peer case, which is about a frame
# arriving on the wrong network rather than about which network we are on.
OTHER_NETWORK_MAGIC = TEST_MAGIC
HEADER_LENGTH = 24
COMMAND_LENGTH = 12
MAX_PAYLOAD_SIZE = 32 * 1024 * 1024
PROTOCOL_VERSION = 1

NET_ADDRESS_LENGTH = 18
MAX_ADDRESSES = 256
MAX_INVENTORY = 1000
INVENTORY_ITEM_LENGTH = 36
TRANSACTION_KIND = 1
BLOCK_KIND = 2
COINBASE_OUTPOINT = b"\x00" * 32 + b"\xff\xff\xff\xff"

# What each fixed-width command's payload must weigh. A node that sends a
# different number of bytes under one of these names has broken the format, so
# this is an assertion about the node rather than a lookup table. `addr` is
# variable-length and is checked where it is parsed instead.
PAYLOAD_SIZES = {"ping": 8, "pong": 8, "version": 30, "verack": 0, "getaddr": 0}
VARIABLE_LENGTH = {"addr", "inv", "getdata", "tx", "block", "headers", "getheaders"}


def hash256(payload: bytes) -> bytes:
    return sha256(sha256(payload).digest()).digest()


def frame(command: str, payload: bytes, magic: bytes = MAGIC) -> bytes:
    name = command.encode("ascii")
    if len(name) > COMMAND_LENGTH:
        raise ValueError(f"command {command!r} exceeds {COMMAND_LENGTH} bytes")

    return b"".join(
        [
            magic,
            name.ljust(COMMAND_LENGTH, b"\0"),
            struct.pack("<I", len(payload)),
            hash256(payload)[:4],
            payload,
        ]
    )


def ping(nonce: int, magic: bytes = MAGIC) -> bytes:
    return frame("ping", struct.pack("<Q", nonce), magic)


def pong(nonce: int, magic: bytes = MAGIC) -> bytes:
    return frame("pong", struct.pack("<Q", nonce), magic)


def version(
    nonce: int,
    listen_address: str,
    protocol_version: int = PROTOCOL_VERSION,
    magic: bytes = MAGIC,
) -> bytes:
    return frame(
        "version",
        struct.pack("<IQ", protocol_version, nonce) + pack_address(listen_address),
        magic,
    )


def verack(magic: bytes = MAGIC) -> bytes:
    return frame("verack", b"", magic)


def getaddr(magic: bytes = MAGIC) -> bytes:
    return frame("getaddr", b"", magic)


def addr(addresses, magic: bytes = MAGIC) -> bytes:
    return frame(
        "addr",
        compact_size(len(addresses)) + b"".join(pack_address(a) for a in addresses),
        magic,
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

    def as_inventory(self) -> list:
        """Every item, as `(kind, hash)`. Nothing is filtered out: a node that
        sends the wrong kind has a bug, and a reader that quietly drops it is
        the reason nobody notices."""
        assert self.command in ("inv", "getdata"), f"a {self.command} is not an inventory"
        count, read = read_compact_size(self.payload)
        assert count <= MAX_INVENTORY, f"node sent {count} items"

        body = self.payload[read:]
        assert len(body) == count * INVENTORY_ITEM_LENGTH, (
            f"{count} items claimed, {len(body)} bytes supplied"
        )

        items = []
        for at in range(0, len(body), INVENTORY_ITEM_LENGTH):
            (kind,) = struct.unpack("<I", body[at : at + 4])
            assert kind in (TRANSACTION_KIND, BLOCK_KIND), f"node sent kind {kind}"
            items.append((kind, body[at + 4 : at + 36]))

        return items

    def blocks_named(self) -> list:
        return [hash for kind, hash in self.as_inventory() if kind == BLOCK_KIND]

    def transactions_named(self) -> list:
        return [hash for kind, hash in self.as_inventory() if kind == TRANSACTION_KIND]

    def as_headers(self) -> list:
        """The eighty-byte headers out of a `headers`, in the order sent."""
        assert self.command == "headers", f"a {self.command} is not headers"
        count, read = read_compact_size(self.payload)

        body = self.payload[read:]
        assert len(body) == count * 80, (
            f"{count} headers claimed, {len(body)} bytes supplied"
        )

        return [body[at : at + 80] for at in range(0, len(body), 80)]

    def as_block_header(self) -> bytes:
        """The eighty bytes proof-of-work covers, out of a `block`."""
        assert self.command == "block", f"a {self.command} is not a block"
        return self.payload[:80]

    def as_version(self) -> Version:
        assert self.command == "version", f"a {self.command} is not a version"
        protocol_version, nonce = struct.unpack("<IQ", self.payload[:12])

        return Version(
            protocol_version=protocol_version,
            nonce=nonce,
            listen_address=unpack_address(self.payload[12:]),
        )


def parse(buffer: bytes, magic: bytes = MAGIC) -> Tuple[Optional[Frame], int]:
    """Returns (frame, bytes consumed), or (None, 0) if more bytes are needed.

    Every check here is an assertion about the node, not defensive coding: a
    node that emits foreign magic bytes or a checksum that does not cover its
    payload has failed, and should fail loudly at the point of parsing.
    """
    if len(buffer) < HEADER_LENGTH:
        return None, 0

    assert buffer[:4] == magic, f"frame is not on our network: {buffer[:4].hex()}"

    size = struct.unpack("<I", buffer[16:20])[0]
    assert size <= MAX_PAYLOAD_SIZE, f"node emitted a {size}-byte payload"

    if len(buffer) < HEADER_LENGTH + size:
        return None, 0

    payload = buffer[HEADER_LENGTH : HEADER_LENGTH + size]
    assert (
        hash256(payload)[:4] == buffer[20:24]
    ), "checksum does not cover the payload it was sent with"

    command = buffer[4 : 4 + COMMAND_LENGTH].rstrip(b"\0").decode("ascii")
    if command not in VARIABLE_LENGTH:
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




def hash160(payload: bytes) -> bytes:
    return new_ripemd160(sha256(payload).digest()).digest()


def compressed_public_key(private_key: bytes) -> bytes:
    point = SigningKey.from_string(private_key, curve=SECP256k1).get_verifying_key().pubkey.point
    prefix = b"\x03" if point.y() & 1 else b"\x02"

    return prefix + point.x().to_bytes(32, "big")


def p2pkh(pubkey_hash: bytes) -> bytes:
    """OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG."""
    return b"\x76\xa9\x14" + pubkey_hash + b"\x88\xac"


def var_bytes(payload: bytes) -> bytes:
    return compact_size(len(payload)) + payload


@dataclass(frozen=True)
class TxIn:
    previous_txid: bytes
    v_out: int
    coinbase_data: bytes = b""
    witness: tuple = ()


@dataclass(frozen=True)
class TxOut:
    value: int
    script_pubkey: bytes


@dataclass(frozen=True)
class Transaction:
    inputs: tuple
    outputs: tuple
    version: int = 1

    def serialize(self, with_witness: bool = True) -> bytes:
        body = struct.pack("<I", self.version) + compact_size(len(self.inputs))
        for txin in self.inputs:
            body += txin.previous_txid + struct.pack("<I", txin.v_out)
            body += var_bytes(txin.coinbase_data)
            if with_witness:
                body += compact_size(len(txin.witness))
                body += b"".join(var_bytes(item) for item in txin.witness)

        body += compact_size(len(self.outputs))
        for txout in self.outputs:
            body += struct.pack("<Q", txout.value) + var_bytes(txout.script_pubkey)

        return body

    def txid(self) -> bytes:
        return hash256(self.serialize(with_witness=False))

    def wtxid(self) -> bytes:
        return hash256(self.serialize())


def sign_input(private_key: bytes, digest: bytes) -> bytes:
    """64 bytes of r ‖ s, normalised to low-S as the node requires."""
    key = SigningKey.from_string(private_key, curve=SECP256k1)
    signature = key.sign_digest(digest, sigencode=sigencode_string_canonize)

    return signature


def inv(txids, magic: bytes = MAGIC) -> bytes:
    return frame("inv", _inventory(txids), magic)


def getdata(txids, magic: bytes = MAGIC) -> bytes:
    return frame("getdata", _inventory(txids), magic)


def tx(transaction: Transaction, magic: bytes = MAGIC) -> bytes:
    return frame("tx", transaction.serialize(), magic)


def getheaders(locator, magic: bytes = MAGIC, stop: bytes = b"\0" * 32) -> bytes:
    return frame(
        "getheaders", compact_size(len(locator)) + b"".join(locator) + stop, magic
    )


def inv_blocks(hashes, magic: bytes = MAGIC) -> bytes:
    return frame("inv", _inventory(hashes, BLOCK_KIND), magic)


def getdata_blocks(hashes, magic: bytes = MAGIC) -> bytes:
    return frame("getdata", _inventory(hashes, BLOCK_KIND), magic)


def _inventory(hashes, kind: int = TRANSACTION_KIND) -> bytes:
    return compact_size(len(hashes)) + b"".join(
        struct.pack("<I", kind) + hash for hash in hashes
    )
