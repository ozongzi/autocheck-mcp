FROM rust:latest AS base

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends --fix-missing \
    ca-certificates curl git unzip xz-utils pkg-config libssl-dev && \
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

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo fetch

COPY src ./src
RUN cargo build --release

# ── final image ───────────────────────────────────────────────────────────────
FROM base AS final

COPY --from=builder /build/target/release/autocheck-mcp /usr/local/bin/autocheck-mcp

WORKDIR /workspace
ENTRYPOINT ["autocheck-mcp"]
