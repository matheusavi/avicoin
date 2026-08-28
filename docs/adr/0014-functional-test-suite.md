# ADR-0014 — Functional test suite: Python, driving real binaries

- **Status:** Accepted
- **Date:** 2026-07-31
- **Deciders:** @matheusavi

## Context

Everything the project guarantees is currently a Rust unit test. Those cover
parsing, framing and config layering at in-memory seams, but nothing has ever
checked that the built binary *starts*, binds what it was told to bind, speaks
the protocol on a real socket, or fails properly when its configuration is
wrong. `protocol.rs` is tested through `Read`/`Write` with in-memory buffers;
`config.rs::resolve` is tested as pure logic. Neither runs the program.

[ADR-0001](0001-v1-scope.md) put the first end-to-end suite in **M7**, described
as "pytest driving nodes over the API", after M6 provides an HTTP surface to
drive. Two things forced the question earlier than that.

**There was a Python suite, and it rotted.** Commit `d225d6d` added
`tests/integration/test_ping_pong.py`: 203 lines of framework for a single test.
It spawned two nodes correctly — A listening, B dialling A — so the topology was
never the problem. Two other things were:

- it asserted on **stdout strings** (`wait_for_log("Ping received")`), which
  cannot express the properties that matter: that a checksum covers its payload,
  that a pong carries the nonce of the ping that provoked it, that two messages
  in one read are answered in order;
- **nothing ran it.** It invokes `--listen` and `--connect`. Those flags were
  renamed to `--host-address` and `--addresses-to-connect`, and the suite went on
  pointing at a binary that no longer existed. No signal ever fired, because
  there was no CI job and `cargo test` does not know Python exists.

The second point is the real lesson, and it is about wiring rather than
language.

**Waiting for M6 costs more than it saves.** A harness that speaks the wire
protocol has to be written eventually. Written now, it covers one message type
with an 8-byte payload. Written at M7, it covers blocks, transactions, witness
serialization and a dozen message types at once.

## Options considered

### Option A — Rust integration tests under `tests/`

`cargo test` discovers `tests/*.rs`, and `CARGO_BIN_EXE_avicoin` points at the
built binary, so spawning processes and talking to them needs no dependency at
all. This was built and works: twenty tests, all nine behaviour mutations
caught.

Its decisive advantage is that it runs by default. The failure mode that killed
`d225d6d` — a suite that silently stops matching the binary — cannot happen to a
suite that runs whenever anyone types `cargo test`.

Its weaknesses are narrower than they first appear. It gives almost no
compile-time safety here: the binary is spawned with string arguments and the
wire format is hand-rolled inside the test, so a renamed flag breaks it at run
time exactly as it would break Python. And a hand-rolled copy of the wire format
living in the same crate is a weaker independence claim than a second
implementation in a different language — nothing but discipline stops a future
edit from importing the real encoder and quietly making the test tautological.

### Option B — Python, driving real binaries, byte-level

A `test/functional/` suite that spawns the binary and acts as a real peer:
framing, checksums and parsing implemented in Python, so assertions are on bytes
rather than log lines. This is the shape Bitcoin Core uses — its functional
tests drive real daemons and carry a test-side implementation of the P2P
protocol precisely so they do not have to scrape logs.

It buys genuine implementation independence: the Python encoder cannot
accidentally share code with the Rust one, so a bug symmetric across encode and
decode is caught rather than mirrored.

It costs a second toolchain, and it re-adopts the arrangement that already
rotted once in this repository. That cost is only acceptable if the rot is fixed
at the same time, which is what the decision below turns on.

### Option C — Two suites

Rust for protocol and process conformance, Python for scenario tests at M7.
Rejected: the split is in *what is asserted*, not in how nodes are driven.
Spawn, wait for ready, talk, tear down is identical for both, so this buys two
copies of one harness. Bitcoin Core has one functional suite, not two.

## Decision

**One functional suite, in Python, under `test/functional/`, asserting on bytes.**
It grows into M7 rather than being replaced there.

