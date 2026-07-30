# ADR-0013 — Persistence

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

[ADR-0001](0001-v1-scope.md) deferred persistence, on the reasoning that a
restarted node resyncs from peers. That fails for the node v1 actually targets: a
**single public node**. With no peers to sync from, every provider reboot,
redeploy, OOM kill, or crash returns the explorer to height zero — the showcase
breaking on a schedule its owner does not control.

Two further pressures arrived with it. [ADR-0012](0012-reorg-and-undo-data.md)
made undo records mandatory, and they must be durable or a node that dies
mid-reorg cannot recover. And the chain is expected to run for months: at 30s
blocks that is ~20,000 blocks a week, so roughly a million a year.

`block_storage.rs` exists as an empty stub.

## What Bitcoin does

Bitcoin's storage is a **hybrid**, and the split is the interesting part:

- **Blocks** — `blocks/blkNNNNN.dat`. Raw blocks framed by a 4-byte network magic
  and a 4-byte length, appended in order, rolled at ~128 MiB. Hand-rolled flat
  files.
- **Undo data** — `blocks/revNNNNN.dat`, paralleling the block files.
- **Block index** — `blocks/index/`, a **LevelDB** database: hash → height, file,
  offset, status, cumulative work.
- **UTXO set** — `chainstate/`, also **LevelDB**: outpoint → coin.

Startup **loads** rather than replays. A best-block marker in the chainstate lets a
crashed node replay only the blocks it is missing, so replay is the *recovery*
path, not the normal one.

The reasoning holds for us: blocks are write-once, read-by-offset and bulky, which
flat files suit exactly; the UTXO set is random-access, mutated every block, and
must survive a crash, which a database suits exactly.

Worth noting: **Bitcoin itself takes a dependency here.** It hand-rolls the block
files and delegates the databases. The rule in `CLAUDE.md` permits the same —
an embedded key-value store is generic plumbing, not a Bitcoin library.

## Decision

**Bitcoin's hybrid.**

- **Hand-rolled `blocks.dat` and `undo.dat`**, append-only, each record framed by
  the network magic and a length prefix. This is the part worth building — it
  fills the `block_storage.rs` stub with a real on-disk format, and framing plus
  offset-addressed reads is squarely the house style.
- **An embedded Rust key-value store (`redb`) for the block index and the UTXO
  set.** Crash-safe by construction, random-access, and no hand-rolled B-tree.
  Chosen over `sled` for being pure Rust with a stable release.
- **Startup loads**: the index comes from the store and the UTXO set is already
  materialised. A best-block marker records how far the UTXO set has been advanced;
  if it lags the index tip after a crash, the node replays only the missing blocks
  from `blocks.dat`.
- **Per-node data directory**, so several nodes run on one host — which the local
  multi-node network and the e2e suite both require.
- **The wallet key lives in its own file**, mode `0600`, plaintext. Honest for a
  coin whose README disclaims real-world use; encryption would imply a security
  property that nothing else here provides.

## Consequences

- **Amends [ADR-0001](0001-v1-scope.md)**, which deferred persistence, and adds a
  milestone: **M5 Persistence**, moving the viewer and deployment milestones to M6
  and M7.
- Adds `redb` to `Cargo.toml` — the second dependency admitted this session, after
  `ripemd`.
- A node that restarts resumes at its real tip, so the public node survives the
  restarts its host will impose.
- **Crash consistency is now a property to test**, not an assumption: kill a node
  mid-write and confirm it recovers to a consistent tip. That belongs in the e2e
  suite, which is the only place it can be exercised honestly.
- Chain growth becomes a disk concern rather than a memory one: ~200 MB/year for
  mostly-empty blocks at 30s. Pruning is not needed and is not implemented.
- Settles the glossary terms **data directory**, **best-block marker**, and
  **block file**.
