"""A connection is not a peer until both sides have said who they are."""

from framework.messages import PROTOCOL_VERSION, frame, ping, verack, version


def test_a_node_opens_a_connection_by_identifying_itself(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    said = net.dial(address).next_frame().as_version()

    assert said.protocol_version == PROTOCOL_VERSION
    assert said.listen_address == address, (
        "a peer re-dials the address we advertise, so it must be the bound one "
        "and not the 127.0.0.1:0 that was asked for"
    )


def test_each_node_mints_its_own_nonce(net):
    """#41 tells a self-connection apart by this, and cannot if they collide."""
    first = net.node("--host-address", "127.0.0.1:0")
    second = net.node("--host-address", "127.0.0.1:0")

    nonces = {
        net.dial(node.listening_on()).next_frame().as_version().nonce
        for node in (first, second)
    }

    assert len(nonces) == 2, f"two nodes advertised the same nonce: {nonces}"


def test_a_version_is_answered_with_a_verack(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.next_frame_of("version")

    peer.send(version(0x1122334455667788, "127.0.0.1:5000"))

    assert peer.next_frame_of("verack").payload == b""


def test_a_peer_that_completes_the_handshake_keeps_talking(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    peer.handshake()
    peer.send(ping(0xFEEDFACE))

    assert peer.pongs_within() == [0xFEEDFACE]


def test_a_peer_sending_a_verack_it_was_never_owed_is_dropped(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.next_frame_of("version")
    villain.send(verack())
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_second_version_after_the_handshake_is_refused(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.handshake()
    villain.send(version(0x1122334455667788, "127.0.0.1:5000"))
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_verack_carrying_a_payload_is_not_a_verack(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.next_frame_of("version")
    villain.send(version(0x1122334455667788, "127.0.0.1:5000"))
    # Correctly framed and checksummed, so it is refused for its body alone.
    villain.send(frame("verack", b"\0"))
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"
