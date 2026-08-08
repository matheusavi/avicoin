# ADR-0016 — A connection has to last before it resets the backoff

- **Status:** Accepted
- **Date:** 2026-08-08
- **Deciders:** @matheusavi

## Context

`addresses_to_connect` was dialled once at boot. A peer down at that moment was
lost until the process restarted, and so was one that dropped later. Retrying
needs a backoff, or a peer that is simply gone becomes a busy loop.

The usual rule is **grow on failure, reset on success**, where success means the
TCP connect returned. That rule is wrong here, and the repo's own default
configuration is the counterexample.

## Decision

One thread per configured address. It dials, waits for the connection to end,
backs off, and dials again — so a live connection cannot be dialled a second
time, by construction rather than by a check that could race.

The backoff doubles from `first` (1s) to `cap` (60s). It resets only when the
connection **lasted at least `settled`** (10s). A connection shorter than that
did not work, whatever the socket reported.

*Lasted* is measured from the moment the socket connected, never from the moment
the dial began. A dial that fails lasted no time at all, however long it spent
failing: a blackholed address sits in `connect()` for around two minutes on
Linux, and counting that as a long-lived connection would reset the backoff on
precisely the peer it exists to back off.

## Why not reset on a successful connect

The checked-in `config.toml` points `host_address` and `addresses_to_connect` at
the same address, so the default local setup dials *itself*. Since
[ADR-0015](0015-peer-identity-and-duplicate-connections.md) that self-connection
is recognised at the handshake and dropped — after the TCP connect succeeded.

Under "reset on connect" the sequence is: connect (reset to 1s) → handshake →
recognise our own nonce → drop → wait 1s → repeat. Forever, once per second,
out of the box. The backoff would never grow, because every attempt "succeeds".

The same shape covers a peer that accepts and immediately hangs up, whether
broken or hostile: it would hold us at the shortest retry interval indefinitely.

Requiring the connection to *last* removes both. A self-connection dies in
milliseconds, so the backoff climbs to its cap and stays there; a real peer that
runs for ten seconds resets it.

## Consequences

- A node dialling itself settles into a 60s retry rather than a 1/s loop. It
  still retries, because "stop dialling an address that turned out to be us" is
  identity work this ADR does not cover.
- A peer whose connection legitimately ends inside 10s is redialled slightly
  later than it might be. Nothing depends on the difference.
- `settled` is a duration rather than "did it reach Ready". Both would work here;
  a duration needs no signal threaded back out of the connection, and does not go
  stale if what counts as established changes.
- Discovery peers are out of scope. When #44 lands they will come and go by
  design, and reconnecting to them is not the same intent.
