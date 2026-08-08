"""A nonce, not an address, decides who a connection is talking to — ADR-0015.

The mutual-dial cases put the harness on *both* ends: it listens for the node's
dial and dials the node back under one nonce, which is the same shape two nodes
dialling each other produce. Two real processes cannot do it reliably until #43,
because whichever binds second is dialled by nobody.
"""

import pytest

from framework.messages import ping, verack, version
from framework.p2p import ELSEWHERE, a_free_address, address_of, expect_dialled

# A node's nonce is 64 random bits, so these sit either side of it. Picking them
# by arithmetic on the node's own would only add a branch for a case that needs
# a 1-in-2^64 draw to happen.
LOSING = 0
WINNING = 2**64 - 1


def still_a_peer(connection, nonce):
    """Finish the handshake this connection opened, then prove it still works.

    Nothing flows to a peer before Ready, so a surviving connection has to be
    taken all the way through to say anything about it at all.
    """
    connection.next_frame_of("verack")
    connection.send(verack())
    connection.send(ping(nonce))

    assert connection.pongs_within() == [nonce]


def test_a_peer_claiming_the_nodes_own_nonce_is_hung_up_on(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.send(version(villain.learn_nonce(), ELSEWHERE))

    villain.expect_closed()
    assert net.dial(address).next_frame().command == "version"


def test_a_node_pointed_at_itself_still_serves_other_peers(net):
    """One address in both fields, which is the checked-in `config.toml`.

    That the self-connection is *dropped* is asserted above and in Rust; peer
    count has no surface on the wire until M6, so what this adds is that the
    real configuration does not wedge the node.
    """
    itself = a_free_address()
    node = net.node("--host-address", itself, "--addresses-to-connect", itself)

    peer = net.dial(node.listening_on())
    peer.handshake()
    peer.send(ping(0x5E1F))

    assert peer.pongs_within() == [0x5E1F]


def a_mutual_dial(net):
    """Two connections to one node: one it dialled, one we dialled."""
    ours = net.listener()
    node = net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", address_of(ours)
    )

    it_dialled = net.track(expect_dialled(ours))
    we_dialled = net.dial(node.listening_on())

    assert it_dialled.learn_nonce() == we_dialled.learn_nonce(), (
        "one process, one nonce -- a per-connection nonce would leave the dedup "
        "below passing or failing at random rather than going red"
    )

    return it_dialled, we_dialled, address_of(ours)


@pytest.mark.parametrize(
    "ours, node_keeps_its_own_dial",
    [(LOSING, True), (WINNING, False)],
    ids=["below_the_nodes_nonce", "above_the_nodes_nonce"],
)
def test_a_mutual_dial_settles_on_the_socket_the_larger_nonce_dialled(
    net, ours, node_keeps_its_own_dial
):
    it_dialled, we_dialled, address = a_mutual_dial(net)

    for connection in (it_dialled, we_dialled):
        connection.send(version(ours, address))

    kept, dropped = (
        (it_dialled, we_dialled) if node_keeps_its_own_dial else (we_dialled, it_dialled)
    )
    dropped.expect_closed()
    still_a_peer(kept, 0xC0FFEE)


def test_two_connections_under_one_nonce_never_both_survive(net):
    """The same peer twice is one peer, however it reached us."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    first = net.dial(address)
    first.learn_nonce()
    first.send(version(LOSING, ELSEWHERE))

    second = net.dial(address)
    second.learn_nonce()
    second.send(version(LOSING, ELSEWHERE))

    second.expect_closed()
    still_a_peer(first, 0xD00D)


def test_a_different_nonce_is_a_different_peer(net):
    """Dedup must not collapse two genuine peers behind one address."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    peers = []
    for nonce in (LOSING, LOSING + 1):
        peer = net.dial(address)
        peer.learn_nonce()
        peer.send(version(nonce, ELSEWHERE))
        peers.append(peer)

    for peer, nonce in zip(peers, (0xAAA, 0xBBB)):
        still_a_peer(peer, nonce)
