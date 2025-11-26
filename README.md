<div align="center">

<h1>Coinswap</h1>

<p>
    Functioning, minimal-viable binaries and libraries to perform a trustless, p2p <a href="https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964">Maxwell-Belcher Coinswap Protocol</a>.
  </p>

<p>
    <a href="https://github.com/citadel-tech/coinswap/blob/master/LICENSE"><img alt="MIT or Apache-2.0 Licensed" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg"/></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/build.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/lint.yaml/badge.svg"></a>
    <a href="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml"><img alt="CI Status" src="https://github.com/citadel-tech/coinswap/actions/workflows/test.yaml/badge.svg"></a>
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
   - **Faucet:** [Mutinynet's Facuet](https://faucet.mutinynet.com/)

- **GUI Coinswap Client available for testing**: [Taker App](https://github.com/citadel-tech/taker-app)

- **One-Click Deployment for Coinswap maker server on Mutinynet available here**: [Coinswap Docker](./docs/docker.md)  

## ⚠️ Warning
This library is currently under development and is in an experimental stage. **Mainnet use is NOT recommended.**

# About

Coinswap is an [atomic swap](https://bitcoinops.org/en/topics/coinswap/) protocol that allows trustless atomic-swaps through a decentralized, sybil-resistant marketplace.

Existing swap solutions are centralized, rely on large swap servers, and have service providers as [single points of failure](https://en.wikipedia.org/wiki/Single_point_of_failure). This project aims at providing a completely trustless, self-custodial, and decentralized atomic-swap protocol, via a marketplace seeded in the Bitcoin blockchain itself. The protocol uses [Fidelity Bonds](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md) to both attain sybil-resistance as well as seeding the marketplace using bitcoin transactions, enhancing censorship-resistance massively. The market doesn't need to be hosted on any particular server. Anyone with a bitcoin node can participate in the market.

The market consists of two actors: the `Makers` and the `Takers`. The `Makers` are *swap service providers*. They earn fees from swaps for providing liquidity. Individual makers can adjust their own fee rates, and open-market competition will ensure fee pressure is kept at the lowest. Individual makers can advertise for better reliability by putting in larger fidelity bonds to attract more swaps from the market.

The `Takers` are the clients, looking for swap services. The takers pay for all the fees, including both mining fees and maker swap fees. The takers don't need fidelity bonds to participate in the market. Takers choose the makers from the market based on their valid fidelity, available liquidity, offered fee rates, and past memory of successful swaps.

Anyone can become a taker or a maker or both. They only need a running bitcoin full node with [specific configurations](/docs/demo.md#create-configuration-file) and enough liquidity to put for the swaps. Running the maker servers is a lucrative way for node-runners to put their nodes to work and earn sats. Unlike lightning nodes, the swap servers do not require active management. It works in an *install-fund-forget* mode, making it much simpler to run it in headless full node systems, like Umbrel, Start9, Mynode, etc. Although the liquidity for fidelity bonds and swap services has to be kept in the node's hot wallet.

Similar to lightning, the swaps can be routed through multiple makers. No single maker in a swap route knows about other makers involved in the route. During a swap, the taker coordinates all messages between makers, reducing the visibility of each individual maker on the complete swap route. All communication occurs over Tor. The taker acts as a relay between makers, and the makers act as just a dumb server, responding to taker queries, providing liquidity, and earning fees. All protocol-level complexities are handled at the client side by the takers. This design allows keeping the maker servers very lightweight, which can be run on any consumer-grade hardware.

The project builds on Chris Belcher's proof-of-concept of [teleport-transactions](https://github.com/bitcoin-teleport/teleport-transactions) and has matured with complete protocol handling, functional testing, sybil resistance, command-line applications, and GUI Apps.

The same protocol can also be extended to support other coins, creating an open decentralized cross-chain swap marketplace.

For all protocol-level details, see the [Coinswap Protocol Specifications](https://github.com/citadel-tech/Coinswap-Protocol-Specification).

# Components

## Crate Binaries
The crate compiles into following binaries.

**`makerd`**: Coinswap maker server daemon for swap providers. Requires continuous uptime and Bitcoin Core RPC connection. [Demo](./docs/makerd.md)

**`maker-cli`**: RPC controller for the `makerd`. Manage server, access wallet, view swap statistics. [Demo](./docs/maker-cli.md)

**`taker`**: A command-line client to perform Coinswaps. [Demo](./docs/taker.md)

## Dockers

**Coinswap Docker**: A complete coinswap stack with pre configured, bitcoind, tor, makred, maker-cli and taker apps. Useful for one-click-setup. [See the guide](./docs/docker.md) 

## Apps
**`taker-app(beta)`**: a GUI swap client to perform Coinswaps. [Build from source](https://github.com/citadel-tech/taker-app) 

# Setup & Installation

## Dependencies

Following dependencies are needed to compile the crate.

```shell
sudo apt install build-essential automake libtool
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

- Browse [issues](https://github.com/citadel-tech/coinswap/issues), especially [`good first issue`](https://github.com/citadel-tech/coinswap/issues?q=is%3Aopen+is%3Aissue+label%3A%22good+first+issue%22) tags
- Review [open PRs](https://github.com/citadel-tech/coinswap/pulls) 
- Search for `TODO`s in the codebase
- Read the [docs](./docs)

### Git Hooks

The repo contains pre-commit githooks to do auto-linting before commits. Set up the pre-commit hook by running:

```bash
ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit
```

## Community

Dev community: [Discord](https://discord.gg/Wz42hVmrrK)

Dev discussions predominantly happen via FOSS best practices, and by using Github as the major community forum.

The Issues, PRs and Discussions are where all the hard lifting is happening.

