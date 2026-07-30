# ADR-0008 — Coinbase

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

[ADR-0003](0003-transaction-witness-format.md) deleted `script_sig` and moved
unlocking data into a witness that the **txid does not cover**. That was right for
spending, and it broke something else: it removed the field Bitcoin uses to keep
coinbases unique.

Two coinbases by the same miner, to the same address, for the same reward
serialize identically and share a txid. The later one then overwrites the earlier
UTXO entry — the BIP30 duplicate-txid problem, which Bitcoin first patched with an
expensive per-transaction check and then solved structurally with **BIP34**:
require the block height inside the coinbase's `scriptSig`, which the txid covers.

Our witness is excluded from the txid *by construction*, so it cannot carry
height. The uniqueness value has to live somewhere the txid covers.

## Options considered

### Option A — Reintroduce a free-form field on `TxIn` *(chosen)*
A `coinbase_data` byte string, covered by the txid, required empty on
non-coinbase inputs.
- **+** Bitcoin's shape; immediately recognisable.
- **+** Extranonce lives there too, exactly as Bitcoin does, which leaves the
  coinbase witness empty as ADR-0003 already specified.
- **−** Adds three rules: emptiness elsewhere, the height prefix, a length bound.
- **−** One byte of overhead on every ordinary input for a field always empty.

### Option B — A separate `Coinbase` type
`Block { coinbase: Coinbase, transactions: Vec<Transaction> }`, with `height` as a
real field.
- **+** *Removes* three runtime rules by making them unconstructible: first-is-
  coinbase, at-most-one-coinbase, and no-coinbase-in-mempool — the last being a
  check Bitcoin has to write explicitly.
- **−** A second transaction-shaped type to serialize, parse and hash; three call
  sites handle both shapes; a visible deviation from Bitcoin.

### Option C — Repurpose `sequence`
Define the coinbase input's `sequence` as the height.
- **+** Nothing new to build.
- **−** A field whose name contradicts its meaning; keeps all three runtime rules.
- Moot in any case: [ADR-0011](0011-network-identity-and-fields.md) deletes
  `sequence`.

## Decision

**Option A.** `TxIn` carries `coinbase_data: Vec<u8>`, compact-size prefixed and
**covered by the txid**.

```
coinbase_data := height (u32, little-endian) ‖ extranonce ‖ arbitrary bytes
                 └─ offset 0, fixed width ─┘
                 total length ≤ 100 bytes
```

- **Height is a plain `u32` little-endian**, not BIP34's minimally-encoded script
  number. A `CScriptNum` is sign-magnitude with minimal-encoding rules, and
  [ADR-0002](0002-output-locking-model.md) deliberately excluded numeric opcodes
  from the VM — building that encoding for one field that is never executed buys
  nothing. A fixed offset also makes parsing and validation unambiguous, and it
  matches invariant 1 (all multi-byte integers little-endian). Compact-size was
  rejected because invariant 4 reserves it for *counts*, and a height is not one.
- **Extranonce lives here**, so grinding it changes the coinbase txid and hence
  the merkle root, giving fresh search space — as in Bitcoin. The coinbase witness
  stays **empty**, exactly as ADR-0003 states.
- **The 100-byte cap is Bitcoin's**, leaving ~96 bytes for extranonce and an
  arbitrary message.

**The coinbase remains a `Transaction`**, identified by predicate: exactly one
input whose outpoint is null (32 zero bytes, `v_out = 0xFFFFFFFF`).

**Rules this requires:**

- `coinbase_data` must be empty on every non-coinbase input.
- A coinbase's `coinbase_data` must be ≥ 4 bytes and begin with the block's height.
- Length ≤ 100 bytes.
- The first transaction in a block is a coinbase; no other transaction is.
- No coinbase may enter the mempool.
- Coinbase outputs must not exceed `subsidy(height) + fees`
  ([ADR-0006](0006-monetary-model.md)).

**Malleability is not reopened.** The emptiness rule is consensus, not policy, so
no relaying party can put bytes into an ordinary transaction's `coinbase_data` to
move its txid. Only a miner writes the field, and rewriting a mined block means
redoing its proof-of-work.

**Maturity is 100 blocks**, as a per-network parameter
([ADR-0007](0007-genesis-and-network-parameters.md)). Its unit is blocks rather
than time, because the depth it must exceed is a function of hashrate
distribution, not of how fast blocks arrive — so Bitcoin's number transfers
directly even though blocks here are 30s rather than 10 minutes. At 30s that is
50 minutes, after which a continuously mining node always has mature coins.
Maturity exists to protect against a reorg orphaning a coinbase and cascading
invalidity into everything spending it, which is only meaningful because
[ADR-0012](0012-reorg-and-undo-data.md) brought reorg back into scope.

## Consequences

- `TxIn` becomes `{ previous_output, coinbase_data, witness }` — `sequence` is
  removed by [ADR-0011](0011-network-identity-and-fields.md).
- Ordinary transactions pay one byte per input for an always-empty field.
- e2e tests must mine past maturity before spending a coinbase. Bitcoin's regtest
  has the same property and everyone generates 101 blocks; at test difficulty this
  is instant, and the test network lowers the parameter anyway.
- The arbitrary-data tail is available for a genesis message, in the tradition of
  *"Chancellor on the brink of second bailout for banks"*.
- Settles the glossary terms **coinbase**, **coinbase_data**, **extranonce**, and
  **maturity**.
