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
than separate binaries: default is wallet/relay ("send only"), `--mine` starts a
miner thread, `--headless` swaps the interactive surface for plain stdout logging.

### Central shared state

```
struct Node {
    chain:   Blockchain,   // block index, best tip, height map, cumulative work
    utxo:    UtxoSet,      // Outpoint -> (value, locking commitment)
    mempool: Mempool,      // txid -> Transaction
    peers:   PeerTable,    // peer_id -> PeerHandle { addr, tx: Sender<Message>, state }
    wallet:  Wallet,
    config:  Config,
    log:     RingBuffer,   // in-memory log for the UI; mirrored to stdout in --headless
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
- a **writer** — drains a `Receiver<Message>` into the socket, and drives the
  ping timer via `recv_timeout`.

`TcpStream::try_clone()` gives the two halves independent handles. Registering a
peer stores its writer `Sender` in `PeerTable`; `broadcast()` locks the table and
pushes to every peer's channel. A slow or dead peer therefore blocks only its own
writer thread, never the node.

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
| `messages/` | `Header`, `Message<T>`, `Payload` trait, `MessageReceived` dispatch | Built (ping/pong) |
| `protocol.rs` | Connection loop | Built — to be split into reader/writer |
| `block.rs` | Header assembly, merkle root over wtxids, target math, `mine()` | Built — merkle leaf and pair order change per ADR-0003/0010; not wired to the node |
| `transaction.rs` | `Transaction` / `TxIn` / `TxOut` / `Outpoint` / `Witness`, dual serialization | Built — reshaped by ADR-0003/0008/0011 |
| `wallet.rs` | Keypair, `TxBuilder`, signing | Stubbed — UTXO selection, balance, change are TODO |
| `block_storage.rs` | `blocks.dat` / `undo.dat` framing and offset reads | Empty stub (ADR-0013) |
| `script.rs` | Opcodes, stack, interpreter, resource limits | Not built (ADR-0002) |
| `address.rs` | Base58Check — display edge only | Not built (ADR-0005) |
| `node.rs` | `Node` / `SharedNode`, peer registry, broadcast | Not built |
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
merely to drop a dependency. The *only* crate removed is `secp256k1`, because it
wraps Bitcoin Core's libsecp256k1 and violates the "no Bitcoin-specific library"
rule. That removal is a crate-for-crate swap, not a hand-roll.

| Concern | Decision |
|---|---|
| ECDSA / secp256k1 signing | **Swap** `secp256k1` → RustCrypto **`k256`**. The one crate dropped. |
| SHA-256 | **Keep** `sha256`. |
| Error handling | **Keep** `anyhow` — it already threads through every `ByteReader` read and call site. |
| Config / CLI | **Keep** `toml` + `serde` + `clap`. |
| Big-int target math | **Keep** `primitive-types` (`U256`). |
| Hex, randomness | **Keep** `hex` and `rand` (key material comes from `rand`). |
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
