# Coinswap Dockerfile
# Contains makerd, maker-cli, and taker

## --- Builder Stage ---
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

# Build coinswap binaries
WORKDIR /usr/src/coinswap
COPY . .
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release

## --- Runtime Stage ---
FROM alpine:3.20

# Install runtime dependencies
RUN --mount=type=cache,target=/var/cache/apk \
    apk add --no-cache \
    ca-certificates \
    openssl \
    libgcc \
    sqlite \
    zeromq

# Create app user
RUN adduser -D -u 1001 coinswap

# Create common directories
RUN mkdir -p /app/bin /home/coinswap/.coinswap && \
    chown -R coinswap:coinswap /app /home/coinswap

# Copy binaries
COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/makerd /app/bin/
COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/maker-cli /app/bin/
COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/taker /app/bin/

# Set permissions
RUN chmod +x /app/bin/*

# Switch to app user
USER coinswap
WORKDIR /app

# Add binaries to PATH
ENV PATH="/app/bin:$PATH"

# Set environment variables
ENV COINSWAP_DATA_DIR="/home/coinswap/.coinswap"

# Expose ports (Maker ports)
EXPOSE 6102 6103

# Volumes
VOLUME ["/home/coinswap/.coinswap"]

# Default command
CMD ["makerd"]
