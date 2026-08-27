# On-disk format

What a node writes into its [data directory](glossary.md), described so a file
can be decoded without reading the code that wrote it.
[ADR-0013](adr/0013-persistence.md) is where the choices are argued; this is
what they came out as.

All integers are little-endian, as everywhere else in this project.

## `lock` — the claim

Empty. A node holds an advisory lock on it for as long as it runs, so a second
node pointed at the same directory exits rather than sharing it. The claim is
the open file rather than its contents, so a node that dies releases it and no
stale lock has to be cleaned up by hand.

## `wallet.key` — the key

64 hexadecimal characters and a newline: the 32-byte secp256k1 private key, in
**plaintext**, at mode `0600` on Unix. Written when it is absent, loaded when
it is there, and refused when its mode lets anyone else reach it — refused
rather than narrowed, since whoever widened it may already have copied it. On
other platforms it inherits the directory's permissions and nothing is checked.

Written through `wallet.new` and a rename, both flushed. A key half-written by
a crash would not parse, and a key file that does not parse is refused for the
rest of the node's life.

Plaintext is a decision, and the README says why.

## `network` — the stamp

Text, two lines:

```
<network name>
<genesis block hash, hex, big-endian>
```

Verified on every open and rewritten. A node whose parameter set disagrees with
either line exits rather than running; the hash is what the comparison turns
on, since a name could survive a rename and a genesis could not. The hash is
big-endian, as every hash a person reads is.

It is written to `network.new` and renamed into place. The rename needs write
permission on the *directory*, which truncating the stamp would not — so
rewriting it is also the check that the directory can still be added to.

## `blocks.dat` and `undo.dat` — framed records

Both are append-only files of the same shape, and they share nothing: a torn
write in one costs the other nothing.

A record is:

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | network magic (`AVI1` on main, `AVIT` on test) |
| 4 | 4 | payload length, `u32`, at most `MAX_RECORD` = 2,000,000 |
| 8 | *length* | payload |

The next record begins immediately after. A record is addressed **only** by the
byte offset of its magic, which is what `append` returns and what the block
index stores.

`blocks.dat` payloads are blocks in the wire serialization — the same bytes a
`block` message carries, so a record extracted from the file is a `block`
message's payload without further work.

`undo.dat` payloads are **undo records**: what one block spent, per transaction
and in the order it spent it, so a reorg can put it back.

| Field | Encoding |
|---|---|
| transaction count | compact-size |
| *per transaction:* entry count | compact-size |
| *per entry:* outpoint | 32-byte txid, then a `u32` index |
| *per entry:* height | `u32` — the height the coin was created at |
| *per entry:* coinbase flag | one byte, `0` or `1` |
| *per entry:* value | `u64` atoms |
| *per entry:* script | compact-size length, then that many bytes |

The height and the flag are not decoration: restoring a coin during a reorg
means re-checking its maturity against the new tip, which needs both
([ADR-0012](adr/0012-reorg-and-undo-data.md)).

The record has one entry list per transaction in the block, coinbase included —
so it lines up with `block.transactions` position by position, and the coinbase
list is empty.

### Reading a file back

Walk from offset 0. A frame ends the readable region — rather than being an
error — when any of these holds:

- fewer than 8 bytes remain,
- the magic is not this network's,
- the length is greater than `MAX_RECORD` (2,000,000, which is 2 × `MAX_BLOCK_SIZE`),
- the payload would run past the end of the file.

The length is checked against `MAX_RECORD` **before** any buffer is allocated
for it. It is the one number on disk that a torn write can make arbitrary, and
it gets the same treatment `ByteReader::read_count` gives a count a stranger
sends.

The file is then truncated to where the last whole record ended — but **only**
if what is being thrown away is at most one record's worth (`MAX_RECORD` plus a
frame header). That is the most a crash can leave behind. A file that is
unreadable further back than that has not been torn, it has been corrupted, and
opening refuses it rather than deleting the good records after the damage.

## The order these are written

A block's bytes and its undo record reach their files, **and are flushed**,
before the single `chain.redb` commit that records the block, moves its coins
and advances the marker. A crash therefore leaves either the old state or the
new one:

- between the files and the commit: the files hold bytes nothing points at,
  which cost disk and nothing else;
- inside the commit: it is one transaction, so it did not happen.

A disconnect writes no files and commits *before* it moves anything.

A block is appended **once**. Reconnecting one — after a reorg, or after a
restart with the marker behind the tip — reuses the offsets it already has, so
these files grow with the chain rather than with reorg churn. Only an *applied*
block is written: a body on a branch that never won is never here.

## `chain.redb` — the index, the coins and the marker

An embedded key-value store, [`redb`](https://github.com/cberner/redb), holding
three tables. **Every hash in it is in internal (little-endian) byte order** —
the bytes a hash *is*, not the reversed form `Display` prints and the `network`
stamp records. A hash copied from a log or from the stamp must be reversed
before it will match a key here. It is not a format this project defines, so what follows is what
the tables mean rather than how they are laid out. redb takes its own lock on
the file, which is a second answer to the same question `lock` answers.

**`headers`** — block hash (32 bytes) → the 80-byte header, then a `u64` offset
into `blocks.dat` and a `u64` offset into `undo.dat`. `u64::MAX` in either means
"not there": a header is recorded when the node learns of it, and the offsets
arrive later, when its block is applied.

**`coins`** — an outpoint (32-byte txid then a `u32` index) → a coin, encoded as
the `u32` height, the one-byte coinbase flag, and the output (`u64` atoms then a
compact-size-prefixed script). The same encoding an undo entry uses for its
coin.

**`markers`** — `best_block` → the 32-byte hash of the block the UTXO set has
been advanced to. Ordinarily *behind* the `headers` table's best tip, since
headers arrive ahead of bodies.
