# ADR-0020 — A bound on a transaction, and where validation runs

- **Status:** Accepted
- **Date:** 2026-08-27
- **Deciders:** @matheusavi

## Context

M3 made a peer able to hand the node a transaction and have it validated.
Validation is the first genuinely **CPU-bound** work a stranger can ask for: one
ECDSA verification and one script execution per input.

Two things were unbounded, and they compound.

**Nothing capped a transaction's size.** `MAX_PAYLOAD_SIZE` is 32 MiB and the
smallest input encodes to 38 bytes, so one legal `tx` message could name roughly
880,000 inputs — 880,000 signature verifications, from one message, before the
node decides it does not like the transaction.

**And that verification runs under the node's single mutex.**
`Registered::accept` takes the lock and calls `Mempool::accept`, which validates
before inserting. `ARCHITECTURE.md` already sets the miner's precedent — it
"holds no lock while it grinds" — and the 880,000-input case is the same shape
of problem: while one peer's transaction is verified, every other peer's reader,
writer, handshake and relay waits.

The lock itself is not the mistake. One mutex over the whole node is a deliberate
choice, made because the contention ceiling is irrelevant at demo scale and a
single lock removes an entire class of ordering bugs. What changed is that the
work done under it stopped being bounded by anything small.

## Options considered

### Option A — Cap the transaction, keep validation under the lock *(chosen for now)*
One consensus rule — a transaction's witness-included serialization must be at
most 100,000 bytes — bounds the inputs to roughly 2,600, and with them the work
any single message can demand.

- **+** One length comparison, checked before anything expensive.
- **+** Keeps `Mempool::accept` a single call that either holds a transaction or
  does not, which is the property every test and caller is written against.
- **−** A worst-case transaction still holds the lock for a noticeable moment.
  Bounded and rare, but real.

### Option B — Validate outside the lock, then re-check and insert
Gather the coins the transaction names under the lock, release it, verify, then
re-acquire and re-check before inserting.

- **+** No CPU-bound work under the lock at all.
- **−** Two-phase, so the re-check has to be exactly right or a transaction can
  be admitted against a UTXO set that moved underneath it.
- **−** **The race it guards against does not exist yet.** Only a block connect
  mutates the UTXO set, and there are no blocks until M4. Building the
  optimistic path now means building it without the thing that makes it
  necessary, and testing it against a hazard that cannot occur.

### Option C — A worker pool that verifies in parallel
- **−** Every problem Option B has, plus a scheduler, in a project whose
  concurrency model is deliberately threads and channels with no runtime.

## Decision

**Option A now, Option B in M4.**

**A transaction's witness-included serialization must be at most 100,000 bytes.**
It is a consensus rule rather than a relay policy: a bound only some nodes
enforce is one an attacker routes around, and there is no transaction anyone
needs to make that is larger. The figure is Bitcoin's standardness limit, for a
transaction of the same shape.

**Signature verification stays under the node lock until M4.** M4 is when it has
to move regardless: the miner cares about lock contention by construction, and
block connects start mutating the UTXO set, which is what makes the re-check in
Option B necessary rather than ceremonial. Doing it then means building the
optimistic path alongside the hazard it exists for. Tracked as an issue, not as
a sentence here.

### Option B, as built

*2026-08-27, in M4.*

A peer's transaction now takes three steps, and the lock is held for the first
and the last. Under it: the cheap refusals — already held, past the bound,
conflicting with something we hold — and a copy of just the coins the
transaction names. Outside it: the signatures and the scripts. Under it again:
`Mempool::admit`, which confirms every one of those coins is **still there and
unchanged**, and still spendable at the height the chain is at *now* — read
again rather than carried over, because a reorg lowers the tip and a stale
higher one would let an immature coinbase through.

The hazard the re-check exists for is real and arrived on schedule: a block can
connect while a signature is being verified, and spend the very coin the
transaction was validated against. There is a test that does exactly that.

### What still runs under the lock

**A block's validation does.** `Chain::accept` — and `Chain::disconnect`, which
calls `Mempool::accept` for every payment a disconnected block returns —
verifies signatures with the lock held, whether the block came from a peer or
from our own miner. A block is stranger-supplied too, so this is the same
exposure the transaction path just shed, smaller only because `MAX_BLOCK_SIZE`
caps it at 1 MB.

It is not covered here because the optimistic path does not transfer: a
transaction's re-check is "are these few coins unchanged", and a block's would
be "is the whole set the one this was validated against". Getting that wrong
connects a block against a set that moved, which is worse than the stall.
Tracked as its own issue, wanting M5's stored UTXO set as a source of read
snapshots.

## Consequences

- `check_shape` gains one rule, so it is refused before any coin is looked up.
- The worst case a peer can demand under the lock drops from ~880,000 signature
  verifications to ~2,600.
- M4 inherits a stated reason to move validation off the lock, rather than
  discovering it — and moves the transaction path only, leaving the block path
  named rather than fixed.
- `OUTBOUND_QUEUE` still bounds **messages, not bytes**, which `ARCHITECTURE.md`
  foretold would stop being a memory bound once transactions are relayed. This
  decision does not close that; it caps one transaction at 100 kB, so the queue's
  exposure is now 128 × 100 kB per peer rather than 128 × 32 MiB. Tracked
  separately.
