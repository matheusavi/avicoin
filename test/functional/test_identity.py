"""A nonce, not an address, decides who a connection is talking to.

The mutual-dial cases put the test on *both* ends: it listens for the node's
dial and dials the node back under one nonce, which is exactly the shape two
nodes dialling each other produces — and unlike two real nodes it does not
depend on which process bound its port first.
"""

from framework.messages import ping, version
from framework.p2p import address_of, expect_dialled

ELSEWHERE = "127.0.0.1:5000"


def below(nonce: int) -> int:
    return nonce - 1 if nonce > 0 else nonce + 1


def above(nonce: int) -> int:
    return nonce + 1 if nonce < 2**64 - 1 else nonce - 1


def test_a_peer_claiming_the_nodes_own_nonce_is_hung_up_on(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.send(version(villain.learn_nonce(), ELSEWHERE))

    villain.expect_closed()
    assert net.dial(address).next_frame().command == "version"


def test_a_node_pointed_at_itself_does_not_become_its_own_peer(net):
    """The checked-in config.toml is exactly this, so it is the default setup."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    itself = net.dial(address)
    itself.send(version(itself.learn_nonce(), address))

    itself.expect_closed()
    peer = net.dial(address)
    peer.handshake()
    peer.send(ping(0x5E1F))
    assert peer.pongs_within() == [0x5E1F], "the node must survive meeting itself"


def a_mutual_dial(net):
    """Two connections to one node: one it dialled, one we dialled."""
    ours = net.listener()
    node = net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", address_of(ours)
    )

    it_dialled = net.track(expect_dialled(ours))
    its_nonce = it_dialled.learn_nonce()

    we_dialled = net.dial(node.listening_on())
    we_dialled.learn_nonce()

    return it_dialled, we_dialled, its_nonce, address_of(ours)


def test_a_mutual_dial_below_the_nodes_nonce_keeps_what_the_node_dialled(net):
    it_dialled, we_dialled, its_nonce, address = a_mutual_dial(net)
    nonce = below(its_nonce)

    for connection in (it_dialled, we_dialled):
        connection.send(version(nonce, address))

    we_dialled.expect_closed()
    it_dialled.send(ping(0xC0FFEE))
    assert it_dialled.pongs_within() == [0xC0FFEE], "one connection must survive"


def test_a_mutual_dial_above_the_nodes_nonce_keeps_what_the_node_accepted(net):
    it_dialled, we_dialled, its_nonce, address = a_mutual_dial(net)
    nonce = above(its_nonce)

    for connection in (it_dialled, we_dialled):
        connection.send(version(nonce, address))

    it_dialled.expect_closed()
    we_dialled.send(ping(0xC0FFEE))
    assert we_dialled.pongs_within() == [0xC0FFEE], "one connection must survive"


def test_two_connections_under_one_nonce_never_both_survive(net):
    """The same peer twice is one peer, however it reached us."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    first = net.dial(address)
    nonce = below(first.learn_nonce())
    first.send(version(nonce, ELSEWHERE))

    second = net.dial(address)
    second.learn_nonce()
    second.send(version(nonce, ELSEWHERE))

    second.expect_closed()
    first.send(ping(0xD00D))
    assert first.pongs_within() == [0xD00D]


def test_a_different_nonce_is_a_different_peer(net):
    """Dedup must not collapse two genuine peers behind one address."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    first = net.dial(address)
    its_nonce = first.learn_nonce()
    first.send(version(below(its_nonce), ELSEWHERE))

    second = net.dial(address)
    second.learn_nonce()
    second.send(version(below(its_nonce) - 1, ELSEWHERE))

    for peer, nonce in ((first, 0xAAA), (second, 0xBBB)):
        peer.send(ping(nonce))
        assert peer.pongs_within() == [nonce], "both peers should still be here"
