# ADR-0002 — Output locking model (what locks a coin)

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

The keystone of the domain model. Everything downstream — `address.rs`, the
sighash ([ADR-0004](0004-sighash.md)), `Wallet::send`, mempool and block
validation, coinbase outputs, and genesis funding — depends
on a precise answer to "what does a `TxOut` commit a coin to, and what must a
spender present to unlock it?"

The code answered it three incompatible ways at once:

- `transaction.rs` — `TxOut { value: u64, destiny_pub_key: String }`. The field
  name says *public key*.
- That `String` was serialized as raw UTF-8 into the transaction body, so it was
  committed by the **txid** — making a text encoding part of consensus.
- `wallet.rs` populated it from a `destination_address` parameter. So the field
  was *named* pubkey, *typed* String, and *fed* an address.

The round-trip test passed `"first_public_key"` as an output and
`"first_signature"` as an input — arbitrary UTF-8. Nothing in the type system
said these were keys or signatures at all, and the spend rule was undefined
because no code had ever needed to check one.

A pubkey and an address are different objects:
`address = Base58Check(version ‖ HASH160(pubkey))`. One is a 33-byte EC point;
the other is a checksummed, versioned human string. Serializing the *address
string* is a consensus trap: the txid would commit to the Base58 alphabet, the
version byte, and the checksum, so two nodes disagreeing on any encoding detail
compute different txids for the same coin and silently fork.

## Options considered

### Option A — Pubkey-hash field; address display-only
`TxOut { value: u64, pubkey_hash: [u8;20] }`, spent by a `TxIn` carrying
`(sig, pubkey)` with `HASH160(pubkey) == out.pubkey_hash` and a valid signature.

- **+** Clean consensus/encoding split; smallest change; simplest sighash.
- **−** The spend rule is hardcoded. With exactly one template ever, the
  "interpreter" is a fixed comparison — no generality, and nothing to show for
  the concept that makes Bitcoin's output model interesting.

### Option B — Base58 address String in the output
Closest to the code as written; the human string is serialized.
- **−** Encoding, version byte, and checksum become consensus. Wasteful (~34
  bytes vs 20) and fork-prone. Conflates UI with consensus. Rejected outright.

### Option C — Pay-to-pubkey (store the full pubkey)
`TxOut { value, pubkey: [u8;33] }`; spend needs only a signature.
- **+** Simplest possible spend path; no HASH160 anywhere.
- **−** Loses the pubkey-hash and address arc entirely; larger outputs; the
  pubkey is exposed at funding time rather than at spend time.

### Option D — Minimal Script + a stack VM *(chosen)*
`TxOut { value, script_pubkey: Vec<u8> }`, unlocked by witness data, evaluated by
a small interpreter. The one shipped template is
`OP_DUP OP_HASH160 <20-byte hash> OP_EQUALVERIFY OP_CHECKSIG`.

- **+** Builds the single most distinctive idea in Bitcoin's design — that an
  output specifies a *predicate*, not an identity.
- **+** The interpreter is highly testable in isolation and has a genuinely small
  interface (see Consequences).
- **+** Everything Option A would have taught is still built: HASH160,
  Base58Check, the pubkey-hash template. It arrives *through* the VM rather than
  instead of it.
- **−** More code than Option A, and it couples to the sighash — resolved, and
  made much simpler, by [ADR-0004](0004-sighash.md).

## Decision

**Option D.** A `TxOut` commits to a `script_pubkey`: a program that a spender
must satisfy. Spending supplies a **witness** — a list of stack items — which
seeds the VM's stack before `script_pubkey` executes. The output is unlocked when
execution leaves exactly one truthy item on the stack.

The one template that ships in v1:

```
script_pubkey:  OP_DUP OP_HASH160 <20-byte pubkey hash> OP_EQUALVERIFY OP_CHECKSIG
witness:        [ <64-byte signature>, <33-byte compressed pubkey> ]
```

**VM semantics.** Chosen for a chain with no legacy to preserve, so several of
Bitcoin's compromises are simply not inherited:

- **Single-phase execution.** The witness is a list of data items, not a script,
  so the stack is seeded directly and only `script_pubkey` executes. Bitcoin's
  two-phase evaluation and its push-only rule exist to constrain a `script_sig`
  that *could* contain opcodes. Ours cannot — it is structurally data.
- **Clean stack.** Success requires exactly one item remaining, and that it is
  truthy. Bitcoin tolerates leftover items for legacy reasons and had to add this
  rule later.
- **Truthiness:** empty and zero are false, everything else true. No
  negative-zero case — Bitcoin's exists only because its numbers are
  sign-magnitude.
