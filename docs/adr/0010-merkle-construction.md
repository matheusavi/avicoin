# ADR-0010 — Merkle construction

- **Status:** Accepted — context corrected 2026-07-31, see
  [Correction](#correction--the-existing-code-is-not-a-merkle-tree)
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

`Block::get_merkle_root_hash` (`block.rs:91-117`) predates every decision in this
directory. Three things were open:

1. **What the leaves are.** [ADR-0003](0003-transaction-witness-format.md) settled
   this: **wtxids**, not txids. Building the tree over witness-*including* hashes
   is what commits witnesses to the block header directly, and is why no coinbase
   witness commitment is needed. `block.rs:92` calls `get_tx_id()` and must call
   `get_wtxid()`.
2. **Odd counts.** Whatever replaces the current code must decide what happens to
   an unpaired node.
3. **Pair ordering.** Bitcoin concatenates left-to-right; the existing code does
   not.

Odd-count duplication is **block-level malleability** — structurally the same
problem ADR-0003 removed at the transaction level. Its payload is a denial of
service: an attacker duplicates a transaction to produce an invalid block with the
*same hash* as a legitimate one; a node that marks that hash permanently invalid
then rejects the real block when it arrives.

## Options considered

### Option A — Promote odd nodes unchanged
An unpaired node moves up a level untouched instead of being hashed with itself.
The construction becomes injective, so `[a,b,c]` and `[a,b,c,c]` give different
roots and the CVE class closes structurally.
- **−** Deviates from Bitcoin's algorithm, so block hashes are computed
  differently from every reference implementation and worked example.

### Option B — Bitcoin's algorithm plus Bitcoin's mitigation *(chosen)*
Keep duplicate-last, and reject blocks containing duplicate transactions — which
is exactly Bitcoin's own remedy.

## Decision

**Option B.** The tree is built the way Bitcoin builds it, and the vulnerability
is closed the way Bitcoin closes it.

- **Leaves are wtxids.**
- **Pairing is left-to-right.** This *is* Bitcoin's algorithm; the existing
  pop-from-the-end ordering is not, and has no rationale to preserve beyond being
  how it was first written. Since the leaf type is changing anyway, the ordering
  is corrected in the same edit.
- **Odd counts duplicate the last node**, as in Bitcoin.
- **New rule: a block containing two transactions with the same wtxid is invalid.**
  The check is on `wtxid` rather than `txid` because the tree is built over
  wtxids, so wtxid duplication is what creates the collision.

**A block rejected for duplicate wtxids must not have its hash cached as
permanently invalid.** This is the part that separates a fix from a half-fix. The
attack's payload was never the malformed block — it was poisoning the hash so the
legitimate block sharing it is refused later. Bitcoin's patch is careful about
exactly this, and rejecting without that care leaves the denial of service fully
intact.

## Correction — the existing code is not a merkle tree

*Added 2026-07-31. The decision below stands; the description of what it replaces
was wrong, and the work is larger than this ADR implied.*

This ADR was written believing the existing code built a tree with two defects:
reversed pair order, and self-pairing on odd counts. It does not build a tree at
all.

`get_merkle_root_hash` pushes each hash back onto the same vector it is popping
from, so results are consumed again within the same pass. For four leaves it
produces:

```
H(H(H(d, c), b), a)          a right-leaning chain
```

where Bitcoin produces `H(H(a,b), H(c,d))`. Verified by transcribing the loop and
comparing against a reference implementation: the two agree **only for a single
transaction** — where the root is trivially the leaf — and differ at every count
above one, by ordering at two and structurally from three upward.

**Nothing caught this**, and that is the more useful half of the finding:

- `prepare_for_mining` *requires* a merkle root rather than computing one, so
  `block_generates_correct_hash` passes the fixture's hardcoded genesis root
  straight through and never calls `get_merkle_root_hash`.
- `mines_generates_correct_hash` does call it via `mine()`, but asserts only that
  a nonce was found under the target — which **any** root satisfies.

So the function has never had a test that pins its output. A known-answer test
against a real block's merkle root would have failed on day one.

**Consequence for the decision below:** Option B is still right, but it is not the
one-line leaf change plus a pair-order fix described in the Consequences section.
It is a replacement of the whole construction. The acceptance criteria live in the
tracking issue.

### Landed 2026-07-31 — the construction, not yet the leaves

`block::merkle_root` replaces the loop: each level is built into a **new** vector,
paired left to right, duplicating the last node wherever a level has an odd count.
Feeding a level's results back into the vector being read was the whole defect.

It is pinned by a known-answer test — Bitcoin block 170's two transactions against
that block's published merkle root — plus the genesis single-leaf case and direct
structural assertions for four and six leaves. Six is what separates per-level
duplication from padding the leaf list to a power of two; the two agree at three
and five, so a smaller odd case would not have distinguished them. Restoring the
original algorithm turns four of these red.

Two parts of this decision remain, each blocked on work that does not exist yet:

- **Leaves are still txids.** They become wtxids when witness separation lands in
  M3 ([ADR-0003](0003-transaction-witness-format.md)). The tree algorithm is
  indifferent to which hash it is given, so this is a leaf change now, exactly as
  the Consequences section originally described.
- **The duplicate-wtxid rejection is not implemented**, nor the rule that such a
  rejection must not cache the block hash as permanently invalid. Both are block
  *validation*, which arrives with M4 — there is no validation path to add them
  to. The CVE-2012-2459 exposure they close is therefore still open, and stays
  tracked in the milestone rather than being quietly considered done here.

- `get_merkle_root_hash` is **rewritten**, not adjusted — see the Correction
  above. It gains a known-answer test against a real block, which is the thing it
  has never had.
- Block validation gains one rule (duplicate wtxids) and one requirement on the
  *failure path* (do not poison the hash) — the latter being easy to implement and
  easy to forget, so it belongs in a test rather than only in prose.
- The tree remains non-injective. Correctness rests on the duplicate check
  running, which is a weaker guarantee than Option A's structural one, accepted in
  exchange for computing block hashes the way every Bitcoin reference does.
- Settles the glossary term **merkle root**.
