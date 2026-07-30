# ADR-0006 — Monetary model

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

The coin's economics were entirely undefined. `TxOut.value` was a bare `u64` with
no stated unit, no supply bound, and no rule about what happens when output values
are summed. M4's coinbase cannot compute a subsidy without this, and the
value-overflow attack class lives here.

## Decision

**Atomic unit.** `1 AVI = 100,000,000 atoms`. `TxOut.value` and `Amount` count
atoms; the divisor is a display concern applied at the wallet and UI edge. The
ratio is Bitcoin's, because the number is arbitrary and copying the familiar one
is free.

**Subsidy schedule.** 50 AVI initially, halved every **20,160 blocks** by integer
right-shift, reaching zero after 33 halvings. Thereafter miners earn fees only.

**Target block time is 30 seconds**, which makes the halving interval
approximately one week. That pace is actively defended by
[ADR-0009](0009-difficulty-and-timestamps.md); without retarget it would have
been a hope rather than a parameter.

**Total supply is ~2,016,000 AVI**, and it is **emergent, not enforced**. No code
tracks cumulative issuance. The figure is `50 × 20,160 × 2` — the sum of the
halving series — and it holds because the schedule holds. This mirrors Bitcoin,
where nothing checks the 21 million bound either.

**Overflow rule.** Two mechanisms, deliberately overlapping:

1. Every individual value must satisfy `0 ≤ value ≤ MAX_MONEY`, where
   `MAX_MONEY = 2,016,000 × 10⁸` atoms.
2. Every sum uses `checked_add` on `Amount`; `None` rejects the transaction.

The first does more than back up the second. With each value capped near
`2×10¹⁴` and output count bounded by `MAX_PAYLOAD_SIZE`, a sum cannot approach
`u64`'s `1.8×10¹⁹` ceiling — so overflow becomes **unreachable** rather than
merely detected.

### Why both

In August 2010 a Bitcoin transaction carried two outputs of roughly 92 billion
BTC. Their sum overflowed the *signed* 64-bit accounting and came out negative,
so the `sum(outputs) ≤ sum(inputs)` check passed and 184 billion BTC were created
from nothing (CVE-2010-5139).

Rust removes half of that automatically: `u64` has no negatives and `checked_add`
is total, so the 2010 bug is not directly reproducible. But wraparound is still
reachable, and the historical lesson is precisely that *relying on a bound to
prevent overflow* is the reasoning that failed. So the arithmetic is made safe
**and** the bound is enforced — the bound for early rejection and structural
impossibility, the checked arithmetic so correctness never rests on the bound
being right.

## Consequences

- `Amount` (from [ADR-0003](0003-transaction-witness-format.md)) is where the
  arithmetic lives: `checked_add`, `checked_sub`, and a constructor rejecting
  values above `MAX_MONEY`. Raw `u64` arithmetic on values is a bug.
- Fee is `sum(inputs) − sum(outputs)`, computed with the same checked operations,
  and must be non-negative.
- A coinbase's outputs must not exceed `subsidy(height) + fees`. Claiming less is
  legal and burns the difference, as in Bitcoin.
- **Supply is not a validation rule.** A future ADR that changes the schedule
  changes the cap silently; the number in this document is a derivation, not a
  constant to check against.
- Settles the glossary terms **atom**, **AVI**, **subsidy**, **halving**, and
  **MAX_MONEY**.
