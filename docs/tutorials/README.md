# Overview

These guides explains how to:

- [Simulating a Regular Swap With 2 Makers and 1 Taker](./simulating-swap.md)
- [Simulating a Failed Swap and Triggering Recovery](./failed-swap-recovery.md)
- [Check Fidelity Bond on Blockchain](./verify-fidelity-bond.md)
- [Maker Selection Process](./maker-selection.md)

## Prerequisites

Before proceeding, ensure you have the following installed and configured:

- **Rust and Cargo**: The Coinswap project is built with Rust. Install `rustup` by following the instructions on [rust-lang.org](https://www.rust-lang.org/tools/install).
- **Bitcoin Core**: A running `bitcoind` instance configured for `custom signet` is essential. Refer to the [demo setup guide](../demo.md) for detailed instructions on setting up Bitcoin Core for `custom signet`.
- **Tor**: Installed and configured. Follow the [Tor setup guide](../tor.md) if you haven't already.
- **Coinswap Binaries v0.1.0**: Ensure `makerd`, `maker-cli`, and `taker` binaries are compiled and available in your `PATH` or you know their location (e.g., `target/release/`). You can build them by cloning the repository and running `cargo build --release`.

## Troubleshooting

- **Version compatibility**: This tutorial is based on Bitcoin Core v29.0 and Coinswap binaries v0.1.0. Command syntax or behavior might differ in other versions.

## See Also

- [Coinswap Market](http://a4ovtjlwiclzy37bjaurcbb6wpl6dtckmlqwrywq7uoajeaz6kth4uyd.onion/): Lists available makers on `custom signet` (accessible via Tor Browser).
- [Bitcoin Core documentation](https://developer.bitcoin.org/reference/rpc/index.html)
- [Coinswap Protocol Specification](https://github.com/citadel-tech/Coinswap-Protocol-Specification)
- [Coinswap Setup](../demo.md)
- [Bitcoin Core Setup](../bitcoind.md)
- [Maker Server Guide](../makerd.md)
- [Maker CLI Reference](../maker-cli.md)
- [Taker Client Guide](../taker.md)

## Glossary

- **Custom signet**: A custom-configured Bitcoin Signet network used for development and testing.
- **Fidelity bond**: A security mechanism in CoinSwap where makers lock up a certain amount of Bitcoin to ensure honest behavior.
- **Maker**: A participant in the CoinSwap protocol who provides liquidity by offering to swap coins.
- **Taker**: A participant in the CoinSwap protocol who initiates a swap by accepting a maker's offer.
- **Timelock contract**: A Bitcoin script that locks funds until a specified time or block height, used in CoinSwap for recovery in case of a failed swap.
