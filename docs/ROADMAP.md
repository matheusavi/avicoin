# Avi Coin — Roadmap to v1

Status: planning. This document is the agreed build plan to take Avi Coin from a
ping/pong prototype to a working, deployable, Bitcoin-like network with a wallet,
mining, a terminal UI, and a web viewer.

## Guiding principles

1. **Roots / from-scratch.** Implement Bitcoin's own primitives ourselves
   (framing, compact-size, hashing usage, base58check, target math, sync). Learn
   by building, the way the existing codebase already does.
2. **Don't rewrite working code to shed a dependency.** Keep the crates already
   in use. For *new* code, prefer `std`; hand-roll only Bitcoin-specific
   primitives (see below), and reach for a small, general-purpose crate for
   generic plumbing rather than building it by hand.
3. **Never use a library created specifically for Bitcoin.** In particular
   `rust-secp256k1` (wraps Bitcoin Core's libsecp256k1) is **banned**. General
   crypto crates that merely support the secp256k1 curve are allowed.
4. **Concurrency = threads + channels** (no async runtime). Shared node state
   behind `Arc<Mutex<…>>`; per-peer writer channels for pushing messages.
5. **Keep `anyhow`.** It already threads through every `ByteReader` read and call
   site; migrating to a hand-rolled `Error` enum would mean rewriting working
   code, so we don't. (A typed-error migration can be revisited later if the
   hidden-error-set downside ever bites.)

## Dependency posture

The guiding decision: **keep the crates already in the tree** — no rewrites just
to drop a dependency. The *only* crate we remove is `secp256k1`, because it wraps
Bitcoin Core's libsecp256k1 and violates the "no Bitcoin-specific library" rule —
and that is a crate-for-crate swap, not a hand-roll.

Decisions made for this plan:

| Concern | Decision |
|---|---|
| ECDSA / secp256k1 signing | **Swap** `secp256k1` → RustCrypto **`k256`** (general-purpose, not btc-specific). The one crate we drop. |
| SHA-256 | **Keep** `sha256`. |
| Error handling | **Keep** `anyhow` (rewriting to a typed enum would touch every call site). |
| Config / CLI | **Keep** `toml` + `serde` + `clap`. |
| Big-int target math | **Keep** `primitive-types` (`U256`) in `block.rs`. |
| Hex, randomness | **Keep** `hex` and `rand` (key material comes from `rand`, not `/dev/urandom`). |
| Terminal UI | **Add** `crossterm` (full-screen: raw mode + input + rendering; hand-drawn layout, no higher-level TUI framework). |
| JSON (Phase 7) | **Add** `serde_json` (`serde` is already present). |
| HTTP server (Phase 7) | **Add** a small HTTP crate (e.g. `tiny_http`) rather than hand-rolling HTTP/1.1. |

Hand-roll from scratch (Bitcoin-specific primitives — this is the point of the
project, and none of it exists yet):

| Item | Notes |
|---|---|
| Base58Check / addresses | `address.rs`: Base58Check over `hash160(pubkey)`. A canonical Bitcoin thing to build. |
| Framing, compact-size, target-from-`n_bits`, sighash, sync | Already the house style; continue building these ourselves. |

Target `Cargo.toml` after the swap: **`anyhow`**, **`hex`**, **`primitive-types`**,
**`sha256`**, **`k256`**, **`rand`**, **`serde`**, **`toml`**, **`clap`** — plus
**`crossterm`** (Phase 6) and **`serde_json`** + a small HTTP crate (Phase 7);
`rstest` under dev-dependencies. (`k256` brings its own transitive tree; that is
the accepted cost of not hand-writing EC crypto.)

---

## Architecture target

Central shared state (`node.rs`):

```
struct Node {
    chain:   Blockchain,   // block index, best tip, height map, cumulative work
    utxo:    UtxoSet,      // Outpoint -> (value, address)
    mempool: Mempool,      // txid -> Transaction
    peers:   PeerTable,    // peer_id -> PeerHandle { addr, tx: Sender<Message>, state }
    wallet:  Wallet,
    config:  Config,
    log:     RingBuffer,   // in-memory log for the TUI; also mirrored to stdout in --headless
}
type SharedNode = Arc<Mutex<Node>>;
```

