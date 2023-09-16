# Teleport Transactions - Technical Readme

## Table of Contents

- [Teleport Transactions - Technical Readme](#teleport-transactions---technical-readme)
  - [Table of Contents](#table-of-contents)
  - [About](#about)
  - [Architecture](#architecture)
  - [Build and Run](#build-and-run)
  - [Project Status](#project-status)
  - [Roadmap](#roadmap)
    - [Beta Release](#beta-release)
    - [Further Improvements](#further-improvements)
  - [Community](#community)

## About

Teleport Transactions is a rust implementation of a variant of atomic-swap protocol, using HTLCs on Bitcoin.

Mailing list post: [here](https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2020-October/018221.html)

Detailed design, [here](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964)

Developer's Doc: [here](/docs/developer_resources.md)

Run Demo: [here](/docs/run_teleport.md)

## Architecture

The project is divided into distinct modules, each focused on specific functionalities:

- `taker`: Contains Taker-related behaviors, with core logic in `src/taker/taker.rs`. Takers manage most protocol logic, while Makers play a relatively passive role.
- `maker`: Encompasses Maker-specific logic.
- `wallet`: Manages wallet-related operations, including storage and blockchain interaction.
- `market`: Handles market-related logic, where Makers post their offers.
- `watchtower`: Provides a Taker-offloadable watchtower implementation for monitoring contract transactions.
- `scripts`: Offers simple scripts to utilize library APIs in the `teleport` app.
- `bin`: Houses deployed project binaries.

## Build and Run

The project follows the standard Rust build workflow and generates a CLI app named `teleport`.

```sh
cargo build
```

The project includes both unit and integration tests. The integration tests simulates various edge cases of the coinswap protocol.

To run the unit tests:
```sh
cargo test
```

To run the integration tests, `--features integration-test` must be enabled. Run integration tests with:

```sh
cargo test --features integration-test
```

For manual swaps using the `teleport` app, follow the instructions in [run_coinswap](./docs/run_teleport.md).

For in-depth developer documentation on protocol workflow and implementation, consult [developer_resources](./docs/developer_resources.md).

## Project Status

The project is currently in a pre-alpha stage, intended for demonstration and prototyping. The protocol has various hard-coded configuration variables and known/unknown bugs. Basic swap protocol functionality works on `regtest` and `signet` networks, but it's not recommended for `mainnet` use.

If you're interested in contributing to the project, explore the [open issues](https://github.com/utxo-teleport/teleport-transactions/issues) and submit a PR.

## Roadmap

### Beta Release
- [x] Basic protocol workflow with integration tests.
- [x] Modularize protocol components.
- [x] Refine logging information.
- [ ] Fix all clippy warnings.
- [x] Abort Case 1: Taker aborts after setup. Makers identify this, and gets their fund back via contract tx.
- [ ] Abort Case 2: One of the Maker aborts after setup. Taker and other Makers identify this and get their funds back via contract tx. Taker bans the aborting Maker's fidelity bond.
- [ ] Malice Case 1: Taker broadcasts contract immaturely. Other Makers identify this, get their funds back via contract tx.
- [ ] Malice Case 2: One of the Makers broadcast contract immaturely. The Taker identify this, bans the Maker's fidelity bond, other Makers get back funds via contract tx.
- [ ] Achieve >80% test coverage, including bad and recovery paths in integration tests.
- [ ] Switch to binary encoding for wallet data storage and network messages.
- [ ] Implement configuration file support for Takers and Makers.
- [ ] Deploy standalone binaries for Maker.
- [ ] Secure wallet file storage through encryption.
- [ ] Establish Maker marketplace via nostr relays.
- [ ] Create FFIs for Taker library.
- [ ] Develop an example web Taker client.
- [ ] Deploy Makers in Signet, and Demo coinswap via an example Taker client.

### Further Improvements
- [ ] Implement UTXO merging and branch-out via swap for improved UTXO management.
- [ ] Describe contract and funding transactions via miniscript, using BDK for wallet management.
- [ ] Enable wallet syncing via CBF (BIP157/158).
- [ ] Transition to taproot outputs for the entire protocol, enhancing anonymity and obfuscating contract transactions.
- [ ] Optional Payjoin integration via coinswap.
- [ ] Implement customizable wallet data storage (SQLite, Postgres).

## Community

* Join the IRC channel: `#coinswap` on Libera IRC network. Accessible via [webchat client](https://web.libera.chat/#coinswap) or through Tor on the [Hackint network](https://www.hackint.org/transport/tor) at `ncwkrwxpq2ikcngxq3dy2xctuheniggtqeibvgofixpzvrwpa77tozqd.onion:6667`. Logs are available [here](http://gnusha.org/coinswap/).