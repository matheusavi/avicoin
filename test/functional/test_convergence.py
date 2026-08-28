"""Several nodes, mining at once, ending up on one chain.

This is M4's headline guarantee and the reason reorg exists: at a
thirty-second target two miners find a block at the same height often enough
that a network which never reconciles shatters within a week. Here the
target is a second and the race is constant, so a few seconds of two miners
is many chances to disagree.

Every scenario waits for a *state*, never for a duration, and every wait is
bounded — ADR-0014. Nothing reads stdout.

Proven by mutation, not by a green run:

- disabling the chain switch (`Chain::accept`'s reorg arm) turns the
  partition scenario red and leaves the other two green, which is right: only
  that one makes a node abandon a chain it built;
- making the miner announce nothing turns all three red.

Both were checked, and both mutations were confirmed to have compiled in
before the result was believed.

**What this file cannot show.** Chain selection is by cumulative work, and on
the test network every block sits at the same floor difficulty — so work and
length are the same number here, and no scenario can tell "the heaviest won"
from "the longest won". That distinction is a unit-level guarantee, made by
constructing a branch at a harder target: see `blockchain.rs`'s
`the_shorter_branch_wins_when_it_carries_more_work` and
`a_shorter_branch_wins_the_switch_when_it_carries_more_work`. Claiming it
here would be claiming a test that does not exist.
"""

import time

from framework.genesis import genesis_hash
from framework.messages import TEST_MAGIC, getdata_blocks, getheaders, hash256
from framework.p2p import PATIENCE

# Longer than PATIENCE: convergence needs a block to be found, relayed and
# connected, and these scenarios run several nodes at once on one core.
#
# Raised from 25s after a run in five failed here on a loaded machine — the
# healed network had converged, but the *observation* this test insists on
# (one node mining on a block the other mined) had not happened yet. A weaker
# assertion would be a faster test that proved less, so the deadline moved
# instead.
CONVERGENCE = 60.0


def a_node(net, *args: str):
    return net.node("--network", "test", "--host-address", "127.0.0.1:0", *args)


def chain_of(peer, window: float = 2.0):
    """Every header a node will give from genesis, as hashes, oldest first."""
    peer.send(getheaders([genesis_hash()], TEST_MAGIC))
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.take_frames("headers", 0.3):
            return [hash256(header) for header in frame.as_headers()]

    return []


def watch(net, node):
    peer = net.dial(node.listening_on(), TEST_MAGIC)
    peer.handshake()
    return peer


# Agreeing on genesis is not agreeing. A chain has to have been mined, raced
# over and settled before "the same tip" means anything.
MEANINGFUL = 4


def agree_within(watchers, window: float = CONVERGENCE, at_least: int = MEANINGFUL):
    """The shared tip once every node reports the same one, beyond `at_least`
    blocks, or None."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        chains = [chain_of(peer) for peer in watchers]
        if all(len(chain) > at_least for chain in chains):
            tips = {chain[-1] for chain in chains}
            if len(tips) == 1:
                return tips.pop()

        # A block takes a second to arrive; asking faster than that is a flood
        # of our own making.
        time.sleep(0.5)

    return None


def test_two_nodes_mining_at_once_end_up_on_one_chain(net):
    first = a_node(net, "--mine")
    second = a_node(net, "--mine", "--addresses-to-connect", first.listening_on())

    watchers = [watch(net, first), watch(net, second)]

    assert agree_within(watchers), "two miners never reconciled"


def test_a_node_that_only_listens_follows_the_miners(net):
    """Three nodes, one of them not mining: it still ends on their chain."""
    first = a_node(net, "--mine")
    second = a_node(net, "--mine", "--addresses-to-connect", first.listening_on())
    idle = a_node(net, "--addresses-to-connect", first.listening_on())

    watchers = [watch(net, node) for node in (first, second, idle)]

    assert agree_within(watchers), "the listener never caught up"


def announced(peer, window: float = 2.0):
    """Block hashes the node offered while we watched. A node announces what
    it mined, so this is what it is building on — which `getheaders` is not:
    that answers from the heaviest chain a node knows, connected or not."""
    return [hash for frame in peer.take_frames("inv", window) for hash in frame.blocks_named()]


def block_from(peer, hash, window: float = PATIENCE):
    """Asks for a block and returns `(parent, miner)` — the miner being the
    `script_pubkey` its coinbase pays, which is how a test tells the two nodes
    apart. Their wallets are minted per run, so it has to be learned."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        peer.send(getdata_blocks([hash], TEST_MAGIC))
        for frame in peer.take_frames("block", 0.5):
            if hash256(frame.as_block_header()) == hash:
                return frame.as_block_header()[4:36], frame.coinbase_script()

    raise AssertionError(f"the node never served {hash[::-1].hex()}")


def test_two_chains_mined_apart_converge_once_the_network_heals(net):
    """A partition, healed without restarting either side.

    The two miners start knowing nobody, so each builds its own chain. A third
    node that knows both then tells each about the other — discovery heals the
    split, and cumulative work settles it. Which of the two wins is not
    asserted: at one difficulty the heavier chain is the longer one, and there
    is nothing here to tell them apart.

    What is watched is what each miner *builds on*, not what it knows: a node
    answers `getheaders` from the heaviest chain it has headers for, which is
    not the same as the chain it has connected. One node mining on a block the
    other mined is a reorg and nothing else.
    """
    first = a_node(net, "--mine")
    second = a_node(net, "--mine")
    apart = [watch(net, first), watch(net, second)]

    # While they are alone, everything a node announces is its own, so this is
    # where each one's coinbase script — its signature — is learned.
    mined = [set(), set()]
    miners = [None, None]
    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline and not all(len(seen) >= 2 for seen in mined):
        for at, peer in enumerate(apart):
            for hash in announced(peer, 1.0):
                _, miners[at] = block_from(peer, hash)
                mined[at].add(hash)

    assert all(len(seen) >= 2 for seen in mined), "neither built a chain of its own"
    assert not (mined[0] & mined[1]), "they were never really apart"
    assert miners[0] != miners[1], "two nodes, two wallets"

    a_node(
        net,
        "--addresses-to-connect",
        first.listening_on(),
        "--addresses-to-connect",
        second.listening_on(),
    )

    # Convergence is one node *mining on* a block the other mined. Relaying
    # the other's blocks is not convergence; building on them is.
    #
    # Every block seen is remembered, and the condition is checked over all of
    # them: the moment that matters is one announcement, and requiring it to be
    # the one in hand when the check runs would make this a race.
    parents = {}
    minted = {}
    deadline = time.monotonic() + CONVERGENCE
    while time.monotonic() < deadline:
        for at, peer in enumerate(apart):
            for hash in announced(peer, 1.0):
                if hash not in parents:
                    parents[hash], minted[hash] = block_from(peer, hash)

            # Only the parents of what this peer just announced, and only
            # briefly: a node does not serve a block it has not got, so asking
            # it repeatedly for the other side's history is a minute spent
            # timing out rather than watching.
            for hash, miner in list(minted.items()):
                parent = parents[hash]
                if parent not in minted:
                    try:
                        parents[parent], minted[parent] = block_from(peer, parent, 0.5)
                    except AssertionError:
                        continue
                if {miner, minted[parent]} == set(miners):
                    return

    raise AssertionError(
        f"neither node ever mined on the other's chain within {CONVERGENCE}s; "
        f"saw {len(minted)} blocks, mined by "
        f"{[sum(1 for m in minted.values() if m == who) for who in miners]}"
    )
