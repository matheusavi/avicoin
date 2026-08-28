# ADR-0001 — v1 scope: a trimmed, demoable spine

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

> Read this first. It bounds every other decision in this directory.

## Context

Avi Coin's real objective is **portfolio and interview signal**, not a shipped
product: a working, externally verifiable demo; clean idiomatic Rust a reader can
absorb in fifteen minutes; and a handful of genuinely hard problems with
articulable trade-offs.

The previous plan laid out nine phases ending in full consensus, reorg handling,
difficulty retarget, persistence, a full-screen terminal UI, an HTTP API, a web
viewer, a deployment, and a comprehensive e2e suite. It was a good map of the
*subject*, but a poor plan for the *objective*, for three reasons:

1. **A finished small thing beats an unfinished large one.** For the stated
   objective, "half of nine phases" is strictly worse than "all of five" — an
   interviewer cannot run a half-built node.
2. **The tail phases looked like the least signal per unit of work** — a terminal
   UI duplicates a surface the web viewer already provides. *(This judgement was
   right about the UI and wrong about difficulty retarget and reorg, for reasons
   that only became visible once the target was a public node. See "How scope
   decisions get made here".)*
3. **Nothing was externally verifiable until the very last phases.** The one
   feature that makes the project *checkable by someone else* — a public URL
   showing a live chain — sat behind everything else.

The competing option was to abandon the own-coin path entirely for a byte-exact
Bitcoin light wallet on signet, broadcasting a real transaction verifiable on a
public block explorer. That is a tighter and more differentiated demo, but it
strands the ~1,600 lines of framing, parsing, block, and mining code already
written, and discards the concurrency work that carries most of the Rust signal.

## Options considered

### Option A — Trimmed own-coin spine *(chosen)*
Carry the own-coin path through peer handshake, transactions, mining, and block
relay, then go straight to the HTTP API, web viewer, and a public deployment.
Defer difficulty retarget, reorg, persistence, and the terminal UI. *(Three of
those four deferrals were subsequently reversed — the milestone table below is the
current boundary, not this sentence.)*

- **+** Preserves everything already built.
- **+** Reaches an externally verifiable state — a public URL with a growing
  chain — in roughly half the work of the nine-phase plan.
- **+** Keeps the highest-signal problems: threaded peer I/O, bounds-checked
  binary parsing, the UTXO model, Script, sighash design, PoW.
- **−** The result is explicitly a *simplified* coin. A reader who probes reorg
  or retarget finds a documented gap rather than an implementation.

### Option B — The full nine-phase plan
- **+** The complete learning arc; nothing documented as missing.
- **−** Largest surface, latest verifiability, highest risk of stalling
  permanently half-done — the specific failure the objective cannot afford.

### Option C — Pivot to a Bitcoin signet light wallet
- **+** Tightest scope; genuinely external verification (a real txid on a real
  explorer); differentiates from the crowded toy-blockchain genre.
- **−** Strands the existing block/mining/framing code and most of the
  concurrency work. A different project wearing this one's name.

### Option D — Option A now, Option C as a second act
- **+** Strictly the most total value.
- **−** Two demos' worth of work; re-creates the over-scoping this ADR exists to
  correct. Better revisited once A has actually shipped.

## Decision

**Option A.** v1 is a trimmed spine, delivered as seven milestones:

| # | Milestone | What it delivers |
|---|---|---|
| M1 | Node foundations | `SharedNode`; per-peer reader + writer threads; peer registry; `broadcast()`; a logging ring buffer replacing scattered `println!`. |
| M2 | Peer handshake & discovery | `version` / `verack` / `getaddr` / `addr`; per-peer handshake state machine; only `Ready` peers are relayed to; dedup, self-connect guard, reconnect. |
| M3 | Transactions end-to-end (send-only) | `k256` swap; the Script interpreter and the P2PKH template; witness-separated transactions; sighash; Base58Check addresses at the display edge; UTXO set; input selection, change, fee; mempool validation; `inv` / `getdata` / `tx` relay; genesis allocation so wallets are funded before mining exists. |
| M4 | Mining, consensus & block relay | Block index and best tip; coinbase; throttled miner thread behind `--mine`; `block` relay and headers sync; **per-block difficulty retarget** and timestamp rules; **reorg by cumulative work, with undo data**; block-acceptance rules (PoW ≤ target, merkle root matches, exactly one coinbase, inputs exist and unspent, `sum(in) ≥ sum(out)`, scripts validate, correct subsidy). |
| M5 | Persistence | Append-only `blocks.dat` and `undo.dat`; an embedded key-value store for the block index and UTXO set; a per-node data directory; startup that loads rather than replays, with crash recovery from a best-block marker; the wallet key on disk. |
| M6 | HTTP API & web viewer | Read endpoints plus `POST /tx` and `POST /connect`; a static HTML/JS block explorer polling them. Doubles as the e2e control surface. |
| M7 | Deploy & multi-node e2e | Dockerfile and compose for a local multi-node network; one public node behind a URL; **scenario** cases — fork convergence, crash recovery — added to the functional suite of [ADR-0014](0014-functional-test-suite.md), which exists and gates CI from M1 rather than starting here; CI gains `fmt` and `clippy -D warnings`. |

Milestones live on GitHub and are the unit of planning. Each gets a **spec**
(problem, user stories, implementation and testing decisions) as a GitHub issue;
tickets are cut from a spec when it is picked up, not in advance.

**Explicitly deferred:** the terminal UI, Script beyond the opcode set fixed in
[ADR-0002](0002-output-locking-model.md), sighash types other than ALL, timelocks
and replace-by-fee ([ADR-0011](0011-network-identity-and-fields.md) deletes the
fields they would use), block pruning, and any network performance work. Each is a
candidate second act.

*(Difficulty retarget, reorg, and persistence were originally on this list. See
"How scope decisions get made here" below — all three were restored, and the
reason they had to be is the most useful thing this ADR records.)*

## How scope decisions get made here

Script was originally on that deferred list, on the grounds that *"every output
uses one template; an interpreter with a single opcode path is cost without
signal."* That reasoning was wrong, and the way it was wrong is the useful part.

It measured the **feature** (one template, so the generality goes unused) instead
of the **learning** (an output specifies a *predicate*, not an identity — the
single most distinctive idea in Bitcoin's design). This ADR's criterion is signal
per unit of work, and by that criterion a bounded interpreter scores high: a
stack, roughly twelve opcodes, explicit resource limits, and one template, all
testable in isolation.

It also would have produced a worse artifact. With a hardcoded pubkey-hash
comparison, a "VM" is ceremony around a fixed check. With a slightly wider opcode
set a *second* script becomes expressible — a hash-preimage lock — which is the
difference between having an interpreter and appearing to have one.

Three further deferrals were then reversed for a different and stronger reason:
they didn't cost learning, they **broke the demo**.

- **Difficulty retarget** ([ADR-0009](0009-difficulty-and-timestamps.md)). A fixed
  `n_bits` is fine for a controlled local run. On a public node anyone can join,
  the first visitor with an ordinary laptop collapses block time — and when they
  disconnect, a windowed scheme can take weeks to recover. The chain dies.
- **Reorg** ([ADR-0012](0012-reorg-and-undo-data.md)). Without it, any second
  miner permanently splits the network — tens of times a week at 30s blocks.
  "Anyone can join and the network adjusts" becomes "anyone can join as long as
  they don't mine," which nothing enforces.
- **Persistence** ([ADR-0013](0013-persistence.md)). Resyncing from peers is no
  fallback for a *single* public node. Every host-initiated restart returns the
  explorer to height zero.

Each was deferred under an unstated assumption — a short-lived demo on a closed
network — that the actual goal contradicts. The deferrals were wrong on **facts**,
not on values.

**So the test has three legs: does the cut remove work, remove learning, or break
the demo?** Cuts that remove work are cheap. Cuts that remove learning are what
this project exists to avoid. Cuts that break the demo are not cuts at all — they
are the deliverable being quietly removed, and they are the hardest to notice,
because each one looks locally reasonable.

Two decisions taken alongside these reversals *reduced* net work:
[ADR-0004](0004-sighash.md) collapsed the sighash to the txid, removing a design
problem entirely, and putting the merkle root over wtxids
([ADR-0003](0003-transaction-witness-format.md)) removed any need for a coinbase
witness commitment.

## v1, as delivered

*2026-08-28.*

Every ticket in every milestone is closed but one — the public URL, which needs
a host rather than a change to this repository. What shipped against what this
ADR scoped:

| | Scoped | Shipped |
|---|---|---|
| M1 | A node that listens, dials, handshakes and pings | as scoped |
| M2 | Peer table, identity by nonce, discovery, reserved outbound slots | as scoped |
| M3 | Transactions end-to-end, send-only | as scoped; the `send` subcommand the code was missing a caller for came later, in M7 (#139) |
| M4 | Mining, per-block retarget, reorg, block relay, headers-first sync | as scoped |
| M5 | `blocks.dat`/`undo.dat`, `redb`, a data directory, load-not-replay, the key on disk | as scoped, **plus** moving block validation off the node lock (#115) |
| M6 | HTTP/JSON API and a web viewer | as scoped, with `tiny_http` **taken back out** — see below |
| M7 | Container, compose, scenario suite, `fmt`/`clippy` gates, docs | as scoped, **less the public URL**, which is a deployment rather than a change |

Three things came out differently, and each is recorded where a reader meets
it rather than only here:

- **`tiny_http` did not survive review.** It bounded nothing a stranger drives:
  no read timeout, a request line read into a `Vec` with no cap, a thread per
  connection, and an accept error that dropped the listener in silence. HTTP/1.1
  is hand-rolled in `api.rs` instead. The [dependency
  posture](../ARCHITECTURE.md#dependency-posture) carries the reversal.
- **Validation is not minimum-viable in one respect it was scoped to be.** Both
  the transaction path and the tip-extension block path check signatures with
  the node lock released ([ADR-0020](0020-transaction-bounds-and-where-validation-runs.md)).
  That was not scope creep for its own sake — a stranger holding the node still
  for a block's worth of curve arithmetic is the demo breaking, which is this
  ADR's own test for restoring a cut.
- **The public URL is the one deliverable not delivered.** Everything it needs
  is in `deploy/`; what is missing is a host, which is not something a change to
  this repository can supply. #127 says so plainly rather than being closed.

The deferred list below is unchanged, and is now what it always claimed to be:
**second acts, not gaps.**

## Consequences

- The old `docs/ROADMAP.md` is deleted. Its architecture content moved to
  `docs/ARCHITECTURE.md`; its phase plan became the milestones above.
- **Validation is minimum-viable, not complete.** M4's rules make the chain
  honest; they do not make it attack-resistant against a determined adversary
  with real hashrate. This is a documented boundary, and the README already
  disclaims real-world use.
- **v1 grew.** Six milestones became seven, and M4 absorbed two subsystems. That
  is a real cost, taken deliberately: the alternative was a demo that breaks when
  used the way it invites people to use it.
- **The public node runs on very little compute**, so the miner hashes in bursts
  with sleeps rather than saturating a core. Difficulty adapts to whatever
  hashrate results, so throttling is a runtime knob, not a consensus concern.
- Milestones M1 → M5 are strictly sequential. M6 can begin as soon as M1's shared
  state exists; M7 needs M6's API and M5's durability.
