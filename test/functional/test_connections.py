from framework.messages import ping, pong
from framework.p2p import address_of, expect_dialled


def test_a_node_pings_whoever_dials_it(net):
    node = net.node("--host-address", "127.0.0.1:0")

    assert net.dial(node.listening_on()).next_frame_of("ping").command == "ping"


def test_a_node_answers_a_ping_with_a_pong_carrying_the_same_nonce(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    peer.send(ping(0x0123456789ABCDEF))

    assert peer.pongs_within() == [0x0123456789ABCDEF]


def test_a_node_dials_every_address_it_was_given(net):
    first = net.listener()
    second = net.listener()

    net.node(
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        address_of(first),
        "--addresses-to-connect",
        address_of(second),
    )

    for listening in (first, second):
        peer = net.track(expect_dialled(listening))
        assert peer.next_frame().command == "version"


def test_a_pong_is_accepted_and_does_not_provoke_another_pong(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    opening = peer.next_frame_of("ping")
    peer.send(pong(opening.nonce))

    node.line_containing("Pong received")
    peer.expect_silence()


def test_two_real_nodes_hand_shake_and_complete_a_ping_pong_round_trip(net):
    listener = net.node("--host-address", "127.0.0.1:0")

    dialler = net.node(
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        listener.listening_on(),
    )

    # A pong only arrives if the other node parsed our ping, framed a reply,
    # and we parsed that -- the whole path, in both directions, and only after
    # each accepted the other as a peer. This is the one test that reads stdout,
    # and it goes away when M6 provides an API.
    for node in (listener, dialler):
        node.line_containing("Handshake with")
        node.line_containing("Pong received")
