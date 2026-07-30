# ADR-0003 — Transaction witness format

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

> Supersedes nothing. An earlier draft of this ADR, titled "TxIn signature &
> pubkey wire encoding", was rewritten before ever being accepted:
> [ADR-0002](0002-output-locking-model.md) chose a Script model, which changed
> the question from "how do we encode two typed fields" to "what is the shape of
> the unlocking data".

## Context

Under [ADR-0002](0002-output-locking-model.md), a spender no longer presents
typed `(signature, pubkey)` fields — it presents whatever satisfies the output's
`script_pubkey`. This ADR fixes the shape of that data and where it lives.

The starting point was:

```rust
pub struct TxIn {
    pub previous_output: Outpoint,
    pub signature: String,   // a secp256k1 signature's .to_string()
    pub sequence: u32,
}
```

Three problems. There was no pubkey field at all, so no locking model except pure
pay-to-pubkey could work. The signature was a `String` serialized as UTF-8, so
its text encoding was part of the txid. And `wallet.rs` signed
`Message::from_digest(outpoint.tx_id)` — the *previous* transaction's id — so the
signature committed to nothing about the spend: not the amount, not the
destination, not even which output index. Anyone observing it could replay it in
any transaction spending that same outpoint. There was no authorization at all.

The design pressure comes from **malleability**. `get_tx_id()` hashes
`get_raw_format()`, which serialized the signature. That makes the txid movable
by anyone: re-encode the unlocking data, or exploit the fact that `(r, s)` and
`(r, -s mod n)` are both valid ECDSA signatures over the same digest, and you get
a different txid for a semantically identical transaction. The mempool, the
wallet, and the M6 end-to-end tests all track transactions by txid.

## Options considered

### Option A — Canonicalise, keep the signature inside the txid
Enforce low-S and a minimal-push encoding as validity rules; the txid keeps
covering the unlocking data. This is Bitcoin pre-2017.
- **+** Small; one serialization mode; one hash.
- **−** Patches malleability rather than removing it, and cannot prove it closed.
  The rules are then carried forever.

### Option B — Copy Bitcoin's SegWit
Witness separation plus script versioning and a witness commitment in an
`OP_RETURN` output of the coinbase, with the merkle root still over txids.
- **+** Maximum fidelity to Bitcoin as it exists today.
- **−** Most of that machinery exists because SegWit had to be a **soft fork** on
  a live chain. The worst consequence here: a native SegWit output is not
  executed by the interpreter in the normal way — its validation is special-cased
  outside the VM. That would make the one template we ship the one that bypasses
  the VM ADR-0002 just chose to build.

### Option C — Clean witness separation *(chosen)*
The same core idea as SegWit, without the soft-fork baggage, because this chain
has no legacy to preserve.

## Decision

**Option C.** Unlocking data lives in a **witness** that the txid does not cover.

**Shape.** A witness is a list of stack items:

```rust
pub struct Witness(Vec<Vec<u8>>);   // stack items, not a script
```

For a P2PKH spend it holds exactly `[64-byte signature, 33-byte compressed
pubkey]`. Because it is data and not a program, "push-only" is not a rule to
enforce — it is the type. Opcodes cannot be smuggled in, and there is no push
encoding left to vary.

**Placement — inline on the input:**

```rust
pub struct TxIn {
    pub previous_output: Outpoint,
    pub sequence: u32,
    pub witness: Witness,
}
```

Bitcoin keeps witnesses in a collection parallel to the inputs; that layout is
forced by SegWit's soft-fork constraints. Inline placement makes an input and the
data authorizing it a single value, so they cannot desync, be reordered
independently, or mismatch in length — an invariant a parallel `Vec` would leave
to runtime checks. `script_sig` is removed entirely; there is no unlocking
*script* field.

**Two hashes.**

| Hash | Covers | Used by |
|---|---|---|
| `txid` | version, inputs (outpoint + sequence), outputs, lock_time | `Outpoint` references, the sighash, mempool keys |
| `wtxid` | all of the above **plus** witnesses | the block's merkle root |

`get_raw_format(include_witness: bool)` serves both. The witness-excluded form is
**only ever used for hashing, never for transmission** — the wire always carries
witnesses — so no marker or flag byte is needed. Bitcoin's exists purely so that
pre-SegWit parsers could still read the new format.

