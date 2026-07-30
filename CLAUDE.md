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
cargo build                 # build the node (debug binary at target/debug/avicoin)
cargo run                   # run a node using config.toml
cargo test                  # run all Rust unit tests (inline #[cfg(test)] modules)
cargo test <name>           # run tests matching a substring, e.g. cargo test read_u64
cargo test byte_reader::tests::test_read_u16   # run one specific test by full path
```

Run the node with CLI overrides (these take precedence over `config.toml`):

```bash
cargo run -- --host-address 127.0.0.1:34352 --addresses-to-connect 127.0.0.1:5000 --addresses-to-connect 127.0.0.1:5001
```

CI (`.github/workflows/rust-tests.yml`) runs `cargo test` on pushes/PRs to `main`. **There is no end-to-end suite yet** — [ADR-0001](docs/adr/0001-v1-scope.md) puts it in M7, driving nodes over the HTTP API from M6. Until then, every guarantee is a Rust test.

## Configuration resolution

`configs.rs::get_configs()` layers config in this order, each overriding the previous when non-empty: **built-in defaults → `config.toml` → CLI args (clap)**. The default `config.toml` sets both `host_address` and `addresses_to_connect` to the same loopback address so a single node connects to *itself*, which is what drives the ping/pong exchange the e2e test asserts on.

## Architecture

The node is a small P2P server modeled on Bitcoin's message framing. `main.rs` spawns one listener thread (`protocol::listen`) plus one outbound thread per configured peer (`protocol::connect`); both funnel into `handle_connection`, which is a blocking per-connection loop (`protocol.rs`) that:
- sends a `Ping` every `PING_INTERVAL` (11s), first ping fires immediately,
- reads with a 5s read timeout, appending bytes to a growing `recv_buffer`,
- drains complete messages out of `recv_buffer` and replies to each (Ping → Pong).

**Message framing (`src/messages/`)** is the core of the wire protocol:
- `Message<T>` = `Header` (24 bytes) + typed `payload: T`. The header is 4 magic bytes (`0xf9beb4d9`), a 12-byte command name, a 4-byte little-endian payload size, and a 4-byte checksum (first 4 bytes of the double-SHA256 of the payload).
- Any payload type implements the `Payload` trait (`get_raw_format`, `get_command_name`); adding a new message type means adding a `Payload` impl and a new variant + command-name arm in `MessageReceived` (`message.rs`).
- `MessageReceived::try_parse_message` is designed for streamed TCP data: it returns `(None, 0)` when the buffer holds only a partial message (so the caller keeps reading), and otherwise returns the parsed message and the number of bytes consumed. It validates magic bytes, enforces `MAX_PAYLOAD_SIZE` (32 MiB), and verifies the checksum before dispatching by command name.

**Serialization conventions** (shared by messages, blocks, transactions):
- All multi-byte integers are little-endian on the wire; hashes are computed little-endian and only reversed to big-endian for display.
- Variable-length counts use Bitcoin compact-size encoding (`util::get_compact_int` / `ByteReader::read_compact`).
- Hashing everywhere is **double SHA-256** via `util::get_hash` (Bitcoin's HASH256).
- `ByteReader` (`byte_reader.rs`) is the single bounds-checked cursor used for all deserialization; prefer it over manual slicing. All reads return `anyhow::Result`.

**Domain model (not yet wired into the network layer):**
- `block.rs`: `Block::mine()` builds the 80-byte header into `mine_array`, computes the merkle root of its transactions, derives the target from compact `n_bits`, and brute-forces the nonce until double-SHA256(header) < target.
- `transaction.rs`: `Transaction`/`TxIn`/`TxOut`/`Outpoint` with serialize/parse; `get_tx_id()` is the double-SHA256 of the raw format.
- `wallet.rs`: `Wallet` holds a secp256k1 keypair; `send()` builds and signs a transaction but UTXO selection, balance, and change are stubbed TODOs.
- `block_storage.rs` is an empty stub.

## Testing conventions

Rust tests live inline as `#[cfg(test)] mod tests` at the bottom of each source file; there is no separate `tests/` directory. Parameterized cases use `rstest` (`#[rstest]` + `#[case(...)]`). Round-trip serialize→parse tests are the standard pattern for any new wire/serialization format.

Prefer the highest existing seam over a new one. The connection path is tested through `std::io::Read`/`Write` with in-memory buffers rather than sockets (see `protocol.rs`'s `receive_ping_send_pong`), and pure logic like config layering is tested directly. Tests are written with the change they cover, not retrofitted.
