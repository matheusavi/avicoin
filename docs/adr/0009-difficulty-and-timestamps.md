# ADR-0009 — Difficulty retarget and timestamp rules

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

[ADR-0001](0001-v1-scope.md) originally deferred difficulty retarget on the
grounds that a fixed `n_bits` demonstrates proof-of-work just as well. That holds
for a controlled local demo. It is false for **a public node anyone can join**,
which is what v1 is now for.

Difficulty tuned to one throttled miner on cheap hosting means the first visitor
with an ordinary laptop is orders of magnitude faster. Block time collapses, the
chain sprints through halvings, and storage balloons. The property that makes the
demo interesting — strangers can connect and mine — is exactly what breaks it.

### The failure that actually matters

Adaptation is **asymmetric**, and the dangerous direction is the one Bitcoin never
has to worry about.

When hashrate *arrives*, blocks come faster, so a retarget window completes faster
and difficulty catches up quickly. Self-correcting.

When hashrate *leaves*, blocks slow by the same factor — and a windowed scheme now
needs N more blocks to trigger while each one takes hours. Concretely: your node
plus a visitor's laptop sets the difficulty, the visitor closes their laptop, and
a 60-block window needs weeks to complete. The chain dies. This is the classic
small-chain **death spiral**, and it is a direct consequence of running on very
little compute.

Bitcoin's 2016-block window with a 4× clamp is tuned for stability at scale and
offers no protection here. Bitcoin's *testnet* does face this and patches it with
a special case: if no block arrives for 20 minutes, the next may be minimum
difficulty.

## Decision

**Retarget every block**, from a moving window of the last **N ≈ 60** blocks, with
a per-block clamp.

- **No death spiral.** Difficulty begins falling on the very next block after
  hashrate drops, in proportion to the observed slowdown. There is no window to
  wait out in either direction.
- **No special case.** Bitcoin testnet's minimum-difficulty escape hatch is
  unnecessary, so consensus keeps one difficulty rule rather than two — no
  "undefined except when".
- **No timewarp bug.** Bitcoin's timewarp vulnerability comes from measuring a
  retarget window one block short of its true span. With no window boundary, the
  off-by-one has nowhere to live.
- **Statistically sound.** Block intervals are exponentially distributed, so a
  60-block window has a relative standard error near `1/√60` ≈ 13% — difficulty
  jitters by roughly that much at constant hashrate, which is acceptable. A much
  shorter window would oscillate; a much longer one would readmit the spiral.

`N`, the clamp, and the starting difficulty are **pinned at implementation**, with
the stated intent that adapting to a 1000× change in hashrate takes tens of
blocks, not hundreds, and that starting difficulty suits a deliberately throttled
miner.

### Timestamp rules

Per-block retarget computes difficulty directly from recent timestamps, which
makes them **load-bearing**: a miner who lies about time moves difficulty. Bitcoin
dilutes this across 2016 blocks; we cannot.

- **Median-time-past:** a block's timestamp must exceed the median of the previous
  11. Chosen over a simple "later than its parent" rule because a single
  far-future timestamp would otherwise permanently ratchet the floor; a median
  absorbs outliers.
- **Future limit: local time + 5 minutes.** Bitcoin allows 2 hours, a 2009 artifact
  from unreliable clock synchronisation. At 30s blocks that is 240 block times —
  four times the entire retarget window — and would hand a miner exactly the lever
  this ADR closes. Five minutes is 10 block times: ample for ordinary clock skew
  in an era of universal NTP, and far too small a fraction of the window to steer
  difficulty meaningfully.

## Consequences

- **Amends [ADR-0001](0001-v1-scope.md)**, which deferred retarget.
- Difficulty is a function of the previous N block headers, so validating a block
  requires them — the block index must expose ancestors cheaply.
- A node with more than five minutes of clock drift will reject blocks the network
  accepts. This presents as an unexplained partition rather than a clear error, so
  the node should log loudly when it rejects on the future-time rule.
- The throttled miner (burst hashing with sleeps, so average CPU stays low on
  cheap hosting) is a runtime concern, not a consensus one — difficulty adapts to
  whatever hashrate results.
- Settles the glossary terms **retarget**, **median-time-past**, and **target
  block time**; supersedes the placeholder "fixed `n_bits`" entry.
