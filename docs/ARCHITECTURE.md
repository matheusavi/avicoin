# Avi Coin — Architecture

The target design and the invariants that hold across it. This document answers
*what we are building and why it is shaped this way*.

- **What we are building and how much of it** → [ADR-0001](adr/0001-v1-scope.md).
- **Individual decisions and their trade-offs** → [docs/adr/](adr/).
- **Vocabulary** → [glossary.md](glossary.md). One term, one meaning.
- **The work itself** (milestones, specs, tickets) → the
  [GitHub project](https://github.com/matheusavi/avicoin/milestones). Not here.
- **How the code looks *today*, for an agent about to edit it** → `CLAUDE.md`.

---

## Shape of the system

One binary, one process per node. A node is a P2P peer that holds a chain, a UTXO
set, a mempool, a wallet, and a set of connections. Roles are runtime flags rather
than separate binaries: default is wallet/relay ("send only") and `--mine` starts a
miner thread. There is no `--headless`: it existed to distinguish stdout from a
full-screen terminal UI, and that UI is out of v1 scope, so stdout is the only
output mode.

### Central shared state

```
struct Node {
    chain:   Blockchain,   // block index, best tip, height map, cumulative work
    utxo:    UtxoSet,      // Outpoint -> (value, locking commitment)
    mempool: Mempool,      // txid -> Transaction
    peers:   PeerTable,    // PeerId -> PeerHandle { address, origin, tx: SyncSender<Vec<u8>> }
    wallet:  Wallet,
    config:  Config,
    log:     RingBuffer,   // bounded; every entry also goes to stdout
}
type SharedNode = Arc<Mutex<Node>>;
```

One mutex over the whole node. This is deliberate: the contention ceiling is
irrelevant at demo scale, and a single lock removes an entire class of ordering
bugs that would otherwise dominate the interesting work. If it ever needs
splitting, split it per-field and record an ADR.

### Concurrency model

Threads and channels. **No async runtime** — see
[dependency posture](#dependency-posture).

Per connection, **two threads**:

- a **reader** — blocking `read` loop → append to buffer → drain complete
  messages → dispatch;
- a **writer** — drains a `Receiver<Vec<u8>>` into the socket, and drives the
  ping timer via `recv_timeout`.

The channel carries **already-framed bytes**, not `Message<T>`: payload types
differ per message, so a channel of `Message<T>` would need an enum of every
message type, re-added at each new one. Serializing at the enqueue site also puts
the failure where a caller can see it, and lets a future `broadcast()` frame once
and hand the same bytes to every peer.

`TcpStream::try_clone()` gives the two halves independent handles.
`spawn_connection` registers the peer — one place, so dialling and accepting
cannot drift — and a guard removes it on any exit, including a panic.
`broadcast()` locks the table and pushes to every peer's channel.

The table holds each peer's **only** sender, and that is load-bearing: removing
the entry drops the last sender, the writer sees the disconnect, and its shutdown
wakes the reader. So dropping a peer ends its connection rather than leaving two
threads and a 32 MiB `recv_buffer` running outside the table meant to bound them.
The reader therefore sends through the table rather than holding a clone.

Delivery is `try_send`, never a blocking send: one stalled socket must not hold
the node's lock and stop delivery to everyone else. A peer whose queue is full
past `OUTBOUND_QUEUE` (128 messages) is **dropped**, not buffered.

Dropping it is not on its own enough to end the connection, and the reason is
worth knowing: `mpsc` hands the writer every *buffered* message before it ever
reports `Disconnected`, so a writer whose queue was full goes on writing to the
socket that stalled. The write half therefore carries a **30s write timeout**.
That, not the table, is what guarantees a peer which stopped reading eventually
loses its connection — with or without anything evicting it. Teardown is bounded
by that timeout rather than immediate, so a peer can briefly outlive its table
entry.

`OUTBOUND_QUEUE` bounds **messages, not bytes**. Today every queued message is a
32-byte pong, so it is a memory bound in practice; once blocks and transactions
are relayed it stops being one, and the queue will need a byte budget instead.

**`MAX_PEERS` is 32, and the policy at the cap is to refuse the newcomer** rather
than evict an established peer — there is no peer scoring to evict on yet. The
cap exists because each connection may legally hold `MAX_PAYLOAD_SIZE` in its
`recv_buffer`, so the exposure is `peers × 32 MiB`; this bounds the multiplier
without making it small, and lowering the per-connection ceiling is separate
work. Inbound and outbound share the cap, so a flood of inbound connections can
crowd out configured dials — acceptable while peers come from a static list, not
once discovery lands in M2.

The two threads share a fate, in both directions. The reader ending releases the
registration — before joining the writer, or the sender it holds would keep the
writer alive forever — so the writer sees `Disconnected` and stops. The writer
ending drops the write half, whose `Drop` shuts the socket down and wakes the
reader out of a `read` that has no timeout. Neither can outlive the other, and
neither can outlive its entry in the table.

The miner is one more thread, holding no lock while it grinds: it snapshots the
mempool, builds a candidate block, releases the lock, and only re-acquires it to
connect and broadcast a solved block.

### Invariants

These hold everywhere and are not up for per-module negotiation:

1. **All multi-byte integers are little-endian on the wire.** Hashes are handled
   little-endian internally and reversed to big-endian *only* for display.
2. **Every deserialization goes through `ByteReader`.** It is the single
   bounds-checked cursor; no manual slicing. All reads return `anyhow::Result`.
3. **Hashing is double SHA-256** (`util::get_hash`) for txids, block hashes,
   merkle nodes, and header PoW.
4. **Variable-length counts use compact-size encoding.**
5. **Encoding never enters consensus.** Human-facing encodings (Base58Check
   addresses, hex) are computed at the wallet/UI edge and are never serialized
   into a transaction or committed by a txid. See ADR-0002 and ADR-0005.
6. **Every wire format has a round-trip test.** serialize → parse → compare is
   the standard pattern for any new format.
7. **A txid never covers a witness; a merkle leaf always does.** `txid` hashes
   the witness-excluded serialization and is what an `Outpoint` references, what
   a mempool is keyed by, and what a signature covers. `wtxid` hashes the
   witness-included form and is the merkle leaf. They are separate newtypes
   precisely because substituting one for the other is a silent consensus bug.
   See ADR-0003.

### Module map

| Module | Role | State |
|---|---|---|
| `byte_reader.rs` | Bounds-checked deserialization cursor | Built |
| `util.rs` | HASH256, compact-size | Built |
| `config.rs` | Resolves configuration and validates addresses into `SocketAddr`; `resolve` is the canonical statement of precedence | Built |
| `messages/` | `Header`, `Message<T>`, `Payload` trait, `MessageReceived` dispatch | Built (ping/pong) |
| `protocol.rs` | Per-connection reader and writer threads; the writer drives the ping timer | Built |
| `block.rs` | Header assembly, merkle construction, target math, `mine()` | Built — tree is correct (ADR-0010); leaves become wtxids with ADR-0003 in M3; not wired to the node |
| `transaction.rs` | `Transaction` / `TxIn` / `TxOut` / `Outpoint` / `Witness`, dual serialization | Built — reshaped by ADR-0003/0008/0011 |
| `wallet.rs` | Keypair, `TxBuilder`, signing | Stubbed — UTXO selection, balance, change are TODO |
| `block_storage.rs` | `blocks.dat` / `undo.dat` framing and offset reads | Empty stub (ADR-0013) |
| `script.rs` | Opcodes, stack, interpreter, resource limits | Not built (ADR-0002) |
| `address.rs` | Base58Check — display edge only | Not built (ADR-0005) |
| `node.rs` | `Node` / `SharedNode`, `PeerTable`, `send_to` / `broadcast`, the log `RingBuffer` | Built — nothing broadcasts until relay lands in M3; the log has no reader until M6 |
| `blockchain.rs` | Block index, cumulative work, multiple tips, connect/disconnect, reorg | Not built (ADR-0012) |
| `difficulty.rs` | Per-block retarget, timestamp rules | Not built (ADR-0009) |
| `utxo.rs` | `Outpoint` → output set, backed by the KV store | Not built |
| `mempool.rs` | Validated pending transactions | Not built |
| `params.rs` | Network parameter sets; genesis derivation | Not built (ADR-0007) |
| `api.rs` | HTTP/JSON read surface + e2e control surface | Not built |

Adding a new message type means: a `Payload` impl, a `MessageReceived` variant,
and a command-name arm in the parse dispatch. Nothing else.

---

## Dependency posture

The rules are in `CLAUDE.md`; this is the crate-by-crate consequence of them and
the authority when the two disagree on a specific crate.

The governing decision: **keep the crates already in the tree** — no rewrites
merely to drop a dependency. Two crates are removed, each for a reason that
outweighs that:

- **`secp256k1`** wraps Bitcoin Core's libsecp256k1, breaking "no
  Bitcoin-specific library". Replaced by `k256`.
- **`sha256`** was a wrapper over `sha2` whose conveniences we never used. It
  returned hex strings, so `get_hash` decoded back to bytes twice per call —
  5.5× slower than hashing directly, on the function `Block::mine()` runs in its
  nonce loop. It also pulled `tokio`, `async-trait` and `bytes` into the tree
  (without tokio's runtime feature, so nothing async ever ran — but 20 crates
  for a wrapper). Replaced by `sha2`, the crate it wrapped.

Both are crate-for-crate swaps, not hand-rolls.

| Concern | Decision |
|---|---|
| ECDSA / secp256k1 signing | **Swap** `secp256k1` → RustCrypto **`k256`**. |
| SHA-256 | **`sha2`** (RustCrypto), same family as `k256` and `ripemd`. |
| Error handling | **Keep** `anyhow` — it already threads through every `ByteReader` read and call site. |
| Config / CLI | **Keep** `toml` + `serde`; **`clap`** parses CLI arguments. |
| Big-int target math | **Keep** `primitive-types` (`U256`). |
| Hex, randomness | **Keep** `rand` (key material comes from `rand`). `hex` is now used only by tests and lives in `[dev-dependencies]`; it returns to `[dependencies]` when something displays a hash. |
| RIPEMD160 | **Add** `ripemd` (RustCrypto). ADR-0002: the HASH160 *composition* is Bitcoin's and is hand-rolled; RIPEMD160 itself is general-purpose cryptography from 1996. `sha2` and `digest` are already in `Cargo.lock`, so this adds no new transitive weight. |
| Block index & UTXO storage | **Add** `redb` (embedded key-value store). ADR-0013: this mirrors Bitcoin's own split — it hand-rolls block files and delegates its databases to LevelDB. The flat files are ours; a B-tree is generic plumbing. |
| JSON | **Add** `serde_json` (`serde` already present). |
| HTTP server | **Add** a small HTTP crate (e.g. `tiny_http`) rather than hand-rolling HTTP/1.1. |

Hand-rolled from scratch, because building them is the point of the project:
Base58Check and addresses, the Script interpreter, the HASH160 composition,
message framing, compact-size, target-from-`n_bits`, sighash, merkle
construction, and chain sync.

Deferred with the features that need them: `crossterm` (terminal UI).

---

## Deliberately deferred

**[ADR-0001](adr/0001-v1-scope.md) is the authority** on what is out of scope for
v1 and why — including the criterion those cuts are judged against. It is not
repeated here, because two copies of a scope boundary drift, and these two
already had.

In one line: the terminal UI, Script beyond the opcode set in
[ADR-0002](adr/0002-output-locking-model.md), sighash types other than ALL,
timelocks and replace-by-fee, block pruning, and network performance work. Each is
a candidate second act, not a gap.

Difficulty retarget, reorg, and persistence were on that list and were **restored**
— each broke the public node rather than merely trimming it. ADR-0001 records why,
and the test it produced: *does the cut remove work, remove learning, or break the
demo?*