Per connection: **two threads** — a reader (blocking `read` loop → parse →
dispatch) and a writer (drains `Receiver<Message>` → socket; also drives the
ping timer via `recv_timeout`). `TcpStream::try_clone()` gives reader/writer
independent handles. Registering a peer stores its writer `Sender` in
`PeerTable`; `broadcast()` locks the table and sends to each peer's channel.

Roles are runtime flags on one binary: default = wallet/relay ("send only");
`--mine` starts the miner thread ("allow to mine").

---

## Phase 0 — Foundations: concurrency refactor

Enabling work everything else needs. Low external risk; all covered by existing
or new unit tests. (No dependency shed — the crates stay; see Dependency
posture.)

- `node.rs`: `Node` + `SharedNode`. Refactor `protocol.rs` `handle_connection`
  into `reader_loop` + `writer_loop` with a per-peer `Sender<Message>`; add
  `broadcast()` / `send_to()`; register/unregister peers in `PeerTable`.
- Replace scattered `println!` with a small logging layer writing to the
  `RingBuffer` and (in `--headless`) stdout.
- **Exit criteria:** two nodes still exchange ping/pong; `cargo test` green; a
  test proves a message enqueued from one thread is written to a peer's socket.

## Phase 1 — Handshake & peer management  *(connect to other nodes)*

- New messages (each = `Payload` impl + `MessageReceived` variant + parse arm):
  `version`, `verack`, `getaddr`, `addr`.
- Per-peer handshake state machine: send `version` on connect → require peer
  `version` + `verack` → mark `Ready`; only relay to `Ready` peers.
- Peer table: dedup, self-connection guard, seed reconnect/backoff, optional
  discovery via `addr` gossip.
- **Exit criteria:** 3 nodes booted with partial seed lists discover the full
  mesh.

## Phase 2 — Transactions end-to-end (send-only)  *(send transactions; wallet)*

- **Crypto swap:** replace `secp256k1` with `k256`; key material from `rand`.
- **Signing done right:** simplified sighash = `HASH256(tx serialized with input
  signatures blanked)`; `TxIn` carries `(signature, pubkey)`; verify signature
  over sighash and that `address(pubkey)` == referenced output's address.
  (Current code signs `outpoint.tx_id` — replace it.)
- `address.rs`: Base58Check over `hash160(pubkey)`; wallet exposes its address.
- `utxo.rs`: `Outpoint -> (value, address)`; wallet scans it for its own address,
  selects inputs, builds change, computes fee.
- `Wallet::send()`: real UTXO selection + change + signing (removes the stubs).
- `mempool.rs`: validate (inputs unspent, no double-spend, valid sigs, fee ≥ 0),
  insert, relay via `inv` → `getdata` → `tx`.
- New messages: `inv`, `getdata`, `tx`.
- **Genesis allocation:** hardcoded genesis + config-driven UTXO allocation so
  wallets are funded without mining (lets us test sending before mining exists).
- **Exit criteria:** fund wallet A via genesis, submit a tx on node A, assert it
  appears in B's and C's mempools; double-spend rejected.

## Phase 3 — Chain, block assembly, mining, coinbase  *(coinbase, tx pool, assemble block, send blocks)*

- `blockchain.rs`: genesis, block index (hash→block/height), best tip; connect/
  disconnect block updates the UTXO set.
- **Coinbase tx:** null-outpoint input, no signature, outputs = subsidy + fees to
  miner's address; coinbase-specific rules.
- **Miner thread** (`--mine`): snapshot mempool → assemble block (coinbase first)
  → set merkle root / `n_bits` / time → `Block::mine()` → connect locally +
  broadcast.
- **Block relay & sync:** `block` message + `getheaders`/`headers` (or
  `getblocks`) so a late node reaches the tip (initial block download).
- **Exit criteria:** miner node advances height on all nodes; a tx submitted to a
  non-miner gets mined in and cleared from every mempool; miner balance grows by
  subsidy + fees.

## Phase 4 — Validation & difficulty  *(block validation, adjust difficulty)*

