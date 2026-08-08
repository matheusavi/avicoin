"""A node finds peers it was never told about.

The three-node case is the milestone's exit criterion: partial seed lists in,
full mesh out.
"""

import time

from framework.messages import MAX_ADDRESSES, addr, frame, compact_size, getaddr, pack_address, ping
from framework.p2p import (
    ELSEWHERE,
    IMPATIENCE,
    PATIENCE,
    a_free_address,
    accept_within,
    address_of,
    expect_dialled,
)


def eventually_knows(peer, address: str) -> None:
    """Discovery is not instant, so ask again — bounded, and on bytes."""
    deadline = time.monotonic() + PATIENCE
    known = []

    while time.monotonic() < deadline:
        peer.send(getaddr())
        known = peer.next_frame_of("addr").as_addresses()
        if address in known:
            return
        time.sleep(0.1)

    raise AssertionError(f"never learned {address} within {PATIENCE}s; knows {known}")


def test_a_node_asks_a_new_peer_who_else_is_out_there(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())

    peer.handshake()

    assert peer.next_frame_of("getaddr").command == "getaddr"


def test_a_getaddr_is_answered_with_listening_addresses_not_source_ports(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    advertised = "127.0.0.1:9999"
    other = net.dial(address)
    other.handshake(nonce=0xA11CE, listen_address=advertised)

    asker = net.dial(address)
    asker.handshake(nonce=0xB0B)
    asker.send(getaddr())

    served = asker.next_frame_of("addr").as_addresses()

    assert served == [advertised], (
        "the ephemeral port a peer dialled us from is not one anybody can dial "
        f"back; got {served}"
    )


def test_a_peer_reaching_ready_is_announced_to_the_others(net):
    """Unsolicited: the watcher never asks. Without this a mesh converges only
    when the startup order happens to suit it — see ADR-0017."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    watcher = net.dial(address)
    watcher.handshake(nonce=0xA11CE, listen_address="127.0.0.1:9001")

    newcomer = net.dial(address)
    newcomer.handshake(nonce=0xB0B, listen_address="127.0.0.1:9002")

    announced = watcher.next_frame_of("addr").as_addresses()

    assert "127.0.0.1:9002" in announced, (
        f"the watcher asked for nothing and should still hear of a new peer; "
        f"got {announced}"
    )


def test_a_peer_is_not_announced_to_itself(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    established = net.dial(address)
    established.handshake(nonce=0xA11CE, listen_address="127.0.0.1:9001")

    newcomer = net.dial(address)
    newcomer.handshake(nonce=0xB0B, listen_address="127.0.0.1:9002")

    for heard in newcomer.frames_within():
        if heard.command == "addr":
            assert "127.0.0.1:9002" not in heard.as_addresses(), (
                "news about a peer is of no use to that peer, and would have it "
                "dial itself"
            )


def test_a_node_dials_an_address_it_was_told_about(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

    undiscovered = net.listener()
    peer.send(addr([address_of(undiscovered)]))

    found = net.track(expect_dialled(undiscovered))
    assert found.next_frame().command == "version"


def test_a_node_does_not_dial_an_address_that_is_its_own(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()
    peer = net.dial(address)
    peer.handshake()

    peer.send(addr([address]))

    # Dialling ourselves would be caught by the nonce guard, but only after a
    # connection, a handshake and a slot.
    assert peer.pongs_within() == [], "no traffic expected either way"
    live = net.dial(address)
    live.handshake()
    live.send(ping(0x11FE))
    assert live.pongs_within() == [0x11FE], "the node is still serving peers"


def test_a_flood_of_junk_addresses_does_not_dial_without_limit(net):
    """The reachable one among them still gets dialled; the rest cost nothing
    the node cannot bound."""
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

    reachable = net.listener()
    junk = [f"127.0.0.1:{port}" for port in range(19000, 19000 + MAX_ADDRESSES - 1)]
    peer.send(addr(junk + [address_of(reachable)]))

    found = net.track(expect_dialled(reachable))
    assert found.next_frame().command == "version"


def test_an_addr_claiming_more_than_the_cap_is_refused(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.handshake()
    too_many = [ELSEWHERE] * (MAX_ADDRESSES + 1)
    villain.send(
        frame(
            "addr",
            compact_size(len(too_many)) + b"".join(pack_address(a) for a in too_many),
        )
    )

    villain.expect_closed()
    assert net.dial(address).next_frame().command == "version"


def test_three_nodes_with_partial_seed_lists_find_the_whole_mesh(net):
    """M2's exit criterion. Only the middle node is told about the other two,
    so the outer pair can only meet each other through discovery.

    The third node is started *late*, on purpose. A node sends `getaddr` once,
    as it becomes Ready, so had all three come up together the first node might
    have learned of the third from the middle node's answer — or might not,
    depending on which handshake finished first. Delaying it puts that answer
    provably in the past, leaving the relay as the only way the news can travel.
    """
    first = a_free_address()
    second = a_free_address()
    third = a_free_address()

    outer = net.node("--host-address", first)
    net.node(
        "--host-address",
        second,
        "--addresses-to-connect",
        first,
        "--addresses-to-connect",
        third,
    )

    watching_first = net.dial(outer.listening_on())
    watching_first.handshake()
    # Once the first node reports the middle one, its one `getaddr` has been
    # asked and answered — with nothing about a node that is not yet running.
    eventually_knows(watching_first, second)

    latecomer = net.node("--host-address", third)

    eventually_knows(watching_first, third)

    watching_third = net.dial(latecomer.listening_on())
    watching_third.handshake()
    eventually_knows(watching_third, first)


def test_a_peer_that_has_not_handshaken_is_told_nothing(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.learn_nonce()

    peer.send(getaddr())

    assert peer.frames_within() == [], "an unidentified peer is owed no addresses"


def test_a_learned_address_we_already_hold_is_not_dialled_again(net):
    listening = net.listener()
    node = net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", address_of(listening)
    )

    held = net.track(expect_dialled(listening))
    held.handshake(listen_address=address_of(listening))

    gossip = net.dial(node.listening_on())
    gossip.handshake()
    gossip.send(addr([address_of(listening)]))

    assert accept_within(listening, IMPATIENCE) is None, (
        "a peer we already hold is not one to go and find"
    )
