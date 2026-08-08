"""Nothing reaches a peer that has not said who it is.

The one exception is the handshake's own traffic: our `version`, which opens
every connection, and the `verack` that answers theirs. Gating those on Ready
would be the handshake waiting on itself.
"""

from framework.messages import ping, verack, version
from framework.p2p import ELSEWHERE


def test_a_peer_that_has_not_identified_itself_is_sent_nothing(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    assert peer.next_frame().command == "version"
    peer.send(ping(0xDEAD))

    assert peer.frames_within() == [], (
        "no pong for a peer we owe nothing, and no keep-alive either -- a "
        "connection used to be pinged the moment it opened"
    )


def test_half_a_handshake_is_still_not_a_peer(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.learn_nonce()

    peer.send(version(0x51DE, ELSEWHERE))
    assert peer.next_frame_of("verack").command == "verack"

    peer.send(ping(0xDEAD))
    assert peer.pongs_within() == [], "their verack has still not arrived"


def test_the_same_connection_starts_being_answered_once_it_is_ready(net):
    """No reconnect: the gate opens on the connection that was refused."""
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.learn_nonce()

    peer.send(ping(0xEA71))
    assert peer.pongs_within() == [], "too early"

    peer.send(version(0x51DE, ELSEWHERE))
    peer.next_frame_of("verack")
    peer.send(verack())

    peer.send(ping(0x1A7E))
    assert peer.pongs_within() == [0x1A7E]
