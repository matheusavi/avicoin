"""A bad peer loses its connection. It never takes the node down.

Each case ends by dialling again: proving the connection died is only half the
guarantee, and the half that matters least.
"""

import struct

from framework.messages import (
    HEADER_LENGTH,
    MAGIC,
    MAX_PAYLOAD_SIZE,
    OTHER_NETWORK_MAGIC,
    frame,
    ping,
)


def test_a_peer_speaking_another_networks_magic_bytes_is_dropped(net):
    """Not a corrupted byte — the magic of the network next door. The two
    parameter sets differ from block zero, and this is the cheap early filter
    that keeps their traffic apart."""
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    foreign = OTHER_NETWORK_MAGIC + ping(1)[4:]
    villain = net.dial(address)
    villain.send(foreign)
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_test_network_node_refuses_mainnet_framing(net):
    """The filter runs in both directions, so a test node cannot be fed by a
    mainnet peer either."""
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.send(MAGIC + ping(1)[4:])
    villain.expect_closed()


def test_a_peer_claiming_a_four_gigabyte_payload_is_dropped(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    header = bytearray(ping(1)[:HEADER_LENGTH])
    header[16:20] = struct.pack("<I", 0xFFFFFFFF)
    villain = net.dial(address)
    villain.send(bytes(header))
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_payload_at_the_limit_is_not_refused_for_being_too_large(net):
    """The cap is 32 MiB inclusive; a claim exactly at it is legal, merely
    incomplete. The node should wait for bytes that never come, not hang up."""
    node = net.node("--host-address", "127.0.0.1:0")

    header = bytearray(ping(1)[:HEADER_LENGTH])
    header[16:20] = struct.pack("<I", MAX_PAYLOAD_SIZE)
    peer = net.dial(node.listening_on())
    peer.send(bytes(header))

    assert peer.next_frame().command == "version", "the connection should still be live"


def test_a_peer_whose_payload_does_not_match_its_checksum_is_dropped(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    corrupted = bytearray(ping(1))
    corrupted[-1] ^= 0xFF
    villain = net.dial(address)
    villain.send(bytes(corrupted))
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_peer_sending_an_unknown_command_is_dropped(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    villain = net.dial(address)
    villain.send(frame("notacommand", struct.pack("<Q", 7)))
    villain.expect_closed()

    assert net.dial(address).next_frame().command == "version"


def test_a_peer_that_vanishes_mid_message_does_not_take_the_node_down(net):
    node = net.node("--host-address", "127.0.0.1:0")
    address = node.listening_on()

    deserter = net.dial(address)
    deserter.send(ping(1)[: HEADER_LENGTH - 4])
    deserter.close()

    assert net.dial(address).next_frame().command == "version"
