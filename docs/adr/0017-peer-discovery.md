# ADR-0017 — Discovery needs to push, not only pull

- **Status:** Accepted
- **Date:** 2026-08-08
- **Deciders:** @matheusavi

## Context

A node should find peers it was never told about. The obvious mechanism is
Bitcoin's: ask a peer for addresses with `getaddr`, answer one with `addr`, dial
what comes back.

M2's exit criterion is three nodes with partial seed lists converging on the full
mesh: A and C each know nothing, B knows both. Pull alone does not get there, and
the reason is a race rather than a missing feature.

## Decision

`getaddr` is sent to a peer as it becomes **Ready**, and answered with the
**listening** addresses of other Ready peers — never `PeerHandle.address`, which
on anything we accepted is an ephemeral source port nobody can dial back.

**And** a peer becoming Ready is announced to the others: a one-address `addr`,
relayed to every Ready peer except the one it is about.

An `addr` is dialled from, skipping our own address, peers we already hold, and
anything once the table is full.

## Why pull alone does not converge

B dials A, then dials C. A completes its handshake with B and immediately asks
`getaddr` — at which point B has not finished meeting C, so B answers with
nothing. Nothing asks again. A never learns of C, and the mesh is two edges
short of the criterion.

The pull is triggered by *our* handshake completing; the fact we want is created
by *someone else's*. No amount of ordering fixes that, because B's two handshakes
are genuinely concurrent.

Two ways out were available:

- **Ask periodically.** Convergence then takes a retry interval, and every node
  pays a timer forever to catch an event that is rare after startup.
- **Push on the event.** The node that learns something tells the others as it
  learns it. Convergence is immediate, costs one message per new peer, and is
  what Bitcoin does with relayed `addr`.

Push wins on both counts, and it gives `broadcast` its first real caller.

## Consequences

- Discovery converges without a timer, and the exit criterion is a functional
  test rather than a manual check.
- A dial reserves its peer slot **before** connecting. Reserving after would let
  an `addr` full of unroutable addresses buy one thread parked in `connect()` per
  entry — around two minutes each on Linux — while the table stayed empty and
  looked healthy. `MAX_PEERS` now bounds work in flight, not merely work that
  succeeded.
- `addr` is capped at `MAX_ADDRESSES` (256) per message, refused on the count
  before any address is read. The payload cap alone would allow over a million.
- Addresses are not persisted, so discovery starts from the seed list every run.
  An address book that outlives the process is out of v1 scope
  ([ADR-0001](0001-v1-scope.md)).
- Nothing ages or scores addresses. A peer can push the same address repeatedly;
  each one costs a table lookup and is dropped when we already hold it.
