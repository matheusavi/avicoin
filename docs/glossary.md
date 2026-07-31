# Avi Coin — Glossary

Ubiquitous language for the project. One term = one meaning, used the same way in
code, docs, and conversation.

A term marked ⏳ is not settled yet. If the decision has been made it cites the
ADR that made it; if not, it names the **topic** rather than a number — ADR
numbers are assigned on write, never reserved, so there is no number to cite
until the decision exists. Open topics are listed in
[docs/adr/README.md](adr/README.md).

Status legend: ✅ settled · ⏳ pending a decision · ⚠️ deliberate deviation from
Bitcoin · 🅧 deferred out of v1 by [ADR-0001](adr/0001-v1-scope.md).

---

## Serialization & hashing

- **HASH256 / double-SHA-256** ✅ — `SHA256(SHA256(x))`, via `util::get_hash`. The
  only hash used for txids, block hashes, merkle nodes, and header PoW.
- **HASH160** ✅ (ADR-0002) — `RIPEMD160(SHA256(x))`, via the `ripemd` crate. See
  the Script section for why the composition is hand-rolled and the hash function
  is not.
- **Compact-size** ✅ — Bitcoin's variable-length count encoding
  (`util::get_compact_int` / `ByteReader::read_compact`). Prefixes every
  variable-length vector on the wire.
- **Little-endian on the wire** ✅ — all multi-byte integers are LE when
  serialized. Hashes are handled LE internally and only reversed to big-endian
  for human display.
- **ByteReader** ✅ — the single bounds-checked deserialization cursor
  (`byte_reader.rs`); every parse goes through it and returns `anyhow::Result`.

## Wire protocol

- **Magic bytes** ✅ (ADR-0011) — 4-byte network identifier prefixing every message
  header. `0x41564931` (ASCII `"AVI1"`) on mainnet, `0x41564954` (`"AVIT"`) on the
  test network. A cheap early filter that rejects a foreign message at header
  parse — *not* the real network boundary, which is the **genesis hash**.
- **Header** ✅ — 24 bytes: magic(4) ‖ command(12) ‖ payload_len(4, LE) ‖
  checksum(4). Checksum = first 4 bytes of `HASH256(payload)`.
- **Message\<T\>** ✅ — `Header` + typed `payload: T` where `T: Payload`.
- **MAX_PAYLOAD_SIZE** ✅ — 32 MiB cap enforced during parse.

## Transaction model

- **Outpoint** ✅ — `(Txid, v_out: u32)`; a reference to one specific output of
  one specific transaction. The identity of a coin. References a **txid**, never
  a wtxid.
- **TxIn** ✅ (ADR-0003, ADR-0008, ADR-0011) — spends an Outpoint. Carries
  `previous_output`, **coinbase_data**, and its **Witness** inline. There is no
  `script_sig` and no `sequence`.
- **Transaction** ✅ (ADR-0011) — `{ version, inputs, outputs }`. `version` must be
  `1`; it is kept solely as the escape hatch for a future format change. There is
  no `lock_time`.
- **coinbase_data** ✅ (ADR-0008) — a byte string on every `TxIn`, **covered by the
  txid**, required empty on non-coinbase inputs. On a coinbase it is
  `height (u32 LE) ‖ extranonce ‖ arbitrary`, at most 100 bytes. Never executed —
  the VM never sees it — which is why it is not a return of `script_sig`. It
  carries the height because the txid must be unique per coinbase, and the Witness
  is excluded from the txid by construction.
- **Extranonce** ✅ (ADR-0008) — miner-varied bytes inside `coinbase_data`. Changing
  them changes the coinbase txid and so the merkle root, giving fresh search space
  once the header nonce is exhausted.
- **Validation totality** ✅ (ADR-0011) — the property that every field is either
  interpreted or constrained to a single legal value. No field is parsed and
  ignored; `sequence` and `lock_time` were deleted rather than pinned, because a
  field that does not exist cannot be malleated and needs no rule.
- **TxOut** ✅ (ADR-0002) — `value` plus a **script_pubkey**.
- **script_pubkey** ✅ (ADR-0002) — the program in a TxOut that a spender must
  satisfy. The only template v1 ships is
  `OP_DUP OP_HASH160 <PubKeyHash> OP_EQUALVERIFY OP_CHECKSIG`. Replaces the
  earlier placeholder term *locking commitment*, which is retired.
