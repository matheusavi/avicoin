# ADR-0005 — Address encoding

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** @matheusavi

## Context

[ADR-0002](0002-output-locking-model.md) commits an output to a `script_pubkey`
containing a raw 20-byte pubkey hash. Humans cannot exchange 20 raw bytes, so
there must be a text encoding — and the project explicitly wants Base58Check
built from scratch, since it is a canonical Bitcoin thing to implement.

The question is what that encoding produces, and what it is allowed to touch.

## Decision

**An address is `Base58Check(0x17 ‖ HASH160(pubkey))`**, where the checksum is
the first 4 bytes of `HASH256(0x17 ‖ HASH160(pubkey))`.

**Addresses are display-only.** They are computed at the wallet and UI edge,
never serialized into a transaction, never committed by a txid, and never seen by
the VM. This is invariant 5 in `ARCHITECTURE.md`: encoding does not enter
consensus. Serializing the address string instead would drag the Base58 alphabet,
the version byte, and the checksum into consensus, so two nodes disagreeing on
any encoding detail would compute different txids for the same coin and fork
silently.

### Why version byte `0x17`

The leading character of a Base58Check string is determined by the version byte.
Measured over 4,000 random hashes per version:

| Version | Leading character | Length |
|---|---|---|
| `0x00` — Bitcoin mainnet | variable | 26–34 |
| `0x05` — Bitcoin P2SH | `3` | 34 |
| `0x6f` — Bitcoin testnet | `m` / `n` | 34 |
| **`0x17` (23) — Avi Coin** | **`A`** | **34** |
| `0x19` (25) | `B` | 34 |

Three reasons:

1. **Unmistakably not Bitcoin.** M6 puts a node behind a public URL with a
   send-transaction form. Version `0x00` would produce addresses indistinguishable
   from real Bitcoin addresses, inviting someone to paste one in — and the form
   would accept it, because it is structurally valid. A distinct prefix removes
   that class of confusion. `0x6f` trades it for a different one: those addresses
   claim to be Bitcoin testnet.
2. **Fixed length.** Base58Check must encode each leading `0x00` byte of the
   payload as a leading `1` character — which is *why* Bitcoin mainnet addresses
   start with `1`, the version byte being itself a zero byte, and why they vary
   between 26 and 34 characters. A non-zero version byte means that case never
   arises: every Avi Coin address is exactly 34 characters. (The padding rule is
   still implemented correctly; it is simply unreachable.)
3. `A` for Avi Coin. Small, but it makes the block explorer read as this
   project's rather than as a Bitcoin clone's.

## Consequences

- New module `address.rs`: Base58 alphabet, the leading-zero padding rule,
  checksum computation, encode and decode, and rejection of a bad checksum or an
  unexpected version byte.
- A wallet's public identity is its address; `Wallet` exposes it, and `TxBuilder`
  accepts one for `pay_to` and decodes it to a `PubKeyHash` before building the
  `script_pubkey`.
- Round-trip tests (`encode → decode`), plus tests that a corrupted character and
  a wrong version byte are both rejected. Known-answer tests can be taken from
  public Base58Check vectors by encoding with version `0x00`.
- Changing the version byte later invalidates every address in circulation, so
  this is effectively permanent once a public node runs.
- Settles the glossary term **Address**.
