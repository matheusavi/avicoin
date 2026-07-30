# ADR-0010 — Merkle construction

- **Status:** Accepted
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
2. **Odd counts.** The implementation pairs the last node with itself, so `[a,b,c]`
   and `[a,b,c,c]` produce the same root — CVE-2012-2459.
3. **Pair ordering.** The implementation pops from the *end* of the vector, so it
   concatenates `(last, second-to-last)`. Bitcoin concatenates left-to-right.

The odd-count duplication is **block-level malleability** — structurally the same
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

## Consequences

- `get_merkle_root_hash` changes leaf source, pair order, and gains no complexity
  otherwise; its existing tests are re-baselined, since every root it has ever
  produced changes.
- Block validation gains one rule (duplicate wtxids) and one requirement on the
  *failure path* (do not poison the hash) — the latter being easy to implement and
  easy to forget, so it belongs in a test rather than only in prose.
- The tree remains non-injective. Correctness rests on the duplicate check
  running, which is a weaker guarantee than Option A's structural one, accepted in
  exchange for computing block hashes the way every Bitcoin reference does.
- Settles the glossary term **merkle root**.
