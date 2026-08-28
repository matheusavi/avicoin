import time

from framework.http import get_json
from framework.messages import ping, pong
from framework.p2p import PATIENCE, a_free_address, address_of, expect_dialled


def test_a_node_pings_whoever_dials_it(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    peer.handshake()

    assert peer.next_frame_of("ping").command == "ping"


def test_a_node_answers_a_ping_with_a_pong_carrying_the_same_nonce(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

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
    peer.handshake()

    opening = peer.next_frame_of("ping")
    peer.send(pong(opening.nonce))

    # Our own ping is the barrier. The writer is FIFO, so the pong answering it
    # comes after anything our pong provoked — and if nothing was provoked,
    # that one pong is all there is. The node's next ping would work too, but
    # it is eleven seconds away and PATIENCE is eight.
    peer.send(ping(0x50_4f_4e_47))

    assert peer.pongs_within() == [0x50_4f_4e_47], "a pong is not something to answer"


def test_two_real_nodes_hand_shake_and_each_reports_the_other_as_ready(net):
    """The whole path in both directions, observed through the API.

    A peer reaches `ready` only once its `version` and its `verack` have both
    arrived, so each node reporting the other as ready means each parsed what
    the other framed. This used to read stdout — ADR-0014's one exception, kept
    only until there was another surface. There is one now.
    """
    first_api, second_api = a_free_address(), a_free_address()
    listener = net.node("--host-address", "127.0.0.1:0", "--api-address", first_api)
    dialler = net.node(
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        listener.listening_on(),
        "--api-address",
        second_api,
    )
    dialler.line_containing("API on")

    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        both = [get_json(api, "/peers")[1] for api in (first_api, second_api)]
        if all(seen["count"] and seen["peers"][0]["handshake"] == "ready" for seen in both):
            break

    for seen, direction in zip(both, ("inbound", "outbound")):
        assert seen["count"] == 1, seen
        assert seen["peers"][0]["handshake"] == "ready", seen
        assert seen["peers"][0]["direction"] == direction, seen

    assert both[0]["peers"][0]["listening"] == dialler.listening_on()
    assert both[1]["peers"][0]["listening"] == listener.listening_on()
