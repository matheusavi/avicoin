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

### The directory, as built

*2026-08-27, in M5.*

A data directory carries a **stamp**: a `network` file holding the parameter
set's name and its genesis hash. It is written on first open and verified on
every one after; a directory stamped by another network ends the process,
naming both. The hash is what the comparison turns on — a name could survive a
rename, a genesis could not.

The stamp is rewritten through a temporary name and a rename on every open.
The rename needs write permission on the *directory* rather than on one
existing file, which is the permission every ticket after this one actually
needs — truncating the stamp in place would have passed on a directory nothing
else could be created in. A crash between creating a fresh directory and
stamping it still leaves it unstamped; the next open stamps it, which is the
right answer for a directory that holds nothing yet.

**A directory is held exclusively while a node runs.** An advisory lock on a
`lock` file, taken *before* the stamp is read, makes the check and the write
that follows it one operation. Without it two nodes starting together on a
fresh directory both read no stamp and both write one, and the loser ends up
running against a directory stamped for the other chain — a hole the "one node
per directory" line in the documentation asserted rather than closed. The lock
lives in the open file rather than in its contents, so a node that dies takes
its claim with it and no stale lock has to be cleaned up by hand.

On Unix the directory is created `0700`. The wallet key lands here at mode
`0600`, and a key that strict inside a world-readable directory is a smaller
promise than it looks.

The path resolves through the configuration precedence like any other field,
defaulting to `.avicoin` under the home directory. The functional suite points
every node it launches at a directory inside its own sandbox — the default is
shared by every node on a host, including the developer's own.

### The block files, as built

*2026-08-27, in M5.*

`RecordFile` is one append-only file of framed records; `blocks.dat` and
`undo.dat` are two of them and share no state, so a torn write in one costs the
other nothing. A record is the network magic, a `u32` length, then the payload,
and the offset an append returns is the only way a record is addressed.

Opening walks the frames and truncates to where the last whole one ended. A
short header, foreign magic, a length past `MAX_RECORD`, or a payload running
past the end of the file all end the readable region rather than raising —
those are what a crash mid-append leaves, and a node that refused to start
because of one would be worse off than one that lost the record in flight. A
failed *read* is still an error, and is not mistaken for a torn write.

**Repair is bounded by what a crash can actually do.** A crash costs the one
record in flight, so a file unreadable further back than `MAX_RECORD` is
corruption, and opening refuses it by name instead of truncating. Truncating
unconditionally would answer a single flipped bit near the front of a large
file by deleting every good record behind it, silently, before any caller had
a handle on it.

The scan seeks over payloads rather than reading them. It runs over the whole
file at every startup and the answer it wants is an offset.

`MAX_RECORD` is twice `MAX_BLOCK_SIZE`: an undo record carries a whole `TxOut`
per input its block spent, so it can exceed the block that produced it, and the
bound still keeps a corrupt length prefix from asking for an arbitrary buffer.
The format is written down in [on-disk-format.md](../on-disk-format.md).

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
