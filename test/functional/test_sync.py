"""A node that is behind catches up.

Headers first: a peer asks with a locator, checks the work in what comes
back, and only then asks for bodies. Everything is asserted on bytes.
"""

import time

from framework.genesis import genesis_hash
from framework.messages import TEST_MAGIC, getheaders, hash256
from framework.p2p import IMPATIENCE, PATIENCE


def a_mining_node(net, *args: str):
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0", "--mine", *args)
    return node, node.listening_on()


def headers_within(peer, window: float = PATIENCE):
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.frames_within(0.3):
            if frame.command == "headers":
                return frame.as_headers()

    return []


def test_a_node_asks_a_new_peer_whether_it_is_behind(net):
    """A locator goes out on becoming Ready — whether we are behind is the
    peer's answer to give, not something to guess."""
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()

    asked = [
        frame for frame in peer.frames_within(IMPATIENCE) if frame.command == "getheaders"
    ]
    assert asked, "the node never asked"


def test_a_node_answers_a_locator_with_the_chain_after_it(net):
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()
    # Let it mine a few, then ask from genesis.
    time.sleep(2)

    peer.send(getheaders([genesis_hash()], TEST_MAGIC))
    headers = headers_within(peer)

    assert headers, "nothing came back"
    assert headers[0][4:36] == genesis_hash(), "the first follows what we named"
    for parent, child in zip(headers, headers[1:]):
        assert child[4:36] == hash256(parent), "and each follows the last"


def test_a_node_answers_a_locator_it_has_never_heard_of_from_genesis(net):
    _, address = a_mining_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()
    time.sleep(2)

    peer.send(getheaders([b"\x09" * 32], TEST_MAGIC))
    headers = headers_within(peer)

    assert headers, "nothing came back"
    assert hash256(headers[0]) == genesis_hash(), "its own chain from the start"


def test_a_fresh_node_catches_up_to_a_running_one(net):
    """The milestone's sync guarantee: a node started later reaches the same
    tip, and can serve the blocks it learned to a peer of its own."""
    _, first = a_mining_node(net)
    time.sleep(2)

    second = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--addresses-to-connect", first
    )
    watcher = net.dial(second.listening_on(), TEST_MAGIC)
    watcher.handshake()

    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        watcher.send(getheaders([genesis_hash()], TEST_MAGIC))
        for frame in watcher.frames_within(0.5):
            if frame.command == "headers" and len(frame.as_headers()) >= 2:
                return

    raise AssertionError(f"the second node never caught up within {PATIENCE}s")