- **Unknown opcodes fail immediately.** They are not reserved for future
  soft-fork meaning; there are no soft forks to plan for.
- **Explicit limits** on script size, stack depth, and operation count, checked
  during execution. Exact values are pinned at implementation time.

**Opcode set (~12).** What the template needs plus its cheap neighbours: data
push, `OP_DUP`, `OP_HASH160`, `OP_SHA256`, `OP_EQUAL`, `OP_EQUALVERIFY`,
`OP_VERIFY`, `OP_DROP`, `OP_SWAP`, `OP_CHECKSIG`, `OP_CHECKSIGVERIFY`,
`OP_TRUE`/`OP_FALSE`. The surplus is deliberate: it makes a *second* working
script expressible — a hash-preimage lock — which is what distinguishes a real
interpreter from a fixed comparison wearing a VM's clothes.

Deliberately excluded: conditionals (`OP_IF`/`OP_ELSE`), numeric opcodes and
Bitcoin's sign-magnitude encoding, `OP_CHECKMULTISIG`, and the
`OP_PUSHDATA1/2/4` family.

**HASH160** is `RIPEMD160(SHA256(x))`, using the RustCrypto `ripemd` crate. The
composition is Bitcoin's and is hand-rolled; RIPEMD160 itself is a
general-purpose hash from 1996 and is therefore a dependency, exactly as the
project's dependency rule prescribes. `sha2` and the `digest` traits are already
in `Cargo.lock`, so this costs one crate entry and no new transitive weight.

**Addresses stay display-only** — see [ADR-0005](0005-address-encoding.md). The
output commits to 20 raw bytes inside `script_pubkey`; Base58Check happens at the
wallet and UI edge and never enters consensus or a txid.

## What was pinned at implementation

*2026-08-27.*

This decision fixed that there **are** explicit limits and left the numbers to
the implementation. They are:

| Limit | Value | Bitcoin's |
|---|---|---|
| Script size | 1,000 bytes | 10,000 |
| Stack depth | 100 items | 1,000 |
| Operations | 200 | 201 |
| Stack item size | 520 bytes | 520 |

The first three are an order of magnitude under Bitcoin's, because the scripts
this chain ships are 25 bytes and 5 operations deep, and a limit exists to bound
what a stranger can make a validator do — not to leave room for scripts nobody
writes.

**The fourth is not in the list above** and is the one that needed adding. A
`script_pubkey` is bounded by its own size, and with no `OP_PUSHDATA` family the
largest item a script can push is 75 bytes. But the **witness** seeds the stack
from outside the script, and it is bounded only by `MAX_PAYLOAD_SIZE` — so
without an item limit, one 32 MiB witness item makes `OP_HASH160` hash 32 MiB.
520 bytes is Bitcoin's number and is far above the 64 a signature needs.

The interpreter's signature also moved. This decision sketched
`execute(script_pubkey, witness_stack, txid) -> Result<bool>`; it is
`Result<()>`. Nothing distinguishes "ran and left something falsy" from "could
not run" — both mean the coin stays put — so the two channels collapse into one
that carries a reason.

## Consequences

- **The VM's interface is unusually small.** Because the sighash is the txid
  ([ADR-0004](0004-sighash.md)), `OP_CHECKSIG` does not need the transaction — it
  needs 32 bytes. The interpreter is
  `execute(script_pubkey, witness_stack, txid) -> Result<bool>`, with no
  knowledge of transactions, UTXOs, or the chain.
- **A consensus/policy distinction arises without being invented.** Consensus
  accepts any script the VM validates; the *wallet* recognises only the P2PKH
  template when scanning the UTXO set for its own coins. Non-standard outputs are
  perfectly valid and simply invisible to wallets.
- **`TxOut.destiny_pub_key: String` becomes `script_pubkey: Vec<u8>`**, and the
  transaction round-trip test is rewritten — its `"first_public_key"` strings
  cannot survive.
- New modules: `script.rs` (opcodes, stack, interpreter, limits) and `address.rs`
  (Base58Check). `util.rs` gains `hash160`.
- Settles the glossary terms **TxOut**, **Locking commitment** (which resolves to
  *script_pubkey*), **HASH160**, and **Address**.
- Feeds [ADR-0003](0003-transaction-witness-format.md) (the witness format) and
  the pending coinbase decision (coinbase outputs use the same template; a
  coinbase input carries an empty witness).
- **Reversed a deferral in [ADR-0001](0001-v1-scope.md)**, which had originally
  put Script out of v1. See that ADR's "How scope decisions get made here".