- Full block validation: PoW ≤ target, correct `n_bits` for height, merkle root
  matches, timestamp sanity, exactly one coinbase, inputs exist & unspent,
  `sum(in) ≥ sum(out)`, valid sigs, correct subsidy.
- Reorg handling via cumulative work: switch to heaviest chain, rewinding/
  replaying the UTXO set.
- Difficulty retarget every N blocks from actual vs. expected timespan (clamped),
  encoded back into `n_bits`.
- **Exit criteria:** unit tests per rejection rule; e2e where two miners briefly
  fork and all nodes converge on the heaviest chain.

## Phase 5 — Persistence  *(store blocks)*

- Implement `block_storage.rs`: append-only length-prefixed `blocks.dat` in a
  per-node data dir; rebuild block index + UTXO set on startup (periodic UTXO
  snapshot optional).
- Persist the wallet key to disk.
- **Exit criteria:** restart a node; it resumes at the same tip/balance without
  re-syncing from peers.

## Phase 6 — Terminal interface (full-screen)

- `crossterm`-based full-screen TUI over `SharedNode` (in-process; no RPC).
  Panels: peers, chain/tip/difficulty, mempool, wallet address+balance,
  scrolling log (from the Phase 0 ring buffer). Hand-drawn layout, raw-mode
  input; no higher-level TUI framework.
- Commands/keybinds: send tx (form), connect to peer, toggle mining, quit.
- `--headless` keeps plain stdout logging for servers/CI.
- **Exit criteria:** drive a small local network entirely from the TUI.

## Phase 7 — HTTP API, web viewer & deployment  *(webserver, deployed example)*

- `api.rs`: minimal HTTP server via a small crate (e.g. `tiny_http`) over
  `TcpListener`. Endpoints: `GET /info`, `/peers`, `/blocks?from=&count=`,
  `/block/{hash}`, `/mempool`, `/wallet`, `/tx/{txid}`; `POST /tx`,
  `POST /connect`. JSON via `serde_json`. This also becomes the e2e control
  surface.
- Web frontend: static HTML/JS/CSS (no framework), polling the API — block
  explorer, mempool, peers, send-tx form.
- Deployment: multi-stage `Dockerfile` (`cargo build --release` → slim runtime),
  `docker-compose.yml` for a local multi-node net, and a deploy config (Fly.io /
  Render / Railway) running one public node + web viewer with a persistent
  volume for `blocks.dat` and the HTTP port exposed.
- **Exit criteria:** a public URL shows a live node's chain growing.

## Phase 8 — Comprehensive e2e + CI

Tests grow every phase; this is the capstone. Drive nodes over the HTTP API
(retire the current stdout "Ping"/"Pong" grep).

- pytest fixtures bootstrap N nodes on distinct ports with temp data dirs, wire
  them via seeds / `POST /connect`, and assert: peer discovery; tx propagation
  (send-only); mine-and-include; balance updates; restart/persistence; fork
  convergence. Teardown kills procs and cleans dirs.
- CI: add `cargo fmt --check`, `cargo clippy -D warnings`, release build, and run
  the Python e2e (currently CI runs only `cargo test`).

---

## Feature-list coverage map

| Feature-list / requested item | Phase |
|---|---|
| Simple block mining ✅ (integrate into node) | 0/3 |
| Send transactions to another person | 2 |
| Create coinbase transaction | 3 |
| Create transaction pool (mempool) | 2→3 |
| Assemble mined block from pool | 3 |
| Send blocks to peers | 3 |
| Store blocks | 5 |
| Block validation | 4 |
| Adjust difficulty | 4 |
| Wallet: distribute keys / monitor UTXOs / sign / broadcast | 2 |
| Connect to other nodes | 1 |
| Send-only mode vs. mining mode | 2 (send) / 3 (`--mine`) |
| Terminal interface | 6 |
| Web viewer + deployed example | 7 |
| Multi-node e2e (bootstrap + send between nodes) | grows per phase, capstone 8 |

## Sequencing

`0 → 1 → 2` delivers "connect + send" (demoable thanks to genesis funding).
`3 → 4` delivers real mining/consensus. `5` hardens restarts. `6` and `7` are
interfaces and can run in parallel once state + API exist. `8` runs throughout,
finalized last.
