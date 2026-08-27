"""A node that mines is still a node.

The chain is not observable to a peer until block relay, so what this asserts
is the property the miner was written around: it holds no lock while it
grinds. ADR-0014 allows exactly one test to read stdout, and this is not it —
that mining starts at all is covered where it can be, in `config.rs`.
"""

from framework.messages import TEST_MAGIC, ping
from framework.p2p import IMPATIENCE


def test_a_mining_node_still_answers_its_peers(net):
    """The miner takes the node lock to snapshot the tip and to submit a
    block, and holds it across neither of the two things that take time —
    searching, and writing to a socket. A node that answered slowly here
    would be holding it across one of them."""
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0", "--mine")
    peer = net.dial(node.listening_on(), TEST_MAGIC)
    peer.handshake()

    for nonce in range(20):
        peer.send(ping(nonce, TEST_MAGIC))
        assert nonce in peer.pongs_within(IMPATIENCE), f"ping {nonce} went unanswered"


def test_a_mining_node_still_completes_a_handshake(net):
    """A peer arriving while the miner is already running is served as
    promptly as one that arrived before it."""
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0", "--mine")
    address = node.listening_on()
    net.dial(address, TEST_MAGIC).handshake()

    later = net.dial(address, TEST_MAGIC)
    later.handshake()
    later.send(ping(7, TEST_MAGIC))

    assert 7 in later.pongs_within(IMPATIENCE)
