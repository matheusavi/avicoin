# The public node

What runs behind the URL in the [README](../README.md). One container, a
persistent volume, and a proxy that terminates TLS.

```bash
# On the host, with Docker and the compose plugin installed:
git clone https://github.com/matheusavi/avicoin
cd avicoin/deploy
sed -i 's/avicoin.example.com/<your host name>/' Caddyfile
docker compose up -d
```

That is the whole deployment. `restart: unless-stopped` is what makes it
survive the host's reboots, and the named volume is what makes it come back at
its real tip rather than at genesis.

## What is deliberately not here

**TLS and rate limiting are the proxy's**, and the proxy is the host's choice.
[ADR-0001](../docs/adr/0001-v1-scope.md) puts both out of scope, and a
half-measure written here would imply a security property this project does not
have. Caddy is one answer because it gets a certificate without being told how;
any reverse proxy does.

**The API port is not published.** The proxy reaches it over the compose
network. The P2P port is published, because the point is that anyone can join.

**There is nothing to authorise.** The API cannot sign and cannot spend — see
[docs/api.md](../docs/api.md) — so the proxy is protecting the node's *time*,
not its coins. Spending happens with `avicoin send` on the machine that holds
the key, which is never this one unless you put it there.

## Joining it

```bash
avicoin --addresses-to-connect <host>:34352 --api-address 127.0.0.1:8080
```

Then open <http://localhost:8080> and watch your own node catch up. Add
`--mine` to mine against the public chain; difficulty adapts to whatever
hashrate arrives, which is the thing that is easier to see than to be told.