- **Witness** ✅ (ADR-0003) — the unlocking data for one input: a list of stack
  items (`Vec<Vec<u8>>`), **not** a script. For a P2PKH spend it is exactly
  `[64-byte signature, 33-byte compressed pubkey]`. Because it is data and not a
  program, opcodes cannot appear in it — "push-only" is the type, not a rule.
- **Stack item** ✅ — one element of a Witness or of the VM's stack: an arbitrary
  byte string. Empty and zero are **false**; anything else is **true**.
- **txid** ✅ (ADR-0003) — `HASH256` of the **witness-excluded** serialization.
  Covers version, inputs (outpoint + sequence), outputs, and lock_time. This is
  what an Outpoint references, what a mempool is keyed by, and what a spender
  signs. Unmalleable: no witness byte can move it.
- **wtxid** ✅ (ADR-0003) — `HASH256` of the **witness-included** serialization.
  Used only as the leaf of the block's merkle tree, which is how witnesses get
  committed by the header. Distinct from txid; the newtypes `Txid` and `Wtxid`
  exist so the two cannot be confused.
- **PubKeyHash** ✅ — `HASH160(compressed pubkey)`, 20 bytes. Appears raw inside a
  script_pubkey; its human encoding is an **Address**.
- **Address** ✅ (ADR-0005) — `Base58Check(0x17 ‖ PubKeyHash)`. Always 34
  characters, always leading `A`. **Display-only**: computed at the wallet/UI
  edge, never serialized, never committed by a txid, never seen by the VM.
- **Sighash** ✅ (ADR-0004) — the digest a spender signs. It **is the txid**: one
  digest per transaction, shared by every input. `SIGHASH_ALL` semantics only;
  there is no sighash type byte. Nothing is blanked, because the witness is
  already outside the hashed form.

## Script

- **Script / the VM** ✅ (ADR-0002) — the stack interpreter that decides whether a
  witness unlocks a script_pubkey. Its whole interface is
  `execute(script_pubkey, witness_stack, txid) -> Result<bool>` — it knows
  nothing of transactions, UTXOs, or the chain, because the sighash is just the
  txid.
- **Single-phase execution** ⚠️ ✅ (ADR-0002) — the stack is seeded from the
  witness items and only the script_pubkey executes. Bitcoin evaluates an
  unlocking *script* first and carries the stack across; that phase exists to
  constrain a `script_sig` that could contain opcodes, which ours cannot.
- **Clean stack** ⚠️ ✅ (ADR-0002) — a script succeeds only if execution leaves
  **exactly one** item and it is truthy. Bitcoin tolerates leftover items for
  legacy reasons and had to add this rule afterwards.
- **P2PKH template** ✅ — the one script pattern v1 ships. Consensus accepts any
  script the VM validates; the *wallet* recognises only this template when
  scanning the UTXO set, so non-standard outputs are valid but invisible to
  wallets.
- **HASH160** ✅ (ADR-0002) — `RIPEMD160(SHA256(x))`, via the `ripemd` crate. The
  composition is hand-rolled; the hash function is a dependency, since RIPEMD160
  is general-purpose cryptography rather than a Bitcoin primitive.

## Blocks & consensus

- **Block header** ✅ — the 80-byte `mine_array`: version(4) ‖ prev_hash(32) ‖
  merkle_root(32) ‖ time(4) ‖ n_bits(4) ‖ nonce(4).
- **n_bits** ✅ — compact 32-bit encoding of the PoW target
  (`Block::get_target_256`: mantissa = low 23 bits, exponent = high 8 bits,
  `target = mantissa << (8 * exponent)`).
- **Target** ✅ — the 256-bit threshold; a block is valid PoW when
  `HASH256(header)` interpreted LE is `< target`.
- **Merkle root** ✅ (ADR-0003, ADR-0010) — root of the **wtxid** tree in the
  header. Building it over wtxids rather than txids is what commits witnesses to
  the block directly, removing any need for a coinbase witness commitment. The
  algorithm is Bitcoin's: pair left-to-right, duplicate the last node on odd
  counts. That duplication is not injective, so it is paired with a rule rejecting
  blocks that contain duplicate wtxids — and such a rejection must **not** cache
  the block hash as permanently invalid, or the denial of service survives.
