# Build context: web/ (parent of autocheck-mcp/)
# docker buildx build --platform linux/amd64 -f autocheck-mcp/Dockerfile -t autocheck-mcp:latest .

FROM docker.m.daocloud.io/library/rust:1 AS base

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

RUN rustup component add clippy

# ── Bun ───────────────────────────────────────────────────────────────────────
ENV BUN_INSTALL=/usr/local/bun \
    PATH=/usr/local/bun/bin:$PATH
RUN curl -fsSL https://bun.sh/install | bash

# ── uv ────────────────────────────────────────────────────────────────────────
RUN curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh

# ── build autocheck-mcp ───────────────────────────────────────────────────────
FROM base AS builder
WORKDIR /build

# Copy agentix path dependency first (for layer caching)
COPY agentix/agentix           /build/agentix/agentix
COPY agentix/agentix-macros    /build/agentix/agentix-macros
COPY agentix/Cargo.toml        /build/agentix/Cargo.toml

# Copy autocheck-mcp source
COPY autocheck-mcp/Cargo.toml autocheck-mcp/Cargo.lock ./
# Patch path dependency to point to /build/agentix/agentix
RUN sed -i 's|path = "\.\./agentix/agentix"|path = "/build/agentix/agentix"|g' Cargo.toml

RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo fetch

COPY autocheck-mcp/src ./src
RUN cargo build --release

# ── final image ───────────────────────────────────────────────────────────────
FROM base AS final

COPY --from=builder /build/target/release/autocheck-mcp /usr/local/bin/autocheck-mcp

WORKDIR /workspace
ENTRYPOINT ["autocheck-mcp"]
