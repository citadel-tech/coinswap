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

Teleport Transactions is a Rust implementation of the Coinswap protocol on Bitcoin. The Coinswap protocol enhances privacy on the Bitcoin network, specifically addressing transaction ownership traceability through chain analysis.

Imagine Alice wants to send bitcoin with maximum privacy. She initiates a unique transaction that appears ordinary on the blockchain, with her coins seemingly moving from address A to B. However, the coins actually end up in an unrelated address Z. This process confounds any attempt to trace ownership.

This protocol also benefits users like Carol, who use regular wallets. Her transactions appear the same as Alice's, adding uncertainty to any analysis. Even users unaware of this software enjoy improved privacy.

In a world where privacy is vital due to data collection by advertisers and institutions, this enhancement is significant. Moreover, it bolsters Bitcoin's fungibility, making it a more effective form of currency.

For detailed design, refer to the [Design for a CoinSwap Implementation for Massively Improving Bitcoin Privacy and Fungibility](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964) document.

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