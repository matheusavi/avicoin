"""A configured address is an intent to stay connected, not one attempt.

Peers learned by discovery are not covered: those come and go by design. These
are the addresses an operator wrote down.
"""

from framework.p2p import IMPATIENCE, a_free_address, accept_within, address_of, expect_dialled


def test_a_peer_that_is_down_at_boot_is_dialled_once_it_comes_up(net):
    address = a_free_address()
    node = net.node("--host-address", "127.0.0.1:0", "--addresses-to-connect", address)

    # Synchronisation, not the assertion: nothing is listening yet, and binding
    # before the first dial has failed would green this test without a retry
    # ever happening. The guarantee below is still asserted on bytes.
    node.line_containing("Could not connect to")

    peer = net.track(expect_dialled(net.listener_on(address)))

    assert peer.next_frame().command == "version"


def test_a_configured_peer_that_drops_mid_session_is_dialled_again(net):
    listening = net.listener()
    net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", address_of(listening)
    )

    first = net.track(expect_dialled(listening))
    assert first.next_frame().command == "version"
    first.close()

    second = net.track(expect_dialled(listening))
    assert second.next_frame().command == "version", "the node gave up after one loss"


def test_a_connection_that_is_still_alive_is_not_dialled_a_second_time(net):
    listening = net.listener()
    net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", address_of(listening)
    )

    peer = net.track(expect_dialled(listening))
    peer.handshake()

    # Long enough for several retries had the node been counting them: the
    # first would be due a second after the connection ended.
    assert accept_within(listening, IMPATIENCE) is None, (
        "a peer we are already connected to must not be dialled again"
    )
