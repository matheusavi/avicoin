# Architecture Decision Records

Each ADR captures one significant decision: the context, the options weighed, the
choice, and its consequences.

**Start with [ADR-0001](0001-v1-scope.md)** — it bounds what v1 is, and therefore
which other decisions matter at all.

**Status legend:** `Proposed` (a recommendation on the table, awaiting the
owner's ruling) · `Accepted` (decided) · `Superseded by ADR-NNNN` · `Rejected`.

> ⚠️ A `Proposed` ADR records a recommendation and the options — it is **not** a
> made decision. Nothing is binding until its status flips to `Accepted`.

## Numbering

**A number is assigned when an ADR is written, never before.** Numbers are
append-only: the next ADR takes the next free number regardless of topic, and
existing ADRs are never renumbered.

Reserving numbers for planned decisions was tried and abandoned. It collided
within a single session — one grilling round produced two ADRs, which knocked
every later reservation out of alignment and left a decision carrying the
invented id `ADR-0002-econ`. An undecided topic is referred to by name, never by
a number.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](0001-v1-scope.md) | v1 scope: a trimmed, demoable spine | Accepted |
| [0002](0002-output-locking-model.md) | Output locking model — Script + a minimal VM | Accepted |
| [0003](0003-transaction-witness-format.md) | Transaction witness format | Accepted |
| [0004](0004-sighash.md) | Sighash: what a spender signs | Accepted |
| [0005](0005-address-encoding.md) | Address encoding | Accepted |
| [0006](0006-monetary-model.md) | Monetary model | Accepted |
| [0007](0007-genesis-and-network-parameters.md) | Genesis block and network parameters | Accepted |
| [0008](0008-coinbase.md) | Coinbase | Accepted |
| [0009](0009-difficulty-and-timestamps.md) | Difficulty retarget and timestamp rules | Accepted |
| [0010](0010-merkle-construction.md) | Merkle construction | Accepted |
| [0011](0011-network-identity-and-fields.md) | Network identity and transaction field policy | Accepted |
| [0012](0012-reorg-and-undo-data.md) | Reorg and undo data | Accepted |
| [0013](0013-persistence.md) | Persistence | Accepted |
| [0014](0014-functional-test-suite.md) | Functional test suite: Python, driving real binaries | Accepted |
| [0015](0015-peer-identity-and-duplicate-connections.md) | Peer identity is the version nonce, and duplicates break the tie by it | Accepted |
| [0016](0016-reconnecting-to-configured-peers.md) | A connection has to last before it resets the backoff | Accepted |

[TEMPLATE.md](TEMPLATE.md) is the starting point for a new record. It is not an
ADR and holds no number.

### Reading order

0002 → 0003 → 0004 are one design, decided together, each depending on the last:
how a coin is locked, how it is unlocked, and what a spender signs. Read them in
sequence.

0006 → 0007 → 0008 are likewise coupled: the unit and schedule, where the first
coins come from, and how each block mints its reward.

0009 and 0012 are the two decisions that reversed deferrals in 0001, and both
explain **why** in a way that generalises — worth reading even if difficulty and
reorg aren't the topic at hand.

## Open decisions

**None.** Every decision needed for M1–M7 has been made.

New questions get an ADR when they arise, taking the next free number. Two things
are deliberately *not* open questions:

### Settled during implementation, not by ADR

The rule is fixed; the constant is chosen when the code is written.

- **VM resource limits** (ADR-0002) — that there *are* explicit caps on script
  size, stack depth, and operation count, and that exceeding one fails the script.
- **Retarget window, per-block clamp, and starting difficulty** (ADR-0009) — with
  the stated intent that adapting to a 1000× hashrate change takes tens of blocks,
  not hundreds, and that starting difficulty suits a throttled miner.

### Deferred with their features

These get an ADR only if the feature returns. See ADR-0001.

- The terminal UI.
- Script beyond the ~12 opcodes in ADR-0002 — conditionals, numeric opcodes,
  `OP_CHECKMULTISIG`, the `OP_PUSHDATA` family.
- Sighash types other than ALL, and therefore partially-signed transactions.
- Timelocks and replace-by-fee — ADR-0011 deleted the fields they would use.
- Block pruning, compact blocks, and other network performance work.
