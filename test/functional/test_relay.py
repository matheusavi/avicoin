"""A payment made against one node reaches another.

The transactions here are built by `framework/`, which derives the test
network's genesis independently. If the node and the suite ever disagree about
Base58Check, HASH160, the P2PKH template, the transaction encoding or the txid,
these tests stop being able to find their coins — which is the suite working.
"""

import time

from framework.genesis import MATURITY, funded
from framework.messages import (
    TEST_MAGIC,
    Transaction,
    TxIn,
    TxOut,
    compressed_public_key,
    getdata,
    hash160,
    inv,
    p2pkh,
    sign_input,
    tx,
)
from framework.p2p import IMPATIENCE, PATIENCE


def a_payment(fee: int = 100, to: bytes = b"\x11" * 20) -> Transaction:
    """Spends the first coin the test allocation created."""
    key, txid, v_out, value = funded(0)

    unsigned = Transaction(
        inputs=(TxIn(previous_txid=txid, v_out=v_out),),
        outputs=(TxOut(value=value - fee, script_pubkey=p2pkh(to)),),
    )
    signature = sign_input(key, unsigned.txid())

    return Transaction(
        inputs=(
            TxIn(
                previous_txid=txid,
                v_out=v_out,
                witness=(signature, compressed_public_key(key)),
            ),
        ),
        outputs=unsigned.outputs,
    )


def a_test_node(net, *args: str):
    """The test network, because that is where the allocation has coins."""
    node = net.node("--network", "test", "--host-address", "127.0.0.1:0", *args)
    return node, node.listening_on()


def announced_within(peer, window: float = PATIENCE):
    """Every txid the node offers within the window."""
    deadline = time.monotonic() + window
    offered = []

    while time.monotonic() < deadline:
        for frame in peer.frames_within(0.3):
            if frame.command == "inv":
                offered.extend(frame.transactions_named())
        if offered:
            return offered

    return offered


def test_a_node_announces_a_transaction_it_accepted(net):
    """To everyone but the peer that sent it, who already has it."""
    _, address = a_test_node(net)
    sender = net.dial(address, TEST_MAGIC)
    watcher = net.dial(address, TEST_MAGIC)
    sender.handshake()
    watcher.handshake()

    payment = a_payment()
    sender.send(tx(payment, TEST_MAGIC))

    assert payment.txid() in announced_within(watcher)
    assert payment.txid() not in announced_within(sender, IMPATIENCE)


def test_a_node_does_not_announce_a_transaction_it_refused(net):
    _, address = a_test_node(net)
    sender = net.dial(address, TEST_MAGIC)
    watcher = net.dial(address, TEST_MAGIC)
    sender.handshake()
    watcher.handshake()

    forged = a_payment()
    overspending = Transaction(
        inputs=forged.inputs,
        outputs=(TxOut(value=forged.outputs[0].value * 2, script_pubkey=forged.outputs[0].script_pubkey),),
    )
    sender.send(tx(overspending, TEST_MAGIC))

    assert overspending.txid() not in announced_within(watcher, IMPATIENCE)


def test_a_node_asks_for_a_transaction_it_was_offered_and_does_not_hold(net):
    _, address = a_test_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()

    payment = a_payment()
    peer.send(inv([payment.txid()], TEST_MAGIC))

    assert peer.next_frame_of("getdata").transactions_named() == [payment.txid()]


def test_a_node_serves_what_it_holds_and_stays_quiet_about_what_it_does_not(net):
    _, address = a_test_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()

    payment = a_payment()
    peer.send(tx(payment, TEST_MAGIC))
    announced_within(peer)

    peer.send(getdata([b"\x09" * 32], TEST_MAGIC))
    peer.send(getdata([payment.txid()], TEST_MAGIC))

    served = peer.next_frame_of("tx")
    assert served.payload == payment.serialize()


def test_two_real_nodes_converge_on_the_same_mempool(net):
    _, first = a_test_node(net)
    _, second = a_test_node(net, "--addresses-to-connect", first)

    sender = net.dial(first, TEST_MAGIC)
    sender.handshake()
    payment = a_payment()
    sender.send(tx(payment, TEST_MAGIC))

    watcher = net.dial(second, TEST_MAGIC)
    watcher.handshake()

    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        watcher.send(getdata([payment.txid()], TEST_MAGIC))
        for frame in watcher.frames_within(0.5):
            if frame.command == "tx" and frame.payload == payment.serialize():
                return

    raise AssertionError(f"the second node never held the payment within {PATIENCE}s")


def test_maturity_lets_the_test_network_spend_from_height_zero(net):
    """The allocation is a coinbase, so it would be unspendable for a hundred
    blocks on mainnet. The test network lowers the parameter, and this is what
    that is for."""
    assert MATURITY < 100

    _, address = a_test_node(net)
    peer = net.dial(address, TEST_MAGIC)
    peer.handshake()

    payment = a_payment()
    peer.send(tx(payment, TEST_MAGIC))
    peer.send(getdata([payment.txid()], TEST_MAGIC))

    assert peer.next_frame_of("tx").payload == payment.serialize()
