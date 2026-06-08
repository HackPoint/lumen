FROM rust:1.88-slim-bookworm

# C toolchain (needed by tree-sitter grammar crates and rusqlite)
# SQLite dev headers (for rusqlite)
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libsqlite3-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Node.js 22 + pnpm (for lumenator Angular build checks in CI)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*
RUN npm install -g pnpm@10

# Rust components
RUN rustup component add rustfmt clippy

# Pre-warm the registry index so first cargo build is faster
RUN cargo search --limit 0 || true
