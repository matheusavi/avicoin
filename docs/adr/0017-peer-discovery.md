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
anything once the table is full. None of it happens before the peer is Ready:
`handle_messages` gates everything that is not the handshake itself, so a
stranger's `addr` cannot have us dialling on its say-so.

Dials in flight are bounded by their **own budget** (`MAX_DIALS_IN_FLIGHT`, 8)
and a `CONNECT_TIMEOUT` of 5s, not by the peer table.

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

Push wins on both counts.

## Consequences

- Discovery converges without a timer, and the exit criterion is a functional
  test rather than a manual check.
- The dial budget is separate from `MAX_PEERS`, and that separation was bought
  the hard way. Bounding dials by *reserving a peer slot before connecting* looks
  neater — one cap, no counter — and was tried first. It is much worse: a
  `connect()` to an unroutable address blocks for about two minutes on Linux, so
  a single `addr` of 32 such addresses holds every slot the node has and it stops
  accepting inbound connections as well as dialling. A review probe reproduced
  exactly that. Trading a bounded thread leak for total peering denial is not a
  trade. The budget bounds the leak; `CONNECT_TIMEOUT` bounds how long each entry
  can hold a share of it; `MAX_PEERS` goes on meaning peers.

  *2026-08-27, in M6.* That last sentence was true of the design and not of
  the code: the share was held for as long as the **connection** lasted, not
  the dial, because the guard was dropped after `serve_connection` returned.
  Eight settled peers therefore stopped the node dialling a ninth — discovery
  silently over, with twenty-four peer slots empty — and once `POST /connect`
  existed it answered "too many dials are already in flight" forever. The
  guard is now dropped the moment `connect_timeout` returns, which is what
  `CONNECT_TIMEOUT` bounding it means.
- Addresses past the budget are **dropped, not queued**. There is no backlog to
  drain and no memory to grow; discovery is repeated often enough that losing one
  address costs nothing.
- `addr` is capped at `MAX_ADDRESSES` (256) per message, refused on the count
  before any address is read. The payload cap alone would allow over a million.
- Addresses are not persisted, so discovery starts from the seed list every run.
  An address book that outlives the process is out of v1 scope
  ([ADR-0001](0001-v1-scope.md)).
- Nothing ages or scores addresses. A peer can push the same address repeatedly;
  each one costs a table lookup and is dropped when we already hold it.
- `knows()` cannot recognise a gossiped address as a peer we are already
  *accepting* until that peer's `version` arrives, because until then all we have
  is its ephemeral source port. The dial that results is collapsed moments later
  by the nonce dedup ([ADR-0015](0015-peer-identity-and-duplicate-connections.md));
  the cost is one connection, briefly.
- Outbound and inbound still share `MAX_PEERS`, so a node whose slots are full of
  inbound connections cannot dial what it discovers. That is #45.
