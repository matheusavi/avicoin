# Two stages: one that compiles, one that runs. The runtime image carries the
# binary and nothing else — the viewer is compiled into it with `include_str!`,
# so there are no assets to lose beside it.
FROM rust:1.98-slim-bookworm AS build

WORKDIR /src
# The manifests first, so a change to the source does not re-fetch the index.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

COPY params ./params
COPY src ./src
# Cargo does not notice a source file whose mtime is older than the stub's.
RUN touch src/main.rs && cargo build --release


FROM debian:bookworm-slim

# Nothing here needs to be root, and a container that runs as root for no
# reason is a container that can be surprising later.
RUN useradd --system --create-home --home-dir /avicoin --shell /usr/sbin/nologin avicoin
COPY --from=build /src/target/release/avicoin /usr/local/bin/avicoin

# Made and owned *before* the VOLUME. Docker seeds a fresh anonymous volume
# from the image's directory, ownership included; declaring the volume over a
# root-owned mount point leaves a node that cannot take its own lock.
RUN mkdir -p /avicoin/data && chown avicoin:avicoin /avicoin/data

USER avicoin
WORKDIR /avicoin

# The data directory is a volume, so recreating a container keeps the chain.
# One node per directory: the node takes an advisory lock on it.
VOLUME ["/avicoin/data"]
ENV AVICOIN_DATA=/avicoin/data

EXPOSE 34352 8080

# Up is not the same as working. This asks the node's own API whether the tip
# has moved, which a wedged miner would fail and a running process would not.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["avicoin", "health", "--api-address", "127.0.0.1:8080"]

# No `config.toml` is baked in: configuration arrives as arguments, so the
# image does not become a fourth layer the precedence in CLAUDE.md knows
# nothing about.
ENTRYPOINT ["avicoin"]
CMD ["--data-dir", "/avicoin/data", "--host-address", "0.0.0.0:34352", "--api-address", "0.0.0.0:8080"]
