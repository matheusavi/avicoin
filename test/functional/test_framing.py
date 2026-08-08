"""TCP is a byte stream, not a message stream. Message boundaries are ours."""

import time

from framework.messages import ping


def test_a_message_dribbled_one_byte_at_a_time_is_still_understood(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

    for byte in ping(0xDEADBEEF):
        peer.send(bytes([byte]))
        time.sleep(0.001)

    assert peer.pongs_within() == [0xDEADBEEF]


def test_two_messages_arriving_in_one_read_are_both_answered(net):
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

    peer.send(ping(11) + ping(22))

    assert peer.pongs_within() == [11, 22]


def test_a_second_message_split_across_the_first_read_is_not_lost(net):
    """The tail of one read holds a whole message plus the head of the next."""
    node = net.node("--host-address", "127.0.0.1:0")
    peer = net.dial(node.listening_on())
    peer.handshake()

    stream = ping(101) + ping(202)
    peer.send(stream[:-4])
    time.sleep(0.05)
    peer.send(stream[-4:])

    assert peer.pongs_within() == [101, 202]
