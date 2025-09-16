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

### ⚠️ Important
Coinswap v0.1.0 marketplace is now live on Custom Signet. [Check it out here](http://a4ovtjlwiclzy37bjaurcbb6wpl6dtckmlqwrywq7uoajeaz6kth4uyd.onion/) (Tor Browser required).

A Block Explorer is available to check signet blocks and transactions. [Check it out here](http://xlrj7ilheypw67premos73gxlcl7ha77kbhrqys7mydp7jve25olsxyd.onion/)

A Faucet is available for getting test coins for the custom signet. [Check it out here](http://s2ncekhezyo2tkwtftti3aiukfpqmxidatjrdqmwie6xnf2dfggyscad.onion/)

### ⚠️ Warning
This library is currently under beta development and is in an experimental stage. There are known and unknown bugs. **Mainnet use is strictly NOT recommended.** 

# About

Coinswap is a decentralized [atomic swap](https://bitcoinops.org/en/topics/coinswap/) protocol that enables trustless swaps of Bitcoin UTXOs through a decentralized, Sybil-resistant marketplace.

Existing atomic swap solutions are centralized, rely on large swap servers, and have service providers as single points of failure for censorship and privacy attacks. This project implements atomic swaps via a decentralized market-based protocol.

The project builds on Chris Belcher's [teleport-transactions](https://github.com/bitcoin-teleport/teleport-transactions) and has significantly matured with complete protocol handling, functional testing, Sybil resistance, and command-line applications.

Anyone can become a swap service provider (**Maker**) by running `makerd` to earn fees. **Takers** use the `taker` app to swap with multiple makers, routing through various makers for privacy. The system uses a *smart-client-dumb-server* philosophy with minimal server requirements, allowing any home node operator to run a maker.

The protocol employs [fidelity bonds](https://github.com/JoinMarket-Org/joinmarket-clientserver/blob/master/docs/fidelity-bonds.md) for Sybil and DoS resistance. Takers coordinate swaps and handle recovery; makers respond to queries. All communication occurs over Tor.

For technical details, see the [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification).

# Setup & Installation

## Dependencies

```shell
sudo apt install build-essential automake libtool
```

**Tor Installation**: Required for all operations. Download from torproject.org for your OS. Bitcoin Core automatically detects Tor and creates anonymous services. See the [Tor guide](./docs/tor.md) for configuration details.

**Bitcoin Core**: Requires fully synced, non-pruned node with RPC access on Custom Signet with `-txindex` enabled. Follow the [bitcoind setup guide](./docs/bitcoind.md).

## Build and Install

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

## Verify Setup

```console
makerd --help
maker-cli --help
taker --help

# Test connection to market
taker fetch-offers
```

# Applications

**`makerd`**: Server daemon for swap providers. Requires continuous uptime and Bitcoin Core RPC connection. [Demo](./docs/makerd.md)

**`maker-cli`**: RPC controller for `makerd`. Manage server, access wallet, view swap statistics. [Demo](./docs/maker-cli.md)

**`taker`**: Swap client acting as a Bitcoin wallet with swap capability. [Demo](./docs/taker.md)

### ❗ Important

Always stop `makerd` with `maker-cli stop` to ensure wallet data integrity. Avoid using `ctrl+c`.

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

