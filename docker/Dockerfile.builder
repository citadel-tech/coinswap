## Multi-stage build for coinswap project using Alpine
FROM rust:1.90-alpine3.20 AS builder

# Install system dependencies
RUN apk add --no-cache \
    build-base \
    cmake \
    git \
    curl \
    pkgconfig \
    openssl-dev \
    sqlite-dev \
    sqlite-static \
    zeromq-dev

# Create non-root user
RUN addgroup -g 1000 coinswap && \
    adduser -D -u 1000 -G coinswap coinswap

# Set working directory
WORKDIR /tmp

# Create directory for compiled binaries
RUN mkdir -p /tmp/binaries

# Build coinswap
COPY . /tmp/coinswap
WORKDIR /tmp/coinswap

# Build the main coinswap project
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release && \
    cp target/release/makerd /tmp/binaries/ && \
    cp target/release/taker /tmp/binaries/ && \
    cp target/release/maker-cli /tmp/binaries/

# Build tracker from external repository
WORKDIR /tmp
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    git clone https://github.com/citadel-tech/tracker.git && \
    cd tracker && \
    cargo build --release && \
    cp target/release/tracker /tmp/binaries/

# Runtime stage
FROM alpine:3.20

# Install runtime dependencies
RUN apk add --no-cache \
    libgcc \
    openssl \
    sqlite \
    zeromq

# Create non-root user
RUN addgroup -g 1000 coinswap && \
    adduser -D -u 1000 -G coinswap coinswap

# Copy binaries from builder stage
COPY --from=builder /tmp/binaries/* /usr/local/bin/

# Create data directories
RUN mkdir -p /home/coinswap/.coinswap && \
    chown -R coinswap:coinswap /home/coinswap

# Switch to non-root user
USER coinswap
WORKDIR /home/coinswap

# Default command
CMD ["/bin/sh"]