**The merkle root is built over wtxids, not txids.** This is what makes the
separation cost nothing: witnesses are committed directly by the block header, so
there is no second merkle tree and no witness commitment hidden in the coinbase.
Bitcoin cannot do this because redefining the merkle root is a hard fork. (Feeds the pending
merkle-construction decision.)

**Encodings.**

- **Signature:** 64-byte compact `(r ‖ s)`, with **low-S enforced at validation**.
  Fixed width lets validation reject a wrong-sized item before touching crypto,
  and low-S means a transaction has exactly one valid witness encoding — so the
  wtxid is canonical, which matters because wtxids are what the merkle root
  commits to. DER was rejected: it is variable-length and its main historical
  lesson is that it was malleable enough to need a consensus rule (BIP66) to pin
  down.
- **Pubkey:** 33-byte compressed SEC point, **compressed only**. Allowing
  uncompressed keys would be a genuine footgun — the same private key yields two
  different encodings that hash to two different `HASH160`s and therefore two
  different addresses, so a wallet could fund one and spend from the other and
  see its own coins as unspendable. Bitcoin carries both only for pre-2012
  reasons.

**Type modelling.** Newtypes `Txid`, `Wtxid`, `PubKeyHash`, and `Amount` are
introduced. `Txid` and `Wtxid` are both `[u8; 32]` with entirely different
meanings, and confusing them is a silent consensus bug that no obvious test
catches — a wtxid in an `Outpoint` makes a coin unspendable; a merkle tree over
txids stops committing witnesses at all.

A transaction under construction has no witnesses, so the wallet builds through a
`TxBuilder` whose `sign()` is the **only** constructor of a `Transaction`.
Everything downstream therefore holds a transaction that is witnessed by
construction.

Typestate (`Transaction<Unsigned>` / `Transaction<Witnessed>`) was considered and
rejected *here*: `Transaction` is the most-touched type in the codebase, so a
type parameter would appear in `Mempool`, `Block`, `UtxoSet`, and every
validation signature in order to express a distinction that matters only inside
the wallet. The typestate budget is better spent on **M2's peer handshake**
(`Connecting → VersionSent → VersionAcked → Ready`), which is a genuine
multi-state protocol where the type prevents relaying to a peer that has not
completed the handshake.

## Consequences

- **Malleability is structurally impossible**, not merely ruled out. No
  minimal-push canonicalisation rule is needed anywhere.
- **The sighash collapses to the txid** — there is nothing left to blank. See
  [ADR-0004](0004-sighash.md), which this decision resolves as a corollary.
- **The coinbase gets simpler.** Its input carries an empty witness. Bitcoin
  needs a "witness reserved value" there solely to anchor its coinbase
  commitment, which this design removes. (Feeds the pending coinbase decision.)
- `transaction.rs` changes throughout: `TxIn.signature: String` is replaced by
  `witness`; `get_raw_format` takes a flag; `get_tx_id()` is joined by
  `get_wtxid()`; the round-trip test is rewritten in both directions.
- `block.rs`'s merkle construction switches to wtxids.
- `wallet.rs` is rebuilt around `TxBuilder` on `k256`; the
  `Message::from_digest(outpoint.tx_id)` signing line is deleted.
- `Cargo.toml`: `secp256k1` out, `k256` in.
- Settles the glossary terms **TxIn**, **Witness**, **wtxid**, and **txid**.

## Later changes

The reasoning above stands unchanged; the field list moved.

- [ADR-0008](0008-coinbase.md) added `coinbase_data: Vec<u8>` to `TxIn`, covered
  by the txid, required empty on non-coinbase inputs. This does **not** reverse the
  removal of `script_sig`: `coinbase_data` is never executed and the VM never sees
  it, so "there is no unlocking script field" remains true. Deleting `script_sig`
  also deleted the field Bitcoin uses to keep coinbases unique, and that is what
  the new field restores.
- [ADR-0011](0011-network-identity-and-fields.md) deleted `sequence` from `TxIn`
  and `lock_time` from `Transaction` outright, on the same reasoning this ADR used
  for `script_sig` — a field that does not exist cannot be malleated and needs no
  rule.

Current shape: `TxIn { previous_output, coinbase_data, witness }` and
`Transaction { version, inputs, outputs }`.
