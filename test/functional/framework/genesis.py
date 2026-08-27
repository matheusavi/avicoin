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

from .messages import (
    COINBASE_OUTPOINT,
    Transaction,
    TxIn,
    TxOut,
    compressed_public_key,
    hash160,
    p2pkh,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
PARAMS = REPO_ROOT / "params"

ALPHABET = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
VERSION_BYTE = 0x17
GENESIS_MESSAGE = b"Avi Coin test network"
MATURITY = 1


def base58check_decode(text: str) -> bytes:
    """Returns the 20-byte payload, checking the version byte and checksum."""
    from .messages import hash256

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


def funded(index: int = 0):
    """A key, and the outpoint and value of the coin genesis gave it."""
    key = test_keys()[index]
    pubkey_hash = hash160(compressed_public_key(key))
    coinbase = genesis_coinbase()

    for v_out, output in enumerate(coinbase.outputs):
        if output.script_pubkey == p2pkh(pubkey_hash):
            return key, coinbase.txid(), v_out, output.value

    raise AssertionError("the allocation does not fund this key")
