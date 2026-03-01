<div align="center">

<h1>Coinswap</h1>

<p>
    Functioning, minimal-viable binaries and libraries to perform a trustless, p2p <a href="https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964">Maxwell-Belcher Coinswap Protocol</a>.
  </p>

<p>
    <a href="https://github.com/citadel-tech/coinswap/blob/master/LICENSE"><img alt="MIT or Apache-2.0 Licensed" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg"/></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml"><img alt="Build Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml"><img alt="Lint Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml"><img alt="Test Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml/badge.svg"></a>
    <a href="https://codecov.io/github/citadel-tech/coinswap?branch=master">
    <img alt="Coverage" src="https://codecov.io/github/citadel-tech/coinswap/coverage.svg?branch=master">
    </a>
    <a href="https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html"><img alt="Rustc Version 1.75.0+" src="https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg"/></a>
  </p>
</div>

## 🚨 Announcements
 - **Coinswap Protocol V2 with Taproot-Musig2 is NOW LIVE!** : [See Specifications](https://github.com/citadel-tech/Coinswap-Protocol-Specification/tree/main/v2)
  
 - **Public Coinswap Marketplace Available On** [Mutinynet](https://github.com/benthecarman/bitcoin/tree/mutinynet-inq-29).
   - **Block Explorer:** [Mutinynet's mempool.space](https://mutinynet.com/mining)
   - **Faucet:** [Mutinynet's Faucet](https://faucet.mutinynet.com/)

- **GUI Coinswap Client available for testing**: [Taker App](https://github.com/citadel-tech/taker-app)

- **One-Click Deployment for Coinswap maker server available here**: [Coinswap Docker](./docs/docker.md)  

## ⚠️ Warning
This library is currently under development and is in an experimental stage. **Mainnet use is NOT recommended.**

# About

Coinswap is a trustless, self-custodial [atomic swap](https://bitcoinops.org/en/topics/coinswap/) protocol built on Bitcoin. Unlike existing solutions that rely on centralized servers as [single points of failure](https://en.wikipedia.org/wiki/Single_point_of_failure), Coinswap's marketplace is seeded in the Bitcoin blockchain itself — no central host required, anyone with a Bitcoin node can participate.

**Sybil resistance** is achieved through [Fidelity Bonds](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md): time-locked UTXOs that make Sybil attacks economically costly while simultaneously bootstrapping the marketplace on-chain.

**Two roles:**

- **`Makers`** are swap service providers. They earn fees (mining + swap) for supplying liquidity, compete on fee rates in an open market, and signal reliability through larger fidelity bonds. Unlike Lightning nodes, maker servers need no active management — they run in *install-fund-forget* mode on any consumer hardware (Umbrel, Start9, Mynode, etc.), though liquidity must remain in the node's hot wallet.

- **`Takers`** are clients initiating swaps. They pay all fees, require no fidelity bond, and select makers based on bond validity, available liquidity, fee rates, and past swap history.

**Multi-hop routing** mirrors Lightning: swaps are routed through multiple makers, and no single maker sees the full route. The taker relays all messages between makers over Tor, keeping each maker's view partial. Protocol complexity lives entirely on the taker side, keeping maker servers lightweight.

The project extends Chris Belcher's [teleport-transactions](https://github.com/bitcoin-teleport/teleport-transactions) proof-of-concept into a production-grade implementation with full protocol handling, functional testing, sybil resistance, CLI tools, and a GUI app. The same protocol can be extended for cross-chain swaps.

For protocol-level details, see the [Coinswap Protocol Specifications](https://github.com/citadel-tech/Coinswap-Protocol-Specification).

# Components

## Crate Binaries
The crate compiles into following binaries.

**`makerd`**: Coinswap maker server daemon for swap providers. Requires continuous uptime and Bitcoin Core RPC connection. [Demo](./docs/makerd.md)

**`maker-cli`**: RPC controller for the `makerd`. Manage server, access wallet, view swap statistics. [Demo](./docs/maker-cli.md)

**`taker`**: A command-line client to perform Coinswaps. [Demo](./docs/taker.md)

## Dockers

**Coinswap Docker**: A complete coinswap stack with pre configured, bitcoind, tor, makerd, maker-cli and taker apps. Useful for one-click-setup. [See the guide](./docs/docker.md) 

## Apps
**`taker-app(beta)`**: a GUI swap client to perform Coinswaps. [Build from source](https://github.com/citadel-tech/taker-app) 

# Setup & Installation

## Dependencies

Following dependencies are needed to compile the crate.

```shell
sudo apt install build-essential automake libtool protobuf-compiler
```
To run all the coinswap apps the two more systems are needed.

**Bitcoin**: A fully synced bitcoin node with required configurations. Follow the [bitcoind setup guide](./docs/bitcoind.md) for full details.

**Tor**: Tor is Required for all network communications. Download from torproject.org for your OS. Bitcoin Core automatically detects Tor and creates anonymous services. See the [Tor guide](./docs/tor.md) for configuration details.

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

The [Test Framework](./tests/test_framework/mod.rs) spawns toy marketplaces in Bitcoin regtest to test swap scenarios. Each test in [tests](./tests/) covers different edge cases. Start with [standard_swap](./tests/standard_swap.rs) to understand programmatic simulation.

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

Dev community: [Matrix](https://matrix.to/#/#citadel-foss:matrix.org)

Dev discussions predominantly happen via FOSS best practices, and by using Github as the major community forum.

The Issues, PRs and Discussions are where all the hard lifting is happening.

