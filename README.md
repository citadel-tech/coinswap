# Teleport Transactions - Technical Readme

## Table of Contents

- [Teleport Transactions - Technical Readme](#teleport-transactions---technical-readme)
  - [Table of Contents](#table-of-contents)
  - [About](#about)
  - [Architecture](#architecture)
  - [Build and Run](#build-and-run)
  - [Project Status](#project-status)
  - [Roadmap](#roadmap)
    - [V 0.1.0](#v-010)
    - [V 0.1.\*](#v-01)
  - [Community](#community)

## About

Teleport Transactions is a rust implementation of a variant of atomic-swap protocol, using HTLCs on Bitcoin. Read more at:

* [Mailing list post](https://lists.linuxfoundation.org/pipermail/bitcoin-dev/2020-October/018221.html)
* [Detailed design](https://gist.github.com/chris-belcher/9144bd57a91c194e332fb5ca371d0964)
* [Developer's resources](/docs/developer_resources.md)
* [Run demo](/docs/run_teleport.md)

## Architecture

The project is divided into distinct modules, each focused on specific functionalities. The project directory-tree is given below:

```console
docs/
src/
├─ bin/
├─ maker/
├─ market/
├─ protocol/
├─ scripts/
├─ taker/
├─ wallet/
├─ watchtower/
tests/
```

- `taker`: Contains Taker-related behaviors, with core logic in `src/taker/api.rs`. Takers manage most protocol logic, while Makers play a relatively passive role.
- `maker`: Encompasses Maker-specific logic.
- `wallet`: Manages wallet-related operations, including storage and blockchain interaction.
- `market`: Handles market-related logic, where Makers post their offers.
- `watchtower`: Provides a Taker-offloadable watchtower implementation for monitoring contract transactions.
- `scripts`: Offers simple scripts to utilize library APIs in the `teleport` app.
- `bin`: Houses deployed project binaries.
- `protocol`: Contains utility functions, error handling, and messages for protocol communication.

## Build and Run

The project follows the standard Rust build workflow and generates a CLI app named `teleport`.

```console
$ cargo build
```

The project includes both unit and integration tests. The integration tests simulates various edge cases of the coinswap protocol.

To run the unit tests:

```console
$ cargo test
```

To run the integration tests, `--features integration-test` flag must be enabled. Run integration tests with:

```console
$ cargo test --features integration-test
```

To print out logs on the tests, set the `RUST_LOG` env variable to either `info`, `warn` or `error`.

For manual swaps using the `teleport` app, follow the instructions in [Run Teleport](./docs/run_teleport.md).

For in-depth developer documentation on protocol workflow and implementation, consult [Developer's Resources](./docs/developer_resources.md).

## Project Status

The project is currently in a pre-alpha stage, intended for demonstration and prototyping. The protocol has various hard-coded configuration variables and known/unknown bugs. Basic swap protocol functionality works on `regtest` and `signet` networks, but it's not recommended for `mainnet` use.

If you're interested in contributing to the project, explore the [open issues](https://github.com/utxo-teleport/teleport-transactions/issues) and submit a PR.

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
- [ ] Complete all unit tests in modules.
- [ ] Achieve >80% crate level test coverage ratio (including integration tests).
- [ ] Clean up and integrate fidelity bonds with maker banning.
- [ ] Switch to binary encoding for wallet data storage and network messages.
- [ ] Make tor detectable and connectable by default for Maker and Taker. And Tor configs to their config lists.
- [ ] Sketch a simple `AddressBook` server. Tor must. This is for MVP. Later on we will move to more decentralized address server architecture.
- [ ] Turn maker server into a `makerd` binary, and a `maker-cli` rpc controller app, with MVP API.
- [ ] Finalize the Taker API for downstream wallet integration.
- [ ] Develop an example web Taker client, with a downstream wallet.
- [ ] Package `makerd` and `maker-cli` in a downstream node.
- [ ] Release V 0.1.0 in Signet for beta testing.

### V 0.1.*

- [ ] Implement UTXO merging and branch-out via swap for improved UTXO management.
- [ ] Describe contract and funding transactions via miniscript, using BDK for wallet management.
- [ ] Enable wallet syncing via CBF (BIP157/158).
- [ ] Transition to taproot outputs for the entire protocol, enhancing anonymity and obfuscating contract transactions.
- [ ] Optional: Payjoin integration via coinswap.
- [ ] Implement customizable wallet data storage (SQLite, Postgres).

## Community

* Join the IRC channel: `#coinswap` on Libera IRC network. Accessible via [webchat client](https://web.libera.chat/#coinswap) or through Tor on the [Hackint network](https://www.hackint.org/transport/tor) at `ncwkrwxpq2ikcngxq3dy2xctuheniggtqeibvgofixpzvrwpa77tozqd.onion:6667`. Logs are available [here](http://gnusha.org/coinswap/).