Option A is the safer engineering choice and was recommended. It lost on two
counts that Option B wins outright: an independent implementation of the wire
format is a stronger conformance guarantee than a copy in the same crate, and
the harness is far cheaper to build now, against one message type, than at M7
against the whole protocol.

The rot that killed `d225d6d` is not accepted as a cost. It is fixed by rules
that are part of this decision, not aspirations attached to it:

1. **A blocking CI job ships in the same pull request as the tests.** Never
   later. A suite in a second toolchain with no gate is a suite that is already
   dead; the only thing that would have caught `--listen` → `--host-address` is
   a job that fails the build.

   *Blocking* means enforced, not intended. `main` carries branch protection
   requiring both **Unit tests** and **Functional tests**, applying to
   administrators, with changes required to go through a pull request and force
   pushes disabled. Without `enforce_admins`, the rule would be decorative for
   the only person who can merge. The cost is accepted: if CI is unavailable,
   nothing lands until protection is lifted.
2. **Assertions are on bytes, never on log lines.** `framework/p2p.py` frames,
   checksums and parses; `framework/http.py` speaks HTTP to the API.

   *2026-08-27, in M6.* This used to carry an exception — one test, two real
   nodes completing a round trip, allowed to read stdout because no other
   surface existed. There is one now: each node reports the other as `ready`
   through `GET /peers`, which says the same thing and says it in bytes. The
   exception is gone rather than grandfathered.

   Stdout is still read where it is the **only** surface, and that is a
   shorter list than it was, not an empty one:

   - to *learn* something before anything can be asked — the port a node bound
     after `:0`, or that its API is up;
   - for a refusal the node makes **before it serves anything**: a data
     directory another network built, a key file anyone can read, an address
     that will not parse. There is no API on a process that exited, and the
     exit code alone does not say which rule stopped it;
   - for a dial that failed. A node that could not reach a peer has nothing to
     show a peer, and `/peers` cannot report an absence with a reason.

   Everything a running node can be *asked* is asked. The rule is that a log
   line is not an assertion where a byte would do — not that stdout is never
   read.
3. **Every wait is bounded.** A hanging test is worse than a failing one: it
   takes the suite with it. Sockets, `accept`, process exit and log scanning
   each carry their own deadline, and a per-item timeout is not sufficient when
   the node keeps producing something else.
4. **Behaviour coverage is proven by mutation, not by a green run.** A test that
   asserts nothing passes. The suite is only complete when reverting each
   guarantee turns something red, and each mutation is verified to have actually
   applied before its result counts.

## Consequences

**`cargo test` stops meaning "run the tests."** This is the real price, paid
knowingly. `cargo test` covers the unit tests; the functional suite needs
`pytest`. CI runs both, and the CI job is what makes that safe.

**A second toolchain enters the repo** — Python 3.12, a `.venv`, a pinned
`requirements.txt`, and a committed `.envrc` for direnv. `pytest` is the only
third-party package; framing needs `hashlib` and `struct`, and the M6 HTTP
client will need `urllib.request` and `json`, all standard library. The
[dependency posture](../ARCHITECTURE.md#dependency-posture) governs crates and is
unaffected.

**M7 shrinks.** Its suite is no longer written from nothing; it adds scenario
cases — fork convergence, crash recovery — to a harness that already exists and
already runs in CI. ADR-0001's M7 row is amended to cite this record.

**`test/` is singular and separate from `tests/`.** Cargo auto-discovers
`tests/*.rs` as integration targets. Keeping the Python suite out of that
directory means the two toolchains can never collide over one root.

**CI hardening stays partly deferred.** Only the functional-test job is added
now. `cargo fmt --check` and `clippy -D warnings` remain M7 work — the latter
would fail today on thirteen dead-code warnings from modules deliberately not
yet wired into the network layer.

**What this rules out:** importing the node's own encoder into the suite. The
Python implementation of the wire format is deliberate duplication, and it is
the thing being tested against. If the two disagree, that is the suite doing its
job, not drift to be tidied away.
