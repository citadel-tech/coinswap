<div align="center">

<h1>Coinswap</h1>

<p>
    Functioning, minimal-viable binaries and libraries to perform a trustless, p2p <a href="https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964">Maxwell-Belcher Coinswap Protocol</a>.
  </p>

<p>
    <!--
    <a href="https://crates.io/crates/coinswap"><img alt="Crate Info" src="https://img.shields.io/crates/v/coinswap.svg"/></a>
    <a href="https://docs.rs/coinswap"><img alt="API Docs" src="https://img.shields.io/badge/docs.rs-coinswap-green"/></a>
    -->
    <a href="https://github.com/utxo-teleport/teleport-transactions/blob/master/LICENSE.md"><img alt="CC0 1.0 Universal Licensed" src="https://img.shields.io/badge/license-CC0--1.0-blue.svg"/></a>
    <a href="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/build.yaml"><img alt="CI Status" src="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/build.yaml/badge.svg"></a>
    <a href="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/lint.yaml"><img alt="CI Status" src="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/lint.yaml/badge.svg"></a>
    <a href="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/test.yaml"><img alt="CI Status" src="https://github.com/utxo-teleport/teleport-transactions/actions/workflows/test.yaml/badge.svg"></a>
    <a href="https://codecov.io/github/utxo-teleport/teleport-transactions?branch=master">
    <img alt="Coverage" src="https://codecov.io/github/utxo-teleport/teleport-transactions/coverage.svg?branch=master">
    </a>
    <a href="https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html"><img alt="Rustc Version 1.75.0+" src="https://img.shields.io/badge/rustc-1.75.0%2B-lightgrey.svg"/></a>
  </p>
</div>

> [!WARNING]
> This library is currently under beta development and at an experimental stage. There are known and unknown bugs. Mainnet use is strictly NOT recommended.

## Table of Contents

