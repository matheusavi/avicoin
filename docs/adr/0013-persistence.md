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

### The store, as built

*2026-08-27, in M5.*

Three `redb` tables: `headers`, `coins` and `markers`. The interesting one is
that a `Batch` is a single redb write transaction — everything one block changes
lands together or not at all, which is what makes a crash cost a block rather
than half of one. A batch that is dropped rather than committed leaves nothing,
and there is a test that says so, because the whole ordering argument rests on
it.

The `headers` table stores each header with the offsets of its block and its
undo record, and `u64::MAX` where there is none. Two writes, not one: a header
is recorded when the node learns of it, and the offsets only exist once its
block has been applied. That is also why the marker ordinarily sits *behind* the
best tip — headers arrive ahead of bodies, and always did.

`UtxoSet`'s callers are untouched, which was the point of `get` returning an
owned `Coin` back in M4. `UtxoSet::restored` and `BlockIndex::restored` take
what the store held; the index walks from genesis so a parent is never seen
after its child, and a header whose parent the store does not hold is
corruption rather than an orphan.

**A restart cannot remember which of two equal-work tips arrived first**, so it
settles that tie by hash. Arbitrary, but the same arbitrary answer every time —
a node that came back on a different branch after each restart would be worse
than one that came back on a fixed wrong one. It is the one place the
"first seen wins" rule in `blockchain.rs` cannot survive a restart, and saying
so is cheaper than a reader discovering it.

**The set is loaded whole into memory, and redb is its durable mirror rather
than its backing store.** This ticket's brief allowed for a set that reads
inside a read transaction — that is what `get` returning an owned `Coin` was
for, and it remains possible without touching a caller. It was not built that
way, because a set read through redb makes `Coins::coin`'s `Option` swallow a
storage error as "no such coin", and a storage error that reads as a missing
coin is a consensus bug. The cost is that the set's size stays a memory concern
at the scale ADR-0001 scopes; the option is still open behind the same
interface if that ever stops being true.

redb takes its own lock on the database file, so two `Store`s on one path is
refused independently of `DataDir`'s lock. Two answers to one question is the
right number here.
### The key, as built

*2026-08-27, in M5.*

`wallet.key` in the data directory: 64 hex characters and a newline, plaintext,
mode `0600` **on Unix** — on other platforms the file inherits the directory's
permissions and nothing is checked, which the README says rather than implying
a property that is not there. The mode is passed in the same `OpenOptions` call
that creates the file, because creating it readable and narrowing it afterwards
leaves a window where it is not.

A key file **anyone else can reach is refused, not narrowed**. Whoever widened
it may already have copied it, and a node that quietly fixed the mode and
carried on would hide exactly the event worth knowing about. The mode is read
from the open handle rather than from the path a second time: two lookups of
one name are two different files to anything that can swap them.

The write goes through a staging name and a rename, and flushes the file and
the directory entry. Everything else here treats a half-written record as the
cost of a crash; this file cannot, because a key that does not parse is refused
for the rest of the node's life, and the alternative — minting a new one — is
discarding coins.

**The directory's own mode is checked too.** `0700` on creation says nothing
about a directory that was already there, and anyone who can write to it can
unlink the key and leave their own — which is `0600` and passes every check the
key itself makes, while every block is mined to somebody else's address. A data
directory anyone else can write to is refused.

`Node::shared` takes the wallet rather than minting one, so the decision about
where a key comes from belongs to `main` and the data directory, and tests keep
an ephemeral one.

### The ordering, as built

*2026-08-27, in M5.*

`persist::Storage` is the type that knows the order, and the order is the
whole of it. Applying a block:

1. the block's bytes to `blocks.dat` and its undo record to `undo.dat`,
2. **both flushed**,
3. one `redb` commit carrying the index entry, every coin the block moved, and
   the best-block marker.

Every crash window lands on one side or the other. Between 2 and 3 the files
hold bytes nothing points at, which cost disk and nothing else. Inside 3, redb
is atomic, so it did not happen. The marker moves with the coins because they
are the same commit — which is why a node comes back at a block boundary rather
than inside one.

Disconnecting is the mirror and has no files to write, so it **commits first
and moves the set after**: a failed commit has to leave nothing moved, and
`unwind` cannot fail once it has started. A crash between the two costs the
in-memory set, which a restart rebuilds from the store anyway.

**Headers are committed in one write per batch**, with no offsets and no
marker. A peer's `headers` message carries up to two thousand, and the caller
holds the node lock while they are taken — two thousand durable commits under
it is the difference between a stalled node and a working one. That is why the marker ordinarily sits *behind*
the index's best tip: headers arrive ahead of bodies, and always did. It is the
ordinary state of a syncing node, not a crash artefact.

**Startup loads and then connects forward.** The index comes from the store, the
set is already materialised, and the tip is the marker. `Chain::catch_up` then
connects along the best chain as far as the bodies on disk allow — asking the
in-memory offsets which blocks are there rather than reading them, since
reading would parse the whole chain to answer a question a lookup answers — the same
`switch_to` a running node uses when a body finally arrives, not a separate
recovery path. It stops at the first body the node never received, because that
is a thing to ask a peer for rather than a corruption. A marker naming a block
the index does not hold *is* a corruption, and is said so.

Bodies and undo records in memory now hold only what has **not** been applied.
An applied block is durable, so keeping it would make a long-running node's
footprint grow with its chain for nothing — which is the growth this ADR opened
by naming. `Chain::body` falls back to `blocks.dat`, and that is what lets a
restarted node serve a block to a peer and undo one in a reorg.

A disk read is deliberately **not** cached. `getdata` reaches `Chain::body`, so
caching what it returns would hand a peer walking the chain a way to fill our
memory with blocks we already have on disk — the same bounding discipline
`MAX_PEERS` and `OUTBOUND_QUEUE` follow.

**The mempool is deliberately not persisted.** It holds transactions nobody has
committed to anything, every one of them is still held by whoever relayed it,
and the alternative is a node that comes back insisting on payments the network
has forgotten. `Node::stored` mints an empty one; nothing on disk mentions it.

**A block is written once.** `remember_block` reuses the offsets a block
already has, so a reorg reconnecting what it disconnected, or a startup
connecting forward past a lagging marker, appends nothing. Without that, disk
would grow with reorg churn rather than with the chain.

**A block on a losing branch is not durable.** Only an *applied* block reaches
`blocks.dat`; a body accepted onto a branch that never won lives in memory
until the process ends. After a restart its header is still indexed and its
body is not, so the node asks a peer for it again if that branch becomes worth
having. That is the right trade — a stranger can offer bodies for branches that
never win, and writing them all would let them fill a disk — but it means a
reorg after a restart may need one round trip that a reorg before it would not.

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
- Chain *bodies* become a disk concern rather than a memory one: ~200 MB/year for
  mostly-empty blocks at 30s. Pruning is not needed and is not implemented. The
  **UTXO set** is not: as built it is loaded whole into memory and mirrored to
  the store, for the reason the section above gives.
- Settles the glossary terms **data directory**, **best-block marker**, and
  **block file**.
