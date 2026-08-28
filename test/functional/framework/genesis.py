"""The test network's genesis block, derived here rather than asked for.

The node derives the same block from the same committed files. Deriving it
independently means a disagreement about Base58Check, HASH160, the P2PKH
template, transaction serialization or the txid shows up as a test that cannot
find its coins -- which is the whole reason `framework/messages.py` is a second
implementation.
"""

from functools import cache
from pathlib import Path
from typing import List, Tuple

import struct

from .messages import (
    COINBASE_OUTPOINT,
    Transaction,
    TxIn,
    TxOut,
    compact_size,
    compressed_public_key,
    hash160,
    hash256,
    p2pkh,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
PARAMS = REPO_ROOT / "params"

ALPHABET = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
VERSION_BYTE = 0x17
GENESIS_MESSAGE = b"Avi Coin test network"
GENESIS_TIME = 1_756_252_800
GENESIS_NONCE = 15
MATURITY = 1


def base58check(payload: bytes) -> str:
    """The inverse, so a test can name an address the node has never seen.

    A second implementation on purpose, like everything else here — a test that
    encoded an address with the node's encoder could not catch the node being
    wrong about addresses.
    """
    body = bytes([VERSION_BYTE]) + payload
    body += hash256(body)[:4]

    number = int.from_bytes(body, "big")
    text = b""
    while number:
        number, digit = divmod(number, 58)
        text = ALPHABET[digit : digit + 1] + text

    return "1" * (len(body) - len(body.lstrip(b"\0"))) + text.decode("ascii")


def base58check_decode(text: str) -> bytes:
    """Returns the 20-byte payload, checking the version byte and checksum."""
    number = 0
    for character in text:
        number = number * 58 + ALPHABET.index(character.encode("ascii"))

    body = number.to_bytes((number.bit_length() + 7) // 8, "big")
    body = b"\0" * (len(text) - len(text.lstrip("1"))) + body

    payload, checksum = body[:-4], body[-4:]
    assert hash256(payload)[:4] == checksum, f"{text} has a bad checksum"
    assert payload[0] == VERSION_BYTE, f"{text} is not an Avi Coin address"

    return payload[1:]


def read_lines(name: str) -> List[str]:
    return [
        line.strip()
        for line in (PARAMS / name).read_text().splitlines()
        if line.strip() and not line.startswith("#")
    ]


@cache
def test_keys() -> Tuple[bytes, ...]:
    return tuple(bytes.fromhex(line) for line in read_lines("testnet.keys"))


@cache
def allocation() -> Tuple[Tuple[bytes, int], ...]:
    entries = []
    for line in read_lines("testnet.allocation"):
        address, atoms = line.split()
        entries.append((base58check_decode(address), int(atoms)))

    return tuple(entries)


@cache
def genesis_coinbase() -> Transaction:
    return Transaction(
        inputs=(
            TxIn(
                previous_txid=COINBASE_OUTPOINT[:32],
                v_out=0xFFFFFFFF,
                coinbase_data=(0).to_bytes(4, "little") + GENESIS_MESSAGE,
            ),
        ),
        outputs=tuple(
            TxOut(value=atoms, script_pubkey=p2pkh(pubkey_hash))
            for pubkey_hash, atoms in allocation()
        ),
    )


STARTING_BITS = 0x2000FFFF
TARGET_BLOCK_TIME = 1


def merkle_root(leaves) -> bytes:
    """Pairs left to right, duplicating the last of an odd level — Bitcoin's
    construction, over wtxids (ADR-0003, ADR-0010)."""
    assert leaves, "a block has at least a coinbase"
    level = list(leaves)

    while len(level) > 1:
        level = [
            hash256(level[at] + level[min(at + 1, len(level) - 1)])
            for at in range(0, len(level), 2)
        ]

    return level[0]


def header_bytes(previous: bytes, root: bytes, time: int, bits: int, nonce: int) -> bytes:
    return (
        struct.pack("<i", 1)
        + previous
        + root
        + struct.pack("<III", time, bits, nonce)
    )


def target_from_bits(bits: int) -> int:
    exponent, mantissa = bits >> 24, bits & 0x00FFFFFF
    assert not mantissa & 0x00800000, "a negative target names nothing"

    return mantissa >> (8 * (3 - exponent)) if exponent < 3 else mantissa << (8 * (exponent - 3))


def mine(previous: bytes, transactions, time: int, bits: int = STARTING_BITS):
    """Grinds a nonce, and returns the framed block payload with its hash.

    Everything here — the merkle root, the header layout, the target — is a
    second implementation. If it disagrees with the node's, the node refuses
    what this builds, which is the suite doing its job.
    """
    root = merkle_root([transaction.wtxid() for transaction in transactions])
    target = target_from_bits(bits)

    for nonce in range(1 << 32):
        header = header_bytes(previous, root, time, bits, nonce)
        if int.from_bytes(hash256(header), "little") < target:
            body = compact_size(len(transactions)) + b"".join(
                transaction.serialize() for transaction in transactions
            )
            return header + body, hash256(header)

    raise AssertionError("no nonce solved a target this easy")


def coinbase(height: int, extranonce: int, pubkey_hash: bytes, atoms: int) -> Transaction:
    return Transaction(
        inputs=(
            TxIn(
                previous_txid=COINBASE_OUTPOINT[:32],
                v_out=0xFFFFFFFF,
                coinbase_data=struct.pack("<I", height) + struct.pack("<Q", extranonce),
            ),
        ),
        outputs=(TxOut(value=atoms, script_pubkey=p2pkh(pubkey_hash)),),
    )


def subsidy(height: int) -> int:
    halvings = height // 20_160
    return 0 if halvings >= 64 else (50 * 100_000_000) >> halvings


@cache
def genesis_hash() -> bytes:
    coinbase = genesis_coinbase()
    return hash256(
        header_bytes(
            b"\0" * 32,
            merkle_root([coinbase.wtxid()]),
            GENESIS_TIME,
            STARTING_BITS,
            GENESIS_NONCE,
        )
    )


def funded(index: int = 0):
    """A key, and the outpoint and value of the coin genesis gave it."""
    key = test_keys()[index]
    pubkey_hash = hash160(compressed_public_key(key))
    coinbase = genesis_coinbase()

    for v_out, output in enumerate(coinbase.outputs):
        if output.script_pubkey == p2pkh(pubkey_hash):
            return key, coinbase.txid(), v_out, output.value

    raise AssertionError("the allocation does not fund this key")
