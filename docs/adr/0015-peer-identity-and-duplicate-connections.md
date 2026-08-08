# ADR-0015 — Peer identity is the version nonce, and duplicates break the tie by it

- **Status:** Accepted
- **Date:** 2026-08-08
- **Deciders:** @matheusavi

## Context

A node has to answer two questions about every connection: *is this me?* and
*do I already have this peer?* Getting either wrong is not cosmetic. The
checked-in `config.toml` points a node at its own listening address, so the
default local setup dials itself; and two nodes that appear in each other's seed
lists will dial each other, opening two TCP connections where one peer exists.

Neither question can be answered from an address. [ADR-0011](0011-network-identity-and-fields.md)
fixed the `version` fields but not the policy that uses them, and #16 shipped a
dedup over **dialled addresses only**, deferring the rest here. The reason it
had to: an *accepted* connection shows the peer's ephemeral source port, not the
port it listens on, so the same peer arriving inbound never matches the outbound
entry. That dedup could only ever catch us dialling one address twice.

The second question has a trap in it that is easy to miss.

## Decision

**A peer's identity is the nonce in its `version`**, minted once per process run
and carried on every connection from that process. `PeerTable`'s address dedup
is removed rather than kept alongside it.

A `version` whose nonce is our own means the connection loops back to this
process, and it is dropped.

A nonce already in the table means one peer on two connections. **The survivor
is the connection dialled by the larger of the two nonces.**

## Why the tie-break is phrased over the nonces

The obvious rules are phrased from one node's point of view — "keep the one we
dialled", "keep the older one", "keep the newer one" — and the first of those is
what we tried first. All of them are wrong for the same reason.

Two nodes that dial each other hold **the same two sockets under opposite
origins**. A's outbound socket *is* B's inbound socket:

```
        A (nonce 10)                       B (nonce 5)
   S1  Dialled  ─────────────────────────►  Accepted
   S2  Accepted ◄─────────────────────────  Dialled
```

Under "keep the one we dialled", A keeps S1 and B keeps S2 — and each drops what
the other kept, so **both** sockets close and the peer is lost entirely. Under
"keep the older one" the two ends can disagree too, because which connection
arrived first is a matter of local timing.

A correct rule has to be a function of something both ends see identically. The
pair of nonces is the only such thing available at that moment, so the rule is
stated over it: keep the connection dialled by the larger nonce. Each end
evaluates it as *keep our dial iff our nonce is larger* — opposite answers about
`Origin`, which is exactly what agreeing on one socket requires.

Above, A keeps S1 because 10 > 5, and B keeps S1 because 5 < 10 means keeping
what it accepted. One socket, agreed at both ends.

The rule reads the same regardless of which `version` arrives first: whichever
connection identifies second either loses (and hangs up) or wins (and evicts the
incumbent, whose table entry holds its only sender — see
[ARCHITECTURE](../ARCHITECTURE.md#concurrency-model)). Both orders are covered by
tests.

## Consequences

- Two connections to one peer collapse to one, whichever direction they came
  from — the case #16's dedup could not reach.
- A node dialling itself no longer registers a peer, so the repo's default
  config demonstrates the guard rather than tripping over it.
- **Nothing dedups before the handshake.** A connection holds a slot from
  `spawn_connection` until its `version` arrives, bounded by `HANDSHAKE_TIMEOUT`.
  That is the price of identity living in a message rather than in an address.
- Equal nonces are indistinguishable from a self-connection and the connection is
  dropped. At 64 random bits the collision is not worth a tie-break of its own.
- The rule is stable but not *fair*: a node with a large nonce always keeps its
  outbound connections. Nothing depends on fairness here, and if peer scoring
  ever does, this is the decision to revisit.
