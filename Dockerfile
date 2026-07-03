# Multi-stage Dockerfile for reproducible, minimal builds.
# Stage 1 compiles the Rust binary; stage 2 ships only the binary — no source, no
# toolchain, no secrets.
#
# Build: docker build -t kestrel:latest .
# Run (bare-metal host, sharing the local Reth IPC socket):
#   docker run --env-file .env \
#     -v /var/run/reth:/var/run/reth \
#     --network host \
#     kestrel:latest
# The bot connects to Reth over the mounted IPC socket (RETH_IPC_PATH); --network host
# lets it reach the loopback-bound metrics/control ports. On a native (non-Docker)
# deployment the binary talks to the IPC socket directly with no mounts needed.

# ── Stage 1: Build ─────────────────────────────────────────────────────────────
FROM rust:1.82-slim AS builder

# Install build dependencies for rusqlite (bundled SQLite) and OpenSSL (reqwest)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the workspace manifests first so layer cache is warm on dep-only rebuilds
COPY bot/Cargo.toml ./Cargo.toml
COPY bot/kestrel/Cargo.toml ./kestrel/Cargo.toml

# Create a dummy main to pre-cache dependency compilation
RUN mkdir -p kestrel/src && echo 'fn main(){}' > kestrel/src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf kestrel/src

# Now copy real source and build for real
COPY bot/kestrel/src ./kestrel/src

# Touch main.rs so cargo detects the source changed
RUN touch kestrel/src/main.rs
RUN cargo build --release

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# Install CA certs (needed by reqwest for HTTPS to builders/Flashbots) and
# libssl (dynamically linked by reqwest unless using vendored-openssl feature).
# No Rust toolchain, no source code, no build artefacts.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy only the compiled binary — nothing else from the build stage
COPY --from=builder /build/target/release/kestrel /usr/local/bin/kestrel

# Non-root user for defence-in-depth
RUN useradd --system --no-create-home kestrel
USER kestrel

# Prometheus metrics port (internal — not exposed to public)
EXPOSE 9100
# Bot control WebSocket (internal only)
EXPOSE 9102

ENTRYPOINT ["/usr/local/bin/kestrel"]
