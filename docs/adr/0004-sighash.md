# ADR-0004 — Sighash: what a spender signs

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

A signature exists to prove the holder of a key authorized *this exact payment*,
so it must cover the payment's details. But the signature also has to travel
inside the transaction — which is circular: adding the signature changes the
transaction, so the signature no longer matches what it signed.

Bitcoin resolves this by hashing a **doctored copy** with the signature slots
emptied, and signing that. It works, but "exactly which parts are emptied, what
is substituted back, and how it varies per input" is most of what makes sighash
design fiddly. It is also where transaction malleability historically lived.

This ADR was reserved expecting that work. [ADR-0003](0003-transaction-witness-format.md)
removed the need for it.

## Decision

**The sighash is the txid.** A spender signs `HASH256` of the witness-excluded
serialization — byte for byte the same preimage that defines the transaction's
`txid`. One digest per transaction, shared by every input. `SIGHASH_ALL`
semantics only; there is no sighash type byte and no variant.

This follows directly from witness separation: the unlocking data already lives
outside the hashed form, so the circularity never arises. There is no doctored
copy to construct, because "the transaction without its signatures" is a real
object that already has a name.

### Why this is sufficient

- The txid commits to the version, every input's outpoint and sequence, every
  output's value and `script_pubkey`, and the lock_time — everything that
  determines which coins move and where they go. Altering any of it changes the
  txid, so the signature stops matching and the transaction is rejected.
- Two different transactions have different txids, so a signature cannot be
  replayed into another transaction spending the same outpoint. (The old code
  signed `outpoint.tx_id`, which had exactly this flaw.)
- Two inputs in one transaction locked to the same key share a digest, so their
  signatures are interchangeable. Nothing is gained by swapping them: both inputs
  belong to the same transaction and both are controlled by that key.

### Why Bitcoin signs more, and why we don't

BIP143 additionally commits to each input's **value**, its `scriptCode`, and the
index of the input being signed. The value commitment exists for **offline
signers**: a hardware wallet cannot verify that the coin it is spending is worth
10 rather than 10,000 unless the amount is fed to it and bound into the
signature. The scriptCode and index commitments matter when a transaction mixes
script types.

Neither applies here. The wallet reads amounts from its own UTXO set, and there
is one script template. Adopting BIP143's digest would be real complexity
guarding a threat model this project does not have — precisely the anticipatory
scope [ADR-0001](0001-v1-scope.md) exists to resist.

## Consequences

- **The interpreter's interface shrinks.** `OP_CHECKSIG` needs 32 bytes, not a
  transaction, so `script.rs` is
  `execute(script_pubkey, witness_stack, txid) -> Result<()>` and knows nothing
  about transactions, UTXOs, or the chain. *(Both ADRs sketched `Result<bool>`;
  [ADR-0002](0002-output-locking-model.md) records why it collapsed.)*
- **No sighash type byte** anywhere in the format. `SIGHASH_SINGLE`, `NONE`, and
  `ANYONECANPAY` are unavailable — so is any flow where one party signs their
  input and hands a partial transaction to another to complete. No planned
  feature needs that; if one ever does, `Transaction.version` is the natural
  place to introduce a second scheme.
- Signature verification is `verify(sig, txid, pubkey)` with the low-S check from
  ADR-0003.
- Settles the glossary term **Sighash**.

## Later changes

The argument above stands; two of the fields it enumerates no longer exist.

[ADR-0011](0011-network-identity-and-fields.md) deleted `sequence` and
`lock_time`, and [ADR-0008](0008-coinbase.md) added `coinbase_data`. So "the txid
commits to the version, every input's outpoint and sequence, every output, and the
lock_time" now reads:

> the txid commits to the version, every input's outpoint and `coinbase_data`, and
> every output.

The sufficiency argument is unaffected — it turns on the txid covering everything
that determines *which coins move and where they go*, which is still true, and is
in fact now tighter: two of the three fields that carried no meaning were removed
rather than constrained, so there is less in the digest that means nothing.
