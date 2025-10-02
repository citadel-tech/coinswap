# Shared builder stage for coinswap project using Alpine
FROM alpine:3.18 AS builder

# Install system dependencies and rustup
RUN --mount=type=cache,target=/var/cache/apk \
    apk add --no-cache \
    curl \
    git \
    pkgconfig \
    openssl-dev \
    musl-dev \
    build-base

# Install Rust via rustup to get the correct version
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.75.0
ENV PATH="/root/.cargo/bin:$PATH"

# Set working directory
WORKDIR /app

# Copy only dependency manifests first for better caching
COPY Cargo.toml rust-toolchain.toml rustfmt.toml README.md ./

# Create a dummy src/main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies first (this layer will be cached)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release && rm src/main.rs

# Now copy the actual source code and tests
COPY src/ ./src/
COPY tests/ ./tests/

# Build the project with all binaries
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --bins && \
    mkdir -p /tmp/binaries && \
    cp target/release/makerd target/release/maker-cli target/release/taker /tmp/binaries/

# Clone and build the tracker
WORKDIR /tmp
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    git clone https://github.com/citadel-tech/tracker.git && \
    cd tracker && \
    cargo build --release && \
    cp target/release/tracker /tmp/binaries/