- [Table of Contents](#table-of-contents)
- [About](#about)
- [Build and Test](#build-and-test)
- [Architecture](#architecture)
- [Project Status](#project-status)
- [Roadmap](#roadmap)
  - [V 0.1.0](#v-010)
  - [V 0.1.\*](#v-01)
- [Community](#community)

## About

Teleport Transactions is a rust implementation of a variant of atomic-swap protocol, using HTLCs on Bitcoin. Read more at:

* [Mailing list post](https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2020-October/018221.html)
* [Detailed design](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964)
* [Developer's resources](/docs/dev-book.md)

## Build and Test

The repo contains a fully automated integration testing framework on Bitcoin Regtest. The bitcoin binary used for testing is
included [here](./bin/bitcoind).

> [!TIP]
> Delete the bitcoind binary to reduce repo size, if you don't intend to run the integration tests.

The integration tests are the best way to look at a working demonstration of the coinswap protocol, involving multiple makers,
a taker and the directory server. All working over Tor by default. No pre-requisite setup is needed, other than rust and cargo.

Run all the integration tests by running:

```console
$ cargo test --features=integration-test -- --nocapture
```

Each test in the [tests](./tests/) folder covers a different edge-case situation and demonstrates how the taker and makers recover
from various types of swap failures.

keep an eye on the logs, that's where all the actions are happening.

Play through a single test case, for example, `standard_swap`,  by running:

```console
$ cargo test --features=integration-test --tests test_standard_coinswap -- --nocapture
```
The individual test names can be found in the test files.

For in-depth developer documentation on the coinswap protocol and implementation, consult the [dev book](/docs/dev-book.md).

## Architecture

The project is divided into distinct modules, each focused on specific functionalities.

```console
docs/
src/
├─ bin/
├─ maker/
├─ market/
├─ protocol/
├─ taker/
tests/
```
| Directory           | Description |
|---------------------|-------------|
| **`doc`**           | Contains all the project-related docs. The [dev-book](./docs/dev-book.md) includes major developer salient points.|
| **`src/taker`**     | Taker module houses its core logic in `src/taker/api.rs` and handles both Taker-related behaviors and most of the protocol-related logic. |
| **`src/maker`**     | Encompasses Maker-specific logic and plays a relatively passive role compared to Taker. |
| **`src/wallet`**    | Manages wallet-related operations, including storage and blockchain interaction. |
| **`src/market`**    | Handles market-related logic, where Makers post their offers. |
| **`src/protocol`**  | Contains utility functions, error handling, and messages for protocol communication. |
| **`tests`**         | Contains integration tests. Describes behavior of various abort/malice cases.|

> [!IMPORTANT]
>  The project currently only compiles in Linux. Mac/Windows is not supported. To compile in Mac/Windows use virtual machines

## Roadmap

### V 0.1.0

- [X] Basic protocol workflow with integration tests.
- [X] Modularize protocol components.
- [X] Refine logging information.
- [X] Abort 1: Taker aborts after setup. Makers identify this, and gets their fund back via contract tx.
- [X] Abort 2: One Maker aborts **before setup**. Taker retaliates by banning the maker, moving on with other makers, if it can't find enough makers, then recovering via contract transactions.
  - [X] Case 1: Maker drops **before** sending sender's signature. Taker tries with another Maker and moves on.
  - [X] Case 2: Maker drops **before** sending sender's signature. Taker doesn't have any new Maker. Recovers from swap.
  - [X] Case 3: Maker drops **after** sending sender's signatures. Taker doesn't have any new Maker. Recovers from swap.
- [X] Build a flexible Test-Framework with `bitcoind` backend.
- [X] Abort 3: Maker aborts **after setup**. Taker and other Makers identify this and recovers back via contract tx. Taker bans the aborting Maker's fidelity bond.
  - [X] Case 1: Maker Drops at `ContractSigsForRecvrAndSender`. Does not broadcasts the funding txs. Taker and Other Maker recovers. Maker gets banned.
  - [X] Case 2: Maker drops at `ContractSigsForRecvr` after broadcasting funding txs. Taker and other Makers recover. Maker gets banned.
  - [X] Case 3: Maker Drops at `HashPreimage` message and doesn't respond back with privkeys. Taker and other Maker recovers. Maker gets banned.
- [X] Malice 1: Taker broadcasts contract immaturely. Other Makers identify this, get their funds back via contract tx.
- [X] Malice 2: One of the Makers broadcast contract immaturely. The Taker identify this, bans the Maker's fidelity bond, other Makers get back funds via contract tx.
- [X] Fix all clippy warnings.
- [x] Implement configuration file i/o support for Takers and Makers.
- [x] Switch to binary encoding for network messages.
- [x] Switch to binary encoding for wallet data.
- [x] Clean up and integrate fidelity bonds with maker banning.
- [x] Make tor detectable and connectable by default for Maker and Taker. And Tor configs to their config lists.
- [x] Sketch a simple `Directory Server`. Tor must. This will act as the MVP DNS server.
- [x] Achieve >80% crate-level test coverage ratio (including integration tests).
- [ ] Turn maker server into a `maker` cli app, that spawns the server in the background, and exposes a basic maker wallet API.
- [ ] Turn the taker into a `taker` cli app. This also has basic taker wallet API + `do_coinswap()` which spawns a swap process in the background.
- [ ] Create `swap_dns_server` as a stand-alone directory server binary.
- [ ] A fresh `demo.md` doc to demonstrate a swap process with `maker` and `taker` and `swap_dns_server` in Signet.
- [ ] Release v0.1.0 in crates.io.

### V 0.1.*

- [ ] Implement UTXO merging and branch-out via swap for improved UTXO management.
- [ ] Describe contract and funding transactions via miniscript, using BDK for wallet management.
- [ ] Enable wallet syncing via CBF (BIP157/158).
- [ ] Transition to taproot outputs for the entire protocol, enhancing anonymity and obfuscating contract transactions.
- [ ] Implement customizable wallet data storage (SQLite, Postgres).
- [ ] Optional: Payjoin integration via coinswap.

# Contributing

The project is under active development by a few motivated Rusty Bitcoin devs. Any contribution for features, tests, docs and other fixes/upgrades is encouraged and welcomed. The maintainers will use the PR thread to provide quick reviews and suggestions and are generally proactive at merging good contributions.

Few directions for new contributors:

- The list of [issues](https://github.com/utxo-teleport/teleport-transactions/issues) is a good place to look for contributable tasks and open problems.

- Issues marked with [`good first issue`](https://github.com/utxo-teleport/teleport-transactions/issues?q=is%3Aopen+is%3Aissue+label%3A%22good+first+issue%22) are good places to get started for newbie Rust/Bitcoin devs.

- The [docs](./docs) are a good place to start reading up on the protocol.

- Reviewing [open PRs](https://github.com/utxo-teleport/teleport-transactions/pulls) are a good place to start gathering a contextual understanding of the codebase.

- Search for `TODO`s in the codebase to find in-line marked code todos and smaller improvements.

### Setting Up Git Hooks

The repo contains pre-commit githooks to do auto-linting before commits. Set up the pre-commit hook by running:

```bash
ln -s ../../git_hooks/pre-commit .git/hooks/pre-commit
```


## Community

The dev community lurks in a Discord [here](https://discord.gg/Wz42hVmrrK).

Dev discussions predominantly happen via FOSS best practices, and by using Github as the Community Forum.

The Issues, PRs and Discussions are where all the hard lifting happening.
