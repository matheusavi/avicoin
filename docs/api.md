# HTTP API

What a node serves when `api_address` is set. **It is off unless configured** —
exposing a node to HTTP is a decision somebody makes, not one a default makes
for them.

The shapes below are a contract. M7's scenario tests read them, so a field
renamed here breaks tests rather than quietly changing meaning.

## Writes come from this node's own page

A `POST` carrying an `Origin` that is not this node's is **`403`**. A
cross-origin `fetch` with a simple body reaches a write endpoint without a
preflight, and the attacker never needs to read the response — the side effect
*is* the attack. Somebody with the viewer open on their own node should not
have another page make it dial an address or hold a transaction.

A request with no `Origin` at all is a client that is not a browser, and is
allowed: `curl` and the functional suite are not what CSRF is about.

## Conventions

- Every response is JSON, with `Content-Type: application/json`.
- **Hashes are big-endian**, the way an explorer shows them and the way the
  node's own log prints them — reversed from the bytes anything hashes
  (invariant 5). A hash from this API pastes into a block explorer; it does
  not paste into `chain.redb`.
- Amounts are in **atoms** unless a field's name says otherwise.
- An error is `{"error": "<reason>"}` with a status that is not 200. A reason
  is for a person: it says what was wrong, not what to do about it.
- `404` is a thing that does not exist. `400` is a request that does not make
  sense. Neither is a panic, and a panic in one handler is contained: it
  becomes a `500` and the server answers the next request.
- Every request and every response is capped, and every read is timed. A
  request head is at most 8 KiB, a body at most 256 KiB, a response at most
  1 MiB, and a socket that goes quiet costs a worker ten seconds and no more.
  Connections past a fixed queue get a `503` and are closed rather than held.
- `405` is a method an endpoint does not answer. The request line must be
  HTTP: `this is not HTTP` is a `400`, not a `this` request for `is`.

## Endpoints

### `GET /status`

Where the node is.

```json
{
  "network": "main",
  "height": 412,
  "tip": "00000000a1b2…",
  "peers": 3,
  "mempool": 7,
  "headers": 415,
  "coins": 1204
}
```

| Field | Meaning |
|---|---|
| `network` | The parameter set's name — `main` or `test`. |
| `height` | The height of the **connected** tip, not of the best header. |
| `tip` | That block's hash, big-endian. |
| `peers` | Connections in the peer table, Ready or not. |
| `mempool` | Transactions held. |
| `headers` | Headers the index holds, best chain and every other branch. Ahead of `height` while a node is syncing — headers arrive before bodies. |
| `coins` | Unspent outputs in the UTXO set. |

### `GET /blocks?from=&count=`

A page of the best chain, oldest first. `from` is a height and defaults to 0;
`count` defaults to and is capped at 50. Asking for more is a `400`, not a
truncated answer — a caller that thinks it got everything is worse off than one
that got an error.

```json
{
  "height": 412,
  "blocks": [
    { "hash": "00000000a1b2…", "height": 410, "time": 1756252800 }
  ]
}
```

`height` is the **connected** tip's, the same number `/status` gives, and the
page never runs past it. This is the chain the node has *applied*, not the
heaviest headers it knows: while a node is behind — or holds a fork of its own
— those are two different chains rather than a prefix of one another, and a
height taken from the headers would name a block this node does not have.
`GET /block/height/{n}` reads the same chain, so the two always agree.

### `GET /block/{hash}` and `GET /block/height/{n}`

One block, by either name. They return the same object.

```json
{
  "hash": "00000000a1b2…",
  "height": 410,
  "best_chain": true,
  "confirmations": 3,
  "version": 1,
  "previous_block": "00000000c3d4…",
  "merkle_root": "9f8e…",
  "time": 1756252800,
  "n_bits": "0x1e00ffff",
  "nonce": 3378221,
  "size": 216,
  "transactions": [ … ]
}
```