- **Coinbase** ✅ (ADR-0008) — the block's first transaction, minting subsidy +
  fees. A `Transaction` identified by predicate: one input with a null outpoint.
  Its input carries an **empty Witness** and a `coinbase_data` beginning with the
  block height.
- **Maturity** ✅ (ADR-0008) — 100 blocks (a network parameter) before a coinbase
  output may be spent. Measured in blocks, not time, because the reorg depth it
  must exceed depends on hashrate distribution rather than block interval.
- **Atom / AVI** ✅ (ADR-0006) — `1 AVI = 100,000,000 atoms`. Values are counted in
  atoms everywhere; the divisor is applied only for display.
- **Subsidy / halving** ✅ (ADR-0006) — 50 AVI initially, halved every 20,160
  blocks (~1 week at a 30s target), reaching zero after 33 halvings, after which
  miners earn fees only.
- **MAX_MONEY** ✅ (ADR-0006) — `2,016,000 × 10⁸` atoms. A bound on every
  individual value, which combined with `checked_add` on `Amount` makes `u64`
  overflow unreachable rather than merely detected. The total supply it names is
  **emergent** from the schedule; no code tracks issuance.
- **Difficulty retarget** ✅ (ADR-0009) — recomputed **every block** from a moving
  window of the last ~60. Continuous adaptation in both directions, so there is no
  death spiral when hashrate leaves, no window boundary, and therefore no timewarp
  bug.
- **Median-time-past** ✅ (ADR-0009) — the median timestamp of the previous 11
  blocks; a block's timestamp must exceed it. A block must also not exceed local
  time by more than 5 minutes — far tighter than Bitcoin's 2 hours, which at 30s
  blocks would be four times the retarget window.
- **Genesis block** ✅ (ADR-0007) — height zero, containing exactly one
  coinbase-shaped transaction whose outputs are the **allocation**. Must satisfy
  PoW like any block; its nonce is committed. **The mainnet allocation is empty —
  there is no premine.**
- **Network parameters** ✅ (ADR-0007) — allocation, starting difficulty, maturity,
  and magic bytes as one set. The genesis block is derived from it, so different
  parameters give a different genesis hash and the chains cannot silently merge.

## Node & networking

- **Config** ✅ — the node's resolved settings: defaults, then `config.toml`, then
  CLI arguments, each overriding the previous where it supplies a value.
  Addresses are validated into `SocketAddr` at that boundary, so nothing
  downstream holds an address that might not parse.
- **SharedNode** ✅ — `Arc<Mutex<Node>>` central state. Designed, not yet built
  (M1).
- **PeerTable / PeerHandle** ✅ — peer registry with a per-peer writer channel.
  Not yet built (M1).
- **Ready peer** ✅ — a peer that has completed version/verack; only Ready peers
  are relayed to. Not yet built (M2).
- **Reorg** ✅ (ADR-0012) — switching to a heavier branch. Disconnect back to the
  fork point restoring outputs from each block's **undo record**, then connect
  forward. Cost is proportional to reorg *depth*, not chain height. Not optional:
  without it, any second miner permanently splits the network — tens of times a
  week at 30s blocks.
- **Cumulative work** ✅ (ADR-0012) — total proof-of-work along a branch; the chain
  selection rule. Not height — once difficulty varies per block, a shorter branch
  can carry more work.
- **Undo record** ✅ (ADR-0012) — per block, for every input it spent:
  `(Outpoint, TxOut, height, is_coinbase)`. The height and coinbase flag are
  required so a restored coinbase output's **maturity** can be re-checked against
  the new tip.
- **Best-block marker** ✅ (ADR-0013) — how far the persisted UTXO set has been
  advanced. If it lags the block index tip after a crash, the node replays only
  the missing blocks. Replay is the recovery path, never the normal startup path.
- **Data directory** ✅ (ADR-0013) — per-node, holding `blocks.dat`, `undo.dat`,
  the embedded key-value store for the index and UTXO set, and the wallet key
  (mode `0600`, plaintext). Per-node so a multi-node network runs on one host.
