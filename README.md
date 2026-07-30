# Avi Coin

A Bitcoin-like cryptocurrency built from scratch in Rust, **without referencing
Bitcoin's source code** — guided only by public documentation such as
[bitcoin.org](https://bitcoin.org/). It reimplements the wire protocol, block and
transaction serialization, proof-of-work mining, and wallet signing.

A personal project with two goals: getting better at Rust, and understanding
Bitcoin by rebuilding it.

## Status

Pre-v1. Today the node is a P2P server that frames and exchanges messages
(ping/pong) over Bitcoin-style headers; block mining, transactions, and the
wallet exist as modules but are not yet wired into the network layer.

v1 is a deliberately scoped but *complete* coin, delivered as seven milestones —
see **[ADR-0001](docs/adr/0001-v1-scope.md)** for what's in, what's out, and why.

| # | Milestone | |
|---|---|---|
| M1 | Node foundations — shared state, per-peer reader/writer threads | ☐ |
| M2 | Peer handshake & discovery — `version` / `verack` / `addr` | ☐ |
| M3 | Transactions end-to-end — Script VM, addresses, UTXO set, mempool, relay | ☐ |
| M4 | Mining, consensus & block relay — coinbase, retarget, reorg | ☐ |
| M5 | Persistence — block files, undo data, crash recovery | ☐ |
| M6 | HTTP API & web block explorer | ☐ |
| M7 | Deploy & multi-node end-to-end tests | ☐ |

Progress is tracked in
[GitHub milestones and issues](https://github.com/matheusavi/avicoin/milestones),
not in this file.

**Deliberately out of scope for v1:** a terminal UI, Script beyond the shipped
opcode set, sighash types other than ALL, timelocks, and block pruning. Each is a
documented decision rather than an oversight.

## Design notes

The interesting parts — all reasoned out from public documentation rather than
from Bitcoin's source:

- **Coins are locked by a Script program** evaluated by a small stack VM. An
  output specifies a *predicate*, not an identity.
- **Unlocking data lives in a witness the txid doesn't cover**, so txids can't be
  malleated. The merkle root is then built over witness-*including* hashes, which
  makes Bitcoin's entire SegWit apparatus — script versioning, the coinbase
  witness commitment — unnecessary. Bitcoin needed all of it because SegWit had to
  be a soft fork on a live chain; a new chain doesn't.
- **The sighash is just the txid.** Once the witness sits outside the hashed form,
  the circularity that forces Bitcoin's "blank out the signature slots" dance
  never arises.
- **Difficulty retargets every block** from a moving window rather than in
  2016-block steps — a chain running on very little compute dies if a visiting
  miner leaves mid-window.
- **~2,016,000 AVI**, 50 per block, halving weekly, **no premine**.

Start at [ADR-0002](docs/adr/0002-output-locking-model.md) and read forward.

## Documentation

| | |
|---|---|
| [ADR-0001 — v1 scope](docs/adr/0001-v1-scope.md) | What we're building and why it's this size. Start here. |
| [Architecture](docs/ARCHITECTURE.md) | Target design, concurrency model, invariants, module map. |
| [Decision records](docs/adr/) | Every significant decision, its options, and its consequences. |
| [Glossary](docs/glossary.md) | One term, one meaning — across code, docs, and conversation. |

## Building and running

```bash
cargo build          # debug binary at target/debug/avicoin
cargo run            # run a node using config.toml
cargo test           # unit tests
./e2e_tests.sh       # end-to-end: venv + pip install + build + pytest
```

CLI flags override `config.toml`:

```bash
cargo run -- --host-address 127.0.0.1:34352 \
             --addresses-to-connect 127.0.0.1:5000 \
             --addresses-to-connect 127.0.0.1:5001
```

## Disclaimer

This project is purely for **learning purposes**. It is **not** intended for
real-world use, and it does not match Bitcoin in security or in features.
