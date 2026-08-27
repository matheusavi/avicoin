# ADR-0018 — Inbound connections may not take every slot

- **Status:** Accepted
- **Date:** 2026-08-08
- **Deciders:** @matheusavi

## Context

`MAX_PEERS` (32) was shared between connections we dial and connections we
accept, first come first served. [ARCHITECTURE](../ARCHITECTURE.md) recorded that
as acceptable "while peers come from a static list, not once discovery lands in
M2". Discovery has landed.

Sharing the cap hands an attacker a lever: open 32 connections and the node has
nowhere left to dial from, so **every peer it can see is one the attacker
allowed**. That is the precondition for an eclipse attack, and it is cheap to
close here because the peer table is the only thing counting connections.

## Decision

**Inbound connections may occupy at most `MAX_INBOUND` (24) slots. The remaining
`RESERVED_OUTBOUND` (8) can only ever be filled by connections this node chose to
make.** Outbound is not capped below `MAX_PEERS`: it may use slots inbound is not
using.

A refusal for this reason is `Refused::InboundFull`, distinct from
`Refused::AtCapacity`, so a log says which bound was hit.

## Alternatives

**Two independent caps** — `MAX_INBOUND` inbound *and* `MAX_OUTBOUND` outbound,
never trading. Rejected: a node with a long seed list could not use slots no
inbound peer wanted, and the second cap has to be tuned against a number
(`MAX_PEERS`) that is already arbitrary.

**Evict an inbound peer when a dial needs a slot.** Rejected for now: eviction
needs a policy for *which* peer, and there is no peer scoring to base one on.
Refusing the newcomer is the rule everywhere else in the table
([ARCHITECTURE](../ARCHITECTURE.md#concurrency-model)); making one case behave
differently would be the surprising choice.

**Leave it and rely on the dial budget.** Rejected: `MAX_DIALS_IN_FLIGHT`
([ADR-0017](0017-peer-discovery.md)) bounds how many dials are *in progress*, not
whether any can succeed. A full table refuses them at registration regardless.

## Consequences

- A node under an inbound flood keeps eight slots to dial configured and
  discovered peers with. The flood is still served — up to 24 of it.
- **A listen-only node accepts 24 rather than 32.** Named here because the ticket
  asked for it: an unstated policy is how a node ends up quietly refusing the
  peer it most wanted. Nothing is lost that the node was using — the reservation
  only bites once 24 inbound peers are already connected.
- The reservation is by *origin*, which is exactly what
  [ADR-0015](0015-peer-identity-and-duplicate-connections.md)'s tie-break already
  turns on. A peer that reaches us both ways still collapses to one connection,
  and which one it keeps decides which budget it lands in.
- Nothing yet distinguishes a *useful* outbound peer from one that is merely
  dialled. When peer scoring exists, this reservation is where it should apply.
