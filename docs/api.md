# HTTP API

What a node serves when `api_address` is set. **It is off unless configured** —
exposing a node to HTTP is a decision somebody makes, not one a default makes
for them.

The shapes below are a contract. M7's scenario tests read them, so a field
renamed here breaks tests rather than quietly changing meaning.

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
  "mempool": 7
}
```

| Field | Meaning |
|---|---|
| `network` | The parameter set's name — `main` or `test`. |
| `height` | The height of the **connected** tip, not of the best header. |
| `tip` | That block's hash, big-endian. |
| `peers` | Connections in the peer table, Ready or not. |
| `mempool` | Transactions held. |

## What is deliberately absent

No endpoint reads the wallet's private key, and none signs anything. A public
URL must not be able to spend the operator's coins, so there is no `POST /send`
and there will not be one — a transaction is submitted already signed.
