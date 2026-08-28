# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Avi Coin is a learning-only, Bitcoin-like cryptocurrency written from scratch in Rust, deliberately **without referencing Bitcoin's source code** (guided only by public docs like bitcoin.org). It reimplements Bitcoin's wire protocol, block/transaction serialization, proof-of-work mining, and wallet signing. It is not meant for real use.

## Dependency philosophy (hard rules)

- **Roots / from-scratch:** implement Bitcoin's own primitives ourselves (framing, compact-size, base58check, target math, sync). This is a learning project — building it is the point.
- **Keep working crates; minimize new ones:** don't rewrite working code just to drop a dependency. For *new* code prefer `std`; hand-roll Bitcoin-specific primitives, and pick the smallest general-purpose crate for generic plumbing.
- **Never use a library created specifically for Bitcoin.** `rust-secp256k1` (wraps Bitcoin Core's libsecp256k1) is **banned**; ECDSA uses RustCrypto **`k256`** instead. General crypto crates that merely support the secp256k1 curve are fine.
- **Concurrency = threads + channels**, no async runtime.
- **Keep `anyhow`** — it already threads through every call site (all `ByteReader` reads return `anyhow::Result`); migrating to a hand-rolled `Error` enum would mean rewriting working code, so we don't.

Crate-by-crate consequences of these rules live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#dependency-posture), which is the
authority when the two disagree on a specific crate.

## Where things live

| You need | Look in |
|---|---|
| What v1 is, and what's deliberately out | [docs/adr/0001-v1-scope.md](docs/adr/0001-v1-scope.md) — **read this first** |
| Target design, invariants, module map | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| A decision and why it went that way | [docs/adr/](docs/adr/) — the [index](docs/adr/README.md) also lists the decisions still open |
| What a term means | [docs/glossary.md](docs/glossary.md) |
| What a node writes to disk | [docs/on-disk-format.md](docs/on-disk-format.md) |
| What a node serves over HTTP | [docs/api.md](docs/api.md) |
| The work itself — epics, specs, tickets | GitHub milestones and issues, **not** this repo |

## Agent skills

### Issue tracker

Issues, specs, and tickets live on GitHub (`gh` CLI); milestones are the epics.
See [docs/agents/issue-tracker.md](docs/agents/issue-tracker.md).

### Triage labels

The five canonical labels, unrenamed. See
[docs/agents/triage-labels.md](docs/agents/triage-labels.md).

### Domain docs

Single-context. The glossary is `docs/glossary.md` (not a root `CONTEXT.md`), ADRs
are in `docs/adr/`, and **ADR numbers are assigned on write, never reserved** — an
undecided topic is named, not numbered. See
[docs/agents/domain.md](docs/agents/domain.md).

## Commands

```bash
cargo run -- send --to <address> --amount 1.5 --api-address 127.0.0.1:8080  # spend
cargo fmt                   # format; CI gates on `cargo fmt --check`
cargo clippy --all-targets -- -D warnings   # what CI's lint job runs
cargo build                 # build the node (debug binary at target/debug/avicoin)
cargo run                   # run a node — config.toml is optional
cargo test                  # run all Rust unit tests (inline #[cfg(test)] modules)
cargo test <name>           # run tests matching a substring, e.g. cargo test read_u64
cargo test byte_reader::tests::test_read_u16   # run one specific test by full path
```

Run the node with CLI overrides:

```bash
cargo run -- --host-address 127.0.0.1:34352 --addresses-to-connect 127.0.0.1:5000 --addresses-to-connect 127.0.0.1:5001
```

Run the functional suite (see [ADR-0014](docs/adr/0014-functional-test-suite.md)):

```bash
python3 -m venv .venv && .venv/bin/pip install -r requirements.txt   # once
.venv/bin/python -m pytest test/functional                           # all of it
.venv/bin/python -m pytest test/functional -k hostile                # a subset
AVICOIN_BIN=/path/to/avicoin .venv/bin/python -m pytest test/functional
```

It runs `cargo build` first, every time, unless `AVICOIN_BIN` says otherwise — "build only if missing" once let a `cargo clippy` run leave a stale binary behind, and the suite silently tested code that was not the code under test. With `direnv` installed, `.envrc` activates the venv and plain `pytest test/functional` works.

CI (`.github/workflows/tests.yml`) runs three jobs on every pull request and on pushes to `main`: **Format and lint** (`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`), **Unit tests** (`cargo test`) and **Functional tests** (`pytest`). All three must pass, and the lint job is separate so a red build says at a glance which kind of problem it is.

The lint job tracks **stable**, not a pinned version, so a Rust release can turn it red on code nobody touched — that already happened on this ticket, with a lint that did not exist in the toolchain it was written against. That is the gate working: pinning would stop it finding anything new, which is the whole point of having it. `rustup update stable` before wondering why CI disagrees with you.

There is **no crate-level `#![allow(dead_code)]`**, and there will not be: it would turn the gate off on the day it was installed. Every unused item is a decision instead — deleted where nothing will ever call it, `#[cfg(test)]` where the tests are the caller, and `#[allow(dead_code, reason = …)]` naming the issue that will call it where one exists. `wallet.rs`'s whole construction path carries the last kind, because the API deliberately has no `POST /send` and #139 is what gives it a caller. `cargo test` alone does **not** cover the functional suite — that is the whole reason the CI job exists, and why it shipped with the tests rather than after them.

## Configuration resolution

**built-in defaults → `config.toml` → CLI args (clap)**, each overriding the previous *where it supplies a value*. Absent is not the same as empty: an omitted field falls through to the layer below, an explicitly empty one is that layer's answer. **This section is the authority** — `config.rs::resolve` implements it and carries no prose of its own.

- `config.toml` is optional, and so is every field in it. A file that is present but unparseable, or that contains an unknown key, is a startup error rather than a silent fallback.
- Addresses are parsed into `SocketAddr` **at this boundary**, so a malformed address fails at startup naming the field and value, instead of panicking later inside whichever thread first tried to bind or dial. `network` is resolved the same way: the name selects a whole `params::Params`, and an unknown one is a startup error, not a fallback to mainnet.
- `network` defaults to `main`. It is not a field a running node can be pointed at halfway — it chooses the parameter set, and therefore a genesis block, so the two networks are separate chains rather than one chain with a flag ([ADR-0007](docs/adr/0007-genesis-and-network-parameters.md)).
- `data_dir` defaults to `.avicoin` under the home directory — or under the working directory when there is no home, which is the one place the resolver falls back silently rather than failing. It is where one node keeps its chain, its UTXO set and its key. **One node per directory**, enforced by an advisory lock rather than asserted. `DataDir::open` creates it, claims it, stamps it with the network that built it, and refuses one another network built ([ADR-0013](docs/adr/0013-persistence.md)) — before the listener binds, so a wrong path costs no port. An empty `data_dir` is a startup error rather than an absent value, the same rule `host_address` follows.
- `api_address` is **absent by default**, and absent means no listening socket at all — exposing a node to HTTP is a decision somebody makes. It is parsed at this boundary like every other address, and `main` binds it before spawning the server thread, so a taken port fails the process rather than a detached thread.
- One value is written back *after* resolution: `main` replaces `host_address` with the address the listener actually bound. `:0` asks the OS to choose a port, and the chosen one is what `version` must advertise for a peer to dial us back.
- With no `config.toml` and no arguments the node listens on `127.0.0.1:34352` on the **main** network with no peers, which is a valid standalone node — others can dial it.
- The repo's checked-in `config.toml` points `host_address` and `addresses_to_connect` at the same loopback address, so a single node connects to *itself* and exercises the ping/pong exchange.

## Architecture

The node is a small P2P server modeled on Bitcoin's message framing. `main.rs` binds the listener (so a bad address or a taken port fails the process, not a detached thread), then runs `protocol::listen` on one thread and gives each configured address a `protocol::keep_connected` thread. Inbound connections go through `spawn_connection`, which gives each its own thread — so the accept loop is never blocked, and one peer's failure is logged rather than taking the listener down.

`keep_connected` waits out its connection before dialling again, so a live one cannot be redialled — no check to race. The backoff resets only when a connection *lasted* `Retry::settled`, timed from the connect rather than from the attempt. [ADR-0016](docs/adr/0016-reconnecting-to-configured-peers.md) has both reasons; the checked-in `config.toml` is the counterexample that produced the first.

`spawn_connection` registers the peer in `node.peers` before handing off, and a `Registered` guard removes it on any exit including a panic. A refused connection — `MAX_PEERS`, or `MAX_INBOUND` for an accepted one ([ADR-0018](docs/adr/0018-reserved-outbound-slots.md)) — is dropped there and never becomes a thread pair.

Each connection then runs `handle_connection`, which splits into **two threads** over `try_clone()`d socket handles, joined by a *bounded* `sync_channel` of already-framed bytes:
- the **reader** blocks in `read` under the handshake timeout, appends to a growing `recv_buffer`, drains complete messages out of it, and *enqueues* replies (Ping → Pong, Version → Verack) rather than writing them;
- the **writer** owns every write. Its first is our `version`, passed in rather than queued so nothing can precede it. Then it drains the channel with `recv_timeout`, and that timeout *is* the ping timer — a `Ping` goes out every `PING_INTERVAL` (11s) whatever the reader is doing, but **only to a Ready peer**. The interval is a parameter of `write_loop`, so tests use milliseconds.

A peer is **Ready** only once its `version` and `verack` have both arrived; the state lives on `PeerHandle` and advances in one order, once. `HANDSHAKE_TIMEOUT` (20s) is an absolute deadline checked on every turn of the read loop — not a per-read timeout, which a peer sending legal traffic would reset forever.

**Nothing is sent to a peer that is not Ready** — `send_to` returns `Delivered::NotReady` and queues nothing, `relay` skips it, the writer holds its ping. The one way past is `PeerTable::answer_handshake`, open only to a peer in `AwaitingVerack`. Their `verack` is also what starts the keep-alive. Both are explained in [ARCHITECTURE](docs/ARCHITECTURE.md#the-handshake).

**Identity is the `version` nonce, never an address** ([ADR-0015](docs/adr/0015-peer-identity-and-duplicate-connections.md)). `Node::identify` runs when a `version` arrives: our own nonce drops the connection, and a nonce already in the table leaves exactly one of the two standing — the one dialled by the larger nonce. `PeerTable` has no address dedup left. The `version` also records where the peer *listens*, which is the only address worth passing on — `PeerHandle.address` is an ephemeral source port on anything we accepted.

**Discovery** ([ADR-0017](docs/adr/0017-peer-discovery.md)): becoming Ready sends that peer a `getaddr` **and** tells the other peers where it listens. The second half is what makes a mesh converge — a `getaddr` alone is answered from whatever its target happened to know at that instant. A `getaddr` is answered with Ready peers' listening addresses; an `addr` is dialled from, minus our own address, peers we hold, and anything past `MAX_PEERS`. Discovery dials carry their own budget (`MAX_DIALS_IN_FLIGHT`) and a `CONNECT_TIMEOUT`, deliberately *not* peer slots. The budget bounds a dial's **connect**, not the connection it opens — a share held for a connection's life would mean eight settled peers stop the node dialling a ninth — the ADR records why bounding them with `MAX_PEERS` is worse than the leak it fixes.

**Relay**: accepting a transaction into the mempool offers its txid to every Ready peer *except* the one it came from, and a mined block is offered to all of them. A block whose parent is unknown is held and its parent asked for from the peer that sent it, so a chain walks back one block at a time until it connects.

**Sync**: becoming Ready also sends a `getheaders` carrying a locator — ten hashes back from the tip, then doubling, ending at genesis — so two nodes find where they agree in `log(height)` hashes rather than by sending a chain. `Chain::add_headers` records a batch of headers — each once it has shown its own work, the `n_bits` the retarget rule requires, and a timestamp that passes — in **one** durable commit, and reports the first refusal so a peer's bad headers are not dropped in silence. Only then are bodies asked for. Asking for the bulk data first is asking a stranger to fill our memory. A full batch of `MAX_HEADERS` means the peer has more, so another locator goes out. An `inv` for a txid we do not hold produces a `getdata` **to that peer only** — a broadcast there would have every peer fetching what one of them offered. An `inv` for something we hold produces nothing, which is what stops two nodes trading the same payment forever. A `tx` goes through the same `Mempool::accept` a locally built one does: relay is not a way around validation.

`handle_messages` has **one gate**: everything that is not `version` or `verack` is ignored unless the peer is Ready. Put new message arms below it, not above.

Nothing calls `println!` outside `node::record`, which prints **and** appends to a bounded `Log` on the shared node — M6's HTTP API reads it. It prints *before* taking the lock: stdout is a blocking syscall, and holding the node across it would stall every peer behind a pipe nobody drains, which is the same rule `relay` follows with `try_send`. A poisoned lock is recovered rather than propagated, because logging must not be the thing that kills a thread.

`record` takes the lock for you, so **never call it while already holding the node lock**: std's `Mutex` is not reentrant, and the borrow checker does not stop you — a held guard and a helper that locks are both immutable borrows of the same `Arc`.

**The peer table holds each peer's only sender**, and the reader enqueues through it (`Registered::deliver`) rather than keeping a clone. That is deliberate and load-bearing: removing a peer drops the last sender, so the writer sees `Disconnected`, its shutdown wakes the reader, and the connection ends. A clone anywhere else turns "drop this peer" into bookkeeping while the threads and the peer's 32 MiB `recv_buffer` carry on outside the table meant to bound them.

The two ends share a fate: the reader ending releases the registration — **before** the join, or the table's sender keeps the writer alive — and the writer ending shuts the socket down, which unblocks the reader.

**Message framing (`src/messages/`)** is the core of the wire protocol:
- Built so far: `ping` / `pong` (an 8-byte nonce each), `version` (30 bytes: protocol version, node nonce, and a fixed-width IPv6-with-IPv4-mapped listen address), `verack` and `getaddr` (both empty), `addr` (a compact-size count then that many 18-byte addresses, capped at `MAX_ADDRESSES`), `inv` / `getdata` (the same list of 36-byte `(kind, hash)` entries, capped at `MAX_INVENTORY`, sharing one type because they differ only in what the receiver does with them), and `tx` (one witnessed transaction). The 18-byte address codec is `messages/net_address.rs`, shared by `version` and `addr`.
- `Message<T>` = `Header` (24 bytes) + typed `payload: T`. The header is 4 magic bytes — the selected network's, threaded through `Message::new` and `try_parse_message` rather than a constant, so a node cannot hold one network's magic and another's parameters ([ADR-0011](docs/adr/0011-network-identity-and-fields.md)) — a 12-byte command name, a 4-byte little-endian payload size, and a 4-byte checksum (first 4 bytes of the double-SHA256 of the payload).
- Any payload type implements the `Payload` trait (`get_raw_format`, `get_command_name`); adding a new message type means adding a `Payload` impl and a new variant + command-name arm in `MessageReceived` (`message.rs`).
- `MessageReceived::try_parse_message` is designed for streamed TCP data: it returns `(None, 0)` when the buffer holds only a partial message (so the caller keeps reading), and otherwise returns the parsed message and the number of bytes consumed. It validates magic bytes, enforces `MAX_PAYLOAD_SIZE` (32 MiB), and verifies the checksum before dispatching by command name.

**Serialization conventions** (shared by messages, blocks, transactions):
- All multi-byte integers are little-endian on the wire; hashes are computed little-endian and only reversed to big-endian for display.
- Variable-length counts use Bitcoin compact-size encoding (`util::get_compact_int` / `ByteReader::read_compact`).
- Hashing everywhere is **double SHA-256** via `util::get_hash` (Bitcoin's HASH256).
- `ByteReader` (`byte_reader.rs`) is the single bounds-checked cursor used for all deserialization; prefer it over manual slicing. All reads return `anyhow::Result`.

**Domain model (not yet wired into the network layer):**
- `block.rs`: `Header` is the 80 bytes proof-of-work covers, and the only part of a block a peer must send before its work can be checked; `Header::raw` is the one place those bytes are assembled, and mining rewrites four of them in place. `Block::search(from, until)` looks for a nonce in a range and `Block::seal()` is handed one: it builds the 80-byte header into `mine_array`, computes the merkle root of its transactions, derives the target from compact `n_bits`, and refuses a nonce that does not solve it. The miner uses both, which is what lets it hash in bursts and abandon a candidate when the tip moves. The root's leaves are **wtxids**, so the header commits witnesses directly; a block whose transactions repeat a wtxid, or contain one serializing to exactly 64 bytes ([ADR-0019](docs/adr/0019-sixty-four-byte-transactions.md)), has no root at all — which is the earliest point either rule can be enforced, since block validation does not exist yet.
- `transaction.rs`: `Transaction`/`TxIn`/`TxOut`/`Outpoint` with serialize/parse; `get_tx_id()` is the double-SHA256 of the raw format. Also `Txid` and `Wtxid`, declared by one macro so they share an implementation without sharing a type — nothing converts between them, because a `Wtxid` in an `Outpoint` makes a coin unspendable and a merkle tree over `Txid`s stops committing witnesses.
- `amount.rs`: `Amount` counts atoms and is never outside `0..=MAX_MONEY`. The bound is the type's invariant, so a sum of any number of them cannot approach `u64`'s ceiling; the arithmetic is checked anyway (ADR-0006). `subsidy(height)` is the emission schedule — 50 AVI right-shifted once per `HALVING_INTERVAL`, zero after 33 halvings. The ~2,016,000 AVI cap is what it sums to, not a rule anything checks.
- `crypto.rs`: `PrivateKey` / `PublicKey` / `Signature` over `k256`. A public key is 33 compressed bytes and a signature is 64 bytes of `r ‖ s`, both parsed by fixed width; signing normalises to low-S and `Signature::parse` refuses anything that is not. Nothing else in the tree touches `k256`.
- `send.rs`: the `send` subcommand, and the only way to spend. The API has **no `POST /send`** and will not: spending authority behind a public URL is the one thing a node must not offer. So the split is that the node holds the chain and this holds the key — `Wallet::read` takes the key file by path **without claiming the directory** (the node that owns it is running and holds the lock), `/status` and `/address` say what there is to spend, `TxBuilder` signs here, and `POST /tx` submits what any stranger could have submitted. It never *mints* a key: one the node has not got is an address nobody will pay. Its HTTP client is hand-rolled for the reason `api.rs`'s server is.
- `wallet.rs`: `Wallet::stored` keeps the key in `wallet.key` in the data directory — 64 hex characters, plaintext, mode `0600` on unix, created with the mode in the same call as the file so there is no window where it is wider, and put in place by a rename with both the file and the directory flushed. A file anyone else can reach is **refused rather than narrowed**: whoever widened it may already have copied it, and quietly fixing the mode would hide that. The mode is read from the open handle, not from the path a second time. Minting it per run, which is what happened before, meant every restart mined to a new address.
- `wallet.rs`, the rest: `Wallet` holds one `crypto` keypair and exposes its `Address`. `owns` recognises exactly the P2PKH template for its own hash — consensus accepts any script the interpreter validates, a wallet recognises one, and a non-standard output is valid and simply invisible. `TxBuilder` is the construction path: `pay` decodes an address (so a mistyped one fails before anything is selected), selection takes the largest coins first for determinism, change under `DUST` goes to the fee rather than becoming an output nobody will move, and `sign` is the only way a transaction comes *out of the wallet*. Not the only way one comes into existence — `Transaction` has public fields and `parse_raw` builds one from the wire — so "witnessed by construction" is a property of the wallet's path, not of the type.
- `address.rs`: `Address` holds a `PubKeyHash`, not text — so one that exists is one that encodes. `Display` is `Base58Check(0x17 ‖ hash)` and `FromStr` refuses a bad checksum, a wrong version byte, a non-base58 character, and a wrong length. Addresses never enter consensus: decode to a `PubKeyHash` before anything a txid covers ([ADR-0005](docs/adr/0005-address-encoding.md)).
- `script.rs`: `execute(script_pubkey, witness, txid) -> Result<()>` — `Ok` means unlocked, and the error says why not. Single-phase: the witness is data, so it seeds the stack and only `script_pubkey` runs. Success is exactly one item left and it truthy; an unknown opcode fails immediately; four limits (script size, stack depth, operations, stack item size) are checked as it runs. `p2pkh()` builds the one template that ships. Because the sighash is the txid, `OP_CHECKSIG` needs 32 bytes and the interpreter knows nothing about transactions, UTXOs, or the chain.
- `utxo.rs`: `Outpoint` → `Coin { output, height, from_coinbase }`. `connect` spends what a transaction names and creates what it pays, returning the `Undo` that `disconnect` needs to put it back exactly — the inverse is written beside the forward operation so the two cannot drift, and M4's reorg is its first caller. The height and coinbase flag are what let a restored coin's maturity be re-checked against a new tip ([ADR-0012](docs/adr/0012-reorg-and-undo-data.md)). In memory for now; [ADR-0013](docs/adr/0013-persistence.md) backs it with the key-value store in M5.
- `validation.rs`: `check_shape` is everything judgeable without looking anything up — at most `MAX_TRANSACTION_SIZE` (100 kB, [ADR-0020](docs/adr/0020-transaction-bounds-and-where-validation-runs.md)), version 1, at least one input and output, `coinbase_data` empty off a coinbase and between 4 and 100 bytes on one, no outpoint spent twice, outputs summing within `MAX_MONEY`. The size rule is first because everything after it costs a signature verification per input. `check_spend` adds the rules that need the UTXO set and **returns the fee**, which is how a caller knows the sums were actually checked. `check_coinbase` is the other half: the height at the front of `coinbase_data` must be the block's, and the outputs may claim at most `subsidy(height) + fees`. Claiming less is legal and burns the difference. Both are total: there is no field a node parses and ignores ([ADR-0011](docs/adr/0011-network-identity-and-fields.md)).
- `mempool.rs`: validated transactions by txid, plus the outpoints they claim, so a conflict is a lookup rather than a scan. It refuses a coinbase, a duplicate, a conflict, and anything past `MAX_MEMPOOL`. `admissible` is the cheap half — already held, past the bound, conflicting — and runs **before** validation, because a peer that can make us verify signatures for free has found a way to spend our CPU. A peer's transaction is validated with **no lock held** and then `admit`ted, which re-checks that every coin it was validated against is still there and unchanged, and spendable at the height the chain is at *then* rather than the one it was verified against — a reorg lowers the tip ([ADR-0020](docs/adr/0020-transaction-bounds-and-where-validation-runs.md)). A **block's** validation still runs under the lock; the ADR says why and it is tracked separately — a peer relaying nothing but valid transactions must not exhaust memory, the same rule `MAX_PEERS` and `OUTBOUND_QUEUE` follow.
- `difficulty.rs`: `required_bits` recomputes the target **every block** from a moving 60-block window, clamped to a factor of 2 per block, and never past the network's `starting_bits` floor. The target interval is the network's — thirty seconds on mainnet, one on the test network, where a test has to finish. The intermediate multiply is in `U512` because the test network's starting target is near the top of the 256-bit range. `check_timestamp` enforces median-time-past over the previous 11 and a five-minute future limit — its error tells the operator to suspect their own clock first, because a wrong clock presents as an unexplained partition ([ADR-0009](docs/adr/0009-difficulty-and-timestamps.md)).
- `blockchain.rs`: `Chain` is what the node has actually applied — a tip, the bodies behind it, and the undo record of what each consumed. `accept` validates before anything moves and rolls back if application and validation ever disagree; a refused block's hash is recorded so its branch is not walked again, except where the hash does not identify the body it refused (`block::SharedHash` — a duplicated wtxid, or a body that does not match the claimed root), in which case the **body** is remembered instead; `disconnect` restores the set and returns the block's payments to the mempool, keeping only those still valid. Applied blocks and their undo records are durable ([ADR-0013](docs/adr/0013-persistence.md)); a body on a branch that never won is in memory only, and is asked for again after a restart. `BlockIndex` underneath maps a `BlockHash` to what the node knows about that block — header, height, **cumulative work**, parent. Work is `2^256 / (target + 1)` summed over the branch; height is not a proxy for it once difficulty varies per block, so selecting by height would simply be wrong. More than one tip is normal, not an error: two miners racing is the case reorg exists for. An equal-work tip does not displace one already held, so a peer cannot take the tip by being loud. A header whose parent is unknown is **refused** rather than rooted — holding an orphan is the caller's decision, not the index's.
- `miner.rs`: a thread behind `--mine`. It takes the node lock to snapshot the tip and the mempool, releases it, and grinds without it — then re-acquires only to submit. It hashes in bursts with sleeps, because the public node runs on very little compute and difficulty adapts to whatever hashrate results; between bursts it checks whether the tip moved and abandons a candidate that is now on the wrong chain. It also **waits for the clock**: a chain that runs faster than wall time accumulates future timestamps until every other node refuses it.
- `block_storage.rs`: `RecordFile` is an append-only file of records framed by the network magic and a `u32` length, addressed by the offset `append` returns; `BlockFiles` is the pair (`blocks.dat`, `undo.dat`) a node keeps. Opening truncates to the last record that reads back whole — but only when what it throws away is at most one record's worth, since that is all a crash can leave; anything further back is corruption and is refused by name rather than erased. A length past `MAX_RECORD` ends the readable region **before** anything is allocated for it — the rule `ByteReader::read_count` applies to a count a stranger sends. The format is written down in [docs/on-disk-format.md](docs/on-disk-format.md), so a file can be decoded without reading this code.
- `api.rs`: HTTP/1.1 is **hand-rolled** here, which is a reversal recorded in the [dependency posture](docs/ARCHITECTURE.md#dependency-posture): `tiny_http` bounded nothing a stranger drives — no read timeout, an uncapped request-line read, a thread per connection, and an accept error that killed the listener in silence. One accept thread, a fixed pool of `WORKERS`, and a bounded queue in front of it; a connection past the queue is answered `503` and closed, because a queue that grows is a queue a stranger fills. Every read is capped (`MAX_HEAD`, `MAX_BODY`) and timed (`PATIENCE`), and an accept error is logged rather than fatal. `route` takes the node lock, copies what it needs and **gives it back before returning**; everything after it writes to a socket a stranger controls the read end of, which is the same rule `record` follows for stdout. A handler's panic becomes a 500 and the worker takes the next request. `main` binds the port so a taken one fails the process. Shapes are documented in [docs/api.md](docs/api.md) because M7's scenarios read them. **Encoding happens at this edge and nowhere else** (invariant 5): a hash is reversed to big-endian here, an `Amount` becomes an AVI string here, and a request carrying a display-order hash reverses it back before looking anything up. A block's body may be on disk, so `described` copies what it needs under the lock and reads `blocks.dat` with the lock let go. `/address` answers from the **UTXO set**, never a walk over blocks — the set is what "unspent" means; `/peers` reports where a peer *listens*, never the ephemeral source port an accepted connection came from; `/log` finally gives the bounded `Log` the reader M1 built it for, and its `next` counts lines the log has since dropped so a caller falling behind gets the oldest still held rather than the wrong ones. Every collection is capped — `MAX_LISTED` for the lists, `MAX_PAGE` for `/blocks`, `MAX_SCANNED` for how far `/tx` looks back — and every `count` is of the whole thing rather than of the page. The scans (`/address` over the UTXO set, `/mempool` over its entries, `/tx` over blocks) clone only what they keep: handing a whole collection out and filtering afterwards copies all of it *under the node lock*. The two write endpoints are **not** privileged routes: `POST /tx` calls `protocol::accept_transaction`, the same function a peer's `tx` message reaches, and `POST /connect` calls `protocol::dial_requested`, the same path a configured peer takes — a rule enforced for a stranger and not for a `POST` would be a rule with a hole in it. Nothing in `api.rs` reads the wallet key or signs. The **viewer** — `src/viewer/`, served at `/`, `/viewer.css` and `/viewer.js` — is compiled in with `include_str!`, so the deployment is one artefact and the bytes served are the files in the repo. It has no build step and makes no request outside the origin, and it renders what the API encoded rather than encoding anything itself; there are tests that grep for both — tripwires against the spelling anybody would reach for, not proofs. A **`POST` whose `Origin` is not this node's is refused**: a cross-origin `fetch` with a simple body reaches a write endpoint with no preflight, and the side effect is the whole attack.
- `persist.rs`: `Storage` is the order things reach disk, and that order is the whole of the type. Applying a block writes its bytes to `blocks.dat` and its undo record to `undo.dat`, **flushes both**, then makes one `redb` commit carrying the index entry, the coins and the best-block marker — so every crash window leaves the old state or the new one, never half a block. A disconnect writes no files and therefore **commits first and moves the set after**. Headers are one commit per batch — a peer's `headers` carries up to two thousand and the caller holds the node lock — with no offsets and no marker, which is why the marker ordinarily sits *behind* the index's best tip: headers arrive ahead of bodies. `Chain::open` loads the index, the set and the marker; `catch_up` then connects forward along the best chain as far as the bodies on disk allow, through the same `switch_to` a running node uses. The mempool is deliberately not persisted. `Chain`'s `bodies`/`undo` maps hold only what has **not** been applied once there is storage — an applied block is dropped from memory, and `Chain::body` falls back to `blocks.dat`. A disk read is deliberately not cached: `getdata` reaches `body`, so keeping what it returns would let a peer walking the chain fill our memory with blocks we already have on disk.
- `store.rs`: three `redb` tables — `headers` (hash → header plus its `blocks.dat` and `undo.dat` offsets), `coins` (outpoint → `Coin`), and `markers` (`best_block`). A `Batch` is one redb write transaction, so everything one block changes commits together or not at all; a dropped batch leaves nothing. `Store::headers`/`coins` are the whole-table reads `Chain::open` rebuilds from — `BlockIndex::restored` walks from genesis so a parent is never seen after its child, breaking an equal-work tie by hash because a restart cannot remember arrival order, and `UtxoSet::restored` takes the set whole. **The set stays in memory**; redb is its durable mirror, not its backing store. Loading, not replaying: the cost is the size of the set, not the height of the chain. The codecs are in [docs/on-disk-format.md](docs/on-disk-format.md).
- `data_dir.rs`: `DataDir::open` creates the per-node directory (`0700` on unix), refuses one anybody else can write to — that is somewhere they can leave their own wallet key — takes an advisory lock on it, and stamps it with the network's name and genesis hash — refusing a directory another network built, the ADR-0007 separation applied to disk. The lock is taken **before** the stamp is read, so the check and the write that follows are one operation; two nodes starting together on a fresh directory would otherwise both find no stamp. `main` opens it **before binding the listener**. It holds `blocks.dat`, `undo.dat`, `chain.redb`, `wallet.key`, the `network` stamp and the `lock` — all of them in [docs/on-disk-format.md](docs/on-disk-format.md).

## Comments

**Write code that doesn't need them.** Default to none. No doc comments restating a signature, no inline narration of what the next line does — the name and the shape carry it. When a comment feels necessary, rename or restructure first; the comment is usually a hint that something is unclear.

Two things are not commentary and stay:

- **Functional doc comments** — clap `///` on argument fields becomes `--help` text, so it is output, not prose.
- **A short note where the code is correct for a reason not visible in it** — a workaround, a deliberate deviation, an ordering requirement someone would otherwise "tidy" away. Say *why*, never *what*, and keep it to a line.

Design reasoning belongs in an [ADR](docs/adr/), behaviour in a test, and how-it-works in `docs/ARCHITECTURE.md` or this file — not beside the code, where it goes stale unnoticed.

## Testing conventions

There are two suites, and the split is by **what has to be running**, not by how much is covered.

- **Rust unit tests** live inline as `#[cfg(test)] mod tests` at the bottom of each source file. Cargo's `tests/` directory is deliberately unused, so it can never collide with `test/`. Parameterized cases use `rstest` (`#[rstest]` + `#[case(...)]`). Round-trip serialize→parse tests are the standard pattern for any new wire/serialization format.
- **Python functional tests** live in `test/functional/` and need a *running node*: they launch the binary and speak the protocol to it over a socket. See [ADR-0014](docs/adr/0014-functional-test-suite.md).

Anything provable without spawning a process belongs inline, in Rust. Reach for `test/functional/` when the guarantee is about the program — that it binds what it was told to, refuses a malformed config, survives a hostile peer — rather than about a function.

Prefer the highest existing seam over a new one. The connection path is tested through `std::io::Read`/`Write` and the outbound channel, with in-memory buffers rather than sockets — see `protocol.rs`'s `an_inbound_ping_is_answered_with_a_pong_on_the_outbound_channel`. Pure logic like config layering is tested directly. Tests are written with the change they cover, not retrofitted.

A loopback `TcpListener` is fair game only where the socket wiring *is* the guarantee — that two threads share one connection, that a dropped write half wakes a parked reader. Reach for it when an in-memory seam would assert on a mock of the thing under test; not otherwise.

### Rules the functional suite lives by

These come from [ADR-0014](docs/adr/0014-functional-test-suite.md) and are not negotiable per-test:

- **Assert on bytes, never on log lines.** `framework/p2p.py` frames, checksums and parses; `framework/http.py` speaks HTTP to the API. ADR-0014's one exception — a test allowed to read stdout because no other surface existed — is gone: it observes through `GET /peers` now. Stdout is still read where it is the only surface, and the ADR lists exactly where: to learn a port or that the API is up, for a refusal made *before* the node serves anything, and for a dial that failed. Everything a running node can be **asked** is asked.
- **`framework/messages.py` never imports the node's encoder.** It is a second implementation on purpose; a test that reuses the encoder cannot catch a bug symmetric across encode and decode. If the two disagree, that is the suite working.
- **Every wait is bounded.** A hanging test is worse than a failing one: it takes the suite with it. Sockets, `accept`, process exit and log scanning each carry their own deadline, and a per-item timeout is not enough when the node keeps producing something else. `PATIENCE` bounds what should happen; `IMPATIENCE` what should not.
- **Coverage is proven by mutation, not by a green run.** Revert the guarantee, confirm something goes red, and check the mutation actually applied before believing the result.

pytest runs serially, so `PATIENCE` is paid once per failing test. It is 8s, not 20s, for that reason. If the suite grows enough for this to hurt, `pytest-xdist` is the lever — the tests already use ephemeral ports and private sandboxes, so they are parallel-safe.
