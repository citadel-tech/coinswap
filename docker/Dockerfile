# Coinswap Dockerfile
# Contains makerd, maker-cli, and taker

## --- Builder Stage ---
FROM rust:1.90-alpine3.20 AS builder

RUN apk add --no-cache \
    build-base \
    protobuf \
    cmake \
    git \
    curl \
    pkgconfig \
    openssl-dev \
    sqlite-dev \
    sqlite-static \
    zeromq-dev \
    ca-certificates \
    openssl-libs-static \
    libgcc \
    musl-dev

WORKDIR /usr/src/coinswap
COPY . .
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release

## --- Runtime Stage ---
FROM alpine:3.20

RUN --mount=type=cache,target=/var/cache/apk \
    apk add --no-cache \
    build-base \
    protobuf \
    cmake \
    git \
    curl \
    pkgconfig \
    openssl-dev \
    sqlite-dev \
    sqlite-static \
    zeromq-dev \
    ca-certificates \
    openssl-libs-static \
    libgcc \
    musl-dev

RUN adduser -D -u 1001 coinswap

RUN mkdir -p /app/bin /home/coinswap/.coinswap && \
    chown -R coinswap:coinswap /app /home/coinswap

COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/makerd /app/bin/
COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/maker-cli /app/bin/
COPY --from=builder --chown=coinswap:coinswap /usr/src/coinswap/target/release/taker /app/bin/

RUN chmod +x /app/bin/*

USER coinswap
WORKDIR /app

ENV PATH="/app/bin:$PATH"

ENV COINSWAP_DATA_DIR="/home/coinswap/.coinswap"



VOLUME ["/home/coinswap/.coinswap"]

CMD ["makerd"]
