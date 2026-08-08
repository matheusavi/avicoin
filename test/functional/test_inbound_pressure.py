"""A node can always reach out, however many connections reach in."""

from framework.p2p import a_free_address, address_of, expect_dialled

# More than MAX_PEERS (32), so under a shared cap the flood would take every
# slot and the configured dial below would be refused.
A_FLOOD = 40


def test_an_inbound_flood_still_leaves_room_to_reach_a_configured_peer(net):
    wanted = a_free_address()
    node = net.node("--host-address", "127.0.0.1:0", "--addresses-to-connect", wanted)
    address = node.listening_on()

    # Its first dial fails, so the node is retrying while the flood arrives.
    node.line_containing("Could not connect to")

    for _ in range(A_FLOOD):
        net.dial(address)

    peer = net.track(expect_dialled(net.listener_on(wanted)))

    assert peer.next_frame().command == "version", (
        "an attacker who can fill every slot decides who this node may see"
    )


def test_a_node_nobody_dials_out_to_still_accepts_a_crowd(net):
    """The reservation costs a listen-only node slots; it must not cost it the
    ability to serve peers at all."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    crowd = [net.dial(address) for _ in range(A_FLOOD)]

    served = 0
    for peer in crowd:
        try:
            peer.handshake()
            served += 1
        except AssertionError:
            pass

    assert served >= 16, f"only {served} of {A_FLOOD} inbound peers were served"
