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
making `Storage::remember_block` a no-op turns three of these red, and the
reorg scenario goes red without the chain switch it exists to survive.

The unit tests in `persist.rs` carry the two guarantees this file cannot see:
a no-op `catch_up`, and a disconnect that does not commit. The second is the
interesting one — a disconnect that never reaches the store leaves the *chain*
recoverable, because the marker still names a state the store agrees with and
the reorg simply happens again on the next start. What it corrupts is the
**UTXO set**, and nothing a peer can ask for shows that. Until M6's API can
report a balance, that guarantee is a unit test's, and
`a_disconnect_survives_a_restart` compares the whole table for exactly this
reason.

**What killing a process cannot show.** `SIGKILL` does not empty the page
cache, so nothing here can tell a flushed write from one the kernel has not
written yet. Removing the `sync` calls before the commit leaves every test in
this file green. That ordering — the files durable before the store names
them — is argued in ADR-0013 and visible in ten lines of `persist.rs`; a test
that claimed to prove it would need to lose power, not a process.
"""

import time

from framework.genesis import genesis_hash
from framework.messages import TEST_MAGIC, getdata_blocks, getheaders, hash256, ping
from framework.p2p import PATIENCE

# Longer than PATIENCE: a restart re-reads the store, and these run several
# processes on one core. ADR-0014's reason for PATIENCE being 8s rather than 20
# is that a serial suite pays it once per failing test, so this is spent only
# where a state genuinely takes several blocks to reach.
RECOVERY = 12.0


def a_node(net, *args: str):
    return net.node("--network", "test", "--host-address", "127.0.0.1:0", *args)


class Watcher:
    """One connection per node, asked repeatedly, re-dialled if it breaks.

    Dialling afresh for every question would exhaust a node's inbound slots in
    seconds. But a node under these tests is killed, restarted and put under
    load, and a connection it drops is a fact of a peer layer with caps rather
    than the thing being tested.

    **A redial re-sends the whole exchange**, never the tail of it. The failure
    this exists for is a first `sendall` that succeeds into a doomed socket and
    a second that raises: redialling on the second alone would drop the first
    on the floor, and a node that never heard the question would look exactly
    like a node that answered "genesis". A test that passed on that would be
    worse than the flake it replaced.
    """

    def __init__(self, net, node):
        self.net = net
        self.node = node
        self.peer = self._dial()

    def _dial(self):
        peer = self.net.dial(self.node.listening_on(), TEST_MAGIC)
        peer.handshake()
        return peer

    def ask(self, *messages):
        """Every message of one exchange, or the exchange again on a new
        connection. `OSError` covers the broken pipe and the reset; a timeout
        is one too, and re-asking is the right answer to that as well."""
        try:
            for message in messages:
                self.peer.send(message)
        except OSError:
            self.peer = self._dial()
            for message in messages:
                self.peer.send(message)

    def take_frames(self, command, window):
        """A dropped connection is empty; a node that framed something wrong is
        not. `framework/messages.py` raises `AssertionError` for a bad
        checksum, an unknown command or an oversized payload — assertions about
        the node — and swallowing those would turn the suite's most specific
        complaint into its least."""
        try:
            return self.peer.take_frames(command, window)
        except OSError:
            return []


def watch(net, node):
    return Watcher(net, node)


def test_a_watcher_that_redials_re_sends_the_whole_exchange():
    """The failure this guards is a first `sendall` that succeeds into a
    doomed socket and a second that raises. Redialling on the second alone
    drops the first, and a node that never heard the question looks exactly
    like a node that answered "genesis" — a green test observing nothing.
    """

    class Doomed:
        def __init__(self):
            self.sent = []
            self.dies_after = 1

        def send(self, message):
            if self.dies_after is not None and len(self.sent) >= self.dies_after:
                self.dies_after = None
                self.sent.clear()
                raise BrokenPipeError(32, "Broken pipe")
            self.sent.append(message)

    doomed = Doomed()
    watcher = Watcher.__new__(Watcher)
    watcher.peer = doomed
    watcher._dial = lambda: doomed

    watcher.ask(b"first", b"second")

    assert doomed.sent == [b"first", b"second"], doomed.sent


def chain_of(peer, window: float = 3.0):
    """Every header a node will give from genesis, oldest first. Its own
    account of where it is, taken over the wire rather than from stdout.

    A node at genesis has no headers to send and sends none, so silence and
    emptiness look alike. A `ping` behind the request tells them apart: the
    writer is FIFO, so a `pong` arriving with nothing before it means the node
    answered and had nothing to say. A node that sends neither has not
    answered, and that is a failure rather than an empty chain.
    """
    nonce = int(time.monotonic() * 1000) % (1 << 32)
    peer.ask(getheaders([genesis_hash()], TEST_MAGIC), ping(nonce, TEST_MAGIC))
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.take_frames("headers", 0.3):
            return [genesis_hash()] + [hash256(h) for h in frame.as_headers()]
        if any(frame.nonce == nonce for frame in peer.take_frames("pong", 0.1)):
            return [genesis_hash()]

    raise AssertionError(f"neither headers nor a pong within {window}s")


def serves(peer, hash_, window: float = 3.0) -> bool:
    """Whether the node will hand over a block's *body*.

    This is the question that matters and `getheaders` is not it: the header
    index is persisted the moment a header is accepted, so a node that lost
    every block would still recite the whole chain. A body comes from
    `blocks.dat`, so serving one is the node saying it really has the block.
    """
    peer.ask(getdata_blocks([hash_], TEST_MAGIC))
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


def paid_to(peer, hash_, window: float = 3.0) -> bytes:
    """The script a block's coinbase pays, which is the miner's address in the
    only form a peer can see."""
    peer.ask(getdata_blocks([hash_], TEST_MAGIC))
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        for frame in peer.take_frames("block", 0.3):
            if hash256(frame.as_block_header()) == hash_:
                return frame.coinbase_script()

    raise AssertionError(f"no block within {window}s")


def test_a_node_that_mined_and_stopped_mines_to_the_same_address(net):
    """Not "the key file is the same file" — that would prove the file
    survived, not that the node came back as the same miner. The coinbase
    script is the address in the only form a peer can see."""
    node = a_node(net, "--mine")
    peer = watch(net, node)
    before = past(peer, 1)
    paid_before = paid_to(peer, before[-1])

    restarted = net.restart(node, *TEST_NODE)
    after = past(watch(net, restarted), len(before))

    assert paid_to(watch(net, restarted), after[-1]) == paid_before


def a_prefix_of_the_other(one, two) -> bool:
    """Neither can contradict the other. The miner keeps running between the
    reading and the kill, so the node may legitimately have gone further than
    the chain we last saw — what it may not do is disagree about a block."""
    shorter = min(len(one), len(two))
    return one[:shorter] == two[:shorter]


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
    assert a_prefix_of_the_other(recovered, seen), "it came back on a chain it had shown"
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
        assert a_prefix_of_the_other(
            recovered, published
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


def dial(addresses):
    return [part for address in addresses for part in ("--addresses-to-connect", address)]


def agreed_within(one, two, window: float = RECOVERY):
    """The tip both report once they report the same one, or None."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        left, right = chain_of(one), chain_of(two)
        if len(left) > 3 and left == right:
            return left[-1]

    return None


