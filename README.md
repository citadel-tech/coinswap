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

The project includes both unit and integration tests. The integration tests simulate a standard coinswap protocol involving a Taker and two Makers.

To run integration tests, ensure you have a `bitcoind` node running in `regtest` mode on the default port, along with a sample `bitcoin.conf` file as shown:

```conf
regtest=1
fallbackfee=0.0001
server=1
txindex=1
rpcuser=regtestrpcuser
rpcpassword=regtestrpcpass
```

You'll also need a legacy wallet named `teleport` with sufficient funds (> 0.15 BTC) loaded in Bitcoin Core.

Run integration tests with:

```sh
cargo test test_standard_coinswap
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
- [ ] Refine logging information.
- [ ] Achieve >80% test coverage, including bad and recovery paths in integration tests.
- [ ] Switch to binary encoding for wallet data storage and network messages.
- [ ] Implement configuration file support for Takers and Makers.
- [ ] Deploy standalone binaries for Maker and Watchtower.
- [ ] Introduce watchtower service fee.
- [ ] Secure wallet file storage through encryption.
- [ ] Design robust wallet backup mechanism.
- [ ] Implement fidelity bond banning for misbehaving Makers.
- [ ] Establish Maker marketplace via nostr relays.
- [ ] Deploy Maker binary as a Cyphernode app.
- [ ] Create Flutter FFI for Taker library, demoable via web/mobile app.

### Further Improvements
- [ ] Implement UTXO merging and branch-out via swap for improved UTXO management.
- [ ] Describe contract and funding transactions via miniscript, using BDK for wallet management.
- [ ] Enable wallet syncing via CBF (BIP157/158).
- [ ] Transition to taproot outputs for the entire protocol, enhancing anonymity and obfuscating contract transactions.
- [ ] Optional Payjoin integration via coinswap.
- [ ] Implement customizable wallet data storage (SQLite, Postgres).

## Community

* Join the IRC channel: `#coinswap` on Libera IRC network. Accessible via [webchat client](https://web.libera.chat/#coinswap) or through Tor on the [Hackint network](https://www.hackint.org/transport/tor) at `ncwkrwxpq2ikcngxq3dy2xctuheniggtqeibvgofixpzvrwpa77tozqd.onion:6667`. Logs are available [here](http://gnusha.org/coinswap/).