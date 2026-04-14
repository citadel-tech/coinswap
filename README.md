<div align="center">

# Coinswap
Functioning, minimal-viable binaries and libraries to perform a trustless, p2p [Maxwell-Belcher Coinswap Protocol](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964).

<div>

[![MIT or Apache-2.0 Licensed](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/citadel-tech/coinswap/blob/master/LICENSE)
[![Build Status](https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml/badge.svg)](https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml)
[![Lint Status](https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml/badge.svg)](https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml)
[![Test Status](https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml/badge.svg)](https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml)
[![Coverage](https://codecov.io/github/citadel-tech/coinswap/coverage.svg?branch=master)](https://codecov.io/github/citadel-tech/coinswap?branch=master)
[![Rustc Version 1.75.0+](https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg)](https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html)

</div>

</div>

## 🚨 Announcement

**Coinswap `v0.2.1` is now released.**

See the [full `v0.2.1` release notes](https://github.com/citadel-tech/coinswap/releases/tag/v0.2.1) and the [core changelog](https://github.com/citadel-tech/coinswap/compare/v0.2.0...v0.2.1).

### What's New in `v0.2.1`

- Unified Legacy and Taproot protocol support in the core library, making downstream applications easier to maintain and extend.
- [Maker Dashboard](https://github.com/citadel-tech/maker-dashboard)
- [Taker App](https://github.com/citadel-tech/taker-app)
- [Coinswap Website](https://citadel-tech.github.io/website/)

## Previous

Earlier project announcements are retained here for reference:

| Announcement | Reference |
| --- | --- |
| Coinswap Protocol V2 with Taproot-MuSig2 is live | [See specifications](https://github.com/citadel-tech/Coinswap-Protocol-Specification/tree/main/v2%20protocol) |
| Public Coinswap marketplace is available on Mutinynet | [Mutinynet branch](https://github.com/benthecarman/bitcoin/tree/mutinynet-inq-29), [block explorer](https://mutinynet.com/mining), [faucet](https://faucet.mutinynet.com/) |
| One-click deployment for a Coinswap maker server is available | [Coinswap Docker](./docs/docker.md) |

## ⚠️ Warning
This library is currently under development and is in an experimental stage. **Mainnet use is NOT recommended.**

# About

Coinswap is a trustless, self-custodial [atomic swap](https://bitcoinops.org/en/topics/coinswap/) protocol built on Bitcoin. Unlike existing solutions that rely on centralized servers as [single points of failure](https://en.wikipedia.org/wiki/Single_point_of_failure), Coinswap's marketplace is seeded in the Bitcoin blockchain itself — no central host required, anyone with a Bitcoin node can participate. 

For a quicker dive into the idea, see the [**Website**](https://citadel-tech.github.io/website/).

**Sybil resistance** is achieved through [Fidelity Bonds](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md): time-locked UTXOs that make Sybil attacks economically costly while simultaneously bootstrapping the marketplace on-chain.

**Two roles:**

- **`Makers`** are swap service providers. They earn swap fees for supplying liquidity, compete on fee rates in an open market, and signal reliability through larger fidelity bonds. Unlike Lightning nodes, maker servers need no active management — they run in *install-fund-forget* mode on any consumer hardware (Umbrel, Start9, Mynode, etc.), though liquidity must remain in the node's hot wallet to serve swap requests.

- **`Takers`** are clients initiating swaps. They pay all the fees (swap + mining), require no fidelity bond, and select makers based on bond validity, available liquidity, fee rates, and past swap history.

**Multi-hop routing** mirrors Lightning: swaps are routed through multiple makers, and no single maker sees the full route. The taker relays all messages between makers over Tor, keeping each maker's view partial. Protocol complexity lives entirely on the taker side, keeping maker servers lightweight.

The project extends Chris Belcher's [teleport-transactions](https://github.com/bitcoin-teleport/teleport-transactions) proof-of-concept into a production-grade implementation with full protocol handling, functional testing, sybil resistance, CLI tools, and a GUI app. The same protocol can be extended for cross-chain swaps.

For protocol-level details, see the [Coinswap Protocol Specifications](https://github.com/citadel-tech/Coinswap-Protocol-Specification).

For an in-depth exploration of the repository, it's recommended to use [Deep Wiki](https://deepwiki.com/citadel-tech/coinswap).

# Components

## Crate Binaries
The crate compiles into the following binaries.

**`makerd`**: Coinswap maker server daemon for swap providers. Requires continuous uptime and Bitcoin Core RPC connection. [Demo](./docs/makerd.md)

**`maker-cli`**: RPC controller for the `makerd`. Manage server, access wallet, view swap statistics. [Demo](./docs/maker-cli.md)

**`taker`**: A command-line client to perform Coinswaps. [Demo](./docs/taker.md)

## Dockers

**Coinswap Docker**: A complete coinswap stack with pre-configured bitcoind, tor, makerd, maker-cli and taker apps. Useful for one-click-setup. [See the guide](./docs/docker.md)

## Apps
**`taker-app(beta)`**: a GUI swap client to perform Coinswaps. [Build from source](https://github.com/citadel-tech/taker-app) 

# Setup & Installation

## Dependencies

Following dependencies are needed to compile the crate.

```shell
sudo apt install build-essential automake libtool protobuf-compiler
```
To run all the coinswap apps, two more systems are needed:

**Bitcoin**: A fully synced bitcoin node with required configurations. Follow the [bitcoind setup guide](./docs/bitcoind.md) for full details.

**Tor**: Tor is required for all network communications. Download from torproject.org for your OS. Bitcoin Core automatically detects Tor and creates anonymous services. See the [Tor guide](./docs/tor.md) for configuration details.

## Build and Install

### Option 1: Build from Source

```console
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap
cargo build --release
```

Install the necessary binaries in your system:

```console
sudo install ./target/release/taker /usr/local/bin/
sudo install ./target/release/makerd /usr/local/bin/  
sudo install ./target/release/maker-cli /usr/local/bin/  
```

### Option 2: Using Docker

We provide a helper script to easily configure, build, and run the Coinswap stack (including Bitcoin Core and Tor).

```console
git clone https://github.com/citadel-tech/coinswap.git
cd coinswap

# Configure, build, and start
./docker-setup configure
./docker-setup start
```

For advanced usage, manual commands, and architecture details, refer to the [Docker Documentation](./docs/docker.md).

## Verify Setup

### Native Installation

```console
makerd --help
maker-cli --help
taker --help

# Test connection to market
taker fetch-offers
```

### Docker Installation

```console
# Check binaries
./docker-setup taker --help
./docker-setup maker-cli --help

# Test connection to market
./docker-setup taker fetch-offers
```

# Development

## Testing

Extensive functional testing simulates various protocol edge cases:

```console
cargo test --features=integration-test -- --nocapture
```

The [Test Framework](./tests/integration/test_framework/mod.rs) spawns toy marketplaces in Bitcoin regtest to test swap scenarios. Each test in [tests/integration](./tests/integration/) covers different edge cases. Start with [standard_swap](./tests/integration/standard_swap.rs) to understand programmatic simulation.

## Contributing

- Browse [issues](https://github.com/citadel-tech/coinswap/issues), especially [`good first issue`](https://github.com/citadel-tech/coinswap/issues?q=is%3Aopen+is%3Aissue+label%3A%22good+first+issue%22)
- Review [open PRs](https://github.com/citadel-tech/coinswap/pulls)
- Search for `TODO`s in the codebase
- Read the [docs](./docs)
- Read the [Contributing Guide](./CONTRIBUTING.md) — including the AI contributions policy

### Git Hooks

The repo contains pre-commit githooks to do auto-linting before commits. Set up the pre-commit hook by running:

```bash
ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit
```

## Community

Dev community: [Matrix](https://matrix.to/#/#ciatdel-foss:matrix.org)

Dev discussions predominantly happen via FOSS best practices, and by using Github as the major community forum.

The Issues, PRs and Discussions are where all the hard lifting is happening.
