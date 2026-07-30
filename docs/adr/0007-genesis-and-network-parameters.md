# ADR-0007 — Genesis block and network parameters

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

Wallets need coins before they can send any, and M3 (transactions) lands before
M4 (mining). The original plan called for "config-driven UTXO allocation" without
saying whether that allocation was part of consensus.

One fact settles most of it. An `Outpoint` is `(Txid, v_out)` — a reference to an
output *of some transaction*. There is no way to inject a UTXO that isn't one.
A side-channel config that seeded the UTXO set directly would produce coins whose
outpoints reference a txid that does not exist, and two nodes with different
configs would disagree about the UTXO set while agreeing on every block: a fork
with no detectable moment of divergence.

## Decision

**The genesis block contains exactly one transaction**, coinbase-shaped (null
outpoint, empty witness), whose outputs *are* the allocation. Premined coins are
therefore ordinary outputs with a real txid and index, spendable by the ordinary
rules. Genesis is never a special case in the transaction model.

**The allocation is a file committed to the repo, and the genesis block is
derived from it deterministically.** It is consensus: byte-identical on every
node building from the same source.

**The mainnet allocation is empty. There is no premine.** Every coin that will
ever exist comes from a block reward. This became possible only once mining moved
firmly into scope — the premine's original job was making M3 testable before M4
existed, and that job belongs to the test network. It keeps the ~2,016,000 cap in
[ADR-0006](0006-monetary-model.md) true without an asterisk, and leaves no
author-held allocation to explain.

**The test allocation funds well-known keys** whose private material ships with
the source, so e2e tests and local development have spendable coins from height
zero.

**Genesis must satisfy proof-of-work**, with the nonce committed alongside the
allocation. There is no height-zero exemption, so "every block satisfies PoW",
"exactly one coinbase" and the merkle-root rule are all total. This costs
essentially nothing: starting difficulty is deliberately low — it is tuned for a
throttled miner on cheap hosting — so the search takes milliseconds. A small tool
regenerates the nonce when an allocation changes.

### Network parameters

The allocation, the starting difficulty, and the coinbase maturity
([ADR-0008](0008-coinbase.md)) form one **network parameter set**, and the genesis
block is derived from it. Two consequences follow, and the second is the valuable
one:

1. Consensus-relevant values are varied by choosing a parameter set, never by
   editing a config on a running node.
2. **Different parameters produce a different genesis hash.** A test node and a
   mainnet node therefore have different chains from block zero and cannot
   silently merge — the strongest network separation available, stronger than
   magic bytes ([ADR-0011](0011-network-identity-and-fields.md)), which only
   filter at the wire.

## Consequences

- A `genesis` tool (or test) finds and prints the nonce for a parameter set;
  changing an allocation without regenerating it yields a block that fails its own
  PoW check, loudly, at startup.
- The genesis coinbase's outputs enter the UTXO set by the same path as any other
  coinbase. On mainnet it has zero outputs, so the first subsidy is forgone.
- **Maturity applies to the genesis coinbase too**, so test allocations are not
  spendable until 100 blocks deep unless the test network lowers the parameter —
  which it does.
- M3's exit criteria are met on the test network, not the public one. The public
  chain's first spendable coins arrive about 50 minutes in (100 blocks at 30s).
- Settles the glossary terms **genesis block**, **allocation**, and **network
  parameters**.
