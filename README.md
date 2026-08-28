# Avi Coin

A Bitcoin-like cryptocurrency built from scratch in Rust, **without referencing
Bitcoin's source code** — guided only by public documentation such as
[bitcoin.org](https://bitcoin.org/). It reimplements the wire protocol, block and
transaction serialization, proof-of-work mining, and wallet signing.

A personal project with two goals: getting better at Rust, and understanding
Bitcoin by rebuilding it.

## Status

**v1 is built.** All seven milestones are delivered: a node mines, validates and
relays blocks, reorganises onto the heavier chain, keeps a mempool, spends coins
from a wallet, survives a kill at any point, and serves a block explorer over
HTTP. **[ADR-0001](docs/adr/0001-v1-scope.md)** is what v1 meant, and its
[v1, as delivered](docs/adr/0001-v1-scope.md#v1-as-delivered) section is what
came out — including the three things that came out differently.

| # | Milestone | |
|---|---|---|
| M1 | Node foundations — shared state, per-peer reader/writer threads | ✅ |
| M2 | Peer handshake & discovery — `version` / `verack` / `addr` | ✅ |
| M3 | Transactions end-to-end — Script VM, addresses, UTXO set, mempool, relay | ✅ |
| M4 | Mining, consensus & block relay — coinbase, retarget, reorg | ✅ |
| M5 | Persistence — block files, undo data, crash recovery | ✅ |
| M6 | HTTP API & web block explorer | ✅ |
| M7 | Deploy & multi-node end-to-end tests | ✅ |

The one thing v1 asked for and does not have is a **public node** at a name you
can type: everything it needs is in [`deploy/`](deploy/), and what is missing is
a host — see [#127](https://github.com/matheusavi/avicoin/issues/127).

The work itself is tracked in
[GitHub milestones and issues](https://github.com/matheusavi/avicoin/milestones),
not in this file.

Built means built, not production-grade: this is a learning project, the coin
has no value, and the wallet key sits in plaintext on disk. The
[disclaimer](#disclaimer) is not boilerplate.

**Deliberately out of scope for v1:** a terminal UI, Script beyond the shipped
opcode set, sighash types other than ALL, timelocks and RBF, block pruning, and
network performance work. Each is a documented decision rather than an oversight,
and each is a second act rather than a gap.

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
cargo run            # run a node — no configuration required
cargo test           # unit tests
```

With no `config.toml` and no arguments a node listens on `127.0.0.1:34352` with
no peers, which others can dial. `config.toml` is optional and so is every field
in it; CLI flags override both:

```bash
cargo run -- --host-address 127.0.0.1:34352 \
             --addresses-to-connect 127.0.0.1:5000 \
             --addresses-to-connect 127.0.0.1:5001
```

## The live chain

<!-- Replace when the node is deployed; deploy/README.md is the recipe. -->
**Not yet deployed.** Everything it needs is in [`deploy/`](deploy/) — one
`docker compose up -d` on a host with a name — and this line becomes the URL
when somebody with a host runs it.

Until then, `docker compose up` in the repo root gives you the same thing
locally: three nodes, one mining, a viewer on <http://localhost:8080>.

## Running a node

```bash
docker build -t avicoin .
docker run -d -p 34352:34352 -p 8080:8080 -v avicoin-data:/avicoin/data avicoin \
  --data-dir=/avicoin/data --host-address=0.0.0.0:34352 \
  --api-address=0.0.0.0:8080 --mine
```

Then open <http://localhost:8080>. `docker compose up` brings up a small
network of three instead. Without a container, `cargo run -- --api-address
127.0.0.1:8080 --mine` does the same thing.

[docs/deployment.md](docs/deployment.md) has the rest: the volume, the
healthcheck, and what the image deliberately does not carry.
[deploy/](deploy/) is the public node's own recipe.

## Joining a network

```bash
cargo run -- --addresses-to-connect <host>:34352 --api-address 127.0.0.1:8080
```

Your node syncs from theirs; the viewer shows it catching up. Add `--mine` to
mine against their chain — difficulty adapts to whatever hashrate arrives,
which is easier to watch than to be told.

## Spending

```bash
cargo run -- send --to <address> --amount 1.5 --api-address 127.0.0.1:8080
```

The key never leaves the machine it is on: `send` reads it, signs locally, and
hands the node a transaction any stranger could have handed it. There is no
`POST /send`, and there will not be one.

## Disclaimer

This project is purely for **learning purposes**. It is **not** intended for
real-world use, and it does not match Bitcoin in security or in features.

**The wallet's private key is stored in plaintext**, in `wallet.key` inside the
node's data directory, at mode `0600` on Unix. (On other platforms the file
inherits whatever permissions the directory gives it, and the node does not
check them.) That is a deliberate choice, not an
omission: encrypting it would imply a security property nothing else here
provides, and a passphrase prompt would imply a threat model this project does
not have. Anyone who can read the file can spend the coins, and the coins are
not worth anything.
