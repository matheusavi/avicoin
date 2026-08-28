"""Networks, and the things that only go wrong when several nodes disagree.

Each of these describes a network and then waits on a **state**. Nothing here
reads stdout except to learn a port; everything asserted comes from the API.

Marked `scenario`, because they take minutes rather than seconds:
`pytest -m "not scenario"` is the subset a developer wants mid-change, and CI
runs the lot.

Proven by mutation, each one confirmed to have compiled in first:

- disabling `Chain::accept`'s reorg arm turns the partition, the racing and
  the crash-during-reorg scenarios red, and leaves the other three green —
  which is right: only those three make a node abandon a chain it built.
- stopping the node asking for the bodies a relayed block taught it about
  turns the partition scenario red. That path is what this file found: a node
  that came back with a fork of its own learned the heavier branch and then
  said "the heavier branch does not validate" for ever, because nothing asked
  for the bodies in between.
"""

import pytest

from framework.scenario import SETTLE, Scenario, agreed, until

pytestmark = pytest.mark.scenario


def a_network(net) -> Scenario:
    return Scenario(net)


def an_unfunded_address() -> str:
    """An address the genesis allocation does not pay, built by the suite's own
    encoder — so a balance there is one this test caused."""
    from framework.genesis import base58check
    from framework.messages import hash160

    return base58check(hash160(b"nobody has ever paid this"))


def test_a_node_joining_an_existing_network_reaches_its_tip(net):
    """Initial sync at more than two nodes, and through a node that did not
    mine any of it: the joiner is told about the *follower*, not the miner."""
    scenario = a_network(net)
    miner = scenario.start("miner", mining=True)
    follower = scenario.start("follower", knows=[miner])

    until(lambda: follower.height() > 3, what="the follower syncing", among=[miner, follower])
    reached = follower.height()

    joiner = scenario.start("joiner", knows=[follower])

    until(
        lambda: agreed([follower, joiner], beyond=reached) is not None,
        what="the joiner catching up with the network",
        among=[miner, follower, joiner],
    )
    assert joiner.height() > reached


def test_a_partitioned_network_converges_once_it_is_introduced(net):
    """Two miners that have never heard of each other build two chains, then
    one `POST /connect` heals it and cumulative work settles it.

    The assertion is that they agree **on the block at height 2** — a height
    both of them filled while apart, with different blocks. Agreeing there
    means one of them threw its own block away, which is a reorg and nothing
    else. A shared *height* would prove nothing; two nodes on two branches
    have the same height all the time.

    The left miner stops at the heal and the right one is waited past it, so
    which branch wins is settled rather than raced. Stopping the loser only
    *biases* the outcome; a scenario that asserts which side gave way has to
    know, and one that depended on winning a race would be one that flakes —
    `test_convergence` learned that twice.
    """
    scenario = a_network(net)
    left = scenario.start("left", mining=True)
    right = scenario.start("right", mining=True)

    until(lambda: left.height() > 2, what="the left chain", among=[left])
    until(lambda: right.height() > 2, what="the right chain", among=[right])

    apart = left.block_at(2)["hash"]
    assert apart != right.block_at(2)["hash"], "they were never really apart"

    left.stop()
    until(
        lambda: right.height() > 5,
        what="the winning chain pulling clear of the stopped one",
        among=[right],
    )
    left = scenario.restart(left, knows=[right])

    until(
        lambda: left.block_at(2) is not None
        and left.block_at(2)["hash"] == right.block_at(2)["hash"],
        window=SETTLE,
        what="the two chains agreeing on a block they filled apart",
        among=[left, right],
    )
    assert left.block_at(2)["hash"] != apart, "it abandoned the block it mined"


def test_a_killed_node_catches_up_with_the_network_that_kept_going(net):
    """Persistence and sync proven together, which is the pair M5 and M3 could
    each only half-prove."""
    scenario = a_network(net)
    miner = scenario.start("miner", mining=True)
    follower = scenario.start("follower", knows=[miner])

    until(lambda: follower.height() > 2, what="the follower syncing", among=[miner, follower])
    before = follower.height()

    follower.kill()
    until(lambda: miner.height() > before + 2, what="the network moving on", among=[miner])

    scenario.restart(follower, knows=[miner])

    until(
        lambda: agreed([miner, follower], beyond=before) is not None,
        what="the restarted node catching up",
        among=[miner, follower],
    )
    assert follower.height() > before