`size` is the block's witness-included serialization — the bytes a `block`
message carries. `transaction_count` is the block's; `transactions` is a page
of at most 200, because a megabyte block renders to several megabytes of JSON.

`best_chain` says whether the block is on the chain the node has connected.
A block on a branch that lost has **`confirmations: 0`** and keeps its height:
it is not confirmed by anything, and giving it the same number as the block
that beat it would be saying it was. A hash that is not 32 bytes of hex is a `400`; one that is but
names nothing is a `404`. A height past the tip is a `404`, and a height that
is not a number is a `400`.

### `GET /tx/{txid}`

One transaction, from the mempool or from a block on the best chain.

```json
{
  "txid": "3a7c…",
  "wtxid": "b21f…",
  "version": 1,
  "coinbase": false,
  "size": 220,
  "inputs": [
    { "previous_output": { "txid": "9d4e…", "index": 0 }, "witness_items": 2 }
  ],
  "outputs": [
    { "index": 0, "atoms": 4999999900, "avi": "49.99999900", "script_pubkey": "76a914…" }
  ]
}
```

A confirmed transaction also carries `block` and `height`; one in the mempool
carries `confirmations: 0`.

**Nothing indexes a transaction by its id**, so this is a scan: the mempool,
then the last 500 blocks of the **connected** chain, newest first. Connected
rather than merely known, because headers arrive ahead of bodies and a window
of header-only entries would report "not found" for a transaction on disk. Past that it is a
`404` that says so. An unbounded scan is one a stranger picks the cost of.

**`txid` and `wtxid` are both here on purpose.** They differ for any
transaction with a witness — the second covers bytes the first does not
([ADR-0003](adr/0003-transaction-witness-format.md)) — and showing both is what makes
witness separation a thing a reader can check rather than a claim.

`atoms` is the number everything hashes; `avi` is the same number for a person.
Neither is derived from the other after the fact — both come from the same
`Amount`, so nothing rounds.

### `GET /address/{address}`

What one address holds, from the UTXO set — which is what "unspent" means. A
scan of the chain would be answering a different question, slowly.

```json
{
  "address": "AVi…",
  "atoms": 5000000000,
  "avi": "50.00000000",
  "unspent_count": 214,
  "unspent": [
    { "txid": "3a7c…", "index": 0, "atoms": 5000000000, "avi": "50.00000000",
      "height": 410, "coinbase": true }
  ]
}
```

The balance is the sum of `unspent`, and both come from the same numbers. An
address nobody has paid is a **200 with an empty list**, not a 404: it is a
real address with no coins, and a caller has to be able to tell that from a
typo. A string that is not valid Base58Check, or whose version byte is not Avi
Coin's, is a 400.

`unspent` is sorted by outpoint and then **paged**: `?from=` skips that many,
and at most 200 come back. `unspent_count` is how many there are altogether, so
a caller knows when it has them all — `avicoin send` pages until it does,
because a wallet that can only *see* two hundred coins can only *spend* two
hundred, and a mining node passes that in about two hundred blocks. Sorted
before it is paged, so two requests describe two parts of one list.

`atoms` is the whole balance regardless of the page.

Answering this means scanning the UTXO set **under the node lock**: nothing
indexes it by script, and the set is behind the lock. The scan clones only what
it keeps, but it is the one unauthenticated endpoint whose cost grows with the
node's state rather than with a constant, and it is why there is no paging past
the cap.

### `GET /mempool`

```json
{
  "count": 7,
  "transactions": [
    { "txid": "3a7c…", "fee_atoms": 220, "size": 220 }
  ]
}
```

Richest first — the order a miner would take them in. `count` is the whole
mempool; `transactions` is capped at 200.

### `GET /peers`

```json
{
  "count": 3,
  "peers": [
    { "id": 4, "listening": "203.0.113.7:34352", "direction": "inbound",
      "handshake": "ready", "connected_seconds": 412 }
  ]
}
```

