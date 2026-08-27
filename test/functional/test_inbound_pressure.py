"""A node can always reach out, however many connections reach in."""

from framework.p2p import a_free_address, expect_dialled

# Enough to outlast any sane peer cap, so the test does not have to know one.
GIVE_UP_AFTER = 128


def crowd_out(net, address) -> int:
    """Dial until the node stops serving new connections, and say how many it
    served. Measured rather than assumed: the peer caps are Rust constants this
    side cannot see, and guessing one is how a test passes vacuously after
    somebody raises it."""
    served = 0

    for _ in range(GIVE_UP_AFTER):
        peer = net.dial(address)
        # A served connection opens with the node's version; a refused one is
        # closed, and yields nothing.
        if not peer.frames_within(0.2):
            return served
        served += 1

    raise AssertionError(f"the node served {GIVE_UP_AFTER} inbound connections")


def test_an_inbound_flood_still_leaves_room_to_reach_a_configured_peer(net):
    wanted = a_free_address()
    node = net.node("--host-address", "127.0.0.1:0", "--addresses-to-connect", wanted)
    address = node.listening_on()

    # Its first dial failed, so the node is retrying while the flood arrives.
    node.line_containing("Could not connect to")

    served = crowd_out(net, address)
    assert served > 0, "the node refused every inbound connection"

    peer = net.track(expect_dialled(net.listener_on(wanted)))

    assert peer.next_frame().command == "version", (
        "an attacker who can fill every slot decides who this node may see"
    )
