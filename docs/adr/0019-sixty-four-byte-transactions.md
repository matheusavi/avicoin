# ADR-0019 — A 64-byte transaction is invalid

- **Status:** Accepted
- **Date:** 2026-08-27
- **Deciders:** @matheusavi

## Context

[ADR-0010](0010-merkle-construction.md) chose Bitcoin's merkle construction:
left-to-right pairing, the last node duplicated on an odd level. Bitcoin's trees
do not **domain-separate** leaves from internal nodes — both are
`HASH256` of 64 bytes — so a 64-byte *transaction* hashes exactly the way an
internal node does, and nothing in the bytes says which it was.

This codebase can hit 64 bytes. The smallest transaction under
[ADR-0003](0003-transaction-witness-format.md)'s shape is 53 bytes — version,
one input with a null outpoint and empty `coinbase_data` and witness, one output
with an empty `script_pubkey` — so 11 bytes of `script_pubkey` lands on the nose.

The exposure needs merkle **proof verification** to fool: a verifier walking a
path cannot tell a leaf from an internal node, so a crafted 64-byte transaction
can be presented as a node and a fabricated proof accepted. Nothing verifies
proofs today — there is no SPV path, no `merkleblock`, no compact blocks, and
[ADR-0001](0001-v1-scope.md) defers all three. It stops being harmless the moment
a light-client path is added, which is exactly when nobody will be thinking about
merkle internals.

## Options considered

### Option A — Domain-separate leaves from internal nodes
Prefix the preimage differently for a leaf and a node, as Bitcoin's own
successors do.

- **+** Closes the class structurally rather than by rule, and cannot be
  forgotten.
- **−** The tree stops being Bitcoin's. ADR-0010 chose Bitcoin's algorithm
  precisely so the construction can be checked against every reference
  implementation and worked example — and it is, by a known-answer test against
  a real block's published root. Domain separation deletes that test's subject.
- **−** It buys generality against an attack that needs a feature v1 does not
  have and has deferred.

### Option B — A 64-byte transaction is invalid *(chosen)*
One consensus rule, checked wherever a block's transactions are read.

- **+** The tree stays Bitcoin's, so the known-answer test keeps its meaning.
- **+** The rule is total and cheap: a length comparison per transaction.
- **−** A rule that can be forgotten rather than a structure that cannot. The
  same trade ADR-0010 already made with duplicate wtxids, for the same reason.
- **−** It forbids a small band of otherwise-legal transactions. Nothing needs
  them: a transaction that size carries an 11-byte `script_pubkey`, which is not
  the P2PKH template and not the hash-preimage lock.

## Decision

**Option B. A transaction whose witness-included serialization is exactly 64
bytes is invalid.**

The witness-included form is the one that matters, because it is what the block
body carries and what the `wtxid` hashes — and the `wtxid` is the leaf
([ADR-0003](0003-transaction-witness-format.md)). A rule over the
witness-excluded form would guard bytes nobody transmits.

Enforcement lands at the earliest point that exists: `get_merkle_root_hash`
refuses to produce a root for a block containing one, alongside the
duplicate-wtxid rule from ADR-0010. That is not merely convenient — a block with
no merkle root cannot be mined and cannot be validated, so the rule cannot be
bypassed by a path that forgot it. M4's block validation inherits both rather
than reimplementing them.

## Consequences

- One length check per transaction when a block's root is computed.
- Amends the open item in [ADR-0010](0010-merkle-construction.md)'s "What has
  landed", which recorded the question without answering it.
- If a light-client or compact-block path is ever added, this rule is what makes
  its proofs safe, and it is already in force rather than being retrofitted.
- The "do not cache a rejected block's hash as permanently invalid" requirement
  from ADR-0010 is **not** settled here. There is no hash cache to poison until
  M4's block index exists, and the rule belongs with it.
- Settles nothing in the glossary; "merkle root" already covers it.
