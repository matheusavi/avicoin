# ADR-0011 — Network identity and transaction field policy

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

Two loose ends that share a theme: things in the protocol that carried no defined
meaning.

**Magic bytes.** Every message header begins with `0xf9beb4d9` — **Bitcoin
mainnet's** magic. An Avi Coin node currently announces itself as Bitcoin on the
wire.

**Uninterpreted fields.** `TxIn.sequence` and `Transaction.lock_time` are parsed,
serialized, and never read. Both are covered by the txid, which makes them
malleability vectors: anyone relaying could alter them and change a transaction's
txid, undoing the unmalleability [ADR-0003](0003-transaction-witness-format.md)
was built to guarantee. `Transaction.version` has the same problem.

## Decision

### Magic bytes

`0x41564931` — ASCII **`"AVI1"`** — on mainnet, and `0x41564954` — **`"AVIT"`** —
on the test network.

Bitcoin chose `0xf9beb4d9` with the high bit set in every byte, reasoning that
such a value is improbable in ordinary data. That reasoning barely applies here:
payloads are hashes, keys, and script bytes, which are high-entropy, so any 4-byte
value collides with probability 2⁻³² regardless of which one is picked. Given the
choice is therefore free, **self-documenting in a hex dump** is worth more than
notional improbability — the magic is recognisable at a glance when reading raw
bytes while debugging the wire protocol.

Magic bytes are a cheap **early filter**, rejecting a foreign message at header
parse. They are not the real network boundary: that is the genesis hash
([ADR-0007](0007-genesis-and-network-parameters.md)), which differs whenever
network parameters differ and therefore separates chains from block zero.

### Uninterpreted fields

**`TxIn.sequence` and `Transaction.lock_time` are deleted.**

The obvious alternative was to constrain them — require fixed values so every
field is either interpreted or bounded. Deleting is strictly better: **a field
that does not exist cannot be malleated and needs no rule to enforce.** It is the
same move ADR-0003 made with `script_sig`, for the same reason. It also saves four
bytes per input and four per transaction.

```rust
pub struct TxIn {
    pub previous_output: Outpoint,
    pub coinbase_data:   Vec<u8>,   // ADR-0008; empty on non-coinbase inputs
    pub witness:         Witness,   // ADR-0003
}

pub struct Transaction {
    pub version: u32,               // must be 1
    pub inputs:  Vec<TxIn>,
    pub outputs: Vec<TxOut>,
}
```

**`Transaction.version` is kept and constrained to `1`.** Anything else is
invalid, so it is neither undefined nor malleable. It earns its four bytes as the
one deliberate escape hatch for format evolution —
[ADR-0004](0004-sighash.md) already names it as where a second signing scheme
would be introduced. Without it, any future change is an unsignalled hard fork,
which matters more for a chain intended to stay running publicly than for a
throwaway. The block header keeps its own version field on the same reasoning.

**Validation is now total.** Every field in a transaction is either interpreted
(`previous_output`, `witness`, `outputs`, `coinbase_data` on a coinbase) or
constrained to a single legal value (`version`, and `coinbase_data` elsewhere).
There is no field a node parses and ignores.

## Consequences

- Adding real timelocks later is a format change rather than a matter of giving
  meaning to an existing field. Accepted: ADR-0001 defers timelocks, there are no
  compatibility obligations, and `version` exists to signal such a change.
- Relative-timelock and replace-by-fee style features, which Bitcoin builds on
  `sequence`, are unavailable without a format change. Neither is planned.
- `transaction.rs` serialization, parsing, and round-trip tests all change shape;
  every txid the code has ever produced changes.
- The magic-byte constant moves into the network parameter set, so a node cannot
  hold mainnet magic and test parameters.
- Settles the glossary terms **magic bytes** and **validation totality**; retires
  **sequence** and **lock_time**.
