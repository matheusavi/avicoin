"""A block mined on one node reaches another.

The chain becomes observable to a peer here for the first time: a node
announces what it mined, serves what it is asked for, and passes on what it
accepted. Everything is asserted on bytes.
"""

import time

from framework.genesis import coinbase, genesis_hash, mine, subsidy
from framework.messages import (
    TEST_MAGIC,
    frame,
    getdata_blocks,
    hash160,
    hash256,
    inv_blocks,
)
from framework.p2p import IMPATIENCE, PATIENCE


def a_block_of_our_own(previous: bytes, height: int = 1, extranonce: int = 0):
    """Built and ground here, not by the node. Every part of it — the merkle
    root, the header layout, the target, the coinbase — is `framework/`'s own
    implementation, so a node that accepts it agrees with a second opinion."""
    paying = coinbase(height, extranonce, hash160(b"a wallet nobody holds"), subsidy(height))

    return mine(previous, [paying], int(time.time()))


def a_mining_node(net, *args: str):
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0", "--mine", *args)
    return node, node.listening_on()


def blocks_offered(peer, window: float = PATIENCE):
    """Every block hash the node offers within the window."""
    deadline = time.monotonic() + window
    offered = []

    while time.monotonic() < deadline:
        for frame in peer.frames_within(0.3):
            if frame.command == "inv":
                offered.extend(frame.blocks_named())
        if offered:
            return offered

    return offered


def test_a_miner_announces_what_it_mined(net):
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()

    assert blocks_offered(peer), "a mining node with a peer says so"


def test_a_node_serves_a_block_it_was_asked_for(net):
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()
    offered = blocks_offered(peer)

    peer.send(getdata_blocks(offered[:1], TEST_MAGIC))

    served = peer.next_frame_of("block")
    assert hash256(served.as_block_header()) == offered[0], (
        "the block we were sent is the block we were offered"
    )


def test_a_node_ignores_an_inv_for_a_block_it_already_has(net):
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()
    offered = blocks_offered(peer)

    peer.send(inv_blocks(offered[:1], TEST_MAGIC))

    asked = [
        item
        for frame in peer.frames_within(IMPATIENCE)
        if frame.command == "getdata"
        for item in frame.as_inventory()
    ]
    assert not asked, f"it mined that one; asking for it back is a loop: {asked}"


def test_a_scripted_peer_mines_a_block_and_the_node_relays_it(net):
    """The acceptance criterion in its literal form: a peer that is not a node
    builds a block, and a second peer sees it without asking."""
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0")
    address = node.listening_on()
    sender = net.dial(address, TEST_MAGIC)
    watcher = net.dial(address, TEST_MAGIC)
    sender.handshake()
    watcher.handshake()

    payload, block_hash = a_block_of_our_own(genesis_hash())
    sender.send(frame("block", payload, TEST_MAGIC))

    assert block_hash in blocks_offered(watcher), "relayed to everyone else"
    assert block_hash not in blocks_offered(sender, IMPATIENCE), "but not back"


def test_a_node_refuses_a_block_that_does_not_meet_its_target(net):
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0")
    address = node.listening_on()
    sender = net.dial(address, TEST_MAGIC)
    watcher = net.dial(address, TEST_MAGIC)
    sender.handshake()
    watcher.handshake()

    payload, _ = a_block_of_our_own(genesis_hash())
    # One nonce off: the same block, unmined.
    unmined = payload[:76] + (int.from_bytes(payload[76:80], "little") + 1).to_bytes(
        4, "little"
    ) + payload[80:]
    sender.send(frame("block", unmined, TEST_MAGIC))

    assert not blocks_offered(watcher, IMPATIENCE), "nothing to pass on"


def test_a_second_node_learns_the_chain_from_the_first(net):
    """One node mines, the other only listens — and ends up holding the same
    blocks, which it can only have got by asking for them."""
    _, first = a_mining_node(net)
    second = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--addresses-to-connect", first
    )

    watching = net.dial(first, TEST_MAGIC)
    watching.handshake()
    mined = blocks_offered(watching)
    assert mined

    onward = net.dial(second.listening_on(), TEST_MAGIC)
    onward.handshake()

    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        onward.send(getdata_blocks(mined[:1], TEST_MAGIC))
        for frame in onward.frames_within(0.5):
            if frame.command == "block" and hash256(frame.as_block_header()) == mined[0]:
                return

    raise AssertionError(f"the second node never held the first's block within {PATIENCE}s")
