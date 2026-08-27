"""A block mined on one node reaches another.

The chain becomes observable to a peer here for the first time: a node
announces what it mined, serves what it is asked for, and passes on what it
accepted. Everything is asserted on bytes.
"""

import time

from framework.messages import (
    BLOCK_KIND,
    TEST_MAGIC,
    getdata_blocks,
    hash256,
    inv_blocks,
)
from framework.p2p import IMPATIENCE, PATIENCE


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
                offered.extend(frame.as_inventory(BLOCK_KIND))
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
        frame
        for frame in peer.frames_within(IMPATIENCE)
        if frame.command == "getdata" and frame.as_inventory(BLOCK_KIND)
    ]
    assert not asked, "it mined that one; asking for it back is a loop"


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
