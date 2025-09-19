# Multi-stage build for coinswap project using Alpine
FROM alpine:3.18 AS builder

# Install system dependencies and rustup
RUN --mount=type=cache,target=/var/cache/apk \
    apk add --no-cache \
    curl \
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

# Now copy the actual source code
COPY src/ ./src/

# Build the project with all binaries (source changes won't rebuild deps)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --bins && \
    cp target/release/makerd target/release/maker-cli target/release/taker \
       target/release/directoryd target/release/directory-cli /tmp/

# Runtime stage - minimal Alpine
FROM alpine:3.18

# Install runtime dependencies with cache
RUN --mount=type=cache,target=/var/cache/apk \
    apk add --no-cache \
    ca-certificates \
    curl \
    wget \
    tor \
    openssl \
    libgcc

# Download and install Bitcoin Core with cache
RUN --mount=type=cache,target=/tmp/bitcoin-cache \
    if [ ! -f /tmp/bitcoin-cache/bitcoin-26.0-x86_64-linux-gnu.tar.gz ]; then \
        wget -q https://bitcoincore.org/bin/bitcoin-core-26.0/bitcoin-26.0-x86_64-linux-gnu.tar.gz \
        -O /tmp/bitcoin-cache/bitcoin-26.0-x86_64-linux-gnu.tar.gz; \
    fi && \
    tar -xzf /tmp/bitcoin-cache/bitcoin-26.0-x86_64-linux-gnu.tar.gz && \
    cp bitcoin-26.0/bin/* /usr/local/bin/ && \
    rm -rf bitcoin-26.0

# Create app user
RUN adduser -D -u 1001 coinswap

# Create directories with proper structure
RUN mkdir -p /app/bin /app/data /home/coinswap/.coinswap/maker /home/coinswap/.coinswap/taker /home/coinswap/.bitcoin && \
    chown -R coinswap:coinswap /app /home/coinswap

# Copy binaries from builder
COPY --from=builder /tmp/makerd /app/bin/
COPY --from=builder /tmp/maker-cli /app/bin/
COPY --from=builder /tmp/taker /app/bin/
COPY --from=builder /tmp/directoryd /app/bin/
COPY --from=builder /tmp/directory-cli /app/bin/

# Set proper permissions
RUN chmod +x /app/bin/*

# Switch to app user
USER coinswap
WORKDIR /app

# Add binaries to PATH
ENV PATH="/app/bin:$PATH"

# Set environment variables for data directories
ENV COINSWAP_DATA_DIR="/home/coinswap/.coinswap"
ENV BITCOIN_DATA_DIR="/home/coinswap/.bitcoin"

# Expose ports (adjust as needed for your services)
EXPOSE 6102 6103 8080 9050 18332

# Create volume mount points
VOLUME ["/home/coinswap/.coinswap", "/home/coinswap/.bitcoin"]

# Health check to ensure binaries are working
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD makerd --help > /dev/null 2>&1 || exit 1

# Default command
CMD ["sh", "-c", "echo 'Coinswap Docker Container Ready!' && echo 'Available commands: makerd, maker-cli, taker, directoryd, directory-cli, bitcoind, tor' && echo 'Example: docker run coinswap makerd --help' && /bin/sh"]