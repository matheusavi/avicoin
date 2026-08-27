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
    chain:   Chain,        // block index, connected tip, bodies, undo records
    utxo:    UtxoSet,      // Outpoint -> Coin { output, height, from_coinbase }
    mempool: Mempool,      // txid -> Transaction
    peers:   PeerTable,    // PeerId -> PeerHandle { address, origin, handshake, tx: SyncSender<Vec<u8>> }
    wallet:  Wallet,
    config:  Config,
    log:     Log,          // bounded ring; every entry also goes to stdout
    nonce:   u64,          // minted per run; how a node recognises itself on the wire
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
  messages → dispatch. Its read timeout is the handshake's, so a peer that
  connects and then says nothing wakes it rather than parking it forever;
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

Delivery is also gated on **Ready** — see [the handshake](#the-handshake).

Dropping it is not on its own enough to end the connection, and the reason is
worth knowing: `mpsc` hands the writer every *buffered* message before it ever
reports `Disconnected`, so a writer whose queue was full goes on writing to the
socket that stalled. The write half therefore carries a **30s write timeout**.
That, not the table, is what guarantees a peer which stopped reading eventually
loses its connection — with or without anything evicting it. Teardown is bounded
by that timeout rather than immediate, so a peer can briefly outlive its table
entry.

`OUTBOUND_QUEUE` bounds messages and `MAX_QUEUED_BYTES` (4 MiB per peer)
bounds what they weigh. A peer whose socket has stalled is dropped at
whichever it reaches first, which is the point: 128 pongs is four kilobytes,
128 blocks would be 128 megabytes, and only one of those was ever a bound.
The writer subtracts as it drains, so the figure is what is actually waiting
rather than a count of sends.

**`MAX_PEERS` is 32, and the policy at the cap is to refuse the newcomer** rather
than evict an established peer — there is no peer scoring to evict on yet. The
cap exists because each connection may legally hold `MAX_PAYLOAD_SIZE` in its
`recv_buffer`, so the exposure is `peers × 32 MiB`; this bounds the multiplier
without making it small, and lowering the per-connection ceiling is separate
work.

**Inbound and outbound do not share it.** Accepted connections may take at most
`MAX_INBOUND` (24) slots; the remaining `RESERVED_OUTBOUND` (8) can only be
filled by a connection this node chose to make. Without that, 32 inbound
connections leave nowhere to dial from, and every peer the node can see is one
the attacker allowed — the eclipse-attack precondition.
[ADR-0018](adr/0018-reserved-outbound-slots.md) has the alternatives, and states
plainly what it costs: a listen-only node accepts 24 rather than 32.

The two threads share a fate, in both directions. The reader ending releases the
registration — before joining the writer, or the sender it holds would keep the
writer alive forever — so the writer sees `Disconnected` and stops. The writer
ending drops the write half, whose `Drop` shuts the socket down and wakes the
reader, which on an established connection would otherwise sit out its full read
timeout. Neither can outlive the other, and neither can outlive its entry in the
table.

### The handshake

A connection is not a peer until both sides have identified themselves. The
writer's **first** write is our `version` — ahead of the outbound queue, not in
it, so nothing we enqueue can precede the message that says who is speaking. A
received `version` is answered with a `verack`, and the peer is **Ready** once
both of theirs have arrived: `AwaitingVersion → AwaitingVerack → Ready`, on
`PeerHandle`.

The state advances only on what the *peer* sends, and only in that order. A
`verack` before any `version`, or a second `version` after Ready, ends the
connection — a handshake happens once, and treating a repeat as a fresh one is
how a peer resets whatever the handshake was gating.

`HANDSHAKE_TIMEOUT` (20s) is an **absolute deadline**, checked on every turn of
the read loop rather than only when a read expires: a peer that dribbles legal
traffic it never completes a handshake with keeps the read returning, and a
per-read timeout would never fire on it. Failing it ends the connection through
the same path as any other read error, so the slot and the `recv_buffer` go with
it.

**Nothing is sent to a peer that is not Ready.** `send_to` queues nothing and
reports `Delivered::NotReady`; `broadcast` skips the peer and does not count it;
the writer holds its ping. `NotReady` is not a connection failure — a peer may
legally ping us mid-handshake, and the answer is silence, not a hang-up.

That rule cannot be absolute, because **our `verack` is what makes a peer
Ready**: gating it would gate it on itself, and no handshake would ever
complete. `PeerTable::answer_handshake` is the one way past, and it is open only
to a peer in `AwaitingVerack` — so the exception cannot be reached from any other
state, by any other message.

Their `verack` also *starts* the keep-alive. The writer's timer is the only thing
that pings, it fires once per `PING_INTERVAL`, and nothing wakes it early; a peer
that has just finished a handshake would otherwise wait out a full interval in
silence. So the reader enqueues the first ping as it advances the peer to Ready.

Until Ready, then, a connection has sent exactly one message — our `version` —
and answered exactly one, with a `verack`.

### Discovery

Becoming Ready also sends that peer a `getaddr`, and tells every *other* Ready
peer where this one listens. Both halves are needed:
[ADR-0017](adr/0017-peer-discovery.md) shows why asking alone leaves a three-node
mesh two edges short, because the pull is triggered by our handshake while the
fact we want is created by someone else's.

A `getaddr` is answered with peers' **listening** addresses, from their
`version`. `PeerHandle.address` is an ephemeral source port on anything we
accepted, so passing it on would hand out addresses nobody can dial.

An `addr` is dialled from, minus our own address, peers we hold, and anything
past `MAX_PEERS` — and none of it before the peer is Ready. Dials in flight are
bounded by a budget of their own rather than by peer slots, because holding a
slot across a `connect()` to an unroutable address denies the node every
connection it has, inbound included, for as long as the connect takes.

### Who a connection is talking to

A peer's identity is the **nonce in its `version`**, minted once per process
run — an address cannot serve, because an accepted connection shows an ephemeral
source port. Our own nonce means the connection loops back to this process and
it is dropped; a nonce already in the table means one peer on two connections,
and the one dialled by the larger nonce survives.

Why the tie-break is phrased over the nonce pair rather than from one node's
point of view — and why "keep the one we dialled" loses both connections — is
[ADR-0015](adr/0015-peer-identity-and-duplicate-connections.md).

A configured address gets a **`keep_connected` thread**: it dials, waits out the
connection, backs off, dials again. Nothing can redial a live connection because
the thread that would is blocked on it. The backoff resets on a connection that
*lasted*, not on one that merely connected —
[ADR-0016](adr/0016-reconnecting-to-configured-peers.md).

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
| `util.rs` | HASH256, HASH160, compact-size | Built |
| `config.rs` | Resolves configuration and validates addresses into `SocketAddr`; `resolve` is the canonical statement of precedence. One value is written back after it: `main` replaces `host_address` with the address the listener bound, since `:0` asks the OS to choose and `version` must advertise the choice | Built |
| `messages/` | `Header`, `Message<T>`, `Payload` trait, `MessageReceived` dispatch | Built (ping/pong, version/verack, getaddr/addr, inv/getdata/tx/block, getheaders/headers) |
| `protocol.rs` | Per-connection reader and writer threads; the writer drives the ping timer | Built |
| `block.rs` | Header assembly, merkle construction, target math, `mine()` | Built — tree is correct and its leaves are wtxids (ADR-0010); a duplicated wtxid or a 64-byte transaction (ADR-0019) costs the block its root; not wired to the node |
| `transaction.rs` | `Transaction` / `TxIn` / `TxOut` / `Outpoint` / `Witness` / `Txid` / `Wtxid`, dual serialization | Built to ADR-0003/0008/0011 |
| `amount.rs` | `Amount` — atoms, `MAX_MONEY`, checked arithmetic | Built |
| `crypto.rs` | `k256` keypairs, compressed public keys, 64-byte low-S signatures, `PubKeyHash` | Built |
| `wallet.rs` | Keypair, `TxBuilder`, selection, change, signing; the key on disk at mode `0600` | Built (ADR-0013) |
| `block_storage.rs` | `blocks.dat` / `undo.dat` framing and offset reads | Built (ADR-0013) — the format is [documented](on-disk-format.md) |
| `data_dir.rs` | The per-node directory, and the stamp that says which chain built it | Built (ADR-0013) |
| `store.rs` | The block index, the UTXO set and the best-block marker in `redb` | Built (ADR-0013) |
| `api.rs` | HTTP/JSON over the state the node holds; the shapes are in [api.md](api.md) | Built — `/status`; the rest of M6 follows |
| `persist.rs` | `Storage` — the order things reach disk, and what a restart loads | Built (ADR-0013) |
| `script.rs` | Opcodes, stack, interpreter, resource limits | Built (ADR-0002) |
| `address.rs` | Base58Check — display edge only | Built (ADR-0005) |
| `node.rs` | `Node` / `SharedNode`, `PeerTable`, the `Handshake` state machine, `send_to` / `broadcast`, the `Log` | Built — the log has no reader until M6 |
| `blockchain.rs` | Block index, cumulative work, multiple tips, connect/disconnect, reorg | Built — index, connect and disconnect; reorg follows (ADR-0012) |
| `difficulty.rs` | Per-block retarget, timestamp rules | Built (ADR-0009) |
| `utxo.rs` | `Outpoint` → `Coin`; connect/disconnect and maturity. In memory, with `redb` as its durable mirror (ADR-0013) | Built |
| `mempool.rs` | Validated pending transactions, bounded | Built |
| `validation.rs` | The rules a transaction must satisfy, and the fee it pays | Built — block rules join it in M4 |
| `params.rs` | Network parameter sets; genesis derivation | Built (ADR-0007) |
| `miner.rs` | The throttled mining thread behind `--mine` | Built |
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
| ECDSA / secp256k1 signing | **Swapped** `secp256k1` → RustCrypto **`k256`**, behind `crypto.rs`. |
| SHA-256 | **`sha2`** (RustCrypto), same family as `k256` and `ripemd`, and held at the version `k256` pulls so the tree carries one SHA-256 rather than two. |
| Error handling | **Keep** `anyhow` — it already threads through every `ByteReader` read and call site. |
| Config / CLI | **Keep** `toml` + `serde`; **`clap`** parses CLI arguments. |
| Big-int target math | **Keep** `primitive-types` (`U256`), with **no default features**. Its `std` feature means `fixed-hash/std`, which enables an optional `rand 0.8` nothing here calls and which carries RUSTSEC-2024-0421. The arithmetic we use is in the core of the crate, so turning `std` off costs nothing and drops a whole `rand` from the tree. |
| Hex, randomness | **Keep** `rand` (key material comes from `rand`). `hex` is in `[dependencies]`: `Txid` and `Wtxid` display as reversed hex. |
| RIPEMD160 | **Added** `ripemd` (RustCrypto). ADR-0002: the HASH160 *composition* is Bitcoin's and is hand-rolled; RIPEMD160 itself is general-purpose cryptography from 1996. `sha2` and `digest` are already in `Cargo.lock`, so this adds no new transitive weight. |
| Block index & UTXO storage | **Added** `redb` (embedded key-value store). ADR-0013: this mirrors Bitcoin's own split — it hand-rolls block files and delegates its databases to LevelDB. The flat files are ours; a B-tree is generic plumbing. |
| JSON | **Added** `serde_json` (`serde` already present). |
| HTTP server | **Added** `tiny_http` rather than hand-rolling HTTP/1.1. The rule is to hand-roll *Bitcoin's* primitives; HTTP is not one, and it brings three small transitive crates rather than an async runtime. |

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
