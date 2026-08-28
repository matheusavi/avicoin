# Running a node

## In a container

```bash
docker build -t avicoin .
docker run -d --name avicoin \
  -p 34352:34352 -p 8080:8080 \
  -v avicoin-data:/avicoin/data \
  avicoin --data-dir=/avicoin/data \
          --host-address=0.0.0.0:34352 \
          --api-address=0.0.0.0:8080 \
          --mine
```

Then open <http://localhost:8080>.

The image is two stages: one that compiles a release binary, one that runs it.
The runtime layer carries **the binary and nothing else** — the viewer is
compiled in with `include_str!`, so there are no assets to lose beside it, and
there is no toolchain in the image that ships.

It runs as `avicoin`, not root. Nothing here needs root, and a container that
takes it anyway is one that can be surprising later.

**No `config.toml` is baked in.** Configuration arrives as arguments, so the
image does not become a fourth layer that
[CLAUDE.md's precedence](../CLAUDE.md#configuration-resolution) knows nothing
about.

### The data directory is a volume

`/avicoin/data`, so recreating a container keeps the chain, the UTXO set and
the wallet key. **One node per directory** — the node takes an advisory lock on
it, so a second container pointed at the same volume exits rather than
corrupting anything.

### The healthcheck

```
HEALTHCHECK CMD avicoin health --api-address 127.0.0.1:8080
```

Up is not the same as working: a node whose miner has wedged, or which has lost
every peer, answers `/status` perfectly well and is doing nothing. So the check
asks whether the **tip has moved** since it last looked, and calls the node
unhealthy once it has stood still for `--stall-seconds` (120 by default).

The memory lives in the container, at `/tmp/avicoin-health`. A restarted
container starts the clock again rather than inheriting a verdict about the
node it replaced, and nothing but the node writes to the node's directory.

## A local network

```bash
docker compose up
```

Three nodes on one host: a miner on `:8080`, one told about it on `:8081`, and
one told only about *that* one on `:8082` — so the third finds the miner
through discovery, which is the half of the network that would otherwise go
untried here. Each has its own volume and its own ports.

This is the development network. Note what it is not: the functional suite
drives **processes**, not containers, so nothing in `test/` needs Docker.

## Without a container

```bash
cargo run -- --api-address 127.0.0.1:8080 --mine
```

`config.toml` is optional and so is every field in it; CLI arguments override
it. The defaults put the data directory in `.avicoin` under your home.
