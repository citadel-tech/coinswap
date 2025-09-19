# This Dockerfile creates a builder environment and a common base runtime image.

## --- Builder Stage ---
# Compiles all necessary binaries.
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

## --- Base Runtime Stage ---
# This is the common base for all service images.
FROM alpine:3.20

# Install common runtime dependencies
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

# Switch to app user
USER coinswap
WORKDIR /app

# Add binaries to PATH
ENV PATH="/app/bin:$PATH"

# Set common environment variables
ENV COINSWAP_DATA_DIR="/home/coinswap/.coinswap"

# Create volume mount points
VOLUME ["/home/coinswap/.coinswap"]