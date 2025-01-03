# Maker CLI Documentation

The maker CLI is a command-line interface for interacting with the maker server in the Coinswap protocol. It allows you to manage your maker wallet, view balances, and perform various operations.

## Overview

The maker CLI provides functionality to:
- Monitor wallet balances
- View UTXOs
- Generate new addresses
- Send funds to external addresses
- Get network information

## Installation

The maker CLI is part of the Coinswap project. To build from source:

```bash:docs/maker-docs.md
git clone https://github.com/citadel-tech/coinswap
cd coinswap
cargo build --release
```

## Commands

### Basic Commands

1. **Ping**
   - Tests connectivity to the maker server
   ```bash
   maker-cli ping
   ```

2. **View UTXOs**
   - List seed UTXOs: `maker-cli seed-utxo`
   - List swap coin UTXOs: `maker-cli swap-utxo`
   - List contract UTXOs: `maker-cli contract-utxo`
   - List fidelity UTXOs: `maker-cli fidelity-utxo`

3. **Check Balances**
   - Seed balance: `maker-cli seed-balance`
   - Swap coin balance: `maker-cli swap-balance`
   - Contract balance: `maker-cli contract-balance`
   - Fidelity balance: `maker-cli fidelity-balance`

4. **Address Management**
   - Generate new address: `maker-cli new-address`

5. **Send Funds**
   ```bash
   maker-cli send-to-address --address <BITCOIN_ADDRESS> --amount <SATS> --fee <SATS>
   ```

6. **Network Information**
   - Get Tor address: `maker-cli get-tor-address`
   - Get data directory: `maker-cli get-data-dir`

## Configuration

The maker CLI uses a default data directory at `~/.coinswap/maker`. This directory contains:
- Wallet data
- Debug logs
- Configuration files

## Fidelity Bonds

The maker CLI supports fidelity bond operations. These bonds are used as a Sybil resistance mechanism. The value of fidelity bonds is calculated based on:

- Time-locked amount
- Lock period
- Current time
- Interest rate (1.5% default)
- Bond value exponent (1.3 default)

The bond value calculation uses a compound interest formula to incentivize longer lock periods and higher amounts.

## Network Connectivity

The maker CLI supports both clearnet and Tor connections with the following default settings:
- Connection timeout: 60 seconds
- Retry delay: 10 seconds
- Heartbeat interval: 3 seconds

## Logging

The maker CLI includes comprehensive logging functionality:
- Default log location: `~/.coinswap/maker/debug.log`
- Configurable log levels: off, error, warn, info, debug, trace
- Console and file logging support

## Error Handling

The CLI handles various error scenarios including:
- Network connectivity issues
- Invalid transactions
- Insufficient funds
- Bond-related errors (expiry, non-existence, already spent)
- Script validation errors

## Security Considerations

1. The maker CLI is currently in beta development and experimental stage
2. Mainnet use is not recommended
3. Always verify transactions before broadcasting
4. Keep your wallet backup secure
5. Use appropriate fee rates to avoid transaction delays

## Example Usage

1. Start the maker server and check connectivity:
```bash
maker-cli ping
```
2. Generate a new address for receiving funds:
```bash
maker-cli new-address
```

3. Check balances:
```bash
maker-cli seed-balance
maker-cli swap-balance
```

4. Send funds:
```bash
maker-cli send-to-address --address bc1qxxx... --amount 100000 --fee 1000
```

## Project Status

The maker CLI is part of the Coinswap project which is currently in active development. Features and APIs may change as the project evolves.

For more detailed information about the Coinswap protocol and implementation details, please refer to the [developer documentation](/docs/dev-book.md).

## Support

For support and discussions:
- Join the [Discord community](https://discord.gg/Wz42hVmrrK)
- Create issues on [GitHub](https://github.com/citadel-tech/coinswap/issues)
- Follow development progress through pull requests and discussions****