def test_a_node_killed_during_a_reorg_reaches_the_tip_a_survivor_reaches(net):
    """The crash ADR-0013 was written for.

    The reorg is arranged rather than hoped for. Two miners build competing
    chains in isolation; a victim is first synced to the *shorter* one, then
    introduced to both — so it has to abandon blocks it already published.
    It is killed and restarted while that happens.

    What is asserted is not that the victim survives. It is that it ends up on
    the same chain as a node that was never touched, and that it really did
    abandon blocks — a victim that had never reorged would prove nothing.
    """
    loser = a_node(net, "--mine")
    winner = a_node(net, "--mine")

    # Isolated, so the two chains are genuinely different.
    abandoned = past(watch(net, loser), 1)
    victim = a_node(net, *dial([loser.listening_on()]))
    on_the_loser = past(watch(net, victim), 1)
    assert a_prefix_of_the_other(on_the_loser, abandoned), "it synced the losing chain"

    # The loser stops here, so which branch wins is settled rather than raced —
    # a scenario that depended on a race would be a scenario that flakes.
    loser.stop(cleanup=False)
    ahead = past(watch(net, winner), len(on_the_loser) + 4, window=RECOVERY * 2)
    assert len(ahead) > len(on_the_loser), "the branch it must move to is heavier"

    to_winner = dial([winner.listening_on()])
    survivor = a_node(net, *to_winner)
    victim = net.restart(victim, *TEST_NODE, *to_winner)

    for _ in range(3):
        victim.kill()
        victim = net.reuse(victim, *TEST_NODE, *to_winner)
        past(watch(net, victim), 1, window=RECOVERY)

    watching = watch(net, victim)
    settled = agreed_within(watching, watch(net, survivor), window=RECOVERY * 2)

    assert settled is not None, "the killed node never caught up with the untouched one"
    final = chain_of(watching)
    assert not a_prefix_of_the_other(
        final, on_the_loser
    ), "it never left the branch it started on, so nothing was recovered from"
