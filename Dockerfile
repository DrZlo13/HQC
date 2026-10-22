# hqc-hub image: the hub binary plus the built web client (docs/RUNBOOK.md §1.8).
# Multi-platform: no architecture is hard-coded; CI builds linux/amd64.

# --- hub binary -------------------------------------------------------------
# rust:1 tracks the latest stable, like rust-toolchain.toml (channel = stable). The toolchain
# file itself is not copied in: the image's toolchain is not named "stable" and rustup would
# download another one.
FROM rust:1-slim-trixie AS hub
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Dependencies first, against stub workspace crates, so this layer is reused (locally and
# through the GitHub Actions layer cache) until Cargo.toml or Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
COPY crates/hqc-proto/Cargo.toml crates/hqc-proto/
COPY crates/hqc-claude-jsonl/Cargo.toml crates/hqc-claude-jsonl/
COPY crates/hqc-hub/Cargo.toml crates/hqc-hub/
COPY crates/hqc-worker/Cargo.toml crates/hqc-worker/
RUN for c in crates/*/; do mkdir -p "$c/src" && touch "$c/src/lib.rs"; done \
    && for c in hqc-hub hqc-worker; do echo 'fn main() {}' > "crates/$c/src/main.rs"; done \
    && cargo build --release --locked -p hqc-hub \
    && rm -rf crates

COPY crates crates
# COPY keeps the context's mtimes, which may be older than the stub build: make cargo see
# the real sources as changed.
RUN find crates -name '*.rs' -exec touch {} + \
    && cargo build --release --locked -p hqc-hub \
    && target/release/hqc-hub --version

# --- web client ---------------------------------------------------------------
# Static files only, so it runs on the build host's platform even when cross-building.
FROM --platform=$BUILDPLATFORM node:22-trixie-slim AS client
WORKDIR /src/client
COPY client/package.json client/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY client ./
RUN npm run build

# --- runtime ------------------------------------------------------------------
FROM debian:trixie-slim
# ca-certificates: JWKS from <team>.cloudflareaccess.com and push services over HTTPS;
# libssl3t64: web-push encrypts payloads with OpenSSL.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 hqc \
    && useradd --uid 10001 --gid hqc --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin hqc

COPY --from=hub /src/target/release/hqc-hub /usr/local/bin/hqc-hub
COPY --from=client /src/client/dist /opt/hqc/client

ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="hqc-hub" \
      org.opencontainers.image.description="hqc hub: state, routing, client and worker APIs" \
      org.opencontainers.image.source="https://github.com/DrZlo13/hqc" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.revision="$REVISION"

# 0.0.0.0 is the container's own interface: safe only while the port is not published to
# the outside (RUNBOOK §1.8). HQC_DEV=1 still refuses a non-loopback listen address.
# NO_COLOR: plain log lines in `docker logs` and journald.
ENV HQC_HUB_LISTEN=0.0.0.0:8080 \
    HQC_CLIENT_DIR=/opt/hqc/client \
    RUST_LOG=info \
    NO_COLOR=1
USER hqc:hqc
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/hqc-hub"]
