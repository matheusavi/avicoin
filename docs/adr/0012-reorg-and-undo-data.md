# ADR-0012 — Reorg and undo data

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

[ADR-0001](0001-v1-scope.md) deferred reorg handling, reasoning that a
cumulative-work chain switch is a large subsystem exercising a scenario a
three-node demo never reaches. Checking *why* that was safe showed it isn't.

**Without reorg, any second miner permanently splits the network.** Two miners
occasionally find blocks at the same height within the propagation window; at 30s
blocks that happens on the order of once per few hundred blocks, so **tens of
times a week**. A node that accepts the first valid chain it sees and never
switches will keep miner 1's block while its peer keeps miner 2's, and they never
reconcile. The splits accumulate and the network shatters.

That directly contradicts the goal: *if someone wants to join, the network should
adjust itself*. If joining means mining, the deferred design fragments instead of
adjusting. Same shape of error as the retarget deferral
([ADR-0009](0009-difficulty-and-timestamps.md)) — sound for a controlled
single-miner demo, wrong for a public network.

## Options considered

### Option A — Rewind by re-deriving the UTXO set from genesis
On discovering a heavier chain, rebuild the UTXO set by replaying from block zero.
- **+** No undo records to design, serialize, store, or keep consistent. Perhaps a
  third of the code.
- **+** A full replay costs seconds at this chain's size, on a path that runs a few
  times a week.
- **−** Reorg cost grows with chain height rather than with reorg depth, so it
  degrades steadily as the node runs for months.

### Option B — Per-block undo data *(chosen)*
Each block stores what it consumed, so a rewind walks back one block at a time.

## Decision

**Option B**, Bitcoin's approach. Reorg returns to v1, implemented with per-block
undo records.

**Undo record.** For every input a block spends, the record holds:

```
(Outpoint, TxOut, height: u32, is_coinbase: bool)
```

The `TxOut` is what gets restored to the UTXO set on disconnect. The **height and
coinbase flag are not optional extras**: if a restored output was a coinbase, its
maturity must be re-checked against the new tip
([ADR-0008](0008-coinbase.md)), and that check needs the height at which it was
created. Bitcoin's `Coin` structure records exactly these fields for exactly this
reason.

**Chain selection is by cumulative work**, not height — a shorter chain with more
total work wins, which is the only correct rule once difficulty varies per block
([ADR-0009](0009-difficulty-and-timestamps.md)). The block index tracks cumulative
work per block and tolerates **multiple tips**.

**A switch** finds the fork point, disconnects blocks from the current tip back to
it (restoring outputs from each block's undo record and removing the outputs that
block created), then connects blocks forward along the new branch, validating each
and writing its undo record. Transactions in disconnected blocks return to the
mempool; transactions in connected blocks leave it.

Reorg cost is proportional to **reorg depth**, independent of chain height — so a
node that has been running for a year reorganises as cheaply as one started
yesterday.

## Consequences

- **Amends [ADR-0001](0001-v1-scope.md)**, which deferred reorg.
- Undo records must be **durable**, so this decision is entangled with
  [ADR-0013](0013-persistence.md): they are written alongside blocks and must
  survive a crash, or a node that dies mid-reorg cannot recover.
- **Maturity acquires a real purpose.** It exists to stop a coinbase being spent
  before a reorg could orphan it and cascade invalidity through its descendants —
  meaningless without this ADR, load-bearing with it.
- Block validation must be re-runnable during a connect, so validation cannot
  depend on ambient state beyond the UTXO set and the block index.
- A maximum reorg depth is **not** imposed. Nothing in this design needs one, and a
  cap would be a second chain-selection rule contradicting cumulative work.
- Settles the glossary terms **reorg**, **cumulative work**, and **undo record**.