`listening` is where the peer **listens**, which is the only address anyone
could dial back — never the ephemeral source port an accepted connection came
from ([ADR-0015](adr/0015-peer-identity-and-duplicate-connections.md)). It is
`null` until the peer's `version` has arrived. `direction` is `inbound` or
`outbound`; `handshake` is `awaiting-version`, `awaiting-verack` or `ready`;
`connected_seconds` is how long the connection has been in the table.

### `GET /log?since=`

The tail of the node's bounded log.

```json
{ "next": 118, "lines": ["Listening on 127.0.0.1:34352", "…"] }
```

`next` is what to pass as `since` next time. It counts every line the node has
ever recorded, including ones that have fallen off the front of the bounded
log — so a caller that falls far enough behind gets the oldest lines still
held rather than the wrong ones. At most 200 lines per response.

## Write endpoints

Two, and neither is a privileged route.

### `POST /tx`

The body is a **signed** transaction as hex — the same bytes a `tx` message
carries. It goes through the same validation, the same mempool and the same
relay a peer's transaction does; there is no second door.

```json
{ "txid": "3a7c…" }
```

A refusal is a `400` carrying **the reason**, because a demo where a
submission fails silently is worse than one where it fails:

```json
{ "error": "3a7c… pays out 50.00000000 against 10.00000000 in" }
```

Not hex, hex that is not a transaction, and a body past
`MAX_TRANSACTION_SIZE` (100,000 bytes) are each a `400` before anything
expensive happens. The API **never signs**: the transaction arrives signed or
it is refused.

### `POST /connect`

The body is an address, `host:port`. It is dialled through the same path a
configured peer takes, budget and caps included.

```json
{ "dialling": "203.0.113.7:34352" }
```

A `400` says which limit stopped it: this node's own address, an address
already a peer, a full peer table, or too many dials already in flight. The
last of those bounds dials **in progress**, not the connections they open — a
peer that stays up costs a peer slot and nothing else. The
endpoint cannot be used to walk around a limit the P2P layer enforces.

`200` means the dial **started**, not that it succeeded — a peer appears in
`GET /peers` when it does. Nothing here blocks on a stranger's TCP handshake.

## The viewer

`GET /` (and `/index.html`) is a single page; `/viewer.css` and `/viewer.js`
are its two assets. `HEAD` on any of them answers without a body; anything else
is a `405` — the viewer is read-only.
They are served by the same server, and **nothing on the page reaches outside
the origin** — no CDN, no font host, no analytics. A page that fetched from
elsewhere is a page that breaks when elsewhere does, and a deployment that is
no longer one artefact.

There is **no build step**: no bundler, no transpiler, no framework. The three
files are compiled into the binary with `include_str!`, so what is served is
byte-for-byte what a reader of the repo sees, and the deployment is one file
with nothing to lose beside it.

The page polls the read endpoints every two seconds — each section on its own,
so one endpoint failing leaves the others current rather than frozen — and
renders what they already encoded. It does no encoding of its own: no byte
reversal, no dividing atoms into AVI, because invariant 5 puts that at this
API's edge and nowhere else. There are tests that grep for both; they are
tripwires against the spellings anybody would write rather than proofs.

## What is deliberately absent

`GET /tx` searches the mempool and then the **best chain**, newest block
first. A transaction only ever on a branch that lost is a `404`: it is not part
of the chain, and saying otherwise would be saying it is.

**Addresses are not per-network.** [ADR-0005](adr/0005-address-encoding.md) gives Avi
Coin one version byte, `0x17`, for both networks — so there is no such thing as
"an address for the other network" to refuse. The two chains are kept apart by
their genesis and their magic, not by their address format.

No endpoint reads the wallet's private key, and none signs anything. A public
URL must not be able to spend the operator's coins, so there is no `POST /send`
and there will not be one — a transaction is submitted already signed.

Spending is `avicoin send`, which runs on the machine that holds the key:
it reads `wallet.key` directly, asks this API only what the address holds,
signs locally, and posts the result to `POST /tx`. What crosses the wire is a
signed transaction — the same thing any stranger could have sent.