def test_two_miners_racing_end_up_on_one_chain(net):
    """The case reorg exists for, on real binaries.

    Two miners that know each other from the start race constantly at a
    one-second target, so they disagree often. What is asserted is that they
    agree on the block at a height they both filled — not that they have the
    same height, which two nodes on two branches have all the time.
    """
    scenario = a_network(net)
    first = scenario.start("first", mining=True)
    second = scenario.start("second", mining=True, knows=[first])

    until(
        lambda: first.height() > 5 and second.height() > 5,
        what="both mining",
        among=[first, second],
    )

    def agree_at(height: int) -> bool:
        theirs, ours = first.block_at(height), second.block_at(height)
        return theirs is not None and ours is not None and theirs["hash"] == ours["hash"]

    # Deep enough to be settled: the tip itself flips while they race, and
    # asserting on it would be asserting on the race.
    until(
        lambda: agree_at(3),
        what="the two miners agreeing on a settled block",
        among=[first, second],
    )


def test_a_node_killed_during_a_reorg_ends_where_an_untouched_node_ends(net):
    """The crash M5 was written for, in a network rather than in one process.

    The reorg is arranged rather than hoped for: a victim is synced to the
    *shorter* chain, that chain's miner is stopped so the outcome is settled,
    and the victim is killed twice while it moves to the other one. It has to
    end where a node that was never touched ends, and it has to have really
    abandoned blocks.
    """
    scenario = a_network(net)
    loser = scenario.start("loser", mining=True)
    winner = scenario.start("winner", mining=True)

    until(lambda: loser.height() > 2, what="the losing chain", among=[loser])
    until(lambda: winner.height() > 2, what="the winning chain", among=[winner])

    victim = scenario.start("victim", knows=[loser])
    until(
        lambda: victim.height() > 2,
        what="the victim syncing the losing chain",
        among=[loser, victim],
    )
    abandoned = victim.block_at(2)["hash"]
    assert abandoned != winner.block_at(2)["hash"], "they were never really apart"

    # Stopped, so which branch wins is settled rather than raced.
    loser.stop()
    until(lambda: winner.height() > 8, what="the winning chain pulling ahead", among=[winner])

    survivor = scenario.start("survivor", knows=[winner])
    victim = scenario.restart(victim, knows=[winner])

    for _ in range(2):
        victim.kill()
        victim = scenario.restart(victim, knows=[winner])

    until(
        lambda: agreed([victim, survivor], beyond=5) is not None,
        what="the killed node and the untouched one agreeing",
        among=[winner, victim, survivor],
    )
    assert victim.block_at(2)["hash"] != abandoned, "it really did change branch"


def test_a_payment_made_on_one_node_is_spendable_from_another(net):
    """The send path end to end across a network: one node mines and sends,
    the other sees it confirmed and can spend what it was paid."""
    import subprocess

    from framework.genesis import read_lines
    from framework.http import get_json
    from framework.node import binary_path
    from framework.p2p import PATIENCE

    scenario = a_network(net)
    miner = scenario.start("miner", mining=True)
    watcher = scenario.start("watcher", knows=[miner])

    until(lambda: watcher.height() > 3, what="the watcher syncing", among=[miner, watcher])

    # Somewhere genesis did not fund, so "it was paid" has a before as well as
    # an after. An allocation address already holds fifty AVI, and asserting
    # its balance is positive would assert what was true before the send.
    paying = an_unfunded_address()
    assert get_json(watcher.api, f"/address/{paying}")[1]["atoms"] == 0

    sent = subprocess.run(
        [
            str(binary_path()),
            "send",
            "--api-address",
            miner.api,
            "--data-dir",
            str(miner.node.sandbox.data_dir),
            "--to",
            paying,
            "--amount",
            "1",
        ],
        capture_output=True,
        text=True,
        timeout=PATIENCE,
    )
    assert sent.returncode == 0, sent.stderr
    txid = sent.stdout.strip()

    # Confirmed, and seen by the node that did not make it — which is the
    # whole point of doing this across a network.
    until(
        lambda: get_json(watcher.api, f"/tx/{txid}")[1].get("block") is not None,
        what="the payment reaching the other node in a block",
        among=[miner, watcher],
    )
    paid = get_json(watcher.api, f"/address/{paying}")[1]
    assert paid["atoms"] == 100_000_000, paid
    assert paid["unspent_count"] == 1, paid
