"""Kill a node and watch it come back.

Crash consistency is a property to test, not an assumption, and it cannot be
tested honestly without killing a process. Everything here uses `SIGKILL`:
a graceful stop proves nothing, because the whole question is what survives
when the process is given no say.

Every wait is bounded and every wait is on a *state* — ADR-0014. Observation
is the chain a node will hand a peer over the wire, never a log line.

**Observation is a block's body, never its header.** The header index is
committed the moment a header is accepted, so a node that had lost every
block would still recite the whole chain from `getheaders`. Bodies come off
`blocks.dat`. The first draft of this file asserted on headers, passed
against a node with persistence entirely disabled, and was wrong.

Proven by mutation, and the mutation confirmed to have compiled in first:
making `Storage::remember_block` a no-op turns three of these red. The unit
tests in `persist.rs` cover the other two directions — a no-op `catch_up`,
and a disconnect that does not commit.

**What killing a process cannot show.** `SIGKILL` does not empty the page
cache, so nothing here can tell a flushed write from one the kernel has not
written yet. Removing the `sync` calls before the commit leaves every test in
this file green. That ordering — the files durable before the store names
them — is argued in ADR-0013 and visible in ten lines of `persist.rs`; a test
that claimed to prove it would need to lose power, not a process.
"""

import time

from framework.genesis import genesis_hash
from framework.messages import TEST_MAGIC, getdata_blocks, getheaders, hash256
from framework.p2p import PATIENCE

# A restart re-reads the store, and these run several processes on one core.
RECOVERY = 20.0


def a_node(net, *args: str):
    return net.node("--network", "test", "--host-address", "127.0.0.1:0", *args)


def watch(net, node):
    """One connection per node, asked repeatedly. Dialling afresh for every
    question would exhaust the node's inbound slots in seconds."""
    peer = net.dial(node.listening_on(), TEST_MAGIC)
    peer.handshake()
    return peer


def chain_of(peer, window: float = 3.0):
    """Every header a node will give from genesis, oldest first. Its own
    account of where it is, taken over the wire rather than from stdout."""
    peer.send(getheaders([genesis_hash()], TEST_MAGIC))
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.take_frames("headers", 0.3):
            return [genesis_hash()] + [hash256(h) for h in frame.as_headers()]

    return [genesis_hash()]


def serves(peer, hash_, window: float = 3.0) -> bool:
    """Whether the node will hand over a block's *body*.

    This is the question that matters and `getheaders` is not it: the header
    index is persisted the moment a header is accepted, so a node that lost
    every block would still recite the whole chain. A body comes from
    `blocks.dat`, so serving one is the node saying it really has the block.
    """
    peer.send(getdata_blocks([hash_], TEST_MAGIC))
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.take_frames("block", 0.3):
            if hash256(frame.as_block_header()) == hash_:
                return True

    return False


def past(peer, height: int, window: float = RECOVERY) -> list:
    """The node's chain once it is longer than `height`, or its last answer."""
    deadline = time.monotonic() + window
    chain = chain_of(peer)

    while time.monotonic() < deadline and len(chain) - 1 <= height:
        chain = chain_of(peer)

    return chain


TEST_NODE = ("--network", "test", "--host-address", "127.0.0.1:0")


def test_a_fresh_data_directory_starts_at_genesis(net):
    node = a_node(net)

    assert chain_of(watch(net, node)) == [genesis_hash()]


def test_a_node_that_mined_and_stopped_comes_back_where_it_was(net):
    node = a_node(net, "--mine")
    before = past(watch(net, node), 2)
    assert len(before) - 1 >= 3, "the miner has to have got somewhere first"

    restarted = net.restart(node, *TEST_NODE)
    peer = watch(net, restarted)
    after = chain_of(peer)

    assert after[: len(before)] == before, "the chain it had is the chain it has"
    assert serves(peer, before[-1]), "and it still has the blocks, not just the headers"


def test_a_node_that_mined_and_stopped_keeps_its_address(net):
    node = a_node(net, "--mine")
    past(watch(net, node), 1)
    key = (node.sandbox.data_dir / "wallet.key").read_text()

    restarted = net.restart(node, *TEST_NODE)
    restarted.listening_on()

    assert (restarted.sandbox.data_dir / "wallet.key").read_text() == key


def test_a_node_killed_while_mining_comes_back_on_a_block_it_announced(net):
    node = a_node(net, "--mine")
    seen = past(watch(net, node), 3)

    node.kill()
    restarted = net.reuse(node, *TEST_NODE)
    peer = watch(net, restarted)
    recovered = chain_of(peer)

    # The last block it told us about, or that block's parent — never
    # something in between, and never a chain that disagrees with what it
    # already said.
    assert len(recovered) >= len(seen) - 1, f"{len(recovered)} against {len(seen)}"
    assert recovered == seen[: len(recovered)], "it came back on a chain it had shown"
    assert serves(peer, seen[len(seen) - 2]), "with the blocks behind it"


def test_a_node_killed_between_blocks_never_comes_back_between_them(net):
    """Four kills, each at an arbitrary point in a block's application. Every
    recovered chain has to be a prefix of what the node had already published,
    or an extension of it, which is what "never something in between" means."""
    node = a_node(net, "--mine")
    published = past(watch(net, node), 2)

    for _ in range(4):
        node.kill()
        node = net.reuse(node, *TEST_NODE, "--mine")
        peer = watch(net, node)
        recovered = chain_of(peer)

        assert serves(peer, recovered[-1]), "a chain it recites is a chain it holds"
        assert (
            recovered == published[: len(recovered)]
            or published == recovered[: len(published)]
        ), f"{len(recovered)} blocks against {len(published)} published"
        published = past(peer, len(recovered), window=PATIENCE)


def test_a_restarted_node_catches_up_with_a_network_that_kept_going(net):
    miner = a_node(net, "--mine")
    follower = a_node(net, "--addresses-to-connect", miner.listening_on())
    reached = past(watch(net, follower), 2)
    assert len(reached) > 3, "the follower has to have synced something first"

    follower.kill()
    back = net.reuse(follower, *TEST_NODE, "--addresses-to-connect", miner.listening_on())
    peer = watch(net, back)
    caught_up = past(peer, len(reached))

    assert len(caught_up) > len(reached), "it kept going rather than starting over"
    assert caught_up[: len(reached)] == reached
    assert serves(peer, reached[-1]), "it kept the blocks it had synced before"
